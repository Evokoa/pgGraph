//! SQL hydration helpers for source rows returned by graph operations.

use crate::catalog::{primary_key_expr, read_catalog, sql_table_name_from_oid};
use crate::{acl, safety, types};
use pgrx::prelude::*;
use std::collections::{HashMap, HashSet};

// PostgreSQL's binary JSONB stores at least one four-byte entry per scalar or
// container element. An owned `serde_json::Value` uses no more than 128 bytes
// of structural/key allocation per such entry on supported 64-bit targets.
// Charging 32 bytes per binary byte therefore covers the owned tree, while the
// encoded-text term covers string payload and escape expansion. Per-row slack
// covers the root value, allocator rounding, and pgrx's wrapper.
const JSONB_OWNED_BYTES_PER_BINARY_BYTE: u64 = 32;
const JSONB_OWNED_BYTES_PER_ROW: u64 = 1_024;

pub(crate) fn reserve_jsonb_materialization(
    workspace: &mut crate::resource::ResourceLease<'_>,
    row_count: u64,
    binary_bytes: u64,
    text_bytes: u64,
) -> safety::GraphResult<()> {
    let structural = binary_bytes
        .checked_mul(JSONB_OWNED_BYTES_PER_BINARY_BYTE)
        .ok_or_else(|| safety::GraphError::Internal("JSONB workspace overflowed".to_string()))?;
    let row_overhead = row_count
        .checked_mul(JSONB_OWNED_BYTES_PER_ROW)
        .ok_or_else(|| {
            safety::GraphError::Internal("JSONB row workspace overflowed".to_string())
        })?;
    let bytes = structural
        .checked_add(text_bytes)
        .and_then(|bytes| bytes.checked_add(row_overhead))
        .ok_or_else(|| safety::GraphError::Internal("JSONB workspace overflowed".to_string()))?;
    workspace
        .try_grow(crate::resource::ByteCount::from_bytes(bytes))
        .map_err(crate::safety::resource_limit_error)
}

pub(crate) fn hydrate_node(
    table_oid: u32,
    node_id: &str,
) -> safety::GraphResult<Option<pgrx::JsonB>> {
    let governor = hydration_governor()?;
    hydrate_node_governed(table_oid, node_id, &governor)
}

pub(crate) fn hydrate_node_governed(
    table_oid: u32,
    node_id: &str,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Option<pgrx::JsonB>> {
    let mut workspace = reserve_hydration_workspace(governor, 1, node_id.len())?;
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find_map(|table| {
            Ok::<u32, crate::safety::GraphError>(table.table_oid)
                .ok()
                .filter(|oid| *oid == table_oid)
                .map(|_| table)
        })
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot hydrate node from unregistered table OID {}",
                table_oid
            ))
        })?;

    acl::check_table_acl(table_oid)?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
    let pk_expr = primary_key_expr("src", &table.id_columns);
    let json_sizes = Spi::connect(|client| {
        let query = format!(
            "SELECT pg_catalog.pg_column_size(pg_catalog.to_jsonb(src.*))::bigint,
                    pg_catalog.octet_length(pg_catalog.to_jsonb(src.*)::text)::bigint
               FROM {} src WHERE {} = $1 LIMIT 1",
            table_name.as_sql(),
            pk_expr
        );
        let result = client
            .select(&query, None, &[node_id.into()])
            .map_err(|error| {
                safety::GraphError::Internal(format!("hydration size preflight failed: {error}"))
            })?;
        if result.is_empty() {
            return Ok(None);
        }
        let row = result.first();
        let binary = row.get::<i64>(1).map_err(|error| {
            safety::GraphError::Internal(format!("hydration JSONB size read failed: {error}"))
        })?;
        let text = row.get::<i64>(2).map_err(|error| {
            safety::GraphError::Internal(format!("hydration JSON text size read failed: {error}"))
        })?;
        Ok::<_, safety::GraphError>(binary.zip(text))
    })?;
    let Some((binary_bytes, text_bytes)) = json_sizes else {
        return Ok(None);
    };
    reserve_jsonb_materialization(
        &mut workspace,
        1,
        u64::try_from(binary_bytes.max(0)).unwrap_or(0),
        u64::try_from(text_bytes.max(0)).unwrap_or(0),
    )?;
    let hydrated = Spi::connect(|client| {
        let query = format!(
            "SELECT to_jsonb(src.*) FROM {} src WHERE {} = $1 LIMIT 1",
            table_name.as_sql(),
            pk_expr
        );
        let result = client
            .select(&query, None, &[node_id.into()])
            .map_err(|e| {
                safety::GraphError::Internal(format!(
                    "hydration failed for {}: {}",
                    table_name.as_sql(),
                    e
                ))
            })?;
        if result.is_empty() {
            return Ok(None);
        }
        let row = result.first();
        row.get::<pgrx::JsonB>(1)
            .map_err(|e| safety::GraphError::Internal(format!("hydration read failed: {}", e)))
    })?;
    workspace.retain_until_governor_drop();
    Ok(hydrated)
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn hydrate_nodes(
    rows: &[types::TraversalResult],
) -> safety::GraphResult<HashMap<(u32, String), pgrx::JsonB>> {
    let governor = hydration_governor()?;
    hydrate_nodes_governed(rows, &governor)
}

