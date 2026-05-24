//! SQL-layer traversal request validation, execution, and path formatting.

use crate::api_types::{TraverseRequest, TraverseRow};
use crate::catalog::{regclass_text, table_oid_from_name};
use crate::sql_filters::{
    hydration_filters_match, parse_structured_filter, typed_pushdown_filter_op,
    ParsedStructuredFilter,
};
use crate::sql_hydration::hydrate_nodes;
use crate::{acl, safety, types, ENGINE};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub(crate) struct TraverseCandidate {
    pub(crate) root_table: pgrx::pg_sys::Oid,
    pub(crate) root_id: String,
    pub(crate) root_table_name: String,
    row: types::TraversalResult,
    pre_hydrated: Option<serde_json::Value>,
}

pub(crate) fn validate_traverse_options(
    direction: &str,
    _tenant: Option<&str>,
    strategy: &str,
    uniqueness: &str,
) -> safety::GraphResult<(
    types::TraversalDirection,
    types::TraversalStrategy,
    types::TraversalUniqueness,
)> {
    let Some(direction) = types::TraversalDirection::parse(direction) else {
        return Err(safety::GraphError::InvalidFilter {
            reason: "traverse direction supports 'any', 'out', or 'in'".to_string(),
        });
    };
    let Some(strategy) = types::TraversalStrategy::parse(strategy) else {
        if strategy.eq_ignore_ascii_case("weighted") {
            return Err(safety::GraphError::InvalidFilter {
                reason: "traverse strategy 'weighted' is reserved; use graph.weighted_shortest_path() for weighted paths"
                    .to_string(),
            });
        }
        return Err(safety::GraphError::InvalidFilter {
            reason: "traverse strategy supports 'bfs' or 'dfs'".to_string(),
        });
    };
    let Some(uniqueness) = types::TraversalUniqueness::parse(uniqueness) else {
        return Err(safety::GraphError::InvalidFilter {
            reason: "traverse uniqueness supports 'node_global' or 'node_per_root'".to_string(),
        });
    };
    Ok((direction, strategy, uniqueness))
}

pub(crate) fn execute_traverse_rows(
    request: &TraverseRequest<'_>,
) -> safety::GraphResult<Vec<TraverseRow>> {
    let candidates = execute_traverse_candidates(request)?;
    paginate_and_format_traverse_candidates(
        candidates,
        request.hydrate,
        request.offset,
        request.limit,
    )
}

