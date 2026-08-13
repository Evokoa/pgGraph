//! # BFS — Breadth-First Search hot loop
//!
//! The core traversal engine. It preallocates traversal state and uses a
//! VecDeque frontier + RoaringBitmap visited + adaptive parent/depth metadata
//! for path reconstruction.
//!
//! ## Performance Constraints
//!
//! - No source primary-key string comparisons in the inner loop
//! - Zero disk I/O
//! - All data accessed via contiguous arrays (cache-friendly)
//! - Circuit breakers: max_depth, max_nodes, max_frontier
//!
//! See: `docs/contributor_guide/traversal-search-paths.mdx`

use std::collections::{HashMap, HashSet, VecDeque};

use roaring::RoaringBitmap;

use crate::edge_store::EdgeStore;
use crate::filter_index::FilterIndex;
use crate::node_store::NodeStore;
use crate::projection::neighbors::{
    NeighborSource, OverlayDeletes, OverlayInserts, OverlayNeighbors,
};
use crate::safety::{GraphError, GraphResult};
use crate::types::{FilterOp, PathCoordinate, TableOid, TraversalResult};
#[cfg(any(test, feature = "benchmarks"))]
use crate::visibility::VisibilityCoordinator;
use crate::visibility::{QueryExecutionContext, VisibilityScope};

