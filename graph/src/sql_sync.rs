//! SQL sync-log replay, trigger management, and tenant-scope helpers.

use crate::catalog::{
    catalog_fingerprint, read_catalog, selected_or_default_graph_metadata, table_oid_from_name,
};
use crate::filter_index::{EncodedFilterValue, FilterColumnType};
use crate::persistence::{
    graph_artifact_checksum_for_path, graph_artifact_version, graph_file_path, load_graph_file,
    projection_manifest_root, read_sync_checkpoint,
};
use crate::projection::ingest::{ProjectionIngestResult, ProjectionIngester, ProjectionSyncRow};
use crate::projection::manifest::ProjectionManifestStore;
use crate::projection::normalize::{MutationBufferLimits, MutationOperation};
use crate::resolution_index::ResolutionIndexBuilder;
use crate::sql_filters::{
    encode_date_filter_value, encode_timestamptz_filter_value, parse_uuid_u128,
};
use crate::types::TraversalDirection;
use crate::{builder, config, engine, safety, sync, ENGINE};
use pgrx::prelude::*;
use std::collections::{HashMap, HashSet};
use xxhash_rust::xxh3::xxh3_64;

pub(crate) fn current_sync_mode() -> safety::GraphResult<config::SyncMode> {
    match config::parsed_sync_mode() {
        Some(config::SyncMode::Wal) => Err(safety::GraphError::InvalidFilter {
            reason:
                "graph.sync_mode = 'wal' is reserved for roadmap work; please use 'trigger' or 'manual'"
                    .to_string(),
        }),
        Some(mode) => Ok(mode),
        None => Err(safety::GraphError::InvalidFilter {
            reason: format!(
                "unsupported graph.sync_mode '{}'; expected 'manual', 'trigger', or 'wal'",
                config::sync_mode()
            ),
        }),
    }
}

pub(crate) fn install_sync_triggers() -> safety::GraphResult<usize> {
    let (tables, _edges, filter_columns) = read_catalog()?;
    let mut installed = 0usize;
    for table in &tables {
        let oid = table_oid_from_name(&table.table_name)?;
        let qt = sync::get_qualified_table(oid)?;
        let mut trigger_columns = table.columns.to_vec();
        for filter in filter_columns
            .iter()
            .filter(|filter| filter.table_name == table.table_name)
        {
            if !trigger_columns
                .iter()
                .any(|column| column == &filter.column_name)
            {
                trigger_columns.push(filter.column_name.clone());
            }
        }
        if let Some(tenant_column) = &table.tenant_column {
            if !trigger_columns.iter().any(|column| column == tenant_column) {
                trigger_columns.push(tenant_column.clone());
            }
        }
        let trigger_sql = sync::generate_trigger_sql(&qt, &table.id_columns, &trigger_columns);
        Spi::run(&trigger_sql).map_err(|e| {
            safety::GraphError::Internal(format!(
                "trigger creation failed for {}: {}",
                table.table_name, e
            ))
        })?;
        installed += 1;
    }

    Ok(installed)
}

pub(crate) fn remove_sync_triggers() -> safety::GraphResult<usize> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let mut removed = 0usize;
    for table in &tables {
        let oid = table_oid_from_name(&table.table_name)?;
        let qt = sync::get_qualified_table(oid)?;
        let table_sql = sync::qualified_table_sql(&qt);
        Spi::run(&format!(
            "DROP TRIGGER IF EXISTS graph_sync_insert ON {table_sql};
             DROP TRIGGER IF EXISTS graph_sync_update ON {table_sql};
             DROP TRIGGER IF EXISTS graph_sync_delete ON {table_sql};
             DROP TRIGGER IF EXISTS graph_sync_truncate ON {table_sql};",
        ))
        .map_err(|err| {
            safety::GraphError::Internal(format!(
                "trigger removal failed for {}: {}",
                table.table_name, err
            ))
        })?;
        removed += 1;
    }

    Ok(removed)
}

pub(crate) fn disabled_graph_trigger_count() -> safety::GraphResult<i32> {
    Spi::connect(|client| {
        let result = client.select(
            "SELECT count(*)::int
             FROM pg_trigger
             WHERE tgname IN ('graph_sync_insert', 'graph_sync_update', 'graph_sync_delete', 'graph_sync_truncate')
               AND tgenabled = 'D'",
            None,
            &[],
        )?;
        Ok::<_, pgrx::spi::SpiError>(result.first().get::<i32>(1)?.unwrap_or(0))
    })
    .map_err(|e| safety::GraphError::Internal(format!("trigger status check failed: {}", e)))
}

pub(crate) fn pending_sync_rows(applied_sync_id: i64) -> safety::GraphResult<i64> {
    let applicable_table_oids = SyncReplayContext::load()?.applicable_table_oids();
    if applicable_table_oids.is_empty() {
        return Ok(0);
    }
    Spi::connect(|client| {
        let result = client.select(
            "SELECT CASE
                WHEN to_regclass('graph._sync_log') IS NULL THEN 0::bigint
                ELSE (
                    SELECT count(*)::bigint
                      FROM graph._sync_log
                     WHERE id > $1
                       AND table_oid::oid::integer = ANY($2::int4[])
                )
             END",
            None,
            &[applied_sync_id.into(), applicable_table_oids.into()],
        )?;
        Ok::<_, pgrx::spi::SpiError>(result.first().get::<i64>(1)?.unwrap_or(0))
    })
    .map_err(|e| safety::GraphError::Internal(format!("sync status check failed: {}", e)))
}

pub(crate) fn max_sync_log_id() -> safety::GraphResult<i64> {
    let applicable_table_oids = SyncReplayContext::load()?.applicable_table_oids();
    if applicable_table_oids.is_empty() {
        return Ok(0);
    }
    Spi::connect(|client| {
        let result = client.select(
            "SELECT CASE
                WHEN to_regclass('graph._sync_log') IS NULL THEN 0::bigint
                ELSE (
                    SELECT COALESCE(max(id), 0)::bigint
                      FROM graph._sync_log
                     WHERE table_oid::oid::integer = ANY($1::int4[])
                )
             END",
            None,
            &[applicable_table_oids.into()],
        )?;
        Ok::<_, pgrx::spi::SpiError>(result.first().get::<i64>(1)?.unwrap_or(0))
    })
    .map_err(|e| safety::GraphError::Internal(format!("sync checkpoint read failed: {}", e)))
}

#[derive(Default)]
pub(crate) struct SyncApplyStats {
    pub(crate) inserts: i64,
    pub(crate) updates: i64,
    pub(crate) deletes: i64,
    pub(crate) truncates: i64,
}

#[derive(Debug, Default)]
pub(crate) struct ProjectionIngestStats {
    pub(crate) rows_ingested: i64,
    pub(crate) segments_published: i64,
    pub(crate) sync_watermark: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncOp {
    Insert,
    Update,
    Delete,
    Truncate,
}

impl SyncOp {
    fn edge_delta_estimate(self) -> usize {
        match self {
            Self::Insert | Self::Delete => 1,
            Self::Update => 2,
            Self::Truncate => 0,
        }
    }
}

fn parse_sync_op(op: &str) -> safety::GraphResult<SyncOp> {
    match op.trim() {
        "I" => Ok(SyncOp::Insert),
        "U" => Ok(SyncOp::Update),
        "D" => Ok(SyncOp::Delete),
        "T" => Ok(SyncOp::Truncate),
        other => Err(safety::GraphError::Internal(format!(
            "sync row has unsupported operation '{}'",
            other
        ))),
    }
}

pub(crate) struct SyncLogEntry {
    pub(crate) id: i64,
    pub(crate) op: SyncOp,
    pub(crate) table_oid: Option<u32>,
    pub(crate) table_name: String,
    pub(crate) old_pk: Option<String>,
    pub(crate) new_pk: Option<String>,
    pub(crate) properties: Option<String>,
    pub(crate) old_row: Option<String>,
    pub(crate) new_row: Option<String>,
}

#[derive(Debug)]
struct ParsedSyncRows {
    old: Option<serde_json::Value>,
    new: Option<serde_json::Value>,
}

impl ParsedSyncRows {
    fn from_entry(entry: &SyncLogEntry) -> safety::GraphResult<Self> {
        Ok(Self {
            old: parse_sync_row_image(entry.id, "old_row", entry.old_row.as_deref())?,
            new: parse_sync_row_image(entry.id, "new_row", entry.new_row.as_deref())?,
        })
    }
}

enum SyncRowOperation<'a> {
    Insert {
        pk: &'a str,
        tenant: Option<&'a str>,
    },
    Update {
        old_pk: &'a str,
        new_pk: &'a str,
        old_tenant: Option<&'a str>,
        new_tenant: Option<&'a str>,
    },
    Delete {
        pk: &'a str,
        old_tenant: Option<&'a str>,
    },
    Truncate,
}

