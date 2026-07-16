//! SQL-layer traversal request validation, execution, and path formatting.

use crate::api_types::{TraverseRequest, TraverseRow};
use crate::catalog::{regclass_text, relation_name, table_oid_from_name};
use crate::sql_filters::{
    hydration_filters_match, parse_structured_filter, typed_pushdown_filter_op,
    ParsedStructuredFilter,
};
use crate::sql_hydration::hydrate_nodes_governed;
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
    let governor = query_governor()?;
    execute_traverse_rows_governed(request, &governor)
}

/// Executes and materializes one traversal under a single operation budget.
pub(crate) fn execute_traverse_rows_governed(
    request: &TraverseRequest<'_>,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Vec<TraverseRow>> {
    let candidates = execute_traverse_candidates_governed(request, governor)?;
    paginate_and_format_traverse_candidates_governed(
        candidates,
        request.hydrate,
        request.offset,
        request.limit,
        governor,
    )
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn execute_traverse_candidates(
    request: &TraverseRequest<'_>,
) -> safety::GraphResult<Vec<TraverseCandidate>> {
    let governor = query_governor()?;
    execute_traverse_candidates_governed(request, &governor)
}

/// Executes a traversal while preserving the caller's work and elapsed clocks.
pub(crate) fn execute_traverse_candidates_governed(
    request: &TraverseRequest<'_>,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Vec<TraverseCandidate>> {
    let request_bytes = traversal_request_workspace_upper_bound(request)?;
    let _request_workspace = governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryCandidates,
            request_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
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
        let mut filter_ops = Vec::with_capacity(structured_filter.pushdown_filters.len());
        for filter in &structured_filter.pushdown_filters {
            filter_ops.push(typed_pushdown_filter_op(&eng.filter_index, filter)?);
        }

        eng.traverse_with_filter_ops_governed(
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
            governor,
        )
    })?;

    let page_lease = reserve_traversal_rows(
        governor,
        crate::resource::ResourcePhase::QueryBlocking,
        &results,
        2,
    )?;
    let mut page = results
        .into_iter()
        .filter(|r| request.include_start || r.depth != 0)
        .filter(|r| table_filter.is_empty() || table_filter.contains(&r.node_table.0))
        .collect::<Vec<_>>();
    governor
        .check_elapsed(crate::resource::ResourcePhase::QueryBlocking)
        .map_err(crate::safety::resource_limit_error)?;
    crate::resource::check_postgres_interrupts();
    page.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.node_table.cmp(&right.node_table))
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    let root_table_name = relation_name(request.root_table.to_u32())?;
    let needs_hydration_verification = !structured_filter.hydration_filters.is_empty();
    let mut hydrated = if needs_hydration_verification {
        hydrate_nodes_governed(&page, governor)?
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

    let candidates = page
        .into_iter()
        .map(|row| {
            let pre_hydrated = hydrated
                .remove(&(row.node_table.0, row.node_id.clone()))
                .map(|node| node.0);
            TraverseCandidate {
                root_table: request.root_table,
                root_id: request.root_id.to_string(),
                root_table_name: root_table_name.clone(),
                row,
                pre_hydrated,
            }
        })
        .collect();
    page_lease.retain_until_governor_drop();
    Ok(candidates)
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

/// Sorts multi-root candidates while charging the stable-sort workspace.
pub(crate) fn sort_traverse_candidates_for_many_governed(
    rows: &mut [TraverseCandidate],
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<()> {
    let bytes = rows
        .len()
        .checked_mul(std::mem::size_of::<TraverseCandidate>())
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("traversal sort estimate overflowed".to_string())
        })?;
    let _workspace = governor
        .reserve_memory(crate::resource::ResourcePhase::QueryBlocking, bytes)
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryBlocking,
            crate::resource::WorkUnits::new(sort_work_units(rows.len())?),
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .check_elapsed(crate::resource::ResourcePhase::QueryBlocking)
        .map_err(crate::safety::resource_limit_error)?;
    crate::resource::check_postgres_interrupts();
    sort_traverse_candidates_for_many(rows);
    Ok(())
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

/// Applies cross-root uniqueness with a governed hash-set workspace.
pub(crate) fn apply_traversal_uniqueness_governed(
    candidates: &mut Vec<TraverseCandidate>,
    uniqueness: types::TraversalUniqueness,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<()> {
    if uniqueness == types::TraversalUniqueness::NodePerRoot {
        return Ok(());
    }
    let key_bytes = candidates.iter().try_fold(0usize, |bytes, candidate| {
        bytes
            .checked_add(candidate.row.node_id.len())
            .and_then(|bytes| bytes.checked_add(64))
            .ok_or_else(|| {
                safety::GraphError::Internal("uniqueness estimate overflowed".to_string())
            })
    })?;
    let _workspace = governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryBlocking,
            crate::resource::ByteCount::from_usize(key_bytes).ok_or_else(|| {
                safety::GraphError::Internal("uniqueness estimate does not fit u64".to_string())
            })?,
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryBlocking,
            crate::resource::WorkUnits::new(u64::try_from(candidates.len()).unwrap_or(u64::MAX)),
        )
        .map_err(crate::safety::resource_limit_error)?;
    apply_traversal_uniqueness(candidates, uniqueness);
    Ok(())
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn paginate_and_format_traverse_candidates(
    candidates: Vec<TraverseCandidate>,
    hydrate: bool,
    offset: i32,
    limit: i32,
) -> safety::GraphResult<Vec<TraverseRow>> {
    let governor = query_governor()?;
    paginate_and_format_traverse_candidates_governed(candidates, hydrate, offset, limit, &governor)
}

/// Pages, hydrates, and formats candidates under the execution governor.
pub(crate) fn paginate_and_format_traverse_candidates_governed(
    candidates: Vec<TraverseCandidate>,
    hydrate: bool,
    offset: i32,
    limit: i32,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Vec<TraverseRow>> {
    let offset = usize_from_nonnegative(offset, "offset")?;
    let limit = usize_from_nonnegative(limit, "limit")?;
    let selected = candidates.len().saturating_sub(offset).min(limit);
    let page_bytes = selected
        .checked_mul(std::mem::size_of::<TraverseCandidate>())
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("traversal page estimate overflowed".to_string())
        })?;
    let page_lease = governor
        .reserve_memory(crate::resource::ResourcePhase::QueryBlocking, page_bytes)
        .map_err(crate::safety::resource_limit_error)?;
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
        hydrate_nodes_governed(&rows_to_hydrate, governor)?
    } else {
        HashMap::new()
    };

    let output_bytes = traverse_output_upper_bound(&page)?;
    let output_lease = governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryCandidates,
            output_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryCandidates,
            crate::resource::WorkUnits::new(traverse_output_work(&page)?),
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .check_elapsed(crate::resource::ResourcePhase::QueryCandidates)
        .map_err(crate::safety::resource_limit_error)?;
    crate::resource::check_postgres_interrupts();
    let rows = page
        .drain(..)
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
                relation_name(candidate.row.node_table.0)?,
            ))
        })
        .collect::<safety::GraphResult<Vec<_>>>()?;
    page_lease.retain_until_governor_drop();
    output_lease.retain_until_governor_drop();
    Ok(rows)
}