pub(crate) fn execute_traverse_candidates(
    request: &TraverseRequest<'_>,
) -> safety::GraphResult<Vec<TraverseCandidate>> {
    acl::check_table_acl(request.root_table.to_u32())?;

    let table_filter = request
        .node_tables
        .map(|tables| {
            tables
                .iter()
                .map(|oid| {
                    acl::check_table_acl(oid.to_u32())?;
                    Ok::<_, safety::GraphError>(oid.to_u32())
                })
                .collect::<safety::GraphResult<HashSet<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let structured_filter = request
        .filter
        .map(|filter| parse_structured_filter(filter, &table_filter))
        .transpose()?
        .unwrap_or(ParsedStructuredFilter {
            pushdown_filters: Vec::new(),
            hydration_filters: Vec::new(),
        });

    let results = ENGINE.with(|e| {
        let eng = e.borrow();
        let mut filter_ops = match request.filter_condition {
            Some(condition) => eng
                .filter_index
                .parse_condition(condition)
                .map_err(|reason| safety::GraphError::InvalidFilter { reason })?,
            None => Vec::new(),
        };
        for filter in &structured_filter.pushdown_filters {
            filter_ops.push(typed_pushdown_filter_op(&eng.filter_index, filter)?);
        }

        eng.traverse_with_filter_ops(
            request.root_table.to_u32(),
            request.root_id,
            request.max_depth,
            u32_from_nonnegative(request.max_nodes, "max_nodes")?,
            u32_from_nonnegative(request.max_frontier, "max_frontier")?,
            request.edge_types.map(<[String]>::to_vec),
            filter_ops,
            request.tenant,
            request.strategy,
            request.direction,
        )
    })?;

    let mut page = results
        .into_iter()
        .filter(|r| request.include_start || r.depth != 0)
        .filter(|r| table_filter.is_empty() || table_filter.contains(&r.node_table.0))
        .collect::<Vec<_>>();
    page.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.node_table.cmp(&right.node_table))
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    let root_table_name = regclass_text(request.root_table.to_u32())?;
    let needs_hydration_verification = !structured_filter.hydration_filters.is_empty();
    let hydrated = if needs_hydration_verification {
        hydrate_nodes(&page)?
    } else {
        HashMap::new()
    };
    if needs_hydration_verification {
        page.retain(|row| {
            hydrated
                .get(&(row.node_table.0, row.node_id.clone()))
                .is_some_and(|node| {
                    hydration_filters_match(
                        row.node_table.0,
                        node,
                        &structured_filter.hydration_filters,
                    )
                })
        });
    }

    Ok(page
        .into_iter()
        .map(|row| {
            let pre_hydrated = hydrated
                .get(&(row.node_table.0, row.node_id.clone()))
                .map(|node| node.0.clone());
            TraverseCandidate {
                root_table: request.root_table,
                root_id: request.root_id.to_string(),
                root_table_name: root_table_name.clone(),
                row,
                pre_hydrated,
            }
        })
        .collect())
}

pub(crate) fn sort_traverse_candidates_for_many(rows: &mut [TraverseCandidate]) {
    rows.sort_by(|left, right| {
        left.root_table
            .to_u32()
            .cmp(&right.root_table.to_u32())
            .then_with(|| left.root_id.cmp(&right.root_id))
            .then_with(|| left.row.depth.cmp(&right.row.depth))
            .then_with(|| left.row.node_table.cmp(&right.row.node_table))
            .then_with(|| left.row.node_id.cmp(&right.row.node_id))
    });
}

pub(crate) fn apply_traversal_uniqueness(
    candidates: &mut Vec<TraverseCandidate>,
    uniqueness: types::TraversalUniqueness,
) {
    if uniqueness == types::TraversalUniqueness::NodePerRoot {
        return;
    }

    let mut seen = HashSet::new();
    candidates.retain(|candidate| {
        seen.insert((candidate.row.node_table.0, candidate.row.node_id.clone()))
    });
}

pub(crate) fn paginate_and_format_traverse_candidates(
    candidates: Vec<TraverseCandidate>,
    hydrate: bool,
    offset: i32,
    limit: i32,
) -> safety::GraphResult<Vec<TraverseRow>> {
    let offset = usize_from_nonnegative(offset, "offset")?;
    let limit = usize_from_nonnegative(limit, "limit")?;
    let mut page = candidates
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let mut hydrated = if hydrate {
        let rows_to_hydrate = page
            .iter()
            .filter(|candidate| candidate.pre_hydrated.is_none())
            .map(|candidate| candidate.row.clone())
            .collect::<Vec<_>>();
        hydrate_nodes(&rows_to_hydrate)?
    } else {
        HashMap::new()
    };

    page.drain(..)
        .map(|candidate| {
            let node = if hydrate {
                candidate.pre_hydrated.map(pgrx::JsonB).or_else(|| {
                    hydrated.remove(&(candidate.row.node_table.0, candidate.row.node_id.clone()))
                })
            } else {
                None
            };
            Ok((
                candidate.root_table,
                candidate.root_id,
                pgrx::pg_sys::Oid::from_u32(candidate.row.node_table.0),
                candidate.row.node_id.clone(),
                candidate.row.depth,
                pgrx::JsonB(serde_json::Value::Array(path_coordinates_json(
                    candidate.row.path,
                )?)),
                pgrx::JsonB(serde_json::Value::Array(
                    candidate
                        .row
                        .edge_path
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                )),
                node,
                candidate.root_table_name,
                regclass_text(candidate.row.node_table.0)?,
            ))
        })
        .collect()
}

pub(crate) fn path_coordinates_json(
    path: Vec<types::PathCoordinate>,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    path.into_iter()
        .map(|coord| {
            Ok(serde_json::json!({
                "table": regclass_text(coord.table_oid.0)?,
                "id": coord.node_id,
            }))
        })
        .collect()
}

pub(crate) fn parse_node_ref_json_string(
    value: &serde_json::Value,
) -> safety::GraphResult<types::PathCoordinate> {
    let (table, id) = parse_node_ref_json_parts(value)?;
    Ok(types::PathCoordinate {
        table_oid: types::TableOid(table_oid_from_name(&table)?),
        node_id: id,
    })
}

pub(crate) fn parse_node_ref_json_parts(
    value: &serde_json::Value,
) -> safety::GraphResult<(String, String)> {
    match value {
        serde_json::Value::String(raw) => {
            let parsed: serde_json::Value =
                serde_json::from_str(raw).map_err(|err| safety::GraphError::InvalidFilter {
                    reason: format!("invalid node_ref_string '{}': {}", raw, err),
                })?;
            parse_node_ref_value(&parsed)
        }
        other => parse_node_ref_value(other),
    }
}

fn parse_node_ref_value(value: &serde_json::Value) -> safety::GraphResult<(String, String)> {
    match value {
        serde_json::Value::Array(parts) => parse_node_ref_array(parts),
        serde_json::Value::Object(map) => {
            let table =
                map.get("table")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| safety::GraphError::InvalidFilter {
                        reason: "node_ref object table must be text".to_string(),
                    })?;
            let id = map
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| safety::GraphError::InvalidFilter {
                    reason: "node_ref object id must be text".to_string(),
                })?;
            Ok((table.to_string(), id.to_string()))
        }
        _ => Err(safety::GraphError::InvalidFilter {
            reason: "node_ref must be a [table, id] array, {table, id} object, or graph.node_ref_string() text".to_string(),
        }),
    }
}

