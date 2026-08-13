//! SQL-layer aggregation and exact path-count orchestration.

use crate::api_types::{
    AggregateAccumulator, AggregateKind, AggregateSpec, AggregationTraversalRequest,
    TraverseRequest, TraverseRow,
};
use crate::catalog::{table_oid_from_name, validate_column_exists};
use crate::projection::layered::LayeredNeighbors;
use crate::projection::neighbors::{Neighbor, NeighborSource, OverlayNeighbors};
use crate::sql_hydration::{hydrate_node_governed_with_tables, hydrate_nodes_governed_with_tables};
use crate::sql_traversal::{
    execute_traverse_rows_in_context, json_i32_field, json_number_as_f64, json_number_from_f64,
    optional_string_array, parse_node_ref_json_string, path_node_field, required_string_field,
    usize_from_nonnegative,
};
use crate::{acl, safety, sql_facade::check_enabled_result, types, Engine, ENGINE};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

type AggregationEdgeOverlay = (
    crate::projection::neighbors::EdgeOverlay,
    crate::projection::neighbors::EdgeOverlay,
);
pub(crate) type IndexedPath = Rc<[u32]>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AggregateScope {
    ReturnedNodes,
    ChosenParentPath,
    AllPossiblePaths,
}

impl AggregateScope {
    fn expands_parent_path(self) -> bool {
        matches!(self, Self::ChosenParentPath)
    }
}

pub(crate) fn aggregate_impl(
    traversal: &serde_json::Value,
    aggregations: &serde_json::Value,
    scope: &str,
    path_limit: i32,
    tables: &[crate::builder::RegisteredTable],
    edges: &[crate::builder::RegisteredEdge],
    filter_columns: &[crate::builder::RegisteredFilterColumn],
) -> safety::GraphResult<serde_json::Value> {
    check_enabled_result()?;
    check_analytics_source_acls(tables, edges)?;
    let request = parse_aggregation_traversal_request(traversal)?;
    let specs = parse_aggregation_specs(aggregations)?;
    let scope = parse_aggregate_scope(scope)?;
    let path_limit = usize_from_nonnegative(path_limit, "path_limit")?;
    let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    let visibility = crate::sql_visibility::build_visibility_scope(tables, edges, &governor)?;
    let context = crate::visibility::QueryExecutionContext::new(&governor, &visibility);
    match scope {
        AggregateScope::ReturnedNodes | AggregateScope::ChosenParentPath => {}
        AggregateScope::AllPossiblePaths => {
            let (paths, _exact, capped) =
                indexed_paths_for_request_in_context(&request, path_limit, &context)?;
            if capped {
                return Err(safety::GraphError::InvalidFilter {
                    reason: format!(
                        "all_possible_paths expansion exceeds graph.max_exact_path_count ({})",
                        path_limit
                    ),
                });
            }
            return aggregate_indexed_paths_governed(&paths, specs, &governor, tables);
        }
    }

    let rows = execute_aggregation_traversal_governed(
        &request,
        path_limit,
        &context,
        tables,
        filter_columns,
    )?;
    let rows = rows
        .into_iter()
        .filter(|row| row.4 >= request.min_depth)
        .collect::<Vec<_>>();
    let aggregate_rows = if scope.expands_parent_path() {
        expand_rows_to_parent_path_governed(rows, &governor, tables)?
    } else {
        rows
    };
    let mut accumulators = specs
        .iter()
        .map(|spec| (spec.alias.clone(), AggregateAccumulator::default()))
        .collect::<HashMap<_, _>>();

    for row in aggregate_rows {
        let node_table = row.2.to_u32();
        let Some(node) = row.7.as_ref() else {
            continue;
        };
        for spec in specs.iter().filter(|spec| spec.table_oid == node_table) {
            let value = node.0.get(&spec.column);
            let Some(acc) = accumulators.get_mut(&spec.alias) else {
                continue;
            };
            match spec.kind {
                AggregateKind::Count | AggregateKind::Sum | AggregateKind::Avg => {
                    accumulate_json_value(acc, spec.kind, value);
                }
            }
        }
    }

    aggregate_output(specs, accumulators)
}

pub(crate) fn accumulate_json_value(
    acc: &mut AggregateAccumulator,
    kind: AggregateKind,
    value: Option<&serde_json::Value>,
) {
    match kind {
        AggregateKind::Count => {
            if value.is_some_and(|value| !value.is_null()) {
                acc.count += 1;
            }
        }
        AggregateKind::Sum | AggregateKind::Avg => {
            if let Some(number) = value.and_then(json_number_as_f64) {
                acc.sum += number;
                acc.count += 1;
            }
        }
    }
}

pub(crate) fn aggregate_output(
    specs: Vec<AggregateSpec>,
    mut accumulators: HashMap<String, AggregateAccumulator>,
) -> safety::GraphResult<serde_json::Value> {
    let mut output = serde_json::Map::new();
    for spec in specs {
        let acc = accumulators.remove(&spec.alias).unwrap_or_default();
        let value = match spec.kind {
            AggregateKind::Count => serde_json::Value::from(acc.count),
            AggregateKind::Sum => json_number_from_f64(acc.sum)?,
            AggregateKind::Avg => {
                if acc.count == 0 {
                    serde_json::Value::Null
                } else {
                    json_number_from_f64(acc.sum / acc.count as f64)?
                }
            }
        };
        output.insert(spec.alias, value);
    }
    Ok(serde_json::Value::Object(output))
}

