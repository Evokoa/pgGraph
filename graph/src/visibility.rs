//! Pure caller-visibility policy consumed by graph algorithms.
//!
//! PostgreSQL policy evaluation and SPI scans live in `sql_visibility`; this
//! module only owns compact query-scoped admission state.

use roaring::RoaringBitmap;

use crate::edge_store::RelationshipId;
use crate::safety::{GraphError, GraphResult};

/// Caller-visible intersection of the loaded projection and source-table RLS.
#[derive(Debug, Clone)]
pub(crate) struct VisibilityScope(VisibilityState);

#[derive(Debug, Clone)]
enum VisibilityState {
    /// No registered source relation applies RLS to the outer caller.
    Unrestricted,
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
    fn sequence(&self) -> u32 {
        match self {
            Self::Node { sequence, .. } | Self::Relationship { sequence, .. } => *sequence,
        }
    }
    fn key_bytes(&self) -> usize {
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
}

#[cfg(test)]
#[path = "visibility/p1_contract_tests.rs"]
mod p1_contract_tests;
