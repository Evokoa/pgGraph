//! Pure caller-visibility policy consumed by graph algorithms.
//!
//! PostgreSQL policy evaluation and SPI scans live in `sql_visibility`; this
//! module only owns compact query-scoped admission state.

use roaring::RoaringBitmap;
use std::collections::HashMap;

use crate::edge_store::RelationshipId;
use crate::safety::{GraphError, GraphResult};

/// Caller-visible intersection of the loaded projection and source-table RLS.
#[derive(Debug, Clone)]
pub(crate) struct VisibilityScope(VisibilityState);

#[derive(Debug, Clone)]
enum VisibilityState {
    /// No registered source relation applies RLS to the outer caller.
    Unrestricted,
    /// A caller-proven visible identity for a no-expansion execution shape.
    DirectNode(u32),
    /// Hidden projection identities for relations whose RLS policies apply.
    Enforced {
        hidden_nodes: RoaringBitmap,
        hidden_relationships: RoaringBitmap,
        relationship_rls_edge_types: RoaringBitmap,
    },
}

/// One query's prepared visibility authority and context factory.
#[derive(Debug, Clone)]
pub(crate) struct VisibilityCoordinator {
    scope: VisibilityScope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VisibilityCacheLimits {
    pub(crate) max_entries: usize,
    pub(crate) max_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct VisibilityStatementCache {
    node_verdicts: HashMap<u32, VisibilityVerdict>,
    relationship_verdicts: HashMap<RelationshipId, VisibilityVerdict>,
    retained_bytes: usize,
    limits: VisibilityCacheLimits,
}

impl VisibilityStatementCache {
    pub(crate) fn new(limits: VisibilityCacheLimits) -> Self {
        Self {
            node_verdicts: HashMap::new(),
            relationship_verdicts: HashMap::new(),
            retained_bytes: 0,
            limits,
        }
    }

    pub(crate) fn node_verdict(&self, node_idx: u32) -> VisibilityVerdict {
        self.node_verdicts
            .get(&node_idx)
            .copied()
            .unwrap_or(VisibilityVerdict::Unknown)
    }

    pub(crate) fn relationship_verdict(
        &self,
        relationship_id: RelationshipId,
    ) -> VisibilityVerdict {
        self.relationship_verdicts
            .get(&relationship_id)
            .copied()
            .unwrap_or(VisibilityVerdict::Unknown)
    }

    pub(crate) fn try_record(
        &mut self,
        node_idx: u32,
        verdict: VisibilityVerdict,
    ) -> GraphResult<()> {
        if verdict == VisibilityVerdict::Unknown {
            return Err(GraphError::InvalidFilter {
                reason: "visibility cache cannot record Unknown".into(),
            });
        }
        if let std::collections::hash_map::Entry::Occupied(mut entry) =
            self.node_verdicts.entry(node_idx)
        {
            entry.insert(verdict);
            return Ok(());
        }
        let entry_bytes = std::mem::size_of::<(u32, VisibilityVerdict)>() * 2;
        let requested_entries = self
            .node_verdicts
            .len()
            .checked_add(self.relationship_verdicts.len())
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| GraphError::InvalidFilter {
                reason: "visibility cache entry count overflow".into(),
            })?;
        let requested_bytes = self
            .retained_bytes
            .checked_add(entry_bytes)
            .ok_or_else(|| GraphError::InvalidFilter {
                reason: "visibility cache byte count overflow".into(),
            })?;
        self.check_capacity(requested_entries, requested_bytes, entry_bytes)?;
        self.node_verdicts
            .try_reserve(1)
            .map_err(|_| GraphError::ResourceLimit {
                resource: "visibility_cache".into(),
                phase: crate::resource::ResourcePhase::QueryVisibility
                    .as_str()
                    .into(),
                used: u64::try_from(requested_bytes).unwrap_or(u64::MAX),
                requested: u64::try_from(entry_bytes).unwrap_or(u64::MAX),
                limit: u64::try_from(self.limits.max_bytes).unwrap_or(u64::MAX),
            })?;
        self.node_verdicts.insert(node_idx, verdict);
        self.retained_bytes = requested_bytes;
        Ok(())
    }

    #[allow(dead_code, reason = "P2 freezes relationship cache semantics for P3")]
    pub(crate) fn try_record_relationship(
        &mut self,
        relationship_id: RelationshipId,
        verdict: VisibilityVerdict,
    ) -> GraphResult<()> {
        if verdict == VisibilityVerdict::Unknown {
            return Err(GraphError::InvalidFilter {
                reason: "visibility cache cannot record Unknown".into(),
            });
        }
        if let std::collections::hash_map::Entry::Occupied(mut entry) =
            self.relationship_verdicts.entry(relationship_id)
        {
            entry.insert(verdict);
            return Ok(());
        }
        let requested_entries = self
            .node_verdicts
            .len()
            .checked_add(self.relationship_verdicts.len())
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| GraphError::InvalidFilter {
                reason: "visibility cache entry count overflow".into(),
            })?;
        let entry_bytes = std::mem::size_of::<(RelationshipId, VisibilityVerdict)>() * 2;
        let requested_bytes = self
            .retained_bytes
            .checked_add(entry_bytes)
            .ok_or_else(|| GraphError::InvalidFilter {
                reason: "visibility cache byte count overflow".into(),
            })?;
        self.check_capacity(requested_entries, requested_bytes, entry_bytes)?;
        self.relationship_verdicts
            .try_reserve(1)
            .map_err(|_| cache_resource_limit(requested_bytes, entry_bytes, self.limits))?;
        self.relationship_verdicts.insert(relationship_id, verdict);
        self.retained_bytes = requested_bytes;
        Ok(())
    }

