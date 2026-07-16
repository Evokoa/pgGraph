//! # PathFinder — Shortest path algorithms
//!
//! Bidirectional BFS for unweighted shortest path.
//! Dijkstra (BinaryHeap) for weighted shortest path.
//!
//! See: `docs/contributor_guide/traversal-search-paths.mdx`

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};

use roaring::RoaringBitmap;

#[cfg(test)]
use crate::edge_store::EdgeStore;
use crate::node_store::NodeStore;
#[cfg(test)]
use crate::projection::neighbors::CsrNeighbors;
use crate::projection::neighbors::{NeighborSource, WeightedNeighborSource};
use crate::resource::{ResourceGovernor, ResourceLimitError, ResourcePhase, WorkUnits};
use crate::safety::GraphResult;
use crate::types::{PathStep, TableOid, WeightedPathStep};

#[derive(Debug, Clone, Copy)]
struct ParentStep {
    parent: u32,
    edge_type: u8,
}

#[derive(Debug, Clone, Copy)]
struct WeightedParentStep {
    parent: u32,
    edge_type: u8,
    edge_weight: u32,
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
        },
        None,
    )
}

pub(crate) struct UnweightedPathRequest<'a> {
    pub(crate) source: u32,
    pub(crate) target: u32,
    pub(crate) max_depth: i32,
    pub(crate) has_unidirectional_edges: bool,
    pub(crate) edge_type_registry: &'a [String],
}

/// Find an unweighted path while enforcing expansion and elapsed-time limits.
pub(crate) fn shortest_path_with_neighbors_governed(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    request: UnweightedPathRequest<'_>,
    governor: &ResourceGovernor,
) -> GraphResult<Option<Vec<PathStep>>> {
    let budget = PathWorkBudget::new(governor);
    let result = shortest_path_with_neighbors_inner(node_store, neighbors, request, Some(&budget));
    budget.finish(result)
}

fn shortest_path_with_neighbors_inner(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    request: UnweightedPathRequest<'_>,
    budget: Option<&PathWorkBudget<'_>>,
) -> Option<Vec<PathStep>> {
    let UnweightedPathRequest {
        source,
        target,
        max_depth,
        has_unidirectional_edges,
        edge_type_registry,
    } = request;
    if source >= node_store.node_count() || target >= node_store.node_count() {
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
            budget,
        );
    }

    bidirectional_bfs(
        node_store,
        neighbors,
        source,
        target,
        max_depth,
        edge_type_registry,
        budget,
    )
}