impl<'a> SyncRowOperation<'a> {
    fn from_entry(
        entry: &'a SyncLogEntry,
        tenant_change: &'a TenantChange,
    ) -> safety::GraphResult<Self> {
        match entry.op {
            SyncOp::Insert => {
                let pk = entry
                    .new_pk
                    .as_deref()
                    .or(entry.old_pk.as_deref())
                    .ok_or_else(|| {
                        safety::GraphError::Internal(format!(
                            "sync row {} missing insert pk",
                            entry.id
                        ))
                    })?;
                Ok(Self::Insert {
                    pk,
                    tenant: tenant_change.new.as_deref(),
                })
            }
            SyncOp::Update => {
                let old_pk = entry.old_pk.as_deref().ok_or_else(|| {
                    safety::GraphError::Internal(format!("sync row {} missing old_pk", entry.id))
                })?;
                let new_pk = entry.new_pk.as_deref().ok_or_else(|| {
                    safety::GraphError::Internal(format!("sync row {} missing new_pk", entry.id))
                })?;
                Ok(Self::Update {
                    old_pk,
                    new_pk,
                    old_tenant: tenant_change.old.as_deref(),
                    new_tenant: tenant_change.new.as_deref(),
                })
            }
            SyncOp::Delete => {
                let pk = entry
                    .old_pk
                    .as_deref()
                    .or(entry.new_pk.as_deref())
                    .ok_or_else(|| {
                        safety::GraphError::Internal(format!(
                            "sync row {} missing delete pk",
                            entry.id
                        ))
                    })?;
                Ok(Self::Delete {
                    pk,
                    old_tenant: tenant_change.old.as_deref(),
                })
            }
            SyncOp::Truncate => Ok(Self::Truncate),
        }
    }
}

fn parse_sync_row_image(
    entry_id: i64,
    field_name: &str,
    raw: Option<&str>,
) -> safety::GraphResult<Option<serde_json::Value>> {
    raw.map(|row| {
        serde_json::from_str(row).map_err(|err| {
            safety::GraphError::Internal(format!(
                "sync row {entry_id} {field_name} JSON parse failed: {err}"
            ))
        })
    })
    .transpose()
}

pub(crate) struct SyncReplayContext {
    tables: Vec<builder::RegisteredTable>,
    edges: Vec<builder::RegisteredEdge>,
    filters: Vec<builder::RegisteredFilterColumn>,
    table_oids: HashMap<String, u32>,
    all_table_oids: Vec<u32>,
    edge_source_tables: HashSet<String>,
    edge_source_oids: HashSet<u32>,
}

impl SyncReplayContext {
    fn load() -> safety::GraphResult<Self> {
        let (tables, edges, filters) = read_catalog()?;
        let mut table_oids = HashMap::new();

        for table in &tables {
            if let Ok(oid) = table_oid_from_name(&table.table_name) {
                table_oids.insert(table.table_name.clone(), oid);
            }
        }
        for edge in &edges {
            if !table_oids.contains_key(&edge.from_table) {
                if let Ok(oid) = table_oid_from_name(&edge.from_table) {
                    table_oids.insert(edge.from_table.clone(), oid);
                }
            }
            if !table_oids.contains_key(&edge.to_table) {
                if let Ok(oid) = table_oid_from_name(&edge.to_table) {
                    table_oids.insert(edge.to_table.clone(), oid);
                }
            }
        }

        let all_table_oids = table_oids.values().copied().collect::<Vec<_>>();
        let edge_source_tables = edges
            .iter()
            .map(|edge| edge.from_table.clone())
            .collect::<HashSet<_>>();
        let edge_source_oids = edges
            .iter()
            .filter_map(|edge| table_oids.get(&edge.from_table).copied())
            .collect::<HashSet<_>>();

        Ok(Self {
            tables,
            edges,
            filters,
            table_oids,
            all_table_oids,
            edge_source_tables,
            edge_source_oids,
        })
    }

    fn table_oid(&self, table_name: &str) -> Option<u32> {
        self.table_oids.get(table_name).copied()
    }

    fn applicable_table_oids(&self) -> Vec<i32> {
        let mut oids = self
            .all_table_oids
            .iter()
            .chain(self.edge_source_oids.iter())
            .filter_map(|oid| i32::try_from(*oid).ok())
            .collect::<Vec<_>>();
        oids.sort_unstable();
        oids.dedup();
        oids
    }

    fn table_oid_or_lookup(&mut self, table_name: &str) -> safety::GraphResult<u32> {
        if let Some(oid) = self.table_oid(table_name) {
            return Ok(oid);
        }
        let oid = table_oid_from_name(table_name)?;
        self.table_oids.insert(table_name.to_string(), oid);
        self.all_table_oids.push(oid);
        Ok(oid)
    }
}

struct LegacySyncEntry {
    id: i64,
    op: SyncOp,
    table_name: String,
    old_pk: String,
    new_pk: String,
    properties: Option<String>,
}

fn required_sync_i64(value: Option<i64>, column: &str) -> safety::GraphResult<i64> {
    value.ok_or_else(|| {
        safety::GraphError::Internal(format!("sync row missing required column {column}"))
    })
}

fn required_sync_string(value: Option<String>, column: &str) -> safety::GraphResult<String> {
    value.ok_or_else(|| {
        safety::GraphError::Internal(format!("sync row missing required column {column}"))
    })
}

pub(crate) fn apply_sync_internal() -> safety::GraphResult<SyncApplyStats> {
    ensure_engine_loaded_for_apply_sync()?;
    let target_sync_id = max_sync_log_id()?;
    apply_sync_to_high_watermark(target_sync_id)
}

pub(crate) fn apply_sync_to_high_watermark(
    target_sync_id: i64,
) -> safety::GraphResult<SyncApplyStats> {
    ensure_engine_loaded_for_apply_sync()?;
    if should_apply_sync_via_durable_projection() {
        return apply_sync_via_durable_projection(target_sync_id);
    }
    let mut stats = apply_sync_until(Some(target_sync_id), config::sync_batch_size())?;

    apply_legacy_sync_buffer(&mut stats)?;

    let pending = ENGINE.with(|e| pending_sync_rows(e.borrow().applied_sync_id))?;
    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.record_pending_sync_rows(pending);
    });

    Ok(stats)
}

fn ensure_engine_loaded_for_apply_sync() -> safety::GraphResult<()> {
    if ENGINE.with(|e| e.borrow().built) {
        return Ok(());
    }

    let graph_path = graph_file_path()?;
    if !graph_path.exists() {
        return Err(safety::GraphError::NotBuilt);
    }

    let mut loaded = load_graph_file(&graph_path)?;
    if let Ok((tables, edges, filters)) = read_catalog() {
        loaded.set_catalog_fingerprint(catalog_fingerprint(&tables, &edges, &filters));
    }
    ENGINE.with(|engine| {
        *engine.borrow_mut() = loaded;
    });
    Ok(())
}

fn should_apply_sync_via_durable_projection() -> bool {
    let graph_path = match graph_file_path() {
        Ok(path) => path,
        Err(_) => return false,
    };
    graph_path.exists()
        && ENGINE.with(|e| e.borrow().projection_mode == config::ProjectionMode::MutableOverlay)
}

