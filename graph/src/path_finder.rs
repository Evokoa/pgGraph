//! # PathFinder — Shortest path algorithms
//!
//! Bidirectional BFS for unweighted shortest path.
//! Dijkstra (BinaryHeap) for weighted shortest path.
//!
//! See: `docs/contributor_guide/traversal-search-paths.mdx`

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};

use roaring::RoaringBitmap;

use crate::bfs::{
    BfsAdjacencyCandidate, BfsAdjacencyCandidateBatch, BfsAdjacencyVerdict, BfsCandidateLimits,
    BfsProjectionEpoch,
};
#[cfg(test)]
use crate::edge_store::EdgeStore;
use crate::node_store::NodeStore;
#[cfg(test)]
use crate::projection::neighbors::CsrNeighbors;
use crate::projection::neighbors::{
    Neighbor, NeighborSource, OwnedNeighborCursor, WeightedNeighbor, WeightedNeighborSource,
};
use crate::resource::{ResourceGovernor, ResourcePhase, WorkUnits};
use crate::safety::{GraphError, GraphResult};
use crate::types::{EdgeTypeId, PathStep, TableOid, WeightedPathStep};
#[cfg(any(test, feature = "benchmarks"))]
use crate::visibility::VisibilityCoordinator;
use crate::visibility::{QueryExecutionContext, VisibilityScope};

#[derive(Debug, Clone, Copy)]
struct ParentStep {
    parent: u32,
    edge_type: EdgeTypeId,
    /// Hop distance from this step's own BFS root (source for `fwd_parent`,
    /// target for `bwd_parent`). Needed so `bidirectional_bfs` can compare
    /// the combined distance of multiple meeting candidates discovered in
    /// the same level, rather than accepting whichever is found first.
    depth: i32,
}

/// Yield returned by an owned unweighted-path machine.
#[derive(Debug)]
pub(crate) enum ResumablePathMaterialization {
    Batch(BfsAdjacencyCandidateBatch),
    Progress,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumablePathState {
    NeedEndpoints,
    NeedCandidates,
    NeedVisibility,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingPathBatch {
    Endpoints,
    Adjacency,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchSide {
    Forward,
    Backward,
}

#[derive(Debug)]
struct PathSideState {
    visited: RoaringBitmap,
    parent: HashMap<u32, ParentStep>,
    frontier: VecDeque<u32>,
}

impl PathSideState {
    fn try_new(root: u32, _node_count: usize) -> GraphResult<Self> {
        let mut parent = HashMap::new();
        parent.try_reserve(1).map_err(path_allocation_error)?;
        parent.insert(
            root,
            ParentStep {
                parent: root,
                edge_type: EdgeTypeId::UNTYPED,
                depth: 0,
            },
        );
        let mut visited = RoaringBitmap::new();
        visited.insert(root);
        let mut frontier = VecDeque::new();
        frontier.try_reserve(1).map_err(path_allocation_error)?;
        frontier.push_back(root);
        Ok(Self {
            visited,
            parent,
            frontier,
        })
    }
}

/// Owned single-direction BFS state. Callers may drop every projection borrow
/// while the returned candidate batch is resolved by PostgreSQL.
#[derive(Debug)]
pub(crate) struct ResumableSingleDirectionBfs {
    source: u32,
    target: u32,
    max_depth: i32,
    frontier: VecDeque<(u32, i32)>,
    visited: RoaringBitmap,
    parent: HashMap<u32, ParentStep>,
    adjacency_cursor: OwnedNeighborCursor,
    pending_adjacency: VecDeque<Neighbor>,
    pending_adjacency_exhausts_node: bool,
    active_node: Option<(u32, i32)>,
    endpoint_cursor: usize,
    source_visible: Option<bool>,
    target_visible: Option<bool>,
    state: ResumablePathState,
    pending_kind: Option<PendingPathBatch>,
    pending_batch: Option<(Option<u32>, usize)>,
    next_sequence: u32,
    found: bool,
    projection_epoch: Option<BfsProjectionEpoch>,
}

impl ResumableSingleDirectionBfs {
    pub(crate) fn try_new(
        node_count: usize,
        source: u32,
        target: u32,
        max_depth: i32,
    ) -> GraphResult<Self> {
        let valid = (source as usize) < node_count && (target as usize) < node_count;
        let mut frontier = VecDeque::new();
        frontier.try_reserve(1).map_err(path_allocation_error)?;
        let mut visited = RoaringBitmap::new();
        let mut parent = HashMap::new();
        parent.try_reserve(1).map_err(path_allocation_error)?;
        if valid {
            visited.insert(source);
            parent.insert(
                source,
                ParentStep {
                    parent: source,
                    edge_type: EdgeTypeId::UNTYPED,
                    depth: 0,
                },
            );
        }
        let mut pending_adjacency = VecDeque::new();
        pending_adjacency
            .try_reserve(crate::bfs::RESUMABLE_BFS_PAGE_CAPACITY)
            .map_err(path_allocation_error)?;
        Ok(Self {
            source,
            target,
            max_depth,
            frontier,
            visited,
            parent,
            adjacency_cursor: OwnedNeighborCursor::default(),
            pending_adjacency,
            pending_adjacency_exhausts_node: false,
            active_node: None,
            endpoint_cursor: 0,
            source_visible: None,
            target_visible: None,
            state: if valid {
                ResumablePathState::NeedEndpoints
            } else {
                ResumablePathState::Complete
            },
            pending_kind: None,
            pending_batch: None,
            next_sequence: 0,
            found: false,
            projection_epoch: None,
        })
    }

    pub(crate) fn bind_projection_epoch(&mut self, epoch: BfsProjectionEpoch) {
        self.projection_epoch = Some(epoch);
    }

    pub(crate) fn require_projection_epoch(&self, actual: BfsProjectionEpoch) -> GraphResult<()> {
        require_path_epoch(self.projection_epoch, actual)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.state == ResumablePathState::Complete
    }

    #[cfg(any(test, feature = "benchmarks"))]
    pub(crate) fn finish(
        self,
        node_store: &NodeStore,
        edge_type_registry: &[String],
    ) -> GraphResult<Option<Vec<PathStep>>> {
        self.finish_with(node_store, |type_id| {
            edge_type_registry.get(type_id.get() as usize).cloned()
        })
    }

    pub(crate) fn finish_with(
        self,
        node_store: &NodeStore,
        edge_type_label: impl Fn(EdgeTypeId) -> Option<String>,
    ) -> GraphResult<Option<Vec<PathStep>>> {
        if !self.found {
            return Ok(None);
        }
        path_from_single_parents(
            node_store,
            self.source,
            self.target,
            &self.parent,
            edge_type_label,
        )
        .map(Some)
    }
}

/// Owned bidirectional BFS state preserving the eager level and meeting-node
/// tie rules while visibility is resolved between bounded pages.
#[derive(Debug)]
pub(crate) struct ResumableBidirectionalBfs {
    source: u32,
    target: u32,
    max_depth: i32,
    forward: PathSideState,
    backward: PathSideState,
    selected_side: Option<SearchSide>,
    level_remaining: usize,
    round_depth: i32,
    meeting_node: Option<u32>,
    best_combined: i32,
    adjacency_cursor: OwnedNeighborCursor,
    pending_adjacency: VecDeque<Neighbor>,
    pending_adjacency_exhausts_node: bool,
    active_node: Option<u32>,
    endpoint_cursor: usize,
    source_visible: Option<bool>,
    target_visible: Option<bool>,
    state: ResumablePathState,
    pending_kind: Option<PendingPathBatch>,
    pending_batch: Option<(Option<u32>, usize)>,
    next_sequence: u32,
    found: bool,
    projection_epoch: Option<BfsProjectionEpoch>,
}

impl ResumableBidirectionalBfs {
    pub(crate) fn try_new(
        node_count: usize,
        source: u32,
        target: u32,
        max_depth: i32,
    ) -> GraphResult<Self> {
        let valid = (source as usize) < node_count && (target as usize) < node_count;
        let forward = PathSideState::try_new(source, node_count)?;
        let backward = PathSideState::try_new(target, node_count)?;
        let mut pending_adjacency = VecDeque::new();
        pending_adjacency
            .try_reserve(crate::bfs::RESUMABLE_BFS_PAGE_CAPACITY)
            .map_err(path_allocation_error)?;
        Ok(Self {
            source,
            target,
            max_depth,
            forward,
            backward,
            selected_side: None,
            level_remaining: 0,
            round_depth: 0,
            meeting_node: None,
            best_combined: i32::MAX,
            adjacency_cursor: OwnedNeighborCursor::default(),
            pending_adjacency,
            pending_adjacency_exhausts_node: false,
            active_node: None,
            endpoint_cursor: 0,
            source_visible: None,
            target_visible: None,
            state: if valid {
                ResumablePathState::NeedEndpoints
            } else {
                ResumablePathState::Complete
            },
            pending_kind: None,
            pending_batch: None,
            next_sequence: 0,
            found: false,
            projection_epoch: None,
        })
    }

    pub(crate) fn bind_projection_epoch(&mut self, epoch: BfsProjectionEpoch) {
        self.projection_epoch = Some(epoch);
    }

    pub(crate) fn require_projection_epoch(&self, actual: BfsProjectionEpoch) -> GraphResult<()> {
        require_path_epoch(self.projection_epoch, actual)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.state == ResumablePathState::Complete
    }

    #[cfg(any(test, feature = "benchmarks"))]
    pub(crate) fn finish(
        self,
        node_store: &NodeStore,
        edge_type_registry: &[String],
    ) -> GraphResult<Option<Vec<PathStep>>> {
        self.finish_with(node_store, |type_id| {
            edge_type_registry.get(type_id.get() as usize).cloned()
        })
    }

    pub(crate) fn finish_with(
        self,
        node_store: &NodeStore,
        edge_type_label: impl Fn(EdgeTypeId) -> Option<String>,
    ) -> GraphResult<Option<Vec<PathStep>>> {
        if !self.found {
            return Ok(None);
        }
        path_from_bidirectional_parents(
            node_store,
            self.source,
            self.target,
            self.meeting_node,
            &self.forward.parent,
            &self.backward.parent,
            edge_type_label,
        )
        .map(Some)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "path paging keeps owned search state, projection identity, filtering, and budget explicit"
)]
pub(crate) fn materialize_single_direction_path_batch(
    machine: &mut ResumableSingleDirectionBfs,
    node_store: &NodeStore,
    neighbors: &(impl NeighborSource + ?Sized),
    relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
    edge_type_filter: Option<&RoaringBitmap>,
    limits: BfsCandidateLimits,
    governor: &ResourceGovernor,
) -> GraphResult<ResumablePathMaterialization> {
    validate_path_materialization(machine.state, limits)?;
    let _page_lease = reserve_path_page_workspace(governor, limits)?;
    if machine.state == ResumablePathState::NeedEndpoints {
        return materialize_endpoint_batch(
            machine.source,
            machine.target,
            &mut machine.endpoint_cursor,
            &mut machine.next_sequence,
            &mut machine.pending_batch,
            &mut machine.pending_kind,
            &mut machine.state,
            node_store,
            limits,
        );
    }

    loop {
        let (current, current_depth) = if let Some(active) = machine.active_node {
            active
        } else if let Some(active) = machine.frontier.pop_front() {
            machine.active_node = Some(active);
            machine.adjacency_cursor = OwnedNeighborCursor::default();
            machine.pending_adjacency.clear();
            machine.pending_adjacency_exhausts_node = false;
            active
        } else {
            machine.state = ResumablePathState::Complete;
            return Ok(ResumablePathMaterialization::Complete);
        };
        if current_depth >= machine.max_depth {
            reset_single_active(machine);
            continue;
        }

        fill_path_raw_page(
            current,
            neighbors,
            &mut machine.adjacency_cursor,
            &mut machine.pending_adjacency,
            &mut machine.pending_adjacency_exhausts_node,
            limits.max_candidates,
        )?;
        let mut candidates = path_candidate_vec(limits)?;
        let mut key_bytes = 0usize;
        let mut stopped_at_target = false;
        while let Some(neighbor) = machine.pending_adjacency.pop_front() {
            if edge_type_filter.is_some_and(|allowed| !allowed.contains(neighbor.type_id.get())) {
                consume_path_work_without_interrupt(governor)?;
                continue;
            }
            if !path_candidate_fits(
                &candidates,
                &mut key_bytes,
                neighbor,
                Some(&mut machine.pending_adjacency),
                limits,
                node_store,
                relationships,
            )? {
                break;
            }
            let candidate = path_candidate(
                machine.next_sequence,
                current,
                current_depth,
                neighbor,
                node_store,
                relationships,
            )?;
            candidates.push(candidate);
            consume_path_work_without_interrupt(governor)?;
            machine.next_sequence = next_path_sequence(machine.next_sequence)?;
            if neighbor.target == machine.target {
                stopped_at_target = true;
                break;
            }
        }
        let exhausted_current = machine.pending_adjacency.is_empty()
            && machine.pending_adjacency_exhausts_node
            && !stopped_at_target;
        if exhausted_current {
            reset_single_active(machine);
        }
        if candidates.is_empty() {
            return Ok(
                if machine.active_node.is_none() && machine.frontier.is_empty() {
                    machine.state = ResumablePathState::Complete;
                    ResumablePathMaterialization::Complete
                } else {
                    ResumablePathMaterialization::Progress
                },
            );
        }
        return finish_path_batch(
            candidates,
            exhausted_current,
            limits,
            &mut machine.pending_batch,
            &mut machine.pending_kind,
            &mut machine.state,
        );
    }
}

pub(crate) fn apply_single_direction_path_verdicts(
    machine: &mut ResumableSingleDirectionBfs,
    batch: &BfsAdjacencyCandidateBatch,
    verdicts: &[BfsAdjacencyVerdict],
    node_store: &NodeStore,
    governor: &ResourceGovernor,
) -> GraphResult<()> {
    let kind = validate_path_verdicts(
        machine.state,
        machine.pending_batch,
        machine.pending_kind,
        batch,
        verdicts,
    )?;
    machine.pending_batch = None;
    machine.pending_kind = None;
    match kind {
        PendingPathBatch::Endpoints => {
            apply_endpoint_verdicts(
                machine.source,
                machine.target,
                batch,
                verdicts,
                &mut machine.source_visible,
                &mut machine.target_visible,
            );
            if machine.endpoint_cursor < endpoint_count(machine.source, machine.target) {
                machine.state = ResumablePathState::NeedEndpoints;
            } else if machine.source_visible != Some(true) || machine.target_visible != Some(true) {
                machine.state = ResumablePathState::Complete;
            } else if machine.source == machine.target {
                machine.found = true;
                machine.state = ResumablePathState::Complete;
            } else if machine.max_depth <= 0 {
                machine.state = ResumablePathState::Complete;
            } else {
                machine.frontier.push_back((machine.source, 0));
                machine.state = ResumablePathState::NeedCandidates;
            }
        }
        PendingPathBatch::Adjacency => {
            for (candidate, verdict) in batch.candidates.iter().zip(verdicts) {
                if !verdict.visible()
                    || machine.visited.contains(candidate.target_node)
                    || !node_store.is_active(candidate.target_node)
                    || crate::projection::tx_delta::node_deleted(candidate.target_node)
                {
                    continue;
                }
                machine.visited.insert(candidate.target_node);
                governor
                    .reserve_memory(
                        ResourcePhase::QueryPaths,
                        crate::resource::ByteCount::from_bytes(PATH_STATE_BYTES_PER_VISIT as u64),
                    )
                    .map_err(crate::safety::resource_limit_error)?
                    .retain_until_governor_drop();
                machine
                    .parent
                    .try_reserve(1)
                    .map_err(path_allocation_error)?;
                machine
                    .frontier
                    .try_reserve(1)
                    .map_err(path_allocation_error)?;
                machine.parent.insert(
                    candidate.target_node,
                    ParentStep {
                        parent: candidate.parent_node,
                        edge_type: candidate.edge_type,
                        depth: candidate.parent_depth + 1,
                    },
                );
                machine
                    .frontier
                    .push_back((candidate.target_node, candidate.parent_depth + 1));
                if candidate.target_node == machine.target {
                    machine.found = true;
                    machine.state = ResumablePathState::Complete;
                    return Ok(());
                }
            }
            machine.state = ResumablePathState::NeedCandidates;
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "path paging keeps owned search state, projection identity, filtering, and budget explicit"
)]
pub(crate) fn materialize_bidirectional_path_batch(
    machine: &mut ResumableBidirectionalBfs,
    node_store: &NodeStore,
    neighbors: &(impl NeighborSource + ?Sized),
    relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
    edge_type_filter: Option<&RoaringBitmap>,
    limits: BfsCandidateLimits,
    governor: &ResourceGovernor,
) -> GraphResult<ResumablePathMaterialization> {
    validate_path_materialization(machine.state, limits)?;
    let _page_lease = reserve_path_page_workspace(governor, limits)?;
    if machine.state == ResumablePathState::NeedEndpoints {
        return materialize_endpoint_batch(
            machine.source,
            machine.target,
            &mut machine.endpoint_cursor,
            &mut machine.next_sequence,
            &mut machine.pending_batch,
            &mut machine.pending_kind,
            &mut machine.state,
            node_store,
            limits,
        );
    }

    {
        if machine.selected_side.is_some()
            && machine.active_node.is_none()
            && machine.level_remaining == 0
        {
            if machine.meeting_node.is_some() {
                machine.found = true;
                machine.state = ResumablePathState::Complete;
                return Ok(ResumablePathMaterialization::Complete);
            }
            machine.round_depth += 1;
            machine.selected_side = None;
        }
        if machine.selected_side.is_none() {
            if machine.forward.frontier.is_empty()
                || machine.backward.frontier.is_empty()
                || machine.round_depth >= machine.max_depth
            {
                machine.state = ResumablePathState::Complete;
                return Ok(ResumablePathMaterialization::Complete);
            }
            let side = if machine.forward.frontier.len() <= machine.backward.frontier.len() {
                SearchSide::Forward
            } else {
                SearchSide::Backward
            };
            machine.level_remaining = side_state(machine, side).frontier.len();
            machine.selected_side = Some(side);
        }
        let side = machine.selected_side.ok_or_else(|| {
            GraphError::Internal("bidirectional path level has no selected side".into())
        })?;
        let current = if let Some(current) = machine.active_node {
            current
        } else {
            let Some(current) = side_state_mut(machine, side).frontier.pop_front() else {
                return Err(GraphError::Internal(
                    "bidirectional path level lost its frozen frontier".into(),
                ));
            };
            machine.level_remaining = machine.level_remaining.saturating_sub(1);
            machine.active_node = Some(current);
            machine.adjacency_cursor = OwnedNeighborCursor::default();
            machine.pending_adjacency.clear();
            machine.pending_adjacency_exhausts_node = false;
            current
        };
        fill_path_raw_page(
            current,
            neighbors,
            &mut machine.adjacency_cursor,
            &mut machine.pending_adjacency,
            &mut machine.pending_adjacency_exhausts_node,
            limits.max_candidates,
        )?;
        let mut candidates = path_candidate_vec(limits)?;
        let mut key_bytes = 0usize;
        while let Some(neighbor) = machine.pending_adjacency.pop_front() {
            if edge_type_filter.is_some_and(|allowed| !allowed.contains(neighbor.type_id.get())) {
                consume_path_work_without_interrupt(governor)?;
                continue;
            }
            if !path_candidate_fits(
                &candidates,
                &mut key_bytes,
                neighbor,
                Some(&mut machine.pending_adjacency),
                limits,
                node_store,
                relationships,
            )? {
                break;
            }
            let candidate = path_candidate(
                machine.next_sequence,
                current,
                machine.round_depth,
                neighbor,
                node_store,
                relationships,
            )?;
            candidates.push(candidate);
            consume_path_work_without_interrupt(governor)?;
            machine.next_sequence = next_path_sequence(machine.next_sequence)?;
        }
        let exhausted_current =
            machine.pending_adjacency.is_empty() && machine.pending_adjacency_exhausts_node;
        if exhausted_current {
            reset_bidirectional_active(machine);
        }
        if candidates.is_empty() {
            return Ok(ResumablePathMaterialization::Progress);
        }
        finish_path_batch(
            candidates,
            exhausted_current,
            limits,
            &mut machine.pending_batch,
            &mut machine.pending_kind,
            &mut machine.state,
        )
    }
}

pub(crate) fn apply_bidirectional_path_verdicts(
    machine: &mut ResumableBidirectionalBfs,
    batch: &BfsAdjacencyCandidateBatch,
    verdicts: &[BfsAdjacencyVerdict],
    node_store: &NodeStore,
    governor: &ResourceGovernor,
) -> GraphResult<()> {
    let kind = validate_path_verdicts(
        machine.state,
        machine.pending_batch,
        machine.pending_kind,
        batch,
        verdicts,
    )?;
    machine.pending_batch = None;
    machine.pending_kind = None;
    match kind {
        PendingPathBatch::Endpoints => {
            apply_endpoint_verdicts(
                machine.source,
                machine.target,
                batch,
                verdicts,
                &mut machine.source_visible,
                &mut machine.target_visible,
            );
            if machine.endpoint_cursor < endpoint_count(machine.source, machine.target) {
                machine.state = ResumablePathState::NeedEndpoints;
            } else if machine.source_visible != Some(true) || machine.target_visible != Some(true) {
                machine.state = ResumablePathState::Complete;
            } else if machine.source == machine.target {
                machine.meeting_node = Some(machine.source);
                machine.found = true;
                machine.state = ResumablePathState::Complete;
            } else {
                machine.state = ResumablePathState::NeedCandidates;
            }
        }
        PendingPathBatch::Adjacency => {
            let side = machine.selected_side.ok_or_else(|| {
                GraphError::Internal("bidirectional path verdict has no selected side".into())
            })?;
            let new_depth = machine.round_depth + 1;
            for (candidate, verdict) in batch.candidates.iter().zip(verdicts) {
                if !verdict.visible()
                    || !node_store.is_active(candidate.target_node)
                    || crate::projection::tx_delta::node_deleted(candidate.target_node)
                {
                    continue;
                }
                let (selected, opposite) = split_side_states(machine, side);
                if !selected.visited.contains(candidate.target_node) {
                    reserve_path_state_growth(
                        governor,
                        &mut selected.parent,
                        &mut selected.frontier,
                    )?;
                    selected.visited.insert(candidate.target_node);
                    selected.parent.insert(
                        candidate.target_node,
                        ParentStep {
                            parent: candidate.parent_node,
                            edge_type: candidate.edge_type,
                            depth: new_depth,
                        },
                    );
                    selected.frontier.push_back(candidate.target_node);
                }
                if opposite.visited.contains(candidate.target_node) {
                    selected
                        .parent
                        .entry(candidate.target_node)
                        .or_insert(ParentStep {
                            parent: candidate.parent_node,
                            edge_type: candidate.edge_type,
                            depth: new_depth,
                        });
                    let selected_depth = selected.parent[&candidate.target_node].depth;
                    let opposite_depth = opposite.parent[&candidate.target_node].depth;
                    let combined = selected_depth + opposite_depth;
                    if combined < machine.best_combined {
                        machine.best_combined = combined;
                        machine.meeting_node = Some(candidate.target_node);
                    }
                }
            }
            machine.state = ResumablePathState::NeedCandidates;
        }
    }
    Ok(())
}

fn validate_path_materialization(
    state: ResumablePathState,
    limits: BfsCandidateLimits,
) -> GraphResult<()> {
    if !matches!(
        state,
        ResumablePathState::NeedEndpoints | ResumablePathState::NeedCandidates
    ) {
        return Err(GraphError::Internal(
            "path candidate materialization requires a candidate state".into(),
        ));
    }
    if limits.max_candidates == 0
        || limits.max_candidates > crate::bfs::RESUMABLE_BFS_PAGE_CAPACITY
        || limits.max_key_bytes == 0
    {
        return Err(GraphError::InvalidFilter {
            reason: "path visibility candidate limits must be positive and bounded".into(),
        });
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "endpoint paging updates the caller-owned visibility protocol state"
)]
fn materialize_endpoint_batch(
    source: u32,
    target: u32,
    endpoint_cursor: &mut usize,
    next_sequence: &mut u32,
    pending_batch: &mut Option<(Option<u32>, usize)>,
    pending_kind: &mut Option<PendingPathBatch>,
    state: &mut ResumablePathState,
    node_store: &NodeStore,
    limits: BfsCandidateLimits,
) -> GraphResult<ResumablePathMaterialization> {
    let endpoints = if source == target {
        [Some(source), None]
    } else {
        [Some(source), Some(target)]
    };
    let mut candidates = path_candidate_vec(limits)?;
    let mut key_bytes = 0usize;
    while *endpoint_cursor < endpoints.len() && candidates.len() < limits.max_candidates {
        let Some(node) = endpoints[*endpoint_cursor] else {
            *endpoint_cursor += 1;
            continue;
        };
        let table_oid = node_store
            .table_oid(node)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("path endpoint {node} has no table identity"),
            })?;
        let source_key = node_store
            .primary_key(node)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("path endpoint {node} has no source identity"),
            })?;
        let next_bytes =
            key_bytes
                .checked_add(source_key.len())
                .ok_or_else(|| GraphError::InvalidFilter {
                    reason: "path endpoint key bytes overflow".into(),
                })?;
        if next_bytes > limits.max_key_bytes {
            if candidates.is_empty() {
                return Err(GraphError::InvalidFilter {
                    reason: "one path endpoint exceeds the key-byte limit".into(),
                });
            }
            break;
        }
        candidates.push(BfsAdjacencyCandidate {
            sequence: *next_sequence,
            parent_node: node,
            parent_depth: 0,
            target_node: node,
            target_table_oid: table_oid,
            target_source_key: source_key.to_owned(),
            edge_type: EdgeTypeId::UNTYPED,
            schema_reversed: false,
            relationship_id: None,
            relationship_mapping_id: None,
            relationship_source_key: None,
        });
        *next_sequence = next_path_sequence(*next_sequence)?;
        *endpoint_cursor += 1;
        key_bytes = next_bytes;
    }
    let materialization = finish_path_batch(
        candidates,
        *endpoint_cursor >= endpoint_count(source, target),
        limits,
        pending_batch,
        pending_kind,
        state,
    )?;
    *pending_kind = Some(PendingPathBatch::Endpoints);
    Ok(materialization)
}

