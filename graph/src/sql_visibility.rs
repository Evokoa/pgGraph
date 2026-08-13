//! PostgreSQL adapter for caller-scoped topology visibility.

#[cfg(feature = "development")]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
#[cfg(feature = "development")]
use std::time::Instant;

use pgrx::prelude::*;
use roaring::RoaringBitmap;

use crate::builder::{RegisteredEdge, RegisteredTable};
use crate::catalog::{primary_key_expr, sql_table_name_from_oid};
use crate::resource::{ByteCount, ResourceGovernor, ResourcePhase, WorkUnits};
use crate::safety::{GraphError, GraphResult};
use crate::visibility::{VisibilityCoordinator, VisibilityScope};
use crate::{acl, config, ENGINE};

const VISIBILITY_CURSOR_ROWS: i64 = 256;
const BITMAP_BYTES_PER_ID_UPPER_BOUND: u64 = 8;

/// Unforgeable evidence that PostgreSQL policy preparation selected a scope.
pub(crate) struct PreparedVisibilityProof(());

fn prepared_coordinator(scope: VisibilityScope) -> VisibilityCoordinator {
    VisibilityCoordinator::from_prepared_scope(PreparedVisibilityProof(()), scope)
}

#[cfg(feature = "development")]
type VisibilityTimer = Instant;
#[cfg(not(feature = "development"))]
type VisibilityTimer = ();

thread_local! {
    /// Owns graph-sized Rust allocations across PostgreSQL ERROR/longjmp.
    /// `PgTryBuilder::finally` clears it if an SPI scan is cancelled.
    static VISIBILITY_BUILD_SLOT: RefCell<Option<VisibilityScope>> = const { RefCell::new(None) };

    #[cfg(feature = "development")]
    static VISIBILITY_CANCEL_AFTER_ROWS: Cell<Option<u64>> = const { Cell::new(None) };

    #[cfg(feature = "development")]
    static VISIBILITY_FORCE_MISSING_IDENTITY: Cell<bool> = const { Cell::new(false) };

    #[cfg(feature = "development")]
    static VISIBILITY_LAST_METRICS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
}