/// Hydrates traversal rows inside a caller-owned operation budget.
///
/// The retained reservation represents the returned map until the caller
/// finishes SQL output materialization and drops the governor.
pub(crate) fn hydrate_nodes_governed(
    rows: &[types::TraversalResult],
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<HashMap<(u32, String), pgrx::JsonB>> {
    let key_bytes = rows.iter().try_fold(0usize, |bytes, row| {
        bytes.checked_add(row.node_id.len()).ok_or_else(|| {
            safety::GraphError::Internal("hydration key size overflowed".to_string())
        })
    })?;
    let mut workspace = reserve_hydration_workspace(governor, rows.len(), key_bytes)?;
    let mut ids_by_table: HashMap<u32, Vec<String>> = HashMap::new();
    for row in rows {
        ids_by_table
            .entry(row.node_table.0)
            .or_default()
            .push(row.node_id.clone());
    }
    if ids_by_table.is_empty() {
        return Ok(HashMap::new());
    }

    let needed_table_oids = ids_by_table.keys().copied().collect::<HashSet<_>>();
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let mut tables_by_oid = HashMap::with_capacity(needed_table_oids.len());
    for table in &tables {
        let oid = table.table_oid;
        if needed_table_oids.contains(&oid) {
            tables_by_oid.insert(oid, table);
            if tables_by_oid.len() == needed_table_oids.len() {
                break;
            }
        }
    }

    let mut hydrated = HashMap::new();
    for (table_oid, mut node_ids) in ids_by_table {
        node_ids.sort();
        node_ids.dedup();
        let table = tables_by_oid.get(&table_oid).ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot hydrate nodes from unregistered table OID {}",
                table_oid
            ))
        })?;
        acl::check_table_acl(table_oid)?;
        let table_name = sql_table_name_from_oid(table.table_oid)?;
        let pk_expr = primary_key_expr("src", &table.id_columns);
        let (visible_rows, binary_bytes, text_bytes) = Spi::connect(|client| {
            let query = format!(
                "SELECT pg_catalog.count(*)::bigint,
                        COALESCE(pg_catalog.sum(pg_catalog.pg_column_size(pg_catalog.to_jsonb(src.*))), 0)::bigint,
                        COALESCE(pg_catalog.sum(pg_catalog.octet_length(pg_catalog.to_jsonb(src.*)::text)), 0)::bigint
                   FROM {} src WHERE {} = ANY($1::text[])",
                table_name.as_sql(),
                pk_expr
            );
            let result = client
                .select(&query, None, &[node_ids.clone().into()])
                .map_err(|error| {
                    safety::GraphError::Internal(format!(
                        "batch hydration size preflight failed: {error}"
                    ))
                })?;
            let row = result.first();
            Ok::<_, safety::GraphError>((
                row.get::<i64>(1)
                    .map_err(|error| {
                        safety::GraphError::Internal(format!(
                            "batch hydration row count read failed: {error}"
                        ))
                    })?
                    .unwrap_or(0),
                row.get::<i64>(2)
                    .map_err(|error| {
                        safety::GraphError::Internal(format!(
                            "batch hydration JSONB size read failed: {error}"
                        ))
                    })?
                    .unwrap_or(0),
                row.get::<i64>(3)
                    .map_err(|error| {
                        safety::GraphError::Internal(format!(
                            "batch hydration JSON text size read failed: {error}"
                        ))
                    })?
                    .unwrap_or(0),
            ))
        })?;
        reserve_jsonb_materialization(
            &mut workspace,
            u64::try_from(visible_rows.max(0)).unwrap_or(0),
            u64::try_from(binary_bytes.max(0)).unwrap_or(0),
            u64::try_from(text_bytes.max(0)).unwrap_or(0),
        )?;
        crate::resource::check_postgres_interrupts();
        let query = format!(
            "SELECT {} AS graph_node_id, to_jsonb(src.*) FROM {} src WHERE {} = ANY($1::text[])",
            pk_expr,
            table_name.as_sql(),
            pk_expr
        );
        Spi::connect(|client| {
            let result = client
                .select(&query, None, &[node_ids.clone().into()])
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "batch hydration failed for {}: {}",
                        table_name.as_sql(),
                        e
                    ))
                })?;
            for row in result {
                let node_id = row
                    .get::<String>(1)
                    .map_err(|e| {
                        safety::GraphError::Internal(format!("hydration PK read failed: {}", e))
                    })?
                    .ok_or_else(|| {
                        safety::GraphError::Internal("hydration returned NULL PK".to_string())
                    })?;
                if let Some(node) = row.get::<pgrx::JsonB>(2).map_err(|e| {
                    safety::GraphError::Internal(format!("hydration row read failed: {}", e))
                })? {
                    hydrated.insert((table_oid, node_id), node);
                }
            }
            Ok::<(), safety::GraphError>(())
        })?;
    }

    workspace.retain_until_governor_drop();
    Ok(hydrated)
}

