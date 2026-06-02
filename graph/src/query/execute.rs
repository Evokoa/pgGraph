//! Physical GQL plan execution against engine stores and edge overlays.

use crate::edge_store::EdgeStore;
use crate::engine::Engine;
use crate::projection::neighbors::EdgeOverlay;
use crate::projection::neighbors::{CsrNeighbors, NeighborSource, OverlayNeighbors};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

use super::logical_plan::BoundDirection;
use super::physical_plan::{PhysicalNodeScan, PhysicalPlan, PhysicalWildcardPathPlan, ReturnSlot};

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
    /// Target coordinate, absent for null-extended optional matches.
    pub(crate) target: Option<GqlNodeCoordinate>,
    /// Relationship start coordinate in the registered edge direction.
    pub(crate) rel_start: Option<GqlNodeCoordinate>,
    /// Relationship end coordinate in the registered edge direction.
    pub(crate) rel_end: Option<GqlNodeCoordinate>,
    /// Path nodes in query traversal order.
    pub(crate) path_nodes: Vec<GqlNodeCoordinate>,
    /// Path relationships in query traversal order.
    pub(crate) path_relationships: Vec<GqlPathRelationship>,
}

/// One relationship step in a GQL path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GqlPathRelationship {
    /// Matched relationship type label.
    pub(crate) rel_type: String,
    /// Relationship start coordinate in the registered edge direction.
    pub(crate) start: GqlNodeCoordinate,
    /// Relationship end coordinate in the registered edge direction.
    pub(crate) end: GqlNodeCoordinate,
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
        if !engine.node_store.is_active(source_idx)
            || crate::projection::tx_delta::node_deleted(source_idx)
        {
            continue;
        }
        let targets = expand_targets(&neighbors, engine, plan, source_idx, rel_type_id, tenant);
        if targets.is_empty() && plan.optional {
            if rows.len() >= row_cap {
                if plan.cap_exhaustion_is_error() {
                    return Err(GraphError::GqlExecution {
                        reason: format!("GQL result row cap exceeded ({row_cap})"),
                    });
                }
                return Ok(rows);
            }
            rows.push(project_optional_row(engine, source_idx));
            continue;
        }
        for target in targets {
            if rows.len() >= row_cap {
                if plan.cap_exhaustion_is_error() {
                    return Err(GraphError::GqlExecution {
                        reason: format!("GQL result row cap exceeded ({row_cap})"),
                    });
                }
                return Ok(rows);
            }
            rows.push(project_row(engine, source_idx, target)?);
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
        if !engine.node_store.is_active(node_idx)
            || crate::projection::tx_delta::node_deleted(node_idx)
        {
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
    let table_is_tenanted = engine.tenanted_table_oids.contains(&plan.table_oid);
    for node_id in
        crate::projection::tx_delta::added_node_keys(plan.table_oid, tenant, table_is_tenanted)
    {
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

/// Execute a physical wildcard path plan.
///
/// # Errors
///
/// Returns [`GraphError`] when the graph is not built or execution exceeds the
/// plan's cardinality cap.
pub(crate) fn execute_wildcard_path(
    engine: &Engine,
    plan: &PhysicalWildcardPathPlan,
    tenant: Option<&str>,
) -> GraphResult<Vec<GqlRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
    let neighbors = GqlNeighbors::new(engine);
    let row_cap = plan.execution_row_cap();
    let mut rows = Vec::new();
    let mut seen_relationships = std::collections::HashSet::new();
    for table_oid in &plan.required_node_table_oids {
        for source_idx in source_nodes(engine, *table_oid, tenant) {
            if !engine.node_store.is_active(source_idx)
                || crate::projection::tx_delta::node_deleted(source_idx)
            {
                continue;
            }
            for target in neighbors.for_direction_any(plan.direction, source_idx) {
                if !engine.node_store.is_active(target.node_idx)
                    || crate::projection::tx_delta::node_deleted(target.node_idx)
                    || !tenant_allows_node(engine, target.node_idx, tenant)
                {
                    continue;
                }
                let (rel_start_idx, rel_end_idx) = match target.orientation {
                    EdgeOrientation::Forward => (source_idx, target.node_idx),
                    EdgeOrientation::Reverse => (target.node_idx, source_idx),
                };
                if plan.direction == BoundDirection::Undirected
                    && !seen_relationships.insert((rel_start_idx, rel_end_idx, target.type_id))
                {
                    continue;
                }
                if rows.len() >= row_cap {
                    if plan.cap_exhaustion_is_error() {
                        return Err(GraphError::GqlExecution {
                            reason: format!("GQL result row cap exceeded ({row_cap})"),
                        });
                    }
                    return Ok(rows);
                }
                rows.push(project_wildcard_row(
                    engine,
                    source_idx,
                    target.node_idx,
                    target.orientation,
                    target.type_id,
                )?);
            }
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
    let mut results = Vec::new();
    let returns_relationship = plan
        .returns
        .iter()
        .any(|slot| matches!(slot, ReturnSlot::Relationship { .. }));
    let preserve_path_matches = plan.hops.variable;
    let mut seen_result_nodes = std::collections::HashSet::new();
    let mut seen_result_relationships = std::collections::HashSet::new();
    let mut seen_frontier = std::collections::HashSet::from([source_idx]);
    let mut current = vec![PathState {
        node_idx: source_idx,
        path_nodes: vec![source_idx],
        path_relationships: Vec::new(),
    }];
    for depth in 1..=plan.hops.max {
        let mut next = Vec::new();
        let mut seen_next = std::collections::HashSet::new();
        for state in current {
            for target in neighbors.for_direction(plan.direction, state.node_idx, rel_type_id) {
                if !engine.node_store.is_active(target.node_idx)
                    || crate::projection::tx_delta::node_deleted(target.node_idx)
                    || !tenant_allows_node(engine, target.node_idx, tenant)
                {
                    continue;
                }
                if preserve_path_matches && state.path_nodes.contains(&target.node_idx) {
                    continue;
                }
                let next_state = state.push(target);
                if depth >= plan.hops.min
                    && target_matches(engine, target.node_idx, plan.target_table_oid, tenant)
                    && (preserve_path_matches
                        || if returns_relationship {
                            seen_result_relationships.insert((
                                target.node_idx,
                                target.orientation,
                                target.type_id,
                            ))
                        } else {
                            seen_result_nodes.insert(target.node_idx)
                        })
                {
                    results.push(GqlTarget {
                        node_idx: target.node_idx,
                        orientation: target.orientation,
                        type_id: target.type_id,
                        path_nodes: next_state.path_nodes.clone(),
                        path_relationships: next_state.path_relationships.clone(),
                    });
                }
                if depth < plan.hops.max
                    && (preserve_path_matches
                        || (seen_frontier.insert(target.node_idx)
                            && seen_next.insert(target.node_idx)))
                {
                    next.push(next_state);
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

#[derive(Debug, Clone)]
struct PathState {
    node_idx: u32,
    path_nodes: Vec<u32>,
    path_relationships: Vec<GqlRelationshipStep>,
}

impl PathState {
    fn push(&self, target: GqlStepTarget) -> Self {
        let mut path_nodes = self.path_nodes.clone();
        path_nodes.push(target.node_idx);
        let mut path_relationships = self.path_relationships.clone();
        path_relationships.push(GqlRelationshipStep {
            from_idx: self.node_idx,
            to_idx: target.node_idx,
            orientation: target.orientation,
            type_id: target.type_id,
        });
        Self {
            node_idx: target.node_idx,
            path_nodes,
            path_relationships,
        }
    }
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
    ) -> Vec<GqlStepTarget> {
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

    fn for_direction_any(&self, direction: BoundDirection, node_idx: u32) -> Vec<GqlStepTarget> {
        let mut neighbors = Vec::new();
        if matches!(direction, BoundDirection::Out | BoundDirection::Undirected) {
            self.append_direction_neighbors_any(
                TraversalDirection::Out,
                node_idx,
                EdgeOrientation::Forward,
                &mut neighbors,
            );
        }
        if matches!(direction, BoundDirection::In | BoundDirection::Undirected) {
            self.append_direction_neighbors_any(
                TraversalDirection::In,
                node_idx,
                EdgeOrientation::Reverse,
                &mut neighbors,
            );
        }
        neighbors.sort_by_key(|target| (target.node_idx, target.orientation, target.type_id));
        neighbors.dedup();
        neighbors
    }

    fn append_direction_neighbors(
        &self,
        direction: TraversalDirection,
        node_idx: u32,
        rel_type_id: u8,
        orientation: EdgeOrientation,
        out: &mut Vec<GqlStepTarget>,
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

    fn append_direction_neighbors_any(
        &self,
        direction: TraversalDirection,
        node_idx: u32,
        orientation: EdgeOrientation,
        out: &mut Vec<GqlStepTarget>,
    ) {
        let (edge_store, overlay) = match direction {
            TraversalDirection::Any | TraversalDirection::Out => {
                (self.out_store, self.out_overlay.as_ref())
            }
            TraversalDirection::In => (self.in_store, self.in_overlay.as_ref()),
        };
        let Some((inserts, deletes)) = overlay else {
            let neighbors = CsrNeighbors::new(edge_store);
            append_all_neighbors(&neighbors, node_idx, orientation, out);
            return;
        };
        let neighbors = OverlayNeighbors::new(edge_store, inserts, deletes);
        append_all_neighbors(&neighbors, node_idx, orientation, out);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GqlTarget {
    node_idx: u32,
    orientation: EdgeOrientation,
    type_id: u8,
    path_nodes: Vec<u32>,
    path_relationships: Vec<GqlRelationshipStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GqlStepTarget {
    node_idx: u32,
    orientation: EdgeOrientation,
    type_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GqlRelationshipStep {
    from_idx: u32,
    to_idx: u32,
    orientation: EdgeOrientation,
    type_id: u8,
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
    out: &mut Vec<GqlStepTarget>,
) {
    out.extend(source.neighbors(node_idx).filter_map(|neighbor| {
        (neighbor.type_id == rel_type_id).then_some(GqlStepTarget {
            node_idx: neighbor.target,
            orientation,
            type_id: neighbor.type_id,
        })
    }));
}

fn append_all_neighbors(
    source: &impl NeighborSource,
    node_idx: u32,
    orientation: EdgeOrientation,
    out: &mut Vec<GqlStepTarget>,
) {
    out.extend(source.neighbors(node_idx).map(|neighbor| GqlStepTarget {
        node_idx: neighbor.target,
        orientation,
        type_id: neighbor.type_id,
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

fn project_row(engine: &Engine, source_idx: u32, target: GqlTarget) -> GraphResult<GqlRow> {
    let target_idx = target.node_idx;
    let (rel_start_idx, rel_end_idx) = match target.orientation {
        EdgeOrientation::Forward => (source_idx, target_idx),
        EdgeOrientation::Reverse => (target_idx, source_idx),
    };
    Ok(GqlRow {
        source: coordinate(engine, source_idx),
        target: Some(coordinate(engine, target_idx)),
        rel_start: Some(coordinate(engine, rel_start_idx)),
        rel_end: Some(coordinate(engine, rel_end_idx)),
        path_nodes: target
            .path_nodes
            .into_iter()
            .map(|node_idx| coordinate(engine, node_idx))
            .collect(),
        path_relationships: target
            .path_relationships
            .into_iter()
            .map(|relationship| {
                let (start_idx, end_idx) = match relationship.orientation {
                    EdgeOrientation::Forward => (relationship.from_idx, relationship.to_idx),
                    EdgeOrientation::Reverse => (relationship.to_idx, relationship.from_idx),
                };
                Ok(GqlPathRelationship {
                    rel_type: edge_type_label(engine, relationship.type_id)?,
                    start: coordinate(engine, start_idx),
                    end: coordinate(engine, end_idx),
                })
            })
            .collect::<GraphResult<Vec<_>>>()?,
    })
}

fn project_wildcard_row(
    engine: &Engine,
    source_idx: u32,
    target_idx: u32,
    orientation: EdgeOrientation,
    type_id: u8,
) -> GraphResult<GqlRow> {
    let (rel_start_idx, rel_end_idx) = match orientation {
        EdgeOrientation::Forward => (source_idx, target_idx),
        EdgeOrientation::Reverse => (target_idx, source_idx),
    };
    let source = coordinate(engine, source_idx);
    let target = coordinate(engine, target_idx);
    let rel_start = coordinate(engine, rel_start_idx);
    let rel_end = coordinate(engine, rel_end_idx);
    let rel_type = edge_type_label(engine, type_id)?;
    Ok(GqlRow {
        source: source.clone(),
        target: Some(target.clone()),
        rel_start: Some(rel_start.clone()),
        rel_end: Some(rel_end.clone()),
        path_nodes: vec![source, target],
        path_relationships: vec![GqlPathRelationship {
            rel_type,
            start: rel_start,
            end: rel_end,
        }],
    })
}

fn project_optional_row(engine: &Engine, source_idx: u32) -> GqlRow {
    GqlRow {
        source: coordinate(engine, source_idx),
        target: None,
        rel_start: None,
        rel_end: None,
        path_nodes: Vec::new(),
        path_relationships: Vec::new(),
    }
}

fn coordinate(engine: &Engine, node_idx: u32) -> GqlNodeCoordinate {
    GqlNodeCoordinate {
        table_oid: engine.node_store.table_oid(node_idx),
        node_id: engine.node_store.primary_key(node_idx).to_string(),
    }
}

fn edge_type_label(engine: &Engine, type_id: u8) -> GraphResult<String> {
    engine
        .edge_type_registry
        .get(type_id as usize)
        .filter(|label| !label.is_empty())
        .cloned()
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("relationship type id `{type_id}` is not present in the built graph"),
        })
}