fn apply_sync_via_durable_projection(target_sync_id: i64) -> safety::GraphResult<SyncApplyStats> {
    let graph_path = graph_file_path()?;
    let root = projection_manifest_root(&graph_path);
    let previous = ProjectionManifestStore::new(root).load_latest_current()?;
    let previous_watermark = previous.as_ref().map_or(
        read_sync_checkpoint(&graph_path)?.unwrap_or(0),
        |manifest| manifest.sync_watermark,
    );
    let batch_size = config::sync_batch_size().max(1);
    let entries =
        read_sync_log_entries_after(previous_watermark, batch_size, Some(target_sync_id))?;
    let mut stats = sync_apply_stats_from_entries(&entries);

    let projection =
        ingest_projection_until_internal(Some(batch_size as i64), None, Some(target_sync_id))?;
    if projection.sync_watermark > previous_watermark {
        reload_engine_after_projection_ingest(&graph_path, projection.sync_watermark)?;
    }

    apply_legacy_sync_buffer(&mut stats)?;

    let pending = ENGINE.with(|e| pending_sync_rows(e.borrow().applied_sync_id))?;
    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.record_pending_sync_rows(pending);
    });

    Ok(stats)
}

fn sync_apply_stats_from_entries(entries: &[SyncLogEntry]) -> SyncApplyStats {
    let mut stats = SyncApplyStats::default();
    for entry in entries {
        match entry.op {
            SyncOp::Insert => stats.inserts += 1,
            SyncOp::Update => stats.updates += 1,
            SyncOp::Delete => stats.deletes += 1,
            SyncOp::Truncate => stats.truncates += 1,
        }
    }
    stats
}

fn reload_engine_after_projection_ingest(
    graph_path: &std::path::Path,
    sync_watermark: i64,
) -> safety::GraphResult<()> {
    let mut loaded = load_graph_file(graph_path)?;
    loaded.record_applied_sync_id(sync_watermark);
    ENGINE.with(|engine| {
        *engine.borrow_mut() = loaded;
    });
    Ok(())
}

pub(crate) fn ingest_projection_internal(
    max_rows: Option<i64>,
    max_bytes: Option<i64>,
) -> safety::GraphResult<ProjectionIngestStats> {
    ingest_projection_until_internal(max_rows, max_bytes, None)
}

fn ingest_projection_until_internal(
    max_rows: Option<i64>,
    max_bytes: Option<i64>,
    target_sync_id: Option<i64>,
) -> safety::GraphResult<ProjectionIngestStats> {
    let graph_path = graph_file_path()?;
    if !graph_path.exists() {
        return Err(safety::GraphError::NotBuilt);
    }
    let root = projection_manifest_root(&graph_path);
    let store = ProjectionManifestStore::new(root.clone());
    let previous = store.load_latest_current()?;
    let previous_watermark = previous.as_ref().map_or(
        read_sync_checkpoint(&graph_path)?.unwrap_or(0),
        |manifest| manifest.sync_watermark,
    );
    let row_limit = optional_nonnegative_usize(max_rows, "max_rows")?
        .unwrap_or_else(|| config::sync_batch_size().max(1));
    let byte_limit = optional_nonnegative_usize(max_bytes, "max_bytes")?
        .unwrap_or_else(config::max_overlay_memory_bytes);
    let entries = read_sync_log_entries_after(previous_watermark, row_limit, target_sync_id)?;
    if entries.is_empty() {
        return Ok(ProjectionIngestStats {
            sync_watermark: previous_watermark,
            ..ProjectionIngestStats::default()
        });
    }
    let consumed_watermark = entries
        .iter()
        .map(|entry| entry.id)
        .max()
        .unwrap_or(previous_watermark);
    let mut context = SyncReplayContext::load()?;
    let rows = projection_rows_from_sync_entries(&entries, &mut context)?;
    let base_artifact_path = graph_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            safety::GraphError::Internal("graph artifact path has no file name".to_string())
        })?
        .to_string();
    let ingester = ProjectionIngester::new(
        root,
        base_artifact_path,
        graph_artifact_checksum_for_path(&graph_path)?,
        graph_artifact_version(),
    );
    let result = if rows.is_empty() {
        ingester.publish_empty_watermark(consumed_watermark)?
    } else {
        ingester.ingest_committed_rows(&rows, MutationBufferLimits::new(row_limit, byte_limit))?
    };
    Ok(projection_ingest_stats(result, previous_watermark))
}

fn projection_ingest_stats(
    result: ProjectionIngestResult,
    previous_watermark: i64,
) -> ProjectionIngestStats {
    ProjectionIngestStats {
        rows_ingested: result.rows_ingested.min(i64::MAX as usize) as i64,
        segments_published: result.segments_published.min(i64::MAX as usize) as i64,
        sync_watermark: result
            .manifest
            .as_ref()
            .map_or(previous_watermark, |manifest| manifest.sync_watermark),
    }
}

fn optional_nonnegative_usize(
    value: Option<i64>,
    name: &str,
) -> safety::GraphResult<Option<usize>> {
    value
        .map(|value| {
            usize::try_from(value).map_err(|_| safety::GraphError::InvalidFilter {
                reason: format!("{name} must be nonnegative"),
            })
        })
        .transpose()
}

pub(crate) fn apply_sync_until(
    target_sync_id: Option<i64>,
    batch_size: usize,
) -> safety::GraphResult<SyncApplyStats> {
    let batch_size = batch_size.max(1);
    let mut stats = SyncApplyStats::default();
    let mut context = SyncReplayContext::load()?;

    loop {
        let applied_sync_id = ENGINE.with(|e| e.borrow().applied_sync_id);
        let log_entries = read_sync_log_entries_after(applied_sync_id, batch_size, target_sync_id)?;
        if log_entries.is_empty() {
            break;
        }
        guard_edge_buffer_capacity_for_sync(&context, &log_entries)?;
        for entry in log_entries {
            apply_sync_log_entry_with_context(&entry, &mut stats, &mut context)?;
            ENGINE.with(|e| {
                e.borrow_mut().record_applied_sync_id(entry.id);
            });
        }
    }

    Ok(stats)
}

pub(crate) fn guard_edge_buffer_capacity_for_sync(
    context: &SyncReplayContext,
    entries: &[SyncLogEntry],
) -> safety::GraphResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    if context.edge_source_tables.is_empty() && context.edge_source_oids.is_empty() {
        return Ok(());
    }
    let estimated_edge_deltas = entries
        .iter()
        .filter(|entry| {
            entry
                .table_oid
                .is_some_and(|oid| context.edge_source_oids.contains(&oid))
                || context
                    .edge_source_tables
                    .contains(entry.table_name.as_str())
        })
        .map(|entry| entry.op.edge_delta_estimate())
        .sum::<usize>();
    if estimated_edge_deltas == 0 {
        return Ok(());
    }
    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        let used = eng.edge_buffer.len();
        let limit = crate::config::EDGE_BUFFER_SIZE.get() as usize;
        if used.saturating_add(estimated_edge_deltas) > limit {
            eng.mark_read_only(engine::ReadOnlyReason::EdgeBufferFull);
            return Err(safety::GraphError::EdgeBufferFull { size: used });
        }
        Ok(())
    })
}