pub(crate) fn path_count_estimate_impl(
    traversal: &serde_json::Value,
    path_limit: i32,
    tables: &[crate::builder::RegisteredTable],
    edges: &[crate::builder::RegisteredEdge],
) -> safety::GraphResult<(i64, bool, bool)> {
    check_enabled_result()?;
    check_analytics_source_acls(tables, edges)?;
    let request = parse_aggregation_traversal_request(traversal)?;
    let path_limit = usize_from_nonnegative(path_limit, "graph.max_exact_path_count")?;
    let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    let visibility = crate::sql_visibility::build_visibility_scope(tables, edges, &governor)?;
    let context = crate::visibility::QueryExecutionContext::new(&governor, &visibility);
    path_count_for_request_in_context(&request, path_limit, &context)
}

fn check_analytics_source_acls(
    tables: &[crate::builder::RegisteredTable],
    edges: &[crate::builder::RegisteredEdge],
) -> safety::GraphResult<()> {
    acl::check_table_acls(
        tables
            .iter()
            .map(|table| table.table_oid)
            .chain(edges.iter().map(|edge| edge.from_table_oid)),
    )
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn path_count_for_request(
    request: &AggregationTraversalRequest,
    path_limit: usize,
) -> safety::GraphResult<(i64, bool, bool)> {
    let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    let visibility = crate::visibility::VisibilityScope::Unrestricted;
    let context = crate::visibility::QueryExecutionContext::new(&governor, &visibility);
    path_count_for_request_in_context(request, path_limit, &context)
}

fn path_count_for_request_in_context(
    request: &AggregationTraversalRequest,
    path_limit: usize,
    context: &crate::visibility::QueryExecutionContext<'_>,
) -> safety::GraphResult<(i64, bool, bool)> {
    let (paths, exact, capped) =
        indexed_paths_for_request_in_context(request, path_limit, context)?;
    if capped || !exact {
        Ok((path_limit as i64, false, true))
    } else {
        Ok((paths.len() as i64, true, false))
    }
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn indexed_paths_for_request(
    request: &AggregationTraversalRequest,
    path_limit: usize,
) -> safety::GraphResult<(Vec<IndexedPath>, bool, bool)> {
    let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    let visibility = crate::visibility::VisibilityScope::Unrestricted;
    let context = crate::visibility::QueryExecutionContext::new(&governor, &visibility);
    indexed_paths_for_request_in_context(request, path_limit, &context)
}

fn indexed_paths_for_request_in_context(
    request: &AggregationTraversalRequest,
    path_limit: usize,
    context: &crate::visibility::QueryExecutionContext<'_>,
) -> safety::GraphResult<(Vec<IndexedPath>, bool, bool)> {
    let governor = context.governor;
    let edge_limit = path_limit.saturating_add(1);
    let path_width = usize::try_from(request.max_depth.max(0))
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let path_bytes = edge_limit
        .checked_mul(path_width)
        .and_then(|slots| slots.checked_mul(std::mem::size_of::<u32>()))
        .and_then(|bytes| bytes.checked_mul(2))
        .and_then(|bytes| bytes.checked_add(edge_limit.saturating_mul(192)))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("exact-path workspace overflowed".to_string())
        })?;
    let path_lease = governor
        .reserve_memory(crate::resource::ResourcePhase::QueryPaths, path_bytes)
        .map_err(crate::safety::resource_limit_error)?;
    let node_table_filter = request
        .node_tables
        .as_ref()
        .filter(|tables| !tables.is_empty())
        .map(|tables| tables.iter().copied().collect::<HashSet<_>>());

    let indexed_paths = ENGINE.with(|engine| {
        let eng = engine.borrow();
        if !eng.built {
            return Err(safety::GraphError::NotBuilt);
        }
        let max_base_degree =
            (0..eng.node_store.node_count()).try_fold(0usize, |maximum, node| {
                governor
                    .consume_work(
                        crate::resource::ResourcePhase::QueryPaths,
                        crate::resource::WorkUnits::new(1),
                    )
                    .map_err(crate::safety::resource_limit_error)?;
                let outgoing = eng.edge_store.neighbors(node).0.len();
                let incoming = eng.reverse_edge_store.neighbors(node).0.len();
                let degree = match request.direction {
                    types::TraversalDirection::Out => outgoing,
                    types::TraversalDirection::In => incoming,
                    types::TraversalDirection::Any => outgoing.saturating_add(incoming),
                };
                Ok::<_, safety::GraphError>(maximum.max(degree))
            })?;
        let overlay_per_frame = eng.analytics_additional_neighbor_upper_bound(request.direction);
        let scratch_bytes = max_base_degree
            .saturating_add(overlay_per_frame)
            .checked_mul(path_width)
            .and_then(|slots| slots.checked_mul(96))
            .and_then(crate::resource::ByteCount::from_usize)
            .ok_or_else(|| {
                safety::GraphError::Internal("exact-path neighbor workspace overflowed".to_string())
            })?;
        let _scratch = governor
            .reserve_memory(crate::resource::ResourcePhase::QueryExpand, scratch_bytes)
            .map_err(crate::safety::resource_limit_error)?;
        let _overlay = governor
            .reserve_memory(
                crate::resource::ResourcePhase::QueryExpand,
                eng.estimated_traversal_overlay_clone_bytes()?,
            )
            .map_err(crate::safety::resource_limit_error)?;
        let layered_neighbors = eng.layered_neighbors()?;
        let edge_overlays = if layered_neighbors.is_some() {
            Default::default()
        } else {
            aggregation_edge_overlay(&eng)
        };
        let edge_type_filter = aggregation_edge_type_filter(&eng, request)?;
        let mut paths = Vec::new();
        let mut seen_paths = HashSet::new();
        for start in &request.starts {
            let seed = eng
                .resolve(start.table_oid.0, &start.node_id)
                .ok_or_else(|| safety::GraphError::NodeNotFound {
                    table: start.table_oid.to_string(),
                    pk: start.node_id.clone(),
                })?;
            if !context.visibility.allows_node(seed) {
                continue;
            }
            let mut path = vec![seed];
            enumerate_all_paths_dfs(
                &eng,
                request,
                seed,
                0,
                &mut path,
                &mut paths,
                &mut seen_paths,
                edge_limit,
                edge_type_filter.as_ref(),
                node_table_filter.as_ref(),
                &edge_overlays,
                layered_neighbors.as_ref(),
                context,
            )?;
            if paths.len() > path_limit {
                break;
            }
        }
        Ok::<_, safety::GraphError>(paths)
    })?;

    let result = if indexed_paths.len() > path_limit {
        (
            indexed_paths.into_iter().take(path_limit).collect(),
            false,
            true,
        )
    } else {
        (indexed_paths, true, false)
    };
    path_lease.retain_until_governor_drop();
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn enumerate_all_paths_dfs(
    eng: &Engine,
    request: &AggregationTraversalRequest,
    current: u32,
    depth: i32,
    path: &mut Vec<u32>,
    paths: &mut Vec<IndexedPath>,
    seen_paths: &mut HashSet<IndexedPath>,
    edge_limit: usize,
    edge_type_filter: Option<&HashSet<u8>>,
    node_table_filter: Option<&HashSet<u32>>,
    edge_overlays: &AggregationEdgeOverlay,
    layered_neighbors: Option<&LayeredNeighbors<'_>>,
    context: &crate::visibility::QueryExecutionContext<'_>,
) -> safety::GraphResult<()> {
    let governor = context.governor;
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryPaths,
            crate::resource::WorkUnits::new(1),
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .check_elapsed(crate::resource::ResourcePhase::QueryPaths)
        .map_err(crate::safety::resource_limit_error)?;
    if depth >= request.min_depth
        && node_table_filter.is_none_or(|tables| {
            eng.node_store
                .table_oid(current)
                .is_some_and(|table_oid| tables.contains(&table_oid))
        })
    {
        record_indexed_path(path, paths, seen_paths);
        if paths.len() >= edge_limit {
            return Ok(());
        }
    }
    if depth >= request.max_depth {
        return Ok(());
    }

    for edge in aggregation_neighbors(
        eng,
        current,
        request.direction,
        edge_overlays,
        layered_neighbors,
    ) {
        if paths.len() >= edge_limit {
            return Ok(());
        }
        if edge_type_filter.is_some_and(|allowed| !allowed.contains(&edge.type_id)) {
            continue;
        }
        if !context
            .visibility
            .allows_relationship(edge.type_id, edge.relationship_id)?
            || !context.visibility.allows_node(edge.target)
            || !eng.node_store.is_active(edge.target)
            || crate::projection::tx_delta::node_deleted(edge.target)
            || path.contains(&edge.target)
        {
            continue;
        }
        path.push(edge.target);
        enumerate_all_paths_dfs(
            eng,
            request,
            edge.target,
            depth + 1,
            path,
            paths,
            seen_paths,
            edge_limit,
            edge_type_filter,
            node_table_filter,
            edge_overlays,
            layered_neighbors,
            context,
        )?;
        path.pop();
    }
    Ok(())
}

fn record_indexed_path(
    path: &[u32],
    paths: &mut Vec<IndexedPath>,
    seen_paths: &mut HashSet<IndexedPath>,
) {
    if seen_paths.contains(path) {
        return;
    }

    let path_snapshot = IndexedPath::from(path);
    seen_paths.insert(Rc::clone(&path_snapshot));
    paths.push(path_snapshot);
}

pub(crate) fn aggregation_edge_type_filter(
    eng: &Engine,
    request: &AggregationTraversalRequest,
) -> safety::GraphResult<Option<HashSet<u8>>> {
    let Some(edge_types) = request
        .edge_types
        .as_ref()
        .filter(|types| !types.is_empty())
    else {
        return Ok(None);
    };
    let mut ids = HashSet::new();
    for edge_type in edge_types {
        let Some(pos) = eng
            .edge_type_registry
            .iter()
            .position(|label| label == edge_type)
        else {
            return Err(safety::GraphError::InvalidFilter {
                reason: format!("unknown edge type '{}'", edge_type),
            });
        };
        ids.insert(pos as u8);
    }
    Ok(Some(ids))
}

pub(crate) fn aggregation_edge_overlay(eng: &Engine) -> AggregationEdgeOverlay {
    (
        eng.traversal_edge_overlay(types::TraversalDirection::Out),
        eng.traversal_edge_overlay(types::TraversalDirection::In),
    )
}

pub(crate) fn aggregation_neighbors(
    eng: &Engine,
    current: u32,
    direction: types::TraversalDirection,
    edge_overlays: &AggregationEdgeOverlay,
    layered_neighbors: Option<&LayeredNeighbors<'_>>,
) -> Vec<Neighbor> {
    if let Some(layered) = layered_neighbors {
        return layered
            .for_direction(direction)
            .neighbors(current)
            .collect();
    }
    let mut neighbors = Vec::new();
    let mut seen = HashSet::new();
    if matches!(
        direction,
        types::TraversalDirection::Out | types::TraversalDirection::Any
    ) {
        let source =
            OverlayNeighbors::new(&eng.edge_store, &(edge_overlays.0).0, &(edge_overlays.0).1);
        for edge in source.neighbors(current) {
            if seen.insert((edge.target, edge.type_id, edge.relationship_id)) {
                neighbors.push(edge);
            }
        }
    }
    if matches!(
        direction,
        types::TraversalDirection::In | types::TraversalDirection::Any
    ) {
        let source = OverlayNeighbors::new(
            &eng.reverse_edge_store,
            &(edge_overlays.1).0,
            &(edge_overlays.1).1,
        );
        for edge in source.neighbors(current) {
            if seen.insert((edge.target, edge.type_id, edge.relationship_id)) {
                neighbors.push(edge);
            }
        }
    }
    neighbors
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn aggregate_indexed_paths(
    paths: &[IndexedPath],
    specs: Vec<AggregateSpec>,
) -> safety::GraphResult<serde_json::Value> {
    let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    let (tables, _edges, _filter_columns) = crate::catalog::read_catalog()?;
    aggregate_indexed_paths_governed(paths, specs, &governor, &tables)
}

fn aggregate_indexed_paths_governed(
    paths: &[IndexedPath],
    specs: Vec<AggregateSpec>,
    governor: &crate::resource::ResourceGovernor,
    tables: &[crate::builder::RegisteredTable],
) -> safety::GraphResult<serde_json::Value> {
    let coordinates_by_idx = indexed_path_coordinates(paths)?;
    let hydrated = hydrate_indexed_path_nodes_governed(&coordinates_by_idx, governor, tables)?;
    let mut accumulators = specs
        .iter()
        .map(|spec| (spec.alias.clone(), AggregateAccumulator::default()))
        .collect::<HashMap<_, _>>();

    for path in paths {
        for idx in path.iter() {
            governor
                .consume_work(
                    crate::resource::ResourcePhase::QueryBlocking,
                    crate::resource::WorkUnits::new(1),
                )
                .map_err(crate::safety::resource_limit_error)?;
            let Some(coord) = coordinates_by_idx.get(idx) else {
                continue;
            };
            let Some(table_nodes) = hydrated.get(&coord.table_oid.0) else {
                continue;
            };
            let Some(node) = table_nodes.get(coord.node_id.as_str()) else {
                continue;
            };
            for spec in specs
                .iter()
                .filter(|spec| spec.table_oid == coord.table_oid.0)
            {
                let value = node.0.get(&spec.column);
                let Some(acc) = accumulators.get_mut(&spec.alias) else {
                    continue;
                };
                accumulate_json_value(acc, spec.kind, value);
            }
        }
    }

    aggregate_output(specs, accumulators)
}

#[allow(dead_code, reason = "compatibility entry point")]
fn hydrate_indexed_path_nodes(
    coordinates_by_idx: &HashMap<u32, types::PathCoordinate>,
) -> safety::GraphResult<HashMap<u32, HashMap<String, pgrx::JsonB>>> {
    let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    let (tables, _edges, _filter_columns) = crate::catalog::read_catalog()?;
    hydrate_indexed_path_nodes_governed(coordinates_by_idx, &governor, &tables)
}

fn hydrate_indexed_path_nodes_governed(
    coordinates_by_idx: &HashMap<u32, types::PathCoordinate>,
    governor: &crate::resource::ResourceGovernor,
    tables: &[crate::builder::RegisteredTable],
) -> safety::GraphResult<HashMap<u32, HashMap<String, pgrx::JsonB>>> {
    let unique_rows = coordinates_by_idx
        .values()
        .map(|coord| types::TraversalResult {
            node_table: coord.table_oid,
            node_id: coord.node_id.clone(),
            depth: 0,
            path: Vec::new(),
            edge_path: Vec::new(),
        })
        .collect::<Vec<_>>();
    hydrate_nodes_governed_with_tables(&unique_rows, governor, tables)
        .map(group_hydrated_nodes_by_table)
}

fn group_hydrated_nodes_by_table(
    hydrated: HashMap<(u32, String), pgrx::JsonB>,
) -> HashMap<u32, HashMap<String, pgrx::JsonB>> {
    let mut grouped = HashMap::new();
    for ((table_oid, node_id), node) in hydrated {
        grouped
            .entry(table_oid)
            .or_insert_with(HashMap::new)
            .insert(node_id, node);
    }
    grouped
}

pub(crate) fn indexed_path_coordinates(
    paths: &[IndexedPath],
) -> safety::GraphResult<HashMap<u32, types::PathCoordinate>> {
    ENGINE.with(|engine| {
        let eng = engine.borrow();
        if !eng.built {
            return Err(safety::GraphError::NotBuilt);
        }
        let mut coordinates = HashMap::new();
        for idx in paths.iter().flat_map(|path| path.iter().copied()) {
            let table_oid =
                eng.node_store
                    .table_oid(idx)
                    .ok_or_else(|| safety::GraphError::CorruptFile {
                        reason: format!("node index {idx} has no table OID metadata"),
                    })?;
            let node_id =
                eng.node_store
                    .primary_key(idx)
                    .ok_or_else(|| safety::GraphError::CorruptFile {
                        reason: format!("node index {idx} has no primary-key metadata"),
                    })?;
            coordinates
                .entry(idx)
                .or_insert_with(|| types::PathCoordinate {
                    table_oid: types::TableOid(table_oid),
                    node_id: node_id.to_string(),
                });
        }
        Ok(coordinates)
    })
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn execute_aggregation_traversal(
    request: &AggregationTraversalRequest,
    limit: usize,
) -> safety::GraphResult<Vec<TraverseRow>> {
    let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    let (tables, _edges, filter_columns) = crate::catalog::read_catalog()?;
    let visibility = crate::visibility::VisibilityScope::Unrestricted;
    let context = crate::visibility::QueryExecutionContext::new(&governor, &visibility);
    execute_aggregation_traversal_governed(request, limit, &context, &tables, &filter_columns)
}

fn execute_aggregation_traversal_governed(
    request: &AggregationTraversalRequest,
    limit: usize,
    context: &crate::visibility::QueryExecutionContext<'_>,
    tables: &[crate::builder::RegisteredTable],
    filter_columns: &[crate::builder::RegisteredFilterColumn],
) -> safety::GraphResult<Vec<TraverseRow>> {
    let node_tables = request
        .node_tables
        .as_ref()
        .map(|oids| {
            oids.iter()
                .copied()
                .map(pgrx::pg_sys::Oid::from_u32)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let node_tables = (!node_tables.is_empty()).then_some(node_tables);
    let mut rows = Vec::new();
    for start in &request.starts {
        let traverse_request = TraverseRequest {
            root_table: pgrx::pg_sys::Oid::from_u32(start.table_oid.0),
            root_id: &start.node_id,
            max_depth: request.max_depth,
            edge_types: request.edge_types.as_deref(),
            node_tables: node_tables.as_deref(),
            filter: None,
            tenant: None,
            direction: request.direction,
            strategy: types::TraversalStrategy::Bfs,
            include_start: true,
            hydrate: true,
            limit: limit.min(i32::MAX as usize) as i32,
            offset: 0,
            max_nodes: crate::config::MAX_NODES.get(),
            max_frontier: crate::config::MAX_FRONTIER.get(),
        };
        let mut start_rows =
            execute_traverse_rows_in_context(&traverse_request, context, tables, filter_columns)?;
        rows.append(&mut start_rows);
    }
    Ok(rows)
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn expand_rows_to_parent_path(
    rows: Vec<TraverseRow>,
) -> safety::GraphResult<Vec<TraverseRow>> {
    let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
    let (tables, _edges, _filter_columns) = crate::catalog::read_catalog()?;
    expand_rows_to_parent_path_governed(rows, &governor, &tables)
}

fn expand_rows_to_parent_path_governed(
    rows: Vec<TraverseRow>,
    governor: &crate::resource::ResourceGovernor,
    tables: &[crate::builder::RegisteredTable],
) -> safety::GraphResult<Vec<TraverseRow>> {
    let output_count = rows.iter().try_fold(0usize, |count, row| {
        let width = row.5 .0.as_array().map_or(0, Vec::len);
        count.checked_add(width).ok_or_else(|| {
            safety::GraphError::Internal("expanded parent-path row count overflowed".to_string())
        })
    })?;
    let output_bytes = output_count
        .checked_mul(2_048)
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("expanded parent-path workspace overflowed".to_string())
        })?;
    let output_lease = governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryCandidates,
            output_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryCandidates,
            crate::resource::WorkUnits::new(u64::try_from(output_count).unwrap_or(u64::MAX)),
        )
        .map_err(crate::safety::resource_limit_error)?;
    let by_coord = rows
        .iter()
        .filter_map(|row| {
            row.7
                .as_ref()
                .map(|node| ((row.2.to_u32(), row.3.as_str()), node))
        })
        .collect::<HashMap<_, _>>();
    let mut expanded = Vec::new();
    for row in &rows {
        let serde_json::Value::Array(path) = &row.5 .0 else {
            continue;
        };
        for coord in path {
            let table = path_node_field(coord, "table")?;
            let id = path_node_field(coord, "id")?;
            let table_oid = table_oid_from_name(table)?;
            let node = if let Some(node) = by_coord.get(&(table_oid, id)) {
                Some(pgrx::JsonB(node.0.clone()))
            } else {
                hydrate_node_governed_with_tables(table_oid, id, governor, tables)?
            };
            expanded.push((
                row.0,
                row.1.clone(),
                pgrx::pg_sys::Oid::from_u32(table_oid),
                id.to_string(),
                row.4,
                pgrx::JsonB(row.5 .0.clone()),
                pgrx::JsonB(row.6 .0.clone()),
                node,
                row.8.clone(),
                crate::catalog::relation_name(table_oid)?,
                row.10,
            ));
        }
    }
    output_lease.retain_until_governor_drop();
    Ok(expanded)
}

pub(crate) fn parse_aggregation_traversal_request(
    value: &serde_json::Value,
) -> safety::GraphResult<AggregationTraversalRequest> {
    let serde_json::Value::Object(map) = value else {
        return Err(safety::GraphError::InvalidFilter {
            reason: "traversal must be a JSON object".to_string(),
        });
    };
    let allowed = [
        "starts",
        "direction",
        "min_depth",
        "max_depth",
        "edge_types",
        "node_tables",
    ];
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!("unsupported traversal key '{}'", key),
        });
    }
    let starts = map
        .get("starts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: "traversal.starts must be an array of graph.node_ref_string() values"
                .to_string(),
        })?
        .iter()
        .map(parse_node_ref_json_string)
        .collect::<safety::GraphResult<Vec<_>>>()?;
    if starts.is_empty() {
        return Err(safety::GraphError::InvalidFilter {
            reason: "traversal.starts must not be empty".to_string(),
        });
    }
    let direction = map
        .get("direction")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("out");
    let direction = match direction {
        "in" => types::TraversalDirection::In,
        "out" => types::TraversalDirection::Out,
        "both" => types::TraversalDirection::Any,
        other => {
            return Err(safety::GraphError::InvalidFilter {
                reason: format!(
                    "traversal.direction must be exactly 'in', 'out', or 'both', got '{}'",
                    other
                ),
            });
        }
    };
    let min_depth = json_i32_field(map, "min_depth", 0)?;
    let max_depth = json_i32_field(map, "max_depth", crate::config::DEFAULT_MAX_DEPTH.get())?;
    if min_depth < 0 || max_depth < 0 || min_depth > max_depth {
        return Err(safety::GraphError::InvalidFilter {
            reason: "traversal min_depth/max_depth must be non-negative and min_depth <= max_depth"
                .to_string(),
        });
    }
    let edge_types = optional_string_array(map, "edge_types")?.filter(|items| !items.is_empty());
    let node_tables = optional_string_array(map, "node_tables")?
        .filter(|items| !items.is_empty())
        .map(|tables| {
            tables
                .into_iter()
                .map(|table| table_oid_from_name(&table))
                .collect::<safety::GraphResult<Vec<_>>>()
        })
        .transpose()?;
    Ok(AggregationTraversalRequest {
        starts,
        direction,
        min_depth,
        max_depth,
        edge_types,
        node_tables,
    })
}

