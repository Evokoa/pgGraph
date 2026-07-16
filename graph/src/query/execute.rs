//! Physical GQL plan execution against engine stores and edge overlays.

use crate::edge_store::{EdgeStore, RelationshipId};
use crate::engine::Engine;
use crate::projection::layered::LayeredNeighbors;
use crate::projection::neighbors::EdgeOverlay;
use crate::projection::neighbors::{CsrNeighbors, NeighborSource, OverlayNeighbors};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

use super::logical_plan::BoundDirection;
use super::physical_plan::{
    PhysicalJoinPlan, PhysicalNodeScan, PhysicalPlan, PhysicalWildcardPathPlan,
    PhysicalWildcardPathSegment,
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
    /// Durable relationship identity for the primary one-hop relationship.
    pub(crate) relationship_id: Option<RelationshipId>,
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
    /// Durable relationship identity when the matched edge has one.
    pub(crate) relationship_id: Option<RelationshipId>,
}

/// One GQL node-only result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GqlNodeRow {
    /// Node coordinate.
    pub(crate) node: GqlNodeCoordinate,
    /// Whether this row is the null-extended result of node-only OPTIONAL MATCH.
    pub(crate) optional_null: bool,
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
    let governor = execution_governor(engine)?;
    execute_governed(engine, plan, tenant, &governor)
}

pub(crate) fn execute_governed(
    engine: &Engine,
    plan: &PhysicalPlan,
    tenant: Option<&str>,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<Vec<GqlRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
    let rel_type_id = edge_type_id(engine, &plan.rel_type)?;
    let mut rows = Vec::new();
    let row_cap = plan.execution_row_cap();
    reserve_execution_rows(
        governor,
        engine,
        row_cap,
        one_hop_row_shape(plan.hops.max)?,
        crate::resource::ResourcePhase::QueryCandidates,
    )?
    .retain_until_governor_drop();
    let neighbors = GqlNeighbors::new(engine)?;
    for source_idx in source_nodes(engine, plan.source_table_oid, tenant) {
        consume_query_work(governor, crate::resource::ResourcePhase::QueryCandidates)?;
        if !node_active(engine, source_idx) || crate::projection::tx_delta::node_deleted(source_idx)
        {
            continue;
        }
        let targets = expand_targets(
            &TargetExpansion {
                neighbors: &neighbors,
                engine,
                plan,
                rel_type_id,
                tenant,
                result_cap: row_cap.saturating_sub(rows.len()),
                governor,
            },
            source_idx,
        )?;
        if targets.is_empty() && plan.optional {
            if rows.len() >= row_cap {
                if plan.cap_exhaustion_is_error() {
                    return Err(GraphError::GqlExecution {
                        reason: format!("GQL result row cap exceeded ({row_cap})"),
                    });
                }
                return Ok(rows);
            }
            rows.push(project_optional_row(engine, source_idx)?);
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
    let governor = execution_governor(engine)?;
    execute_node_scan_governed(engine, plan, tenant, params, &governor)
}

pub(crate) fn execute_node_scan_governed(
    engine: &Engine,
    plan: &PhysicalNodeScan,
    tenant: Option<&str>,
    params: &crate::query::value::QueryParams,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<Vec<GqlNodeRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
    let row_cap = plan.execution_row_cap();
    reserve_execution_rows(
        governor,
        engine,
        row_cap,
        (1, 0),
        crate::resource::ResourcePhase::QueryCandidates,
    )?
    .retain_until_governor_drop();
    if let Some(identity_lookup) = &plan.identity_lookup {
        let Some(node_id) = crate::query::value::identity_lookup_text(identity_lookup, params)?
        else {
            return Ok(optional_node_scan_fallback(plan));
        };
        if let Some(node_idx) = engine.resolve(plan.table_oid, &node_id) {
            if crate::projection::tx_delta::node_deleted(node_idx)
                || !tenant_allows_node(engine, node_idx, tenant)
            {
                return Ok(optional_node_scan_fallback(plan));
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
                return Ok(optional_node_scan_fallback(plan));
            }
        }
        return Ok(vec![GqlNodeRow {
            node: GqlNodeCoordinate {
                table_oid: plan.table_oid,
                node_id,
            },
            optional_null: false,
        }]);
    }
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for node_idx in source_nodes(engine, plan.table_oid, tenant) {
        consume_query_work(governor, crate::resource::ResourcePhase::QueryCandidates)?;
        if !node_active(engine, node_idx) || crate::projection::tx_delta::node_deleted(node_idx) {
            continue;
        }
        let coordinate = coordinate(engine, node_idx)?;
        if seen.insert(coordinate.node_id.clone()) {
            if rows.len() >= row_cap {
                if plan.cap_exhaustion_is_error() {
                    return Err(GraphError::GqlExecution {
                        reason: format!("GQL result row cap exceeded ({row_cap})"),
                    });
                }
                return Ok(rows);
            }
            rows.push(GqlNodeRow {
                node: coordinate,
                optional_null: false,
            });
        }
    }
    let table_is_tenanted = engine.tenanted_table_oids.contains(&plan.table_oid);
    for node_id in
        crate::projection::tx_delta::added_node_keys(plan.table_oid, tenant, table_is_tenanted)
    {
        consume_query_work(governor, crate::resource::ResourcePhase::QueryCandidates)?;
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
                optional_null: false,
            });
        }
    }
    if rows.is_empty() {
        rows.extend(optional_node_scan_fallback(plan));
    }
    Ok(rows)
}

