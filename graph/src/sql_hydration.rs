//! SQL hydration helpers for source rows returned by graph operations.

use crate::catalog::read_catalog;
use crate::{acl, safety, types};
use pgrx::prelude::*;
use std::collections::{HashMap, HashSet};

mod source_key_lookup;
pub(crate) use source_key_lookup::SourceKeyLookup;

/// Reuses current source metadata only within one hydration operation.
pub(crate) struct NodeHydrator<'a> {
    governor: &'a crate::resource::ResourceGovernor,
    tables: &'a [crate::builder::RegisteredTable],
    lookups: HashMap<u32, SourceKeyLookup>,
}

impl<'a> NodeHydrator<'a> {
    pub(crate) fn new(
        governor: &'a crate::resource::ResourceGovernor,
        tables: &'a [crate::builder::RegisteredTable],
    ) -> Self {
        Self {
            governor,
            tables,
            lookups: HashMap::new(),
        }
    }

    pub(crate) fn hydrate(
        &mut self,
        table_oid: u32,
        node_id: &str,
    ) -> safety::GraphResult<Option<pgrx::JsonB>> {
        crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(|| {
            acl::check_table_acl(table_oid)
        }))?;
        if !self.lookups.contains_key(&table_oid) {
            let table = self
                .tables
                .iter()
                .find(|table| table.table_oid == table_oid)
                .ok_or_else(|| {
                    safety::GraphError::Internal(format!(
                        "cannot hydrate node from unregistered table OID {table_oid}"
                    ))
                })?;
            let lookup =
                SourceKeyLookup::prepare(table_oid, &table.id_columns, "src", self.governor)?;
            self.lookups.try_reserve(1).map_err(|_| {
                hydration_allocation_error(
                    self.governor,
                    std::mem::size_of::<(u32, SourceKeyLookup)>(),
                )
            })?;
            self.lookups.insert(table_oid, lookup);
        }
        hydrate_node_with_lookup(node_id, self.governor, &self.lookups[&table_oid])
    }
}

fn hydration_allocation_error(
    governor: &crate::resource::ResourceGovernor,
    requested_bytes: usize,
) -> safety::GraphError {
    safety::GraphError::ResourceLimit {
        resource: "memory bytes".into(),
        phase: crate::resource::ResourcePhase::QueryHydrate.as_str().into(),
        used: governor.memory_used().as_u64(),
        requested: u64::try_from(requested_bytes).unwrap_or(u64::MAX),
        limit: governor.memory_limit().as_u64(),
    }
}

#[cfg(feature = "development")]
thread_local! {
    static HYDRATION_CANCEL_BEFORE_SPI: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(feature = "development")]
fn inject_hydration_cancellation() {
    HYDRATION_CANCEL_BEFORE_SPI.with(|armed| {
        if armed.replace(false) {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
                "injected hydration cancellation"
            );
        }
    });
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn inject_hydration_cancellation() {}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_arm_hydration_cancel")]
fn test_arm_hydration_cancel() -> bool {
    HYDRATION_CANCEL_BEFORE_SPI.with(|armed| armed.set(true));
    true
}