pub(crate) fn parse_aggregation_specs(
    value: &serde_json::Value,
) -> safety::GraphResult<Vec<AggregateSpec>> {
    let serde_json::Value::Object(map) = value else {
        return Err(safety::GraphError::InvalidFilter {
            reason: "aggregations must be a JSON object".to_string(),
        });
    };
    let allowed = ["sum", "avg", "count"];
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!("unsupported aggregate key '{}'", key),
        });
    }
    let mut specs = Vec::new();
    for kind in [AggregateKind::Sum, AggregateKind::Avg, AggregateKind::Count] {
        let Some(value) = map.get(kind.key()) else {
            continue;
        };
        let serde_json::Value::Array(items) = value else {
            return Err(safety::GraphError::InvalidFilter {
                reason: format!("aggregations.{} must be an array", kind.key()),
            });
        };
        for item in items {
            specs.push(parse_aggregate_spec(kind, item)?);
        }
    }
    if specs.is_empty() {
        return Err(safety::GraphError::InvalidFilter {
            reason: "aggregations must request at least one aggregate".to_string(),
        });
    }
    Ok(specs)
}

pub(crate) fn parse_aggregate_scope(scope: &str) -> safety::GraphResult<AggregateScope> {
    match scope {
        "returned_nodes" => Ok(AggregateScope::ReturnedNodes),
        "chosen_parent_path" => Ok(AggregateScope::ChosenParentPath),
        "all_possible_paths" => Ok(AggregateScope::AllPossiblePaths),
        other => Err(safety::GraphError::InvalidFilter {
            reason: format!(
                "unsupported aggregate scope '{}'; expected returned_nodes, chosen_parent_path, or all_possible_paths",
                other
            ),
        }),
    }
}

