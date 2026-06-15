//! Physical GQL plan execution against engine stores and edge overlays.

use crate::edge_store::EdgeStore;
use crate::engine::Engine;
use crate::projection::layered::LayeredNeighbors;
use crate::projection::neighbors::EdgeOverlay;
use crate::projection::neighbors::{CsrNeighbors, NeighborSource, OverlayNeighbors};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

use super::logical_plan::{BindingSide, BoundCmpOp, BoundDirection, Predicate, ValueExpr};
use super::physical_plan::{
    PhysicalJoinPlan, PhysicalNodeScan, PhysicalPlan, PhysicalWildcardPathPlan,
    PhysicalWildcardPathSegment, ReturnSlot,
};

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
    /// Multi-pattern join node slots by logical slot index.
    pub(crate) join_node_slots: Option<Vec<Option<GqlNodeCoordinate>>>,
    /// Multi-pattern join path node slots by pattern index.
    pub(crate) join_path_nodes: Option<Vec<Option<Vec<GqlNodeCoordinate>>>>,
    /// Multi-pattern join relationship slots by pattern index.
    pub(crate) join_relationships: Option<Vec<Option<GqlPathRelationship>>>,
    /// Multi-pattern join path relationship slots by pattern index.
    pub(crate) join_path_relationships: Option<Vec<Option<Vec<GqlPathRelationship>>>>,
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
    reject_tx_created_traversal_entry_points(
        engine,
        [TxTraversalEntryScope {
            table_oid: plan.source_table_oid,
            allowed_node_ids: source_id_equalities(plan.predicate.as_ref(), BindingSide::Source),
        }],
        tenant,
    )?;
    let rel_type_id = edge_type_id(engine, &plan.rel_type)?;
    let neighbors = GqlNeighbors::new(engine)?;
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
    params: &crate::query::value::QueryParams,
) -> GraphResult<Vec<GqlNodeRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
    if let Some(identity_lookup) = &plan.identity_lookup {
        let Some(node_id) = crate::query::value::identity_lookup_text(identity_lookup, params)?
        else {
            return Ok(Vec::new());
        };
        if let Some(node_idx) = engine.resolve(plan.table_oid, &node_id) {
            if crate::projection::tx_delta::node_deleted(node_idx)
                || !tenant_allows_node(engine, node_idx, tenant)
            {
                return Ok(Vec::new());
            }
        } else {
            let table_is_tenanted = engine.tenanted_table_oids.contains(&plan.table_oid);
            if !crate::projection::tx_delta::added_node_keys(
                plan.table_oid,
                tenant,
                table_is_tenanted,
            )
            .iter()
            .any(|added| added == &node_id)
            {
                return Ok(Vec::new());
            }
        }
        return Ok(vec![GqlNodeRow {
            node: GqlNodeCoordinate {
                table_oid: plan.table_oid,
                node_id,
            },
        }]);
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

/// Execute a physical multi-pattern join plan.
///
/// # Errors
///
/// Returns [`GraphError`] when the graph is not built, a requested relationship
/// type is not present, or execution exceeds the plan's cardinality cap.
pub(crate) fn execute_join(
    engine: &Engine,
    plan: &PhysicalJoinPlan,
    tenant: Option<&str>,
) -> GraphResult<Vec<GqlRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
    reject_tx_created_traversal_entry_points(engine, join_tx_traversal_entry_scopes(plan), tenant)?;
    let rel_type_ids = plan
        .patterns
        .iter()
        .map(|pattern| edge_type_id(engine, &pattern.rel_type))
        .collect::<GraphResult<Vec<_>>>()?;
    let neighbors = GqlNeighbors::new(engine)?;
    let mut rows = Vec::new();
    let row_cap = plan.execution_row_cap();
    let state = JoinState {
        node_slots: vec![None; plan.node_slots.len()],
        relationships: vec![None; plan.patterns.len()],
    };
    expand_join_pattern(
        engine,
        &neighbors,
        plan,
        &rel_type_ids,
        tenant,
        state,
        0,
        &mut rows,
        row_cap,
    )?;
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
fn expand_join_pattern(
    engine: &Engine,
    neighbors: &GqlNeighbors<'_>,
    plan: &PhysicalJoinPlan,
    rel_type_ids: &[u8],
    tenant: Option<&str>,
    state: JoinState,
    pattern_idx: usize,
    rows: &mut Vec<GqlRow>,
    row_cap: usize,
) -> GraphResult<()> {
    let Some(pattern) = plan.patterns.get(pattern_idx) else {
        if rows.len() >= row_cap {
            if plan.cap_exhaustion_is_error() {
                return Err(GraphError::GqlExecution {
                    reason: format!("GQL result row cap exceeded ({row_cap})"),
                });
            }
            return Ok(());
        }
        rows.push(project_join_state(engine, state)?);
        return Ok(());
    };
    if plan.optional
        && state
            .relationships
            .iter()
            .take(pattern_idx)
            .any(Option::is_none)
    {
        expand_join_pattern(
            engine,
            neighbors,
            plan,
            rel_type_ids,
            tenant,
            state,
            pattern_idx + 1,
            rows,
            row_cap,
        )?;
        return Ok(());
    }
    let source_candidates =
        join_source_candidates(engine, plan, &state, pattern.source_slot, tenant);
    let null_extend_per_source = plan.optional && state.node_slots.iter().all(Option::is_none);
    let mut matched_any_source = false;
    for source_idx in source_candidates {
        if !join_node_matches(engine, plan, pattern.source_slot, source_idx, tenant) {
            continue;
        }
        let state = state.with_node(pattern.source_slot, source_idx);
        let mut matched_source = false;
        expand_join_pattern_hops(
            engine,
            neighbors,
            plan,
            rel_type_ids,
            tenant,
            state.clone(),
            pattern_idx,
            source_idx,
            0,
            Vec::new(),
            &mut matched_source,
            rows,
            row_cap,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
        if null_extend_per_source && !matched_source {
            let next_state = state.without_relationship(pattern_idx);
            expand_join_pattern(
                engine,
                neighbors,
                plan,
                rel_type_ids,
                tenant,
                next_state,
                pattern_idx + 1,
                rows,
                row_cap,
            )?;
            if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
                return Ok(());
            }
        }
        matched_any_source |= matched_source;
    }
    if plan.optional && !null_extend_per_source && !matched_any_source {
        let next_state = state.without_relationship(pattern_idx);
        expand_join_pattern(
            engine,
            neighbors,
            plan,
            rel_type_ids,
            tenant,
            next_state,
            pattern_idx + 1,
            rows,
            row_cap,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn expand_join_pattern_hops(
    engine: &Engine,
    neighbors: &GqlNeighbors<'_>,
    plan: &PhysicalJoinPlan,
    rel_type_ids: &[u8],
    tenant: Option<&str>,
    state: JoinState,
    pattern_idx: usize,
    current_idx: u32,
    hop_count: u32,
    relationships: Vec<GqlRelationshipStep>,
    matched_source: &mut bool,
    rows: &mut Vec<GqlRow>,
    row_cap: usize,
) -> GraphResult<()> {
    let pattern = &plan.patterns[pattern_idx];
    if hop_count >= pattern.hops.min
        && join_node_matches(engine, plan, pattern.target_slot, current_idx, tenant)
        && state.node_slots[pattern.target_slot].is_none_or(|idx| idx == current_idx)
    {
        *matched_source = true;
        let next_state = state.with_target_path(
            pattern_idx,
            pattern.target_slot,
            current_idx,
            relationships.clone(),
        );
        expand_join_pattern(
            engine,
            neighbors,
            plan,
            rel_type_ids,
            tenant,
            next_state,
            pattern_idx + 1,
            rows,
            row_cap,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
    }
    if hop_count >= pattern.hops.max {
        return Ok(());
    }
    for target in neighbors.for_direction_any(pattern.direction, current_idx) {
        if target.type_id != rel_type_ids[pattern_idx]
            || !wildcard_node_visible(engine, target.node_idx, tenant)
        {
            continue;
        }
        let mut next_relationships = relationships.clone();
        next_relationships.push(GqlRelationshipStep {
            from_idx: current_idx,
            to_idx: target.node_idx,
            orientation: target.orientation,
            type_id: target.type_id,
            schema_reversed: target.schema_reversed,
        });
        expand_join_pattern_hops(
            engine,
            neighbors,
            plan,
            rel_type_ids,
            tenant,
            state.clone(),
            pattern_idx,
            target.node_idx,
            hop_count + 1,
            next_relationships,
            matched_source,
            rows,
            row_cap,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
    }
    Ok(())
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
    let neighbors = GqlNeighbors::new(engine)?;
    let segment_filters = plan
        .segments
        .iter()
        .map(|segment| {
            segment
                .rel_type_filters
                .iter()
                .map(|rel_type| edge_type_id(engine, rel_type))
                .collect::<GraphResult<std::collections::BTreeSet<_>>>()
        })
        .collect::<GraphResult<Vec<_>>>()?;
    let row_cap = plan.execution_row_cap();
    let mut rows = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    let scan_table_oids: Vec<u32> = plan.source_table_filter.map_or_else(
        || plan.required_node_table_oids.iter().copied().collect(),
        |oid| vec![oid],
    );
    reject_tx_created_traversal_entry_points(
        engine,
        scan_table_oids
            .iter()
            .copied()
            .map(|table_oid| TxTraversalEntryScope {
                table_oid,
                allowed_node_ids: source_id_equalities(
                    plan.predicate.as_ref(),
                    BindingSide::PathNode(0),
                ),
            }),
        tenant,
    )?;
    for table_oid in scan_table_oids {
        for source_idx in source_nodes(engine, table_oid, tenant) {
            if !engine.node_store.is_active(source_idx)
                || crate::projection::tx_delta::node_deleted(source_idx)
            {
                continue;
            }
            expand_wildcard_segments(
                &WildcardExpansion {
                    engine,
                    neighbors: &neighbors,
                    plan,
                    segment_filters: &segment_filters,
                    tenant,
                    row_cap,
                },
                source_idx,
                &mut rows,
                &mut seen_paths,
            )?;
        }
    }
    Ok(rows)
}

struct WildcardExpansion<'a> {
    engine: &'a Engine,
    neighbors: &'a GqlNeighbors<'a>,
    plan: &'a PhysicalWildcardPathPlan,
    segment_filters: &'a [std::collections::BTreeSet<u8>],
    tenant: Option<&'a str>,
    row_cap: usize,
}

fn expand_wildcard_segments(
    expansion: &WildcardExpansion<'_>,
    source_idx: u32,
    rows: &mut Vec<GqlRow>,
    seen_paths: &mut std::collections::HashSet<Vec<(u32, u32, u8)>>,
) -> GraphResult<()> {
    let state = PathState {
        node_idx: source_idx,
        path_nodes: vec![source_idx],
        path_relationships: Vec::new(),
    };
    expand_wildcard_segment(
        expansion.engine,
        expansion.neighbors,
        expansion.plan,
        expansion.segment_filters,
        expansion.tenant,
        state,
        0,
        rows,
        seen_paths,
        expansion.row_cap,
    )
}

#[allow(clippy::too_many_arguments)]
fn expand_wildcard_segment(
    engine: &Engine,
    neighbors: &GqlNeighbors<'_>,
    plan: &PhysicalWildcardPathPlan,
    segment_filters: &[std::collections::BTreeSet<u8>],
    tenant: Option<&str>,
    state: PathState,
    segment_idx: usize,
    rows: &mut Vec<GqlRow>,
    seen_paths: &mut std::collections::HashSet<Vec<(u32, u32, u8)>>,
    row_cap: usize,
) -> GraphResult<()> {
    let Some(segment) = plan.segments.get(segment_idx) else {
        let path_key = state
            .path_relationships
            .iter()
            .map(|relationship| {
                let (rel_start_idx, rel_end_idx) = canonical_step_endpoints(relationship);
                (rel_start_idx, rel_end_idx, relationship.type_id)
            })
            .collect::<Vec<_>>();
        if !seen_paths.insert(path_key) {
            return Ok(());
        }
        if rows.len() >= row_cap {
            if plan.cap_exhaustion_is_error() {
                return Err(GraphError::GqlExecution {
                    reason: format!("GQL result row cap exceeded ({row_cap})"),
                });
            }
            return Ok(());
        }
        rows.push(project_wildcard_path_state(engine, state)?);
        return Ok(());
    };
    expand_wildcard_segment_hops(
        engine,
        neighbors,
        plan,
        segment_filters,
        tenant,
        segment,
        state,
        segment_idx,
        0,
        rows,
        seen_paths,
        row_cap,
    )
}

#[allow(clippy::too_many_arguments)]
fn expand_wildcard_segment_hops(
    engine: &Engine,
    neighbors: &GqlNeighbors<'_>,
    plan: &PhysicalWildcardPathPlan,
    segment_filters: &[std::collections::BTreeSet<u8>],
    tenant: Option<&str>,
    segment: &PhysicalWildcardPathSegment,
    state: PathState,
    segment_idx: usize,
    hop_count: u32,
    rows: &mut Vec<GqlRow>,
    seen_paths: &mut std::collections::HashSet<Vec<(u32, u32, u8)>>,
    row_cap: usize,
) -> GraphResult<()> {
    if hop_count >= segment.hops.min
        && wildcard_segment_endpoint_matches(engine, segment, state.node_idx, tenant)
    {
        expand_wildcard_segment(
            engine,
            neighbors,
            plan,
            segment_filters,
            tenant,
            state.clone(),
            segment_idx + 1,
            rows,
            seen_paths,
            row_cap,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
    }
    if hop_count >= segment.hops.max {
        return Ok(());
    }
    for target in neighbors.for_direction_any(segment.direction, state.node_idx) {
        if !segment_filters[segment_idx].is_empty()
            && !segment_filters[segment_idx].contains(&target.type_id)
        {
            continue;
        }
        if !wildcard_node_visible(engine, target.node_idx, tenant) {
            continue;
        }
        let next_state = state.push(target);
        expand_wildcard_segment_hops(
            engine,
            neighbors,
            plan,
            segment_filters,
            tenant,
            segment,
            next_state,
            segment_idx,
            hop_count + 1,
            rows,
            seen_paths,
            row_cap,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
    }
    Ok(())
}

fn wildcard_node_visible(engine: &Engine, target_idx: u32, tenant: Option<&str>) -> bool {
    engine.node_store.is_active(target_idx)
        && !crate::projection::tx_delta::node_deleted(target_idx)
        && tenant_allows_node(engine, target_idx, tenant)
}

fn wildcard_segment_endpoint_matches(
    engine: &Engine,
    segment: &PhysicalWildcardPathSegment,
    target_idx: u32,
    tenant: Option<&str>,
) -> bool {
    wildcard_node_visible(engine, target_idx, tenant)
        && segment
            .target_table_filter
            .is_none_or(|table_oid| engine.node_store.table_oid(target_idx) == table_oid)
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TxTraversalEntryScope {
    table_oid: u32,
    allowed_node_ids: Option<std::collections::BTreeSet<String>>,
}

fn reject_tx_created_traversal_entry_points(
    engine: &Engine,
    scopes: impl IntoIterator<Item = TxTraversalEntryScope>,
    tenant: Option<&str>,
) -> GraphResult<()> {
    let mut checked = std::collections::BTreeSet::new();
    for scope in scopes {
        if !checked.insert((scope.table_oid, scope.allowed_node_ids.clone())) {
            continue;
        }
        let table_is_tenanted = engine.tenanted_table_oids.contains(&scope.table_oid);
        let added = crate::projection::tx_delta::added_node_keys(
            scope.table_oid,
            tenant,
            table_is_tenanted,
        );
        let rejected = added.iter().find(|node_id| {
            scope
                .allowed_node_ids
                .as_ref()
                .is_none_or(|allowed| allowed.contains(*node_id))
        });
        if let Some(node_id) = rejected {
            let table_oid = scope.table_oid;
            return Err(GraphError::GqlExecution {
                reason: format!(
                    "transaction-created nodes cannot be used as traversal entry points \
                     until temporary-id traversal support is implemented (table_oid={table_oid}, \
                     node_id={node_id})"
                ),
            });
        }
    }
    Ok(())
}

fn join_tx_traversal_entry_scopes(plan: &PhysicalJoinPlan) -> Vec<TxTraversalEntryScope> {
    let mut bound_by_prior_pattern = std::collections::BTreeSet::new();
    let mut scopes = Vec::new();
    for pattern in &plan.patterns {
        if plan.optional || !bound_by_prior_pattern.contains(&pattern.source_slot) {
            let source_side = BindingSide::PathNode(pattern.source_slot);
            scopes.push(TxTraversalEntryScope {
                table_oid: plan.node_slots[pattern.source_slot].table_oid,
                allowed_node_ids: source_id_equalities(plan.predicate.as_ref(), source_side),
            });
        }
        bound_by_prior_pattern.insert(pattern.source_slot);
        bound_by_prior_pattern.insert(pattern.target_slot);
    }
    scopes
}

fn source_id_equalities(
    predicate: Option<&Predicate>,
    side: BindingSide,
) -> Option<std::collections::BTreeSet<String>> {
    let predicate = predicate?;
    match predicate {
        Predicate::And(lhs, rhs) => {
            let lhs = source_id_equalities(Some(lhs), side);
            let rhs = source_id_equalities(Some(rhs), side);
            match (lhs, rhs) {
                (Some(lhs), Some(rhs)) => Some(lhs.intersection(&rhs).cloned().collect()),
                (Some(ids), None) | (None, Some(ids)) => Some(ids),
                (None, None) => None,
            }
        }
        Predicate::Or(lhs, rhs) => {
            let lhs = source_id_equalities(Some(lhs), side);
            let rhs = source_id_equalities(Some(rhs), side);
            match (lhs, rhs) {
                (Some(mut lhs), Some(rhs)) => {
                    lhs.extend(rhs);
                    Some(lhs)
                }
                _ => None,
            }
        }
        Predicate::Compare {
            lhs,
            op: BoundCmpOp::Eq,
            rhs: Some(rhs),
        } => literal_id_equality(lhs, rhs, side).or_else(|| literal_id_equality(rhs, lhs, side)),
        _ => None,
    }
}

fn literal_id_equality(
    property_expr: &ValueExpr,
    value_expr: &ValueExpr,
    side: BindingSide,
) -> Option<std::collections::BTreeSet<String>> {
    let ValueExpr::Property {
        side: property_side,
        property,
    } = property_expr
    else {
        return None;
    };
    if *property_side != side || property != "id" {
        return None;
    }
    let ValueExpr::Literal(serde_json::Value::String(node_id)) = value_expr else {
        return None;
    };
    Some(std::collections::BTreeSet::from([node_id.clone()]))
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
                                target.schema_reversed,
                            ))
                        } else {
                            seen_result_nodes.insert(target.node_idx)
                        })
                {
                    results.push(GqlTarget {
                        node_idx: target.node_idx,
                        orientation: target.orientation,
                        type_id: target.type_id,
                        schema_reversed: target.schema_reversed,
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
            schema_reversed: target.schema_reversed,
        });
        Self {
            node_idx: target.node_idx,
            path_nodes,
            path_relationships,
        }
    }
}

#[derive(Debug, Clone)]
struct JoinState {
    node_slots: Vec<Option<u32>>,
    relationships: Vec<Option<Vec<GqlRelationshipStep>>>,
}

impl JoinState {
    fn with_node(&self, slot: usize, node_idx: u32) -> Self {
        let mut next = self.clone();
        next.node_slots[slot] = Some(node_idx);
        next
    }

    fn with_target_path(
        &self,
        pattern_slot: usize,
        node_slot: usize,
        target_idx: u32,
        relationships: Vec<GqlRelationshipStep>,
    ) -> Self {
        let mut next = self.clone();
        next.node_slots[node_slot] = Some(target_idx);
        next.relationships[pattern_slot] = Some(relationships);
        next
    }

    fn without_relationship(&self, pattern_slot: usize) -> Self {
        let mut next = self.clone();
        next.relationships[pattern_slot] = None;
        next
    }
}

struct GqlNeighbors<'a> {
    out_store: &'a EdgeStore,
    in_store: &'a EdgeStore,
    out_overlay: Option<EdgeOverlay>,
    in_overlay: Option<EdgeOverlay>,
    layered: Option<LayeredNeighbors<'a>>,
}

impl<'a> GqlNeighbors<'a> {
    fn new(engine: &'a Engine) -> GraphResult<Self> {
        let (out_overlay, in_overlay) = if engine.has_edge_overlay() {
            (
                Some(engine.traversal_edge_overlay(TraversalDirection::Out)),
                Some(engine.traversal_edge_overlay(TraversalDirection::In)),
            )
        } else {
            (None, None)
        };

        Ok(Self {
            out_store: &engine.edge_store,
            in_store: &engine.reverse_edge_store,
            out_overlay,
            in_overlay,
            layered: engine.layered_neighbors()?,
        })
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
        neighbors
            .sort_by_key(|target| (target.node_idx, target.orientation, target.schema_reversed));
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
        neighbors.sort_by_key(|target| {
            (
                target.node_idx,
                target.orientation,
                target.type_id,
                target.schema_reversed,
            )
        });
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
        if let Some(layered) = &self.layered {
            let neighbors = layered.for_direction(direction);
            append_matching_neighbors(&neighbors, node_idx, rel_type_id, orientation, out);
            return;
        }
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
        if let Some(layered) = &self.layered {
            let neighbors = layered.for_direction(direction);
            append_all_neighbors(&neighbors, node_idx, orientation, out);
            return;
        }
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
    schema_reversed: bool,
    path_nodes: Vec<u32>,
    path_relationships: Vec<GqlRelationshipStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GqlStepTarget {
    node_idx: u32,
    orientation: EdgeOrientation,
    type_id: u8,
    schema_reversed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GqlRelationshipStep {
    from_idx: u32,
    to_idx: u32,
    orientation: EdgeOrientation,
    type_id: u8,
    schema_reversed: bool,
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
            schema_reversed: neighbor.schema_reversed,
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
        schema_reversed: neighbor.schema_reversed,
    }));
}

fn join_source_candidates(
    engine: &Engine,
    plan: &PhysicalJoinPlan,
    state: &JoinState,
    slot: usize,
    tenant: Option<&str>,
) -> Vec<u32> {
    state.node_slots[slot].map_or_else(
        || source_nodes(engine, plan.node_slots[slot].table_oid, tenant),
        |node_idx| vec![node_idx],
    )
}

fn join_node_matches(
    engine: &Engine,
    plan: &PhysicalJoinPlan,
    slot: usize,
    node_idx: u32,
    tenant: Option<&str>,
) -> bool {
    target_matches(engine, node_idx, plan.node_slots[slot].table_oid, tenant)
        && !crate::projection::tx_delta::node_deleted(node_idx)
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
    let (rel_start_idx, rel_end_idx) = canonical_relationship_endpoints(
        source_idx,
        target_idx,
        target.orientation,
        target.schema_reversed,
    );
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
                let (start_idx, end_idx) = canonical_step_endpoints(&relationship);
                Ok(GqlPathRelationship {
                    rel_type: edge_type_label(engine, relationship.type_id)?,
                    start: coordinate(engine, start_idx),
                    end: coordinate(engine, end_idx),
                })
            })
            .collect::<GraphResult<Vec<_>>>()?,
        join_node_slots: None,
        join_path_nodes: None,
        join_relationships: None,
        join_path_relationships: None,
    })
}