/// Deterministic barrier between source metadata preparation and source reads.
#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_hydrate_node_after_lookup")]
fn test_hydrate_node_after_lookup(
    table_oid: pgrx::pg_sys::Oid,
    node_id: &str,
    barrier_key: i64,
) -> Option<pgrx::JsonB> {
    crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(|| {
        acl::check_table_acl(table_oid.to_u32())?;
        let governor = hydration_governor()?;
        let (tables, _, _) = read_catalog()?;
        let table = tables
            .iter()
            .find(|table| table.table_oid == table_oid.to_u32())
            .ok_or_else(|| {
                safety::GraphError::Internal("unregistered source lookup table".into())
            })?;
        let lookup =
            SourceKeyLookup::prepare(table.table_oid, &table.id_columns, "src", &governor)?;
        Spi::connect(|client| {
            client
                .select(
                    "SELECT pg_catalog.pg_advisory_xact_lock($1)",
                    None,
                    &[barrier_key.into()],
                )
                .map(|_| ())
        })
        .map_err(|error| {
            safety::GraphError::Internal(format!("source lookup barrier failed: {error}"))
        })?;
        hydrate_node_with_lookup(node_id, &governor, &lookup)
    }))
    .unwrap_or_else(|error| error.report())
}

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

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn hydrate_node(
    table_oid: u32,
    node_id: &str,
) -> safety::GraphResult<Option<pgrx::JsonB>> {
    let governor = hydration_governor()?;
    hydrate_node_governed(table_oid, node_id, &governor)
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn hydrate_node_with_tables(
    table_oid: u32,
    node_id: &str,
    tables: &[crate::builder::RegisteredTable],
) -> safety::GraphResult<Option<pgrx::JsonB>> {
    let governor = hydration_governor()?;
    hydrate_node_governed_with_tables(table_oid, node_id, &governor, tables)
}

#[allow(dead_code, reason = "compatibility entry point")]
pub(crate) fn hydrate_node_governed(
    table_oid: u32,
    node_id: &str,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Option<pgrx::JsonB>> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    hydrate_node_governed_with_tables(table_oid, node_id, governor, &tables)
}

pub(crate) fn hydrate_node_governed_with_tables(
    table_oid: u32,
    node_id: &str,
    governor: &crate::resource::ResourceGovernor,
    tables: &[crate::builder::RegisteredTable],
) -> safety::GraphResult<Option<pgrx::JsonB>> {
    NodeHydrator::new(governor, tables).hydrate(table_oid, node_id)
}

fn hydrate_node_with_lookup(
    node_id: &str,
    governor: &crate::resource::ResourceGovernor,
    lookup: &SourceKeyLookup,
) -> safety::GraphResult<Option<pgrx::JsonB>> {
    let mut workspace = reserve_hydration_workspace(governor, 1, node_id.len())?;
    let table_name = &lookup.table_name;
    let predicate = lookup.scalar_predicate();
    let size_query = format!(
        "SELECT pg_catalog.pg_column_size(pg_catalog.to_jsonb(src.*))::bigint,
                    pg_catalog.octet_length(pg_catalog.to_jsonb(src.*)::text)::bigint
               FROM {} src WHERE {} LIMIT 1",
        table_name, predicate
    );
    let size_args = vec![lookup.scalar_arg(node_id)];
    let json_sizes =
        crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(|| {
            Spi::connect(|client| {
                let result = client
                    .select(&size_query, None, &size_args)
                    .map_err(|error| {
                        safety::GraphError::Internal(format!(
                            "hydration size preflight failed: {error}"
                        ))
                    })?;
                if result.is_empty() {
                    return Ok(None);
                }
                let row = result.first();
                let binary = row.get::<i64>(1).map_err(|error| {
                    safety::GraphError::Internal(format!(
                        "hydration JSONB size read failed: {error}"
                    ))
                })?;
                let text = row.get::<i64>(2).map_err(|error| {
                    safety::GraphError::Internal(format!(
                        "hydration JSON text size read failed: {error}"
                    ))
                })?;
                Ok::<_, safety::GraphError>(binary.zip(text))
            })
        }))?;
    let Some((binary_bytes, text_bytes)) = json_sizes else {
        return Ok(None);
    };
    reserve_jsonb_materialization(
        &mut workspace,
        1,
        u64::try_from(binary_bytes.max(0)).unwrap_or(0),
        u64::try_from(text_bytes.max(0)).unwrap_or(0),
    )?;
    let hydrate_query = format!(
        "SELECT to_jsonb(src.*) FROM {} src WHERE {} LIMIT 1",
        table_name, predicate
    );
    let hydrate_args = vec![lookup.scalar_arg(node_id)];
    let hydrated =
        crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(|| {
            Spi::connect(|client| {
                let result = client
                    .select(&hydrate_query, None, &hydrate_args)
                    .map_err(|e| {
                        safety::GraphError::Internal(format!(
                            "hydration failed for {}: {}",
                            table_name, e
                        ))
                    })?;
                if result.is_empty() {
                    return Ok(None);
                }
                let row = result.first();
                row.get::<pgrx::JsonB>(1).map_err(|e| {
                    safety::GraphError::Internal(format!("hydration read failed: {}", e))
                })
            })
        }))?;
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
    let (tables, _edges, _filter_columns) = read_catalog()?;
    hydrate_nodes_governed_with_tables(rows, governor, &tables)
}