fn endpoint_count(source: u32, target: u32) -> usize {
    usize::from(source != target) + 1
}

const PATH_STATE_BYTES_PER_VISIT: usize = 512;

pub(crate) fn estimated_resumable_path_workspace_bytes(
    bidirectional: bool,
) -> GraphResult<crate::resource::ByteCount> {
    let roots = if bidirectional { 2usize } else { 1usize };
    let page_bytes = crate::bfs::RESUMABLE_BFS_PAGE_CAPACITY
        .checked_mul(
            std::mem::size_of::<Neighbor>()
                .checked_mul(2)
                .and_then(|bytes| bytes.checked_add(std::mem::size_of::<BfsAdjacencyCandidate>()))
                .ok_or_else(|| GraphError::Internal("path page item size overflowed".into()))?,
        )
        .ok_or_else(|| GraphError::Internal("path page workspace overflowed".into()))?;
    let bytes = std::mem::size_of::<ResumableBidirectionalBfs>()
        .max(std::mem::size_of::<ResumableSingleDirectionBfs>())
        .checked_add(page_bytes)
        .and_then(|bytes| bytes.checked_add(1024 * 1024))
        .and_then(|bytes| bytes.checked_add(roots * PATH_STATE_BYTES_PER_VISIT))
        .ok_or_else(|| GraphError::Internal("path machine workspace overflowed".into()))?;
    crate::resource::ByteCount::from_usize(bytes)
        .ok_or_else(|| GraphError::Internal("path machine workspace does not fit u64".into()))
}

fn reserve_path_state_growth(
    governor: &ResourceGovernor,
    parent: &mut HashMap<u32, ParentStep>,
    frontier: &mut VecDeque<u32>,
) -> GraphResult<()> {
    governor
        .reserve_memory(
            ResourcePhase::QueryPaths,
            crate::resource::ByteCount::from_bytes(PATH_STATE_BYTES_PER_VISIT as u64),
        )
        .map_err(crate::safety::resource_limit_error)?
        .retain_until_governor_drop();
    parent.try_reserve(1).map_err(path_allocation_error)?;
    frontier.try_reserve(1).map_err(path_allocation_error)?;
    Ok(())
}

fn apply_endpoint_verdicts(
    source: u32,
    target: u32,
    batch: &BfsAdjacencyCandidateBatch,
    verdicts: &[BfsAdjacencyVerdict],
    source_visible: &mut Option<bool>,
    target_visible: &mut Option<bool>,
) {
    for (candidate, verdict) in batch.candidates.iter().zip(verdicts) {
        if candidate.target_node == source && source_visible.is_none() {
            *source_visible = Some(verdict.node_visible);
            if source == target {
                *target_visible = Some(verdict.node_visible);
            }
        } else if candidate.target_node == target {
            *target_visible = Some(verdict.node_visible);
        }
    }
}

fn fill_path_raw_page(
    current: u32,
    neighbors: &(impl NeighborSource + ?Sized),
    cursor: &mut OwnedNeighborCursor,
    pending: &mut VecDeque<Neighbor>,
    pending_exhausts_node: &mut bool,
    limit: usize,
) -> GraphResult<()> {
    if pending.is_empty() {
        let mut page = Vec::new();
        page.try_reserve(limit).map_err(path_allocation_error)?;
        *pending_exhausts_node = neighbors.fill_neighbors(current, cursor, limit, &mut page);
        pending
            .try_reserve(page.len())
            .map_err(path_allocation_error)?;
        pending.extend(page);
    }
    Ok(())
}

fn path_candidate_vec(limits: BfsCandidateLimits) -> GraphResult<Vec<BfsAdjacencyCandidate>> {
    let mut candidates = Vec::new();
    candidates
        .try_reserve(limits.max_candidates)
        .map_err(path_allocation_error)?;
    Ok(candidates)
}

fn reserve_path_page_workspace(
    governor: &ResourceGovernor,
    limits: BfsCandidateLimits,
) -> GraphResult<crate::resource::ResourceLease<'_>> {
    let bytes = limits
        .max_candidates
        .checked_mul(std::mem::size_of::<BfsAdjacencyCandidate>())
        .and_then(|bytes| {
            bytes.checked_add(
                limits
                    .max_candidates
                    .checked_mul(std::mem::size_of::<Neighbor>())?,
            )
        })
        .and_then(|bytes| {
            bytes.checked_add(
                limits
                    .max_candidates
                    .checked_mul(std::mem::size_of::<WeightedNeighbor>())?,
            )
        })
        .and_then(|bytes| bytes.checked_add(limits.max_key_bytes))
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "path visibility candidate allocation estimate overflow".into(),
        })?;
    governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            crate::resource::ByteCount::from_usize(bytes).ok_or_else(|| {
                GraphError::Internal("path page workspace does not fit u64".into())
            })?,
        )
        .map_err(crate::safety::resource_limit_error)
}

fn path_candidate(
    sequence: u32,
    current: u32,
    current_depth: i32,
    neighbor: Neighbor,
    node_store: &NodeStore,
    relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
) -> GraphResult<BfsAdjacencyCandidate> {
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
    let relationship_identity = neighbor.relationship_id.and_then(|relationship_id| {
        path_relationship_identity(relationships, relationship_id, |mapping_id, source_key| {
            (mapping_id, source_key.to_owned())
        })
    });
    Ok(BfsAdjacencyCandidate {
        sequence,
        parent_node: current,
        parent_depth: current_depth,
        target_node: neighbor.target,
        target_table_oid,
        target_source_key: target_source_key.to_owned(),
        edge_type: neighbor.type_id,
        schema_reversed: neighbor.schema_reversed,
        relationship_id: neighbor.relationship_id,
        relationship_mapping_id: relationship_identity.as_ref().map(|value| value.0),
        relationship_source_key: relationship_identity.map(|value| value.1),
    })
}