/// Build one eager caller-visibility scope under the query governor.
pub(crate) fn prepare_eager_visibility(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    governor: &ResourceGovernor,
) -> GraphResult<VisibilityCoordinator> {
    let started_at = start_visibility_timer();
    match config::parsed_rls_mode() {
        Some(config::RlsMode::LegacyBypass) => {
            acl::require_rls_bypass_privilege()?;
            let proof = PreparedVisibilityProof(());
            let scope = VisibilityScope::unrestricted(&proof);
            return Ok(prepared_coordinator(record_visibility_metrics(
                scope, started_at,
            )));
        }
        Some(config::RlsMode::Enforce) => {}
        None => {
            return Err(GraphError::InvalidFilter {
                reason: format!(
                    "unsupported graph.rls_mode '{}'; expected 'enforce' or 'legacy_bypass'",
                    config::rls_mode()
                ),
            });
        }
    }

    let any_rls_active = tables
        .iter()
        .any(|table| acl::row_security_applies_to_outer_caller(table.table_oid))
        || edges
            .iter()
            .filter(|edge| {
                !tables
                    .iter()
                    .any(|table| table.table_oid == edge.from_table_oid)
            })
            .any(|edge| acl::row_security_applies_to_outer_caller(edge.from_table_oid));
    if !any_rls_active {
        let proof = PreparedVisibilityProof(());
        let scope = VisibilityScope::unrestricted(&proof);
        return Ok(prepared_coordinator(record_visibility_metrics(
            scope, started_at,
        )));
    }

    let active_node_tables = tables
        .iter()
        .filter(|table| acl::row_security_applies_to_outer_caller(table.table_oid))
        .collect::<Vec<_>>();
    let active_edges = edges
        .iter()
        .filter(|edge| acl::row_security_applies_to_outer_caller(edge.from_table_oid))
        .collect::<Vec<_>>();

    let active_mapping_ids = active_edges
        .iter()
        .map(|edge| edge.mapping_id)
        .collect::<BTreeSet<_>>();
    let (identity_slots, projected_nodes, relationship_rls_edge_types) = ENGINE.with(|engine| {
        let engine = engine.borrow();
        let mut relationship_rls_edge_types = RoaringBitmap::new();
        for edge in &active_edges {
            let edge_type = engine
                .edge_type_registry
                .iter()
                .position(|label| label == &edge.label)
                .ok_or_else(|| {
                    GraphError::Internal(format!(
                        "registered RLS edge label '{}' is absent from the loaded projection",
                        edge.label
                    ))
                })?;
            relationship_rls_edge_types
                .insert(u32::try_from(edge_type).map_err(|_| {
                    GraphError::Internal("edge type index exceeds u32".to_string())
                })?);
        }
        if !active_edges.is_empty()
            && (force_missing_relationship_identity_for_test()
                || engine
                    .edge_store
                    .type_ids_slice()
                    .iter()
                    .zip(engine.edge_store.relationship_ids_slice())
                    .any(|(edge_type, relationship_id)| {
                        relationship_rls_edge_types.contains(u32::from(*edge_type))
                            && *relationship_id == crate::edge_store::NO_RELATIONSHIP_ID
                    }))
        {
            return Err(GraphError::RlsRelationshipIdentityMissing);
        }
        Ok::<_, GraphError>((
            engine.relationship_identities.len(),
            engine.node_store.node_count(),
            relationship_rls_edge_types,
        ))
    })?;

    let bitmap_ids = u64::from(projected_nodes)
        .checked_add(u64::try_from(identity_slots).unwrap_or(u64::MAX))
        .ok_or_else(|| GraphError::Internal("visibility bitmap estimate overflowed".into()))?;
    let bitmap_bytes = bitmap_ids
        .checked_mul(BITMAP_BYTES_PER_ID_UPPER_BOUND)
        .ok_or_else(|| GraphError::Internal("visibility bitmap estimate overflowed".into()))?;
    let bitmap_lease = governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            ByteCount::from_bytes(bitmap_bytes),
        )
        .map_err(crate::safety::resource_limit_error)?;

    let initial_scope = ENGINE.with(|engine| {
        let engine = engine.borrow();
        let proof = PreparedVisibilityProof(());
        let mut scope = VisibilityScope::enforced(
            &proof,
            RoaringBitmap::new(),
            RoaringBitmap::new(),
            relationship_rls_edge_types,
        );
        for table in &active_node_tables {
            if let Some(members) = engine.table_membership.get(&table.table_oid) {
                scope.hide_nodes(members);
            }
            for node_idx in
                crate::projection::tx_delta::added_node_indexes(table.table_oid, None, false)
            {
                scope.hide_node(node_idx);
            }
        }
        for (index, identity) in engine.relationship_identities.iter().enumerate() {
            if let Some(identity) = identity {
                if active_mapping_ids.contains(&identity.mapping_id) {
                    let relationship_id = u32::try_from(index).map_err(|_| {
                        GraphError::Internal("relationship identity index exceeds u32".to_string())
                    })?;
                    scope.hide_relationship(relationship_id);
                }
            }
        }
        let base_identity_count = engine.relationship_identities.len();
        crate::projection::tx_delta::for_each_relationship_identity(
            base_identity_count,
            |relationship_id, identity| {
                if active_mapping_ids.contains(&identity.mapping_id) {
                    scope.hide_relationship(relationship_id);
                }
            },
        );

        Ok::<_, GraphError>(scope)
    })?;

    let scope = pgrx::pg_sys::PgTryBuilder::new(AssertUnwindSafe(|| {
        VISIBILITY_BUILD_SLOT.with(|slot| {
            *slot.borrow_mut() = Some(initial_scope);
        });
        let result = (|| {
            for table in active_node_tables {
                scan_visible_node_keys(table, governor)?;
            }
            for edge in active_edges {
                scan_visible_relationship_keys(edge, identity_slots, governor)?;
            }
            VISIBILITY_BUILD_SLOT
                .with(|slot| slot.borrow_mut().take())
                .ok_or_else(|| {
                    GraphError::Internal("visibility build scope disappeared".to_string())
                })
        })();
        result
    }))
    .finally(|| {
        VISIBILITY_BUILD_SLOT.with(|slot| {
            slot.borrow_mut().take();
        });
    })
    .execute()?;
    bitmap_lease.retain_until_governor_drop();
    Ok(prepared_coordinator(record_visibility_metrics(
        scope, started_at,
    )))
}

