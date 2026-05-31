//! Physical GQL plan execution against engine stores and edge overlays.

use crate::edge_store::EdgeStore;
use crate::engine::Engine;
use crate::projection::neighbors::EdgeOverlay;
use crate::projection::neighbors::{CsrNeighbors, NeighborSource, OverlayNeighbors};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

use super::logical_plan::BoundDirection;
use super::physical_plan::{PhysicalNodeScan, PhysicalPlan, ReturnSlot};

/// Coordinate-only node value returned by Phase 1B.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GqlNodeCoordinate {
    /// Backing source table OID.
    pub(crate) table_oid: u32,
    /// Source row primary-key text.
    pub(crate) node_id: String,
}

/// One GQL result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GqlRow {
    /// Source coordinate.
    pub(crate) source: GqlNodeCoordinate,
    /// Target coordinate.
    pub(crate) target: GqlNodeCoordinate,
    /// Relationship start coordinate in the registered edge direction.
    pub(crate) rel_start: GqlNodeCoordinate,
    /// Relationship end coordinate in the registered edge direction.
    pub(crate) rel_end: GqlNodeCoordinate,
}

/// One GQL node-only result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GqlNodeRow {
    /// Node coordinate.
    pub(crate) node: GqlNodeCoordinate,
}

/// Execute a physical one-hop plan.
///
/// # Errors
///
/// Returns [`GraphError`] when the graph is not built, the requested
/// relationship type is not present in the built engine registry, or execution
/// exceeds the plan's cardinality cap.
pub(crate) fn execute(
    engine: &Engine,
    plan: &PhysicalPlan,
    tenant: Option<&str>,
) -> GraphResult<Vec<GqlRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
    let rel_type_id = edge_type_id(engine, &plan.rel_type)?;
    let neighbors = GqlNeighbors::new(engine);
    let mut rows = Vec::new();
    let row_cap = plan.execution_row_cap();
    for source_idx in source_nodes(engine, plan.source_table_oid, tenant) {
        if !engine.node_store.is_active(source_idx) {
            continue;
        }
        for target in expand_targets(&neighbors, engine, plan, source_idx, rel_type_id, tenant) {
            if rows.len() >= row_cap {
                if plan.cap_exhaustion_is_error() {
                    return Err(GraphError::GqlExecution {
                        reason: format!("GQL result row cap exceeded ({row_cap})"),
                    });
                }
                return Ok(rows);
            }
            rows.push(project_row(engine, source_idx, target));
        }
    }
    Ok(rows)
}

/// Execute a physical node-only scan.
///
/// # Errors
///
/// Returns [`GraphError`] when the graph is not built or execution exceeds the
/// plan's cardinality cap.
pub(crate) fn execute_node_scan(
    engine: &Engine,
    plan: &PhysicalNodeScan,
    tenant: Option<&str>,
) -> GraphResult<Vec<GqlNodeRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
    let mut rows = Vec::new();
    let row_cap = plan.execution_row_cap();
    let mut seen = std::collections::HashSet::new();
    for node_idx in source_nodes(engine, plan.table_oid, tenant) {
        if !engine.node_store.is_active(node_idx) {
            continue;
        }
        let node_id = engine.node_store.primary_key(node_idx).to_string();
        if seen.insert(node_id.clone()) {
            if rows.len() >= row_cap {
                if plan.cap_exhaustion_is_error() {
                    return Err(GraphError::GqlExecution {
                        reason: format!("GQL result row cap exceeded ({row_cap})"),
                    });
                }
                return Ok(rows);
            }
            rows.push(GqlNodeRow {
                node: GqlNodeCoordinate {
                    table_oid: plan.table_oid,
                    node_id,
                },
            });
        }
    }
    for node_id in crate::projection::tx_delta::added_node_keys(plan.table_oid, tenant) {
        if seen.insert(node_id.clone()) {
            if rows.len() >= row_cap {
                if plan.cap_exhaustion_is_error() {
                    return Err(GraphError::GqlExecution {
                        reason: format!("GQL result row cap exceeded ({row_cap})"),
                    });
                }
                return Ok(rows);
            }
            rows.push(GqlNodeRow {
                node: GqlNodeCoordinate {
                    table_oid: plan.table_oid,
                    node_id,
                },
            });
        }
    }
    Ok(rows)
}

fn edge_type_id(engine: &Engine, rel_type: &str) -> GraphResult<u8> {
    engine
        .edge_type_registry
        .iter()
        .position(|label| label == rel_type)
        .map(|idx| idx as u8)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("relationship type `{rel_type}` is not present in the built graph"),
        })
}

fn source_nodes(engine: &Engine, table_oid: u32, tenant: Option<&str>) -> Vec<u32> {
    let nodes: Vec<u32> = if let Some(nodes) = engine.table_membership.get(&table_oid) {
        nodes.iter().collect()
    } else {
        (0..engine.node_store.node_count())
            .filter(|&idx| engine.node_store.table_oid(idx) == table_oid)
            .collect()
    };
    nodes
        .into_iter()
        .filter(|&idx| tenant_allows_node(engine, idx, tenant))
        .collect()
}