    fn check_capacity(
        &self,
        requested_entries: usize,
        requested_bytes: usize,
        entry_bytes: usize,
    ) -> GraphResult<()> {
        if requested_entries > self.limits.max_entries || requested_bytes > self.limits.max_bytes {
            return Err(cache_resource_limit(
                requested_bytes,
                entry_bytes,
                self.limits,
            ));
        }
        Ok(())
    }
}

fn cache_resource_limit(
    requested_bytes: usize,
    entry_bytes: usize,
    limits: VisibilityCacheLimits,
) -> GraphError {
    GraphError::ResourceLimit {
        resource: "visibility_cache".into(),
        phase: crate::resource::ResourcePhase::QueryVisibility
            .as_str()
            .into(),
        used: u64::try_from(requested_bytes.saturating_sub(entry_bytes)).unwrap_or(u64::MAX),
        requested: u64::try_from(entry_bytes).unwrap_or(u64::MAX),
        limit: u64::try_from(limits.max_bytes).unwrap_or(u64::MAX),
    }
}

#[derive(Debug)]
pub(crate) struct LazyVisibilityCoordinator {
    cache: VisibilityStatementCache,
    mode: LazyVisibilityMode,
    probe_tables: std::collections::HashSet<u32>,
    policy_rls_tables: std::collections::HashSet<u32>,
    probe_mappings: std::collections::HashSet<u64>,
    probe_edge_types: std::collections::HashSet<u8>,
}

pub(crate) struct ProvenVisibleNode {
    node_idx: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LazyVisibilityMode {
    Unrestricted,
    Enforced,
}

impl LazyVisibilityCoordinator {
    pub(crate) fn new(
        limits: VisibilityCacheLimits,
        mode: LazyVisibilityMode,
        probe_tables: std::collections::HashSet<u32>,
        policy_rls_tables: std::collections::HashSet<u32>,
    ) -> Self {
        Self::with_relationship_mappings(
            limits,
            mode,
            probe_tables,
            policy_rls_tables,
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        )
    }

    pub(crate) fn with_relationship_mappings(
        limits: VisibilityCacheLimits,
        mode: LazyVisibilityMode,
        probe_tables: std::collections::HashSet<u32>,
        policy_rls_tables: std::collections::HashSet<u32>,
        probe_mappings: std::collections::HashSet<u64>,
        probe_edge_types: std::collections::HashSet<u8>,
    ) -> Self {
        Self {
            cache: VisibilityStatementCache::new(limits),
            mode,
            probe_tables,
            policy_rls_tables,
            probe_mappings,
            probe_edge_types,
        }
    }

    pub(crate) fn table_requires_probe(&self, table_oid: u32) -> bool {
        self.mode == LazyVisibilityMode::Enforced && self.probe_tables.contains(&table_oid)
    }

    pub(crate) fn table_has_policy_rls(&self, table_oid: u32) -> bool {
        self.policy_rls_tables.contains(&table_oid)
    }

    pub(crate) fn is_enforced(&self) -> bool {
        self.mode == LazyVisibilityMode::Enforced
    }

    pub(crate) fn mapping_requires_probe(&self, mapping_id: u64) -> bool {
        self.mode == LazyVisibilityMode::Enforced && self.probe_mappings.contains(&mapping_id)
    }