#[cfg(feature = "development")]
fn start_visibility_timer() -> VisibilityTimer {
    Instant::now()
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn start_visibility_timer() -> VisibilityTimer {}

#[cfg(feature = "development")]
fn record_visibility_metrics(
    scope: VisibilityScope,
    started_at: VisibilityTimer,
) -> VisibilityScope {
    let (hidden_nodes, hidden_relationships) = scope.hidden_counts();
    let elapsed_micros = u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX);
    VISIBILITY_LAST_METRICS.with(|metrics| {
        metrics.set((elapsed_micros, hidden_nodes, hidden_relationships));
    });
    scope
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn record_visibility_metrics(
    scope: VisibilityScope,
    _started_at: VisibilityTimer,
) -> VisibilityScope {
    scope
}

fn scan_visible_node_keys(table: &RegisteredTable, governor: &ResourceGovernor) -> GraphResult<()> {
    acl::check_table_acl(table.table_oid)?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
    let key_expr = primary_key_expr("src", &table.id_columns);
    scan_visible_keys(table_name.as_sql(), &key_expr, governor, |source_key| {
        let node_idx = ENGINE
            .with(|engine| engine.borrow().resolve(table.table_oid, source_key))
            .or_else(|| {
                crate::projection::tx_delta::resolve_added_node(
                    table.table_oid,
                    source_key,
                    None,
                    false,
                )
            });
        if let Some(node_idx) = node_idx {
            VISIBILITY_BUILD_SLOT.with(|slot| {
                if let Some(scope) = slot.borrow_mut().as_mut() {
                    scope.reveal_node(node_idx);
                }
            });
        }
        Ok(())
    })
}

fn scan_visible_relationship_keys(
    edge: &RegisteredEdge,
    base_identity_count: usize,
    governor: &ResourceGovernor,
) -> GraphResult<()> {
    acl::check_table_acl(edge.from_table_oid)?;
    let table_name = sql_table_name_from_oid(edge.from_table_oid)?;
    let key_expr = primary_key_expr("src", &edge.source_key_columns);
    scan_visible_keys(table_name.as_sql(), &key_expr, governor, |source_key| {
        let relationship_id = ENGINE
            .with(|engine| {
                engine
                    .borrow()
                    .relationship_identities
                    .find_id(edge.mapping_id, source_key)
            })
            .or_else(|| {
                crate::projection::tx_delta::find_relationship_identity_id(
                    base_identity_count,
                    edge.mapping_id,
                    source_key,
                )
            });
        if let Some(relationship_id) = relationship_id {
            VISIBILITY_BUILD_SLOT.with(|slot| {
                if let Some(scope) = slot.borrow_mut().as_mut() {
                    scope.reveal_relationship(relationship_id);
                }
            });
        }
        Ok(())
    })
}

fn scan_visible_keys(
    table_sql: &str,
    key_expr: &str,
    governor: &ResourceGovernor,
    mut visit: impl FnMut(&str) -> GraphResult<()>,
) -> GraphResult<()> {
    let max_key_bytes = Spi::get_one::<i64>(&format!(
        "SELECT COALESCE(pg_catalog.max(pg_catalog.octet_length({key_expr})), 0)::bigint \
         FROM {table_sql} AS src"
    ))
    .map_err(|error| GraphError::Internal(format!("RLS visibility preflight failed: {error}")))?
    .unwrap_or(0)
    .max(0);
    let batch_bytes = u64::try_from(max_key_bytes)
        .unwrap_or(u64::MAX)
        .checked_mul(u64::try_from(VISIBILITY_CURSOR_ROWS).unwrap_or(u64::MAX))
        .ok_or_else(|| GraphError::Internal("visibility cursor estimate overflowed".into()))?;
    let _batch_lease = governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            ByteCount::from_bytes(batch_bytes),
        )
        .map_err(crate::safety::resource_limit_error)?;

    Spi::connect(|client| {
        let query = format!("SELECT {key_expr} AS graph_source_key FROM {table_sql} AS src");
        let mut cursor = client.open_cursor(&query, &[]);
        loop {
            crate::resource::check_postgres_interrupts();
            let rows = cursor.fetch(VISIBILITY_CURSOR_ROWS).map_err(|error| {
                GraphError::Internal(format!("RLS visibility cursor fetch failed: {error}"))
            })?;
            if rows.is_empty() {
                break;
            }
            let fetched = rows.len();
            for row in rows {
                let source_key = row
                    .get::<String>(1)
                    .map_err(|error| {
                        GraphError::Internal(format!(
                            "RLS visibility source-key read failed: {error}"
                        ))
                    })?
                    .ok_or_else(|| {
                        GraphError::Internal(
                            "RLS visibility scan returned a NULL source key".to_string(),
                        )
                    })?;
                visit(&source_key)?;
                inject_visibility_scan_cancellation();
            }
            governor
                .consume_work(
                    ResourcePhase::QueryVisibility,
                    WorkUnits::new(u64::try_from(fetched).unwrap_or(u64::MAX)),
                )
                .map_err(crate::safety::resource_limit_error)?;
            governor
                .check_elapsed(ResourcePhase::QueryVisibility)
                .map_err(crate::safety::resource_limit_error)?;
        }
        Ok::<(), GraphError>(())
    })
}