fn query_governor() -> safety::GraphResult<crate::resource::ResourceGovernor> {
    ENGINE.with(|engine| engine.borrow().query_resource_governor())
}

fn traversal_request_workspace_upper_bound(
    request: &TraverseRequest<'_>,
) -> safety::GraphResult<crate::resource::ByteCount> {
    let table_bytes = request
        .node_tables
        .map_or(0, |tables| tables.len().saturating_mul(64));
    let edge_bytes = request.edge_types.map_or(0, |edges| {
        edges.iter().fold(0usize, |bytes, edge| {
            bytes.saturating_add(edge.len()).saturating_add(64)
        })
    });
    let filter_bytes = request
        .filter
        .map_or(Ok(0), |filter| json_workspace_upper_bound(&filter.0))?;
    table_bytes
        .checked_add(edge_bytes)
        .and_then(|bytes| bytes.checked_add(filter_bytes))
        .and_then(|bytes| bytes.checked_add(request.root_id.len()))
        .and_then(|bytes| bytes.checked_add(1_024))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("traversal request estimate overflowed".to_string())
        })
}

fn json_workspace_upper_bound(value: &serde_json::Value) -> safety::GraphResult<usize> {
    const ITEM_BYTES: usize = 512;
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Ok(ITEM_BYTES)
        }
        serde_json::Value::String(value) => ITEM_BYTES.checked_add(value.len()).ok_or_else(|| {
            safety::GraphError::Internal("JSON workspace estimate overflowed".to_string())
        }),
        serde_json::Value::Array(values) => values.iter().try_fold(ITEM_BYTES, |bytes, value| {
            bytes
                .checked_add(json_workspace_upper_bound(value)?)
                .ok_or_else(|| {
                    safety::GraphError::Internal("JSON workspace estimate overflowed".to_string())
                })
        }),
        serde_json::Value::Object(values) => {
            values.iter().try_fold(ITEM_BYTES, |bytes, (key, value)| {
                bytes
                    .checked_add(key.len())
                    .and_then(|bytes| bytes.checked_add(json_workspace_upper_bound(value).ok()?))
                    .ok_or_else(|| {
                        safety::GraphError::Internal(
                            "JSON workspace estimate overflowed".to_string(),
                        )
                    })
            })
        }
    }
}