    pub(crate) fn edge_type_requires_relationship_identity(&self, edge_type: u8) -> bool {
        self.mode == LazyVisibilityMode::Enforced && self.probe_edge_types.contains(&edge_type)
    }

    pub(crate) fn prove_visible_node(&self, node_idx: u32) -> GraphResult<ProvenVisibleNode> {
        if self.node_verdict(node_idx) != VisibilityVerdict::Visible {
            return Err(GraphError::Internal(
                "cannot authorize an unresolved or hidden direct node".into(),
            ));
        }
        Ok(ProvenVisibleNode { node_idx })
    }

    pub(crate) fn node_verdict(&self, node_idx: u32) -> VisibilityVerdict {
        self.cache.node_verdict(node_idx)
    }

    pub(crate) fn record_node(
        &mut self,
        node_idx: u32,
        verdict: VisibilityVerdict,
    ) -> GraphResult<()> {
        self.cache.try_record(node_idx, verdict)
    }

    pub(crate) fn relationship_verdict(
        &self,
        relationship_id: RelationshipId,
    ) -> VisibilityVerdict {
        self.cache.relationship_verdict(relationship_id)
    }

    #[allow(dead_code, reason = "P2 freezes relationship cache semantics for P3")]
    pub(crate) fn record_relationship(
        &mut self,
        relationship_id: RelationshipId,
        verdict: VisibilityVerdict,
    ) -> GraphResult<()> {
        self.cache.try_record_relationship(relationship_id, verdict)
    }
}

impl VisibilityCoordinator {
    pub(crate) fn from_prepared_scope(
        _proof: crate::sql_visibility::PreparedVisibilityProof,
        scope: VisibilityScope,
    ) -> Self {
        Self { scope }
    }

    pub(crate) fn context<'a>(
        &'a self,
        governor: &'a crate::resource::ResourceGovernor,
    ) -> QueryExecutionContext<'a> {
        QueryExecutionContext {
            governor,
            visibility: &self.scope,
            edge_type_filter: None,
        }
    }

    pub(crate) fn context_with_edge_type_filter<'a>(
        &'a self,
        governor: &'a crate::resource::ResourceGovernor,
        edge_type_filter: Option<&'a RoaringBitmap>,
    ) -> QueryExecutionContext<'a> {
        QueryExecutionContext {
            governor,
            visibility: &self.scope,
            edge_type_filter,
        }
    }

    pub(crate) fn allows_node(&self, node_idx: u32) -> bool {
        self.scope.allows_node(node_idx)
    }

    #[allow(dead_code, reason = "P1 freezes the eager parity seam consumed by P2")]
    pub(crate) fn resolve_prepared_batch(
        &self,
        batch: &VisibilityCandidateBatch,
    ) -> GraphResult<VisibilityVerdictBatch> {
        let verdicts = batch
            .candidates
            .iter()
            .map(|candidate| {
                let verdict = match candidate {
                    VisibilityCandidate::Node { node_idx, .. } => self.scope.allows_node(*node_idx),
                    VisibilityCandidate::Relationship {
                        relationship_id,
                        edge_type,
                        ..
                    } => self
                        .scope
                        .allows_relationship(*edge_type, *relationship_id)?,
                };
                Ok((
                    candidate.sequence(),
                    if verdict {
                        VisibilityVerdict::Visible
                    } else {
                        VisibilityVerdict::Hidden
                    },
                ))
            })
            .collect::<GraphResult<Vec<_>>>()?;
        VisibilityVerdictBatch::try_new(batch, verdicts)
    }

    #[cfg(test)]
    pub(crate) fn unrestricted_for_test_or_benchmark() -> Self {
        Self {
            scope: VisibilityScope(VisibilityState::Unrestricted),
        }
    }

    #[cfg(any(test, feature = "benchmarks"))]
    pub(crate) fn unrestricted_for_benchmark(
        _proof: &crate::bench_support::BenchmarkVisibilityProof,
    ) -> Self {
        Self {
            scope: VisibilityScope(VisibilityState::Unrestricted),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_scope_for_test(scope: VisibilityScope) -> Self {
        Self { scope }
    }

    #[cfg(test)]
    pub(crate) fn scope(&self) -> &VisibilityScope {
        &self.scope
    }

    #[cfg(any(test, feature = "benchmarks"))]
    pub(crate) fn scope_for_benchmark(
        &self,
        _proof: &crate::bench_support::BenchmarkVisibilityProof,
    ) -> &VisibilityScope {
        &self.scope
    }
}

#[allow(
    dead_code,
    reason = "P1 freezes the bounded candidate seam consumed by P2"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VisibilityCandidate {
    Node {
        sequence: u32,
        table_oid: u32,
        source_key: String,
        node_idx: u32,
    },
    Relationship {
        sequence: u32,
        mapping_id: u64,
        source_key: String,
        relationship_id: Option<RelationshipId>,
        edge_type: u8,
    },
}

#[allow(
    dead_code,
    reason = "P1 freezes the bounded candidate seam consumed by P2"
)]
impl VisibilityCandidate {
    pub(crate) fn sequence(&self) -> u32 {
        match self {
            Self::Node { sequence, .. } | Self::Relationship { sequence, .. } => *sequence,
        }
    }
    pub(crate) fn key_bytes(&self) -> usize {
        match self {
            Self::Node { source_key, .. } | Self::Relationship { source_key, .. } => {
                source_key.len()
            }
        }
    }
}