fn path_relationship_identity<T>(
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

fn path_candidate_fits(
    candidates: &[BfsAdjacencyCandidate],
    key_bytes: &mut usize,
    neighbor: Neighbor,
    pending: Option<&mut VecDeque<Neighbor>>,
    limits: BfsCandidateLimits,
    node_store: &NodeStore,
    relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
) -> GraphResult<bool> {
    let target_source_key =
        node_store
            .primary_key(neighbor.target)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("node {} has no source identity", neighbor.target),
            })?;
    let relationship_key_bytes = neighbor
        .relationship_id
        .and_then(|relationship_id| {
            path_relationship_identity(relationships, relationship_id, |_, source_key| {
                source_key.len()
            })
        })
        .unwrap_or_default();
    let candidate_bytes = target_source_key
        .len()
        .checked_add(relationship_key_bytes)
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "path visibility candidate key bytes overflow".into(),
        })?;
    let next_bytes =
        key_bytes
            .checked_add(candidate_bytes)
            .ok_or_else(|| GraphError::InvalidFilter {
                reason: "path visibility candidate key bytes overflow".into(),
            })?;
    if next_bytes > limits.max_key_bytes {
        if candidates.is_empty() {
            return Err(GraphError::InvalidFilter {
                reason: "one path visibility candidate exceeds the key-byte limit".into(),
            });
        }
        if let Some(pending) = pending {
            pending.push_front(neighbor);
        }
        return Ok(false);
    }
    *key_bytes = next_bytes;
    Ok(true)
}

fn finish_path_batch(
    candidates: Vec<BfsAdjacencyCandidate>,
    exhausted_current: bool,
    limits: BfsCandidateLimits,
    pending_batch: &mut Option<(Option<u32>, usize)>,
    pending_kind: &mut Option<PendingPathBatch>,
    state: &mut ResumablePathState,
) -> GraphResult<ResumablePathMaterialization> {
    let batch = BfsAdjacencyCandidateBatch::try_new(candidates, exhausted_current, limits)?;
    *pending_batch = Some((
        batch.candidates.first().map(|candidate| candidate.sequence),
        batch.candidates.len(),
    ));
    *pending_kind = Some(PendingPathBatch::Adjacency);
    *state = ResumablePathState::NeedVisibility;
    Ok(ResumablePathMaterialization::Batch(batch))
}

fn validate_path_verdicts(
    state: ResumablePathState,
    expected: Option<(Option<u32>, usize)>,
    pending_kind: Option<PendingPathBatch>,
    batch: &BfsAdjacencyCandidateBatch,
    verdicts: &[BfsAdjacencyVerdict],
) -> GraphResult<PendingPathBatch> {
    if state != ResumablePathState::NeedVisibility {
        return Err(GraphError::Internal(
            "path machine received verdicts outside its visibility state".into(),
        ));
    }
    if verdicts.len() != batch.candidates.len()
        || expected
            != Some((
                batch.candidates.first().map(|candidate| candidate.sequence),
                batch.candidates.len(),
            ))
        || batch
            .candidates
            .iter()
            .zip(verdicts)
            .any(|(candidate, verdict)| candidate.sequence != verdict.sequence)
    {
        return Err(GraphError::Internal(
            "path machine received mismatched visibility verdicts".into(),
        ));
    }
    pending_kind.ok_or_else(|| GraphError::Internal("path batch kind is missing".into()))
}

fn reset_single_active(machine: &mut ResumableSingleDirectionBfs) {
    machine.active_node = None;
    machine.adjacency_cursor = OwnedNeighborCursor::default();
    machine.pending_adjacency.clear();
    machine.pending_adjacency_exhausts_node = false;
}

fn reset_bidirectional_active(machine: &mut ResumableBidirectionalBfs) {
    machine.active_node = None;
    machine.adjacency_cursor = OwnedNeighborCursor::default();
    machine.pending_adjacency.clear();
    machine.pending_adjacency_exhausts_node = false;
}

fn side_state(machine: &ResumableBidirectionalBfs, side: SearchSide) -> &PathSideState {
    match side {
        SearchSide::Forward => &machine.forward,
        SearchSide::Backward => &machine.backward,
    }
}

fn side_state_mut(machine: &mut ResumableBidirectionalBfs, side: SearchSide) -> &mut PathSideState {
    match side {
        SearchSide::Forward => &mut machine.forward,
        SearchSide::Backward => &mut machine.backward,
    }
}

fn split_side_states(
    machine: &mut ResumableBidirectionalBfs,
    side: SearchSide,
) -> (&mut PathSideState, &PathSideState) {
    match side {
        SearchSide::Forward => (&mut machine.forward, &machine.backward),
        SearchSide::Backward => (&mut machine.backward, &machine.forward),
    }
}

fn require_path_epoch(
    expected: Option<BfsProjectionEpoch>,
    actual: BfsProjectionEpoch,
) -> GraphResult<()> {
    if expected == Some(actual) {
        Ok(())
    } else {
        Err(GraphError::Internal(
            "projection changed while path visibility was being resolved; retry the graph query"
                .into(),
        ))
    }
}

fn next_path_sequence(sequence: u32) -> GraphResult<u32> {
    sequence
        .checked_add(1)
        .ok_or_else(|| GraphError::Internal("path visibility sequence overflow".into()))
}

fn consume_path_work_without_interrupt(governor: &ResourceGovernor) -> GraphResult<()> {
    governor
        .consume_work(ResourcePhase::QueryPaths, WorkUnits::new(1))
        .map_err(crate::safety::resource_limit_error)
}

fn path_allocation_error(_error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::QUERY_MEMORY_MB.get().max(1) as u64,
    }
}

fn path_from_single_parents(
    node_store: &NodeStore,
    source: u32,
    target: u32,
    parent: &HashMap<u32, ParentStep>,
    edge_type_label: impl Fn(EdgeTypeId) -> Option<String>,
) -> GraphResult<Vec<PathStep>> {
    let mut path = Vec::new();
    let mut current = target;
    loop {
        let step = parent
            .get(&current)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("path parent is missing for node {current}"),
            })?;
        path.push((current, step.edge_type));
        if current == source {
            break;
        }
        current = step.parent;
    }
    path.reverse();
    path_steps(node_store, &path, edge_type_label)
}

fn path_from_bidirectional_parents(
    node_store: &NodeStore,
    source: u32,
    target: u32,
    meeting_node: Option<u32>,
    forward: &HashMap<u32, ParentStep>,
    backward: &HashMap<u32, ParentStep>,
    edge_type_label: impl Fn(EdgeTypeId) -> Option<String>,
) -> GraphResult<Vec<PathStep>> {
    let meet =
        meeting_node.ok_or_else(|| GraphError::Internal("path has no meeting node".into()))?;
    let mut forward_path = Vec::new();
    let mut current = meet;
    while current != source {
        let step = forward
            .get(&current)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("forward path parent is missing for node {current}"),
            })?;
        forward_path.push((current, step.edge_type));
        current = step.parent;
    }
    forward_path.push((source, EdgeTypeId::UNTYPED));
    forward_path.reverse();

    let mut backward_path = Vec::new();
    let mut child = meet;
    current = backward
        .get(&meet)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: format!("backward path parent is missing for node {meet}"),
        })?
        .parent;
    if current != meet {
        loop {
            let step = backward
                .get(&child)
                .ok_or_else(|| GraphError::CorruptFile {
                    reason: format!("backward path edge is missing for node {child}"),
                })?;
            backward_path.push((current, step.edge_type));
            if current == target {
                break;
            }
            child = current;
            current = backward
                .get(&current)
                .ok_or_else(|| GraphError::CorruptFile {
                    reason: format!("backward path parent is missing for node {current}"),
                })?
                .parent;
        }
    }
    forward_path.extend(backward_path);
    path_steps(node_store, &forward_path, edge_type_label)
}

fn path_steps(
    node_store: &NodeStore,
    path: &[(u32, EdgeTypeId)],
    edge_type_label: impl Fn(EdgeTypeId) -> Option<String>,
) -> GraphResult<Vec<PathStep>> {
    path.iter()
        .enumerate()
        .map(|(index, &(node, edge_type))| {
            let table_oid = node_store
                .table_oid(node)
                .ok_or_else(|| GraphError::CorruptFile {
                    reason: format!("path node {node} has no table identity"),
                })?;
            let node_id = node_store
                .primary_key(node)
                .ok_or_else(|| GraphError::CorruptFile {
                    reason: format!("path node {node} has no source identity"),
                })?
                .to_owned();
            Ok(PathStep {
                step: i32::try_from(index).unwrap_or(i32::MAX),
                node_table: TableOid(table_oid),
                node_id,
                edge_label: (index != 0).then(|| {
                    edge_type_label(edge_type).unwrap_or_else(|| format!("type_{edge_type}"))
                }),
            })
        })
        .collect()
}

#[cfg(any(test, feature = "benchmarks"))]
pub(crate) fn weighted_shortest_path_with_neighbors_for_benchmark(
    node_store: &NodeStore,
    neighbors: &impl WeightedNeighborSource,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
    proof: &crate::bench_support::BenchmarkVisibilityProof,
) -> Option<Vec<WeightedPathStep>> {
    let coordinator = VisibilityCoordinator::unrestricted_for_benchmark(proof);
    weighted_shortest_path_with_neighbors_inner(
        node_store,
        neighbors,
        source,
        target,
        edge_type_registry,
        None,
        coordinator.scope_for_benchmark(proof),
        None,
    )
}

#[derive(Debug, Clone, Copy)]
struct WeightedParentStep {
    parent: u32,
    edge_type: EdgeTypeId,
    edge_weight: u32,
}

/// Owned Dijkstra state that yields bounded adjacency candidates for caller
/// visibility resolution without retaining a projection borrow.
#[derive(Debug)]
pub(crate) struct ResumableDijkstra {
    source: u32,
    target: u32,
    dist: Vec<u64>,
    parent: HashMap<u32, WeightedParentStep>,
    heap: BinaryHeap<Reverse<(u64, u32)>>,
    active: Option<(u64, u32)>,
    adjacency_cursor: OwnedNeighborCursor,
    pending_adjacency: VecDeque<WeightedNeighbor>,
    pending_adjacency_exhausts_node: bool,
    endpoint_cursor: usize,
    source_visible: Option<bool>,
    target_visible: Option<bool>,
    state: ResumablePathState,
    pending_kind: Option<PendingPathBatch>,
    pending_batch: Option<(Option<u32>, usize)>,
    pending_weights: Vec<u32>,
    next_sequence: u32,
    projection_epoch: Option<BfsProjectionEpoch>,
}

impl ResumableDijkstra {
    pub(crate) fn try_new(node_count: usize, source: u32, target: u32) -> GraphResult<Self> {
        if source as usize >= node_count || target as usize >= node_count {
            return Err(GraphError::Internal(
                "resumable weighted path received an invalid endpoint index".into(),
            ));
        }
        let mut dist = Vec::new();
        dist.try_reserve_exact(node_count)
            .map_err(path_allocation_error)?;
        dist.resize(node_count, u64::MAX);
        dist[source as usize] = 0;
        let mut parent = HashMap::new();
        parent.try_reserve(1).map_err(path_allocation_error)?;
        parent.insert(
            source,
            WeightedParentStep {
                parent: source,
                edge_type: EdgeTypeId::UNTYPED,
                edge_weight: 0,
            },
        );
        let mut heap = BinaryHeap::new();
        heap.try_reserve(1).map_err(path_allocation_error)?;
        heap.push(Reverse((0, source)));
        Ok(Self {
            source,
            target,
            dist,
            parent,
            heap,
            active: None,
            adjacency_cursor: OwnedNeighborCursor::default(),
            pending_adjacency: VecDeque::new(),
            pending_adjacency_exhausts_node: false,
            endpoint_cursor: 0,
            source_visible: None,
            target_visible: None,
            state: ResumablePathState::NeedEndpoints,
            pending_kind: None,
            pending_batch: None,
            pending_weights: Vec::new(),
            next_sequence: 0,
            projection_epoch: None,
        })
    }

    pub(crate) fn bind_projection_epoch(&mut self, epoch: BfsProjectionEpoch) {
        self.projection_epoch = Some(epoch);
    }

    pub(crate) fn require_projection_epoch(&self, actual: BfsProjectionEpoch) -> GraphResult<()> {
        require_path_epoch(self.projection_epoch, actual)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.state == ResumablePathState::Complete
    }

    #[cfg(any(test, feature = "benchmarks"))]
    pub(crate) fn finish(
        self,
        node_store: &NodeStore,
        edge_type_registry: &[String],
        governor: &ResourceGovernor,
    ) -> GraphResult<Option<Vec<WeightedPathStep>>> {
        self.finish_with(
            node_store,
            |type_id| edge_type_registry.get(type_id.get() as usize).cloned(),
            governor,
        )
    }

    pub(crate) fn finish_with(
        self,
        node_store: &NodeStore,
        edge_type_label: impl Fn(EdgeTypeId) -> Option<String>,
        governor: &ResourceGovernor,
    ) -> GraphResult<Option<Vec<WeightedPathStep>>> {
        if self.source_visible != Some(true)
            || self.target_visible != Some(true)
            || self.dist[self.target as usize] == u64::MAX
        {
            return Ok(None);
        }
        weighted_path_from_parents(
            node_store,
            self.source,
            self.target,
            &self.dist,
            &self.parent,
            edge_type_label,
            governor,
        )
        .map(Some)
    }
}

/// Find the shortest unweighted path between two nodes using bidirectional BFS.
///
/// Returns `None` if no path exists.
/// Falls back to single-direction BFS if the graph has unidirectional edges.
#[cfg(test)]
pub fn shortest_path(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    source: u32,
    target: u32,
    max_depth: i32,
    has_unidirectional_edges: bool,
    edge_type_registry: &[String],
) -> Option<Vec<PathStep>> {
    let neighbors = CsrNeighbors::new(edge_store);
    shortest_path_with_neighbors(
        node_store,
        &neighbors,
        source,
        target,
        max_depth,
        has_unidirectional_edges,
        edge_type_registry,
    )
}

/// Find the shortest unweighted path over a supplied neighbor source.
#[cfg(test)]
pub(crate) fn shortest_path_with_neighbors(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    source: u32,
    target: u32,
    max_depth: i32,
    has_unidirectional_edges: bool,
    edge_type_registry: &[String],
) -> Option<Vec<PathStep>> {
    shortest_path_with_neighbors_inner(
        node_store,
        neighbors,
        UnweightedPathRequest {
            source,
            target,
            max_depth,
            has_unidirectional_edges,
            edge_type_registry,
            edge_type_label: None,
        },
        None,
        VisibilityCoordinator::unrestricted_for_test_or_benchmark().scope(),
        None,
    )
}

pub(crate) struct UnweightedPathRequest<'a> {
    pub(crate) source: u32,
    pub(crate) target: u32,
    pub(crate) max_depth: i32,
    pub(crate) has_unidirectional_edges: bool,
    pub(crate) edge_type_registry: &'a [String],
    pub(crate) edge_type_label: Option<&'a dyn Fn(EdgeTypeId) -> Option<String>>,
}

fn resolve_edge_type_label(
    registry: &[String],
    resolver: Option<&dyn Fn(EdgeTypeId) -> Option<String>>,
    edge_type: EdgeTypeId,
) -> String {
    resolver
        .and_then(|resolve| resolve(edge_type))
        .or_else(|| registry.get(edge_type.get() as usize).cloned())
        .unwrap_or_else(|| format!("type_{edge_type}"))
}

/// Find an unweighted path while enforcing expansion and elapsed-time limits.
#[cfg(test)]
pub(crate) fn shortest_path_with_neighbors_governed(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    request: UnweightedPathRequest<'_>,
    governor: &ResourceGovernor,
) -> GraphResult<Option<Vec<PathStep>>> {
    let coordinator = VisibilityCoordinator::unrestricted_for_test_or_benchmark();
    let context = coordinator.context(governor);
    shortest_path_with_neighbors_governed_with_context(node_store, neighbors, request, &context)
}