pub(crate) fn read_sync_log_entries_after(
    applied_sync_id: i64,
    limit: usize,
    high_watermark: Option<i64>,
) -> safety::GraphResult<Vec<SyncLogEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let applicable_table_oids = SyncReplayContext::load()?.applicable_table_oids();
    if applicable_table_oids.is_empty() {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT id, op::text, table_oid::oid::integer, table_name,
                    old_pk, new_pk, properties::text, old_row::text, new_row::text
             FROM graph._sync_log
             WHERE id > $1
               AND table_oid::oid::integer = ANY($3::int4[])
               AND ($4::bigint IS NULL OR id <= $4)
             ORDER BY id
             LIMIT $2",
                None,
                &[
                    applied_sync_id.into(),
                    limit.into(),
                    applicable_table_oids.into(),
                    high_watermark.into(),
                ],
            )
            .map_err(|e| safety::GraphError::Internal(format!("sync log read failed: {e}")))?;
        let mut entries = Vec::new();
        for row in rows {
            let table_oid = row
                .get::<i32>(3)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("sync table_oid read failed: {e}"))
                })?
                .map(|oid| oid as u32);
            let id = required_sync_i64(
                row.get::<i64>(1).map_err(|e| {
                    safety::GraphError::Internal(format!("sync id read failed: {e}"))
                })?,
                "id",
            )?;
            let raw_op = required_sync_string(
                row.get::<String>(2).map_err(|e| {
                    safety::GraphError::Internal(format!("sync op read failed: {e}"))
                })?,
                "op",
            )?;
            entries.push(SyncLogEntry {
                id,
                op: parse_sync_op(&raw_op)
                    .map_err(|err| safety::GraphError::Internal(format!("sync row {id}: {err}")))?,
                table_oid,
                table_name: required_sync_string(
                    row.get::<String>(4).map_err(|e| {
                        safety::GraphError::Internal(format!("sync table_name read failed: {e}"))
                    })?,
                    "table_name",
                )?,
                old_pk: row.get::<String>(5).map_err(|e| {
                    safety::GraphError::Internal(format!("sync old_pk read failed: {e}"))
                })?,
                new_pk: row.get::<String>(6).map_err(|e| {
                    safety::GraphError::Internal(format!("sync new_pk read failed: {e}"))
                })?,
                properties: row.get::<String>(7).map_err(|e| {
                    safety::GraphError::Internal(format!("sync properties read failed: {e}"))
                })?,
                old_row: row.get::<String>(8).map_err(|e| {
                    safety::GraphError::Internal(format!("sync old_row read failed: {e}"))
                })?,
                new_row: row.get::<String>(9).map_err(|e| {
                    safety::GraphError::Internal(format!("sync new_row read failed: {e}"))
                })?,
            });
        }
        Ok::<_, safety::GraphError>(entries)
    })
}

fn apply_sync_log_entry_with_context(
    entry: &SyncLogEntry,
    stats: &mut SyncApplyStats,
    context: &mut SyncReplayContext,
) -> safety::GraphResult<()> {
    let table_oid = match entry.table_oid {
        Some(oid) => oid,
        None => context.table_oid_or_lookup(&entry.table_name)?,
    };
    let parsed = parse_sync_properties(entry.properties.as_deref());
    let rows = ParsedSyncRows::from_entry(entry)?;
    let tenant_change = tenant_change_from_entry(table_oid, &rows, &parsed, context)?;
    let operation = SyncRowOperation::from_entry(entry, &tenant_change)?;
    let edge_mutation_reservation =
        sync_entry_edge_mutation_reservation(entry, table_oid, context, &rows)?;

    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.reserve_edge_mutation_capacity(edge_mutation_reservation)?;
        apply_sync_row_operation(&mut eng, table_oid, entry, context, &rows, operation)?;
        match entry.op {
            SyncOp::Insert => {
                stats.inserts += 1;
            }
            SyncOp::Update => {
                stats.updates += 1;
            }
            SyncOp::Delete => {
                stats.deletes += 1;
            }
            SyncOp::Truncate => {
                stats.truncates += 1;
            }
        }
        Ok::<_, safety::GraphError>(())
    })
}

fn apply_sync_row_operation(
    eng: &mut engine::Engine,
    table_oid: u32,
    entry: &SyncLogEntry,
    context: &SyncReplayContext,
    rows: &ParsedSyncRows,
    operation: SyncRowOperation<'_>,
) -> safety::GraphResult<()> {
    match operation {
        SyncRowOperation::Insert { pk, tenant } => {
            sync::sync_insert(eng, table_oid, pk, tenant)?;
            refresh_filter_index_from_sync(
                eng,
                table_oid,
                pk,
                &context.filters,
                &context.table_oids,
                entry,
                rows,
            )?;
            apply_row_edge_mutations(
                eng,
                context,
                table_oid,
                rows.new.as_ref(),
                engine::MutationKind::Insert,
            )
        }
        SyncRowOperation::Update {
            old_pk,
            new_pk,
            old_tenant,
            new_tenant,
        } => {
            apply_row_edge_mutations(
                eng,
                context,
                table_oid,
                rows.old.as_ref(),
                engine::MutationKind::Delete,
            )?;
            if old_pk == new_pk {
                sync::sync_update_tenant(eng, table_oid, new_pk, old_tenant, new_tenant)?;
            } else {
                sync::sync_delete_tenant(eng, table_oid, old_pk, old_tenant)?;
                sync::sync_insert(eng, table_oid, new_pk, new_tenant)?;
            }
            refresh_filter_index_from_sync(
                eng,
                table_oid,
                new_pk,
                &context.filters,
                &context.table_oids,
                entry,
                rows,
            )?;
            apply_row_edge_mutations(
                eng,
                context,
                table_oid,
                rows.new.as_ref(),
                engine::MutationKind::Insert,
            )
        }
        SyncRowOperation::Delete { pk, old_tenant } => {
            apply_row_edge_mutations(
                eng,
                context,
                table_oid,
                rows.old.as_ref(),
                engine::MutationKind::Delete,
            )?;
            sync::sync_delete_tenant(eng, table_oid, pk, old_tenant)
        }
        SyncRowOperation::Truncate => sync::sync_truncate(eng, table_oid).map(|_| ()),
    }
}

fn projection_rows_from_sync_entries(
    entries: &[SyncLogEntry],
    context: &mut SyncReplayContext,
) -> safety::GraphResult<Vec<ProjectionSyncRow>> {
    ENGINE.with(|e| {
        projection_rows_from_sync_entries_with_engine(&mut e.borrow_mut(), entries, context)
    })
}

fn projection_rows_from_sync_entries_with_engine(
    eng: &mut engine::Engine,
    entries: &[SyncLogEntry],
    context: &mut SyncReplayContext,
) -> safety::GraphResult<Vec<ProjectionSyncRow>> {
    let mut rows = Vec::new();
    for entry in entries {
        let table_oid = match entry.table_oid {
            Some(oid) => oid,
            None => context.table_oid_or_lookup(&entry.table_name)?,
        };
        let parsed = parse_sync_properties(entry.properties.as_deref());
        let properties = parsed.iter().cloned().collect::<HashMap<_, _>>();
        let row_images = ParsedSyncRows::from_entry(entry)?;
        let tenant_change = tenant_change_from_entry(table_oid, &row_images, &parsed, context)?;
        append_projection_rows_for_entry(
            eng,
            context,
            table_oid,
            entry,
            &row_images,
            &properties,
            &tenant_change,
            &mut rows,
        )?;
        append_projection_filter_rows(
            eng,
            context,
            table_oid,
            entry,
            &row_images,
            &properties,
            &mut rows,
        )?;
    }
    Ok(rows)
}

