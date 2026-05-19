//! # PathFinder — Shortest path algorithms
//!
//! Bidirectional BFS for unweighted shortest path.
//! Dijkstra (BinaryHeap) for weighted shortest path.
//!
//! See: `docs/contributor_guide/traversal-search-paths.mdx`

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

use roaring::RoaringBitmap;

use crate::edge_store::EdgeStore;
use crate::node_store::NodeStore;
use crate::types::{PathStep, TableOid, WeightedPathStep};

/// Find the shortest unweighted path between two nodes using bidirectional BFS.
///
/// Returns `None` if no path exists.
/// Falls back to single-direction BFS if the graph has unidirectional edges.
pub fn shortest_path(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    source: u32,
    target: u32,
    max_depth: i32,
    has_unidirectional_edges: bool,
    edge_type_registry: &[String],
) -> Option<Vec<PathStep>> {
    if source >= node_store.node_count() || target >= node_store.node_count() {
        return None;
    }

    if source == target {
        return Some(vec![PathStep {
            step: 0,
            node_table: TableOid(node_store.table_oid(source)),
            node_id: node_store.primary_key(source).to_string(),
            edge_label: None,
        }]);
    }

    if has_unidirectional_edges {
        return single_direction_bfs(
            node_store,
            edge_store,
            source,
            target,
            max_depth,
            edge_type_registry,
        );
    }

    bidirectional_bfs(
        node_store,
        edge_store,
        source,
        target,
        max_depth,
        edge_type_registry,
    )
}