const SPARSE_METADATA_MIN_NODES: usize = 4_096;
const SPARSE_METADATA_RATIO: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BfsCandidateLimits {
    pub(crate) max_candidates: usize,
    pub(crate) max_key_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BfsProjectionEpoch {
    pub(crate) generation_id: Option<u64>,
    pub(crate) applied_sync_id: i64,
    pub(crate) node_count: u64,
    pub(crate) edge_count: u64,
    pub(crate) relationship_identity_count: u64,
    pub(crate) edge_buffer_len: u64,
    pub(crate) tx_added_nodes: u64,
    pub(crate) tx_added_edges: u64,
    pub(crate) tx_deleted_nodes: u64,
    pub(crate) tx_deleted_edges: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BfsAdjacencyCandidate {
    pub(crate) sequence: u32,
    pub(crate) parent_node: u32,
    pub(crate) parent_depth: i32,
    pub(crate) target_node: u32,
    pub(crate) target_table_oid: u32,
    pub(crate) target_source_key: String,
    pub(crate) edge_type: u8,
    pub(crate) relationship_id: Option<crate::edge_store::RelationshipId>,
    pub(crate) relationship_mapping_id: Option<u64>,
    pub(crate) relationship_source_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BfsAdjacencyCandidateBatch {
    pub(crate) candidates: Vec<BfsAdjacencyCandidate>,
    pub(crate) exhausted_current: bool,
}

#[derive(Debug)]
pub(crate) enum BfsMaterialization {
    Batch(BfsAdjacencyCandidateBatch),
    Progress,
    Complete,
}

impl BfsAdjacencyCandidateBatch {
    pub(crate) fn try_new(
        candidates: Vec<BfsAdjacencyCandidate>,
        exhausted_current: bool,
        limits: BfsCandidateLimits,
    ) -> GraphResult<Self> {
        if limits.max_candidates == 0 || limits.max_key_bytes == 0 {
            return Err(GraphError::InvalidFilter {
                reason: "BFS visibility candidate limits must be positive".into(),
            });
        }
        if candidates.len() > limits.max_candidates {
            return Err(GraphError::InvalidFilter {
                reason: "BFS visibility candidate count exceeds batch limit".into(),
            });
        }
        let mut bytes = 0usize;
        let mut previous = None;
        for candidate in &candidates {
            if previous.is_some_and(|sequence| candidate.sequence <= sequence) {
                return Err(GraphError::InvalidFilter {
                    reason: "BFS visibility candidate sequence must increase".into(),
                });
            }
            previous = Some(candidate.sequence);
            bytes = bytes
                .checked_add(candidate.target_source_key.len())
                .and_then(|value| {
                    value.checked_add(
                        candidate
                            .relationship_source_key
                            .as_ref()
                            .map_or(0, String::len),
                    )
                })
                .ok_or_else(|| GraphError::InvalidFilter {
                    reason: "BFS visibility candidate key bytes overflow".into(),
                })?;
            if bytes > limits.max_key_bytes {
                return Err(GraphError::InvalidFilter {
                    reason: "BFS visibility candidate key bytes exceed batch limit".into(),
                });
            }
        }
        Ok(Self {
            candidates,
            exhausted_current,
        })
    }
}

#[derive(Debug)]
pub(crate) enum ResumableBfsState {
    NeedCandidates,
    NeedVisibility,
    ReadyToAdmit,
    Complete,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BfsAdjacencyVerdict {
    pub(crate) sequence: u32,
    pub(crate) node_visible: bool,
    pub(crate) relationship_visible: bool,
}

impl BfsAdjacencyVerdict {
    pub(crate) fn visible(self) -> bool {
        self.node_visible && self.relationship_visible
    }
}

pub(crate) struct ResumableBfsMachine {
    pub(crate) frontier: VecDeque<u32>,
    pub(crate) visited: RoaringBitmap,
    pub(crate) depth: TraversalDepthMap,
    pub(crate) parent: TraversalParentMap,
    pub(crate) parent_edge_type: TraversalParentEdgeTypes,
    pub(crate) outputs: Vec<u32>,
    pub(crate) adjacency_cursor: usize,
    pub(crate) state: ResumableBfsState,
    nodes_visited: u32,
    active_node: Option<u32>,
    next_sequence: u32,
    pending_batch: Option<(Option<u32>, usize)>,
    truncated: bool,
    projection_epoch: Option<BfsProjectionEpoch>,
}

impl ResumableBfsMachine {
    pub(crate) fn try_new(node_count: usize, config: &BfsConfig) -> GraphResult<Self> {
        let sparse = use_sparse_metadata(node_count, config.max_nodes);
        let expected = expected_visit_capacity(node_count, config.max_nodes);
        let mut frontier = VecDeque::new();
        frontier
            .try_reserve(config.max_frontier as usize)
            .map_err(traversal_allocation_error)?;
        let mut visited = RoaringBitmap::new();
        let mut depth = TraversalDepthMap::try_new(node_count, sparse, expected)?;
        let mut parent = TraversalParentMap::try_new(node_count, sparse, expected)?;
        let mut parent_edge_type = TraversalParentEdgeTypes::try_new(node_count, sparse, expected)?;
        let seed_valid = (config.seed_node as usize) < node_count;
        if seed_valid {
            visited.insert(config.seed_node);
            depth.set(config.seed_node, 0);
            parent.set(config.seed_node, config.seed_node);
            parent_edge_type.set(config.seed_node, 0);
        }
        let complete_without_expansion = !seed_valid
            || config.max_depth <= 0
            || matches!(
                config.edge_type_filter,
                crate::types::EdgeTypeFilter::NoneMatched
            );
        if !complete_without_expansion {
            frontier.push_back(config.seed_node);
        }
        let mut outputs = Vec::new();
        outputs
            .try_reserve(expected.max(1))
            .map_err(traversal_allocation_error)?;
        if seed_valid {
            outputs.push(config.seed_node);
        }
        Ok(Self {
            frontier,
            visited,
            depth,
            parent,
            parent_edge_type,
            outputs,
            adjacency_cursor: 0,
            state: if complete_without_expansion {
                ResumableBfsState::Complete
            } else {
                ResumableBfsState::NeedCandidates
            },
            nodes_visited: u32::from(seed_valid),
            active_node: None,
            next_sequence: 0,
            pending_batch: None,
            truncated: false,
            projection_epoch: None,
        })
    }

    pub(crate) fn bind_projection_epoch(&mut self, epoch: BfsProjectionEpoch) {
        self.projection_epoch = Some(epoch);
    }

    pub(crate) fn require_projection_epoch(&self, actual: BfsProjectionEpoch) -> GraphResult<()> {
        if self.projection_epoch == Some(actual) {
            Ok(())
        } else {
            Err(GraphError::Internal(
                "projection changed while caller visibility was being resolved; retry the graph query"
                    .into(),
            ))
        }
    }

    #[cfg(test)]
    fn take_candidate_batch(
        &mut self,
        node_store: &NodeStore,
        neighbors: &impl NeighborSource,
        relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
        config: &BfsConfig,
        limits: BfsCandidateLimits,
        governor: &crate::resource::ResourceGovernor,
    ) -> GraphResult<Option<BfsAdjacencyCandidateBatch>> {
        loop {
            match materialize_bfs_candidate_batch(
                self,
                node_store,
                neighbors,
                relationships,
                config,
                limits,
                governor,
            )? {
                BfsMaterialization::Batch(batch) => return Ok(Some(batch)),
                BfsMaterialization::Progress => continue,
                BfsMaterialization::Complete => return Ok(None),
            }
        }
    }

    pub(crate) fn apply_visibility_verdicts(
        &mut self,
        batch: &BfsAdjacencyCandidateBatch,
        verdicts: &[BfsAdjacencyVerdict],
        node_store: &NodeStore,
        filter_index: &FilterIndex,
        config: &BfsConfig,
    ) -> GraphResult<()> {
        if !matches!(self.state, ResumableBfsState::NeedVisibility) {
            return Err(GraphError::Internal(
                "BFS machine received verdicts outside its visibility state".into(),
            ));
        }
        if verdicts.len() != batch.candidates.len() {
            return Err(GraphError::Internal(
                "BFS visibility verdict count mismatch".into(),
            ));
        }
        let actual_batch = (
            batch.candidates.first().map(|candidate| candidate.sequence),
            batch.candidates.len(),
        );
        if self.pending_batch != Some(actual_batch) {
            return Err(GraphError::Internal(
                "BFS machine received verdicts for a different candidate batch".into(),
            ));
        }
        if batch
            .candidates
            .iter()
            .zip(verdicts)
            .any(|(candidate, verdict)| candidate.sequence != verdict.sequence)
        {
            return Err(GraphError::Internal(
                "BFS visibility verdict order mismatch".into(),
            ));
        }
        self.state = ResumableBfsState::ReadyToAdmit;
        self.pending_batch = None;
        let has_filters = !config.filter_ops.is_empty();
        for (candidate, verdict) in batch.candidates.iter().zip(verdicts) {
            if !verdict.visible()
                || self.visited.contains(candidate.target_node)
                || !node_store.is_active(candidate.target_node)
                || crate::projection::tx_delta::node_deleted(candidate.target_node)
            {
                continue;
            }
            if let Some(tenant) = config.tenant.as_deref() {
                if node_store
                    .table_oid(candidate.target_node)
                    .is_some_and(|table_oid| {
                        config.tenanted_table_oids.contains(&table_oid)
                            && !(config
                                .tenant_membership
                                .get(tenant)
                                .is_some_and(|bitmap| bitmap.contains(candidate.target_node))
                                || (filter_index
                                    .mapped_tenant_contains(tenant, candidate.target_node)
                                    && !config.tenant_membership_removals.get(tenant).is_some_and(
                                        |bitmap| bitmap.contains(candidate.target_node),
                                    )))
                    })
                {
                    continue;
                }
            }
            if has_filters && !filter_index.check_filters(candidate.target_node, &config.filter_ops)
            {
                continue;
            }
            self.visited.insert(candidate.target_node);
            self.depth
                .set(candidate.target_node, candidate.parent_depth + 1);
            self.parent
                .set(candidate.target_node, candidate.parent_node);
            self.parent_edge_type
                .set(candidate.target_node, candidate.edge_type);
            self.outputs.push(candidate.target_node);
            self.nodes_visited = self.nodes_visited.saturating_add(1);
            if self.nodes_visited >= config.max_nodes {
                self.truncated = true;
                self.state = ResumableBfsState::Complete;
                return Ok(());
            }
            if candidate.parent_depth + 1 < config.max_depth {
                self.frontier.push_back(candidate.target_node);
                if self.frontier.len() as u32 >= config.max_frontier {
                    self.truncated = true;
                    self.state = ResumableBfsState::Complete;
                    return Ok(());
                }
            }
        }
        self.state = ResumableBfsState::NeedCandidates;
        Ok(())
    }

    pub(crate) fn is_complete(&self) -> bool {
        matches!(self.state, ResumableBfsState::Complete)
    }

    /// Bound the next policy probe by the earliest admission cap that could
    /// complete the eager traversal. Static and visibility rejects simply
    /// cause another bounded probe, so candidates after a cap are never
    /// resolved speculatively.
    pub(crate) fn next_candidate_limit(&self, configured: usize, config: &BfsConfig) -> usize {
        let remaining_nodes = config.max_nodes.saturating_sub(self.nodes_visited).max(1);
        let remaining_frontier = config
            .max_frontier
            .saturating_sub(u32::try_from(self.frontier.len()).unwrap_or(u32::MAX))
            .max(1);
        configured
            .min(remaining_nodes as usize)
            .min(remaining_frontier as usize)
            .max(1)
    }

    pub(crate) fn finish(self) -> BfsResult {
        BfsResult {
            visited: self.visited,
            depth: self.depth,
            parent: self.parent,
            parent_edge_type: self.parent_edge_type,
            truncated: self.truncated,
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "bounded candidate materialization needs machine, projection, identity, configuration, and governor state"
)]
pub(crate) fn materialize_bfs_candidate_batch(
    machine: &mut ResumableBfsMachine,
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
    config: &BfsConfig,
    limits: BfsCandidateLimits,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<BfsMaterialization> {
    if !matches!(machine.state, ResumableBfsState::NeedCandidates) {
        return Err(GraphError::Internal(
            "BFS candidate materialization requires NeedCandidates state".into(),
        ));
    }
    if limits.max_candidates == 0 || limits.max_key_bytes == 0 {
        return Err(GraphError::InvalidFilter {
            reason: "BFS visibility candidate limits must be positive".into(),
        });
    }
    let allocation_bytes = limits
        .max_candidates
        .checked_mul(std::mem::size_of::<BfsAdjacencyCandidate>())
        .and_then(|bytes| bytes.checked_add(limits.max_key_bytes))
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "BFS visibility candidate allocation estimate overflow".into(),
        })?;
    let _lease = governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryVisibility,
            crate::resource::ByteCount::from_bytes(
                u64::try_from(allocation_bytes).unwrap_or(u64::MAX),
            ),
        )
        .map_err(crate::safety::resource_limit_error)?;

    loop {
        let current = if let Some(current) = machine.active_node {
            current
        } else if let Some(current) = machine.frontier.pop_front() {
            machine.active_node = Some(current);
            machine.adjacency_cursor = 0;
            current
        } else {
            machine.state = ResumableBfsState::Complete;
            return Ok(BfsMaterialization::Complete);
        };
        let current_depth = machine.depth.get(current).unwrap_or(-1);
        if current_depth >= config.max_depth {
            machine.active_node = None;
            machine.adjacency_cursor = 0;
            continue;
        }

        let mut candidates = Vec::new();
        candidates
            .try_reserve(limits.max_candidates)
            .map_err(traversal_allocation_error)?;
        let mut key_bytes = 0usize;
        let mut adjacency = Vec::new();
        adjacency
            .try_reserve(limits.max_candidates)
            .map_err(traversal_allocation_error)?;
        let exhausted_current = neighbors.fill_neighbors(
            current,
            machine.adjacency_cursor,
            limits.max_candidates,
            &mut adjacency,
        );
        let mut yielded_for_key_bytes = false;
        for neighbor in adjacency {
            match &config.edge_type_filter {
                crate::types::EdgeTypeFilter::NoneMatched => {
                    consume_expansion_without_interrupt(governor)?;
                    machine.adjacency_cursor =
                        machine.adjacency_cursor.checked_add(1).ok_or_else(|| {
                            GraphError::Internal("BFS adjacency cursor overflow".into())
                        })?;
                    continue;
                }
                crate::types::EdgeTypeFilter::Only(allowed)
                    if !allowed.contains(&neighbor.type_id) =>
                {
                    consume_expansion_without_interrupt(governor)?;
                    machine.adjacency_cursor =
                        machine.adjacency_cursor.checked_add(1).ok_or_else(|| {
                            GraphError::Internal("BFS adjacency cursor overflow".into())
                        })?;
                    continue;
                }
                crate::types::EdgeTypeFilter::All | crate::types::EdgeTypeFilter::Only(_) => {}
            }

            let target_table_oid =
                node_store
                    .table_oid(neighbor.target)
                    .ok_or_else(|| GraphError::CorruptFile {
                        reason: format!("node {} has no table identity", neighbor.target),
                    })?;
            let target_source_key =
                node_store
                    .primary_key(neighbor.target)
                    .ok_or_else(|| GraphError::CorruptFile {
                        reason: format!("node {} has no source identity", neighbor.target),
                    })?;
            let relationship_key_bytes = neighbor
                .relationship_id
                .and_then(|relationship_id| {
                    relationship_identity(relationships, relationship_id, |_, source_key| {
                        source_key.len()
                    })
                })
                .unwrap_or_default();
            let candidate_key_bytes = target_source_key
                .len()
                .checked_add(relationship_key_bytes)
                .ok_or_else(|| GraphError::InvalidFilter {
                    reason: "BFS visibility candidate key bytes overflow".into(),
                })?;
            let next_key_bytes = key_bytes.checked_add(candidate_key_bytes).ok_or_else(|| {
                GraphError::InvalidFilter {
                    reason: "BFS visibility candidate key bytes overflow".into(),
                }
            })?;
            if next_key_bytes > limits.max_key_bytes && candidates.is_empty() {
                return Err(GraphError::InvalidFilter {
                    reason: "one BFS visibility candidate exceeds the key-byte limit".into(),
                });
            }
            if next_key_bytes > limits.max_key_bytes {
                yielded_for_key_bytes = true;
                break;
            }
            consume_expansion_without_interrupt(governor)?;
            machine.adjacency_cursor = machine
                .adjacency_cursor
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("BFS adjacency cursor overflow".into()))?;
            key_bytes = next_key_bytes;
            let relationship_identity = neighbor.relationship_id.and_then(|relationship_id| {
                relationship_identity(relationships, relationship_id, |mapping_id, source_key| {
                    (mapping_id, source_key.to_owned())
                })
            });
            candidates.push(BfsAdjacencyCandidate {
                sequence: machine.next_sequence,
                parent_node: current,
                parent_depth: current_depth,
                target_node: neighbor.target,
                target_table_oid,
                target_source_key: target_source_key.to_owned(),
                edge_type: neighbor.type_id,
                relationship_id: neighbor.relationship_id,
                relationship_mapping_id: relationship_identity.as_ref().map(|value| value.0),
                relationship_source_key: relationship_identity.map(|value| value.1),
            });
            machine.next_sequence = machine
                .next_sequence
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("BFS visibility sequence overflow".into()))?;
        }
        let exhausted_current = exhausted_current && !yielded_for_key_bytes;
        if exhausted_current {
            machine.active_node = None;
            machine.adjacency_cursor = 0;
        }
        if candidates.is_empty() {
            // A leaf or an all-filtered raw page has no policy work. Advance
            // without constructing an invalid zero-sized probe. Returning a
            // progress yield also releases the ENGINE borrow for interrupts.
            return Ok(if machine.is_complete() {
                BfsMaterialization::Complete
            } else {
                BfsMaterialization::Progress
            });
        }
        let batch = BfsAdjacencyCandidateBatch::try_new(candidates, exhausted_current, limits)?;
        machine.pending_batch = Some((
            batch.candidates.first().map(|candidate| candidate.sequence),
            batch.candidates.len(),
        ));
        machine.state = ResumableBfsState::NeedVisibility;
        return Ok(BfsMaterialization::Batch(batch));
    }
}

fn relationship_identity<T>(
    relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
    relationship_id: crate::edge_store::RelationshipId,
    read: impl FnOnce(u64, &str) -> T,
) -> Option<T> {
    if let Some(identity) = relationships.get(relationship_id) {
        return Some(read(identity.mapping_id, identity.source_key));
    }
    let offset = usize::try_from(relationship_id)
        .ok()?
        .checked_sub(relationships.len())?;
    crate::projection::tx_delta::with_relationship_identity(offset, |identity| {
        identity.map(|identity| read(identity.mapping_id, &identity.source_key))
    })
}

/// Configuration for a BFS traversal.
pub struct BfsConfig {
    /// Node index where traversal starts.
    pub seed_node: u32,
    /// Maximum number of hops to expand from the seed.
    pub max_depth: i32,
    /// Maximum number of nodes that may be visited before the circuit breaker stops expansion.
    pub max_nodes: u32,
    /// Maximum queued frontier size before the circuit breaker stops expansion.
    pub max_frontier: u32,
    /// Edge type restriction resolved before entering the hot loop.
    pub edge_type_filter: crate::types::EdgeTypeFilter,
    /// Registered filter-column predicates evaluated during expansion.
    pub filter_ops: Vec<FilterOp>,
    /// Tenant identifier used for topology scoping, when requested.
    pub tenant: Option<String>,
    /// Table OIDs that participate in tenant membership filtering.
    pub tenanted_table_oids: HashSet<u32>,
    /// Per-tenant bitmap of allowed node indices.
    pub tenant_membership: HashMap<String, RoaringBitmap>,
    /// Nodes removed from an immutable mapped tenant base by later deltas.
    pub tenant_membership_removals: HashMap<String, RoaringBitmap>,
    /// Sync overlay edges inserted after the last base build, keyed by source node.
    pub overlay_insert_edges: OverlayInserts,
    /// Sync overlay edges deleted after the last base build, keyed by source node.
    pub overlay_deleted_edges: OverlayDeletes,
}

/// Result of BFS: discovered nodes with parent tracking for path reconstruction.
pub struct BfsResult {
    /// All visited node indices (including seed).
    pub visited: RoaringBitmap,
    /// Depth metadata for visited nodes.
    pub depth: TraversalDepthMap,
    /// Parent of each visited node for path reconstruction.
    pub parent: TraversalParentMap,
    /// Edge type used by `parent[i] -> i`.
    pub parent_edge_type: TraversalParentEdgeTypes,
    /// `true` when `max_nodes` or `max_frontier` stopped expansion before the
    /// frontier was naturally exhausted, meaning nodes reachable within
    /// `max_depth` may not have been visited. `false` for an empty seed, a
    /// filter that matched no edge types, or natural exhaustion.
    pub truncated: bool,
}

/// Traversal depth metadata.
pub enum TraversalDepthMap {
    /// Dense mode stores one depth slot per graph node for fast result conversion.
    Dense(Vec<i32>),
    /// Sparse mode stores only visited nodes for low-visit-budget traversals on large graphs.
    Sparse(HashMap<u32, i32>),
}

impl TraversalDepthMap {
    fn try_new(node_count: usize, sparse: bool, expected_visits: usize) -> GraphResult<Self> {
        if sparse {
            let mut depths = HashMap::new();
            depths
                .try_reserve(expected_visits)
                .map_err(traversal_allocation_error)?;
            Ok(Self::Sparse(depths))
        } else {
            Ok(Self::Dense(try_filled_vec(node_count, -1i32)?))
        }
    }

    fn set(&mut self, node_idx: u32, depth: i32) {
        match self {
            Self::Dense(depths) => depths[node_idx as usize] = depth,
            Self::Sparse(depths) => {
                depths.insert(node_idx, depth);
            }
        }
    }

    fn get(&self, node_idx: u32) -> Option<i32> {
        match self {
            Self::Dense(depths) => depths
                .get(node_idx as usize)
                .copied()
                .filter(|depth| *depth >= 0),
            Self::Sparse(depths) => depths.get(&node_idx).copied(),
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        match self {
            Self::Dense(depths) => depths.len(),
            Self::Sparse(depths) => depths.len(),
        }
    }

    #[cfg(test)]
    fn all_unvisited(&self) -> bool {
        match self {
            Self::Dense(depths) => depths.iter().all(|depth| *depth == -1),
            Self::Sparse(depths) => depths.is_empty(),
        }
    }
}

/// Parent node metadata for traversal path reconstruction.
pub enum TraversalParentMap {
    /// Dense mode stores one parent slot per graph node.
    Dense(Vec<u32>),
    /// Sparse mode stores parents only for visited nodes.
    Sparse(HashMap<u32, u32>),
}

impl TraversalParentMap {
    fn try_new(node_count: usize, sparse: bool, expected_visits: usize) -> GraphResult<Self> {
        if sparse {
            let mut parents = HashMap::new();
            parents
                .try_reserve(expected_visits)
                .map_err(traversal_allocation_error)?;
            Ok(Self::Sparse(parents))
        } else {
            Ok(Self::Dense(try_filled_vec(node_count, u32::MAX)?))
        }
    }

    fn set(&mut self, node_idx: u32, parent: u32) {
        match self {
            Self::Dense(parents) => parents[node_idx as usize] = parent,
            Self::Sparse(parents) => {
                parents.insert(node_idx, parent);
            }
        }
    }

    fn get(&self, node_idx: u32) -> Option<u32> {
        match self {
            Self::Dense(parents) => parents
                .get(node_idx as usize)
                .copied()
                .filter(|parent| *parent != u32::MAX),
            Self::Sparse(parents) => parents.get(&node_idx).copied(),
        }
    }

    #[cfg(test)]
    fn all_unvisited(&self) -> bool {
        match self {
            Self::Dense(parents) => parents.iter().all(|parent| *parent == u32::MAX),
            Self::Sparse(parents) => parents.is_empty(),
        }
    }
}

/// Parent edge-type metadata for traversal path reconstruction.
pub enum TraversalParentEdgeTypes {
    /// Dense mode stores one edge-type slot per graph node.
    Dense(Vec<u8>),
    /// Sparse mode stores edge types only for visited nodes.
    Sparse(HashMap<u32, u8>),
}

impl TraversalParentEdgeTypes {
    fn try_new(node_count: usize, sparse: bool, expected_visits: usize) -> GraphResult<Self> {
        if sparse {
            let mut edge_types = HashMap::new();
            edge_types
                .try_reserve(expected_visits)
                .map_err(traversal_allocation_error)?;
            Ok(Self::Sparse(edge_types))
        } else {
            Ok(Self::Dense(try_filled_vec(node_count, 0u8)?))
        }
    }

    fn set(&mut self, node_idx: u32, edge_type: u8) {
        match self {
            Self::Dense(edge_types) => edge_types[node_idx as usize] = edge_type,
            Self::Sparse(edge_types) => {
                edge_types.insert(node_idx, edge_type);
            }
        }
    }

    fn get(&self, node_idx: u32) -> Option<u8> {
        match self {
            Self::Dense(edge_types) => edge_types.get(node_idx as usize).copied(),
            Self::Sparse(edge_types) => edge_types.get(&node_idx).copied(),
        }
    }
}

fn try_filled_vec<T: Clone>(len: usize, value: T) -> GraphResult<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(traversal_allocation_error)?;
    values.resize(len, value);
    Ok(values)
}

fn traversal_allocation_error(_error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::QUERY_MEMORY_MB.get().max(1) as u64,
    }
}