#[allow(
    clippy::too_many_arguments,
    reason = "sync replay projection rows require entry, parsed row images, tenant state, and output sink"
)]
fn append_projection_rows_for_entry(
    eng: &engine::Engine,
    context: &SyncReplayContext,
    table_oid: u32,
    entry: &SyncLogEntry,
    rows: &ParsedSyncRows,
    properties: &HashMap<String, String>,
    tenant_change: &TenantChange,
    out: &mut Vec<ProjectionSyncRow>,
) -> safety::GraphResult<()> {
    match entry.op {
        SyncOp::Insert => {
            let pk = entry
                .new_pk
                .as_deref()
                .or(entry.old_pk.as_deref())
                .ok_or_else(|| {
                    safety::GraphError::Internal(format!("sync row {} missing insert pk", entry.id))
                })?;
            append_projection_node_row(
                eng,
                table_oid,
                entry,
                pk,
                MutationOperation::UpsertNode,
                tenant_change.new.as_deref(),
                out,
            );
            append_projection_edge_rows(
                eng,
                context,
                table_oid,
                entry,
                rows.new.as_ref(),
                MutationOperation::InsertEdge,
                out,
            )?;
        }
        SyncOp::Update => {
            let old_pk = entry.old_pk.as_deref().ok_or_else(|| {
                safety::GraphError::Internal(format!("sync row {} missing old_pk", entry.id))
            })?;
            let new_pk = entry.new_pk.as_deref().ok_or_else(|| {
                safety::GraphError::Internal(format!("sync row {} missing new_pk", entry.id))
            })?;
            append_projection_edge_rows(
                eng,
                context,
                table_oid,
                entry,
                rows.old.as_ref(),
                MutationOperation::DeleteEdge,
                out,
            )?;
            if old_pk != new_pk || tenant_change.old != tenant_change.new {
                append_projection_node_row(
                    eng,
                    table_oid,
                    entry,
                    old_pk,
                    MutationOperation::DeleteNode,
                    tenant_change.old.as_deref(),
                    out,
                );
            }
            append_projection_node_row(
                eng,
                table_oid,
                entry,
                new_pk,
                MutationOperation::UpsertNode,
                tenant_change.new.as_deref().or_else(|| {
                    properties
                        .get(&tenant_column_for_table(table_oid, context).unwrap_or_default())
                        .map(String::as_str)
                }),
                out,
            );
            append_projection_edge_rows(
                eng,
                context,
                table_oid,
                entry,
                rows.new.as_ref(),
                MutationOperation::InsertEdge,
                out,
            )?;
        }
        SyncOp::Delete => {
            let pk = entry
                .old_pk
                .as_deref()
                .or(entry.new_pk.as_deref())
                .ok_or_else(|| {
                    safety::GraphError::Internal(format!("sync row {} missing delete pk", entry.id))
                })?;
            append_projection_edge_rows(
                eng,
                context,
                table_oid,
                entry,
                rows.old.as_ref(),
                MutationOperation::DeleteEdge,
                out,
            )?;
            append_projection_node_row(
                eng,
                table_oid,
                entry,
                pk,
                MutationOperation::DeleteNode,
                tenant_change.old.as_deref(),
                out,
            );
        }
        SyncOp::Truncate => {}
    }
    Ok(())
}

fn append_projection_node_row(
    eng: &engine::Engine,
    table_oid: u32,
    entry: &SyncLogEntry,
    pk: &str,
    operation: MutationOperation,
    tenant: Option<&str>,
    out: &mut Vec<ProjectionSyncRow>,
) {
    let Some(node_idx) = eng.resolve_existing_node(table_oid, pk) else {
        return;
    };
    out.push(ProjectionSyncRow {
        sync_id: entry.id as u64,
        generation_id: entry.id as u64,
        committed: true,
        operation,
        direction: TraversalDirection::Any,
        source: node_idx,
        target: node_idx,
        type_id: 0,
        schema_reversed: false,
        weight: None,
        table_oid: Some(table_oid),
        pk_hash: Some(ResolutionIndexBuilder::hash_pk(pk)),
        node_idx: Some(node_idx),
        filter_column_id: None,
        filter_value: None,
        tenant_hash: tenant.map(|tenant| xxh3_64(tenant.as_bytes())),
    });
}

fn append_projection_edge_rows(
    eng: &engine::Engine,
    context: &SyncReplayContext,
    table_oid: u32,
    entry: &SyncLogEntry,
    row: Option<&serde_json::Value>,
    operation: MutationOperation,
    out: &mut Vec<ProjectionSyncRow>,
) -> safety::GraphResult<()> {
    let Some(row) = row else {
        return Ok(());
    };
    for edge in &context.edges {
        if context.table_oid(&edge.from_table) != Some(table_oid) {
            continue;
        }
        let Some(from_table) = context
            .tables
            .iter()
            .find(|table| table.table_name == edge.from_table)
        else {
            continue;
        };
        let Some(from_pk) = row_pk_value(row, &from_table.id_columns) else {
            continue;
        };
        let Some(to_pk) = row_text_value(row, &edge.from_column) else {
            continue;
        };
        let edge_label = edge
            .label_column
            .as_deref()
            .and_then(|column| row_text_value(row, column))
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| edge.label.clone());
        let weight = edge
            .weight_column
            .as_deref()
            .and_then(|column| row_u32_value(row, column))
            .transpose()?;
        let Some(type_id) = eng.edge_type_id(&edge_label) else {
            continue;
        };
        let source = resolve_projection_endpoint(
            eng,
            context.table_oid(&edge.from_table),
            &from_pk,
            &context.all_table_oids,
        );
        let target = resolve_projection_endpoint(
            eng,
            context.table_oid(&edge.to_table),
            &to_pk,
            &context.all_table_oids,
        );
        if let (Some(source), Some(target)) = (source, target) {
            push_projection_edge_row(
                ProjectionEdgeRow {
                    entry,
                    source,
                    target,
                    type_id,
                    schema_reversed: false,
                    weight,
                    operation,
                },
                out,
            );
            if edge.bidirectional {
                push_projection_edge_row(
                    ProjectionEdgeRow {
                        entry,
                        source: target,
                        target: source,
                        type_id,
                        schema_reversed: true,
                        weight,
                        operation,
                    },
                    out,
                );
            }
        }
    }
    Ok(())
}

struct ProjectionEdgeRow<'a> {
    entry: &'a SyncLogEntry,
    source: u32,
    target: u32,
    type_id: u8,
    schema_reversed: bool,
    weight: Option<u32>,
    operation: MutationOperation,
}

fn push_projection_edge_row(row: ProjectionEdgeRow<'_>, out: &mut Vec<ProjectionSyncRow>) {
    out.push(ProjectionSyncRow {
        sync_id: row.entry.id as u64,
        generation_id: row.entry.id as u64,
        committed: true,
        operation: row.operation,
        direction: TraversalDirection::Out,
        source: row.source,
        target: row.target,
        type_id: row.type_id,
        schema_reversed: row.schema_reversed,
        weight: row.weight,
        table_oid: None,
        pk_hash: None,
        node_idx: None,
        filter_column_id: None,
        filter_value: None,
        tenant_hash: None,
    });
}

fn append_projection_filter_rows(
    eng: &mut engine::Engine,
    context: &SyncReplayContext,
    table_oid: u32,
    entry: &SyncLogEntry,
    rows: &ParsedSyncRows,
    properties: &HashMap<String, String>,
    out: &mut Vec<ProjectionSyncRow>,
) -> safety::GraphResult<()> {
    match entry.op {
        SyncOp::Insert => {
            let Some(pk) = entry.new_pk.as_deref().or(entry.old_pk.as_deref()) else {
                return Ok(());
            };
            append_projection_filter_rows_for_pk(
                eng,
                context,
                table_oid,
                entry,
                pk,
                rows.new.as_ref(),
                properties,
                MutationOperation::UpsertNode,
                out,
            )?;
        }
        SyncOp::Update => {
            if let Some(old_pk) = entry.old_pk.as_deref() {
                append_projection_filter_rows_for_pk(
                    eng,
                    context,
                    table_oid,
                    entry,
                    old_pk,
                    rows.old.as_ref(),
                    properties,
                    MutationOperation::DeleteNode,
                    out,
                )?;
            }
            if let Some(new_pk) = entry.new_pk.as_deref() {
                append_projection_filter_rows_for_pk(
                    eng,
                    context,
                    table_oid,
                    entry,
                    new_pk,
                    rows.new.as_ref(),
                    properties,
                    MutationOperation::UpsertNode,
                    out,
                )?;
            }
        }
        SyncOp::Delete => {
            let Some(pk) = entry.old_pk.as_deref().or(entry.new_pk.as_deref()) else {
                return Ok(());
            };
            append_projection_filter_rows_for_pk(
                eng,
                context,
                table_oid,
                entry,
                pk,
                rows.old.as_ref(),
                properties,
                MutationOperation::DeleteNode,
                out,
            )?;
        }
        SyncOp::Truncate => {}
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "filter projection needs source row state, operation, and output sink at one replay boundary"
)]
fn append_projection_filter_rows_for_pk(
    eng: &mut engine::Engine,
    context: &SyncReplayContext,
    table_oid: u32,
    entry: &SyncLogEntry,
    pk: &str,
    row: Option<&serde_json::Value>,
    properties: &HashMap<String, String>,
    operation: MutationOperation,
    out: &mut Vec<ProjectionSyncRow>,
) -> safety::GraphResult<()> {
    let Some(node_idx) = eng.resolve_existing_node(table_oid, pk) else {
        return Ok(());
    };
    for filter in &context.filters {
        if context.table_oid(&filter.table_name) != Some(table_oid) {
            continue;
        }
        let Some(column_idx) = eng.filter_index.find_column(&filter.column_name) else {
            continue;
        };
        let encoded = filter_value_from_row_or_properties(
            &filter.column_name,
            eng.filter_index.column_type(column_idx),
            row,
            properties,
            &mut eng.filter_index,
            column_idx,
        )?;
        let Some(value) = encoded.and_then(segment_filter_value) else {
            continue;
        };
        out.push(ProjectionSyncRow {
            sync_id: entry.id as u64,
            generation_id: entry.id as u64,
            committed: true,
            operation,
            direction: TraversalDirection::Any,
            source: node_idx,
            target: node_idx,
            type_id: 0,
            schema_reversed: false,
            weight: None,
            table_oid: None,
            pk_hash: None,
            node_idx: Some(node_idx),
            filter_column_id: Some(column_idx as u32),
            filter_value: Some(value),
            tenant_hash: None,
        });
    }
    Ok(())
}