fn reserve_traversal_rows<'a>(
    governor: &'a crate::resource::ResourceGovernor,
    phase: crate::resource::ResourcePhase,
    rows: &[types::TraversalResult],
    allocation_multiplier: usize,
) -> safety::GraphResult<crate::resource::ResourceLease<'a>> {
    let bytes = rows.iter().try_fold(0usize, |bytes, row| {
        let path_bytes = row.path.iter().try_fold(0usize, |bytes, coord| {
            bytes
                .checked_add(std::mem::size_of::<types::PathCoordinate>())
                .and_then(|bytes| bytes.checked_add(coord.node_id.len()))
                .ok_or_else(|| {
                    safety::GraphError::Internal("traversal row estimate overflowed".to_string())
                })
        })?;
        let edge_bytes = row.edge_path.iter().try_fold(0usize, |bytes, edge| {
            bytes
                .checked_add(std::mem::size_of::<String>())
                .and_then(|bytes| bytes.checked_add(edge.len()))
                .ok_or_else(|| {
                    safety::GraphError::Internal("traversal row estimate overflowed".to_string())
                })
        })?;
        bytes
            .checked_add(std::mem::size_of::<types::TraversalResult>())
            .and_then(|bytes| bytes.checked_add(row.node_id.len()))
            .and_then(|bytes| bytes.checked_add(path_bytes))
            .and_then(|bytes| bytes.checked_add(edge_bytes))
            .ok_or_else(|| {
                safety::GraphError::Internal("traversal row estimate overflowed".to_string())
            })
    })?;
    let bytes = bytes
        .checked_mul(allocation_multiplier)
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("traversal row estimate overflowed".to_string())
        })?;
    governor
        .reserve_memory(phase, bytes)
        .map_err(crate::safety::resource_limit_error)
}

fn traverse_output_upper_bound(
    rows: &[TraverseCandidate],
) -> safety::GraphResult<crate::resource::ByteCount> {
    let bytes = rows.iter().try_fold(0usize, |bytes, candidate| {
        let path = candidate
            .row
            .path
            .iter()
            .try_fold(0usize, |bytes, coordinate| {
                bytes
                    .checked_add(coordinate.node_id.len())
                    .and_then(|bytes| bytes.checked_add(512))
                    .ok_or_else(|| {
                        safety::GraphError::Internal(
                            "traversal output estimate overflowed".to_string(),
                        )
                    })
            })?;
        let edges = candidate
            .row
            .edge_path
            .iter()
            .try_fold(0usize, |bytes, edge| {
                bytes
                    .checked_add(edge.len())
                    .and_then(|bytes| bytes.checked_add(128))
                    .ok_or_else(|| {
                        safety::GraphError::Internal(
                            "traversal output estimate overflowed".to_string(),
                        )
                    })
            })?;
        bytes
            .checked_add(std::mem::size_of::<TraverseRow>())
            .and_then(|bytes| bytes.checked_add(candidate.root_id.len()))
            .and_then(|bytes| bytes.checked_add(candidate.root_table_name.len()))
            .and_then(|bytes| bytes.checked_add(candidate.row.node_id.len().saturating_mul(2)))
            .and_then(|bytes| bytes.checked_add(path))
            .and_then(|bytes| bytes.checked_add(edges))
            .and_then(|bytes| bytes.checked_add(512))
            .ok_or_else(|| {
                safety::GraphError::Internal("traversal output estimate overflowed".to_string())
            })
    })?;
    crate::resource::ByteCount::from_usize(bytes).ok_or_else(|| {
        safety::GraphError::Internal("traversal output estimate does not fit u64".to_string())
    })
}

