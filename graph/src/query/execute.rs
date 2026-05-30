//! Physical GQL plan execution against immutable engine stores.

use crate::engine::Engine;
use crate::safety::{GraphError, GraphResult};

use super::logical_plan::BoundDirection;
use super::physical_plan::PhysicalPlan;

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
    let mut rows = Vec::new();
    let row_cap = plan.execution_row_cap();
    for source_idx in source_nodes(engine, plan.source_table_oid, tenant) {
        if !engine.node_store.is_active(source_idx) {
            continue;
        }
        for target_idx in expand_targets(engine, plan, source_idx, rel_type_id, tenant) {
            if rows.len() >= row_cap {
                if plan.cap_exhaustion_is_error() {
                    return Err(GraphError::GqlExecution {
                        reason: format!("GQL result row cap exceeded ({row_cap})"),
                    });
                }
                return Ok(rows);
            }
            rows.push(project_row(engine, source_idx, target_idx));
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
    engine: &Engine,
    plan: &PhysicalPlan,
    source_idx: u32,
    rel_type_id: u8,
    tenant: Option<&str>,
) -> Vec<u32> {
    let mut current = vec![source_idx];
    let mut results = Vec::new();
    let mut seen_results = std::collections::HashSet::new();
    let mut seen_frontier = std::collections::HashSet::from([source_idx]);
    for depth in 1..=plan.hops.max {
        let mut next = Vec::new();
        let mut seen_next = std::collections::HashSet::new();
        for node_idx in current {
            for neighbor in neighbors_for_direction(engine, plan.direction, node_idx, rel_type_id) {
                if !engine.node_store.is_active(neighbor)
                    || !tenant_allows_node(engine, neighbor, tenant)
                {
                    continue;
                }
                if depth >= plan.hops.min
                    && target_matches(engine, neighbor, plan.target_table_oid, tenant)
                    && seen_results.insert(neighbor)
                {
                    results.push(neighbor);
                }
                if depth < plan.hops.max
                    && seen_frontier.insert(neighbor)
                    && seen_next.insert(neighbor)
                {
                    next.push(neighbor);
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

fn neighbors_for_direction(
    engine: &Engine,
    direction: BoundDirection,
    node_idx: u32,
    rel_type_id: u8,
) -> Vec<u32> {
    let mut neighbors = Vec::new();
    if matches!(direction, BoundDirection::Out | BoundDirection::Undirected) {
        append_matching_neighbors(&engine.edge_store, node_idx, rel_type_id, &mut neighbors);
    }
    if matches!(direction, BoundDirection::In | BoundDirection::Undirected) {
        append_matching_neighbors(
            &engine.reverse_edge_store,
            node_idx,
            rel_type_id,
            &mut neighbors,
        );
    }
    neighbors.sort_unstable();
    neighbors.dedup();
    neighbors
}

fn append_matching_neighbors(
    store: &crate::edge_store::EdgeStore,
    node_idx: u32,
    rel_type_id: u8,
    out: &mut Vec<u32>,
) {
    let (targets, type_ids) = store.neighbors(node_idx);
    out.extend(
        targets
            .iter()
            .zip(type_ids.iter())
            .filter_map(|(&target, &type_id)| (type_id == rel_type_id).then_some(target)),
    );
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

fn project_row(engine: &Engine, source_idx: u32, target_idx: u32) -> GqlRow {
    GqlRow {
        source: coordinate(engine, source_idx),
        target: coordinate(engine, target_idx),
    }
}

fn coordinate(engine: &Engine, node_idx: u32) -> GqlNodeCoordinate {
    GqlNodeCoordinate {
        table_oid: engine.node_store.table_oid(node_idx),
        node_id: engine.node_store.primary_key(node_idx).to_string(),
    }
}