#[allow(
    dead_code,
    reason = "P1 freezes the bounded candidate seam consumed by P2"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VisibilityBatchLimits {
    pub(crate) max_candidates: usize,
    pub(crate) max_key_bytes: usize,
}

#[allow(
    dead_code,
    reason = "P1 freezes the bounded candidate seam consumed by P2"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VisibilityCandidateBatch {
    candidates: Vec<VisibilityCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VisibilityProbeBatch {
    pub(crate) candidates: Vec<VisibilityCandidate>,
    pub(crate) unique_node_keys: Vec<(u32, String)>,
    pub(crate) unique_relationship_keys: Vec<(u64, String)>,
}

impl VisibilityProbeBatch {
    pub(crate) fn try_new(batch: VisibilityCandidateBatch) -> GraphResult<Self> {
        let mut seen = HashMap::<(u32, String), ()>::new();
        seen.try_reserve(batch.candidates.len())
            .map_err(|_| GraphError::ResourceLimit {
                resource: "visibility_probe".into(),
                phase: crate::resource::ResourcePhase::QueryVisibility
                    .as_str()
                    .into(),
                used: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
                requested: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
                limit: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
            })?;
        let mut unique_node_keys = Vec::new();
        let mut unique_relationship_keys = Vec::new();
        unique_node_keys
            .try_reserve(batch.candidates.len())
            .map_err(|_| GraphError::ResourceLimit {
                resource: "visibility_probe".into(),
                phase: crate::resource::ResourcePhase::QueryVisibility
                    .as_str()
                    .into(),
                used: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
                requested: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
                limit: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
            })?;
        unique_relationship_keys
            .try_reserve(batch.candidates.len())
            .map_err(|_| GraphError::ResourceLimit {
                resource: "visibility_probe".into(),
                phase: crate::resource::ResourcePhase::QueryVisibility
                    .as_str()
                    .into(),
                used: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
                requested: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
                limit: u64::try_from(batch.candidates.len()).unwrap_or(u64::MAX),
            })?;
        for candidate in &batch.candidates {
            if let VisibilityCandidate::Node {
                table_oid,
                source_key,
                ..
            } = candidate
            {
                let key = (*table_oid, source_key.clone());
                if seen.insert(key.clone(), ()).is_none() {
                    unique_node_keys.push(key);
                }
            } else if let VisibilityCandidate::Relationship {
                mapping_id,
                source_key,
                ..
            } = candidate
            {
                let key = (*mapping_id, source_key.clone());
                if !unique_relationship_keys.contains(&key) {
                    unique_relationship_keys.push(key);
                }
            }
        }
        Ok(Self {
            candidates: batch.candidates,
            unique_node_keys,
            unique_relationship_keys,
        })
    }
}

#[allow(
    dead_code,
    reason = "P1 freezes the bounded candidate seam consumed by P2"
)]
impl VisibilityCandidateBatch {
    pub(crate) fn try_new(
        candidates: Vec<VisibilityCandidate>,
        limits: VisibilityBatchLimits,
    ) -> GraphResult<Self> {
        if candidates.len() > limits.max_candidates {
            return Err(GraphError::InvalidFilter {
                reason: "visibility candidate count exceeds batch limit".into(),
            });
        }
        let mut bytes = 0usize;
        let mut previous = None;
        for candidate in &candidates {
            if previous.is_some_and(|value| candidate.sequence() <= value) {
                return Err(GraphError::InvalidFilter {
                    reason: "visibility candidate sequence must be strictly increasing".into(),
                });
            }
            previous = Some(candidate.sequence());
            bytes = bytes.checked_add(candidate.key_bytes()).ok_or_else(|| {
                GraphError::InvalidFilter {
                    reason: "visibility candidate key bytes overflow".into(),
                }
            })?;
            if bytes > limits.max_key_bytes {
                return Err(GraphError::InvalidFilter {
                    reason: "visibility candidate key bytes exceed batch limit".into(),
                });
            }
        }
        Ok(Self { candidates })
    }