fn bidirectional_bfs(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    source: u32,
    target: u32,
    max_depth: i32,
    edge_type_registry: &[String],
) -> Option<Vec<PathStep>> {
    let mut fwd_visited = RoaringBitmap::new();
    let mut bwd_visited = RoaringBitmap::new();
    let mut fwd_parent = vec![u32::MAX; node_store.node_count() as usize];
    let mut bwd_parent = vec![u32::MAX; node_store.node_count() as usize];
    let mut fwd_edge_type = vec![0u8; node_store.node_count() as usize];
    let mut bwd_edge_type = vec![0u8; node_store.node_count() as usize];
    let mut fwd_frontier: VecDeque<u32> = VecDeque::new();
    let mut bwd_frontier: VecDeque<u32> = VecDeque::new();

    fwd_visited.insert(source);
    bwd_visited.insert(target);
    fwd_parent[source as usize] = source;
    bwd_parent[target as usize] = target;
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
                let (targets, type_ids) = edge_store.neighbors(current);
                for i in 0..targets.len() {
                    let neighbor = targets[i];
                    if !node_store.is_active(neighbor) {
                        continue;
                    }
                    if !fwd_visited.contains(neighbor) {
                        fwd_visited.insert(neighbor);
                        fwd_parent[neighbor as usize] = current;
                        fwd_edge_type[neighbor as usize] = type_ids[i];
                        fwd_frontier.push_back(neighbor);
                    }
                    if bwd_visited.contains(neighbor) {
                        if fwd_parent[neighbor as usize] == u32::MAX {
                            fwd_parent[neighbor as usize] = current;
                            fwd_edge_type[neighbor as usize] = type_ids[i];
                        }
                        meeting_node = Some(neighbor);
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
                let (targets, type_ids) = edge_store.neighbors(current);
                for i in 0..targets.len() {
                    let neighbor = targets[i];
                    if !node_store.is_active(neighbor) {
                        continue;
                    }
                    if !bwd_visited.contains(neighbor) {
                        bwd_visited.insert(neighbor);
                        bwd_parent[neighbor as usize] = current;
                        bwd_edge_type[neighbor as usize] = type_ids[i];
                        bwd_frontier.push_back(neighbor);
                    }
                    if fwd_visited.contains(neighbor) {
                        if bwd_parent[neighbor as usize] == u32::MAX {
                            bwd_parent[neighbor as usize] = current;
                            bwd_edge_type[neighbor as usize] = type_ids[i];
                        }
                        meeting_node = Some(neighbor);
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
        fwd_path.push((current, fwd_edge_type[current as usize]));
        current = fwd_parent[current as usize];
        if current == u32::MAX {
            return None;
        }
    }
    fwd_path.push((source, 0));
    fwd_path.reverse();

    let mut bwd_path = Vec::new();
    let mut child = meet;
    current = bwd_parent[meet as usize];
    if current != u32::MAX && current != meet {
        loop {
            bwd_path.push((current, bwd_edge_type[child as usize]));
            if current == target {
                break;
            }
            child = current;
            current = bwd_parent[current as usize];
            if current == u32::MAX {
                return None;
            }
        }
    }

    // Combine into PathStep sequence
    let mut steps = Vec::new();
    for (i, &(node, edge_type)) in fwd_path.iter().enumerate() {
        steps.push(PathStep {
            step: i as i32,
            node_table: TableOid(node_store.table_oid(node)),
            node_id: node_store.primary_key(node).to_string(),
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
            node_table: TableOid(node_store.table_oid(node)),
            node_id: node_store.primary_key(node).to_string(),
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
    edge_store: &EdgeStore,
    source: u32,
    target: u32,
    max_depth: i32,
    edge_type_registry: &[String],
) -> Option<Vec<PathStep>> {
    let mut visited = RoaringBitmap::new();
    let mut parent = vec![u32::MAX; node_store.node_count() as usize];
    let mut edge_types_used = vec![0u8; node_store.node_count() as usize];
    let mut frontier: VecDeque<u32> = VecDeque::new();
    let mut depth_map = vec![-1i32; node_store.node_count() as usize];

    visited.insert(source);
    parent[source as usize] = source;
    depth_map[source as usize] = 0;
    frontier.push_back(source);

    while let Some(current) = frontier.pop_front() {
        let current_depth = depth_map[current as usize];
        if current_depth >= max_depth {
            continue;
        }

        let (targets, type_ids) = edge_store.neighbors(current);
        for i in 0..targets.len() {
            let neighbor = targets[i];
            if visited.contains(neighbor) || !node_store.is_active(neighbor) {
                continue;
            }

            visited.insert(neighbor);
            parent[neighbor as usize] = current;
            edge_types_used[neighbor as usize] = type_ids[i];
            depth_map[neighbor as usize] = current_depth + 1;
            frontier.push_back(neighbor);

            if neighbor == target {
                // Found target — reconstruct path
                let mut path = Vec::new();
                let mut cur = target;
                loop {
                    path.push((cur, edge_types_used[cur as usize]));
                    if cur == source {
                        break;
                    }
                    cur = parent[cur as usize];
                }
                path.reverse();

                return Some(
                    path.iter()
                        .enumerate()
                        .map(|(i, &(node, et))| PathStep {
                            step: i as i32,
                            node_table: TableOid(node_store.table_oid(node)),
                            node_id: node_store.primary_key(node).to_string(),
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
                        .collect(),
                );
            }
        }
    }

    None // No path found
}

/// Dijkstra's algorithm for weighted shortest path.
///
/// Uses `BinaryHeap<Reverse<(cost, node)>>` for O((V + E) log V).
pub fn weighted_shortest_path(
    node_store: &NodeStore,
    edge_store: &EdgeStore,
    source: u32,
    target: u32,
    edge_type_registry: &[String],
) -> Option<Vec<WeightedPathStep>> {
    if source >= node_store.node_count() || target >= node_store.node_count() {
        return None;
    }

    if !edge_store.has_weights() {
        return None;
    }

    let node_count = node_store.node_count() as usize;
    let mut dist = vec![u64::MAX; node_count];
    let mut parent = vec![u32::MAX; node_count];
    let mut parent_edge_type = vec![0u8; node_count];
    let mut parent_edge_weight = vec![0u32; node_count];
    let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();

    dist[source as usize] = 0;
    parent[source as usize] = source;
    heap.push(Reverse((0, source)));

    while let Some(Reverse((cost, current))) = heap.pop() {
        if current == target {
            break;
        }
        if cost > dist[current as usize] {
            continue; // Stale entry
        }

        let (targets, type_ids, weights) = edge_store.neighbors_weighted(current);
        for i in 0..targets.len() {
            let neighbor = targets[i];
            let edge_weight = weights[i];
            let edge_cost = u64::from(edge_weight);
            let Some(new_cost) = cost.checked_add(edge_cost) else {
                continue;
            };

            if new_cost < dist[neighbor as usize] && node_store.is_active(neighbor) {
                dist[neighbor as usize] = new_cost;
                parent[neighbor as usize] = current;
                parent_edge_type[neighbor as usize] = type_ids[i];
                parent_edge_weight[neighbor as usize] = edge_weight;
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
        current = parent[current as usize];
    }
    nodes.reverse();

    Some(
        nodes
            .into_iter()
            .enumerate()
            .map(|(step, node)| {
                let edge_type = parent_edge_type[node as usize];
                WeightedPathStep {
                    step: step as i32,
                    node_table: TableOid(node_store.table_oid(node)),
                    node_id: node_store.primary_key(node).to_string(),
                    edge_label: if step == 0 {
                        None
                    } else {
                        Some(
                            edge_type_registry
                                .get(edge_type as usize)
                                .cloned()
                                .unwrap_or_else(|| format!("type_{}", edge_type)),
                        )
                    },
                    edge_weight: (step != 0).then_some(parent_edge_weight[node as usize]),
                    step_cost: dist[node as usize],
                    total_cost,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    //! Covers unweighted and weighted path-finding behavior, including directed
    //! edge semantics and unreachable-node invariants.

    use super::*;
    use crate::edge_store::RawEdge;

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
            },
            RawEdge {
                source: 1,
                target: 3,
                type_id: 1,
                weight: Some(1),
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: Some(5),
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: 1,
                weight: Some(5),
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
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: Some(1),
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
            });
            edges.push(RawEdge {
                source: i + 1,
                target: i,
                type_id: 1,
                weight: None,
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
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
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
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: None,
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
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 0,
                type_id: 1,
                weight: None,
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