fn expand_targets(
    neighbors: &GqlNeighbors<'_>,
    engine: &Engine,
    plan: &PhysicalPlan,
    source_idx: u32,
    rel_type_id: u8,
    tenant: Option<&str>,
) -> Vec<GqlTarget> {
    let mut current = vec![source_idx];
    let mut results = Vec::new();
    let returns_relationship = plan
        .returns
        .iter()
        .any(|slot| matches!(slot, ReturnSlot::Relationship { .. }));
    let mut seen_result_nodes = std::collections::HashSet::new();
    let mut seen_result_relationships = std::collections::HashSet::new();
    let mut seen_frontier = std::collections::HashSet::from([source_idx]);
    for depth in 1..=plan.hops.max {
        let mut next = Vec::new();
        let mut seen_next = std::collections::HashSet::new();
        for node_idx in current {
            for target in neighbors.for_direction(plan.direction, node_idx, rel_type_id) {
                if !engine.node_store.is_active(target.node_idx)
                    || !tenant_allows_node(engine, target.node_idx, tenant)
                {
                    continue;
                }
                if depth >= plan.hops.min
                    && target_matches(engine, target.node_idx, plan.target_table_oid, tenant)
                    && if returns_relationship {
                        seen_result_relationships.insert((target.node_idx, target.orientation))
                    } else {
                        seen_result_nodes.insert(target.node_idx)
                    }
                {
                    results.push(target);
                }
                if depth < plan.hops.max
                    && seen_frontier.insert(target.node_idx)
                    && seen_next.insert(target.node_idx)
                {
                    next.push(target.node_idx);
                }
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    results
}

struct GqlNeighbors<'a> {
    out_store: &'a EdgeStore,
    in_store: &'a EdgeStore,
    out_overlay: Option<EdgeOverlay>,
    in_overlay: Option<EdgeOverlay>,
}

impl<'a> GqlNeighbors<'a> {
    fn new(engine: &'a Engine) -> Self {
        let (out_overlay, in_overlay) = if engine.has_edge_overlay() {
            (
                Some(engine.traversal_edge_overlay(TraversalDirection::Out)),
                Some(engine.traversal_edge_overlay(TraversalDirection::In)),
            )
        } else {
            (None, None)
        };

        Self {
            out_store: &engine.edge_store,
            in_store: &engine.reverse_edge_store,
            out_overlay,
            in_overlay,
        }
    }

    fn for_direction(
        &self,
        direction: BoundDirection,
        node_idx: u32,
        rel_type_id: u8,
    ) -> Vec<GqlTarget> {
        let mut neighbors = Vec::new();
        if matches!(direction, BoundDirection::Out | BoundDirection::Undirected) {
            self.append_direction_neighbors(
                TraversalDirection::Out,
                node_idx,
                rel_type_id,
                EdgeOrientation::Forward,
                &mut neighbors,
            );
        }
        if matches!(direction, BoundDirection::In | BoundDirection::Undirected) {
            self.append_direction_neighbors(
                TraversalDirection::In,
                node_idx,
                rel_type_id,
                EdgeOrientation::Reverse,
                &mut neighbors,
            );
        }
        neighbors.sort_by_key(|target| (target.node_idx, target.orientation));
        neighbors.dedup();
        neighbors
    }

    fn append_direction_neighbors(
        &self,
        direction: TraversalDirection,
        node_idx: u32,
        rel_type_id: u8,
        orientation: EdgeOrientation,
        out: &mut Vec<GqlTarget>,
    ) {
        let (edge_store, overlay) = match direction {
            TraversalDirection::Any | TraversalDirection::Out => {
                (self.out_store, self.out_overlay.as_ref())
            }
            TraversalDirection::In => (self.in_store, self.in_overlay.as_ref()),
        };
        let Some((inserts, deletes)) = overlay else {
            let neighbors = CsrNeighbors::new(edge_store);
            append_matching_neighbors(&neighbors, node_idx, rel_type_id, orientation, out);
            return;
        };
        let neighbors = OverlayNeighbors::new(edge_store, inserts, deletes);
        append_matching_neighbors(&neighbors, node_idx, rel_type_id, orientation, out);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GqlTarget {
    node_idx: u32,
    orientation: EdgeOrientation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum EdgeOrientation {
    Forward,
    Reverse,
}

fn append_matching_neighbors(
    source: &impl NeighborSource,
    node_idx: u32,
    rel_type_id: u8,
    orientation: EdgeOrientation,
    out: &mut Vec<GqlTarget>,
) {
    out.extend(source.neighbors(node_idx).filter_map(|neighbor| {
        (neighbor.type_id == rel_type_id).then_some(GqlTarget {
            node_idx: neighbor.target,
            orientation,
        })
    }));
}

fn target_matches(engine: &Engine, target_idx: u32, table_oid: u32, tenant: Option<&str>) -> bool {
    target_idx < engine.node_store.node_count()
        && engine.node_store.is_active(target_idx)
        && engine.node_store.table_oid(target_idx) == table_oid
        && tenant_allows_node(engine, target_idx, tenant)
}

fn tenant_allows_node(engine: &Engine, node_idx: u32, tenant: Option<&str>) -> bool {
    let Some(tenant) = tenant else {
        return true;
    };
    let table_oid = engine.node_store.table_oid(node_idx);
    !engine.tenanted_table_oids.contains(&table_oid)
        || engine
            .tenant_membership
            .get(tenant)
            .is_some_and(|bitmap| bitmap.contains(node_idx))
}

fn project_row(engine: &Engine, source_idx: u32, target: GqlTarget) -> GqlRow {
    let target_idx = target.node_idx;
    let (rel_start_idx, rel_end_idx) = match target.orientation {
        EdgeOrientation::Forward => (source_idx, target_idx),
        EdgeOrientation::Reverse => (target_idx, source_idx),
    };
    GqlRow {
        source: coordinate(engine, source_idx),
        target: coordinate(engine, target_idx),
        rel_start: coordinate(engine, rel_start_idx),
        rel_end: coordinate(engine, rel_end_idx),
    }
}

fn coordinate(engine: &Engine, node_idx: u32) -> GqlNodeCoordinate {
    GqlNodeCoordinate {
        table_oid: engine.node_store.table_oid(node_idx),
        node_id: engine.node_store.primary_key(node_idx).to_string(),
    }
}
