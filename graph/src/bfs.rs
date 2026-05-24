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
use crate::types::{FilterOp, PathCoordinate, TableOid, TraversalResult};

const SPARSE_METADATA_MIN_NODES: usize = 4_096;
const SPARSE_METADATA_RATIO: usize = 16;

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
    /// Sync overlay edges inserted after the last base build, keyed by source node.
    pub overlay_insert_edges: HashMap<u32, Vec<(u32, u8)>>,
    /// Sync overlay edges deleted after the last base build, keyed by source node.
    pub overlay_deleted_edges: HashMap<u32, HashSet<(u32, u8)>>,
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
}

/// Traversal depth metadata.
pub enum TraversalDepthMap {
    /// Dense mode stores one depth slot per graph node for fast result conversion.
    Dense(Vec<i32>),
    /// Sparse mode stores only visited nodes for low-visit-budget traversals on large graphs.
    Sparse(HashMap<u32, i32>),
}

impl TraversalDepthMap {
    fn new(node_count: usize, sparse: bool, expected_visits: usize) -> Self {
        if sparse {
            Self::Sparse(HashMap::with_capacity(expected_visits))
        } else {
            Self::Dense(vec![-1i32; node_count])
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
    fn new(node_count: usize, sparse: bool, expected_visits: usize) -> Self {
        if sparse {
            Self::Sparse(HashMap::with_capacity(expected_visits))
        } else {
            Self::Dense(vec![u32::MAX; node_count])
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
    fn new(node_count: usize, sparse: bool, expected_visits: usize) -> Self {
        if sparse {
            Self::Sparse(HashMap::with_capacity(expected_visits))
        } else {
            Self::Dense(vec![0u8; node_count])
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
pub fn execute(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
) -> BfsResult {
    let node_count = node_store.node_count() as usize;
    let sparse_metadata = use_sparse_metadata(node_count, config.max_nodes);
    let expected_visits = expected_visit_capacity(node_count, config.max_nodes);

    let mut visited = RoaringBitmap::new();
    let mut depth_map = TraversalDepthMap::new(node_count, sparse_metadata, expected_visits);
    let mut parent = TraversalParentMap::new(node_count, sparse_metadata, expected_visits);
    let mut parent_edge_type =
        TraversalParentEdgeTypes::new(node_count, sparse_metadata, expected_visits);
    if config.seed_node as usize >= node_count {
        return BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
        };
    }

    let mut frontier = VecDeque::with_capacity(config.max_frontier as usize);
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
        return BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
        };
    }

    let has_filters = !config.filter_ops.is_empty();

    // BFS loop — traversal state is allocated before this point.
    while let Some(current) = frontier.pop_front() {
        let current_depth = depth_map.get(current).unwrap_or(-1);

        // Depth limit check
        if current_depth >= config.max_depth {
            continue;
        }

        for (neighbor, edge_type) in neighbors_iter(edge_store, current, config) {
            if !candidate_allowed(
                node_store,
                filter_index,
                config,
                neighbor,
                edge_type,
                &visited,
                has_filters,
            ) {
                continue;
            }

            visited.insert(neighbor);
            depth_map.set(neighbor, current_depth + 1);
            parent.set(neighbor, current);
            parent_edge_type.set(neighbor, edge_type);
            nodes_visited += 1;

            // Circuit breakers
            if nodes_visited >= config.max_nodes {
                return BfsResult {
                    visited,
                    depth: depth_map,
                    parent,
                    parent_edge_type,
                };
            }

            // Only add to frontier if we haven't hit the depth limit
            if current_depth + 1 < config.max_depth {
                frontier.push_back(neighbor);

                // Frontier size circuit breaker
                if frontier.len() as u32 >= config.max_frontier {
                    return BfsResult {
                        visited,
                        depth: depth_map,
                        parent,
                        parent_edge_type,
                    };
                }
            }
        }
    }

    BfsResult {
        visited,
        depth: depth_map,
        parent,
        parent_edge_type,
    }
}

/// Execute depth-first traversal from a seed node.
pub fn execute_dfs(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
) -> BfsResult {
    let node_count = node_store.node_count() as usize;
    let sparse_metadata = use_sparse_metadata(node_count, config.max_nodes);
    let expected_visits = expected_visit_capacity(node_count, config.max_nodes);

    let mut visited = RoaringBitmap::new();
    let mut depth_map = TraversalDepthMap::new(node_count, sparse_metadata, expected_visits);
    let mut parent = TraversalParentMap::new(node_count, sparse_metadata, expected_visits);
    let mut parent_edge_type =
        TraversalParentEdgeTypes::new(node_count, sparse_metadata, expected_visits);
    if config.seed_node as usize >= node_count {
        return BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
        };
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
        return BfsResult {
            visited,
            depth: depth_map,
            parent,
            parent_edge_type,
        };
    }

    let mut stack = Vec::with_capacity(config.max_frontier as usize);
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
            edge_store,
            filter_index,
            config,
            visited: &mut visited,
            depth_map: &mut depth_map,
            parent: &mut parent,
            parent_edge_type: &mut parent_edge_type,
            stack: &mut stack,
            nodes_visited: &mut nodes_visited,
            has_filters,
        };
        if push.push_neighbors(current, current_depth) {
            return BfsResult {
                visited,
                depth: depth_map,
                parent,
                parent_edge_type,
            };
        }
    }

    BfsResult {
        visited,
        depth: depth_map,
        parent,
        parent_edge_type,
    }
}

struct NeighborIter<'a> {
    targets: &'a [u32],
    type_ids: &'a [u8],
    deleted: Option<&'a HashSet<(u32, u8)>>,
    inserted: Option<&'a [(u32, u8)]>,
    base_pos: usize,
    insert_pos: usize,
}