pub(crate) fn shortest_path_with_neighbors_governed_with_context(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    request: UnweightedPathRequest<'_>,
    context: &QueryExecutionContext<'_>,
) -> GraphResult<Option<Vec<PathStep>>> {
    let budget = PathWorkBudget::new(context.governor);
    let result = shortest_path_with_neighbors_inner(
        node_store,
        neighbors,
        request,
        Some(&budget),
        context.visibility,
        context.edge_type_filter,
    );
    budget.finish(result)
}

fn shortest_path_with_neighbors_inner(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    request: UnweightedPathRequest<'_>,
    budget: Option<&PathWorkBudget<'_>>,
    visibility: &VisibilityScope,
    edge_type_filter: Option<&RoaringBitmap>,
) -> Option<Vec<PathStep>> {
    let edge_type_label = request.edge_type_label;
    let admission = PathAdmission {
        budget,
        visibility,
        edge_type_filter,
        edge_type_label,
    };
    let UnweightedPathRequest {
        source,
        target,
        max_depth,
        has_unidirectional_edges,
        edge_type_registry,
        edge_type_label: _,
    } = request;
    if source >= node_store.node_count()
        || target >= node_store.node_count()
        || !visibility.allows_node(source)
        || !visibility.allows_node(target)
    {
        return None;
    }

    if source == target {
        return Some(vec![PathStep {
            step: 0,
            node_table: TableOid(node_store.table_oid(source)?),
            node_id: node_store.primary_key(source)?.to_string(),
            edge_label: None,
        }]);
    }

    if has_unidirectional_edges {
        return single_direction_bfs(
            node_store,
            neighbors,
            source,
            target,
            max_depth,
            edge_type_registry,
            admission,
        );
    }

    bidirectional_bfs(
        node_store,
        neighbors,
        source,
        target,
        max_depth,
        edge_type_registry,
        admission,
    )
}

#[derive(Clone, Copy)]
struct PathAdmission<'a, 'governor> {
    budget: Option<&'a PathWorkBudget<'governor>>,
    visibility: &'a VisibilityScope,
    edge_type_filter: Option<&'a RoaringBitmap>,
    edge_type_label: Option<&'a dyn Fn(EdgeTypeId) -> Option<String>>,
}

fn bidirectional_bfs(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    source: u32,
    target: u32,
    max_depth: i32,
    edge_type_registry: &[String],
    admission: PathAdmission<'_, '_>,
) -> Option<Vec<PathStep>> {
    let mut fwd_visited = RoaringBitmap::new();
    let mut bwd_visited = RoaringBitmap::new();
    let mut fwd_parent = HashMap::new();
    let mut bwd_parent = HashMap::new();
    let mut fwd_frontier: VecDeque<u32> = VecDeque::new();
    let mut bwd_frontier: VecDeque<u32> = VecDeque::new();

    fwd_visited.insert(source);
    bwd_visited.insert(target);
    fwd_parent.insert(
        source,
        ParentStep {
            parent: source,
            edge_type: EdgeTypeId::UNTYPED,
            depth: 0,
        },
    );
    bwd_parent.insert(
        target,
        ParentStep {
            parent: target,
            edge_type: EdgeTypeId::UNTYPED,
            depth: 0,
        },
    );
    fwd_frontier.push_back(source);
    bwd_frontier.push_back(target);

    // (meeting node, combined distance) for the best candidate found in the
    // level currently being scanned. Multiple candidates can appear in one
    // level with different combined distances, since the opposite side's
    // visited depth for each candidate is whatever it was when that node
    // was first discovered — not necessarily the smallest one available.
    // The whole level must be scanned before picking a winner; stopping at
    // the first candidate found can accept a longer path than the shortest
    // one available in the same level.
    let mut meeting_node: Option<u32> = None;
    let mut best_combined = i32::MAX;
    let mut depth = 0;

    while !fwd_frontier.is_empty() && !bwd_frontier.is_empty() && depth < max_depth {
        // Expand the smaller frontier
        if fwd_frontier.len() <= bwd_frontier.len() {
            let level_size = fwd_frontier.len();
            let new_depth = depth + 1;
            for _ in 0..level_size {
                let Some(current) = fwd_frontier.pop_front() else {
                    break;
                };
                for edge in neighbors.neighbors(current) {
                    if !consume_path_work(admission.budget) {
                        return None;
                    }
                    if !path_candidate_visible(admission, edge)
                        || !node_store.is_active(edge.target)
                        || crate::projection::tx_delta::node_deleted(edge.target)
                    {
                        continue;
                    }
                    if !fwd_visited.contains(edge.target) {
                        fwd_visited.insert(edge.target);
                        fwd_parent.insert(
                            edge.target,
                            ParentStep {
                                parent: current,
                                edge_type: edge.type_id,
                                depth: new_depth,
                            },
                        );
                        fwd_frontier.push_back(edge.target);
                    }
                    if bwd_visited.contains(edge.target) {
                        fwd_parent.entry(edge.target).or_insert(ParentStep {
                            parent: current,
                            edge_type: edge.type_id,
                            depth: new_depth,
                        });
                        let fwd_depth = fwd_parent[&edge.target].depth;
                        let bwd_depth = bwd_parent[&edge.target].depth;
                        let combined = fwd_depth + bwd_depth;
                        if combined < best_combined {
                            best_combined = combined;
                            meeting_node = Some(edge.target);
                        }
                    }
                }
            }
        } else {
            let level_size = bwd_frontier.len();
            let new_depth = depth + 1;
            for _ in 0..level_size {
                let Some(current) = bwd_frontier.pop_front() else {
                    break;
                };
                for edge in neighbors.neighbors(current) {
                    if !consume_path_work(admission.budget) {
                        return None;
                    }
                    if !path_candidate_visible(admission, edge)
                        || !node_store.is_active(edge.target)
                        || crate::projection::tx_delta::node_deleted(edge.target)
                    {
                        continue;
                    }
                    if !bwd_visited.contains(edge.target) {
                        bwd_visited.insert(edge.target);
                        bwd_parent.insert(
                            edge.target,
                            ParentStep {
                                parent: current,
                                edge_type: edge.type_id,
                                depth: new_depth,
                            },
                        );
                        bwd_frontier.push_back(edge.target);
                    }
                    if fwd_visited.contains(edge.target) {
                        bwd_parent.entry(edge.target).or_insert(ParentStep {
                            parent: current,
                            edge_type: edge.type_id,
                            depth: new_depth,
                        });
                        let fwd_depth = fwd_parent[&edge.target].depth;
                        let bwd_depth = bwd_parent[&edge.target].depth;
                        let combined = fwd_depth + bwd_depth;
                        if combined < best_combined {
                            best_combined = combined;
                            meeting_node = Some(edge.target);
                        }
                    }
                }
            }
        }

        if meeting_node.is_some() {
            break;
        }
        depth += 1;
    }

    let meet = meeting_node?;

    // Reconstruct path: source → meet → target
    let mut fwd_path = Vec::new();
    let mut current = meet;
    while current != source {
        let step = fwd_parent.get(&current)?;
        fwd_path.push((current, step.edge_type));
        current = step.parent;
    }
    fwd_path.push((source, EdgeTypeId::UNTYPED));
    fwd_path.reverse();

    let mut bwd_path = Vec::new();
    let mut child = meet;
    current = bwd_parent.get(&meet)?.parent;
    if current != meet {
        loop {
            let step = bwd_parent.get(&child)?;
            bwd_path.push((current, step.edge_type));
            if current == target {
                break;
            }
            child = current;
            current = bwd_parent.get(&current)?.parent;
        }
    }

    // Combine into PathStep sequence
    let mut steps = Vec::new();
    for (i, &(node, edge_type)) in fwd_path.iter().enumerate() {
        steps.push(PathStep {
            step: i as i32,
            node_table: TableOid(node_store.table_oid(node)?),
            node_id: node_store.primary_key(node)?.to_string(),
            edge_label: if i == 0 {
                None
            } else {
                Some(resolve_edge_type_label(
                    edge_type_registry,
                    admission.edge_type_label,
                    edge_type,
                ))
            },
        });
    }
    let offset = fwd_path.len();
    for (i, &(node, edge_type)) in bwd_path.iter().enumerate() {
        steps.push(PathStep {
            step: (offset + i) as i32,
            node_table: TableOid(node_store.table_oid(node)?),
            node_id: node_store.primary_key(node)?.to_string(),
            edge_label: Some(resolve_edge_type_label(
                edge_type_registry,
                admission.edge_type_label,
                edge_type,
            )),
        });
    }

    Some(steps)
}

fn single_direction_bfs(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    source: u32,
    target: u32,
    max_depth: i32,
    edge_type_registry: &[String],
    admission: PathAdmission<'_, '_>,
) -> Option<Vec<PathStep>> {
    let mut visited = RoaringBitmap::new();
    let mut parent = HashMap::new();
    let mut frontier: VecDeque<(u32, i32)> = VecDeque::new();

    visited.insert(source);
    parent.insert(
        source,
        ParentStep {
            parent: source,
            edge_type: EdgeTypeId::UNTYPED,
            depth: 0,
        },
    );
    frontier.push_back((source, 0));

    while let Some((current, current_depth)) = frontier.pop_front() {
        if current_depth >= max_depth {
            continue;
        }

        for edge in neighbors.neighbors(current) {
            if !consume_path_work(admission.budget) {
                return None;
            }
            if !path_candidate_visible(admission, edge)
                || visited.contains(edge.target)
                || !node_store.is_active(edge.target)
                || crate::projection::tx_delta::node_deleted(edge.target)
            {
                continue;
            }

            visited.insert(edge.target);
            parent.insert(
                edge.target,
                ParentStep {
                    parent: current,
                    edge_type: edge.type_id,
                    depth: current_depth + 1,
                },
            );
            frontier.push_back((edge.target, current_depth + 1));

            if edge.target == target {
                // Found target — reconstruct path
                let mut path = Vec::new();
                let mut cur = target;
                loop {
                    let step = parent.get(&cur)?;
                    path.push((cur, step.edge_type));
                    if cur == source {
                        break;
                    }
                    cur = step.parent;
                }
                path.reverse();

                return path
                    .iter()
                    .enumerate()
                    .map(|(i, &(node, et))| {
                        Some(PathStep {
                            step: i as i32,
                            node_table: TableOid(node_store.table_oid(node)?),
                            node_id: node_store.primary_key(node)?.to_string(),
                            edge_label: if i == 0 {
                                None
                            } else {
                                Some(resolve_edge_type_label(
                                    edge_type_registry,
                                    admission.edge_type_label,
                                    et,
                                ))
                            },
                        })
                    })
                    .collect();
            }
        }
    }

    None // No path found
}

#[allow(
    clippy::too_many_arguments,
    reason = "weighted paging keeps search state, identities, filtering, and resource bounds explicit"
)]
pub(crate) fn materialize_resumable_dijkstra_batch(
    machine: &mut ResumableDijkstra,
    node_store: &NodeStore,
    neighbors: &(impl WeightedNeighborSource + ?Sized),
    relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
    edge_type_filter: Option<&RoaringBitmap>,
    limits: BfsCandidateLimits,
    governor: &ResourceGovernor,
) -> GraphResult<ResumablePathMaterialization> {
    validate_path_materialization(machine.state, limits)?;
    let _page_lease = reserve_path_page_workspace(governor, limits)?;
    if machine.state == ResumablePathState::NeedEndpoints {
        return materialize_endpoint_batch(
            machine.source,
            machine.target,
            &mut machine.endpoint_cursor,
            &mut machine.next_sequence,
            &mut machine.pending_batch,
            &mut machine.pending_kind,
            &mut machine.state,
            node_store,
            limits,
        );
    }

    let mut progress_steps = 0usize;
    loop {
        let (cost, current) = if let Some(active) = machine.active {
            active
        } else {
            let Some(Reverse((cost, current))) = machine.heap.pop() else {
                machine.state = ResumablePathState::Complete;
                return Ok(ResumablePathMaterialization::Complete);
            };
            progress_steps = progress_steps.saturating_add(1);
            if cost > machine.dist[current as usize] {
                if progress_steps >= limits.max_candidates {
                    return Ok(ResumablePathMaterialization::Progress);
                }
                continue;
            }
            if current == machine.target {
                machine.state = ResumablePathState::Complete;
                return Ok(ResumablePathMaterialization::Complete);
            }
            machine.active = Some((cost, current));
            machine.adjacency_cursor = OwnedNeighborCursor::default();
            (cost, current)
        };
        if machine.pending_adjacency.is_empty() {
            let mut page = Vec::new();
            page.try_reserve(limits.max_candidates)
                .map_err(path_allocation_error)?;
            machine.pending_adjacency_exhausts_node = neighbors.fill_weighted_neighbors(
                current,
                &mut machine.adjacency_cursor,
                limits.max_candidates,
                &mut page,
            );
            machine
                .pending_adjacency
                .try_reserve(page.len())
                .map_err(path_allocation_error)?;
            machine.pending_adjacency.extend(page);
        }
        if machine.pending_adjacency.is_empty() {
            if machine.pending_adjacency_exhausts_node {
                machine.active = None;
            }
            return Ok(ResumablePathMaterialization::Progress);
        }
        if machine.pending_weights.capacity() < machine.pending_adjacency.len() {
            machine
                .pending_weights
                .try_reserve(machine.pending_adjacency.len() - machine.pending_weights.capacity())
                .map_err(path_allocation_error)?;
        }
        machine.pending_weights.clear();
        let mut candidates = path_candidate_vec(limits)?;
        let mut key_bytes = 0usize;
        while let Some(weighted_neighbor) = machine.pending_adjacency.front().copied() {
            let neighbor = Neighbor {
                target: weighted_neighbor.target,
                type_id: weighted_neighbor.type_id,
                schema_reversed: weighted_neighbor.schema_reversed,
                relationship_id: weighted_neighbor.relationship_id,
            };
            if edge_type_filter.is_some_and(|allowed| !allowed.contains(neighbor.type_id.get())) {
                consume_path_work_without_interrupt(governor)?;
                machine.pending_adjacency.pop_front();
                continue;
            }
            if !path_candidate_fits(
                &candidates,
                &mut key_bytes,
                neighbor,
                None,
                limits,
                node_store,
                relationships,
            )? {
                break;
            }
            let candidate = path_candidate(
                machine.next_sequence,
                current,
                0,
                neighbor,
                node_store,
                relationships,
            )?;
            candidates.push(candidate);
            machine.pending_weights.push(weighted_neighbor.weight);
            consume_path_work_without_interrupt(governor)?;
            machine.next_sequence = next_path_sequence(machine.next_sequence)?;
            machine.pending_adjacency.pop_front();
        }
        let exhausted =
            machine.pending_adjacency.is_empty() && machine.pending_adjacency_exhausts_node;
        if exhausted {
            machine.active = None;
        }
        if candidates.is_empty() {
            return Ok(ResumablePathMaterialization::Progress);
        }
        let _ = cost;
        return finish_path_batch(
            candidates,
            exhausted,
            limits,
            &mut machine.pending_batch,
            &mut machine.pending_kind,
            &mut machine.state,
        );
    }
}