fn expected_visit_capacity(node_count: usize, max_nodes: u32) -> usize {
    usize::try_from(max_nodes)
        .unwrap_or(usize::MAX)
        .min(node_count)
}

fn use_sparse_metadata(node_count: usize, max_nodes: u32) -> bool {
    let expected_visits = expected_visit_capacity(node_count, max_nodes);
    node_count >= SPARSE_METADATA_MIN_NODES
        && expected_visits.saturating_mul(SPARSE_METADATA_RATIO) < node_count
}

/// Estimate the traversal metadata and frontier workspace reserved up front.
pub(crate) fn estimated_workspace_bytes(
    node_count: usize,
    max_nodes: u32,
    max_frontier: u32,
) -> GraphResult<crate::resource::ByteCount> {
    let visits = expected_visit_capacity(node_count, max_nodes);
    let metadata_per_node = if use_sparse_metadata(node_count, max_nodes) {
        128usize
    } else {
        std::mem::size_of::<i32>() + std::mem::size_of::<u32>() + std::mem::size_of::<u8>()
    };
    let metadata_nodes = if use_sparse_metadata(node_count, max_nodes) {
        visits
    } else {
        node_count
    };
    metadata_nodes
        .checked_mul(metadata_per_node)
        .and_then(|bytes| bytes.checked_add(visits.checked_mul(8)?))
        .and_then(|bytes| {
            bytes.checked_add(
                usize::try_from(max_frontier)
                    .ok()?
                    .checked_mul(std::mem::size_of::<u32>())?,
            )
        })
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("traversal workspace estimate overflowed".to_string()))
}

/// Execute BFS traversal from a seed node.
///
/// # Arguments
/// * `node_store` — SoA node data (active state and source metadata)
/// * `edge_store` — CSR edge data (neighbors)
/// * `filter_index` — typed filter column data
/// * `config` — BFS parameters
///
/// # Returns
/// BfsResult containing visited set, depth map, and parent metadata.
#[inline]
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "legacy test and benchmark wrapper has no resource governor; production uses execute_governed"
)]
pub fn execute(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
) -> BfsResult {
    execute_inner(
        node_store,
        edge_store,
        filter_index,
        config,
        None,
        VisibilityCoordinator::unrestricted_for_test_or_benchmark().scope(),
    )
    .expect("unbounded traversal accounting should not fail")
}

#[allow(
    clippy::expect_used,
    reason = "Criterion bridge uses an unbounded governor-free traversal"
)]
#[cfg(any(test, feature = "benchmarks"))]
pub(crate) fn execute_for_benchmark(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    proof: &crate::bench_support::BenchmarkVisibilityProof,
) -> BfsResult {
    let coordinator = VisibilityCoordinator::unrestricted_for_benchmark(proof);
    execute_inner(
        node_store,
        edge_store,
        filter_index,
        config,
        None,
        coordinator.scope_for_benchmark(proof),
    )
    .expect("unbounded benchmark traversal accounting should not fail")
}

#[cfg(test)]
#[allow(dead_code, reason = "legacy test compatibility entry point")]
pub(crate) fn execute_governed(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<BfsResult> {
    execute_inner(
        node_store,
        edge_store,
        filter_index,
        config,
        Some(governor),
        VisibilityCoordinator::unrestricted_for_test_or_benchmark().scope(),
    )
}

pub(crate) fn execute_governed_with_context(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    context: &QueryExecutionContext<'_>,
) -> GraphResult<BfsResult> {
    execute_inner(
        node_store,
        edge_store,
        filter_index,
        config,
        Some(context.governor),
        context.visibility,
    )
}

fn execute_inner(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    governor: Option<&crate::resource::ResourceGovernor>,
    visibility: &VisibilityScope,
) -> GraphResult<BfsResult> {
    let node_count = node_store.node_count() as usize;
    let sparse_metadata = use_sparse_metadata(node_count, config.max_nodes);
    let expected_visits = expected_visit_capacity(node_count, config.max_nodes);

    let mut visited = RoaringBitmap::new();
    let mut depth_map = TraversalDepthMap::try_new(node_count, sparse_metadata, expected_visits)?;
    let mut parent = TraversalParentMap::try_new(node_count, sparse_metadata, expected_visits)?;
    let mut parent_edge_type =
        TraversalParentEdgeTypes::try_new(node_count, sparse_metadata, expected_visits)?;
    if config.seed_node as usize >= node_count || !visibility.allows_node(config.seed_node) {
        return Ok(BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
            truncated: false,
        });
    }

    let mut frontier = VecDeque::new();
    frontier
        .try_reserve(config.max_frontier as usize)
        .map_err(traversal_allocation_error)?;
    let mut nodes_visited: u32 = 0;

    let seed = config.seed_node;
    visited.insert(seed);
    depth_map.set(seed, 0);
    parent.set(seed, seed);
    parent_edge_type.set(seed, 0);
    frontier.push_back(seed);
    nodes_visited += 1;

    if matches!(
        config.edge_type_filter,
        crate::types::EdgeTypeFilter::NoneMatched
    ) {
        return Ok(BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
            truncated: false,
        });
    }

    let has_filters = !config.filter_ops.is_empty();
    let neighbors = OverlayNeighbors::new(
        edge_store,
        &config.overlay_insert_edges,
        &config.overlay_deleted_edges,
    );

    while let Some(current) = frontier.pop_front() {
        let current_depth = depth_map.get(current).unwrap_or(-1);

        if current_depth >= config.max_depth {
            continue;
        }

        for neighbor in neighbors.neighbors(current) {
            consume_expansion(governor)?;
            if !candidate_allowed(
                node_store,
                filter_index,
                config,
                neighbor.target,
                neighbor.type_id,
                neighbor.relationship_id,
                &visited,
                has_filters,
                visibility,
            )? {
                continue;
            }

            visited.insert(neighbor.target);
            depth_map.set(neighbor.target, current_depth + 1);
            parent.set(neighbor.target, current);
            parent_edge_type.set(neighbor.target, neighbor.type_id);
            nodes_visited += 1;

            if nodes_visited >= config.max_nodes {
                return Ok(BfsResult {
                    visited,
                    depth: depth_map,
                    parent,
                    parent_edge_type,
                    truncated: true,
                });
            }

            if current_depth + 1 < config.max_depth {
                frontier.push_back(neighbor.target);

                if frontier.len() as u32 >= config.max_frontier {
                    return Ok(BfsResult {
                        visited,
                        depth: depth_map,
                        parent,
                        parent_edge_type,
                        truncated: true,
                    });
                }
            }
        }
    }

    Ok(BfsResult {
        visited,
        depth: depth_map,
        parent,
        parent_edge_type,
        truncated: false,
    })
}