fn project_join_state(engine: &Engine, state: JoinState) -> GraphResult<GqlRow> {
    let join_node_slots = state
        .node_slots
        .iter()
        .map(|node| node.map(|node_idx| coordinate(engine, node_idx)))
        .collect::<Vec<_>>();
    let path_nodes = join_node_slots
        .iter()
        .filter_map(Clone::clone)
        .collect::<Vec<_>>();
    let Some(source) = path_nodes.first().cloned() else {
        return Err(GraphError::GqlExecution {
            reason: "multi-pattern join produced no bound node slots".to_string(),
        });
    };
    let target = path_nodes.last().cloned();
    let join_path_nodes = state
        .relationships
        .iter()
        .map(|relationships| {
            relationships.as_ref().map(|relationships| {
                let mut nodes = Vec::with_capacity(relationships.len() + 1);
                if let Some(first) = relationships.first() {
                    nodes.push(coordinate(engine, first.from_idx));
                }
                nodes.extend(
                    relationships
                        .iter()
                        .map(|relationship| coordinate(engine, relationship.to_idx)),
                );
                nodes
            })
        })
        .collect::<Vec<_>>();
    let join_path_relationships = state
        .relationships
        .iter()
        .map(|relationships| {
            relationships
                .as_ref()
                .map(|relationships| {
                    relationships
                        .iter()
                        .map(|relationship| gql_path_relationship(engine, relationship))
                        .collect::<GraphResult<Vec<_>>>()
                })
                .transpose()
        })
        .collect::<GraphResult<Vec<_>>>()?;
    let join_relationships = join_path_relationships
        .iter()
        .map(|relationships| {
            relationships
                .as_ref()
                .and_then(|path| path.first().cloned())
        })
        .collect::<Vec<_>>();
    let path_relationships = join_path_relationships
        .iter()
        .filter_map(Clone::clone)
        .flatten()
        .collect();
    Ok(GqlRow {
        source,
        target,
        rel_start: None,
        rel_end: None,
        path_nodes,
        path_relationships,
        join_node_slots: Some(join_node_slots),
        join_path_nodes: Some(join_path_nodes),
        join_relationships: Some(join_relationships),
        join_path_relationships: Some(join_path_relationships),
    })
}