fn segment_filter_value(value: EncodedFilterValue) -> Option<u32> {
    match value {
        EncodedFilterValue::Numeric(value)
        | EncodedFilterValue::Date(value)
        | EncodedFilterValue::Timestamptz(value) => Some(value.clamp(0, u32::MAX as i64) as u32),
        EncodedFilterValue::Boolean(value) => Some(u32::from(value)),
        EncodedFilterValue::Text(value) => Some(value),
        EncodedFilterValue::Uuid(_) => None,
    }
}

fn resolve_projection_endpoint(
    eng: &engine::Engine,
    preferred_oid: Option<u32>,
    pk: &str,
    all_oids: &[u32],
) -> Option<u32> {
    if let Some(oid) = preferred_oid {
        if let Some(idx) = eng.resolve_existing_node(oid, pk) {
            return Some(idx);
        }
    }
    all_oids
        .iter()
        .find_map(|&oid| eng.resolve_existing_node(oid, pk))
}

fn sync_entry_edge_mutation_reservation(
    entry: &SyncLogEntry,
    table_oid: u32,
    context: &SyncReplayContext,
    rows: &ParsedSyncRows,
) -> safety::GraphResult<usize> {
    match entry.op {
        SyncOp::Insert => potential_row_edge_mutation_count(context, table_oid, rows.new.as_ref()),
        SyncOp::Update => {
            Ok(
                potential_row_edge_mutation_count(context, table_oid, rows.old.as_ref())?
                    + potential_row_edge_mutation_count(context, table_oid, rows.new.as_ref())?,
            )
        }
        SyncOp::Delete => potential_row_edge_mutation_count(context, table_oid, rows.old.as_ref()),
        SyncOp::Truncate => Ok(0),
    }
}

fn potential_row_edge_mutation_count(
    context: &SyncReplayContext,
    table_oid: u32,
    row: Option<&serde_json::Value>,
) -> safety::GraphResult<usize> {
    let Some(row) = row else {
        return Ok(0);
    };
    let mut count = 0usize;
    for edge in &context.edges {
        let from_oid = context.table_oid(&edge.from_table);
        if from_oid != Some(table_oid) {
            continue;
        }
        let Some(from_table) = context
            .tables
            .iter()
            .find(|table| table.table_name == edge.from_table)
        else {
            continue;
        };
        if row_pk_value(row, &from_table.id_columns).is_none()
            || row_text_value(row, &edge.from_column).is_none()
        {
            continue;
        }
        count = count.saturating_add(if edge.bidirectional { 2 } else { 1 });
    }
    Ok(count)
}

fn refresh_filter_index_from_sync(
    eng: &mut engine::Engine,
    table_oid: u32,
    pk: &str,
    filters: &[builder::RegisteredFilterColumn],
    table_oids: &HashMap<String, u32>,
    entry: &SyncLogEntry,
    rows: &ParsedSyncRows,
) -> safety::GraphResult<()> {
    let Some(node_idx) = eng.resolve(table_oid, pk) else {
        return Ok(());
    };
    let properties = parse_sync_properties(entry.properties.as_deref())
        .into_iter()
        .collect::<HashMap<_, _>>();

    for filter in filters {
        if table_oids.get(&filter.table_name).copied() != Some(table_oid) {
            continue;
        }
        let Some(column_idx) = eng.filter_index.find_column(&filter.column_name) else {
            continue;
        };
        let value = filter_value_from_row_or_properties(
            &filter.column_name,
            eng.filter_index.column_type(column_idx),
            rows.new.as_ref(),
            &properties,
            &mut eng.filter_index,
            column_idx,
        )?;
        eng.filter_index
            .set_encoded_value(column_idx, node_idx, value);
    }

    Ok(())
}

fn filter_value_from_row_or_properties(
    column_name: &str,
    column_type: Option<FilterColumnType>,
    row: Option<&serde_json::Value>,
    properties: &HashMap<String, String>,
    filter_index: &mut crate::filter_index::FilterIndex,
    column_idx: usize,
) -> safety::GraphResult<Option<EncodedFilterValue>> {
    let raw = row
        .and_then(|row| row.get(column_name))
        .cloned()
        .or_else(|| {
            properties
                .get(column_name)
                .map(|value| serde_json::Value::String(value.clone()))
        });
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(column_type) = column_type else {
        return Ok(None);
    };
    match column_type {
        FilterColumnType::Numeric => Ok(Some(EncodedFilterValue::Numeric(json_value_i64(&raw)?))),
        FilterColumnType::Boolean => Ok(Some(EncodedFilterValue::Boolean(json_value_bool(&raw)?))),
        FilterColumnType::Text => {
            let value = json_value_text(&raw)?;
            let token = filter_index.intern_text_value(column_idx, &value);
            Ok(Some(EncodedFilterValue::Text(token)))
        }
        FilterColumnType::Date => Ok(Some(EncodedFilterValue::Date(encode_date_filter_value(
            &string_filter_value(&raw)?,
        )?))),
        FilterColumnType::Timestamptz => Ok(Some(EncodedFilterValue::Timestamptz(
            encode_timestamptz_filter_value(&string_filter_value(&raw)?)?,
        ))),
        FilterColumnType::Uuid => {
            let value = json_value_text(&raw)?;
            Ok(Some(EncodedFilterValue::Uuid(parse_uuid_u128(&value)?)))
        }
    }
}

fn string_filter_value(raw: &serde_json::Value) -> safety::GraphResult<serde_json::Value> {
    Ok(serde_json::Value::String(json_value_text(raw)?))
}

fn json_value_text(raw: &serde_json::Value) -> safety::GraphResult<String> {
    match raw {
        serde_json::Value::String(value) => Ok(value.clone()),
        other => Ok(other.to_string()),
    }
}

fn json_value_i64(raw: &serde_json::Value) -> safety::GraphResult<i64> {
    if let Some(value) = raw.as_i64() {
        return Ok(value);
    }
    let text = json_value_text(raw)?;
    text.parse::<i64>()
        .map_err(|_| safety::GraphError::InvalidFilter {
            reason: "numeric sync filter values must be signed 64-bit integers".to_string(),
        })
}

fn json_value_bool(raw: &serde_json::Value) -> safety::GraphResult<bool> {
    if let Some(value) = raw.as_bool() {
        return Ok(value);
    }
    let text = json_value_text(raw)?;
    text.parse::<bool>()
        .map_err(|_| safety::GraphError::InvalidFilter {
            reason: "boolean sync filter values must be true or false".to_string(),
        })
}