/// Execute BFS traversal over a supplied neighbor source.
#[inline]
#[cfg(test)]
#[allow(
    dead_code,
    clippy::expect_used,
    reason = "legacy test and benchmark wrapper has no resource governor; production uses execute_with_neighbors_governed"
)]
pub(crate) fn execute_with_neighbors(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
) -> BfsResult {
    execute_with_neighbors_inner(
        node_store,
        neighbors,
        filter_index,
        config,
        None,
        VisibilityCoordinator::unrestricted_for_test_or_benchmark().scope(),
    )
    .expect("unbounded traversal accounting should not fail")
}

#[allow(
    clippy::expect_used,
    reason = "Criterion bridge uses an unbounded governor-free traversal"
)]
#[cfg(any(test, feature = "benchmarks"))]
pub(crate) fn execute_with_neighbors_for_benchmark(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    proof: &crate::bench_support::BenchmarkVisibilityProof,
) -> BfsResult {
    let coordinator = VisibilityCoordinator::unrestricted_for_benchmark(proof);
    execute_with_neighbors_inner(
        node_store,
        neighbors,
        filter_index,
        config,
        None,
        coordinator.scope_for_benchmark(proof),
    )
    .expect("unbounded layered benchmark traversal accounting should not fail")
}

#[cfg(test)]
#[allow(dead_code, reason = "legacy test compatibility entry point")]
pub(crate) fn execute_with_neighbors_governed(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<BfsResult> {
    execute_with_neighbors_inner(
        node_store,
        neighbors,
        filter_index,
        config,
        Some(governor),
        VisibilityCoordinator::unrestricted_for_test_or_benchmark().scope(),
    )
}

pub(crate) fn execute_with_neighbors_governed_with_context(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    context: &QueryExecutionContext<'_>,
) -> GraphResult<BfsResult> {
    execute_with_neighbors_inner(
        node_store,
        neighbors,
        filter_index,
        config,
        Some(context.governor),
        context.visibility,
    )
}

fn execute_with_neighbors_inner(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    governor: Option<&crate::resource::ResourceGovernor>,
    visibility: &VisibilityScope,
) -> GraphResult<BfsResult> {
    let node_count = node_store.node_count() as usize;
    let sparse_metadata = use_sparse_metadata(node_count, config.max_nodes);
    let expected_visits = expected_visit_capacity(node_count, config.max_nodes);

    let mut visited = RoaringBitmap::new();
    let mut depth_map = TraversalDepthMap::try_new(node_count, sparse_metadata, expected_visits)?;
    let mut parent = TraversalParentMap::try_new(node_count, sparse_metadata, expected_visits)?;
    let mut parent_edge_type =
        TraversalParentEdgeTypes::try_new(node_count, sparse_metadata, expected_visits)?;
    if config.seed_node as usize >= node_count || !visibility.allows_node(config.seed_node) {
        return Ok(BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
            truncated: false,
        });
    }

    let mut frontier = VecDeque::new();
    frontier
        .try_reserve(config.max_frontier as usize)
        .map_err(traversal_allocation_error)?;
    let mut nodes_visited: u32 = 0;

    // Seed the BFS
    let seed = config.seed_node;
    visited.insert(seed);
    depth_map.set(seed, 0);
    parent.set(seed, seed); // Self-referential root
    parent_edge_type.set(seed, 0);
    frontier.push_back(seed);
    nodes_visited += 1;

    if matches!(
        config.edge_type_filter,
        crate::types::EdgeTypeFilter::NoneMatched
    ) {
        return Ok(BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
            truncated: false,
        });
    }

    let has_filters = !config.filter_ops.is_empty();
    // BFS loop — traversal state is allocated before this point.
    while let Some(current) = frontier.pop_front() {
        let current_depth = depth_map.get(current).unwrap_or(-1);

        // Depth limit check
        if current_depth >= config.max_depth {
            continue;
        }

        for neighbor in neighbors.neighbors(current) {
            consume_expansion(governor)?;
            if !candidate_allowed(
                node_store,
                filter_index,
                config,
                neighbor.target,
                neighbor.type_id,
                neighbor.relationship_id,
                &visited,
                has_filters,
                visibility,
            )? {
                continue;
            }

            visited.insert(neighbor.target);
            depth_map.set(neighbor.target, current_depth + 1);
            parent.set(neighbor.target, current);
            parent_edge_type.set(neighbor.target, neighbor.type_id);
            nodes_visited += 1;

            // Circuit breakers
            if nodes_visited >= config.max_nodes {
                return Ok(BfsResult {
                    visited,
                    depth: depth_map,
                    parent,
                    parent_edge_type,
                    truncated: true,
                });
            }

            // Only add to frontier if we haven't hit the depth limit
            if current_depth + 1 < config.max_depth {
                frontier.push_back(neighbor.target);

                // Frontier size circuit breaker
                if frontier.len() as u32 >= config.max_frontier {
                    return Ok(BfsResult {
                        visited,
                        depth: depth_map,
                        parent,
                        parent_edge_type,
                        truncated: true,
                    });
                }
            }
        }
    }

    Ok(BfsResult {
        visited,
        depth: depth_map,
        parent,
        parent_edge_type,
        truncated: false,
    })
}

/// Execute depth-first traversal from a seed node.
#[inline]
#[cfg(test)]
pub fn execute_dfs(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
) -> BfsResult {
    let neighbors = OverlayNeighbors::new(
        edge_store,
        &config.overlay_insert_edges,
        &config.overlay_deleted_edges,
    );
    execute_dfs_with_neighbors(node_store, &neighbors, filter_index, config)
}

#[cfg(test)]
#[allow(dead_code, reason = "legacy test compatibility entry point")]
pub(crate) fn execute_dfs_governed(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<BfsResult> {
    let coordinator = VisibilityCoordinator::unrestricted_for_test_or_benchmark();
    let context = coordinator.context(governor);
    execute_dfs_governed_with_context(node_store, edge_store, filter_index, config, &context)
}

pub(crate) fn execute_dfs_governed_with_context(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    context: &crate::visibility::QueryExecutionContext<'_>,
) -> GraphResult<BfsResult> {
    let neighbors = OverlayNeighbors::new(
        edge_store,
        &config.overlay_insert_edges,
        &config.overlay_deleted_edges,
    );
    execute_dfs_with_neighbors_governed_with_context(
        node_store,
        &neighbors,
        filter_index,
        config,
        context,
    )
}

/// Execute DFS traversal over a supplied neighbor source.
#[inline]
#[cfg(test)]
pub(crate) fn execute_dfs_with_neighbors(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
) -> BfsResult {
    execute_dfs_with_neighbors_inner(
        node_store,
        neighbors,
        filter_index,
        config,
        None,
        VisibilityCoordinator::unrestricted_for_test_or_benchmark().scope(),
    )
    .expect("unbounded traversal accounting should not fail")
}

#[cfg(test)]
#[allow(dead_code, reason = "legacy test compatibility entry point")]
pub(crate) fn execute_dfs_with_neighbors_governed(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<BfsResult> {
    let coordinator = VisibilityCoordinator::unrestricted_for_test_or_benchmark();
    let context = coordinator.context(governor);
    execute_dfs_with_neighbors_governed_with_context(
        node_store,
        neighbors,
        filter_index,
        config,
        &context,
    )
}

pub(crate) fn execute_dfs_with_neighbors_governed_with_context(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    context: &crate::visibility::QueryExecutionContext<'_>,
) -> GraphResult<BfsResult> {
    execute_dfs_with_neighbors_inner(
        node_store,
        neighbors,
        filter_index,
        config,
        Some(context.governor),
        context.visibility,
    )
}

fn execute_dfs_with_neighbors_inner(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    governor: Option<&crate::resource::ResourceGovernor>,
    visibility: &VisibilityScope,
) -> GraphResult<BfsResult> {
    let node_count = node_store.node_count() as usize;
    let sparse_metadata = use_sparse_metadata(node_count, config.max_nodes);
    let expected_visits = expected_visit_capacity(node_count, config.max_nodes);

    let mut visited = RoaringBitmap::new();
    let mut depth_map = TraversalDepthMap::try_new(node_count, sparse_metadata, expected_visits)?;
    let mut parent = TraversalParentMap::try_new(node_count, sparse_metadata, expected_visits)?;
    let mut parent_edge_type =
        TraversalParentEdgeTypes::try_new(node_count, sparse_metadata, expected_visits)?;
    if config.seed_node as usize >= node_count || !visibility.allows_node(config.seed_node) {
        return Ok(BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
            truncated: false,
        });
    }

    let seed = config.seed_node;
    visited.insert(seed);
    depth_map.set(seed, 0);
    parent.set(seed, seed);
    parent_edge_type.set(seed, 0);

    if matches!(
        config.edge_type_filter,
        crate::types::EdgeTypeFilter::NoneMatched
    ) {
        return Ok(BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
            truncated: false,
        });
    }

    let mut stack = Vec::new();
    stack
        .try_reserve(config.max_frontier as usize)
        .map_err(traversal_allocation_error)?;
    stack.push(seed);
    let mut nodes_visited: u32 = 1;
    let has_filters = !config.filter_ops.is_empty();

    while let Some(current) = stack.pop() {
        let current_depth = depth_map.get(current).unwrap_or(-1);
        if current_depth >= config.max_depth {
            continue;
        }

        let mut push = DfsPushContext {
            node_store,
            neighbors,
            filter_index,
            config,
            visited: &mut visited,
            depth_map: &mut depth_map,
            parent: &mut parent,
            parent_edge_type: &mut parent_edge_type,
            stack: &mut stack,
            nodes_visited: &mut nodes_visited,
            has_filters,
            governor,
            visibility,
        };
        if push.push_neighbors(current, current_depth)? {
            return Ok(BfsResult {
                visited,
                depth: depth_map,
                parent,
                parent_edge_type,
                truncated: true,
            });
        }
    }

    Ok(BfsResult {
        visited,
        depth: depth_map,
        parent,
        parent_edge_type,
        truncated: false,
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared admission gate needs topology, filter, traversal, edge, visited, and visibility state"
)]
fn candidate_allowed(
    node_store: &NodeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    neighbor: u32,
    edge_type: u8,
    relationship_id: Option<crate::edge_store::RelationshipId>,
    visited: &RoaringBitmap,
    has_filters: bool,
    visibility: &VisibilityScope,
) -> GraphResult<bool> {
    if let crate::types::EdgeTypeFilter::Only(ref allowed) = config.edge_type_filter {
        if !allowed.contains(&edge_type) {
            return Ok(false);
        }
    }
    if !visibility.allows_relationship(edge_type, relationship_id)?
        || !visibility.allows_node(neighbor)
    {
        return Ok(false);
    }
    if visited.contains(neighbor) {
        return Ok(false);
    }
    if !node_store.is_active(neighbor) || crate::projection::tx_delta::node_deleted(neighbor) {
        return Ok(false);
    }
    if let Some(tenant) = config.tenant.as_deref() {
        if node_store.table_oid(neighbor).is_some_and(|table_oid| {
            config.tenanted_table_oids.contains(&table_oid)
                && !(config
                    .tenant_membership
                    .get(tenant)
                    .is_some_and(|bitmap| bitmap.contains(neighbor))
                    || (filter_index.mapped_tenant_contains(tenant, neighbor)
                        && !config
                            .tenant_membership_removals
                            .get(tenant)
                            .is_some_and(|bitmap| bitmap.contains(neighbor))))
        }) {
            return Ok(false);
        }
    }
    if has_filters && !filter_index.check_filters(neighbor, &config.filter_ops) {
        return Ok(false);
    }
    Ok(true)
}

struct DfsPushContext<'a> {
    node_store: &'a NodeStore,
    neighbors: &'a dyn NeighborSource,
    filter_index: &'a FilterIndex,
    config: &'a BfsConfig,
    visited: &'a mut RoaringBitmap,
    depth_map: &'a mut TraversalDepthMap,
    parent: &'a mut TraversalParentMap,
    parent_edge_type: &'a mut TraversalParentEdgeTypes,
    stack: &'a mut Vec<u32>,
    nodes_visited: &'a mut u32,
    has_filters: bool,
    governor: Option<&'a crate::resource::ResourceGovernor>,
    visibility: &'a VisibilityScope,
}