fn traverse_output_work(rows: &[TraverseCandidate]) -> safety::GraphResult<u64> {
    rows.iter().try_fold(0u64, |work, row| {
        let row_work = row
            .row
            .path
            .len()
            .saturating_add(row.row.edge_path.len())
            .saturating_add(1);
        work.checked_add(u64::try_from(row_work).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                safety::GraphError::Internal("traversal output work overflowed".to_string())
            })
    })
}

fn sort_work_units(rows: usize) -> safety::GraphResult<u64> {
    if rows < 2 {
        return Ok(u64::try_from(rows).unwrap_or(u64::MAX));
    }
    let comparisons = rows
        .checked_mul(usize::BITS as usize - rows.leading_zeros() as usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("traversal sort work overflowed".to_string())
        })?;
    u64::try_from(comparisons).map_err(|_| {
        safety::GraphError::Internal("traversal sort work does not fit u64".to_string())
    })
}

pub(crate) fn path_coordinates_json(
    path: Vec<types::PathCoordinate>,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    path.into_iter()
        .map(|coord| {
            Ok(serde_json::json!({
                "table": relation_name(coord.table_oid.0)?,
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
    use super::*;
    use std::time::Duration;

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

    fn test_limits(
        memory: crate::resource::ByteCount,
        work: u64,
        elapsed: Duration,
    ) -> crate::resource::ResourceLimits {
        crate::resource::ResourceLimits::bounded(
            crate::resource::MemoryBudget::new(memory),
            crate::resource::DiskBudget::UNLIMITED,
            crate::resource::RowCount::UNLIMITED,
            crate::resource::WorkUnits::new(work),
            crate::resource::ElapsedBudget::new(elapsed),
        )
    }

    fn one_result() -> types::TraversalResult {
        types::TraversalResult {
            node_table: types::TableOid(42),
            node_id: "node-1".to_string(),
            depth: 0,
            path: vec![types::PathCoordinate {
                table_oid: types::TableOid(42),
                node_id: "node-1".to_string(),
            }],
            edge_path: Vec::new(),
        }
    }

    #[test]
    fn retained_execution_rows_cannot_reuse_the_full_output_budget() {
        let sizing = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(
                crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            )),
        );
        let rows = [one_result()];
        let sizing_lease = reserve_traversal_rows(
            &sizing,
            crate::resource::ResourcePhase::QueryCandidates,
            &rows,
            1,
        )
        .expect("sizing reservation succeeds");
        let exact = sizing.memory_used();
        drop(sizing_lease);

        let shared = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(exact)),
        );
        reserve_traversal_rows(
            &shared,
            crate::resource::ResourcePhase::QueryCandidates,
            &rows,
            1,
        )
        .expect("execution result fits")
        .retain_until_governor_drop();
        let error = reserve_traversal_rows(
            &shared,
            crate::resource::ResourcePhase::QueryBlocking,
            &rows,
            1,
        )
        .err()
        .expect("paging cannot reuse memory retained by execution");
        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));

        let separate = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(exact)),
        );
        reserve_traversal_rows(
            &separate,
            crate::resource::ResourcePhase::QueryBlocking,
            &rows,
            1,
        )
        .expect("the same phase fits only under a restarted governor");
    }

    #[test]
    fn hydration_consumes_the_execution_work_budget_instead_of_restarting_it() {
        let governor = crate::resource::ResourceGovernor::new(test_limits(
            crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            10,
            Duration::MAX,
        ));
        governor
            .consume_work(
                crate::resource::ResourcePhase::QueryExpand,
                crate::resource::WorkUnits::new(5),
            )
            .expect("execution work fits");
        let error = crate::sql_hydration::reserve_hydration_workspace(&governor, 6, 0)
            .err()
            .expect("hydration must observe execution work already consumed");
        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));
    }

    #[test]
    fn blocking_phase_observes_elapsed_time_from_operation_start() {
        let expired = crate::resource::ResourceGovernor::new(test_limits(
            crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            u64::MAX,
            Duration::from_millis(10),
        ));
        std::thread::sleep(Duration::from_millis(20));
        let error = sort_traverse_candidates_for_many_governed(&mut [], &expired)
            .expect_err("sort uses the original operation clock");
        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));

        let fresh = crate::resource::ResourceGovernor::new(test_limits(
            crate::resource::ByteCount::from_mib(1).expect("test budget fits"),
            u64::MAX,
            Duration::from_millis(10),
        ));
        sort_traverse_candidates_for_many_governed(&mut [], &fresh)
            .expect("a fresh clock has not expired");
    }
}