    pub(crate) fn candidates(&self) -> &[VisibilityCandidate] {
        &self.candidates
    }

    pub(crate) fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub(crate) fn key_bytes(&self) -> GraphResult<usize> {
        self.candidates.iter().try_fold(0usize, |total, candidate| {
            total
                .checked_add(candidate.key_bytes())
                .ok_or_else(|| GraphError::InvalidFilter {
                    reason: "visibility candidate key bytes overflow".into(),
                })
        })
    }
}

#[allow(
    dead_code,
    reason = "P1 freezes the bounded verdict seam consumed by P2"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisibilityVerdict {
    Unknown,
    Visible,
    Hidden,
}

#[allow(
    dead_code,
    reason = "P1 freezes the bounded verdict seam consumed by P2"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VisibilityVerdictBatch {
    verdicts: Vec<(u32, VisibilityVerdict)>,
}

#[allow(
    dead_code,
    reason = "P1 freezes the bounded verdict seam consumed by P2"
)]
impl VisibilityVerdictBatch {
    pub(crate) fn try_new(
        batch: &VisibilityCandidateBatch,
        verdicts: Vec<(u32, VisibilityVerdict)>,
    ) -> GraphResult<Self> {
        if verdicts.len() != batch.candidates.len()
            || verdicts
                .iter()
                .zip(&batch.candidates)
                .any(|((sequence, verdict), candidate)| {
                    *sequence != candidate.sequence() || *verdict == VisibilityVerdict::Unknown
                })
        {
            return Err(GraphError::InvalidFilter {
                reason: "visibility verdicts must be complete, resolved, and sequence-aligned"
                    .into(),
            });
        }
        Ok(Self { verdicts })
    }

    pub(crate) fn verdicts(&self) -> &[(u32, VisibilityVerdict)] {
        &self.verdicts
    }

    pub(crate) fn verdict(&self, sequence: u32) -> GraphResult<VisibilityVerdict> {
        self.verdicts
            .iter()
            .find_map(|(candidate_sequence, verdict)| {
                (*candidate_sequence == sequence).then_some(*verdict)
            })
            .ok_or_else(|| {
                GraphError::Internal(format!("visibility verdict sequence {sequence} is absent"))
            })
    }
}

impl VisibilityScope {
    pub(crate) fn unrestricted(_proof: &crate::sql_visibility::PreparedVisibilityProof) -> Self {
        Self(VisibilityState::Unrestricted)
    }

    pub(crate) fn enforced(
        _proof: &crate::sql_visibility::PreparedVisibilityProof,
        hidden_nodes: RoaringBitmap,
        hidden_relationships: RoaringBitmap,
        relationship_rls_edge_types: RoaringBitmap,
    ) -> Self {
        Self(VisibilityState::Enforced {
            hidden_nodes,
            hidden_relationships,
            relationship_rls_edge_types,
        })
    }

    pub(crate) fn direct_node(
        _proof: &crate::sql_visibility::PreparedVisibilityProof,
        node: ProvenVisibleNode,
    ) -> Self {
        Self(VisibilityState::DirectNode(node.node_idx))
    }

    #[cfg(test)]
    pub(crate) fn unrestricted_for_test() -> Self {
        Self(VisibilityState::Unrestricted)
    }

    #[cfg(test)]
    pub(crate) fn enforced_for_test(
        hidden_nodes: RoaringBitmap,
        hidden_relationships: RoaringBitmap,
        relationship_rls_edge_types: RoaringBitmap,
    ) -> Self {
        Self(VisibilityState::Enforced {
            hidden_nodes,
            hidden_relationships,
            relationship_rls_edge_types,
        })
    }

    #[inline(always)]
    pub(crate) fn allows_node(&self, node_idx: u32) -> bool {
        match &self.0 {
            VisibilityState::Unrestricted => true,
            VisibilityState::DirectNode(visible_node) => node_idx == *visible_node,
            VisibilityState::Enforced { hidden_nodes, .. } => !hidden_nodes.contains(node_idx),
        }
    }