impl DfsPushContext<'_> {
    fn push_neighbors(&mut self, current: u32, current_depth: i32) -> GraphResult<bool> {
        for neighbor in self.neighbors.neighbors_reversed(current) {
            consume_expansion(self.governor)?;
            if self.push_candidate(
                current,
                current_depth,
                neighbor.target,
                neighbor.type_id,
                neighbor.relationship_id,
            )? {
                continue;
            }
            return Ok(true);
        }

        Ok(false)
    }

    fn push_candidate(
        &mut self,
        current: u32,
        current_depth: i32,
        neighbor: u32,
        edge_type: u8,
        relationship_id: Option<crate::edge_store::RelationshipId>,
    ) -> GraphResult<bool> {
        if !candidate_allowed(
            self.node_store,
            self.filter_index,
            self.config,
            neighbor,
            edge_type,
            relationship_id,
            self.visited,
            self.has_filters,
            self.visibility,
        )? {
            return Ok(true);
        }

        self.visited.insert(neighbor);
        self.depth_map.set(neighbor, current_depth + 1);
        self.parent.set(neighbor, current);
        self.parent_edge_type.set(neighbor, edge_type);
        *self.nodes_visited += 1;

        if *self.nodes_visited >= self.config.max_nodes {
            return Ok(false);
        }

        if current_depth + 1 < self.config.max_depth {
            self.stack.push(neighbor);
            if self.stack.len() as u32 >= self.config.max_frontier {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

fn consume_expansion(governor: Option<&crate::resource::ResourceGovernor>) -> GraphResult<()> {
    let Some(governor) = governor else {
        return Ok(());
    };
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryExpand,
            crate::resource::WorkUnits::new(1),
        )
        .map_err(crate::safety::resource_limit_error)?;
    if governor.work_used().as_u64().is_multiple_of(1_024) {
        crate::resource::check_postgres_interrupts();
        governor
            .check_elapsed(crate::resource::ResourcePhase::QueryExpand)
            .map_err(crate::safety::resource_limit_error)?;
    }
    Ok(())
}

fn consume_expansion_without_interrupt(
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<()> {
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryExpand,
            crate::resource::WorkUnits::new(1),
        )
        .map_err(crate::safety::resource_limit_error)
}

/// Reconstruct the path from seed to a specific node using parent metadata.
///
/// Returns the path as a sequence of node indices from seed to target.
pub fn reconstruct_path(parent: &TraversalParentMap, seed: u32, target: u32) -> Vec<u32> {
    let mut path = Vec::new();
    let mut current = target;

    // Walk backwards from target to seed
    loop {
        path.push(current);
        if current == seed {
            break;
        }
        let Some(p) = parent.get(current) else {
            // Unreachable or already at root
            break;
        };
        if p == current {
            break;
        }
        current = p;
    }

    path.reverse();
    path
}

/// Reconstruct edge type IDs for the path from seed to target.
pub fn reconstruct_edge_path(
    parent: &TraversalParentMap,
    parent_edge_type: &TraversalParentEdgeTypes,
    seed: u32,
    target: u32,
) -> Vec<u8> {
    let mut edge_path = Vec::new();
    let mut current = target;

    while current != seed {
        let Some(parent_node) = parent.get(current) else {
            break;
        };
        if parent_node == current {
            break;
        }
        edge_path.push(parent_edge_type.get(current).unwrap_or(0));
        current = parent_node;
    }

    edge_path.reverse();
    edge_path
}

/// Convert BFS results into [`TraversalResult`] values for SQL output.
///
/// # Errors
///
/// Returns [`GraphError::CorruptFile`] when a visited node or reconstructed
/// path index has no corresponding node metadata.
pub fn to_traversal_results(
    bfs_result: &BfsResult,
    node_store: &NodeStore,
    edge_type_registry: &[String],
) -> GraphResult<Vec<TraversalResult>> {
    let mut results = Vec::with_capacity(bfs_result.visited.len() as usize);

    // Find the seed (depth 0)
    let seed = bfs_result
        .visited
        .iter()
        .find(|&idx| bfs_result.depth.get(idx) == Some(0))
        .unwrap_or(0);

    for node_idx in bfs_result.visited.iter() {
        let depth = bfs_result.depth.get(node_idx).unwrap_or(-1);
        let path_indices = reconstruct_path(&bfs_result.parent, seed, node_idx);
        let path: Vec<PathCoordinate> = path_indices
            .iter()
            .map(|&idx| {
                let (table_oid, node_id) = node_coordinate(node_store, idx)?;
                Ok(PathCoordinate {
                    table_oid,
                    node_id: node_id.to_string(),
                })
            })
            .collect::<GraphResult<Vec<_>>>()?;
        let edge_path = reconstruct_edge_path(
            &bfs_result.parent,
            &bfs_result.parent_edge_type,
            seed,
            node_idx,
        )
        .into_iter()
        .map(|type_id| {
            edge_type_registry
                .get(type_id as usize)
                .cloned()
                .unwrap_or_else(|| type_id.to_string())
        })
        .collect();

        let (node_table, node_id) = node_coordinate(node_store, node_idx)?;
        results.push(TraversalResult {
            node_table,
            node_id: node_id.to_string(),
            depth,
            path,
            edge_path,
        });
    }

    // Sort by depth for consistent output
    results.sort_by_key(|r| r.depth);
    Ok(results)
}

fn node_coordinate(node_store: &NodeStore, node_idx: u32) -> GraphResult<(TableOid, &str)> {
    let table_oid = node_store
        .table_oid(node_idx)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: format!("node index {node_idx} has no table OID metadata"),
        })?;
    let node_id = node_store
        .primary_key(node_idx)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: format!("node index {node_idx} has no primary-key metadata"),
        })?;
    Ok((TableOid(table_oid), node_id))
}

#[cfg(test)]
mod tests {
    //! Covers breadth-first traversal semantics, including depth limits,
    //! directionality, active-node filtering, and edge-type constraints.
    //!
    //! Hidden-versus-absent comparisons below cover caller-visible rows,
    //! chosen paths, counts, caps, and truncation. They deliberately do not
    //! claim equal physical work, timing, or client diagnostics; PostgreSQL
    //! boundary tests own those contracts.

    use super::*;
    use crate::edge_store::RawEdge;
    use crate::resource::{
        ByteCount, DiskBudget, ElapsedBudget, MemoryBudget, ResourceLimits, RowCount, WorkUnits,
    };
    use std::collections::HashSet;
    use std::time::Duration;

    fn build_test_graph() -> (NodeStore, EdgeStore) {
        // Build a simple graph: 0 → 1 → 2 → 3, with 0 → 4
        let mut ns = NodeStore::new();
        for i in 0..5u32 {
            ns.add_node(100, format!("PK-{}", i));
        }

        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 3,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 4,
                type_id: 2,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 4,
                target: 0,
                type_id: 2,
                weight: None,
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(5, edges, false);
        (ns, es)
    }