fn bidirectional_bfs(
    node_store: &NodeStore,
    neighbors: &impl NeighborSource,
    source: u32,
    target: u32,
    max_depth: i32,
    edge_type_registry: &[String],
    budget: Option<&PathWorkBudget<'_>>,
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
            edge_type: 0,
        },
    );
    bwd_parent.insert(
        target,
        ParentStep {
            parent: target,
            edge_type: 0,
        },
    );
    fwd_frontier.push_back(source);
    bwd_frontier.push_back(target);

    let mut meeting_node: Option<u32> = None;
    let mut depth = 0;

    while !fwd_frontier.is_empty() && !bwd_frontier.is_empty() && depth < max_depth {
        // Expand the smaller frontier
        if fwd_frontier.len() <= bwd_frontier.len() {
            let level_size = fwd_frontier.len();
            for _ in 0..level_size {
                let Some(current) = fwd_frontier.pop_front() else {
                    break;
                };
                for edge in neighbors.neighbors(current) {
                    if !consume_path_work(budget) {
                        return None;
                    }
                    if !node_store.is_active(edge.target)
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
                            },
                        );
                        fwd_frontier.push_back(edge.target);
                    }
                    if bwd_visited.contains(edge.target) {
                        fwd_parent.entry(edge.target).or_insert(ParentStep {
                            parent: current,
                            edge_type: edge.type_id,
                        });
                        meeting_node = Some(edge.target);
                        break;
                    }
                }
                if meeting_node.is_some() {
                    break;
                }
            }
        } else {
            let level_size = bwd_frontier.len();
            for _ in 0..level_size {
                let Some(current) = bwd_frontier.pop_front() else {
                    break;
                };
                for edge in neighbors.neighbors(current) {
                    if !consume_path_work(budget) {
                        return None;
                    }
                    if !node_store.is_active(edge.target)
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
                            },
                        );
                        bwd_frontier.push_back(edge.target);
                    }
                    if fwd_visited.contains(edge.target) {
                        bwd_parent.entry(edge.target).or_insert(ParentStep {
                            parent: current,
                            edge_type: edge.type_id,
                        });
                        meeting_node = Some(edge.target);
                        break;
                    }
                }
                if meeting_node.is_some() {
                    break;
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
    fwd_path.push((source, 0));
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
                Some(
                    edge_type_registry
                        .get(edge_type as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("type_{}", edge_type)),
                )
            },
        });
    }
    let offset = fwd_path.len();
    for (i, &(node, edge_type)) in bwd_path.iter().enumerate() {
        steps.push(PathStep {
            step: (offset + i) as i32,
            node_table: TableOid(node_store.table_oid(node)?),
            node_id: node_store.primary_key(node)?.to_string(),
            edge_label: Some(
                edge_type_registry
                    .get(edge_type as usize)
                    .cloned()
                    .unwrap_or_else(|| format!("type_{}", edge_type)),
            ),
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
    budget: Option<&PathWorkBudget<'_>>,
) -> Option<Vec<PathStep>> {
    let mut visited = RoaringBitmap::new();
    let mut parent = HashMap::new();
    let mut frontier: VecDeque<(u32, i32)> = VecDeque::new();

    visited.insert(source);
    parent.insert(
        source,
        ParentStep {
            parent: source,
            edge_type: 0,
        },
    );
    frontier.push_back((source, 0));

    while let Some((current, current_depth)) = frontier.pop_front() {
        if current_depth >= max_depth {
            continue;
        }

        for edge in neighbors.neighbors(current) {
            if !consume_path_work(budget) {
                return None;
            }
            if visited.contains(edge.target)
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
                                Some(
                                    edge_type_registry
                                        .get(et as usize)
                                        .cloned()
                                        .unwrap_or_else(|| format!("type_{}", et)),
                                )
                            },
                        })
                    })
                    .collect();
            }
        }
    }

    None // No path found
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
    )
}

/// Run Dijkstra while enforcing expansion and elapsed-time limits.
pub(crate) fn weighted_shortest_path_with_neighbors_governed(
    node_store: &NodeStore,
    neighbors: &impl WeightedNeighborSource,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
    governor: &ResourceGovernor,
) -> GraphResult<Option<Vec<WeightedPathStep>>> {
    let budget = PathWorkBudget::new(governor);
    let result = weighted_shortest_path_with_neighbors_inner(
        node_store,
        neighbors,
        source,
        target,
        edge_type_registry,
        Some(&budget),
    );
    budget.finish(result)
}

fn weighted_shortest_path_with_neighbors_inner(
    node_store: &NodeStore,
    neighbors: &impl WeightedNeighborSource,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
    budget: Option<&PathWorkBudget<'_>>,
) -> Option<Vec<WeightedPathStep>> {
    if source >= node_store.node_count() || target >= node_store.node_count() {
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
            edge_type: 0,
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
                edge_type: 0,
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
                            .get(parent_step.edge_type as usize)
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
    error: std::cell::Cell<Option<ResourceLimitError>>,
}

impl<'a> PathWorkBudget<'a> {
    fn new(governor: &'a ResourceGovernor) -> Self {
        Self {
            governor,
            error: std::cell::Cell::new(None),
        }
    }

    fn finish<T>(&self, result: Option<T>) -> GraphResult<Option<T>> {
        match self.error.get() {
            Some(error) => Err(crate::safety::resource_limit_error(error)),
            None => Ok(result),
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
        budget.error.set(Some(error));
        return false;
    }
    if budget.governor.work_used().as_u64().is_multiple_of(1_024) {
        crate::resource::check_postgres_interrupts();
        if let Err(error) = budget.governor.check_elapsed(ResourcePhase::QueryPaths) {
            budget.error.set(Some(error));
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
    use crate::edge_store::RawEdge;
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
                    target: 3,
                    type_id: 1,
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
                type_id: 1,
                weight: Some(100),
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 3,
                type_id: 1,
                weight: Some(1),
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: Some(5),
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: 1,
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
                type_id: 1,
                weight: Some(u32::MAX - 1),
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
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
                type_id: 1,
                weight: None,
                schema_reversed: false,
            });
            edges.push(RawEdge {
                source: i + 1,
                target: i,
                type_id: 1,
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
            type_id: 1,
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
                source: 0,
                target: 2,
                type_id: 1,
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
                target: 0,
                type_id: 1,
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