fn parse_node_ref_array(parts: &[serde_json::Value]) -> safety::GraphResult<(String, String)> {
    if parts.len() != 2 {
        return Err(safety::GraphError::InvalidFilter {
            reason: "node_ref array must contain exactly [table, id]".to_string(),
        });
    }
    let table = parts[0]
        .as_str()
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: "node_ref table must be text".to_string(),
        })?;
    let id = parts[1]
        .as_str()
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: "node_ref id must be text".to_string(),
        })?;
    Ok((table.to_string(), id.to_string()))
}

pub(crate) fn json_i32_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: i32,
) -> safety::GraphResult<i32> {
    match map.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| safety::GraphError::InvalidFilter {
                reason: format!("traversal.{} must be a 32-bit integer", key),
            }),
    }
}

pub(crate) fn optional_string_array(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> safety::GraphResult<Option<Vec<String>>> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    let serde_json::Value::Array(values) = value else {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!("traversal.{} must be an array of strings", key),
        });
    };
    values
        .iter()
        .map(|value| {
            value.as_str().map(ToString::to_string).ok_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("traversal.{} entries must be strings", key),
                }
            })
        })
        .collect::<safety::GraphResult<Vec<_>>>()
        .map(Some)
}

pub(crate) fn required_string_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> safety::GraphResult<String> {
    map.get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: format!("aggregate request field '{}' is required", key),
        })
}

pub(crate) fn json_number_as_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    }
}

pub(crate) fn json_number_from_f64(value: f64) -> safety::GraphResult<serde_json::Value> {
    serde_json::Number::from_f64(value)
        .map(serde_json::Value::Number)
        .ok_or_else(|| {
            safety::GraphError::Internal("aggregate produced non-finite number".to_string())
        })
}

pub(crate) fn canonical_node_ref_string(
    table_oid: u32,
    node_id: &str,
) -> safety::GraphResult<String> {
    if node_id.is_empty() {
        return Err(safety::GraphError::InvalidFilter {
            reason: "node_id must not be empty".to_string(),
        });
    }
    let table = regclass_text(table_oid)?;
    serde_json::to_string(&vec![table, node_id.to_string()]).map_err(|err| {
        safety::GraphError::Internal(format!("node_ref_string serialization failed: {}", err))
    })
}

pub(crate) fn format_path_value(
    path: &serde_json::Value,
    edge_path: &serde_json::Value,
    separator: &str,
) -> safety::GraphResult<String> {
    let path = path
        .as_array()
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: "path must be a JSON array".to_string(),
        })?;
    let edge_path = edge_path
        .as_array()
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: "edge_path must be a JSON array".to_string(),
        })?;

    let expected_edges = path.len().saturating_sub(1);
    if edge_path.len() != expected_edges {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!(
                "edge_path length ({}) must equal path length minus one ({})",
                edge_path.len(),
                expected_edges
            ),
        });
    }

    path.windows(2)
        .zip(edge_path.iter())
        .map(|(nodes, label)| {
            let [from, to] = nodes else {
                unreachable!("slice::windows(2) always yields two nodes");
            };
            Ok(format!(
                "{}:{} --{}--> {}:{}",
                path_node_field(from, "table")?,
                path_node_field(from, "id")?,
                label
                    .as_str()
                    .ok_or_else(|| safety::GraphError::InvalidFilter {
                        reason: "edge_path entries must be strings".to_string(),
                    })?,
                path_node_field(to, "table")?,
                path_node_field(to, "id")?
            ))
        })
        .collect::<safety::GraphResult<Vec<_>>>()
        .map(|hops| hops.join(separator))
}

pub(crate) fn path_node_field<'a>(
    node: &'a serde_json::Value,
    field: &str,
) -> safety::GraphResult<&'a str> {
    node.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: format!("path entries must contain string field '{field}'"),
        })
}

pub(crate) fn usize_from_nonnegative(value: i32, name: &str) -> safety::GraphResult<usize> {
    if value < 0 {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!("{} must be non-negative", name),
        });
    }
    Ok(value as usize)
}

pub(crate) fn u32_from_nonnegative(value: i32, name: &str) -> safety::GraphResult<u32> {
    if value < 0 {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!("{} must be non-negative", name),
        });
    }
    Ok(value as u32)
}

#[cfg(test)]
mod tests {
    use super::parse_node_ref_json_parts;

    #[test]
    fn node_ref_json_part_parser_rejects_non_contract_shapes() {
        assert!(parse_node_ref_json_parts(&serde_json::json!("[\"public.users\",\"u1\"]")).is_ok());
        assert!(parse_node_ref_json_parts(&serde_json::json!(["public.users", "u1"])).is_ok());
        assert!(parse_node_ref_json_parts(
            &serde_json::json!({"table": "public.users", "id": "u1"})
        )
        .is_ok());
        assert!(parse_node_ref_json_parts(&serde_json::json!(["public.users"])).is_err());
        assert!(parse_node_ref_json_parts(&serde_json::json!([42, "u1"])).is_err());
        assert!(parse_node_ref_json_parts(&serde_json::json!({"table": "public.users"})).is_err());
    }
}