    fn resumable_test_governor() -> crate::resource::ResourceGovernor {
        crate::resource::ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(16 * 1_024 * 1_024)),
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::new(100_000),
            ElapsedBudget::new(Duration::from_secs(5)),
        ))
    }

    fn resumable_test_config(max_depth: i32, max_nodes: u32, max_frontier: u32) -> BfsConfig {
        BfsConfig {
            seed_node: 0,
            max_depth,
            max_nodes,
            max_frontier,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: Vec::new(),
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: HashMap::new(),
            tenant_membership_removals: HashMap::new(),
            overlay_insert_edges: HashMap::new(),
            overlay_deleted_edges: HashMap::new(),
        }
    }

    fn run_resumable_all_visible(
        nodes: &NodeStore,
        edges: &EdgeStore,
        config: &BfsConfig,
        batch_size: usize,
        governor: &crate::resource::ResourceGovernor,
    ) -> BfsResult {
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let neighbors = crate::projection::neighbors::CsrNeighbors::new(edges);
        let mut machine = ResumableBfsMachine::try_new(nodes.node_count() as usize, config)
            .expect("resumable BFS state should allocate");
        while !machine.is_complete() {
            let Some(batch) = machine
                .take_candidate_batch(
                    nodes,
                    &neighbors,
                    &relationships,
                    config,
                    BfsCandidateLimits {
                        max_candidates: batch_size,
                        max_key_bytes: 1_024 * 1_024,
                    },
                    governor,
                )
                .expect("candidate materialization should succeed")
            else {
                break;
            };
            let verdicts = batch
                .candidates
                .iter()
                .map(|candidate| BfsAdjacencyVerdict {
                    sequence: candidate.sequence,
                    node_visible: true,
                    relationship_visible: true,
                })
                .collect::<Vec<_>>();
            machine
                .apply_visibility_verdicts(&batch, &verdicts, nodes, &FilterIndex::new(), config)
                .expect("visible candidates should be admitted");
        }
        machine.finish()
    }

    fn bfs_result_bytes(result: &BfsResult) -> Vec<u8> {
        result
            .visited
            .iter()
            .flat_map(|node| {
                let mut bytes = node.to_le_bytes().to_vec();
                bytes.extend_from_slice(&result.depth.get(node).unwrap_or(-1).to_le_bytes());
                bytes.extend_from_slice(&result.parent.get(node).unwrap_or(u32::MAX).to_le_bytes());
                bytes.push(result.parent_edge_type.get(node).unwrap_or(u8::MAX));
                bytes
            })
            .chain([u8::from(result.truncated)])
            .collect()
    }

    fn adjacency_candidate(sequence: u32, target: u32, key: &str) -> BfsAdjacencyCandidate {
        BfsAdjacencyCandidate {
            sequence,
            parent_node: 0,
            parent_depth: 0,
            target_node: target,
            target_table_oid: 100,
            target_source_key: key.to_owned(),
            edge_type: 1,
            relationship_id: Some(sequence.saturating_add(1)),
            relationship_mapping_id: Some(7),
            relationship_source_key: Some(format!("edge-{sequence}")),
        }
    }

    #[test]
    fn adjacency_candidate_batches_preserve_neighbor_order_and_reject_count_or_byte_overflow() {
        let limits = BfsCandidateLimits {
            max_candidates: 2,
            max_key_bytes: 16,
        };
        let batch = BfsAdjacencyCandidateBatch::try_new(
            vec![
                adjacency_candidate(4, 1, "a"),
                adjacency_candidate(5, 2, "b"),
            ],
            true,
            limits,
        )
        .expect("ordered bounded candidates should be accepted");
        assert_eq!(
            batch
                .candidates
                .iter()
                .map(|candidate| candidate.sequence)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert!(BfsAdjacencyCandidateBatch::try_new(
            vec![
                adjacency_candidate(5, 1, "a"),
                adjacency_candidate(4, 2, "b")
            ],
            true,
            limits,
        )
        .is_err());
        assert!(BfsAdjacencyCandidateBatch::try_new(
            vec![
                adjacency_candidate(1, 1, "a"),
                adjacency_candidate(2, 2, "b"),
                adjacency_candidate(3, 3, "c"),
            ],
            true,
            limits,
        )
        .is_err());
        assert!(BfsAdjacencyCandidateBatch::try_new(
            vec![adjacency_candidate(1, 1, "source-key-too-large")],
            true,
            limits,
        )
        .is_err());
    }

    #[test]
    fn adjacency_candidates_emit_node_and_relationship_visibility_in_one_sequence() {
        let candidate = adjacency_candidate(9, 3, "node-3");
        assert_eq!(candidate.sequence, 9);
        assert_eq!(
            (
                candidate.target_table_oid,
                candidate.target_source_key.as_str()
            ),
            (100, "node-3")
        );
        assert_eq!(
            (
                candidate.relationship_mapping_id,
                candidate.relationship_source_key.as_deref(),
            ),
            (Some(7), Some("edge-9"))
        );
        assert!(!BfsAdjacencyVerdict {
            sequence: candidate.sequence,
            node_visible: true,
            relationship_visible: false,
        }
        .visible());
    }

    #[test]
    fn resumable_bfs_completes_leaf_without_empty_visibility_probe() {
        let mut nodes = NodeStore::new();
        nodes.add_node(100, "seed".into());
        let edges = EdgeStore::from_edges(1, Vec::new(), false);
        let neighbors = crate::projection::neighbors::CsrNeighbors::new(&edges);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let config = resumable_test_config(2, 10, 10);
        let governor = resumable_test_governor();
        let mut machine = ResumableBfsMachine::try_new(1, &config).unwrap();

        let batch = machine
            .take_candidate_batch(
                &nodes,
                &neighbors,
                &relationships,
                &config,
                BfsCandidateLimits {
                    max_candidates: 2,
                    max_key_bytes: 128,
                },
                &governor,
            )
            .unwrap();

        assert!(batch.is_none());
        assert!(machine.is_complete());
    }

    #[test]
    fn resumable_bfs_skips_filtered_pages_before_visible_candidate() {
        let mut nodes = NodeStore::new();
        for key in ["seed", "filtered-1", "filtered-2", "match"] {
            nodes.add_node(100, key.into());
        }
        let edges = EdgeStore::from_edges(
            4,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: 2,
                    weight: None,
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: 2,
                    weight: None,
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 3,
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
            ],
            false,
        );
        let neighbors = crate::projection::neighbors::CsrNeighbors::new(&edges);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let mut config = resumable_test_config(1, 10, 10);
        config.edge_type_filter = crate::types::EdgeTypeFilter::Only(HashSet::from([1]));
        let governor = resumable_test_governor();
        let mut machine = ResumableBfsMachine::try_new(4, &config).unwrap();

        let batch = machine
            .take_candidate_batch(
                &nodes,
                &neighbors,
                &relationships,
                &config,
                BfsCandidateLimits {
                    max_candidates: 2,
                    max_key_bytes: 128,
                },
                &governor,
            )
            .unwrap()
            .expect("the later matching page should materialize");

        assert_eq!(batch.candidates.len(), 1);
        assert_eq!(batch.candidates[0].target_node, 3);
    }

    #[test]
    fn resumable_bfs_completes_all_filtered_adjacency_without_probe() {
        let mut nodes = NodeStore::new();
        nodes.add_node(100, "seed".into());
        nodes.add_node(100, "filtered".into());
        let edges = EdgeStore::from_edges(
            2,
            vec![RawEdge {
                source: 0,
                target: 1,
                type_id: 2,
                weight: None,
                schema_reversed: false,
            }],
            false,
        );
        let neighbors = crate::projection::neighbors::CsrNeighbors::new(&edges);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let mut config = resumable_test_config(1, 10, 10);
        config.edge_type_filter = crate::types::EdgeTypeFilter::Only(HashSet::from([1]));
        let governor = resumable_test_governor();
        let mut machine = ResumableBfsMachine::try_new(2, &config).unwrap();

        assert!(machine
            .take_candidate_batch(
                &nodes,
                &neighbors,
                &relationships,
                &config,
                BfsCandidateLimits {
                    max_candidates: 1,
                    max_key_bytes: 128
                },
                &governor,
            )
            .unwrap()
            .is_none());
        assert!(machine.is_complete());
    }

    #[test]
    fn resumable_bfs_yields_progress_after_each_filtered_raw_page() {
        let mut nodes = NodeStore::new();
        nodes.add_node(100, "seed".into());
        for index in 0..5 {
            nodes.add_node(100, format!("filtered-{index}"));
        }
        let edges = EdgeStore::from_edges(
            6,
            (1..=5)
                .map(|target| RawEdge {
                    source: 0,
                    target,
                    type_id: 2,
                    weight: None,
                    schema_reversed: false,
                })
                .collect(),
            false,
        );
        let neighbors = crate::projection::neighbors::CsrNeighbors::new(&edges);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let mut config = resumable_test_config(1, 10, 10);
        config.edge_type_filter = crate::types::EdgeTypeFilter::Only(HashSet::from([1]));
        let governor = resumable_test_governor();
        let mut machine = ResumableBfsMachine::try_new(6, &config).unwrap();
        let limits = BfsCandidateLimits {
            max_candidates: 2,
            max_key_bytes: 128,
        };

        for _ in 0..3 {
            assert!(matches!(
                materialize_bfs_candidate_batch(
                    &mut machine,
                    &nodes,
                    &neighbors,
                    &relationships,
                    &config,
                    limits,
                    &governor,
                )
                .unwrap(),
                BfsMaterialization::Progress
            ));
        }
        assert!(matches!(
            materialize_bfs_candidate_batch(
                &mut machine,
                &nodes,
                &neighbors,
                &relationships,
                &config,
                limits,
                &governor,
            )
            .unwrap(),
            BfsMaterialization::Complete
        ));
        assert_eq!(governor.work_used().as_u64(), 5);
    }

    #[test]
    fn resumable_bfs_pages_when_combined_keys_exceed_batch_bytes() {
        let mut nodes = NodeStore::new();
        nodes.add_node(100, "seed".into());
        nodes.add_node(100, "first-key".into());
        nodes.add_node(100, "second-key".into());
        let edges = EdgeStore::from_edges(
            3,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
            ],
            false,
        );
        let neighbors = crate::projection::neighbors::CsrNeighbors::new(&edges);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let config = resumable_test_config(1, 10, 10);
        let governor = resumable_test_governor();
        let mut machine = ResumableBfsMachine::try_new(3, &config).unwrap();
        let limits = BfsCandidateLimits {
            max_candidates: 2,
            max_key_bytes: "first-key".len().max("second-key".len()),
        };

        let first = machine
            .take_candidate_batch(
                &nodes,
                &neighbors,
                &relationships,
                &config,
                limits,
                &governor,
            )
            .unwrap()
            .expect("first byte-bounded page");
        assert_eq!(first.candidates.len(), 1);
        let first_verdict = vec![BfsAdjacencyVerdict {
            sequence: first.candidates[0].sequence,
            node_visible: true,
            relationship_visible: true,
        }];
        machine
            .apply_visibility_verdicts(&first, &first_verdict, &nodes, &FilterIndex::new(), &config)
            .unwrap();

        let second = machine
            .take_candidate_batch(
                &nodes,
                &neighbors,
                &relationships,
                &config,
                limits,
                &governor,
            )
            .unwrap()
            .expect("second byte-bounded page");
        assert_eq!(second.candidates.len(), 1);
        assert_eq!(second.candidates[0].target_source_key, "second-key");
    }

    #[test]
    fn resumable_bfs_rejects_misaligned_verdicts_before_admission() {
        let (nodes, edges) = build_test_graph();
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let neighbors = crate::projection::neighbors::CsrNeighbors::new(&edges);
        let config = resumable_test_config(1, 10, 10);
        let governor = resumable_test_governor();
        let mut machine = ResumableBfsMachine::try_new(nodes.node_count() as usize, &config)
            .expect("machine should allocate");
        let batch = machine
            .take_candidate_batch(
                &nodes,
                &neighbors,
                &relationships,
                &config,
                BfsCandidateLimits {
                    max_candidates: 2,
                    max_key_bytes: 128,
                },
                &governor,
            )
            .unwrap()
            .expect("seed adjacency should materialize");
        let visited_before = machine.visited.clone();
        let outputs_before = machine.outputs.clone();
        let verdicts = batch
            .candidates
            .iter()
            .rev()
            .map(|candidate| BfsAdjacencyVerdict {
                sequence: candidate.sequence,
                node_visible: true,
                relationship_visible: true,
            })
            .collect::<Vec<_>>();

        assert!(machine
            .apply_visibility_verdicts(&batch, &verdicts, &nodes, &FilterIndex::new(), &config,)
            .is_err());
        assert_eq!(machine.visited, visited_before);
        assert_eq!(machine.outputs, outputs_before);
        assert!(matches!(machine.state, ResumableBfsState::NeedVisibility));
    }

    #[test]
    fn resumable_bfs_requires_both_node_and_relationship_visibility() {
        let (nodes, edges) = build_test_graph();
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let neighbors = crate::projection::neighbors::CsrNeighbors::new(&edges);
        let config = resumable_test_config(1, 10, 10);
        let governor = resumable_test_governor();
        for (node_visible, relationship_visible) in [(false, true), (true, false)] {
            let mut machine = ResumableBfsMachine::try_new(nodes.node_count() as usize, &config)
                .expect("machine should allocate");
            let batch = machine
                .take_candidate_batch(
                    &nodes,
                    &neighbors,
                    &relationships,
                    &config,
                    BfsCandidateLimits {
                        max_candidates: 1,
                        max_key_bytes: 128,
                    },
                    &governor,
                )
                .unwrap()
                .expect("seed adjacency should materialize");
            let verdicts = [BfsAdjacencyVerdict {
                sequence: batch.candidates[0].sequence,
                node_visible,
                relationship_visible,
            }];
            machine
                .apply_visibility_verdicts(&batch, &verdicts, &nodes, &FilterIndex::new(), &config)
                .unwrap();
            assert_eq!(machine.visited.iter().collect::<Vec<_>>(), vec![0]);
        }
    }

    #[test]
    fn materialize_bfs_candidate_batch_reads_transaction_relationship_identity() {
        crate::projection::tx_delta::clear_for_test();
        let mut nodes = NodeStore::new();
        nodes.add_node(100, "seed".into());
        nodes.add_node(100, "target".into());
        let edges = EdgeStore::from_edges(2, Vec::new(), false);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let relationship_id = crate::projection::tx_delta::record_relationship_identity(
            relationships.len(),
            crate::edge_store::RelationshipIdentity {
                mapping_id: 77,
                source_key: "tx-edge".into(),
            },
        )
        .expect("transaction relationship identity should allocate");
        let inserts = HashMap::from([(0, vec![(1, 1, false, Some(relationship_id))])]);
        let deletes = HashMap::new();
        let neighbors = OverlayNeighbors::new(&edges, &inserts, &deletes);
        let config = resumable_test_config(1, 10, 10);
        let governor = resumable_test_governor();
        let mut machine = ResumableBfsMachine::try_new(2, &config).unwrap();
        let batch = machine
            .take_candidate_batch(
                &nodes,
                &neighbors,
                &relationships,
                &config,
                BfsCandidateLimits {
                    max_candidates: 1,
                    max_key_bytes: 128,
                },
                &governor,
            )
            .unwrap()
            .expect("overlay edge should produce one candidate");
        assert_eq!(batch.candidates[0].relationship_id, Some(relationship_id));
        assert_eq!(batch.candidates[0].relationship_mapping_id, Some(77));
        assert_eq!(
            batch.candidates[0].relationship_source_key.as_deref(),
            Some("tx-edge")
        );
        crate::projection::tx_delta::clear_for_test();
    }

    #[test]
    fn resumable_bfs_matches_eager_result_bytes() {
        let (nodes, edges) = build_test_graph();
        for mut config in [
            resumable_test_config(3, 100, 100),
            resumable_test_config(0, 100, 100),
            resumable_test_config(-1, 100, 100),
        ] {
            let eager = execute(&nodes, &edges, &FilterIndex::new(), &config);
            let governor = resumable_test_governor();
            let resumable = run_resumable_all_visible(&nodes, &edges, &config, 1, &governor);
            assert_eq!(bfs_result_bytes(&resumable), bfs_result_bytes(&eager));

            config.seed_node = nodes.node_count().saturating_add(7);
            let eager = execute(&nodes, &edges, &FilterIndex::new(), &config);
            let governor = resumable_test_governor();
            let resumable = run_resumable_all_visible(&nodes, &edges, &config, 1, &governor);
            assert_eq!(bfs_result_bytes(&resumable), bfs_result_bytes(&eager));
        }
    }

    #[test]
    fn resumable_bfs_preserves_duplicate_parent_selection() {
        let mut nodes = NodeStore::new();
        for key in ["seed", "left", "right", "target"] {
            nodes.add_node(100, key.into());
        }
        let edge = |source, target| RawEdge {
            source,
            target,
            type_id: 1,
            weight: None,
            schema_reversed: false,
        };
        let edges = EdgeStore::from_edges(
            4,
            vec![edge(0, 1), edge(0, 2), edge(1, 3), edge(2, 3)],
            false,
        );
        let config = resumable_test_config(2, 100, 100);
        let eager = execute(&nodes, &edges, &FilterIndex::new(), &config);
        let governor = resumable_test_governor();
        let resumable = run_resumable_all_visible(&nodes, &edges, &config, 1, &governor);
        assert_eq!(eager.parent.get(3), Some(1));
        assert_eq!(resumable.parent.get(3), eager.parent.get(3));
        assert_eq!(bfs_result_bytes(&resumable), bfs_result_bytes(&eager));
    }

    #[test]
    fn resumable_bfs_preserves_max_nodes_and_frontier_truncation() {
        let (nodes, edges) = build_test_graph();
        for config in [
            resumable_test_config(3, 2, 100),
            resumable_test_config(3, 100, 1),
        ] {
            let eager = execute(&nodes, &edges, &FilterIndex::new(), &config);
            let governor = resumable_test_governor();
            let resumable = run_resumable_all_visible(&nodes, &edges, &config, 1, &governor);
            assert!(eager.truncated);
            assert_eq!(bfs_result_bytes(&resumable), bfs_result_bytes(&eager));
        }
    }

    #[test]
    fn resumable_bfs_never_holds_projection_borrows_while_policy_probe_runs() {
        let config = resumable_test_config(1, 10, 10);
        let governor = resumable_test_governor();
        let batch = {
            let (nodes, edges) = build_test_graph();
            let relationships =
                crate::relationship_identity_store::RelationshipIdentityStore::default();
            let neighbors = crate::projection::neighbors::CsrNeighbors::new(&edges);
            let mut machine = ResumableBfsMachine::try_new(nodes.node_count() as usize, &config)
                .expect("machine should allocate");
            machine
                .take_candidate_batch(
                    &nodes,
                    &neighbors,
                    &relationships,
                    &config,
                    BfsCandidateLimits {
                        max_candidates: 1,
                        max_key_bytes: 128,
                    },
                    &governor,
                )
                .unwrap()
                .expect("candidate should materialize")
        };
        assert_eq!(batch.candidates[0].target_source_key, "PK-1");
    }

    #[test]
    fn resumable_bfs_charges_each_materialized_edge_once_across_yields() {
        let (nodes, edges) = build_test_graph();
        let config = resumable_test_config(1, 100, 100);
        let governor = resumable_test_governor();
        let result = run_resumable_all_visible(&nodes, &edges, &config, 1, &governor);
        assert_eq!(result.visited.len(), 3);
        assert_eq!(governor.work_used().as_u64(), 2);
    }

    #[test]
    fn dfs_visibility_blocks_hidden_intermediate_before_accounting() {
        let (nodes, edges) = build_test_graph();
        let filter_index = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100_000,
            max_frontier: 100_000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };
        let governor = crate::resource::ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(1_024 * 1_024)),
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::new(1_000),
            ElapsedBudget::new(Duration::from_secs(1)),
        ));
        let mut hidden_nodes = RoaringBitmap::new();
        hidden_nodes.insert(1);
        let visibility = VisibilityScope::enforced_for_test(
            hidden_nodes,
            RoaringBitmap::new(),
            RoaringBitmap::new(),
        );
        let context = QueryExecutionContext::new(&governor, &visibility);
        let result =
            execute_dfs_governed_with_context(&nodes, &edges, &filter_index, &config, &context)
                .unwrap();

        assert!(result.visited.contains(0));
        assert!(result.visited.contains(4));
        assert!(!result.visited.contains(1));
        assert!(!result.visited.contains(2));
        assert!(!result.visited.contains(3));
        assert!(!result.truncated);
    }

    #[test]
    fn hidden_candidate_matches_absent_topology_at_the_bfs_output_boundary() {
        let mut nodes = NodeStore::new();
        for id in ["seed", "hidden", "visible", "later"] {
            nodes.add_node(100, id.to_string());
        }
        let raw = |target| RawEdge {
            source: 0,
            target,
            type_id: 1,
            weight: None,
            schema_reversed: false,
        };
        let projected = EdgeStore::from_edges(4, vec![raw(1), raw(2), raw(3)], false);
        let physically_absent = EdgeStore::from_edges(4, vec![raw(2), raw(3)], false);
        let filter_index = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 1,
            max_nodes: 2,
            max_frontier: 100,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };
        let governor = crate::resource::ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(1_024 * 1_024)),
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::new(1_000),
            ElapsedBudget::new(Duration::from_secs(1)),
        ));
        let mut hidden_nodes = RoaringBitmap::new();
        hidden_nodes.insert(1);
        let hidden_scope = VisibilityScope::enforced_for_test(
            hidden_nodes,
            RoaringBitmap::new(),
            RoaringBitmap::new(),
        );
        let hidden_context = QueryExecutionContext::new(&governor, &hidden_scope);
        let absent_coordinator = VisibilityCoordinator::unrestricted_for_test_or_benchmark();
        let absent_context = absent_coordinator.context(&governor);

        let hidden = execute_governed_with_context(
            &nodes,
            &projected,
            &filter_index,
            &config,
            &hidden_context,
        )
        .unwrap();
        let absent = execute_governed_with_context(
            &nodes,
            &physically_absent,
            &filter_index,
            &config,
            &absent_context,
        )
        .unwrap();
        let registry = [String::new(), "REL".to_string()];
        let summarize = |result: &BfsResult| {
            to_traversal_results(result, &nodes, &registry)
                .unwrap()
                .into_iter()
                .map(|row| {
                    (
                        row.node_id,
                        row.depth,
                        row.path
                            .into_iter()
                            .map(|coordinate| coordinate.node_id)
                            .collect::<Vec<_>>(),
                        row.edge_path,
                    )
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(summarize(&hidden), summarize(&absent));
        assert_eq!(hidden.visited.len(), absent.visited.len());
        assert_eq!(hidden.truncated, absent.truncated);
        assert!(
            hidden.truncated,
            "the shared max_nodes cap must be exercised"
        );
    }

    #[test]
    fn eager_coordinator_matches_direct_scope_byte_for_byte() {
        let (nodes, edges) = build_test_graph();
        let governor = crate::resource::ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(1_024 * 1_024)),
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::new(10_000),
            ElapsedBudget::new(Duration::from_secs(1)),
        ));
        let scope = VisibilityScope::enforced_for_test(
            RoaringBitmap::new(),
            RoaringBitmap::new(),
            RoaringBitmap::new(),
        );
        let direct_context = QueryExecutionContext::new(&governor, &scope);
        let coordinator =
            crate::visibility::VisibilityCoordinator::from_scope_for_test(scope.clone());
        let coordinated_context = coordinator.context(&governor);
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 3,
            max_nodes: 100,
            max_frontier: 100,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };
        let direct = execute_governed_with_context(
            &nodes,
            &edges,
            &FilterIndex::new(),
            &config,
            &direct_context,
        )
        .unwrap();
        let coordinated = execute_governed_with_context(
            &nodes,
            &edges,
            &FilterIndex::new(),
            &config,
            &coordinated_context,
        )
        .unwrap();
        let encode = |result: &BfsResult| {
            result
                .visited
                .iter()
                .flat_map(|node| {
                    let mut bytes = node.to_le_bytes().to_vec();
                    bytes.extend_from_slice(&result.depth.get(node).unwrap_or(-1).to_le_bytes());
                    bytes.extend_from_slice(
                        &result.parent.get(node).unwrap_or(u32::MAX).to_le_bytes(),
                    );
                    bytes.extend_from_slice(
                        &result
                            .parent_edge_type
                            .get(node)
                            .unwrap_or(u8::MAX)
                            .to_le_bytes(),
                    );
                    bytes
                })
                .chain([u8::from(result.truncated)])
                .collect::<Vec<_>>()
        };
        assert_eq!(encode(&direct), encode(&coordinated));
    }

    #[test]
    fn bfs_returns_seed_at_depth_zero() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 5,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert!(result.visited.contains(0));
        assert_eq!(result.depth.get(0), Some(0));
    }

    #[test]
    fn bfs_discovers_all_connected_nodes() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert_eq!(result.visited.len(), 5); // All 5 nodes reachable
    }

    #[test]
    fn bfs_respects_max_depth() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 1,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        // At depth 1: seed(0) + neighbors(1, 4) = 3 nodes
        assert_eq!(result.visited.len(), 3);
        assert!(result.visited.contains(0));
        assert!(result.visited.contains(1));
        assert!(result.visited.contains(4));
    }

    #[test]
    fn bfs_edge_type_filter() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let mut edge_filter = HashSet::new();
        edge_filter.insert(1u8); // Only type 1 edges

        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::Only(edge_filter),
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        // Node 4 is only connected via type 2 edges, should not be found
        assert!(!result.visited.contains(4));
        assert_eq!(result.visited.len(), 4); // 0, 1, 2, 3
    }

    #[test]
    fn bfs_streams_overlay_neighbors_without_materializing_base_edges() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let mut overlay_insert_edges = std::collections::HashMap::new();
        overlay_insert_edges.insert(0, vec![(3, 1, false, None), (1, 1, false, None)]);
        let mut overlay_deleted_edges = std::collections::HashMap::new();
        overlay_deleted_edges.insert(0, HashSet::from([(1, 1, false, None)]));

        let config = BfsConfig {
            seed_node: 0,
            max_depth: 1,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges,
            overlay_deleted_edges,
        };

        let result = execute(&ns, &es, &fi, &config);

        assert!(result.visited.contains(0));
        assert!(
            !result.visited.contains(1),
            "deleted base edge should be hidden"
        );
        assert!(
            result.visited.contains(3),
            "inserted overlay edge should be visible"
        );
        assert!(
            result.visited.contains(4),
            "unaffected base edge should still stream"
        );
    }

    #[test]
    fn dfs_streams_reverse_neighbors_without_materializing_vector() {
        let mut ns = NodeStore::new();
        for idx in 0..4u32 {
            ns.add_node(100, format!("PK-{}", idx));
        }
        let es = EdgeStore::from_edges(
            4,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
            ],
            false,
        );
        let fi = FilterIndex::new();
        let mut overlay_insert_edges = std::collections::HashMap::new();
        overlay_insert_edges.insert(
            0,
            vec![
                (3, 1, false, None),
                (2, 1, false, None),
                (3, 1, false, None),
            ],
        );

        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 2,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges,
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute_dfs(&ns, &es, &fi, &config);

        assert!(result.visited.contains(0));
        assert!(
            result.visited.contains(3),
            "DFS should preserve the previous reversed overlay expansion order"
        );
        assert_eq!(result.visited.len(), 2);
    }

    #[test]
    fn path_reconstruction() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        let path = reconstruct_path(&result.parent, 0, 3);
        assert_eq!(path, vec![0, 1, 2, 3]);
    }

    #[test]
    fn max_nodes_circuit_breaker() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 2, // Only allow 2 nodes
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert!(result.visited.len() <= 2);
        assert!(
            result.truncated,
            "max_nodes stopping expansion before natural exhaustion must report truncated"
        );
    }

    #[test]
    fn tombstoned_nodes_skipped_during_traversal() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        ns.add_node(100, "B".to_string());
        ns.add_node(100, "C".to_string());
        // Tombstone B — BFS should skip it and NOT reach C through B
        ns.deactivate(1);

        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(3, edges, false);
        let fi = FilterIndex::new();

        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        // A(0) is visited, B(1) is tombstoned and skipped, C(2) unreachable
        assert!(result.visited.contains(0));
        assert!(!result.visited.contains(1));
        assert!(!result.visited.contains(2));
    }

    #[test]
    fn isolated_node_returns_only_seed() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "lonely".to_string());
        let es = EdgeStore::from_edges(1, vec![], false);
        let fi = FilterIndex::new();

        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert_eq!(result.visited.len(), 1);
        assert!(result.visited.contains(0));
        assert_eq!(result.depth.get(0), Some(0));
    }

    #[test]
    fn depth_zero_returns_only_seed() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 0, // Only seed
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert_eq!(result.visited.len(), 1);
        assert!(result.visited.contains(0));
    }

    #[test]
    fn max_frontier_limits_exploration() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 1, // Very tight frontier — limits per-level expansion
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        // With frontier=1, BFS can't expand beyond the first neighbor
        assert!(
            result.visited.len() < 5,
            "frontier=1 should limit expansion, got {}",
            result.visited.len()
        );
        assert!(
            result.truncated,
            "max_frontier stopping expansion before natural exhaustion must report truncated"
        );
    }

    #[test]
    fn natural_completion_is_not_truncated() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert!(
            !result.truncated,
            "generous max_nodes/max_frontier limits should let the frontier exhaust naturally"
        );
    }

    #[test]
    fn depth_limit_alone_is_not_truncated() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 0,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        // Stopping at a caller-requested max_depth is a complete, correct
        // answer for that depth bound, not a resource-driven truncation.
        assert!(!result.truncated);
    }

    #[test]
    fn dfs_max_nodes_circuit_breaker_reports_truncated() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 2,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute_dfs(&ns, &es, &fi, &config);
        assert!(result.visited.len() <= 2);
        assert!(result.truncated);
    }

    #[test]
    fn dfs_natural_completion_is_not_truncated() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute_dfs(&ns, &es, &fi, &config);
        assert!(!result.truncated);
    }

    #[test]
    fn self_loop_does_not_cause_infinite_loop() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "self".to_string());

        let edges = vec![RawEdge {
            source: 0,
            target: 0,
            type_id: 1,
            weight: None,
            schema_reversed: false,
        }];
        let es = EdgeStore::from_edges(1, edges, false);
        let fi = FilterIndex::new();

        let config = BfsConfig {
            seed_node: 0,
            max_depth: 100,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert_eq!(result.visited.len(), 1); // Just the seed
        assert!(result.visited.contains(0));
    }

    #[test]
    fn path_reconstruction_on_isolated_returns_just_seed() {
        let mut result_parent =
            TraversalParentMap::try_new(1, false, 1).expect("test parent map should allocate");
        result_parent.set(0, 0);
        let path = reconstruct_path(&result_parent, 0, 0);
        assert_eq!(path, vec![0]);
    }

    #[test]
    fn traversal_metadata_switches_to_sparse_for_small_visit_budgets() {
        assert!(use_sparse_metadata(1_000_000, 10_000));
        assert!(!use_sparse_metadata(1_000_000, 100_000));
        assert!(!use_sparse_metadata(100, 1));
    }

    #[test]
    fn empty_edge_type_filter_matches_nothing() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let edge_filter = HashSet::new(); // Empty — no types match

        let config = BfsConfig {
            seed_node: 0,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::Only(edge_filter),
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        // With no edge types allowed, only the seed is reachable
        assert_eq!(result.visited.len(), 1);
    }

    #[test]
    fn invalid_seed_returns_empty_result_instead_of_panicking() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 99,
            max_depth: 10,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);

        assert!(result.visited.is_empty());
        assert_eq!(result.depth.len(), ns.node_count() as usize);
        assert!(result.depth.all_unvisited());
        assert!(result.parent.all_unvisited());
    }

    #[test]
    fn to_traversal_results_includes_all_columns() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: 2,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let bfs_result = execute(&ns, &es, &fi, &config);
        let edge_type_registry = vec!["".to_string(), "test".to_string()];
        let results = to_traversal_results(&bfs_result, &ns, &edge_type_registry).unwrap();

        // Verify all columns are populated
        for r in &results {
            assert!(!r.node_id.is_empty());
            assert!(!r.path.is_empty());
            assert!(r.depth >= 0);
        }

        // Seed at depth 0, path = [{table_oid: 100, node_id: "PK-0"}]
        let seed = results.iter().find(|r| r.depth == 0).unwrap();
        assert_eq!(seed.node_id, "PK-0");
        assert_eq!(seed.path[0].table_oid, TableOid(100));
        assert_eq!(seed.path[0].node_id, "PK-0");
        assert!(seed.edge_path.is_empty());

        let neighbor = results.iter().find(|r| r.node_id == "PK-1").unwrap();
        assert_eq!(neighbor.edge_path, vec!["test"]);

        // Results are sorted by depth
        for w in results.windows(2) {
            assert!(w[0].depth <= w[1].depth);
        }
    }

    #[test]
    fn disconnected_component_not_reached() {
        let mut ns = NodeStore::new();
        for i in 0..4u32 {
            ns.add_node(100, format!("N-{}", i));
        }
        // 0→1, 2→3 (two disconnected pairs)
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 3,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(4, edges, false);
        let fi = FilterIndex::new();

        let config = BfsConfig {
            seed_node: 0,
            max_depth: 100,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert_eq!(result.visited.len(), 2); // Only 0 and 1
        assert!(!result.visited.contains(2));
        assert!(!result.visited.contains(3));
    }

    #[test]
    fn negative_max_depth_returns_only_seed() {
        let (ns, es) = build_test_graph();
        let fi = FilterIndex::new();
        let config = BfsConfig {
            seed_node: 0,
            max_depth: -1,
            max_nodes: 100000,
            max_frontier: 100000,
            edge_type_filter: crate::types::EdgeTypeFilter::All,
            filter_ops: vec![],
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: std::collections::HashMap::new(),
            tenant_membership_removals: std::collections::HashMap::new(),
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        // Seed is inserted at depth 0, which is >= max_depth(-1), so no expansion
        assert_eq!(result.visited.len(), 1);
        assert!(result.visited.contains(0));
    }
}