#[cfg(feature = "development")]
pub(crate) fn visibility_build_slot_is_empty() -> bool {
    VISIBILITY_BUILD_SLOT.with(|slot| slot.borrow().is_none())
}

#[cfg(feature = "development")]
fn inject_visibility_scan_cancellation() {
    VISIBILITY_CANCEL_AFTER_ROWS.with(|remaining| match remaining.get() {
        None => {}
        Some(0) => {
            remaining.set(None);
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
                "injected RLS visibility scan cancellation"
            );
        }
        Some(rows) => remaining.set(Some(rows - 1)),
    });
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn inject_visibility_scan_cancellation() {}

#[cfg(feature = "development")]
fn force_missing_relationship_identity_for_test() -> bool {
    VISIBILITY_FORCE_MISSING_IDENTITY.with(|armed| armed.replace(false))
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn force_missing_relationship_identity_for_test() -> bool {
    false
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_arm_visibility_scan_cancel")]
fn test_arm_visibility_scan_cancel(after_rows: i64) -> bool {
    if after_rows < 0 {
        return false;
    }
    VISIBILITY_CANCEL_AFTER_ROWS.with(|remaining| {
        remaining.set(Some(u64::try_from(after_rows).unwrap_or(u64::MAX)));
    });
    true
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_visibility_build_slot_empty")]
fn test_visibility_build_slot_empty() -> bool {
    visibility_build_slot_is_empty()
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_rls_applies_to_outer_caller")]
fn test_rls_applies_to_outer_caller(table_oid: pgrx::pg_sys::Oid) -> bool {
    acl::row_security_applies_to_outer_caller(table_oid.to_u32())
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_visibility_metrics")]
fn test_visibility_metrics() -> pgrx::JsonB {
    let (elapsed_micros, hidden_nodes, hidden_relationships) =
        VISIBILITY_LAST_METRICS.with(Cell::get);
    pgrx::JsonB(serde_json::json!({
        "elapsed_micros": elapsed_micros,
        "hidden_nodes": hidden_nodes,
        "hidden_relationships": hidden_relationships,
    }))
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_arm_missing_relationship_identity")]
fn test_arm_missing_relationship_identity() -> bool {
    VISIBILITY_FORCE_MISSING_IDENTITY.with(|armed| armed.set(true));
    true
}