pub(crate) fn apply_row_edge_mutations(
    eng: &mut engine::Engine,
    context: &SyncReplayContext,
    table_oid: u32,
    row: Option<&serde_json::Value>,
    kind: engine::MutationKind,
) -> safety::GraphResult<()> {
    let Some(row) = row else {
        return Ok(());
    };
    for edge in &context.edges {
        let from_oid = context.table_oid(&edge.from_table);
        if from_oid != Some(table_oid) {
            continue;
        }
        let Some(from_table) = context
            .tables
            .iter()
            .find(|table| table.table_name == edge.from_table)
        else {
            continue;
        };
        let Some(from_pk) = row_pk_value(row, &from_table.id_columns) else {
            continue;
        };
        let Some(to_pk) = row_text_value(row, &edge.from_column) else {
            continue;
        };
        let edge_label = edge
            .label_column
            .as_deref()
            .and_then(|column| row_text_value(row, column))
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| edge.label.clone());
        let type_id = eng.register_edge_type(&edge_label)?;
        let source = resolve_sync_endpoint(eng, from_oid, &from_pk, &context.all_table_oids);
        let target_oid = context.table_oid(&edge.to_table);
        let target = resolve_sync_endpoint(eng, target_oid, &to_pk, &context.all_table_oids);
        if let (Some(source), Some(target)) = (source, target) {
            push_sync_edge_delta(eng, source, target, type_id, kind)?;
            if edge.bidirectional {
                push_sync_edge_delta(eng, target, source, type_id, kind)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn push_sync_edge_delta(
    eng: &mut engine::Engine,
    source: u32,
    target: u32,
    type_id: u8,
    kind: engine::MutationKind,
) -> safety::GraphResult<()> {
    eng.push_edge_mutation(engine::EdgeMutation {
        source,
        target,
        type_id,
        kind,
    })
}

pub(crate) fn resolve_sync_endpoint(
    eng: &engine::Engine,
    preferred_oid: Option<u32>,
    pk: &str,
    all_oids: &[u32],
) -> Option<u32> {
    if let Some(oid) = preferred_oid {
        if let Some(idx) = eng.resolve(oid, pk) {
            return Some(idx);
        }
    }
    all_oids.iter().find_map(|&oid| eng.resolve(oid, pk))
}

pub(crate) fn row_pk_value(
    row: &serde_json::Value,
    primary_key: &builder::PrimaryKeySpec,
) -> Option<String> {
    if primary_key.columns().len() > 1 {
        let values = primary_key
            .columns()
            .iter()
            .map(|column| row_text_value(row, column))
            .collect::<Option<Vec<_>>>()?;
        Some(
            serde_json::Value::Array(values.into_iter().map(serde_json::Value::String).collect())
                .to_string(),
        )
    } else if let Some(column) = primary_key.columns().first() {
        row_text_value(row, column)
    } else {
        None
    }
}

pub(crate) fn row_text_value(row: &serde_json::Value, column: &str) -> Option<String> {
    let value = row.get(column)?;
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(text) => Some(text.clone()),
        other => Some(other.to_string().trim_matches('"').to_string()),
    }
}

fn row_u32_value(row: &serde_json::Value, column: &str) -> Option<safety::GraphResult<u32>> {
    row_text_value(row, column).map(|value| {
        value
            .parse::<u32>()
            .map_err(|_| safety::GraphError::InvalidFilter {
                reason: format!("{column} sync weight values must be unsigned 32-bit integers"),
            })
    })
}

pub(crate) fn apply_legacy_sync_buffer(stats: &mut SyncApplyStats) -> safety::GraphResult<()> {
    let batch_size = config::sync_batch_size();
    let mut context = SyncReplayContext::load()?;
    let max_legacy_id = max_legacy_sync_id()?;
    let mut after_id = 0;

    loop {
        let entries = read_legacy_sync_entries_after(after_id, max_legacy_id, batch_size)?;
        if entries.is_empty() {
            break;
        }
        after_id = entries.last().map(|entry| entry.id).unwrap_or(after_id);

        let mut applied_ids = Vec::new();
        for legacy in entries {
            let legacy_id = legacy.id;
            let result = (|| {
                let table_oid = context.table_oid_or_lookup(&legacy.table_name)?;
                let entry = SyncLogEntry {
                    id: legacy.id,
                    op: legacy.op,
                    table_oid: Some(table_oid),
                    table_name: legacy.table_name,
                    old_pk: Some(legacy.old_pk),
                    new_pk: Some(legacy.new_pk),
                    properties: legacy.properties,
                    old_row: None,
                    new_row: None,
                };
                apply_sync_log_entry_with_context(&entry, stats, &mut context)?;
                applied_ids.push(entry.id);
                Ok::<_, safety::GraphError>(())
            })();
            match result {
                Ok(()) => {}
                Err(err) => {
                    pgrx::warning!(
                        "graph.apply_sync(): legacy sync row {} failed and remains buffered: {}",
                        legacy_id,
                        err
                    );
                }
            }
        }

        delete_legacy_sync_entries(&applied_ids)?;
    }

    Ok(())
}

fn max_legacy_sync_id() -> safety::GraphResult<i64> {
    Spi::get_one::<i64>("SELECT COALESCE(max(id), 0) FROM graph._sync_buffer")
        .map_err(|e| {
            safety::GraphError::Internal(format!("legacy sync high-water read failed: {e}"))
        })
        .map(|max_id| max_id.unwrap_or(0))
}

fn read_legacy_sync_entries_after(
    after_id: i64,
    max_id: i64,
    limit: usize,
) -> safety::GraphResult<Vec<LegacySyncEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT id, op::text, table_name,
                    COALESCE(old_pk, pk) AS old_pk,
                    COALESCE(new_pk, pk) AS new_pk,
                    properties::text
             FROM graph._sync_buffer
             WHERE id > $1
               AND id <= $2
             ORDER BY id
             LIMIT $3",
                None,
                &[after_id.into(), max_id.into(), limit.into()],
            )
            .map_err(|e| {
                safety::GraphError::Internal(format!("legacy sync buffer read failed: {e}"))
            })?;
        let mut entries = Vec::new();
        for row in rows {
            let id = required_sync_i64(
                row.get::<i64>(1).map_err(|e| {
                    safety::GraphError::Internal(format!("legacy sync id read failed: {e}"))
                })?,
                "id",
            )?;
            let raw_op = required_sync_string(
                row.get::<String>(2).map_err(|e| {
                    safety::GraphError::Internal(format!("legacy sync op read failed: {e}"))
                })?,
                "op",
            )?;
            entries.push(LegacySyncEntry {
                id,
                op: parse_sync_op(&raw_op).map_err(|err| {
                    safety::GraphError::Internal(format!("legacy sync row {id}: {err}"))
                })?,
                table_name: required_sync_string(
                    row.get::<String>(3).map_err(|e| {
                        safety::GraphError::Internal(format!(
                            "legacy sync table_name read failed: {e}"
                        ))
                    })?,
                    "table_name",
                )?,
                old_pk: required_sync_string(
                    row.get::<String>(4).map_err(|e| {
                        safety::GraphError::Internal(format!("legacy sync old_pk read failed: {e}"))
                    })?,
                    "old_pk",
                )?,
                new_pk: required_sync_string(
                    row.get::<String>(5).map_err(|e| {
                        safety::GraphError::Internal(format!("legacy sync new_pk read failed: {e}"))
                    })?,
                    "new_pk",
                )?,
                properties: row.get::<String>(6).map_err(|e| {
                    safety::GraphError::Internal(format!("legacy sync properties read failed: {e}"))
                })?,
            });
        }
        Ok::<_, safety::GraphError>(entries)
    })
}

fn delete_legacy_sync_entries(applied_ids: &[i64]) -> safety::GraphResult<()> {
    if applied_ids.is_empty() {
        return Ok(());
    }
    Spi::run_with_args(
        "DELETE FROM graph._sync_buffer WHERE id = ANY($1)",
        &[applied_ids.to_vec().into()],
    )
    .map_err(|e| safety::GraphError::Internal(format!("legacy sync buffer cleanup failed: {}", e)))
}

#[derive(Debug, Default)]
struct TenantChange {
    old: Option<String>,
    new: Option<String>,
}