pub(crate) fn hydrate_nodes_governed_with_tables(
    rows: &[types::TraversalResult],
    governor: &crate::resource::ResourceGovernor,
    tables: &[crate::builder::RegisteredTable],
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
    let mut tables_by_oid = HashMap::with_capacity(needed_table_oids.len());
    for table in tables {
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
        let lookup = SourceKeyLookup::prepare(table_oid, &table.id_columns, "src", governor)?;
        let table_name = &lookup.table_name;
        let pk_expr = &lookup.key_expr;
        let predicate = lookup.batch_predicate();
        let size_query = format!(
                "SELECT pg_catalog.count(*)::bigint,
                        COALESCE(pg_catalog.sum(pg_catalog.pg_column_size(pg_catalog.to_jsonb(src.*))), 0)::bigint,
                        COALESCE(pg_catalog.sum(pg_catalog.octet_length(pg_catalog.to_jsonb(src.*)::text)), 0)::bigint
                   FROM {} src WHERE {}",
                table_name,
                predicate
            );
        let size_params = vec![lookup.batch_arg(&node_ids, governor)?];
        let (visible_rows, binary_bytes, text_bytes) =
            crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(
                || {
                    inject_hydration_cancellation();
                    Spi::connect(|client| {
                        let result =
                            client
                                .select(&size_query, None, &size_params)
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
                    })
                },
            ))?;
        reserve_jsonb_materialization(
            &mut workspace,
            u64::try_from(visible_rows.max(0)).unwrap_or(0),
            u64::try_from(binary_bytes.max(0)).unwrap_or(0),
            u64::try_from(text_bytes.max(0)).unwrap_or(0),
        )?;
        crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(
            crate::resource::check_postgres_interrupts,
        ));
        let query = format!(
            "SELECT {} AS graph_node_id, to_jsonb(src.*) FROM {} src WHERE {}",
            pk_expr, table_name, predicate
        );
        let hydration_params = vec![lookup.batch_arg(&node_ids, governor)?];
        crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(|| {
            Spi::connect(|client| {
                let result = client
                    .select(&query, None, &hydration_params)
                    .map_err(|e| {
                        safety::GraphError::Internal(format!(
                            "batch hydration failed for {}: {}",
                            table_name, e
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
            })
        }))?;
    }

    workspace.retain_until_governor_drop();
    Ok(hydrated)
}

pub(crate) fn visible_node_keys_governed_with_tables(
    ids_by_table: &HashMap<u32, Vec<String>>,
    governor: &crate::resource::ResourceGovernor,
    tables: &[crate::builder::RegisteredTable],
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
    let tables_by_oid = tables
        .iter()
        .filter(|table| needed_table_oids.contains(&table.table_oid))
        .map(|table| (table.table_oid, table))
        .collect::<HashMap<_, _>>();
    let mut visible = HashSet::new();
    for (table_oid, node_ids) in ids_by_table {
        crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(
            crate::resource::check_postgres_interrupts,
        ));
        let table = tables_by_oid.get(table_oid).ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot check source visibility for unregistered table OID {table_oid}"
            ))
        })?;
        let lookup = SourceKeyLookup::prepare(*table_oid, &table.id_columns, "src", governor)?;
        let table_name = &lookup.table_name;
        let pk_expr = &lookup.key_expr;
        let predicate = lookup.batch_predicate();
        let query = format!(
            "SELECT {pk_expr} AS graph_node_id FROM {} src WHERE {predicate}",
            table_name
        );
        let visibility_params = vec![lookup.batch_arg(node_ids, governor)?];
        crate::sql_visibility::postgres_error_as_rust_unwind(std::panic::AssertUnwindSafe(|| {
            Spi::connect(|client| {
                let result = client
                    .select(&query, None, &visibility_params)
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
            })
        }))?;
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