pub(crate) fn apply_resumable_dijkstra_verdicts(
    machine: &mut ResumableDijkstra,
    batch: &BfsAdjacencyCandidateBatch,
    verdicts: &[BfsAdjacencyVerdict],
    relationships: &crate::relationship_identity_store::RelationshipIdentityStore,
    node_store: &NodeStore,
    governor: &ResourceGovernor,
) -> GraphResult<()> {
    let pending_kind = validate_path_verdicts(
        machine.state,
        machine.pending_batch,
        machine.pending_kind,
        batch,
        verdicts,
    )?;
    machine.pending_kind = None;
    match pending_kind {
        PendingPathBatch::Endpoints => {
            apply_endpoint_verdicts(
                machine.source,
                machine.target,
                batch,
                verdicts,
                &mut machine.source_visible,
                &mut machine.target_visible,
            );
            if machine.endpoint_cursor < endpoint_count(machine.source, machine.target) {
                machine.state = ResumablePathState::NeedEndpoints;
            } else if machine.source_visible == Some(false) || machine.target_visible == Some(false)
            {
                machine.state = ResumablePathState::Complete;
            } else if machine.source_visible == Some(true) && machine.target_visible == Some(true) {
                machine.state = ResumablePathState::NeedCandidates;
            }
        }
        PendingPathBatch::Adjacency => {
            let (cost, current) = machine
                .active
                .or_else(|| {
                    batch.candidates.first().map(|candidate| {
                        (
                            machine.dist[candidate.parent_node as usize],
                            candidate.parent_node,
                        )
                    })
                })
                .ok_or_else(|| {
                    GraphError::Internal("weighted batch lost its active heap node".into())
                })?;
            if machine.pending_weights.len() != batch.candidates.len() {
                return Err(GraphError::Internal(
                    "weighted batch lost its relaxation metadata".into(),
                ));
            }
            for ((candidate, verdict), &weight) in batch
                .candidates
                .iter()
                .zip(verdicts)
                .zip(&machine.pending_weights)
            {
                if !verdict.visible()
                    || !node_store.is_active(candidate.target_node)
                    || crate::projection::tx_delta::node_deleted(candidate.target_node)
                {
                    continue;
                }
                if candidate.relationship_id.is_some_and(|relationship_id| {
                    path_relationship_identity(
                        relationships,
                        relationship_id,
                        |mapping_id, source_key| {
                            candidate.relationship_mapping_id == Some(mapping_id)
                                && candidate.relationship_source_key.as_deref() == Some(source_key)
                        },
                    ) != Some(true)
                }) {
                    return Err(GraphError::Internal(
                        "weighted candidate identity changed across visibility resolution".into(),
                    ));
                }
                let Some(new_cost) = cost.checked_add(u64::from(weight)) else {
                    continue;
                };
                if new_cost < machine.dist[candidate.target_node as usize] {
                    reserve_weighted_state_growth(
                        governor,
                        &mut machine.parent,
                        &mut machine.heap,
                    )?;
                    machine.dist[candidate.target_node as usize] = new_cost;
                    machine.parent.insert(
                        candidate.target_node,
                        WeightedParentStep {
                            parent: current,
                            edge_type: candidate.edge_type,
                            edge_weight: weight,
                        },
                    );
                    machine
                        .heap
                        .push(Reverse((new_cost, candidate.target_node)));
                }
            }
            machine.pending_weights.clear();
            machine.state = ResumablePathState::NeedCandidates;
        }
    }
    machine.pending_batch = None;
    Ok(())
}

fn reserve_weighted_state_growth(
    governor: &ResourceGovernor,
    parent: &mut HashMap<u32, WeightedParentStep>,
    heap: &mut BinaryHeap<Reverse<(u64, u32)>>,
) -> GraphResult<()> {
    governor
        .reserve_memory(
            ResourcePhase::QueryPaths,
            crate::resource::ByteCount::from_bytes(PATH_STATE_BYTES_PER_VISIT as u64),
        )
        .map_err(crate::safety::resource_limit_error)?
        .retain_until_governor_drop();
    parent.try_reserve(1).map_err(path_allocation_error)?;
    heap.try_reserve(1).map_err(path_allocation_error)?;
    Ok(())
}

fn weighted_path_from_parents(
    node_store: &NodeStore,
    source: u32,
    target: u32,
    dist: &[u64],
    parent: &HashMap<u32, WeightedParentStep>,
    edge_type_label: impl Fn(EdgeTypeId) -> Option<String>,
    governor: &ResourceGovernor,
) -> GraphResult<Vec<WeightedPathStep>> {
    let total_cost = dist[target as usize];
    let mut node_count = 0usize;
    let mut string_bytes = 0usize;
    let mut current = target;
    loop {
        node_count = node_count
            .checked_add(1)
            .ok_or_else(|| GraphError::Internal("weighted path length overflowed".into()))?;
        let node_key = node_store
            .primary_key(current)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("weighted path node {current} has no source identity"),
            })?;
        string_bytes = string_bytes
            .checked_add(node_key.len())
            .ok_or_else(|| GraphError::Internal("weighted path output size overflowed".into()))?;
        if current != source {
            let edge_type = parent
                .get(&current)
                .map(|parent| parent.edge_type)
                .ok_or_else(|| {
                    GraphError::Internal("weighted path parent chain is incomplete".into())
                })?;
            string_bytes = string_bytes
                .checked_add(
                    edge_type_label(edge_type)
                        .as_ref()
                        .map_or_else(|| "type_255".len(), String::len),
                )
                .ok_or_else(|| {
                    GraphError::Internal("weighted path output size overflowed".into())
                })?;
        }
        if current == source {
            break;
        }
        current = parent
            .get(&current)
            .ok_or_else(|| GraphError::Internal("weighted path parent chain is incomplete".into()))?
            .parent;
    }
    let output_bytes = node_count
        .checked_mul(std::mem::size_of::<u32>() + std::mem::size_of::<WeightedPathStep>())
        .and_then(|bytes| bytes.checked_add(string_bytes))
        .ok_or_else(|| GraphError::Internal("weighted path output size overflowed".into()))?;
    governor
        .reserve_memory(
            ResourcePhase::QueryPaths,
            crate::resource::ByteCount::from_usize(output_bytes).ok_or_else(|| {
                GraphError::Internal("weighted path output size does not fit u64".into())
            })?,
        )
        .map_err(crate::safety::resource_limit_error)?
        .retain_until_governor_drop();
    let mut nodes = Vec::new();
    nodes
        .try_reserve(node_count)
        .map_err(path_allocation_error)?;
    current = target;
    loop {
        nodes.push(current);
        if current == source {
            break;
        }
        current = parent
            .get(&current)
            .ok_or_else(|| GraphError::Internal("weighted path parent chain is incomplete".into()))?
            .parent;
    }
    nodes.reverse();
    let mut output = Vec::new();
    output
        .try_reserve(nodes.len())
        .map_err(path_allocation_error)?;
    for (step, node) in nodes.into_iter().enumerate() {
        let parent_step = parent.get(&node).copied().unwrap_or(WeightedParentStep {
            parent: node,
            edge_type: EdgeTypeId::UNTYPED,
            edge_weight: 0,
        });
        output.push(WeightedPathStep {
            step: i32::try_from(step)
                .map_err(|_| GraphError::Internal("weighted path step count exceeds i32".into()))?,
            node_table: TableOid(node_store.table_oid(node).ok_or_else(|| {
                GraphError::CorruptFile {
                    reason: format!("weighted path node {node} has no table identity"),
                }
            })?),
            node_id: node_store
                .primary_key(node)
                .ok_or_else(|| GraphError::CorruptFile {
                    reason: format!("weighted path node {node} has no source identity"),
                })?
                .to_owned(),
            edge_label: (step != 0).then(|| {
                edge_type_label(parent_step.edge_type)
                    .unwrap_or_else(|| format!("type_{}", parent_step.edge_type))
            }),
            edge_weight: (step != 0).then_some(parent_step.edge_weight),
            step_cost: dist[node as usize],
            total_cost,
        });
    }
    Ok(output)
}

/// Dijkstra's algorithm for weighted shortest path.
///
/// Uses `BinaryHeap<Reverse<(cost, node)>>` for O((V + E) log V).
#[cfg(test)]
pub fn weighted_shortest_path(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
) -> Option<Vec<WeightedPathStep>> {
    weighted_shortest_path_with_neighbors(
        node_store,
        edge_store,
        source,
        target,
        edge_type_registry,
    )
}

/// Dijkstra's algorithm over a supplied weighted neighbor source.
#[cfg(test)]
pub(crate) fn weighted_shortest_path_with_neighbors(
    node_store: &NodeStore,
    neighbors: &impl WeightedNeighborSource,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
) -> Option<Vec<WeightedPathStep>> {
    weighted_shortest_path_with_neighbors_inner(
        node_store,
        neighbors,
        source,
        target,
        edge_type_registry,
        None,
        VisibilityCoordinator::unrestricted_for_test_or_benchmark().scope(),
        None,
    )
}

/// Run Dijkstra while enforcing expansion and elapsed-time limits.
#[cfg(test)]
#[allow(dead_code, reason = "legacy test compatibility entry point")]
pub(crate) fn weighted_shortest_path_with_neighbors_governed(
    node_store: &NodeStore,
    neighbors: &impl WeightedNeighborSource,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
    governor: &ResourceGovernor,
) -> GraphResult<Option<Vec<WeightedPathStep>>> {
    let coordinator = VisibilityCoordinator::unrestricted_for_test_or_benchmark();
    let context = coordinator.context(governor);
    weighted_shortest_path_with_neighbors_governed_with_context(
        node_store,
        neighbors,
        source,
        target,
        edge_type_registry,
        &context,
    )
}

pub(crate) fn weighted_shortest_path_with_neighbors_governed_with_context(
    node_store: &NodeStore,
    neighbors: &impl WeightedNeighborSource,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
    context: &QueryExecutionContext<'_>,
) -> GraphResult<Option<Vec<WeightedPathStep>>> {
    let budget = PathWorkBudget::new(context.governor);
    let result = weighted_shortest_path_with_neighbors_inner(
        node_store,
        neighbors,
        source,
        target,
        edge_type_registry,
        Some(&budget),
        context.visibility,
        context.edge_type_filter,
    );
    budget.finish(result)
}

#[allow(
    clippy::too_many_arguments,
    reason = "weighted path execution keeps stores, coordinates, admission, and output labels explicit"
)]
fn weighted_shortest_path_with_neighbors_inner(
    node_store: &NodeStore,
    neighbors: &impl WeightedNeighborSource,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
    budget: Option<&PathWorkBudget<'_>>,
    visibility: &VisibilityScope,
    edge_type_filter: Option<&RoaringBitmap>,
) -> Option<Vec<WeightedPathStep>> {
    if source >= node_store.node_count()
        || target >= node_store.node_count()
        || !visibility.allows_node(source)
        || !visibility.allows_node(target)
    {
        return None;
    }

    if !neighbors.has_weighted_edges() {
        return None;
    }

    let node_count = node_store.node_count() as usize;
    let mut dist = vec![u64::MAX; node_count];
    let mut parent = HashMap::new();
    let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();

    dist[source as usize] = 0;
    parent.insert(
        source,
        WeightedParentStep {
            parent: source,
            edge_type: EdgeTypeId::UNTYPED,
            edge_weight: 0,
        },
    );
    heap.push(Reverse((0, source)));

    while let Some(Reverse((cost, current))) = heap.pop() {
        if current == target {
            break;
        }
        if cost > dist[current as usize] {
            continue; // Stale entry
        }

        for edge in neighbors.weighted_neighbors(current) {
            if !consume_path_work(budget) {
                return None;
            }
            let neighbor = edge.target;
            if !path_weighted_candidate_visible(budget, visibility, edge)
                || edge_type_filter.is_some_and(|filter| !filter.contains(edge.type_id.get()))
            {
                continue;
            }
            let edge_weight = edge.weight;
            let edge_cost = u64::from(edge_weight);
            let Some(new_cost) = cost.checked_add(edge_cost) else {
                continue;
            };

            if new_cost < dist[neighbor as usize]
                && node_store.is_active(neighbor)
                && !crate::projection::tx_delta::node_deleted(neighbor)
            {
                dist[neighbor as usize] = new_cost;
                parent.insert(
                    neighbor,
                    WeightedParentStep {
                        parent: current,
                        edge_type: edge.type_id,
                        edge_weight,
                    },
                );
                heap.push(Reverse((new_cost, neighbor)));
            }
        }
    }

    if dist[target as usize] == u64::MAX {
        return None;
    }

    let total_cost = dist[target as usize];
    let mut nodes = Vec::new();
    let mut current = target;
    loop {
        nodes.push(current);
        if current == source {
            break;
        }
        current = parent.get(&current)?.parent;
    }
    nodes.reverse();

    nodes
        .into_iter()
        .enumerate()
        .map(|(step, node)| {
            let parent_step = parent.get(&node).copied().unwrap_or(WeightedParentStep {
                parent: node,
                edge_type: EdgeTypeId::UNTYPED,
                edge_weight: 0,
            });
            Some(WeightedPathStep {
                step: step as i32,
                node_table: TableOid(node_store.table_oid(node)?),
                node_id: node_store.primary_key(node)?.to_string(),
                edge_label: if step == 0 {
                    None
                } else {
                    Some(
                        edge_type_registry
                            .get(parent_step.edge_type.get() as usize)
                            .cloned()
                            .unwrap_or_else(|| format!("type_{}", parent_step.edge_type)),
                    )
                },
                edge_weight: (step != 0).then_some(parent_step.edge_weight),
                step_cost: dist[node as usize],
                total_cost,
            })
        })
        .collect()
}

struct PathWorkBudget<'a> {
    governor: &'a ResourceGovernor,
    error: std::cell::RefCell<Option<crate::safety::GraphError>>,
}

impl<'a> PathWorkBudget<'a> {
    fn new(governor: &'a ResourceGovernor) -> Self {
        Self {
            governor,
            error: std::cell::RefCell::new(None),
        }
    }

    fn finish<T>(&self, result: Option<T>) -> GraphResult<Option<T>> {
        match self.error.borrow_mut().take() {
            Some(error) => Err(error),
            None => Ok(result),
        }
    }

    fn record_error(&self, error: crate::safety::GraphError) {
        let mut slot = self.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(error);
        }
    }
}

fn path_candidate_visible(
    admission: PathAdmission<'_, '_>,
    edge: crate::projection::neighbors::Neighbor,
) -> bool {
    match admission
        .visibility
        .allows_relationship(edge.type_id, edge.relationship_id)
    {
        Ok(allowed) => {
            allowed
                && admission.visibility.allows_node(edge.target)
                && admission
                    .edge_type_filter
                    .is_none_or(|filter| filter.contains(edge.type_id.get()))
        }
        Err(error) => {
            if let Some(budget) = admission.budget {
                budget.record_error(error);
            }
            false
        }
    }
}

fn path_weighted_candidate_visible(
    budget: Option<&PathWorkBudget<'_>>,
    visibility: &VisibilityScope,
    edge: crate::projection::neighbors::WeightedNeighbor,
) -> bool {
    match visibility.allows_relationship(edge.type_id, edge.relationship_id) {
        Ok(allowed) => allowed && visibility.allows_node(edge.target),
        Err(error) => {
            if let Some(budget) = budget {
                budget.record_error(error);
            }
            false
        }
    }
}