pub(crate) fn parse_aggregate_spec(
    kind: AggregateKind,
    value: &serde_json::Value,
) -> safety::GraphResult<AggregateSpec> {
    let serde_json::Value::Object(map) = value else {
        return Err(safety::GraphError::InvalidFilter {
            reason: "aggregate request entries must be JSON objects".to_string(),
        });
    };
    let allowed = ["table", "column", "as"];
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!("unsupported aggregate request key '{}'", key),
        });
    }
    let table_name = required_string_field(map, "table")?;
    let column = required_string_field(map, "column")?;
    let alias = required_string_field(map, "as")?;
    let table_oid = table_oid_from_name(&table_name)?;
    acl::check_table_acl(table_oid)?;
    validate_column_exists(table_oid, &column)?;
    Ok(AggregateSpec {
        kind,
        table_oid,
        column,
        alias,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge_store::{IdentifiedRawEdge, RawEdge, SortedEdgeStoreBuilder};
    use crate::projection::segment::{DeltaSegment, SegmentEdge, SegmentKind};
    use crate::resource::{
        ByteCount, DiskBudget, ElapsedBudget, MemoryBudget, ResourceGovernor, ResourceLimits,
        RowCount, WorkUnits,
    };
    use std::time::Duration;

    fn aggregation_governor() -> ResourceGovernor {
        ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(1_024 * 1_024)),
            DiskBudget::UNLIMITED,
            RowCount::UNLIMITED,
            WorkUnits::new(10_000),
            ElapsedBudget::new(Duration::from_secs(1)),
        ))
    }

    #[test]
    fn parse_aggregate_scope_accepts_supported_values() {
        assert_eq!(
            parse_aggregate_scope("returned_nodes").expect("returned_nodes should parse"),
            AggregateScope::ReturnedNodes
        );
        assert_eq!(
            parse_aggregate_scope("chosen_parent_path").expect("chosen_parent_path should parse"),
            AggregateScope::ChosenParentPath
        );
        assert_eq!(
            parse_aggregate_scope("all_possible_paths").expect("all_possible_paths should parse"),
            AggregateScope::AllPossiblePaths
        );
    }

    #[test]
    fn parse_aggregate_scope_rejects_unknown_values() {
        let err = parse_aggregate_scope("parent_path").expect_err("scope should be rejected");

        assert!(matches!(err, safety::GraphError::InvalidFilter { .. }));
        assert!(err
            .to_string()
            .contains("expected returned_nodes, chosen_parent_path, or all_possible_paths"));
    }

    #[test]
    fn record_indexed_path_keeps_one_snapshot_per_unique_path() {
        let mut paths = Vec::new();
        let mut seen_paths = HashSet::new();
        let first = [1, 2, 3];
        let duplicate = [1, 2, 3];
        let second = [1, 4, 3];

        record_indexed_path(&first, &mut paths, &mut seen_paths);
        record_indexed_path(&duplicate, &mut paths, &mut seen_paths);
        record_indexed_path(&second, &mut paths, &mut seen_paths);

        assert_eq!(paths.len(), 2);
        assert_eq!(&*paths[0], &[1, 2, 3]);
        assert_eq!(&*paths[1], &[1, 4, 3]);
        assert_eq!(seen_paths.len(), 2);
    }

    #[test]
    fn hydrated_nodes_are_grouped_by_table_for_borrowed_lookup() {
        let mut hydrated = HashMap::new();
        hydrated.insert(
            (10, "a".to_string()),
            pgrx::JsonB(serde_json::json!({ "id": "a" })),
        );
        hydrated.insert(
            (10, "b".to_string()),
            pgrx::JsonB(serde_json::json!({ "id": "b" })),
        );
        hydrated.insert(
            (20, "a".to_string()),
            pgrx::JsonB(serde_json::json!({ "id": "a", "table": 20 })),
        );

        let grouped = group_hydrated_nodes_by_table(hydrated);

        assert_eq!(
            grouped
                .get(&10)
                .and_then(|nodes| nodes.get("a"))
                .map(|node| &node.0),
            Some(&serde_json::json!({ "id": "a" }))
        );
        assert_eq!(
            grouped
                .get(&20)
                .and_then(|nodes| nodes.get("a"))
                .map(|node| &node.0),
            Some(&serde_json::json!({ "id": "a", "table": 20 }))
        );
    }

    #[test]
    fn exact_path_enumeration_rejects_hidden_relationships_before_recursing() {
        let mut engine = Engine::new();
        engine.node_store.add_node(100, "A".to_string());
        engine.node_store.add_node(100, "B".to_string());
        let mut builder = SortedEdgeStoreBuilder::new(2, false);
        builder
            .try_push_identified(IdentifiedRawEdge {
                edge: RawEdge {
                    source: 0,
                    target: 1,
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
                relationship_id: 9,
            })
            .unwrap();
        engine.edge_store = builder.finish();
        engine.reverse_edge_store = engine.edge_store.reversed();
        engine.built = true;
        let request = AggregationTraversalRequest {
            starts: Vec::new(),
            direction: types::TraversalDirection::Out,
            min_depth: 0,
            max_depth: 1,
            edge_types: None,
            node_tables: None,
        };
        let mut hidden_relationships = roaring::RoaringBitmap::new();
        hidden_relationships.insert(9);
        let mut relationship_rls_edge_types = roaring::RoaringBitmap::new();
        relationship_rls_edge_types.insert(1);
        let visibility = crate::visibility::VisibilityScope::enforced(
            roaring::RoaringBitmap::new(),
            hidden_relationships,
            relationship_rls_edge_types,
        );
        let governor = aggregation_governor();
        let context = crate::visibility::QueryExecutionContext::new(&governor, &visibility);
        let overlays = aggregation_edge_overlay(&engine);
        let mut path = vec![0];
        let mut paths = Vec::new();
        let mut seen = HashSet::new();

        enumerate_all_paths_dfs(
            &engine, &request, 0, 0, &mut path, &mut paths, &mut seen, 10, None, None, &overlays,
            None, &context,
        )
        .unwrap();

        assert_eq!(paths.len(), 1);
        assert_eq!(&*paths[0], &[0]);

        let mut absent_engine = Engine::new();
        absent_engine.node_store.add_node(100, "A".to_string());
        absent_engine.node_store.add_node(100, "B".to_string());
        absent_engine.edge_store = SortedEdgeStoreBuilder::new(2, false).finish();
        absent_engine.reverse_edge_store = absent_engine.edge_store.reversed();
        absent_engine.built = true;
        let absent_overlays = aggregation_edge_overlay(&absent_engine);
        let absent_visibility = crate::visibility::VisibilityScope::Unrestricted;
        let absent_context =
            crate::visibility::QueryExecutionContext::new(&governor, &absent_visibility);
        let mut absent_path = vec![0];
        let mut absent_paths = Vec::new();
        let mut absent_seen = HashSet::new();
        enumerate_all_paths_dfs(
            &absent_engine,
            &request,
            0,
            0,
            &mut absent_path,
            &mut absent_paths,
            &mut absent_seen,
            10,
            None,
            None,
            &absent_overlays,
            None,
            &absent_context,
        )
        .unwrap();

        assert_eq!(
            paths
                .iter()
                .map(|path| path.as_ref())
                .collect::<Vec<&[u32]>>(),
            absent_paths
                .iter()
                .map(|path| path.as_ref())
                .collect::<Vec<&[u32]>>()
        );
    }

    #[test]
    fn exact_path_enumeration_uses_committed_layered_relationship_identities() {
        let mut engine = Engine::new();
        for id in ["A", "deleted-base", "inserted-segment"] {
            engine.node_store.add_node(100, id.to_string());
        }
        let mut builder = SortedEdgeStoreBuilder::new(3, false);
        builder
            .try_push_identified(IdentifiedRawEdge {
                edge: RawEdge {
                    source: 0,
                    target: 1,
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
                relationship_id: 4,
            })
            .unwrap();
        engine.edge_store = builder.finish();
        engine.reverse_edge_store = engine.edge_store.reversed();
        engine.built = true;
        let mut segment = DeltaSegment::new(
            SegmentKind::Edge,
            0,
            types::TraversalDirection::Out,
            0,
            3,
            1,
        )
        .unwrap();
        segment.edge_deletes.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: Some(4),
        });
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: Some(9),
        });
        let layered = LayeredNeighbors::new(&engine.edge_store, vec![segment]);
        let request = AggregationTraversalRequest {
            starts: Vec::new(),
            direction: types::TraversalDirection::Out,
            min_depth: 0,
            max_depth: 1,
            edge_types: None,
            node_tables: None,
        };
        let mut hidden_relationships = roaring::RoaringBitmap::new();
        hidden_relationships.insert(9);
        let mut relationship_rls_edge_types = roaring::RoaringBitmap::new();
        relationship_rls_edge_types.insert(1);
        let visibility = crate::visibility::VisibilityScope::enforced(
            roaring::RoaringBitmap::new(),
            hidden_relationships,
            relationship_rls_edge_types,
        );
        let governor = aggregation_governor();
        let context = crate::visibility::QueryExecutionContext::new(&governor, &visibility);
        let overlays = aggregation_edge_overlay(&engine);
        let visible_neighbors = aggregation_neighbors(
            &engine,
            0,
            types::TraversalDirection::Out,
            &overlays,
            Some(&layered),
        );
        assert_eq!(visible_neighbors.len(), 1);
        assert_eq!(visible_neighbors[0].target, 2);
        assert_eq!(visible_neighbors[0].relationship_id, Some(9));
        let mut path = vec![0];
        let mut paths = Vec::new();
        let mut seen = HashSet::new();

        enumerate_all_paths_dfs(
            &engine,
            &request,
            0,
            0,
            &mut path,
            &mut paths,
            &mut seen,
            10,
            None,
            None,
            &overlays,
            Some(&layered),
            &context,
        )
        .unwrap();

        assert_eq!(paths.len(), 1);
        assert_eq!(&*paths[0], &[0]);
    }

    #[test]
    fn exact_path_enumeration_rejects_transaction_deleted_targets() {
        crate::projection::tx_delta::clear_for_test();
        let mut engine = Engine::new();
        engine.node_store.add_node(100, "A".to_string());
        engine.node_store.add_node(100, "deleted".to_string());
        engine.edge_store = crate::edge_store::EdgeStore::from_edges(
            2,
            vec![RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            }],
            false,
        );
        engine.reverse_edge_store = engine.edge_store.reversed();
        engine.built = true;
        crate::projection::tx_delta::record_deleted_node(1).unwrap();
        let request = AggregationTraversalRequest {
            starts: Vec::new(),
            direction: types::TraversalDirection::Out,
            min_depth: 0,
            max_depth: 1,
            edge_types: None,
            node_tables: None,
        };
        let governor = aggregation_governor();
        let visibility = crate::visibility::VisibilityScope::Unrestricted;
        let context = crate::visibility::QueryExecutionContext::new(&governor, &visibility);
        let overlays = aggregation_edge_overlay(&engine);
        let mut path = vec![0];
        let mut paths = Vec::new();
        let mut seen = HashSet::new();

        enumerate_all_paths_dfs(
            &engine, &request, 0, 0, &mut path, &mut paths, &mut seen, 10, None, None, &overlays,
            None, &context,
        )
        .unwrap();
        crate::projection::tx_delta::clear_for_test();

        assert_eq!(paths.len(), 1);
        assert_eq!(&*paths[0], &[0]);
    }
}