fn optional_node_scan_fallback(plan: &PhysicalNodeScan) -> Vec<GqlNodeRow> {
    if !plan.optional {
        return Vec::new();
    }
    vec![GqlNodeRow {
        node: GqlNodeCoordinate {
            table_oid: plan.table_oid,
            node_id: String::new(),
        },
        optional_null: true,
    }]
}

/// Execute a physical multi-pattern join plan.
///
/// # Errors
///
/// Returns [`GraphError`] when the graph is not built, a requested relationship
/// type is not present, or execution exceeds the plan's cardinality cap.
#[cfg(test)]
pub(crate) fn execute_join(
    engine: &Engine,
    plan: &PhysicalJoinPlan,
    tenant: Option<&str>,
) -> GraphResult<Vec<GqlRow>> {
    let governor = execution_governor(engine)?;
    execute_join_governed(engine, plan, tenant, &governor)
}

pub(crate) fn execute_join_governed(
    engine: &Engine,
    plan: &PhysicalJoinPlan,
    tenant: Option<&str>,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<Vec<GqlRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
    let rel_type_ids = plan
        .patterns
        .iter()
        .map(|pattern| edge_type_id(engine, &pattern.rel_type))
        .collect::<GraphResult<Vec<_>>>()?;
    let mut rows = Vec::new();
    let row_cap = plan.execution_row_cap();
    reserve_execution_rows(
        governor,
        engine,
        row_cap,
        join_row_shape(plan)?,
        crate::resource::ResourcePhase::QueryBlocking,
    )?
    .retain_until_governor_drop();
    let neighbors = GqlNeighbors::new(engine)?;
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
        governor,
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
    governor: &crate::resource::ResourceGovernor,
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
            governor,
        )?;
        return Ok(());
    }
    let source_candidates =
        join_source_candidates(engine, plan, &state, pattern.source_slot, tenant);
    let null_extend_per_source = plan.optional && state.node_slots.iter().all(Option::is_none);
    let mut matched_any_source = false;
    for source_idx in source_candidates {
        consume_query_work(governor, crate::resource::ResourcePhase::QueryCandidates)?;
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
            governor,
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
                governor,
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
            governor,
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
    governor: &crate::resource::ResourceGovernor,
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
            governor,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
    }
    if hop_count >= pattern.hops.max {
        return Ok(());
    }
    for target in neighbors.for_direction_any(pattern.direction, current_idx) {
        consume_query_work(governor, crate::resource::ResourcePhase::QueryExpand)?;
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
            relationship_id: target.relationship_id,
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
            governor,
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
#[cfg(test)]
pub(crate) fn execute_wildcard_path(
    engine: &Engine,
    plan: &PhysicalWildcardPathPlan,
    tenant: Option<&str>,
) -> GraphResult<Vec<GqlRow>> {
    let governor = execution_governor(engine)?;
    execute_wildcard_path_governed(engine, plan, tenant, &governor)
}

pub(crate) fn execute_wildcard_path_governed(
    engine: &Engine,
    plan: &PhysicalWildcardPathPlan,
    tenant: Option<&str>,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<Vec<GqlRow>> {
    if !engine.built {
        return Err(GraphError::NotBuilt);
    }
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
    reserve_execution_rows(
        governor,
        engine,
        row_cap,
        wildcard_row_shape(plan)?,
        crate::resource::ResourcePhase::QueryPaths,
    )?
    .retain_until_governor_drop();
    let neighbors = GqlNeighbors::new(engine)?;
    let mut rows = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    let scan_table_oids: Vec<u32> = plan.source_table_filter.map_or_else(
        || plan.required_node_table_oids.iter().copied().collect(),
        |oid| vec![oid],
    );
    for table_oid in scan_table_oids {
        for source_idx in source_nodes(engine, table_oid, tenant) {
            consume_query_work(governor, crate::resource::ResourcePhase::QueryCandidates)?;
            if !node_active(engine, source_idx)
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
                    governor,
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
    governor: &'a crate::resource::ResourceGovernor,
}

type WildcardPathStepKey = (u32, u32, u8, Option<RelationshipId>);
type SeenWildcardPaths = std::collections::HashSet<Vec<WildcardPathStepKey>>;

fn expand_wildcard_segments(
    expansion: &WildcardExpansion<'_>,
    source_idx: u32,
    rows: &mut Vec<GqlRow>,
    seen_paths: &mut SeenWildcardPaths,
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
        expansion.governor,
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
    seen_paths: &mut SeenWildcardPaths,
    row_cap: usize,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<()> {
    let Some(segment) = plan.segments.get(segment_idx) else {
        let path_key = state
            .path_relationships
            .iter()
            .map(|relationship| {
                let (rel_start_idx, rel_end_idx) = canonical_step_endpoints(relationship);
                (
                    rel_start_idx,
                    rel_end_idx,
                    relationship.type_id,
                    relationship.relationship_id,
                )
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
        governor,
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
    seen_paths: &mut SeenWildcardPaths,
    row_cap: usize,
    governor: &crate::resource::ResourceGovernor,
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
            governor,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
    }
    if hop_count >= segment.hops.max {
        return Ok(());
    }
    for target in neighbors.for_direction_any(segment.direction, state.node_idx) {
        consume_query_work(governor, crate::resource::ResourcePhase::QueryPaths)?;
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
            governor,
        )?;
        if plan.limit.is_some() && !plan.cap_exhaustion_is_error() && rows.len() >= row_cap {
            return Ok(());
        }
    }
    Ok(())
}

fn wildcard_node_visible(engine: &Engine, target_idx: u32, tenant: Option<&str>) -> bool {
    node_active(engine, target_idx)
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
            .is_none_or(|table_oid| node_table_oid(engine, target_idx) == Some(table_oid))
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

fn execution_governor(engine: &Engine) -> GraphResult<crate::resource::ResourceGovernor> {
    let resident = crate::resource::ByteCount::from_usize(engine.estimated_memory_used_bytes())
        .ok_or_else(|| GraphError::Internal("engine residency does not fit u64".to_string()))?;
    Ok(crate::resource::query_governor(resident))
}

fn reserve_execution_rows<'a>(
    governor: &'a crate::resource::ResourceGovernor,
    engine: &Engine,
    row_cap: usize,
    (coordinates_per_row, relationships_per_row): (usize, usize),
    phase: crate::resource::ResourcePhase,
) -> GraphResult<crate::resource::ResourceLease<'a>> {
    let max_primary_key_bytes = engine
        .node_store
        .max_primary_key_bytes()
        .max(crate::projection::tx_delta::max_added_node_primary_key_bytes());
    let max_relationship_label_bytes = engine
        .edge_type_registry
        .iter()
        .map(String::len)
        .max()
        .unwrap_or_default();
    let coordinate_bytes = std::mem::size_of::<GqlNodeCoordinate>()
        .checked_add(max_primary_key_bytes)
        .ok_or_else(|| GraphError::Internal("GQL coordinate estimate overflowed".to_string()))?;
    let relationship_bytes = std::mem::size_of::<GqlPathRelationship>()
        .checked_add(max_relationship_label_bytes)
        .ok_or_else(|| GraphError::Internal("GQL relationship estimate overflowed".to_string()))?;
    let row_bytes = std::mem::size_of::<GqlRow>()
        .checked_add(
            coordinates_per_row
                .checked_mul(coordinate_bytes)
                .ok_or_else(|| {
                    GraphError::Internal("GQL coordinate workspace overflowed".to_string())
                })?,
        )
        .and_then(|bytes| {
            relationships_per_row
                .checked_mul(relationship_bytes)
                .and_then(|relationships| bytes.checked_add(relationships))
        })
        .and_then(|bytes| {
            relationships_per_row
                .checked_mul(std::mem::size_of::<WildcardPathStepKey>())
                .and_then(|path_keys| bytes.checked_add(path_keys))
        })
        .ok_or_else(|| GraphError::Internal("GQL result row estimate overflowed".to_string()))?;
    let result_bytes = row_cap
        .checked_mul(row_bytes)
        .ok_or_else(|| GraphError::Internal("GQL result workspace overflowed".to_string()))?;
    // Execution keeps final rows alongside bounded target/frontier state and
    // duplicate-detection keys. Charge all of those live sets, not just the
    // returned row representation.
    let path_node_bytes = relationships_per_row
        .checked_add(1)
        .and_then(|nodes| nodes.checked_mul(std::mem::size_of::<u32>()))
        .ok_or_else(|| GraphError::Internal("GQL path-node estimate overflowed".to_string()))?;
    let path_state_bytes = std::mem::size_of::<PathState>()
        .checked_add(path_node_bytes)
        .and_then(|bytes| {
            relationships_per_row
                .checked_mul(std::mem::size_of::<GqlRelationshipStep>())
                .and_then(|relationships| bytes.checked_add(relationships))
        })
        .ok_or_else(|| GraphError::Internal("GQL path-state estimate overflowed".to_string()))?;
    let target_bytes = std::mem::size_of::<GqlTarget>()
        .checked_add(path_state_bytes)
        .ok_or_else(|| GraphError::Internal("GQL target estimate overflowed".to_string()))?;
    let seen_key_bytes = std::mem::size_of::<String>()
        .checked_add(max_primary_key_bytes)
        .and_then(|bytes| bytes.checked_add(3 * std::mem::size_of::<usize>()))
        .ok_or_else(|| GraphError::Internal("GQL seen-key estimate overflowed".to_string()))?;
    let intermediate_per_row = target_bytes
        .checked_add(
            path_state_bytes.checked_mul(2).ok_or_else(|| {
                GraphError::Internal("GQL frontier estimate overflowed".to_string())
            })?,
        )
        .and_then(|bytes| bytes.checked_add(seen_key_bytes))
        .ok_or_else(|| GraphError::Internal("GQL intermediate estimate overflowed".to_string()))?;
    let bounded_rows = row_cap
        .checked_add(1)
        .ok_or_else(|| GraphError::Internal("GQL bounded row count overflowed".to_string()))?;
    let intermediate_bytes = bounded_rows
        .checked_mul(intermediate_per_row)
        .ok_or_else(|| GraphError::Internal("GQL intermediate workspace overflowed".to_string()))?;
    let join_state_bytes = std::mem::size_of::<JoinState>()
        .checked_add(
            coordinates_per_row
                .checked_mul(std::mem::size_of::<Option<u32>>())
                .ok_or_else(|| {
                    GraphError::Internal("GQL join-node state overflowed".to_string())
                })?,
        )
        .and_then(|bytes| {
            relationships_per_row
                .checked_mul(
                    std::mem::size_of::<Option<Vec<GqlRelationshipStep>>>()
                        + std::mem::size_of::<GqlRelationshipStep>(),
                )
                .and_then(|relationships| bytes.checked_add(relationships))
        })
        .ok_or_else(|| GraphError::Internal("GQL join-state estimate overflowed".to_string()))?;
    let recursion_depth = relationships_per_row.max(1);
    let recursive_bytes = path_state_bytes
        .checked_add(join_state_bytes)
        .and_then(|bytes| bytes.checked_mul(recursion_depth))
        .ok_or_else(|| GraphError::Internal("GQL recursion workspace overflowed".to_string()))?;
    let tx_nodes = crate::projection::tx_delta::stats().added_nodes;
    let candidate_count = usize::try_from(engine.node_store.node_count())
        .ok()
        .and_then(|count| count.checked_add(tx_nodes))
        .ok_or_else(|| GraphError::Internal("GQL candidate count overflowed".to_string()))?;
    let candidate_bytes = candidate_count
        .checked_mul(std::mem::size_of::<u32>() + 3 * std::mem::size_of::<usize>())
        .ok_or_else(|| GraphError::Internal("GQL candidate workspace overflowed".to_string()))?;
    // Recursive join/wildcard expansion retains one sorted neighbor vector at
    // each path depth while descending. `relationships_per_row` is a
    // conservative upper bound for that depth for every physical plan shape.
    let neighbor_depth = relationships_per_row.max(1);
    let degree_bytes = usize::try_from(engine.edge_store.edge_count())
        .ok()
        .and_then(|edges| edges.checked_mul(std::mem::size_of::<GqlStepTarget>() * 2))
        .and_then(|bytes| bytes.checked_mul(neighbor_depth))
        .ok_or_else(|| GraphError::Internal("GQL degree workspace overflowed".to_string()))?;
    let overlay_bytes = engine.estimated_traversal_overlay_clone_bytes()?;
    let bytes = result_bytes
        .checked_add(intermediate_bytes)
        .and_then(|total| total.checked_add(recursive_bytes))
        .and_then(|total| total.checked_add(candidate_bytes))
        .and_then(|total| total.checked_add(degree_bytes))
        .and_then(crate::resource::ByteCount::from_usize)
        .and_then(|total| total.checked_add(overlay_bytes))
        .ok_or_else(|| GraphError::Internal("GQL workspace estimate overflowed".to_string()))?;
    governor
        .reserve_memory(phase, bytes)
        .map_err(crate::safety::resource_limit_error)
}

fn one_hop_row_shape(max_hops: u32) -> GraphResult<(usize, usize)> {
    let hops = usize::try_from(max_hops)
        .map_err(|_| GraphError::Internal("GQL hop bound does not fit usize".to_string()))?;
    let coordinates = hops
        .checked_mul(3)
        .and_then(|coordinates| coordinates.checked_add(5))
        .ok_or_else(|| GraphError::Internal("GQL row shape overflowed".to_string()))?;
    Ok((coordinates, hops))
}

fn wildcard_row_shape(plan: &PhysicalWildcardPathPlan) -> GraphResult<(usize, usize)> {
    let hops = plan.segments.iter().try_fold(0usize, |total, segment| {
        usize::try_from(segment.hops.max)
            .ok()
            .and_then(|hops| total.checked_add(hops))
            .ok_or_else(|| GraphError::Internal("wildcard path shape overflowed".to_string()))
    })?;
    let coordinates = hops
        .checked_mul(3)
        .and_then(|coordinates| coordinates.checked_add(5))
        .ok_or_else(|| GraphError::Internal("wildcard row shape overflowed".to_string()))?;
    Ok((coordinates, hops))
}

fn join_row_shape(plan: &PhysicalJoinPlan) -> GraphResult<(usize, usize)> {
    let (hops, path_nodes) =
        plan.patterns
            .iter()
            .try_fold((0usize, 0usize), |(total_hops, total_nodes), pattern| {
                let hops = usize::try_from(pattern.hops.max).map_err(|_| {
                    GraphError::Internal("join hop bound does not fit usize".to_string())
                })?;
                Ok::<_, GraphError>((
                    total_hops.checked_add(hops).ok_or_else(|| {
                        GraphError::Internal("join relationship shape overflowed".to_string())
                    })?,
                    total_nodes
                        .checked_add(hops.checked_add(1).ok_or_else(|| {
                            GraphError::Internal("join path shape overflowed".to_string())
                        })?)
                        .ok_or_else(|| {
                            GraphError::Internal("join path shape overflowed".to_string())
                        })?,
                ))
            })?;
    let relationships = hops
        .checked_mul(2)
        .and_then(|count| count.checked_add(plan.patterns.len()))
        .ok_or_else(|| GraphError::Internal("join row shape overflowed".to_string()))?;
    let coordinates = plan
        .node_slots
        .len()
        .checked_mul(2)
        .and_then(|count| count.checked_add(path_nodes))
        .and_then(|count| count.checked_add(2))
        .and_then(|count| {
            relationships
                .checked_mul(2)
                .and_then(|endpoints| count.checked_add(endpoints))
        })
        .ok_or_else(|| GraphError::Internal("join coordinate shape overflowed".to_string()))?;
    Ok((coordinates, relationships))
}

fn source_nodes<'a>(
    engine: &'a Engine,
    table_oid: u32,
    tenant: Option<&'a str>,
) -> impl Iterator<Item = u32> + 'a {
    let nodes: Box<dyn Iterator<Item = u32> + 'a> =
        if let Some(nodes) = engine.table_membership.get(&table_oid) {
            Box::new(nodes.iter())
        } else {
            Box::new(
                (0..engine.node_store.node_count())
                    .filter(move |&idx| engine.node_store.table_oid(idx) == Some(table_oid)),
            )
        };
    let table_is_tenanted = engine.tenanted_table_oids.contains(&table_oid);
    let added =
        crate::projection::tx_delta::added_node_indexes(table_oid, tenant, table_is_tenanted);
    nodes
        .chain(added)
        .filter(move |&idx| tenant_allows_node(engine, idx, tenant))
}

fn consume_query_work(
    governor: &crate::resource::ResourceGovernor,
    phase: crate::resource::ResourcePhase,
) -> GraphResult<()> {
    governor
        .consume_work(phase, crate::resource::WorkUnits::new(1))
        .map_err(crate::safety::resource_limit_error)?;
    if governor.work_used().as_u64().is_multiple_of(1_024) {
        crate::resource::check_postgres_interrupts();
        governor
            .check_elapsed(phase)
            .map_err(crate::safety::resource_limit_error)?;
    }
    Ok(())
}

struct TargetExpansion<'a> {
    neighbors: &'a GqlNeighbors<'a>,
    engine: &'a Engine,
    plan: &'a PhysicalPlan,
    rel_type_id: u8,
    tenant: Option<&'a str>,
    result_cap: usize,
    governor: &'a crate::resource::ResourceGovernor,
}

fn expand_targets(expansion: &TargetExpansion<'_>, source_idx: u32) -> GraphResult<Vec<GqlTarget>> {
    let TargetExpansion {
        neighbors,
        engine,
        plan,
        rel_type_id,
        tenant,
        result_cap,
        governor,
    } = expansion;
    let mut results = Vec::new();
    let preserve_path_matches = plan.hops.variable;
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
            for target in neighbors.for_direction(plan.direction, state.node_idx, *rel_type_id) {
                consume_query_work(governor, crate::resource::ResourcePhase::QueryExpand)?;
                if !node_active(engine, target.node_idx)
                    || crate::projection::tx_delta::node_deleted(target.node_idx)
                    || !tenant_allows_node(engine, target.node_idx, *tenant)
                {
                    continue;
                }
                if preserve_path_matches && state.path_nodes.contains(&target.node_idx) {
                    continue;
                }
                let next_state = state.push(target);
                if depth >= plan.hops.min
                    && target_matches(engine, target.node_idx, plan.target_table_oid, *tenant)
                {
                    if results.len() >= *result_cap {
                        if plan.cap_exhaustion_is_error() {
                            return Err(GraphError::GqlExecution {
                                reason: format!(
                                    "GQL result row cap exceeded ({})",
                                    plan.execution_row_cap()
                                ),
                            });
                        }
                        return Ok(results);
                    }
                    results.push(GqlTarget {
                        node_idx: target.node_idx,
                        orientation: target.orientation,
                        type_id: target.type_id,
                        schema_reversed: target.schema_reversed,
                        relationship_id: target.relationship_id,
                        path_nodes: next_state.path_nodes.clone(),
                        path_relationships: next_state.path_relationships.clone(),
                    });
                }
                if depth < plan.hops.max
                    && (preserve_path_matches
                        || (seen_frontier.insert(target.node_idx)
                            && seen_next.insert(target.node_idx)))
                {
                    if next.len() >= plan.execution_row_cap() {
                        return Err(GraphError::ResourceLimit {
                            resource: "path states".to_string(),
                            phase: crate::resource::ResourcePhase::QueryPaths
                                .as_str()
                                .to_string(),
                            used: next.len() as u64,
                            requested: 1,
                            limit: plan.execution_row_cap() as u64,
                        });
                    }
                    next.push(next_state);
                }
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    Ok(results)
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
            relationship_id: target.relationship_id,
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
        let layered = engine.layered_neighbors()?;
        let (out_overlay, in_overlay) = if layered.is_some() || !engine.has_edge_overlay() {
            (None, None)
        } else {
            (
                Some(engine.traversal_edge_overlay(TraversalDirection::Out)),
                Some(engine.traversal_edge_overlay(TraversalDirection::In)),
            )
        };

        Ok(Self {
            out_store: &engine.edge_store,
            in_store: &engine.reverse_edge_store,
            out_overlay,
            in_overlay,
            layered,
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
        if direction == BoundDirection::Undirected {
            sort_and_deduplicate_undirected(&mut neighbors);
        } else {
            neighbors.sort_by_key(|target| {
                (
                    target.node_idx,
                    target.orientation,
                    target.schema_reversed,
                    target.relationship_id,
                )
            });
        }
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
        if direction == BoundDirection::Undirected {
            sort_and_deduplicate_undirected(&mut neighbors);
        } else {
            neighbors.sort_by_key(|target| {
                (
                    target.node_idx,
                    target.orientation,
                    target.type_id,
                    target.schema_reversed,
                    target.relationship_id,
                )
            });
        }
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
    relationship_id: Option<RelationshipId>,
    path_nodes: Vec<u32>,
    path_relationships: Vec<GqlRelationshipStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GqlStepTarget {
    node_idx: u32,
    orientation: EdgeOrientation,
    type_id: u8,
    schema_reversed: bool,
    relationship_id: Option<RelationshipId>,
}

fn sort_and_deduplicate_undirected(neighbors: &mut Vec<GqlStepTarget>) {
    neighbors.sort_by_key(|target| {
        (
            target.node_idx,
            target.type_id,
            target.relationship_id,
            target.orientation,
            target.schema_reversed,
        )
    });
    neighbors.dedup_by(|right, left| {
        left.node_idx == right.node_idx
            && left.type_id == right.type_id
            && left.relationship_id.is_some()
            && left.relationship_id == right.relationship_id
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GqlRelationshipStep {
    from_idx: u32,
    to_idx: u32,
    orientation: EdgeOrientation,
    type_id: u8,
    schema_reversed: bool,
    relationship_id: Option<RelationshipId>,
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
            relationship_id: neighbor.relationship_id,
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
        relationship_id: neighbor.relationship_id,
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
        || source_nodes(engine, plan.node_slots[slot].table_oid, tenant).collect(),
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
    node_table_oid(engine, target_idx).is_some_and(|node_table_oid| node_table_oid == table_oid)
        && node_active(engine, target_idx)
        && tenant_allows_node(engine, target_idx, tenant)
}

fn tenant_allows_node(engine: &Engine, node_idx: u32, tenant: Option<&str>) -> bool {
    let Some(tenant) = tenant else {
        return true;
    };
    let Some(table_oid) = node_table_oid(engine, node_idx) else {
        return false;
    };
    !engine.tenanted_table_oids.contains(&table_oid)
        || engine.tenant_contains(tenant, node_idx)
        || crate::projection::tx_delta::added_node_by_index(node_idx)
            .and_then(|node| node.tenant)
            .is_some_and(|created| created == tenant)
}

fn node_active(engine: &Engine, node_idx: u32) -> bool {
    if node_idx < engine.node_store.node_count() {
        engine.node_store.is_active(node_idx)
    } else {
        crate::projection::tx_delta::added_node_by_index(node_idx).is_some()
    }
}

fn node_table_oid(engine: &Engine, node_idx: u32) -> Option<u32> {
    if node_idx < engine.node_store.node_count() {
        engine.node_store.table_oid(node_idx)
    } else {
        crate::projection::tx_delta::added_node_by_index(node_idx).map(|node| node.table_oid)
    }
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
        source: coordinate(engine, source_idx)?,
        target: Some(coordinate(engine, target_idx)?),
        rel_start: Some(coordinate(engine, rel_start_idx)?),
        rel_end: Some(coordinate(engine, rel_end_idx)?),
        relationship_id: target.relationship_id,
        path_nodes: target
            .path_nodes
            .into_iter()
            .map(|node_idx| coordinate(engine, node_idx))
            .collect::<GraphResult<Vec<_>>>()?,
        path_relationships: target
            .path_relationships
            .into_iter()
            .map(|relationship| {
                let (start_idx, end_idx) = canonical_step_endpoints(&relationship);
                Ok(GqlPathRelationship {
                    rel_type: edge_type_label(engine, relationship.type_id)?,
                    start: coordinate(engine, start_idx)?,
                    end: coordinate(engine, end_idx)?,
                    relationship_id: relationship.relationship_id,
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
        .map(|node| {
            node.map(|node_idx| coordinate(engine, node_idx))
                .transpose()
        })
        .collect::<GraphResult<Vec<_>>>()?;
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
            relationships
                .as_ref()
                .map(|relationships| {
                    let mut nodes = Vec::with_capacity(relationships.len() + 1);
                    if let Some(first) = relationships.first() {
                        nodes.push(coordinate(engine, first.from_idx)?);
                    }
                    nodes.extend(
                        relationships
                            .iter()
                            .map(|relationship| coordinate(engine, relationship.to_idx))
                            .collect::<GraphResult<Vec<_>>>()?,
                    );
                    Ok(nodes)
                })
                .transpose()
        })
        .collect::<GraphResult<Vec<_>>>()?;
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
        relationship_id: None,
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
        start: coordinate(engine, start_idx)?,
        end: coordinate(engine, end_idx)?,
        relationship_id: relationship.relationship_id,
    })
}

fn project_wildcard_path_state(engine: &Engine, state: PathState) -> GraphResult<GqlRow> {
    let path_nodes = state
        .path_nodes
        .iter()
        .map(|node_idx| coordinate(engine, *node_idx))
        .collect::<GraphResult<Vec<_>>>()?;
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
                start: coordinate(engine, start_idx)?,
                end: coordinate(engine, end_idx)?,
                relationship_id: relationship.relationship_id,
            })
        })
        .collect::<GraphResult<Vec<_>>>()?;
    let (rel_start, rel_end) = path_relationships
        .first()
        .map(|relationship| (relationship.start.clone(), relationship.end.clone()))
        .unzip();
    let relationship_id = path_relationships
        .first()
        .and_then(|relationship| relationship.relationship_id);
    Ok(GqlRow {
        source,
        target,
        rel_start,
        rel_end,
        relationship_id,
        path_nodes,
        path_relationships,
        join_node_slots: None,
        join_path_nodes: None,
        join_relationships: None,
        join_path_relationships: None,
    })
}

fn project_optional_row(engine: &Engine, source_idx: u32) -> GraphResult<GqlRow> {
    Ok(GqlRow {
        source: coordinate(engine, source_idx)?,
        target: None,
        rel_start: None,
        rel_end: None,
        relationship_id: None,
        path_nodes: Vec::new(),
        path_relationships: Vec::new(),
        join_node_slots: None,
        join_path_nodes: None,
        join_relationships: None,
        join_path_relationships: None,
    })
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

fn coordinate(engine: &Engine, node_idx: u32) -> GraphResult<GqlNodeCoordinate> {
    if let Some(node) = crate::projection::tx_delta::added_node_by_index(node_idx) {
        return Ok(GqlNodeCoordinate {
            table_oid: node.table_oid,
            node_id: node.primary_key,
        });
    }
    let table_oid =
        engine
            .node_store
            .table_oid(node_idx)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("node index {node_idx} has no table OID metadata"),
            })?;
    let node_id =
        engine
            .node_store
            .primary_key(node_idx)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("node index {node_idx} has no primary-key metadata"),
            })?;
    Ok(GqlNodeCoordinate {
        table_oid,
        node_id: node_id.to_string(),
    })
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

#[cfg(test)]
mod resource_accounting_tests {
    use super::*;
    use crate::resource::{
        ByteCount, DiskBudget, ElapsedBudget, MemoryBudget, ResourceGovernor, ResourceLimits,
        RowCount, WorkUnits,
    };
    use std::time::Duration;

    #[test]
    fn execution_preflights_long_ids_and_path_vectors() {
        let mut engine = Engine::new();
        engine.node_store.add_node(10, "x".repeat(4_096));
        let governor = ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_mib(1).expect("test memory fits")),
            DiskBudget::new(ByteCount::from_mib(1).expect("test disk fits")),
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::MAX),
        ));

        let error = match reserve_execution_rows(
            &governor,
            &engine,
            10_000,
            one_hop_row_shape(128).expect("bounded path shape"),
            crate::resource::ResourcePhase::QueryPaths,
        ) {
            Ok(_) => panic!("long path results must not fit a one MiB query budget"),
            Err(error) => error,
        };

        assert!(matches!(error, GraphError::ResourceLimit { .. }));
    }
}
