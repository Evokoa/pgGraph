//! SQL-layer aggregation and exact path-count orchestration.

use crate::api_types::{
    AggregateAccumulator, AggregateKind, AggregateSpec, AggregationTraversalRequest,
    TraverseRequest, TraverseRow,
};
use crate::catalog::{regclass_text, table_oid_from_name, validate_column_exists};
use crate::sql_hydration::{hydrate_node, hydrate_nodes};
use crate::sql_traversal::{
    execute_traverse_rows, json_i32_field, json_number_as_f64, json_number_from_f64,
    optional_string_array, parse_node_ref_json_string, path_node_field, required_string_field,
    usize_from_nonnegative,
};
use crate::{acl, check_enabled_result, edge_store, engine, safety, types, Engine, ENGINE};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

type OverlayInserts = HashMap<u32, Vec<(u32, u8)>>;
type OverlayDeletes = HashMap<u32, HashSet<(u32, u8)>>;
type AggregationEdgeOverlay = (OverlayInserts, OverlayDeletes);
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
) -> safety::GraphResult<serde_json::Value> {
    check_enabled_result()?;
    let request = parse_aggregation_traversal_request(traversal)?;
    let specs = parse_aggregation_specs(aggregations)?;
    let scope = parse_aggregate_scope(scope)?;
    let path_limit = usize_from_nonnegative(path_limit, "path_limit")?;
    match scope {
        AggregateScope::ReturnedNodes | AggregateScope::ChosenParentPath => {}
        AggregateScope::AllPossiblePaths => {
            let (paths, _exact, capped) = indexed_paths_for_request(&request, path_limit)?;
            if capped {
                return Err(safety::GraphError::InvalidFilter {
                    reason: format!(
                        "all_possible_paths expansion exceeds graph.max_exact_path_count ({})",
                        path_limit
                    ),
                });
            }
            return aggregate_indexed_paths(&paths, specs);
        }
    }

    let rows = execute_aggregation_traversal(&request, path_limit)?;
    let rows = rows
        .into_iter()
        .filter(|row| row.4 >= request.min_depth)
        .collect::<Vec<_>>();
    let aggregate_rows = if scope.expands_parent_path() {
        expand_rows_to_parent_path(rows)?
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
) -> safety::GraphResult<(i64, bool, bool)> {
    check_enabled_result()?;
    let request = parse_aggregation_traversal_request(traversal)?;
    let path_limit = usize_from_nonnegative(path_limit, "graph.max_exact_path_count")?;
    path_count_for_request(&request, path_limit)
}

pub(crate) fn path_count_for_request(
    request: &AggregationTraversalRequest,
    path_limit: usize,
) -> safety::GraphResult<(i64, bool, bool)> {
    let (paths, exact, capped) = indexed_paths_for_request(request, path_limit)?;
    if capped || !exact {
        Ok((path_limit as i64, false, true))
    } else {
        Ok((paths.len() as i64, true, false))
    }
}

pub(crate) fn indexed_paths_for_request(
    request: &AggregationTraversalRequest,
    path_limit: usize,
) -> safety::GraphResult<(Vec<IndexedPath>, bool, bool)> {
    let edge_limit = path_limit.saturating_add(1);
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
        let edge_type_filter = aggregation_edge_type_filter(&eng, request)?;
        let (overlay_inserts, overlay_deletes) = aggregation_edge_overlay(&eng, request.direction);
        let mut paths = Vec::new();
        let mut seen_paths = HashSet::new();
        for start in &request.starts {
            let seed = eng
                .resolve(start.table_oid.0, &start.node_id)
                .ok_or_else(|| safety::GraphError::NodeNotFound {
                    table: start.table_oid.to_string(),
                    pk: start.node_id.clone(),
                })?;
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
                &overlay_inserts,
                &overlay_deletes,
            );
            if paths.len() > path_limit {
                break;
            }
        }
        Ok::<_, safety::GraphError>(paths)
    })?;

    if indexed_paths.len() > path_limit {
        Ok((
            indexed_paths.into_iter().take(path_limit).collect(),
            false,
            true,
        ))
    } else {
        Ok((indexed_paths, true, false))
    }
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
    overlay_inserts: &HashMap<u32, Vec<(u32, u8)>>,
    overlay_deletes: &OverlayDeletes,
) {
    if depth >= request.min_depth
        && node_table_filter
            .is_none_or(|tables| tables.contains(&eng.node_store.table_oid(current)))
    {
        record_indexed_path(path, paths, seen_paths);
        if paths.len() >= edge_limit {
            return;
        }
    }
    if depth >= request.max_depth {
        return;
    }

    for (neighbor, edge_type) in aggregation_neighbors(
        eng,
        current,
        request.direction,
        overlay_inserts,
        overlay_deletes,
    ) {
        if paths.len() >= edge_limit {
            return;
        }
        if edge_type_filter.is_some_and(|allowed| !allowed.contains(&edge_type)) {
            continue;
        }
        if !eng.node_store.is_active(neighbor) || path.contains(&neighbor) {
            continue;
        }
        path.push(neighbor);
        enumerate_all_paths_dfs(
            eng,
            request,
            neighbor,
            depth + 1,
            path,
            paths,
            seen_paths,
            edge_limit,
            edge_type_filter,
            node_table_filter,
            overlay_inserts,
            overlay_deletes,
        );
        path.pop();
    }
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

pub(crate) fn aggregation_edge_overlay(
    eng: &Engine,
    direction: types::TraversalDirection,
) -> AggregationEdgeOverlay {
    let mut inserts = HashSet::new();
    let mut deletes = HashSet::new();
    for mutation in &eng.edge_buffer {
        for key in oriented_edge_keys(
            mutation.source,
            mutation.target,
            mutation.type_id,
            direction,
        ) {
            match mutation.kind {
                engine::MutationKind::Insert => {
                    deletes.remove(&key);
                    inserts.insert(key);
                }
                engine::MutationKind::Delete => {
                    inserts.remove(&key);
                    deletes.insert(key);
                }
            }
        }
    }
    let mut insert_map: OverlayInserts = HashMap::new();
    for (source, target, type_id) in inserts {
        insert_map
            .entry(source)
            .or_default()
            .push((target, type_id));
    }
    let mut delete_map: OverlayDeletes = HashMap::new();
    for (source, target, type_id) in deletes {
        delete_map
            .entry(source)
            .or_default()
            .insert((target, type_id));
    }
    (insert_map, delete_map)
}

pub(crate) fn oriented_edge_keys(
    source: u32,
    target: u32,
    type_id: u8,
    direction: types::TraversalDirection,
) -> Vec<(u32, u32, u8)> {
    match direction {
        types::TraversalDirection::In => vec![(target, source, type_id)],
        types::TraversalDirection::Any => {
            vec![(source, target, type_id), (target, source, type_id)]
        }
        types::TraversalDirection::Out => vec![(source, target, type_id)],
    }
}

pub(crate) fn aggregation_neighbors(
    eng: &Engine,
    current: u32,
    direction: types::TraversalDirection,
    overlay_inserts: &HashMap<u32, Vec<(u32, u8)>>,
    overlay_deletes: &OverlayDeletes,
) -> Vec<(u32, u8)> {
    let mut neighbors = Vec::new();
    let mut seen = HashSet::new();
    if matches!(
        direction,
        types::TraversalDirection::Out | types::TraversalDirection::Any
    ) {
        push_base_neighbors(
            &eng.edge_store,
            current,
            overlay_deletes,
            &mut seen,
            &mut neighbors,
        );
    }
    if matches!(
        direction,
        types::TraversalDirection::In | types::TraversalDirection::Any
    ) {
        push_base_neighbors(
            &eng.reverse_edge_store,
            current,
            overlay_deletes,
            &mut seen,
            &mut neighbors,
        );
    }
    if let Some(inserted) = overlay_inserts.get(&current) {
        for &(target, type_id) in inserted {
            if seen.insert((target, type_id)) {
                neighbors.push((target, type_id));
            }
        }
    }
    neighbors
}

pub(crate) fn push_base_neighbors(
    edge_store: &edge_store::EdgeStore,
    current: u32,
    overlay_deletes: &OverlayDeletes,
    seen: &mut HashSet<(u32, u8)>,
    neighbors: &mut Vec<(u32, u8)>,
) {
    let (targets, type_ids) = edge_store.neighbors(current);
    let deleted = overlay_deletes.get(&current);
    for (&target, &type_id) in targets.iter().zip(type_ids.iter()) {
        if deleted.is_some_and(|deleted| deleted.contains(&(target, type_id))) {
            continue;
        }
        if seen.insert((target, type_id)) {
            neighbors.push((target, type_id));
        }
    }
}

pub(crate) fn aggregate_indexed_paths(
    paths: &[IndexedPath],
    specs: Vec<AggregateSpec>,
) -> safety::GraphResult<serde_json::Value> {
    let coordinates_by_idx = indexed_path_coordinates(paths)?;
    let hydrated = hydrate_indexed_path_nodes(&coordinates_by_idx)?;
    let mut accumulators = specs
        .iter()
        .map(|spec| (spec.alias.clone(), AggregateAccumulator::default()))
        .collect::<HashMap<_, _>>();

    for path in paths {
        for idx in path.iter() {
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

fn hydrate_indexed_path_nodes(
    coordinates_by_idx: &HashMap<u32, types::PathCoordinate>,
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
    hydrate_nodes(&unique_rows).map(group_hydrated_nodes_by_table)
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
            coordinates
                .entry(idx)
                .or_insert_with(|| types::PathCoordinate {
                    table_oid: types::TableOid(eng.node_store.table_oid(idx)),
                    node_id: eng.node_store.primary_key(idx).to_string(),
                });
        }
        Ok(coordinates)
    })
}

pub(crate) fn execute_aggregation_traversal(
    request: &AggregationTraversalRequest,
    limit: usize,
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
        let mut start_rows = execute_traverse_rows(&traverse_request)?;
        rows.append(&mut start_rows);
    }
    Ok(rows)
}

pub(crate) fn expand_rows_to_parent_path(
    rows: Vec<TraverseRow>,
) -> safety::GraphResult<Vec<TraverseRow>> {
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
            let node = by_coord
                .get(&(table_oid, id))
                .map(|node| pgrx::JsonB(node.0.clone()))
                .or_else(|| hydrate_node(table_oid, id).ok().flatten());
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
                regclass_text(table_oid)?,
            ));
        }
    }
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
}