    #[inline(always)]
    pub(crate) fn allows_relationship(
        &self,
        edge_type: u8,
        relationship_id: Option<RelationshipId>,
    ) -> GraphResult<bool> {
        match &self.0 {
            VisibilityState::Unrestricted => Ok(true),
            VisibilityState::DirectNode(_) => Ok(false),
            VisibilityState::Enforced {
                hidden_relationships: _,
                relationship_rls_edge_types,
                ..
            } if !relationship_rls_edge_types.contains(u32::from(edge_type)) => Ok(true),
            VisibilityState::Enforced {
                hidden_relationships,
                ..
            } => relationship_id
                .map(|id| !hidden_relationships.contains(id))
                .ok_or(GraphError::RlsRelationshipIdentityMissing),
        }
    }

    pub(crate) fn hide_nodes(&mut self, nodes: &RoaringBitmap) {
        if let VisibilityState::Enforced { hidden_nodes, .. } = &mut self.0 {
            *hidden_nodes |= nodes;
        }
    }

    pub(crate) fn hide_node(&mut self, node_idx: u32) {
        if let VisibilityState::Enforced { hidden_nodes, .. } = &mut self.0 {
            hidden_nodes.insert(node_idx);
        }
    }

    pub(crate) fn reveal_node(&mut self, node_idx: u32) {
        if let VisibilityState::Enforced { hidden_nodes, .. } = &mut self.0 {
            hidden_nodes.remove(node_idx);
        }
    }

    pub(crate) fn hide_relationship(&mut self, relationship_id: RelationshipId) {
        if let VisibilityState::Enforced {
            hidden_relationships,
            ..
        } = &mut self.0
        {
            hidden_relationships.insert(relationship_id);
        }
    }

    pub(crate) fn reveal_relationship(&mut self, relationship_id: RelationshipId) {
        if let VisibilityState::Enforced {
            hidden_relationships,
            ..
        } = &mut self.0
        {
            hidden_relationships.remove(relationship_id);
        }
    }

    pub(crate) fn is_unrestricted(&self) -> bool {
        matches!(self.0, VisibilityState::Unrestricted)
    }

    #[cfg(feature = "development")]
    pub(crate) fn hidden_counts(&self) -> (u64, u64) {
        match &self.0 {
            VisibilityState::Unrestricted => (0, 0),
            VisibilityState::DirectNode(_) => (0, 0),
            VisibilityState::Enforced {
                hidden_nodes,
                hidden_relationships,
                ..
            } => (hidden_nodes.len(), hidden_relationships.len()),
        }
    }
}

/// Borrowed operation state shared by topology algorithms.
pub(crate) struct QueryExecutionContext<'a> {
    pub(crate) governor: &'a crate::resource::ResourceGovernor,
    pub(crate) visibility: &'a VisibilityScope,
    pub(crate) edge_type_filter: Option<&'a RoaringBitmap>,
}

#[cfg(test)]
impl<'a> QueryExecutionContext<'a> {
    pub(crate) const fn new(
        governor: &'a crate::resource::ResourceGovernor,
        visibility: &'a VisibilityScope,
    ) -> Self {
        Self {
            governor,
            visibility,
            edge_type_filter: None,
        }
    }