fn tenant_change_from_entry(
    table_oid: u32,
    rows: &ParsedSyncRows,
    properties: &[(String, String)],
    context: &SyncReplayContext,
) -> safety::GraphResult<TenantChange> {
    let Some(tenant_column) = tenant_column_for_table(table_oid, context) else {
        return Ok(TenantChange::default());
    };
    let old = rows
        .old
        .as_ref()
        .and_then(|row| tenant_from_row(row, &tenant_column));
    let new = rows
        .new
        .as_ref()
        .and_then(|row| tenant_from_row(row, &tenant_column))
        .or_else(|| {
            properties
                .iter()
                .find(|(column, _)| column == &tenant_column)
                .map(|(_, value)| value.clone())
        });
    Ok(TenantChange { old, new })
}

fn tenant_column_for_table(table_oid: u32, context: &SyncReplayContext) -> Option<String> {
    context
        .tables
        .iter()
        .find(|table| context.table_oid(&table.table_name) == Some(table_oid))
        .and_then(|table| table.tenant_column.clone())
}

fn tenant_from_row(row: &serde_json::Value, tenant_column: &str) -> Option<String> {
    row_text_value(row, tenant_column)
}

pub(crate) fn resolve_tenant_scope(
    explicit_tenant: Option<&str>,
) -> safety::GraphResult<Option<String>> {
    let graph_tenant = selected_or_default_graph_metadata()
        .ok()
        .and_then(|graph| graph.tenant)
        .map(|tenant| tenant.trim().to_string())
        .filter(|tenant| !tenant.is_empty());
    if let Some(tenant) = explicit_tenant
        .map(str::trim)
        .filter(|tenant| !tenant.is_empty())
    {
        ensure_tenant_matches_graph_scope(tenant, graph_tenant.as_deref())?;
        return Ok(Some(tenant.to_string()));
    }

    let tenant_setting = config::tenant_setting();
    if !tenant_setting.trim().is_empty() {
        let session_tenant = Spi::connect(|client| {
            let result = client.select(
                "SELECT current_setting($1, true)",
                None,
                &[tenant_setting.into()],
            )?;
            Ok::<_, pgrx::spi::SpiError>(result.first().get::<String>(1)?.unwrap_or_default())
        })
        .map_err(|e| {
            safety::GraphError::Internal(format!("tenant session setting read failed: {}", e))
        })?;
        if !session_tenant.trim().is_empty() {
            ensure_tenant_matches_graph_scope(session_tenant.trim(), graph_tenant.as_deref())?;
            return Ok(Some(session_tenant));
        }
    }

    if let Some(graph_tenant) = graph_tenant {
        return Ok(Some(graph_tenant));
    }

    if config::ENFORCE_TENANT_SCOPE.get() && graph_has_tenanted_tables()? {
        return Err(safety::GraphError::InvalidFilter {
            reason: "tenant scope is required for registered tables with tenant_column; pass tenant or configure graph.tenant_setting".to_string(),
        });
    }

    Ok(None)
}

fn ensure_tenant_matches_graph_scope(
    candidate: &str,
    graph_tenant: Option<&str>,
) -> safety::GraphResult<()> {
    if let Some(graph_tenant) = graph_tenant {
        if candidate != graph_tenant {
            return Err(safety::GraphError::InvalidFilter {
                reason: format!(
                    "tenant scope '{}' conflicts with selected graph tenant '{}'",
                    candidate, graph_tenant
                ),
            });
        }
    }
    Ok(())
}

pub(crate) fn graph_has_tenanted_tables() -> safety::GraphResult<bool> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    Ok(tables.iter().any(|table| table.tenant_column.is_some()))
}

pub(crate) fn parse_sync_properties(raw: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str(raw) else {
        return Vec::new();
    };

    map.into_iter()
        .filter_map(|(key, value)| match value {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some((key, s)),
            other => Some((key, other.to_string())),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        parse_sync_op, parse_sync_properties, required_sync_i64, required_sync_string,
        tenant_change_from_entry, ParsedSyncRows, SyncLogEntry, SyncOp, SyncReplayContext,
    };
    use crate::builder::{PrimaryKeySpec, PropertyColumns, RegisteredTable};
    use crate::safety::GraphError;
    use proptest::prelude::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn parse_sync_op_accepts_supported_codes() {
        assert_eq!(parse_sync_op("I").unwrap(), SyncOp::Insert);
        assert_eq!(parse_sync_op("U").unwrap(), SyncOp::Update);
        assert_eq!(parse_sync_op("D").unwrap(), SyncOp::Delete);
        assert_eq!(parse_sync_op("T").unwrap(), SyncOp::Truncate);
        assert_eq!(parse_sync_op(" I ").unwrap(), SyncOp::Insert);
    }

    #[test]
    fn parse_sync_op_rejects_unknown_codes() {
        let err = parse_sync_op("X").unwrap_err();

        assert!(matches!(err, GraphError::Internal(_)));
        assert!(err.to_string().contains("unsupported operation 'X'"));
    }

    #[test]
    fn tenant_change_prefers_old_and_new_row_images() {
        let context = SyncReplayContext {
            tables: vec![RegisteredTable {
                table_name: "public.accounts".to_string(),
                id_columns: PrimaryKeySpec::from_columns(vec!["id".to_string()]),
                columns: PropertyColumns::from_columns(vec!["name".to_string()]),
                tenant_column: Some("tenant_id".to_string()),
            }],
            edges: Vec::new(),
            filters: Vec::new(),
            table_oids: HashMap::from([("public.accounts".to_string(), 42)]),
            all_table_oids: vec![42],
            edge_source_tables: HashSet::new(),
            edge_source_oids: HashSet::new(),
        };
        let entry = SyncLogEntry {
            id: 1,
            op: SyncOp::Update,
            table_oid: Some(42),
            table_name: "public.accounts".to_string(),
            old_pk: Some("a1".to_string()),
            new_pk: Some("a1".to_string()),
            properties: Some(r#"{"tenant_id":"tenant-from-properties"}"#.to_string()),
            old_row: Some(r#"{"id":"a1","tenant_id":"tenant-old"}"#.to_string()),
            new_row: Some(r#"{"id":"a1","tenant_id":"tenant-new"}"#.to_string()),
        };

        let rows = ParsedSyncRows::from_entry(&entry).unwrap();
        let change = tenant_change_from_entry(
            42,
            &rows,
            &parse_sync_properties(entry.properties.as_deref()),
            &context,
        )
        .unwrap();

        assert_eq!(change.old.as_deref(), Some("tenant-old"));
        assert_eq!(change.new.as_deref(), Some("tenant-new"));
    }

    #[test]
    fn parsed_sync_rows_reports_row_image_parse_errors() {
        let entry = SyncLogEntry {
            id: 99,
            op: SyncOp::Insert,
            table_oid: Some(42),
            table_name: "public.accounts".to_string(),
            old_pk: None,
            new_pk: Some("a1".to_string()),
            properties: None,
            old_row: None,
            new_row: Some("{broken".to_string()),
        };

        let err = ParsedSyncRows::from_entry(&entry).unwrap_err();

        assert!(matches!(err, GraphError::Internal(_)));
        assert!(err
            .to_string()
            .contains("sync row 99 new_row JSON parse failed"));
    }

    #[test]
    fn required_sync_i64_rejects_null_structural_values() {
        assert_eq!(required_sync_i64(Some(42), "id").unwrap(), 42);

        let err = required_sync_i64(None, "id").unwrap_err();

        assert!(matches!(err, GraphError::Internal(_)));
        assert!(err.to_string().contains("id"));
    }

    #[test]
    fn required_sync_string_preserves_empty_strings_but_rejects_null() {
        assert_eq!(
            required_sync_string(Some(String::new()), "op").unwrap(),
            String::new()
        );
        assert_eq!(
            required_sync_string(Some("users".to_string()), "table_name").unwrap(),
            "users"
        );

        let err = required_sync_string(None, "table_name").unwrap_err();

        assert!(matches!(err, GraphError::Internal(_)));
        assert!(err.to_string().contains("table_name"));
    }

    proptest! {
        /// Sync property decoding accepts arbitrary JSON text without panics and
        /// preserves only non-null object fields as stringified key/value pairs.
        #[test]
        fn sync_property_decoder_is_total_for_utf8(input in ".{0,512}") {
            let _ = parse_sync_properties(Some(&input));
        }
    }
}