impl<'a> NeighborIter<'a> {
    fn new(edge_store: &'a EdgeStore, node_idx: u32, config: &'a BfsConfig) -> Self {
        let (targets, type_ids) = edge_store.neighbors(node_idx);
        Self {
            targets,
            type_ids,
            deleted: config.overlay_deleted_edges.get(&node_idx),
            inserted: config
                .overlay_insert_edges
                .get(&node_idx)
                .map(Vec::as_slice),
            base_pos: 0,
            insert_pos: 0,
        }
    }

    fn base_contains(&self, target: u32, type_id: u8) -> bool {
        self.targets
            .iter()
            .zip(self.type_ids.iter())
            .any(|(&base_target, &base_type)| base_target == target && base_type == type_id)
    }
}

impl Iterator for NeighborIter<'_> {
    type Item = (u32, u8);

    fn next(&mut self) -> Option<Self::Item> {
        while self.base_pos < self.targets.len() {
            let pos = self.base_pos;
            self.base_pos += 1;
            let target = self.targets[pos];
            let type_id = self.type_ids[pos];
            if self
                .deleted
                .is_some_and(|deleted| deleted.contains(&(target, type_id)))
            {
                continue;
            }
            return Some((target, type_id));
        }

        let inserted = self.inserted?;
        while self.insert_pos < inserted.len() {
            let (target, type_id) = inserted[self.insert_pos];
            self.insert_pos += 1;
            if self.base_contains(target, type_id) {
                continue;
            }
            return Some((target, type_id));
        }

        None
    }
}

fn neighbors_iter<'a>(
    edge_store: &'a EdgeStore,
    node_idx: u32,
    config: &'a BfsConfig,
) -> NeighborIter<'a> {
    NeighborIter::new(edge_store, node_idx, config)
}

fn candidate_allowed(
    node_store: &NodeStore,
    filter_index: &FilterIndex,
    config: &BfsConfig,
    neighbor: u32,
    edge_type: u8,
    visited: &RoaringBitmap,
    has_filters: bool,
) -> bool {
    if visited.contains(neighbor) {
        return false;
    }
    if let crate::types::EdgeTypeFilter::Only(ref allowed) = config.edge_type_filter {
        if !allowed.contains(&edge_type) {
            return false;
        }
    }
    if !node_store.is_active(neighbor) {
        return false;
    }
    if let Some(tenant) = config.tenant.as_deref() {
        let table_oid = node_store.table_oid(neighbor);
        if config.tenanted_table_oids.contains(&table_oid)
            && !config
                .tenant_membership
                .get(tenant)
                .is_some_and(|bitmap| bitmap.contains(neighbor))
        {
            return false;
        }
    }
    if has_filters && !filter_index.check_filters(neighbor, &config.filter_ops) {
        return false;
    }
    true
}

fn base_contains(targets: &[u32], type_ids: &[u8], target: u32, type_id: u8) -> bool {
    targets
        .iter()
        .zip(type_ids.iter())
        .any(|(&base_target, &base_type)| base_target == target && base_type == type_id)
}

struct DfsPushContext<'a> {
    node_store: &'a NodeStore,
    edge_store: &'a EdgeStore,
    filter_index: &'a FilterIndex,
    config: &'a BfsConfig,
    visited: &'a mut RoaringBitmap,
    depth_map: &'a mut TraversalDepthMap,
    parent: &'a mut TraversalParentMap,
    parent_edge_type: &'a mut TraversalParentEdgeTypes,
    stack: &'a mut Vec<u32>,
    nodes_visited: &'a mut u32,
    has_filters: bool,
}