    pub(crate) const fn with_edge_type_filter(
        governor: &'a crate::resource::ResourceGovernor,
        visibility: &'a VisibilityScope,
        edge_type_filter: Option<&'a RoaringBitmap>,
    ) -> Self {
        Self {
            governor,
            visibility,
            edge_type_filter,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrestricted_scope_allows_nodes_and_identityless_relationships() {
        let scope = VisibilityScope::unrestricted_for_test();
        assert!(scope.allows_node(7));
        assert!(scope.allows_relationship(3, None).unwrap());
    }

    #[test]
    fn enforced_scope_hides_exact_node_and_relationship_ids() {
        let mut hidden_nodes = RoaringBitmap::new();
        hidden_nodes.insert(7);
        let mut hidden_relationships = RoaringBitmap::new();
        hidden_relationships.insert(11);
        let mut rls_edge_types = RoaringBitmap::new();
        rls_edge_types.insert(3);
        let scope =
            VisibilityScope::enforced_for_test(hidden_nodes, hidden_relationships, rls_edge_types);

        assert!(!scope.allows_node(7));
        assert!(scope.allows_node(8));
        assert!(!scope.allows_relationship(3, Some(11)).unwrap());
        assert!(scope.allows_relationship(3, Some(12)).unwrap());
        assert!(scope.allows_relationship(4, None).unwrap());
        assert!(matches!(
            scope.allows_relationship(3, None),
            Err(GraphError::RlsRelationshipIdentityMissing)
        ));
    }

    #[test]
    fn node_only_rls_does_not_require_relationship_identity() {
        let scope = VisibilityScope::enforced_for_test(
            RoaringBitmap::new(),
            RoaringBitmap::new(),
            RoaringBitmap::new(),
        );
        assert!(scope.allows_relationship(3, None).unwrap());
    }

    #[test]
    fn candidate_batches_preserve_sequence_and_reject_count_or_byte_overflow() {
        let node = |sequence, key: &str| VisibilityCandidate::Node {
            sequence,
            table_oid: 1,
            source_key: key.into(),
            node_idx: sequence,
        };
        let limits = VisibilityBatchLimits {
            max_candidates: 2,
            max_key_bytes: 4,
        };
        assert!(
            VisibilityCandidateBatch::try_new(vec![node(0, "a"), node(1, "bb")], limits).is_ok()
        );
        assert!(
            VisibilityCandidateBatch::try_new(vec![node(1, "a"), node(1, "b")], limits).is_err()
        );
        assert!(
            VisibilityCandidateBatch::try_new(vec![node(0, "aaa"), node(1, "bb")], limits).is_err()
        );
    }

    #[test]
    fn verdict_batches_reject_unknown_or_misaligned_results() {
        let batch = VisibilityCandidateBatch::try_new(
            vec![VisibilityCandidate::Node {
                sequence: 7,
                table_oid: 1,
                source_key: "a".into(),
                node_idx: 0,
            }],
            VisibilityBatchLimits {
                max_candidates: 1,
                max_key_bytes: 1,
            },
        )
        .unwrap();
        assert!(
            VisibilityVerdictBatch::try_new(&batch, vec![(7, VisibilityVerdict::Visible)]).is_ok()
        );
        assert!(
            VisibilityVerdictBatch::try_new(&batch, vec![(8, VisibilityVerdict::Hidden)]).is_err()
        );
        assert!(
            VisibilityVerdictBatch::try_new(&batch, vec![(7, VisibilityVerdict::Unknown)]).is_err()
        );
    }

    #[test]
    fn statement_cache_reuses_hidden_and_visible_verdicts_without_reprobing() {
        let mut cache = VisibilityStatementCache::new(VisibilityCacheLimits {
            max_entries: 2,
            max_bytes: 256,
        });
        cache.try_record(1, VisibilityVerdict::Visible).unwrap();
        cache.try_record(2, VisibilityVerdict::Hidden).unwrap();
        let retained = cache.retained_bytes;
        cache.try_record(1, VisibilityVerdict::Visible).unwrap();
        cache.try_record(2, VisibilityVerdict::Hidden).unwrap();
        assert_eq!(cache.retained_bytes, retained);
    }

    #[test]
    fn deduplicated_probe_keys_keep_node_and_relationship_namespaces_distinct() {
        let batch = VisibilityCandidateBatch::try_new(
            vec![
                VisibilityCandidate::Node {
                    sequence: 0,
                    table_oid: 1,
                    source_key: "same".into(),
                    node_idx: 1,
                },
                VisibilityCandidate::Node {
                    sequence: 1,
                    table_oid: 2,
                    source_key: "same".into(),
                    node_idx: 2,
                },
                VisibilityCandidate::Relationship {
                    sequence: 2,
                    mapping_id: 1,
                    source_key: "same".into(),
                    relationship_id: Some(1),
                    edge_type: 1,
                },
                VisibilityCandidate::Relationship {
                    sequence: 3,
                    mapping_id: 2,
                    source_key: "same".into(),
                    relationship_id: Some(2),
                    edge_type: 1,
                },
            ],
            VisibilityBatchLimits {
                max_candidates: 4,
                max_key_bytes: 16,
            },
        )
        .unwrap();
        let probe = VisibilityProbeBatch::try_new(batch).unwrap();
        assert_eq!(
            probe.unique_node_keys,
            vec![(1, "same".into()), (2, "same".into())]
        );
        assert_eq!(
            probe.unique_relationship_keys,
            vec![(1, "same".into()), (2, "same".into())]
        );
        assert_eq!(probe.candidates.len(), 4);
    }

    #[test]
    fn coordinator_resolves_prepared_candidates_in_stable_order() {
        let mut hidden_nodes = RoaringBitmap::new();
        hidden_nodes.insert(2);
        let coordinator =
            VisibilityCoordinator::from_scope_for_test(VisibilityScope::enforced_for_test(
                hidden_nodes,
                RoaringBitmap::new(),
                RoaringBitmap::new(),
            ));
        let batch = VisibilityCandidateBatch::try_new(
            vec![
                VisibilityCandidate::Node {
                    sequence: 4,
                    table_oid: 1,
                    source_key: "a".into(),
                    node_idx: 1,
                },
                VisibilityCandidate::Node {
                    sequence: 5,
                    table_oid: 1,
                    source_key: "b".into(),
                    node_idx: 2,
                },
            ],
            VisibilityBatchLimits {
                max_candidates: 2,
                max_key_bytes: 2,
            },
        )
        .unwrap();
        let verdicts = coordinator.resolve_prepared_batch(&batch).unwrap();
        assert_eq!(
            verdicts.verdicts,
            vec![
                (4, VisibilityVerdict::Visible),
                (5, VisibilityVerdict::Hidden)
            ]
        );
    }

    #[test]
    fn relationship_candidates_fail_closed_without_a_durable_identity() {
        let mut rls_edge_types = RoaringBitmap::new();
        rls_edge_types.insert(3);
        let coordinator =
            VisibilityCoordinator::from_scope_for_test(VisibilityScope::enforced_for_test(
                RoaringBitmap::new(),
                RoaringBitmap::new(),
                rls_edge_types,
            ));
        let batch = VisibilityCandidateBatch::try_new(
            vec![VisibilityCandidate::Relationship {
                sequence: 0,
                mapping_id: 1,
                source_key: "edge-1".into(),
                relationship_id: None,
                edge_type: 3,
            }],
            VisibilityBatchLimits {
                max_candidates: 1,
                max_key_bytes: 6,
            },
        )
        .unwrap();
        assert!(matches!(
            coordinator.resolve_prepared_batch(&batch),
            Err(GraphError::RlsRelationshipIdentityMissing)
        ));
    }

    #[test]
    fn statement_cache_records_negative_verdicts_and_charges_count_and_bytes() {
        let mut cache = VisibilityStatementCache::new(VisibilityCacheLimits {
            max_entries: 2,
            max_bytes: 64,
        });
        assert_eq!(cache.node_verdict(1), VisibilityVerdict::Unknown);
        cache.try_record(1, VisibilityVerdict::Hidden).unwrap();
        cache.try_record(2, VisibilityVerdict::Visible).unwrap();
        assert_eq!(cache.node_verdict(1), VisibilityVerdict::Hidden);
        assert_eq!(cache.node_verdict(2), VisibilityVerdict::Visible);
        assert!(cache.try_record(3, VisibilityVerdict::Visible).is_err());
        assert!(cache.try_record(1, VisibilityVerdict::Unknown).is_err());
        assert_eq!(cache.relationship_verdict(9), VisibilityVerdict::Unknown);
    }

    #[test]
    fn deduplicated_probe_keys_preserve_first_seen_order_and_fan_out_verdicts() {
        let candidate = |sequence, key: &str, node_idx| VisibilityCandidate::Node {
            sequence,
            table_oid: 42,
            source_key: key.into(),
            node_idx,
        };
        let batch = VisibilityCandidateBatch::try_new(
            vec![
                candidate(0, "b", 1),
                candidate(1, "a", 2),
                candidate(2, "b", 1),
            ],
            VisibilityBatchLimits {
                max_candidates: 3,
                max_key_bytes: 3,
            },
        )
        .unwrap();
        let probe = VisibilityProbeBatch::try_new(batch).unwrap();
        assert_eq!(
            probe.unique_node_keys,
            vec![(42, "b".into()), (42, "a".into())]
        );
        assert_eq!(probe.candidates.len(), 3);
    }
}

#[cfg(test)]
#[path = "visibility/p1_contract_tests.rs"]
mod p1_contract_tests;

#[cfg(test)]
#[path = "visibility/p2_contract_tests.rs"]
mod p2_contract_tests;

#[cfg(test)]
#[path = "visibility/p3_contract_tests.rs"]
mod p3_contract_tests;

#[cfg(test)]
#[path = "visibility/p4_contract_tests.rs"]
mod p4_contract_tests;