fn consume_path_work(budget: Option<&PathWorkBudget<'_>>) -> bool {
    let Some(budget) = budget else {
        return true;
    };
    if let Err(error) = budget
        .governor
        .consume_work(ResourcePhase::QueryPaths, WorkUnits::new(1))
    {
        budget.record_error(crate::safety::resource_limit_error(error));
        return false;
    }
    if budget.governor.work_used().as_u64().is_multiple_of(1_024) {
        crate::resource::check_postgres_interrupts();
        if let Err(error) = budget.governor.check_elapsed(ResourcePhase::QueryPaths) {
            budget.record_error(crate::safety::resource_limit_error(error));
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    //! Covers unweighted and weighted path-finding behavior, including directed
    //! edge semantics and unreachable-node invariants.

    use super::*;
    use crate::edge_store::{
        IdentifiedRawEdge, RawEdge, RelationshipIdentity, SortedEdgeStoreBuilder,
    };
    use crate::resource::{
        ByteCount, DiskBudget, ElapsedBudget, MemoryBudget, ResourceLimits, RowCount, WorkUnits,
    };
    use std::time::Duration;

    fn path_governor(work_limit: u64) -> ResourceGovernor {
        ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(1_024 * 1_024)),
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::new(work_limit),
            ElapsedBudget::new(Duration::from_secs(1)),
        ))
    }

    fn identified_store(
        node_count: u32,
        has_weights: bool,
        edges: Vec<(RawEdge, u32)>,
    ) -> EdgeStore {
        let mut edges = edges;
        edges.sort_unstable_by_key(|(edge, _)| {
            (edge.source, edge.target, edge.type_id, edge.schema_reversed)
        });
        let mut builder = SortedEdgeStoreBuilder::new(node_count, has_weights);
        for (edge, relationship_id) in edges {
            builder
                .try_push_identified(IdentifiedRawEdge {
                    edge,
                    relationship_id,
                })
                .unwrap();
        }
        builder.finish()
    }

    fn candidate_limits(page_size: usize) -> BfsCandidateLimits {
        BfsCandidateLimits {
            max_candidates: page_size,
            max_key_bytes: 16 * 1_024,
        }
    }

    fn candidate_verdicts(
        batch: &BfsAdjacencyCandidateBatch,
        hidden_nodes: &RoaringBitmap,
        hidden_relationships: &RoaringBitmap,
    ) -> Vec<BfsAdjacencyVerdict> {
        batch
            .candidates
            .iter()
            .map(|candidate| BfsAdjacencyVerdict {
                sequence: candidate.sequence,
                node_visible: !hidden_nodes.contains(candidate.target_node),
                relationship_visible: candidate
                    .relationship_id
                    .is_none_or(|relationship_id| !hidden_relationships.contains(relationship_id)),
            })
            .collect()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test driver keeps graph coordinates, paging, and visibility masks explicit"
    )]
    fn run_resumable_single(
        nodes: &NodeStore,
        edges: &EdgeStore,
        source: u32,
        target: u32,
        max_depth: i32,
        page_size: usize,
        hidden_nodes: &RoaringBitmap,
        hidden_relationships: &RoaringBitmap,
    ) -> GraphResult<Option<Vec<PathStep>>> {
        let governor = path_governor(100_000);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let neighbors = CsrNeighbors::new(edges);
        let mut machine = ResumableSingleDirectionBfs::try_new(
            nodes.node_count() as usize,
            source,
            target,
            max_depth,
        )?;
        loop {
            match materialize_single_direction_path_batch(
                &mut machine,
                nodes,
                &neighbors,
                &relationships,
                None,
                candidate_limits(page_size),
                &governor,
            )? {
                ResumablePathMaterialization::Batch(batch) => {
                    let verdicts = candidate_verdicts(&batch, hidden_nodes, hidden_relationships);
                    apply_single_direction_path_verdicts(
                        &mut machine,
                        &batch,
                        &verdicts,
                        nodes,
                        &governor,
                    )?;
                }
                ResumablePathMaterialization::Progress => {}
                ResumablePathMaterialization::Complete => break,
            }
            if machine.is_complete() {
                break;
            }
        }
        machine.finish(nodes, &["".to_string(), "REL".to_string()])
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test driver keeps graph coordinates, paging, and visibility masks explicit"
    )]
    fn run_resumable_bidirectional(
        nodes: &NodeStore,
        edges: &EdgeStore,
        source: u32,
        target: u32,
        max_depth: i32,
        page_size: usize,
        hidden_nodes: &RoaringBitmap,
        hidden_relationships: &RoaringBitmap,
    ) -> GraphResult<Option<Vec<PathStep>>> {
        let governor = path_governor(100_000);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let neighbors = CsrNeighbors::new(edges);
        let mut machine = ResumableBidirectionalBfs::try_new(
            nodes.node_count() as usize,
            source,
            target,
            max_depth,
        )?;
        loop {
            match materialize_bidirectional_path_batch(
                &mut machine,
                nodes,
                &neighbors,
                &relationships,
                None,
                candidate_limits(page_size),
                &governor,
            )? {
                ResumablePathMaterialization::Batch(batch) => {
                    let verdicts = candidate_verdicts(&batch, hidden_nodes, hidden_relationships);
                    apply_bidirectional_path_verdicts(
                        &mut machine,
                        &batch,
                        &verdicts,
                        nodes,
                        &governor,
                    )?;
                }
                ResumablePathMaterialization::Progress => {}
                ResumablePathMaterialization::Complete => break,
            }
            if machine.is_complete() {
                break;
            }
        }
        machine.finish(nodes, &["".to_string(), "REL".to_string()])
    }

    fn path_signature(path: Option<Vec<PathStep>>) -> Option<Vec<(String, Option<String>)>> {
        path.map(|steps| {
            steps
                .into_iter()
                .map(|step| (step.node_id, step.edge_label))
                .collect()
        })
    }

    fn symmetric_store(node_count: u32, pairs: &[(u32, u32)]) -> EdgeStore {
        let edges = pairs
            .iter()
            .flat_map(|&(left, right)| {
                [
                    RawEdge {
                        source: left,
                        target: right,
                        type_id: crate::types::EdgeTypeId::test_v6(1),
                        weight: None,
                        schema_reversed: false,
                    },
                    RawEdge {
                        source: right,
                        target: left,
                        type_id: crate::types::EdgeTypeId::test_v6(1),
                        weight: None,
                        schema_reversed: true,
                    },
                ]
            })
            .collect();
        EdgeStore::from_edges(node_count, edges, false)
    }

    #[test]
    fn resumable_single_direction_bfs_matches_eager_target_discovery_and_ties() {
        let mut nodes = NodeStore::new();
        for index in 0..6 {
            nodes.add_node(100, format!("N-{index}"));
        }
        let raw = |source, target| RawEdge {
            source,
            target,
            type_id: crate::types::EdgeTypeId::test_v6(1),
            weight: None,
            schema_reversed: false,
        };
        let edges = EdgeStore::from_edges(
            6,
            vec![raw(0, 1), raw(0, 2), raw(1, 3), raw(2, 3), raw(3, 5)],
            false,
        );
        let eager = shortest_path_with_neighbors(
            &nodes,
            &CsrNeighbors::new(&edges),
            0,
            5,
            5,
            true,
            &["".to_string(), "REL".to_string()],
        );
        for page_size in 1..=4 {
            let lazy = run_resumable_single(
                &nodes,
                &edges,
                0,
                5,
                5,
                page_size,
                &RoaringBitmap::new(),
                &RoaringBitmap::new(),
            )
            .unwrap();
            assert_eq!(path_signature(lazy), path_signature(eager.clone()));
        }
    }

    #[test]
    fn resumable_bidirectional_bfs_matches_eager_meeting_node_selection() {
        let mut nodes = NodeStore::new();
        for index in 0..8 {
            nodes.add_node(100, format!("N-{index}"));
        }
        let edges = symmetric_store(8, &[(0, 1), (0, 2), (1, 4), (2, 3), (3, 4), (4, 7)]);
        let eager = shortest_path_with_neighbors(
            &nodes,
            &CsrNeighbors::new(&edges),
            0,
            7,
            8,
            false,
            &["".to_string(), "REL".to_string()],
        );
        for page_size in 1..=4 {
            let lazy = run_resumable_bidirectional(
                &nodes,
                &edges,
                0,
                7,
                8,
                page_size,
                &RoaringBitmap::new(),
                &RoaringBitmap::new(),
            )
            .unwrap();
            assert_eq!(path_signature(lazy), path_signature(eager.clone()));
        }
    }

    #[test]
    fn resumable_bidirectional_bfs_completes_chosen_level_before_selecting_meeting() {
        let mut nodes = NodeStore::new();
        for index in 0..7 {
            nodes.add_node(100, format!("N-{index}"));
        }
        // The first meeting encountered in the selected level is longer than
        // the later meeting. The eager algorithm scans the complete level.
        let edges = symmetric_store(7, &[(0, 1), (0, 2), (1, 3), (3, 4), (4, 6), (2, 5), (5, 6)]);
        let eager = shortest_path_with_neighbors(
            &nodes,
            &CsrNeighbors::new(&edges),
            0,
            6,
            8,
            false,
            &["".to_string(), "REL".to_string()],
        );
        let lazy = run_resumable_bidirectional(
            &nodes,
            &edges,
            0,
            6,
            8,
            1,
            &RoaringBitmap::new(),
            &RoaringBitmap::new(),
        )
        .unwrap();
        assert_eq!(path_signature(lazy), path_signature(eager));
    }

    #[test]
    fn resumable_unweighted_paths_match_eager_hidden_nodes_relationships_and_endpoints() {
        let mut nodes = NodeStore::new();
        for index in 0..6 {
            nodes.add_node(100, format!("N-{index}"));
        }
        let raw = |source, target| RawEdge {
            source,
            target,
            type_id: crate::types::EdgeTypeId::test_v6(1),
            weight: None,
            schema_reversed: false,
        };
        let edges = identified_store(
            6,
            false,
            vec![
                (raw(0, 1), 10),
                (raw(1, 5), 11),
                (raw(0, 2), 12),
                (raw(2, 3), 13),
                (raw(3, 5), 14),
            ],
        );
        let mut hidden_nodes = RoaringBitmap::new();
        hidden_nodes.insert(1);
        let lazy = run_resumable_single(
            &nodes,
            &edges,
            0,
            5,
            6,
            1,
            &hidden_nodes,
            &RoaringBitmap::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            lazy.iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            ["N-0", "N-2", "N-3", "N-5"]
        );
        hidden_nodes.insert(5);
        assert!(run_resumable_single(
            &nodes,
            &edges,
            0,
            5,
            6,
            1,
            &hidden_nodes,
            &RoaringBitmap::new(),
        )
        .unwrap()
        .is_none());
        let mut hidden_relationships = RoaringBitmap::new();
        hidden_relationships.insert(10);
        let relation_hidden = run_resumable_single(
            &nodes,
            &edges,
            0,
            5,
            6,
            2,
            &RoaringBitmap::new(),
            &hidden_relationships,
        )
        .unwrap()
        .unwrap();
        assert_eq!(relation_hidden[1].node_id, "N-2");
    }

    #[test]
    fn resumable_unweighted_paths_match_eager_overlay_durable_and_tx() {
        // Representation-specific owned cursors already have exhaustive
        // differentials; this freezes the path machine against their common
        // `NeighborSource` contract with an adversarial logical pager.
        let mut nodes = NodeStore::new();
        for index in 0..5 {
            nodes.add_node(100, format!("N-{index}"));
        }
        let edges = symmetric_store(5, &[(0, 1), (0, 2), (1, 3), (2, 3), (3, 4)]);
        let eager = shortest_path_with_neighbors(
            &nodes,
            &CsrNeighbors::new(&edges),
            0,
            4,
            6,
            false,
            &["".to_string(), "REL".to_string()],
        );
        let lazy = run_resumable_bidirectional(
            &nodes,
            &edges,
            0,
            4,
            6,
            1,
            &RoaringBitmap::new(),
            &RoaringBitmap::new(),
        )
        .unwrap();
        assert_eq!(path_signature(lazy), path_signature(eager));
    }

    #[test]
    fn resumable_unweighted_paths_preserve_max_depth_work_caps_errors_and_epoch_rejection() {
        let mut nodes = NodeStore::new();
        nodes.add_node(100, "N-0".into());
        nodes.add_node(100, "N-1".into());
        let edges = symmetric_store(2, &[(0, 1)]);
        assert!(run_resumable_single(
            &nodes,
            &edges,
            0,
            1,
            0,
            1,
            &RoaringBitmap::new(),
            &RoaringBitmap::new(),
        )
        .unwrap()
        .is_none());
        assert!(run_resumable_single(
            &nodes,
            &edges,
            0,
            0,
            -1,
            1,
            &RoaringBitmap::new(),
            &RoaringBitmap::new(),
        )
        .unwrap()
        .is_some());
        let epoch = BfsProjectionEpoch {
            generation_id: Some(1),
            applied_sync_id: 1,
            node_count: 2,
            edge_count: 2,
            relationship_identity_count: 0,
            edge_buffer_len: 0,
            edge_buffer_revision: 0,
            tx_topology_revision: 0,
            tx_added_nodes: 0,
            tx_added_edges: 0,
            tx_deleted_nodes: 0,
            tx_deleted_edges: 0,
        };
        let mut machine = ResumableSingleDirectionBfs::try_new(2, 0, 1, 2).unwrap();
        machine.bind_projection_epoch(epoch);
        let mut changed = epoch;
        changed.edge_count = 3;
        assert!(machine.require_projection_epoch(changed).is_err());

        let mut bidirectional = ResumableBidirectionalBfs::try_new(2, 0, 1, 2).unwrap();
        bidirectional.bind_projection_epoch(epoch);
        assert!(bidirectional.require_projection_epoch(changed).is_err());

        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let neighbors = CsrNeighbors::new(&edges);
        let governor = path_governor(0);
        let mut capped = ResumableSingleDirectionBfs::try_new(2, 0, 1, 2).unwrap();
        let ResumablePathMaterialization::Batch(endpoints) =
            materialize_single_direction_path_batch(
                &mut capped,
                &nodes,
                &neighbors,
                &relationships,
                None,
                candidate_limits(2),
                &governor,
            )
            .unwrap()
        else {
            panic!("endpoint visibility batch expected");
        };
        let verdicts = candidate_verdicts(&endpoints, &RoaringBitmap::new(), &RoaringBitmap::new());
        apply_single_direction_path_verdicts(&mut capped, &endpoints, &verdicts, &nodes, &governor)
            .unwrap();
        assert!(materialize_single_direction_path_batch(
            &mut capped,
            &nodes,
            &neighbors,
            &relationships,
            None,
            candidate_limits(1),
            &governor,
        )
        .is_err());
    }

    #[test]
    fn resumable_path_rejects_oversized_identity_keys_before_candidate_cloning() {
        let limits = BfsCandidateLimits {
            max_candidates: 1,
            max_key_bytes: 8,
        };

        let mut oversized_node = NodeStore::new();
        oversized_node.add_node(100, "source".into());
        oversized_node.add_node(100, "target-key-is-too-large".into());
        let mut pending = VecDeque::new();
        let error = path_candidate_fits(
            &[],
            &mut 0,
            Neighbor {
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                schema_reversed: false,
                relationship_id: None,
            },
            Some(&mut pending),
            limits,
            &oversized_node,
            &crate::relationship_identity_store::RelationshipIdentityStore::default(),
        )
        .unwrap_err();
        assert!(
            matches!(
                &error,
                GraphError::InvalidFilter { reason }
                    if reason == "one path visibility candidate exceeds the key-byte limit"
            ),
            "unexpected oversized node-key error: {error:?}"
        );

        let mut nodes = NodeStore::new();
        nodes.add_node(100, "source".into());
        nodes.add_node(100, "target".into());
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::try_from_owned(vec![
                None,
                Some(RelationshipIdentity {
                    mapping_id: 1,
                    source_key: "relationship-key-is-too-large".into(),
                }),
            ])
            .unwrap();
        let error = path_candidate_fits(
            &[],
            &mut 0,
            Neighbor {
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                schema_reversed: false,
                relationship_id: Some(1),
            },
            Some(&mut pending),
            limits,
            &nodes,
            &relationships,
        )
        .unwrap_err();
        assert!(
            matches!(
                &error,
                GraphError::InvalidFilter { reason }
                    if reason == "one path visibility candidate exceeds the key-byte limit"
            ),
            "unexpected oversized relationship-key error: {error:?}"
        );
    }

    proptest::proptest! {
        #[test]
        fn resumable_unweighted_paths_match_eager_for_generated_graphs_and_visibility_masks(
            node_count in 2u32..10,
            raw_pairs in proptest::collection::vec((0u32..10, 0u32..10), 0..30),
            hidden_flags in proptest::collection::vec(proptest::bool::ANY, 0..10),
            page_size in 1usize..5,
        ) {
            let mut nodes = NodeStore::new();
            for index in 0..node_count {
                nodes.add_node(100, format!("N-{index}"));
            }
            let pairs = raw_pairs
                .into_iter()
                .filter(|(left, right)| left < &node_count && right < &node_count && left != right)
                .collect::<Vec<_>>();
            let edges = symmetric_store(node_count, &pairs);
            let hidden_nodes = hidden_flags
                .into_iter()
                .take(node_count as usize)
                .enumerate()
                .filter_map(|(index, hidden)| hidden.then_some(index as u32))
                .collect::<RoaringBitmap>();
            let visibility = VisibilityScope::enforced_for_test(
                hidden_nodes.clone(),
                RoaringBitmap::new(),
                RoaringBitmap::new(),
            );
            let eager = shortest_path_with_neighbors_inner(
                &nodes,
                &CsrNeighbors::new(&edges),
                UnweightedPathRequest {
                    source: 0,
                    target: node_count - 1,
                    max_depth: 10,
                    has_unidirectional_edges: false,
                    edge_type_registry: &["".to_string(), "REL".to_string()],
                    edge_type_label: None,
                },
                None,
                &visibility,
                None,
            );
            let lazy = run_resumable_bidirectional(
                &nodes,
                &edges,
                0,
                node_count - 1,
                10,
                page_size,
                &hidden_nodes,
                &RoaringBitmap::new(),
            )?;
            proptest::prop_assert_eq!(path_signature(lazy), path_signature(eager));
        }
    }

    #[test]
    fn hidden_shorter_path_yields_longer_visible_path() {
        let mut nodes = NodeStore::new();
        for idx in 0..5 {
            nodes.add_node(100, format!("N-{idx}"));
        }
        let raw = |source, target| RawEdge {
            source,
            target,
            type_id: crate::types::EdgeTypeId::test_v6(1),
            weight: None,
            schema_reversed: false,
        };
        let edges = identified_store(
            5,
            false,
            vec![
                (raw(0, 1), 10),
                (raw(1, 3), 11),
                (raw(0, 2), 12),
                (raw(2, 4), 13),
                (raw(4, 3), 14),
            ],
        );
        let absent_shortcut = identified_store(
            5,
            false,
            vec![(raw(0, 2), 12), (raw(2, 4), 13), (raw(4, 3), 14)],
        );
        let mut hidden_nodes = RoaringBitmap::new();
        hidden_nodes.insert(1);
        let visibility = VisibilityScope::enforced_for_test(
            hidden_nodes,
            RoaringBitmap::new(),
            RoaringBitmap::new(),
        );
        let governor = path_governor(1_000);
        let context = QueryExecutionContext::new(&governor, &visibility);
        let result = shortest_path_with_neighbors_governed_with_context(
            &nodes,
            &CsrNeighbors::new(&edges),
            UnweightedPathRequest {
                source: 0,
                target: 3,
                max_depth: 5,
                has_unidirectional_edges: true,
                edge_type_registry: &["REL".to_string(), "REL".to_string()],
                edge_type_label: None,
            },
            &context,
        )
        .unwrap()
        .unwrap();
        let absent_coordinator = VisibilityCoordinator::unrestricted_for_test_or_benchmark();
        let absent_context = absent_coordinator.context(&governor);
        let absent_result = shortest_path_with_neighbors_governed_with_context(
            &nodes,
            &CsrNeighbors::new(&absent_shortcut),
            UnweightedPathRequest {
                source: 0,
                target: 3,
                max_depth: 5,
                has_unidirectional_edges: true,
                edge_type_registry: &["REL".to_string(), "REL".to_string()],
                edge_type_label: None,
            },
            &absent_context,
        )
        .unwrap()
        .unwrap();
        let summarize = |path: &[crate::types::PathStep]| {
            path.iter()
                .map(|step| (step.step, step.node_id.clone(), step.edge_label.clone()))
                .collect::<Vec<_>>()
        };

        assert_eq!(summarize(&result), summarize(&absent_result));
        assert_eq!(
            result
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["N-0", "N-2", "N-4", "N-3"]
        );
    }

    #[test]
    fn bidirectional_meeting_ignores_hidden_candidate() {
        let mut nodes = NodeStore::new();
        for idx in 0..5 {
            nodes.add_node(100, format!("N-{idx}"));
        }
        let raw = |source, target| RawEdge {
            source,
            target,
            type_id: crate::types::EdgeTypeId::test_v6(1),
            weight: None,
            schema_reversed: false,
        };
        let pairs = [(0, 1), (1, 3), (0, 2), (2, 4), (4, 3)];
        let mut identified = Vec::new();
        for (relationship_id, (source, target)) in (20..).zip(pairs) {
            identified.push((raw(source, target), relationship_id));
            identified.push((raw(target, source), relationship_id));
        }
        let edges = identified_store(5, false, identified);
        let mut hidden_nodes = RoaringBitmap::new();
        hidden_nodes.insert(1);
        let visibility = VisibilityScope::enforced_for_test(
            hidden_nodes,
            RoaringBitmap::new(),
            RoaringBitmap::new(),
        );
        let governor = path_governor(1_000);
        let context = QueryExecutionContext::new(&governor, &visibility);
        let result = shortest_path_with_neighbors_governed_with_context(
            &nodes,
            &CsrNeighbors::new(&edges),
            UnweightedPathRequest {
                source: 0,
                target: 3,
                max_depth: 5,
                has_unidirectional_edges: false,
                edge_type_registry: &["REL".to_string(), "REL".to_string()],
                edge_type_label: None,
            },
            &context,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            result
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["N-0", "N-2", "N-4", "N-3"]
        );
    }

    #[test]
    fn hidden_cheaper_relationship_yields_visible_weighted_path() {
        let mut nodes = NodeStore::new();
        for idx in 0..3 {
            nodes.add_node(100, format!("N-{idx}"));
        }
        let weighted = |source, target, weight| RawEdge {
            source,
            target,
            type_id: crate::types::EdgeTypeId::test_v6(1),
            weight: Some(weight),
            schema_reversed: false,
        };
        let edges = identified_store(
            3,
            true,
            vec![
                (weighted(0, 1, 1), 10),
                (weighted(0, 2, 2), 11),
                (weighted(2, 1, 2), 12),
            ],
        );
        let mut hidden_relationships = RoaringBitmap::new();
        hidden_relationships.insert(10);
        let mut rls_edge_types = RoaringBitmap::new();
        rls_edge_types.insert(1);
        let visibility = VisibilityScope::enforced_for_test(
            RoaringBitmap::new(),
            hidden_relationships,
            rls_edge_types,
        );
        let governor = path_governor(1_000);
        let context = QueryExecutionContext::new(&governor, &visibility);
        let result = weighted_shortest_path_with_neighbors_governed_with_context(
            &nodes,
            &edges,
            0,
            1,
            &["REL".to_string(), "REL".to_string()],
            &context,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            result
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["N-0", "N-2", "N-1"]
        );
        assert_eq!(result.last().unwrap().total_cost, 4);
    }

    #[test]
    fn edge_type_filters_select_longer_unweighted_and_weighted_paths() {
        let mut nodes = NodeStore::new();
        for idx in 0..4 {
            nodes.add_node(100, format!("N-{idx}"));
        }
        let edge = |source, target, type_id, weight| RawEdge {
            source,
            target,
            type_id,
            weight: Some(weight),
            schema_reversed: false,
        };
        let edges = identified_store(
            4,
            true,
            vec![
                (edge(0, 3, EdgeTypeId::test_v6(1), 1), 10),
                (edge(0, 1, EdgeTypeId::test_v6(2), 2), 11),
                (edge(1, 2, EdgeTypeId::test_v6(2), 2), 12),
                (edge(2, 3, EdgeTypeId::test_v6(2), 2), 13),
            ],
        );
        let mut only_long = RoaringBitmap::new();
        only_long.insert(2);
        let governor = path_governor(1_000);
        let visibility = VisibilityScope::unrestricted_for_test();
        let context =
            QueryExecutionContext::with_edge_type_filter(&governor, &visibility, Some(&only_long));
        let registry = ["".to_string(), "direct".to_string(), "long".to_string()];

        let unweighted = shortest_path_with_neighbors_governed_with_context(
            &nodes,
            &CsrNeighbors::new(&edges),
            UnweightedPathRequest {
                source: 0,
                target: 3,
                max_depth: 5,
                has_unidirectional_edges: true,
                edge_type_registry: &registry,
                edge_type_label: None,
            },
            &context,
        )
        .unwrap()
        .unwrap();
        let weighted = weighted_shortest_path_with_neighbors_governed_with_context(
            &nodes, &edges, 0, 3, &registry, &context,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            unweighted
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["N-0", "N-1", "N-2", "N-3"]
        );
        assert_eq!(
            weighted
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["N-0", "N-1", "N-2", "N-3"]
        );
        assert_eq!(weighted.last().unwrap().total_cost, 6);

        let empty = RoaringBitmap::new();
        let empty_context =
            QueryExecutionContext::with_edge_type_filter(&governor, &visibility, Some(&empty));
        assert!(shortest_path_with_neighbors_governed_with_context(
            &nodes,
            &CsrNeighbors::new(&edges),
            UnweightedPathRequest {
                source: 0,
                target: 3,
                max_depth: 5,
                has_unidirectional_edges: true,
                edge_type_registry: &registry,
                edge_type_label: None,
            },
            &empty_context,
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn governed_shortest_path_stops_at_expansion_limit() {
        let mut nodes = NodeStore::new();
        for idx in 0..4 {
            nodes.add_node(100, format!("N-{idx}"));
        }
        let edges = EdgeStore::from_edges(
            4,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: None,
                    schema_reversed: false,
                },
                RawEdge {
                    source: 1,
                    target: 2,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: None,
                    schema_reversed: false,
                },
                RawEdge {
                    source: 2,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: None,
                    schema_reversed: false,
                },
            ],
            false,
        );
        let neighbors = CsrNeighbors::new(&edges);
        let error = shortest_path_with_neighbors_governed(
            &nodes,
            &neighbors,
            UnweightedPathRequest {
                source: 0,
                target: 3,
                max_depth: 4,
                has_unidirectional_edges: true,
                edge_type_registry: &["".to_string(), "edge".to_string()],
                edge_type_label: None,
            },
            &path_governor(1),
        )
        .expect_err("path should stop before exceeding its work budget");

        assert!(matches!(
            error,
            crate::safety::GraphError::ResourceLimit { .. }
        ));
    }

    #[test]
    fn shortest_path_simple_chain() {
        let mut ns = NodeStore::new();
        for i in 0..4u32 {
            ns.add_node(100, format!("N-{}", i));
        }

        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 3,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(4, edges, false);
        let registry = vec!["".to_string(), "connected".to_string()];

        let result = shortest_path(&ns, &es, 0, 3, 20, false, &registry);
        assert!(result.is_some());
        let steps = result.unwrap();
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0].node_id, "N-0");
        assert_eq!(steps[3].node_id, "N-3");
        assert_eq!(steps[0].edge_label, None);
        assert_eq!(steps[1].edge_label.as_deref(), Some("connected"));
        assert_eq!(steps[2].edge_label.as_deref(), Some("connected"));
        assert_eq!(steps[3].edge_label.as_deref(), Some("connected"));
    }

    #[test]
    fn shortest_path_same_node() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "N-0".to_string());
        let es = EdgeStore::from_edges(1, vec![], false);
        let registry = vec![];

        let result = shortest_path(&ns, &es, 0, 0, 20, false, &registry);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn shortest_path_no_connection() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        ns.add_node(100, "B".to_string());
        let es = EdgeStore::from_edges(2, vec![], false);
        let registry = vec![];

        let result = shortest_path(&ns, &es, 0, 1, 20, false, &registry);
        assert!(result.is_none());
    }

    /// Builds a symmetric (undirected) graph from a fixed edge list and
    /// returns `(NodeStore, EdgeStore)` with `node_count` nodes.
    fn symmetric_graph(node_count: u32, undirected_edges: &[(u32, u32)]) -> (NodeStore, EdgeStore) {
        let mut ns = NodeStore::new();
        for i in 0..node_count {
            ns.add_node(100, format!("N-{i}"));
        }
        let mut edges = Vec::new();
        for &(a, b) in undirected_edges {
            edges.push(RawEdge {
                source: a,
                target: b,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            });
            edges.push(RawEdge {
                source: b,
                target: a,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            });
        }
        let es = EdgeStore::from_edges(node_count, edges, false);
        (ns, es)
    }

    /// `bidirectional_bfs` must return a path exactly as short as the
    /// always-correct `single_direction_bfs` ground truth. Constructed so
    /// the backward search discovers two forward-visited candidates in the
    /// same round: a distant one via node 3 (combined length 4, through
    /// 0-1-3-5-4) reached before a near one via node 2 (combined length 3,
    /// through 0-2-6-4). The bug accepts whichever meeting node is found
    /// first by iteration order rather than the shortest one available.
    #[test]
    fn bidirectional_bfs_selects_minimal_combined_distance_meeting_node() {
        // Nodes: 0=S 1=P 2=Q 3=X 4=T 5=C1 6=C2
        // S-P, S-Q (siblings, both forward depth 1)
        // P-X (X only reachable via P, forward depth 2)
        // T-C1, T-C2 (siblings, both backward depth 1)
        // C1-X (bad meeting: combined = depth(X)=2 + depth(via C1)=2 -> 4)
        // C2-Q (good meeting: combined = depth(Q)=1 + depth(via C2)=2 -> 3)
        let edges = [(0, 1), (0, 2), (1, 3), (4, 5), (4, 6), (5, 3), (6, 2)];
        let (ns, es) = symmetric_graph(7, &edges);
        let registry = vec!["".to_string(), "edge".to_string()];

        let bidirectional = shortest_path(&ns, &es, 0, 4, 20, false, &registry)
            .expect("a path exists between S and T");
        let ground_truth = shortest_path(&ns, &es, 0, 4, 20, true, &registry)
            .expect("single-direction BFS must find the same path's existence");

        assert_eq!(
            bidirectional.len(),
            ground_truth.len(),
            "bidirectional BFS returned a non-minimal path: {} steps \
             (bidirectional) vs {} steps (ground truth)",
            bidirectional.len(),
            ground_truth.len()
        );
    }

    proptest::proptest! {
        /// Differential check across random small symmetric graphs:
        /// bidirectional_bfs must never return a path longer than the
        /// always-correct single_direction_bfs ground truth, and must
        /// never report "no path" when one exists.
        #[test]
        fn bidirectional_bfs_matches_single_direction_bfs(
            node_count in 3u32..12,
            raw_edges in proptest::collection::vec((0u32..12, 0u32..12), 0..24),
            source_seed in 0u32..12,
            target_seed in 0u32..12,
        ) {
            let source = source_seed % node_count;
            let target = target_seed % node_count;
            let edges: Vec<(u32, u32)> = raw_edges
                .into_iter()
                .filter(|&(a, b)| a < node_count && b < node_count && a != b)
                .collect();
            let (ns, es) = symmetric_graph(node_count, &edges);
            let registry = vec!["".to_string(), "edge".to_string()];

            let bidirectional = shortest_path(&ns, &es, source, target, 30, false, &registry);
            let ground_truth = shortest_path(&ns, &es, source, target, 30, true, &registry);

            match (bidirectional, ground_truth) {
                (None, None) => {}
                (Some(b), Some(g)) => {
                    proptest::prop_assert_eq!(
                        b.len(),
                        g.len(),
                        "bidirectional BFS returned a non-minimal path"
                    );
                }
                (None, Some(g)) => {
                    proptest::prop_assert!(
                        false,
                        "bidirectional BFS found no path but a {}-step path exists",
                        g.len()
                    );
                }
                (Some(b), None) => {
                    proptest::prop_assert!(
                        false,
                        "bidirectional BFS found a {}-step path but ground truth found none",
                        b.len()
                    );
                }
            }
        }
    }

    #[test]
    fn weighted_shortest_path_prefers_lower_total_cost() {
        let mut ns = NodeStore::new();
        for id in ["A", "B", "C", "D"] {
            ns.add_node(100, id.to_string());
        }

        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: Some(100),
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 3,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: Some(1),
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: Some(5),
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: Some(5),
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(4, edges, true);
        let registry = vec!["".to_string(), "weighted".to_string()];

        let path = weighted_shortest_path(&ns, &es, 0, 3, &registry).unwrap();

        assert_eq!(
            path.iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "C", "D"]
        );
        assert_eq!(
            path.iter()
                .map(|step| step.edge_label.as_deref())
                .collect::<Vec<_>>(),
            vec![None, Some("weighted"), Some("weighted")]
        );
        assert_eq!(
            path.iter().map(|step| step.edge_weight).collect::<Vec<_>>(),
            vec![None, Some(5), Some(5)]
        );
        assert_eq!(
            path.iter().map(|step| step.step_cost).collect::<Vec<_>>(),
            vec![0, 5, 10]
        );
        assert!(path.iter().all(|step| step.total_cost == 10));
    }

    #[test]
    fn weighted_shortest_path_allows_u32_max_total_cost() {
        let mut ns = NodeStore::new();
        for id in ["A", "B", "C"] {
            ns.add_node(100, id.to_string());
        }

        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: Some(u32::MAX - 1),
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: Some(1),
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(3, edges, true);
        let registry = vec!["".to_string(), "weighted".to_string()];

        let path = weighted_shortest_path(&ns, &es, 0, 2, &registry).unwrap();

        assert_eq!(
            path.iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B", "C"]
        );
        assert_eq!(path.last().unwrap().total_cost, u64::from(u32::MAX));
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "weighted differential driver keeps endpoints, masks, and page size explicit"
    )]
    fn run_resumable_dijkstra(
        nodes: &NodeStore,
        edges: &EdgeStore,
        source: u32,
        target: u32,
        edge_filter: Option<&RoaringBitmap>,
        hidden_nodes: &RoaringBitmap,
        hidden_relationships: &RoaringBitmap,
        page_size: usize,
    ) -> GraphResult<Option<Vec<WeightedPathStep>>> {
        let governor = path_governor(100_000);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let mut machine = ResumableDijkstra::try_new(nodes.node_count() as usize, source, target)?;
        loop {
            match materialize_resumable_dijkstra_batch(
                &mut machine,
                nodes,
                edges,
                &relationships,
                edge_filter,
                candidate_limits(page_size),
                &governor,
            )? {
                ResumablePathMaterialization::Batch(batch) => {
                    let verdicts = candidate_verdicts(&batch, hidden_nodes, hidden_relationships);
                    apply_resumable_dijkstra_verdicts(
                        &mut machine,
                        &batch,
                        &verdicts,
                        &relationships,
                        nodes,
                        &governor,
                    )?;
                }
                ResumablePathMaterialization::Progress => {}
                ResumablePathMaterialization::Complete => break,
            }
            if machine.is_complete() {
                break;
            }
        }
        machine.finish(nodes, &["".into(), "route".into()], &governor)
    }

    #[test]
    fn resumable_dijkstra_matches_eager_heap_ties_stale_entries_and_target_pop() {
        let mut nodes = NodeStore::new();
        for id in ["s", "a", "b", "t"] {
            nodes.add_node(100, id.into());
        }
        let edges = EdgeStore::from_edges(
            4,
            vec![
                RawEdge {
                    source: 0,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(10),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(2),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(2),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 1,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(2),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 2,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(2),
                    schema_reversed: false,
                },
            ],
            true,
        );
        let eager = weighted_shortest_path(&nodes, &edges, 0, 3, &["".into(), "route".into()]);
        for page_size in [1, 2, 4] {
            let lazy = run_resumable_dijkstra(
                &nodes,
                &edges,
                0,
                3,
                None,
                &RoaringBitmap::new(),
                &RoaringBitmap::new(),
                page_size,
            )
            .unwrap();
            assert_eq!(lazy, eager);
        }
    }

    #[test]
    fn resumable_dijkstra_hidden_identity_endpoint_is_fail_closed() {
        let mut nodes = NodeStore::new();
        nodes.add_node(100, "s".into());
        let edges = EdgeStore::from_edges(1, Vec::new(), true);
        let mut hidden = RoaringBitmap::new();
        hidden.insert(0);
        assert_eq!(
            run_resumable_dijkstra(
                &nodes,
                &edges,
                0,
                0,
                None,
                &hidden,
                &RoaringBitmap::new(),
                1,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn resumable_dijkstra_matches_eager_generated_visibility_and_weights() {
        let mut nodes = NodeStore::new();
        for id in ["s", "cheap", "visible", "t"] {
            nodes.add_node(100, id.into());
        }
        let edges = EdgeStore::from_edges(
            4,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(1),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 1,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(1),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(3),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 2,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(3),
                    schema_reversed: false,
                },
            ],
            true,
        );
        for hidden_mask in 0u32..4 {
            let mut hidden = RoaringBitmap::new();
            if hidden_mask & 1 != 0 {
                hidden.insert(1);
            }
            if hidden_mask & 2 != 0 {
                hidden.insert(2);
            }
            let baseline = run_resumable_dijkstra(
                &nodes,
                &edges,
                0,
                3,
                None,
                &hidden,
                &RoaringBitmap::new(),
                64,
            )
            .unwrap();
            for page_size in [1, 2, 3] {
                assert_eq!(
                    run_resumable_dijkstra(
                        &nodes,
                        &edges,
                        0,
                        3,
                        None,
                        &hidden,
                        &RoaringBitmap::new(),
                        page_size,
                    )
                    .unwrap(),
                    baseline,
                    "visibility mask {hidden_mask} diverged at page size {page_size}",
                );
            }
        }
    }

    #[test]
    fn resumable_dijkstra_preserves_weighted_step_metadata_and_edge_type_filters() {
        let mut nodes = NodeStore::new();
        for id in ["s", "a", "b", "t"] {
            nodes.add_node(100, id.into());
        }
        let edges = EdgeStore::from_edges(
            4,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(1),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 1,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(1),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: crate::types::EdgeTypeId::test_v6(2),
                    weight: Some(3),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 2,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(2),
                    weight: Some(3),
                    schema_reversed: false,
                },
            ],
            true,
        );
        let mut allowed = RoaringBitmap::new();
        allowed.insert(2);
        let result = run_resumable_dijkstra(
            &nodes,
            &edges,
            0,
            3,
            Some(&allowed),
            &RoaringBitmap::new(),
            &RoaringBitmap::new(),
            1,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            result
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            ["s", "b", "t"]
        );
        assert_eq!(
            result
                .iter()
                .map(|step| step.edge_weight)
                .collect::<Vec<_>>(),
            [None, Some(3), Some(3)]
        );
        assert_eq!(result.last().map(|step| step.total_cost), Some(6));
    }

    #[test]
    fn resumable_dijkstra_pages_one_popped_node_at_a_time() {
        let mut nodes = NodeStore::new();
        for id in ["s", "a", "b", "c", "t"] {
            nodes.add_node(100, id.into());
        }
        let edges = EdgeStore::from_edges(
            5,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(1),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(2),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(3),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 3,
                    target: 4,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(1),
                    schema_reversed: false,
                },
            ],
            true,
        );
        let expected = weighted_shortest_path(&nodes, &edges, 0, 4, &["".into(), "route".into()]);
        let actual = run_resumable_dijkstra(
            &nodes,
            &edges,
            0,
            4,
            None,
            &RoaringBitmap::new(),
            &RoaringBitmap::new(),
            1,
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn resumable_dijkstra_zero_output_pages_are_bounded() {
        let mut nodes = NodeStore::new();
        for id in ["s", "a", "b", "t"] {
            nodes.add_node(100, id.into());
        }
        let edges = EdgeStore::from_edges(
            4,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(1),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: crate::types::EdgeTypeId::test_v6(1),
                    weight: Some(1),
                    schema_reversed: false,
                },
                RawEdge {
                    source: 2,
                    target: 3,
                    type_id: crate::types::EdgeTypeId::test_v6(2),
                    weight: Some(1),
                    schema_reversed: false,
                },
            ],
            true,
        );
        let mut allowed = RoaringBitmap::new();
        allowed.insert(2);
        let result = run_resumable_dijkstra(
            &nodes,
            &edges,
            0,
            3,
            Some(&allowed),
            &RoaringBitmap::new(),
            &RoaringBitmap::new(),
            1,
        )
        .unwrap();
        assert!(
            result.is_none(),
            "filtered empty pages must make bounded progress to completion"
        );
    }

    #[test]
    fn resumable_dijkstra_rejects_epoch_work_memory_and_oversized_keys() {
        let mut machine = ResumableDijkstra::try_new(2, 0, 1).unwrap();
        let epoch = BfsProjectionEpoch {
            generation_id: Some(1),
            applied_sync_id: 1,
            node_count: 2,
            edge_count: 1,
            relationship_identity_count: 0,
            edge_buffer_len: 0,
            edge_buffer_revision: 0,
            tx_topology_revision: 0,
            tx_added_nodes: 0,
            tx_added_edges: 0,
            tx_deleted_nodes: 0,
            tx_deleted_edges: 0,
        };
        machine.bind_projection_epoch(epoch);
        let mut changed = epoch;
        changed.edge_buffer_revision = 1;
        assert!(machine.require_projection_epoch(changed).is_err());

        let mut nodes = NodeStore::new();
        nodes.add_node(100, "x".repeat(17 * 1_024));
        nodes.add_node(100, "target".into());
        let edges = EdgeStore::from_edges(2, Vec::new(), true);
        let governor = path_governor(10);
        let relationships =
            crate::relationship_identity_store::RelationshipIdentityStore::default();
        let mut oversized = ResumableDijkstra::try_new(2, 0, 1).unwrap();
        let error = materialize_resumable_dijkstra_batch(
            &mut oversized,
            &nodes,
            &edges,
            &relationships,
            None,
            BfsCandidateLimits {
                max_candidates: 1,
                max_key_bytes: 16 * 1_024,
            },
            &governor,
        )
        .unwrap_err();
        assert!(matches!(error, GraphError::InvalidFilter { .. }));

        let tiny_memory = ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(1)),
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::new(10),
            ElapsedBudget::new(Duration::from_secs(1)),
        ));
        let mut memory_limited = ResumableDijkstra::try_new(2, 0, 1).unwrap();
        let memory_error = materialize_resumable_dijkstra_batch(
            &mut memory_limited,
            &nodes,
            &edges,
            &relationships,
            None,
            candidate_limits(1),
            &tiny_memory,
        )
        .unwrap_err();
        assert!(matches!(memory_error, GraphError::ResourceLimit { .. }));

        let mut output_nodes = NodeStore::new();
        output_nodes.add_node(100, "output".into());
        let mut output_limited = ResumableDijkstra::try_new(1, 0, 0).unwrap();
        output_limited.source_visible = Some(true);
        output_limited.target_visible = Some(true);
        output_limited.state = ResumablePathState::Complete;
        let output_error = output_limited
            .finish(&output_nodes, &[String::new()], &tiny_memory)
            .unwrap_err();
        assert!(matches!(output_error, GraphError::ResourceLimit { .. }));

        let mut work_nodes = NodeStore::new();
        work_nodes.add_node(100, "source".into());
        work_nodes.add_node(100, "target".into());
        let work_edges = EdgeStore::from_edges(
            2,
            vec![RawEdge {
                source: 0,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: Some(1),
                schema_reversed: false,
            }],
            true,
        );
        let work_error = run_resumable_dijkstra(
            &work_nodes,
            &work_edges,
            0,
            1,
            None,
            &RoaringBitmap::new(),
            &RoaringBitmap::new(),
            1,
        );
        assert!(work_error.is_ok());
        let zero_work = path_governor(0);
        assert!(consume_path_work_without_interrupt(&zero_work).is_err());
    }

    #[test]
    fn max_depth_prevents_reaching_distant_node() {
        let mut ns = NodeStore::new();
        for i in 0..5u32 {
            ns.add_node(100, format!("N-{}", i));
        }
        // Chain: 0→1→2→3→4
        let mut edges = Vec::new();
        for i in 0..4u32 {
            edges.push(RawEdge {
                source: i,
                target: i + 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            });
            edges.push(RawEdge {
                source: i + 1,
                target: i,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            });
        }
        let es = EdgeStore::from_edges(5, edges, false);
        let registry = vec!["".to_string(), "linked".to_string()];

        // max_depth=2: can reach node 2 but not node 4
        let result = shortest_path(&ns, &es, 0, 4, 2, false, &registry);
        assert!(result.is_none(), "should not find path beyond max_depth");

        // But depth=4 should work
        let result = shortest_path(&ns, &es, 0, 4, 4, false, &registry);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 5);
    }

    #[test]
    fn tombstoned_node_blocks_shortest_path() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string()); // 0
        ns.add_node(100, "B".to_string()); // 1
        ns.add_node(100, "C".to_string()); // 2
                                           // Tombstone B — only bridge between A and C
        ns.deactivate(1);

        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(3, edges, false);
        let registry = vec!["".to_string(), "e".to_string()];

        let result = shortest_path(&ns, &es, 0, 2, 20, false, &registry);
        assert!(result.is_none(), "tombstoned bridge should block path");
    }

    #[test]
    fn dijkstra_on_unweighted_graph_returns_none() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        ns.add_node(100, "B".to_string());

        let edges = vec![RawEdge {
            source: 0,
            target: 1,
            type_id: crate::types::EdgeTypeId::test_v6(1),
            weight: None,
            schema_reversed: false,
        }];
        let es = EdgeStore::from_edges(2, edges, false); // NOT weighted
        let registry = vec!["".to_string(), "e".to_string()];

        let result = weighted_shortest_path(&ns, &es, 0, 1, &registry);
        assert!(
            result.is_none(),
            "unweighted graph should return None for Dijkstra"
        );
    }

    #[test]
    fn dijkstra_disconnected_target_returns_none() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        ns.add_node(100, "B".to_string());

        // Weighted but no edges connecting A→B
        let es = EdgeStore::from_edges(2, vec![], true);
        let registry = vec!["".to_string()];

        let result = weighted_shortest_path(&ns, &es, 0, 1, &registry);
        assert!(result.is_none());
    }

    #[test]
    fn shortest_path_with_unidirectional_flag() {
        let mut ns = NodeStore::new();
        for i in 0..3u32 {
            ns.add_node(100, format!("N-{}", i));
        }
        // 0→1→2 (one direction only)
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(3, edges, false);
        let registry = vec!["".to_string(), "forward".to_string()];

        // With unidirectional=true, uses single-direction BFS
        let result = shortest_path(&ns, &es, 0, 2, 20, true, &registry);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 3);

        // Reverse direction should fail (no reverse edges)
        let result = shortest_path(&ns, &es, 2, 0, 20, true, &registry);
        assert!(result.is_none());
    }

    #[test]
    fn shortest_path_invalid_endpoints_return_none() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        let es = EdgeStore::from_edges(1, vec![], false);
        let registry = vec!["".to_string()];

        assert!(shortest_path(&ns, &es, 0, 99, 20, false, &registry).is_none());
        assert!(shortest_path(&ns, &es, 99, 0, 20, false, &registry).is_none());
    }

    #[test]
    fn weighted_shortest_path_invalid_endpoints_return_none() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        let es = EdgeStore::from_edges(1, vec![], true);
        let registry = vec!["".to_string()];

        assert!(weighted_shortest_path(&ns, &es, 0, 99, &registry).is_none());
        assert!(weighted_shortest_path(&ns, &es, 99, 0, &registry).is_none());
    }

    #[test]
    fn shortest_path_same_source_and_target() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        let es = EdgeStore::from_edges(1, vec![], false);
        let registry = vec!["".to_string()];

        let result = shortest_path(&ns, &es, 0, 0, 20, false, &registry);
        // Same node → trivial path of length 1
        assert!(result.is_some());
        let steps = result.unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].node_id, "A");
    }

    #[test]
    fn shortest_path_avoids_tombstoned_nodes() {
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        ns.add_node(100, "B".to_string()); // will be tombstoned
        ns.add_node(100, "C".to_string());
        ns.deactivate(1); // tombstone B

        // A→B→C, but B is dead. Also A→C directly.
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(3, edges, false);
        let registry = vec!["".to_string(), "link".to_string()];

        let result = shortest_path(&ns, &es, 0, 2, 20, false, &registry);
        assert!(result.is_some());
        let steps = result.unwrap();
        // Should go A→C directly, not through tombstoned B
        assert!(!steps.iter().any(|s| s.node_id == "B"));
    }

    #[test]
    fn shortest_path_with_cycle_terminates() {
        // A→B→C→A (cycle), find path from A to C
        let mut ns = NodeStore::new();
        ns.add_node(100, "A".to_string());
        ns.add_node(100, "B".to_string());
        ns.add_node(100, "C".to_string());
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 0,
                type_id: crate::types::EdgeTypeId::test_v6(1),
                weight: None,
                schema_reversed: false,
            },
        ];
        let es = EdgeStore::from_edges(3, edges, false);
        let registry = vec!["".to_string(), "link".to_string()];

        let result = shortest_path(&ns, &es, 0, 2, 20, false, &registry);
        assert!(result.is_some());
        let steps = result.unwrap();
        assert_eq!(steps.first().unwrap().node_id, "A");
        assert_eq!(steps.last().unwrap().node_id, "C");
    }
}