pub(crate) fn visible_node_keys_governed(
    ids_by_table: &HashMap<u32, Vec<String>>,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<HashSet<(u32, String)>> {
    let (row_count, key_bytes) =
        ids_by_table
            .values()
            .try_fold((0usize, 0usize), |(rows, bytes), ids| {
                let rows = rows.checked_add(ids.len()).ok_or_else(|| {
                    safety::GraphError::Internal("visibility row count overflowed".to_string())
                })?;
                let bytes = ids.iter().try_fold(bytes, |bytes, id| {
                    bytes.checked_add(id.len()).ok_or_else(|| {
                        safety::GraphError::Internal("visibility key size overflowed".to_string())
                    })
                })?;
                Ok::<_, safety::GraphError>((rows, bytes))
            })?;
    let workspace = reserve_hydration_workspace(governor, row_count, key_bytes)?;
    let needed_table_oids = ids_by_table.keys().copied().collect::<HashSet<_>>();
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let tables_by_oid = tables
        .iter()
        .filter(|table| needed_table_oids.contains(&table.table_oid))
        .map(|table| (table.table_oid, table))
        .collect::<HashMap<_, _>>();
    let mut visible = HashSet::new();
    for (table_oid, node_ids) in ids_by_table {
        crate::resource::check_postgres_interrupts();
        let table = tables_by_oid.get(table_oid).ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot check source visibility for unregistered table OID {table_oid}"
            ))
        })?;
        acl::check_table_acl(*table_oid)?;
        let table_name = sql_table_name_from_oid(table.table_oid)?;
        let pk_expr = primary_key_expr("src", &table.id_columns);
        let query = format!(
            "SELECT {pk_expr} AS graph_node_id FROM {} src WHERE {pk_expr} = ANY($1::text[])",
            table_name.as_sql()
        );
        Spi::connect(|client| {
            let result = client
                .select(&query, None, &[node_ids.clone().into()])
                .map_err(|e| {
                    safety::GraphError::Internal(format!("source visibility check failed: {e}"))
                })?;
            for row in result {
                let node_id = row
                    .get::<String>(1)
                    .map_err(|e| {
                        safety::GraphError::Internal(format!(
                            "source visibility key read failed: {e}"
                        ))
                    })?
                    .ok_or_else(|| {
                        safety::GraphError::Internal(
                            "source visibility returned NULL key".to_string(),
                        )
                    })?;
                visible.insert((*table_oid, node_id));
            }
            Ok::<(), safety::GraphError>(())
        })?;
    }
    workspace.retain_until_governor_drop();
    Ok(visible)
}

fn hydration_governor() -> safety::GraphResult<crate::resource::ResourceGovernor> {
    let resident = crate::ENGINE
        .with(|engine| {
            crate::resource::ByteCount::from_usize(engine.borrow().estimated_memory_used_bytes())
        })
        .ok_or_else(|| {
            safety::GraphError::Internal("engine residency does not fit u64".to_string())
        })?;
    Ok(crate::resource::query_governor(resident))
}

pub(crate) fn reserve_hydration_workspace<'a>(
    governor: &'a crate::resource::ResourceGovernor,
    rows: usize,
    key_bytes: usize,
) -> safety::GraphResult<crate::resource::ResourceLease<'a>> {
    const CONSERVATIVE_ROW_BYTES: usize = 1_024;
    let bytes = rows
        .checked_mul(CONSERVATIVE_ROW_BYTES)
        .and_then(|bytes| bytes.checked_add(key_bytes))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("hydration workspace estimate overflowed".to_string())
        })?;
    governor
        .consume_work(
            crate::resource::ResourcePhase::QueryHydrate,
            crate::resource::WorkUnits::new(u64::try_from(rows).map_err(|_| {
                safety::GraphError::Internal("hydration row count does not fit u64".to_string())
            })?),
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .reserve_memory(crate::resource::ResourcePhase::QueryHydrate, bytes)
        .map_err(crate::safety::resource_limit_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonb_materialization_bound_rejects_before_owned_value_allocation() {
        let governor = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(
                crate::resource::ByteCount::from_bytes(64 * 1024),
            )),
        );
        let mut workspace = governor
            .reserve_memory(
                crate::resource::ResourcePhase::QueryHydrate,
                crate::resource::ByteCount::ZERO,
            )
            .expect("empty reservation succeeds");

        let error = reserve_jsonb_materialization(&mut workspace, 1, 4_096, 8_192)
            .expect_err("owned JSONB upper bound exceeds the test budget");

        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));
        assert_eq!(workspace.amount(), crate::resource::ByteCount::ZERO);
    }
}