impl DfsPushContext<'_> {
    fn push_neighbors(&mut self, current: u32, current_depth: i32) -> bool {
        let (targets, type_ids) = self.edge_store.neighbors(current);

        if let Some(inserted) = self.config.overlay_insert_edges.get(&current) {
            for insert_pos in (0..inserted.len()).rev() {
                let (neighbor, edge_type) = inserted[insert_pos];
                if base_contains(targets, type_ids, neighbor, edge_type)
                    || inserted[..insert_pos].contains(&(neighbor, edge_type))
                    || self.push_candidate(current, current_depth, neighbor, edge_type)
                {
                    continue;
                }
                return true;
            }
        }

        for (&neighbor, &edge_type) in targets.iter().zip(type_ids.iter()).rev() {
            if self
                .config
                .overlay_deleted_edges
                .get(&current)
                .is_some_and(|deleted| deleted.contains(&(neighbor, edge_type)))
                || self.push_candidate(current, current_depth, neighbor, edge_type)
            {
                continue;
            }
            return true;
        }

        false
    }

    fn push_candidate(
        &mut self,
        current: u32,
        current_depth: i32,
        neighbor: u32,
        edge_type: u8,
    ) -> bool {
        if !candidate_allowed(
            self.node_store,
            self.filter_index,
            self.config,
            neighbor,
            edge_type,
            self.visited,
            self.has_filters,
        ) {
            return true;
        }

        self.visited.insert(neighbor);
        self.depth_map.set(neighbor, current_depth + 1);
        self.parent.set(neighbor, current);
        self.parent_edge_type.set(neighbor, edge_type);
        *self.nodes_visited += 1;

        if *self.nodes_visited >= self.config.max_nodes {
            return false;
        }

        if current_depth + 1 < self.config.max_depth {
            self.stack.push(neighbor);
            if self.stack.len() as u32 >= self.config.max_frontier {
                return false;
            }
        }

        true
    }
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

/// Convert BFS results into TraversalResult structs for SQL output.
pub fn to_traversal_results(
    bfs_result: &BfsResult,
    node_store: &NodeStore,
    edge_type_registry: &[String],
) -> Vec<TraversalResult> {
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
            .map(|&idx| PathCoordinate {
                table_oid: TableOid(node_store.table_oid(idx)),
                node_id: node_store.primary_key(idx).to_string(),
            })
            .collect();
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

        results.push(TraversalResult {
            node_table: TableOid(node_store.table_oid(node_idx)),
            node_id: node_store.primary_key(node_idx).to_string(),
            depth,
            path,
            edge_path,
        });
    }

    // Sort by depth for consistent output
    results.sort_by_key(|r| r.depth);
    results
}

#[cfg(test)]
mod tests {
    //! Covers breadth-first traversal semantics, including depth limits,
    //! directionality, active-node filtering, and edge-type constraints.

    use super::*;
    use crate::edge_store::RawEdge;
    use std::collections::HashSet;

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
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 3,
                target: 2,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 4,
                type_id: 2,
                weight: None,
            },
            RawEdge {
                source: 4,
                target: 0,
                type_id: 2,
                weight: None,
            },
        ];
        let es = EdgeStore::from_edges(5, edges, false);
        (ns, es)
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
        overlay_insert_edges.insert(0, vec![(3, 1), (1, 1)]);
        let mut overlay_deleted_edges = std::collections::HashMap::new();
        overlay_deleted_edges.insert(0, HashSet::from([(1, 1)]));

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
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: 1,
                    weight: None,
                },
            ],
            false,
        );
        let fi = FilterIndex::new();
        let mut overlay_insert_edges = std::collections::HashMap::new();
        overlay_insert_edges.insert(0, vec![(3, 1), (2, 1), (3, 1)]);

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
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert!(result.visited.len() <= 2);
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
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: 1,
                weight: None,
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
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        assert_eq!(result.visited.len(), 1); // Just the seed
        assert!(result.visited.contains(0));
    }

    #[test]
    fn path_reconstruction_on_isolated_returns_just_seed() {
        let mut result_parent = TraversalParentMap::new(1, false, 1);
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
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let bfs_result = execute(&ns, &es, &fi, &config);
        let edge_type_registry = vec!["".to_string(), "test".to_string()];
        let results = to_traversal_results(&bfs_result, &ns, &edge_type_registry);

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
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 3,
                target: 2,
                type_id: 1,
                weight: None,
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
            overlay_insert_edges: std::collections::HashMap::new(),
            overlay_deleted_edges: std::collections::HashMap::new(),
        };

        let result = execute(&ns, &es, &fi, &config);
        // Seed is inserted at depth 0, which is >= max_depth(-1), so no expansion
        assert_eq!(result.visited.len(), 1);
        assert!(result.visited.contains(0));
    }
}