fn gql_path_relationship(
    engine: &Engine,
    relationship: &GqlRelationshipStep,
) -> GraphResult<GqlPathRelationship> {
    let (start_idx, end_idx) = canonical_step_endpoints(relationship);
    Ok(GqlPathRelationship {
        rel_type: edge_type_label(engine, relationship.type_id)?,
        start: coordinate(engine, start_idx),
        end: coordinate(engine, end_idx),
    })
}

fn project_wildcard_path_state(engine: &Engine, state: PathState) -> GraphResult<GqlRow> {
    let path_nodes = state
        .path_nodes
        .iter()
        .map(|node_idx| coordinate(engine, *node_idx))
        .collect::<Vec<_>>();
    let Some(source) = path_nodes.first().cloned() else {
        return Err(GraphError::GqlExecution {
            reason: "wildcard path state has no source node".to_string(),
        });
    };
    let target = path_nodes.last().cloned();
    let path_relationships = state
        .path_relationships
        .iter()
        .map(|relationship| {
            let (start_idx, end_idx) = canonical_step_endpoints(relationship);
            Ok(GqlPathRelationship {
                rel_type: edge_type_label(engine, relationship.type_id)?,
                start: coordinate(engine, start_idx),
                end: coordinate(engine, end_idx),
            })
        })
        .collect::<GraphResult<Vec<_>>>()?;
    let (rel_start, rel_end) = path_relationships
        .first()
        .map(|relationship| (relationship.start.clone(), relationship.end.clone()))
        .unzip();
    Ok(GqlRow {
        source,
        target,
        rel_start,
        rel_end,
        path_nodes,
        path_relationships,
        join_node_slots: None,
        join_path_nodes: None,
        join_relationships: None,
        join_path_relationships: None,
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
        join_node_slots: None,
        join_path_nodes: None,
        join_relationships: None,
        join_path_relationships: None,
    }
}

fn canonical_step_endpoints(relationship: &GqlRelationshipStep) -> (u32, u32) {
    canonical_relationship_endpoints(
        relationship.from_idx,
        relationship.to_idx,
        relationship.orientation,
        relationship.schema_reversed,
    )
}

fn canonical_relationship_endpoints(
    from_idx: u32,
    to_idx: u32,
    orientation: EdgeOrientation,
    schema_reversed: bool,
) -> (u32, u32) {
    let (start_idx, end_idx) = match orientation {
        EdgeOrientation::Forward => (from_idx, to_idx),
        EdgeOrientation::Reverse => (to_idx, from_idx),
    };
    if schema_reversed {
        (end_idx, start_idx)
    } else {
        (start_idx, end_idx)
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
