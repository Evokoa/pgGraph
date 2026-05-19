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

type OverlayInserts = HashMap<u32, Vec<(u32, u8)>>;
type OverlayDeletes = HashSet<(u32, u32, u8)>;
type AggregationEdgeOverlay = (OverlayInserts, OverlayDeletes);

pub(crate) fn aggregate_impl(
    traversal: &serde_json::Value,
    aggregations: &serde_json::Value,
    scope: &str,
    path_limit: i32,
) -> safety::GraphResult<serde_json::Value> {
    check_enabled_result()?;
    let request = parse_aggregation_traversal_request(traversal)?;
    let specs = parse_aggregation_specs(aggregations)?;
    let path_limit = usize_from_nonnegative(path_limit, "path_limit")?;
    match scope {
        "returned_nodes" | "chosen_parent_path" => {}
        "all_possible_paths" => {
            let (paths, _exact, capped) = all_possible_paths_for_request(&request, path_limit)?;
            if capped {
                return Err(safety::GraphError::InvalidFilter {
                    reason: format!(
                        "all_possible_paths expansion exceeds graph.max_exact_path_count ({})",
                        path_limit
                    ),
                });
            }
            return aggregate_coordinate_paths(&paths, specs);
        }
        other => {
            return Err(safety::GraphError::InvalidFilter {
                reason: format!(
                    "unsupported aggregate scope '{}'; expected returned_nodes, chosen_parent_path, or all_possible_paths",
                    other
                ),
            });
        }
    }

    let rows = execute_aggregation_traversal(&request, path_limit)?;
    let rows = rows
        .into_iter()
        .filter(|row| row.4 >= request.min_depth)
        .collect::<Vec<_>>();
    let aggregate_rows = if scope == "chosen_parent_path" {
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
    let (paths, exact, capped) = all_possible_paths_for_request(request, path_limit)?;
    if capped || !exact {
        Ok((path_limit as i64, false, true))
    } else {
        Ok((paths.len() as i64, true, false))
    }
}

pub(crate) fn all_possible_paths_for_request(
    request: &AggregationTraversalRequest,
    path_limit: usize,
) -> safety::GraphResult<(Vec<Vec<types::PathCoordinate>>, bool, bool)> {
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
        let coordinate_paths = paths
            .into_iter()
            .map(|path| {
                path.into_iter()
                    .map(|idx| types::PathCoordinate {
                        table_oid: types::TableOid(eng.node_store.table_oid(idx)),
                        node_id: eng.node_store.primary_key(idx).to_string(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        Ok::<_, safety::GraphError>(coordinate_paths)
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
    paths: &mut Vec<Vec<u32>>,
    seen_paths: &mut HashSet<Vec<u32>>,
    edge_limit: usize,
    edge_type_filter: Option<&HashSet<u8>>,
    node_table_filter: Option<&HashSet<u32>>,
    overlay_inserts: &HashMap<u32, Vec<(u32, u8)>>,
    overlay_deletes: &HashSet<(u32, u32, u8)>,
) {
    if depth >= request.min_depth
        && node_table_filter
            .is_none_or(|tables| tables.contains(&eng.node_store.table_oid(current)))
    {
        if seen_paths.insert(path.clone()) {
            paths.push(path.clone());
        }
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
    let mut insert_map: HashMap<u32, Vec<(u32, u8)>> = HashMap::new();
    for (source, target, type_id) in inserts {
        insert_map
            .entry(source)
            .or_default()
            .push((target, type_id));
    }
    (insert_map, deletes)
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
    overlay_deletes: &HashSet<(u32, u32, u8)>,
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
    overlay_deletes: &HashSet<(u32, u32, u8)>,
    seen: &mut HashSet<(u32, u8)>,
    neighbors: &mut Vec<(u32, u8)>,
) {
    let (targets, type_ids) = edge_store.neighbors(current);
    for (&target, &type_id) in targets.iter().zip(type_ids.iter()) {
        if overlay_deletes.contains(&(current, target, type_id)) {
            continue;
        }
        if seen.insert((target, type_id)) {
            neighbors.push((target, type_id));
        }
    }
}

pub(crate) fn aggregate_coordinate_paths(
    paths: &[Vec<types::PathCoordinate>],
    specs: Vec<AggregateSpec>,
) -> safety::GraphResult<serde_json::Value> {
    let mut unique_rows = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        for coord in path {
            if seen.insert((coord.table_oid.0, coord.node_id.clone())) {
                unique_rows.push(types::TraversalResult {
                    node_table: coord.table_oid,
                    node_id: coord.node_id.clone(),
                    depth: 0,
                    path: Vec::new(),
                    edge_path: Vec::new(),
                });
            }
        }
    }
    let hydrated = hydrate_nodes(&unique_rows)?;
    let mut accumulators = specs
        .iter()
        .map(|spec| (spec.alias.clone(), AggregateAccumulator::default()))
        .collect::<HashMap<_, _>>();

    for path in paths {
        for coord in path {
            let Some(node) = hydrated.get(&(coord.table_oid.0, coord.node_id.clone())) else {
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
            uniqueness: types::TraversalUniqueness::NodeGlobal,
            include_start: true,
            hydrate: true,
            limit: limit.min(i32::MAX as usize) as i32,
            offset: 0,
            max_nodes: crate::config::MAX_NODES.get(),
            max_frontier: crate::config::MAX_FRONTIER.get(),
            filter_condition: None,
        };
        let mut start_rows = execute_traverse_rows(&traverse_request)?;
        rows.append(&mut start_rows);
    }
    Ok(rows)
}

pub(crate) fn expand_rows_to_parent_path(
    rows: Vec<TraverseRow>,
) -> safety::GraphResult<Vec<TraverseRow>> {
    let mut by_coord = rows
        .iter()
        .filter_map(|row| {
            row.7
                .as_ref()
                .map(|node| ((row.2.to_u32(), row.3.clone()), pgrx::JsonB(node.0.clone())))
        })
        .collect::<HashMap<_, _>>();
    let mut expanded = Vec::new();
    for row in rows {
        let serde_json::Value::Array(path) = &row.5 .0 else {
            continue;
        };
        for coord in path {
            let table = path_node_field(coord, "table")?;
            let id = path_node_field(coord, "id")?;
            let table_oid = table_oid_from_name(table)?;
            let node = by_coord
                .remove(&(table_oid, id.to_string()))
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
