//! SQL sync-log replay, trigger management, and tenant-scope helpers.

use crate::catalog::{
    catalog_fingerprint, read_catalog, selected_or_default_graph_metadata,
    selected_or_default_graph_metadata_via_definer, table_oid_from_name,
};
use crate::filter_index::{EncodedFilterValue, FilterColumnType, PersistedFilterValue};
use crate::persistence::{
    graph_artifact_checksum_for_path, graph_artifact_version, graph_file_path, load_graph_file,
    load_graph_file_with_projection_candidate, projection_manifest_root, read_sync_checkpoint,
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
    let (tables, edges, filter_columns) = read_catalog()?;
    let mut trigger_specs =
        std::collections::BTreeMap::<u32, (builder::PrimaryKeySpec, Vec<String>)>::new();
    for table in &tables {
        let mut columns = table.columns.to_vec();
        for filter in filter_columns
            .iter()
            .filter(|filter| filter.table_oid == table.table_oid)
        {
            if !columns.iter().any(|column| column == &filter.column_name) {
                columns.push(filter.column_name.clone());
            }
        }
        if let Some(tenant_column) = &table.tenant_column {
            if !columns.iter().any(|column| column == tenant_column) {
                columns.push(tenant_column.clone());
            }
        }
        trigger_specs.insert(table.table_oid, (table.id_columns.clone(), columns));
    }
    for edge in &edges {
        let (primary_key, columns) = trigger_specs
            .entry(edge.from_table_oid)
            .or_insert_with(|| (edge.source_key_columns.clone(), Vec::new()));
        if primary_key.columns().is_empty() {
            *primary_key = edge.source_key_columns.clone();
        }
        for column in edge
            .source_key_columns
            .columns()
            .iter()
            .chain(std::iter::once(&edge.from_column))
            .chain(edge.weight_column.iter())
            .chain(edge.label_column.iter())
        {
            if !columns.iter().any(|existing| existing == column) {
                columns.push(column.clone());
            }
        }
    }
    let mut installed = 0usize;
    for (oid, (primary_key, trigger_columns)) in trigger_specs {
        let qt = sync::get_qualified_table(oid)?;
        let trigger_sql = sync::generate_trigger_sql(&qt, &primary_key, &trigger_columns);
        let sql = if sync_writer_barrier_trigger_current_for_oid(oid)? {
            trigger_sql
                .split_once("-- Attach triggers")
                .map_or(trigger_sql.as_str(), |(functions, _)| functions)
        } else {
            trigger_sql.as_str()
        };
        Spi::run(sql).map_err(|e| {
            safety::GraphError::Internal(format!(
                "trigger creation failed for relation OID {oid}: {e}"
            ))
        })?;
        installed += 1;
    }

    Ok(installed)
}

pub(crate) fn remove_sync_triggers() -> safety::GraphResult<usize> {
    let (tables, edges, _filter_columns) = read_catalog()?;
    let mut removed = 0usize;
    let table_oids = tables
        .iter()
        .map(|table| table.table_oid)
        .chain(edges.iter().map(|edge| edge.from_table_oid))
        .collect::<std::collections::BTreeSet<_>>();
    for oid in table_oids {
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
                "trigger removal failed for relation OID {oid}: {err}"
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
    Spi::get_one_with_args::<i64>(
        "SELECT graph._pending_sync_rows_for_current_role($1)",
        &[applied_sync_id.into()],
    )
    .map_err(|e| safety::GraphError::Internal(format!("sync status check failed: {}", e)))?
    .ok_or_else(|| safety::GraphError::Internal("pending sync row count was null".to_string()))
}

pub(crate) fn pending_sync_rows_direct(applied_sync_id: i64) -> safety::GraphResult<i64> {
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
    Spi::get_one::<i64>("SELECT graph._max_sync_log_id_for_current_role()")
        .map_err(|e| safety::GraphError::Internal(format!("sync checkpoint read failed: {}", e)))?
        .ok_or_else(|| safety::GraphError::Internal("max sync log id was null".to_string()))
}

pub(crate) fn max_sync_log_id_direct() -> safety::GraphResult<i64> {
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
    edge_source_node_oids: HashMap<u64, u32>,
}

impl SyncReplayContext {
    fn load() -> safety::GraphResult<Self> {
        let (tables, edges, filters) = read_catalog()?;
        let mut table_oids = HashMap::new();

        for table in &tables {
            table_oids.insert(table.table_name.clone(), table.table_oid);
        }
        for edge in &edges {
            table_oids.insert(edge.from_table.clone(), edge.from_table_oid);
            table_oids.insert(edge.to_table.clone(), edge.to_table_oid);
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
        let mut edge_source_node_oids = HashMap::with_capacity(edges.len());
        for edge in &edges {
            if let Some(source_oid) = sync_edge_source_node_oid(edge, &tables)? {
                edge_source_node_oids.insert(edge.mapping_id, source_oid);
            }
        }

        Ok(Self {
            tables,
            edges,
            filters,
            table_oids,
            all_table_oids,
            edge_source_tables,
            edge_source_oids,
            edge_source_node_oids,
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

fn sync_edge_source_node_oid(
    edge: &builder::RegisteredEdge,
    tables: &[builder::RegisteredTable],
) -> safety::GraphResult<Option<u32>> {
    if tables
        .iter()
        .any(|table| table.table_oid == edge.from_table_oid)
    {
        return Ok(Some(edge.from_table_oid));
    }
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT c.confrelid::oid::integer
               FROM pg_constraint c
               JOIN unnest(c.conkey) WITH ORDINALITY AS fk_from(attnum, n) ON true
               JOIN pg_attribute from_attr
                 ON from_attr.attrelid = c.conrelid
                AND from_attr.attnum = fk_from.attnum
              WHERE c.contype = 'f'
                AND c.conrelid = $1::oid
                AND from_attr.attname = $2
              ORDER BY c.oid
              LIMIT 2",
            None,
            &[
                pgrx::pg_sys::Oid::from_u32(edge.from_table_oid).into(),
                edge.from_column.clone().into(),
            ],
        )?;
        if rows.len() != 1 {
            return Ok(None);
        }
        rows.first()
            .get::<i32>(1)
            .map(|oid| oid.map(|oid| oid as u32))
    })
    .map_err(|err| {
        safety::GraphError::Internal(format!(
            "relationship source foreign-key lookup failed: {err}"
        ))
    })
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
    let graph = selected_or_default_graph_metadata()?;
    if ENGINE.with(|e| e.borrow().built)
        && crate::runtime_state::selected_graph_matches_loaded_slot(&graph.graph_id)
    {
        crate::runtime_state::touch_loaded_graph(&graph.graph_id);
        return Ok(());
    }

    let graph_path = graph_file_path()?;
    if !graph_path.exists() {
        return Err(safety::GraphError::NotBuilt);
    }

    let loaded = load_graph_file(&graph_path)?;
    install_loaded_engine_for_selected_graph(&graph, loaded)
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
    ensure_sync_writer_barrier_triggers()?;
    acquire_sync_writer_barrier()?;
    let graph_path = graph_file_path()?;
    let root = projection_manifest_root(&graph_path);
    let previous = ProjectionManifestStore::new(root).load_latest_current()?;
    let previous_watermark = previous.as_ref().map_or(
        read_sync_checkpoint(&graph_path)?.unwrap_or(0),
        |manifest| manifest.sync_watermark,
    );
    ensure_no_current_transaction_sync_rows(previous_watermark)?;
    let batch_size = config::sync_batch_size().max(1);
    let entries =
        read_sync_log_entries_after(previous_watermark, batch_size, Some(target_sync_id))?;
    let mut stats = sync_apply_stats_from_entries(&entries);

    let _projection =
        ingest_projection_until_internal(Some(batch_size as i64), None, Some(target_sync_id))?;

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

fn install_loaded_engine_for_selected_graph(
    graph: &crate::catalog::GraphMetadata,
    mut loaded: engine::Engine,
) -> safety::GraphResult<()> {
    if let Ok((tables, edges, filters)) = read_catalog() {
        loaded.set_catalog_fingerprint(catalog_fingerprint(&tables, &edges, &filters));
    }
    ENGINE.with(|engine| {
        *engine.borrow_mut() = loaded;
    });
    crate::runtime_state::mark_loaded_graph(graph);
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
    // Serialize the read-current -> allocate -> publish sequence across PostgreSQL
    // backends. Sharing the build/vacuum lock also prevents artifact replacement
    // while a projection generation is being prepared.
    crate::sql_build::acquire_build_lock()?;
    ensure_sync_writer_barrier_triggers()?;
    acquire_sync_writer_barrier()?;
    ensure_engine_loaded_for_apply_sync()?;
    let graph = selected_or_default_graph_metadata()?;
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
    let current_artifact_bytes = crate::projection::status::collect_projection_metadata_status(
        &root,
        max_sync_log_id()?,
        0,
        config::compaction_threshold(),
    )
    .map(|status| status.artifact_bytes)
    .unwrap_or(0);
    crate::catalog::enforce_artifact_storage_quota(
        current_artifact_bytes.saturating_add(byte_limit.min(i64::MAX as usize) as i64),
    )?;
    ensure_no_current_transaction_sync_rows(previous_watermark)?;
    let entries = read_sync_log_entries_after(previous_watermark, row_limit, target_sync_id)?;
    if entries.is_empty() {
        return Ok(ProjectionIngestStats {
            sync_watermark: previous_watermark,
            ..ProjectionIngestStats::default()
        });
    }
    if entries.iter().any(|entry| entry.op == SyncOp::Truncate) {
        return Err(safety::GraphError::UnsupportedOperation {
            operation: "durable projection ingestion".to_string(),
            reason: "TRUNCATE cannot be represented incrementally; rebuild the graph".to_string(),
        });
    }
    // Plan durable identities from the persisted base plus current manifest,
    // never from the serving engine. The latter can contain transaction-local
    // node slots that are intentionally absent from durable artifacts.
    let planning_engine = load_graph_file(&graph_path)?;
    let mut context = SyncReplayContext::load()?;
    let rows =
        projection_rows_from_sync_entries_with_engine(&planning_engine, &entries, &mut context)?;
    if rows.is_empty() {
        return Err(safety::GraphError::UnsupportedOperation {
            operation: "durable projection ingestion".to_string(),
            reason: format!(
                "{} committed sync row(s) cannot be represented incrementally; rebuild the graph",
                entries.len()
            ),
        });
    }
    let relationship_identities = planning_engine.relationship_identities.clone();
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
    let (result, validated_engine) = ingester.ingest_committed_rows_with_identities_validated(
        &rows,
        MutationBufferLimits::new(row_limit, byte_limit),
        &relationship_identities,
        |candidate| load_graph_file_with_projection_candidate(&graph_path, candidate),
    )?;
    let stats = projection_ingest_stats(result, previous_watermark);
    if stats.sync_watermark > previous_watermark {
        let mut validated_engine = validated_engine.ok_or_else(|| {
            safety::GraphError::Internal(
                "published projection candidate did not retain its validated engine".to_string(),
            )
        })?;
        validated_engine.record_applied_sync_id(stats.sync_watermark);
        install_loaded_engine_for_selected_graph(&graph, validated_engine)?;
    }
    Ok(stats)
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

#[cfg(feature = "pg_test")]
pub(crate) fn acquire_sync_writer_barrier() -> safety::GraphResult<()> {
    Ok(())
}

fn ensure_sync_writer_barrier_triggers() -> safety::GraphResult<()> {
    let missing = sync_writer_barrier_trigger_gap_count()?;
    if missing == 0 {
        return Ok(());
    }
    Err(safety::GraphError::UnsupportedOperation {
        operation: "durable projection ingestion".to_string(),
        reason: format!(
            "{missing} registered source table(s) do not have the current transaction writer barrier; run graph.build() with graph.sync_mode = 'trigger' or call graph.enable_sync(), then retry"
        ),
    })
}

pub(crate) fn sync_writer_barrier_triggers_current() -> safety::GraphResult<bool> {
    sync_writer_barrier_trigger_gap_count().map(|missing| missing == 0)
}

fn sync_writer_barrier_trigger_gap_count() -> safety::GraphResult<i64> {
    let applicable_table_oids = SyncReplayContext::load()?.applicable_table_oids();
    if applicable_table_oids.is_empty() {
        return Ok(0);
    }
    let expected_lock = format!(
        "pg_advisory_xact_lock_shared({}, {})",
        sync::SYNC_WRITER_LOCK_CLASS,
        sync::SYNC_WRITER_LOCK_KEY
    );
    let applicable_table_count = applicable_table_oids.len() as i64;
    let missing = Spi::get_one_with_args::<i64>(
        "SELECT count(*)::bigint
           FROM unnest($1::int4[]) AS expected(table_oid)
          WHERE NOT EXISTS (
                SELECT 1
                  FROM pg_trigger trigger
                  JOIN pg_proc function ON function.oid = trigger.tgfoid
                 WHERE trigger.tgrelid = expected.table_oid::oid
                   AND NOT trigger.tgisinternal
                   AND trigger.tgenabled <> 'D'
                   AND trigger.tgname IN (
                       'graph_sync_insert',
                       'graph_sync_update',
                       'graph_sync_delete',
                       'graph_sync_truncate'
                   )
                 GROUP BY trigger.tgrelid
                HAVING count(DISTINCT trigger.tgname) = 4
                   AND bool_and(position($2 IN pg_get_functiondef(function.oid)) > 0)
          )",
        &[applicable_table_oids.into(), expected_lock.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!(
            "sync writer barrier trigger verification failed: {err}"
        ))
    })?
    .unwrap_or(applicable_table_count);
    Ok(missing)
}

fn sync_writer_barrier_trigger_current_for_oid(table_oid: u32) -> safety::GraphResult<bool> {
    let expected_lock = format!(
        "pg_advisory_xact_lock_shared({}, {})",
        sync::SYNC_WRITER_LOCK_CLASS,
        sync::SYNC_WRITER_LOCK_KEY
    );
    Spi::get_one_with_args::<bool>(
        "SELECT COALESCE((
             SELECT count(DISTINCT trigger.tgname) = 4
                    AND bool_and(position($2 IN pg_get_functiondef(function.oid)) > 0)
               FROM pg_trigger trigger
               JOIN pg_proc function ON function.oid = trigger.tgfoid
              WHERE trigger.tgrelid = $1::oid
                AND NOT trigger.tgisinternal
                AND trigger.tgenabled <> 'D'
                AND trigger.tgname IN (
                    'graph_sync_insert',
                    'graph_sync_update',
                    'graph_sync_delete',
                    'graph_sync_truncate'
                )
         ), false)",
        &[(table_oid as i32).into(), expected_lock.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!(
            "sync writer barrier trigger verification failed for relation OID {table_oid}: {err}"
        ))
    })
    .map(|current| current.unwrap_or(false))
}

#[cfg(not(feature = "pg_test"))]
pub(crate) fn acquire_sync_writer_barrier() -> safety::GraphResult<()> {
    let acquired = Spi::get_one_with_args::<bool>(
        "SELECT pg_try_advisory_xact_lock($1, $2)",
        &[
            sync::SYNC_WRITER_LOCK_CLASS.into(),
            sync::SYNC_WRITER_LOCK_KEY.into(),
        ],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("sync writer barrier acquisition failed: {err}"))
    })?
    .unwrap_or(false);
    acquired
        .then_some(())
        .ok_or(safety::GraphError::BuildLocked)
}

#[cfg(feature = "pg_test")]
pub(crate) fn ensure_no_current_transaction_sync_rows(_after_id: i64) -> safety::GraphResult<()> {
    Ok(())
}

#[cfg(not(feature = "pg_test"))]
pub(crate) fn ensure_no_current_transaction_sync_rows(after_id: i64) -> safety::GraphResult<()> {
    let applicable_table_oids = SyncReplayContext::load()?.applicable_table_oids();
    let has_current_rows = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
             FROM graph._sync_log
             WHERE id > $1
               AND table_oid::oid::integer = ANY($2::int4[])
               AND xid = txid_current()
         )",
        &[after_id.into(), applicable_table_oids.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("current transaction sync check failed: {err}"))
    })?
    .unwrap_or(false);
    (!has_current_rows)
        .then_some(())
        .ok_or(safety::GraphError::BuildLocked)
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
            refresh_filter_index_from_sync(eng, table_oid, pk, &context.filters, entry, rows)?;
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
            refresh_filter_index_from_sync(eng, table_oid, new_pk, &context.filters, entry, rows)?;
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

fn projection_rows_from_sync_entries_with_engine(
    eng: &engine::Engine,
    entries: &[SyncLogEntry],
    context: &mut SyncReplayContext,
) -> safety::GraphResult<Vec<ProjectionSyncRow>> {
    let mut prepared = Vec::with_capacity(entries.len());
    for entry in entries {
        let table_oid = match entry.table_oid {
            Some(oid) => oid,
            None => context.table_oid_or_lookup(&entry.table_name)?,
        };
        let parsed = parse_sync_properties(entry.properties.as_deref());
        let properties = parsed.iter().cloned().collect::<HashMap<_, _>>();
        let row_images = ParsedSyncRows::from_entry(entry)?;
        let tenant_change = tenant_change_from_entry(table_oid, &row_images, &parsed, context)?;
        prepared.push(PreparedProjectionEntry {
            entry,
            table_oid,
            is_node_table: context
                .tables
                .iter()
                .any(|table| table.table_oid == table_oid),
            properties,
            row_images,
            tenant_change,
        });
    }
    guard_standalone_endpoint_lifecycle(context, &prepared)?;
    let nodes = ProjectionNodePlanner::plan(eng, &prepared)?;
    let mut rows = Vec::new();
    for prepared in &prepared {
        let row_start = rows.len();
        append_projection_rows_for_entry(&nodes, context, prepared, &mut rows)?;
        if prepared.is_node_table {
            append_projection_filter_rows(
                &nodes,
                context,
                prepared.table_oid,
                prepared.entry,
                &prepared.row_images,
                &prepared.properties,
                &mut rows,
            )?;
        }
        if rows.len() == row_start {
            return Err(safety::GraphError::UnsupportedOperation {
                operation: "durable projection ingestion".to_string(),
                reason: format!(
                    "sync row {} cannot be represented incrementally; rebuild the graph",
                    prepared.entry.id
                ),
            });
        }
    }
    for node_idx in eng.node_store.node_count()..nodes.next_node_idx {
        if !rows.iter().any(|row| {
            row.node_idx == Some(node_idx)
                && row.table_oid.is_some()
                && row.primary_key.is_some()
                && row.pk_hash.is_some()
                && !row.operation.is_edge()
        }) {
            let sync_id = nodes
                .upserts
                .iter()
                .find_map(|(sync_id, planned)| (*planned == node_idx).then_some(*sync_id));
            return Err(safety::GraphError::Internal(format!(
                "projection node planner allocated slot {node_idx} for sync row {sync_id:?} without an identity row"
            )));
        }
    }
    Ok(rows)
}

fn guard_standalone_endpoint_lifecycle(
    context: &SyncReplayContext,
    entries: &[PreparedProjectionEntry<'_>],
) -> safety::GraphResult<()> {
    let has_standalone_source = context.edges.iter().any(|edge| {
        !context
            .tables
            .iter()
            .any(|table| table.table_oid == edge.from_table_oid)
    });
    if !has_standalone_source {
        return Ok(());
    }
    for prepared in entries.iter().filter(|entry| entry.is_node_table) {
        let changes_identity = match prepared.entry.op {
            SyncOp::Delete => true,
            SyncOp::Update => prepared.entry.old_pk != prepared.entry.new_pk,
            SyncOp::Insert | SyncOp::Truncate => false,
        };
        if changes_identity {
            return Err(safety::GraphError::UnsupportedOperation {
                operation: "durable projection ingestion".to_string(),
                reason: format!(
                    "sync row {} changes a node identity while a standalone relationship mapping is registered; rebuild the graph",
                    prepared.entry.id
                ),
            });
        }
    }
    Ok(())
}

struct PreparedProjectionEntry<'a> {
    entry: &'a SyncLogEntry,
    table_oid: u32,
    is_node_table: bool,
    properties: HashMap<String, String>,
    row_images: ParsedSyncRows,
    tenant_change: TenantChange,
}

#[derive(Debug, Clone, Copy)]
struct PlannedNode {
    node_idx: u32,
    active: bool,
}

struct ProjectionNodePlanner<'a> {
    engine: &'a engine::Engine,
    timelines: HashMap<u32, HashMap<String, Vec<(i64, PlannedNode)>>>,
    upserts: HashMap<i64, u32>,
    deletes: HashMap<i64, u32>,
    next_node_idx: u32,
}

impl<'a> ProjectionNodePlanner<'a> {
    fn plan(
        engine: &'a engine::Engine,
        entries: &[PreparedProjectionEntry<'_>],
    ) -> safety::GraphResult<Self> {
        let mut planner = Self {
            engine,
            timelines: HashMap::new(),
            upserts: HashMap::new(),
            deletes: HashMap::new(),
            next_node_idx: engine.node_store.node_count(),
        };
        for prepared in entries {
            if !prepared.is_node_table {
                continue;
            }
            let entry = prepared.entry;
            match entry.op {
                SyncOp::Insert => {
                    if let Some(pk) = entry.new_pk.as_deref().or(entry.old_pk.as_deref()) {
                        planner.plan_upsert(prepared.table_oid, pk, entry.id)?;
                    }
                }
                SyncOp::Update => {
                    let old_pk = entry.old_pk.as_deref().ok_or_else(|| {
                        safety::GraphError::Internal(format!(
                            "sync row {} missing old_pk",
                            entry.id
                        ))
                    })?;
                    let new_pk = entry.new_pk.as_deref().ok_or_else(|| {
                        safety::GraphError::Internal(format!(
                            "sync row {} missing new_pk",
                            entry.id
                        ))
                    })?;
                    if old_pk != new_pk {
                        planner.plan_delete(prepared.table_oid, old_pk, entry.id);
                    }
                    planner.plan_upsert(prepared.table_oid, new_pk, entry.id)?;
                }
                SyncOp::Delete => {
                    if let Some(pk) = entry.old_pk.as_deref().or(entry.new_pk.as_deref()) {
                        planner.plan_delete(prepared.table_oid, pk, entry.id);
                    }
                }
                SyncOp::Truncate => {}
            }
        }
        Ok(planner)
    }

    fn latest_planned(&self, table_oid: u32, primary_key: &str) -> Option<PlannedNode> {
        self.timelines
            .get(&table_oid)
            .and_then(|table| table.get(primary_key))
            .and_then(|timeline| timeline.last())
            .map(|(_, node)| *node)
    }

    fn resolve_final(&self, table_oid: u32, primary_key: &str) -> Option<u32> {
        self.latest_planned(table_oid, primary_key).map_or_else(
            || self.engine.resolve(table_oid, primary_key),
            |node| node.active.then_some(node.node_idx),
        )
    }

    fn resolve_before(&self, table_oid: u32, primary_key: &str, sync_id: i64) -> Option<u32> {
        let planned = self
            .timelines
            .get(&table_oid)
            .and_then(|table| table.get(primary_key))
            .and_then(|timeline| timeline.iter().rev().find(|(id, _)| *id < sync_id))
            .map(|(_, node)| *node);
        planned.map_or_else(
            || self.engine.resolve(table_oid, primary_key),
            |node| node.active.then_some(node.node_idx),
        )
    }

    fn resolve_known_before(&self, table_oid: u32, primary_key: &str, sync_id: i64) -> Option<u32> {
        self.timelines
            .get(&table_oid)
            .and_then(|table| table.get(primary_key))
            .and_then(|timeline| timeline.iter().rev().find(|(id, _)| *id < sync_id))
            .map(|(_, node)| node.node_idx)
            .or_else(|| self.engine.resolve_identity(table_oid, primary_key))
    }

    fn resolve_endpoint_final(
        &self,
        preferred_oid: Option<u32>,
        primary_key: &str,
        all_oids: &[u32],
    ) -> Option<u32> {
        if let Some(oid) = preferred_oid {
            return self.resolve_final(oid, primary_key);
        }
        resolve_unique_endpoint(all_oids, |oid| self.resolve_final(oid, primary_key))
    }

    fn resolve_endpoint_known_before(
        &self,
        preferred_oid: Option<u32>,
        primary_key: &str,
        all_oids: &[u32],
        sync_id: i64,
    ) -> Option<u32> {
        if let Some(oid) = preferred_oid {
            return self.resolve_known_before(oid, primary_key, sync_id);
        }
        resolve_unique_endpoint(all_oids, |oid| {
            self.resolve_known_before(oid, primary_key, sync_id)
        })
    }

    fn plan_upsert(
        &mut self,
        table_oid: u32,
        primary_key: &str,
        sync_id: i64,
    ) -> safety::GraphResult<()> {
        let node_idx = match self.latest_planned(table_oid, primary_key) {
            Some(node) if node.active => node.node_idx,
            Some(_) => self.allocate_node_idx()?,
            None => match self.engine.resolve(table_oid, primary_key) {
                Some(node_idx) => node_idx,
                None => self.allocate_node_idx()?,
            },
        };
        self.timelines
            .entry(table_oid)
            .or_default()
            .entry(primary_key.to_string())
            .or_default()
            .push((
                sync_id,
                PlannedNode {
                    node_idx,
                    active: true,
                },
            ));
        self.upserts.insert(sync_id, node_idx);
        Ok(())
    }

    fn plan_delete(&mut self, table_oid: u32, primary_key: &str, sync_id: i64) {
        let node_idx = self.latest_planned(table_oid, primary_key).map_or_else(
            || self.engine.resolve(table_oid, primary_key),
            |node| node.active.then_some(node.node_idx),
        );
        let Some(node_idx) = node_idx else { return };
        self.timelines
            .entry(table_oid)
            .or_default()
            .entry(primary_key.to_string())
            .or_default()
            .push((
                sync_id,
                PlannedNode {
                    node_idx,
                    active: false,
                },
            ));
        self.deletes.insert(sync_id, node_idx);
    }

    fn allocate_node_idx(&mut self) -> safety::GraphResult<u32> {
        let node_idx = self.next_node_idx;
        self.next_node_idx = self.next_node_idx.checked_add(1).ok_or_else(|| {
            safety::GraphError::Internal("projection node index overflowed".to_string())
        })?;
        Ok(node_idx)
    }
}

fn resolve_unique_endpoint(
    table_oids: &[u32],
    mut resolve: impl FnMut(u32) -> Option<u32>,
) -> Option<u32> {
    let mut resolved = None;
    for &table_oid in table_oids {
        let Some(node_idx) = resolve(table_oid) else {
            continue;
        };
        if resolved.is_some_and(|existing| existing != node_idx) {
            return None;
        }
        resolved = Some(node_idx);
    }
    resolved
}

#[allow(
    clippy::too_many_arguments,
    reason = "sync replay projection rows require entry, parsed row images, tenant state, and output sink"
)]
fn append_projection_rows_for_entry(
    nodes: &ProjectionNodePlanner<'_>,
    context: &SyncReplayContext,
    prepared: &PreparedProjectionEntry<'_>,
    out: &mut Vec<ProjectionSyncRow>,
) -> safety::GraphResult<()> {
    let entry = prepared.entry;
    let table_oid = prepared.table_oid;
    let rows = &prepared.row_images;
    let properties = &prepared.properties;
    let tenant_change = &prepared.tenant_change;
    match entry.op {
        SyncOp::Insert => {
            let pk = entry
                .new_pk
                .as_deref()
                .or(entry.old_pk.as_deref())
                .ok_or_else(|| {
                    safety::GraphError::Internal(format!("sync row {} missing insert pk", entry.id))
                })?;
            if prepared.is_node_table {
                append_projection_node_row(
                    table_oid,
                    entry,
                    pk,
                    nodes.upserts.get(&entry.id).copied(),
                    MutationOperation::UpsertNode,
                    tenant_change.new.as_deref(),
                    out,
                )?;
            }
            append_projection_edge_rows(
                nodes,
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
                nodes,
                context,
                table_oid,
                entry,
                rows.old.as_ref(),
                MutationOperation::DeleteEdge,
                out,
            )?;
            if prepared.is_node_table && old_pk != new_pk {
                append_projection_node_row(
                    table_oid,
                    entry,
                    old_pk,
                    nodes.deletes.get(&entry.id).copied(),
                    MutationOperation::DeleteNode,
                    tenant_change.old.as_deref(),
                    out,
                )?;
            } else if prepared.is_node_table && tenant_change.old != tenant_change.new {
                append_projection_tenant_tombstone(
                    entry,
                    nodes.upserts.get(&entry.id).copied(),
                    tenant_change.old.as_deref(),
                    out,
                );
            }
            if prepared.is_node_table {
                append_projection_node_row(
                    table_oid,
                    entry,
                    new_pk,
                    nodes.upserts.get(&entry.id).copied(),
                    MutationOperation::UpsertNode,
                    tenant_change.new.as_deref().or_else(|| {
                        properties
                            .get(&tenant_column_for_table(table_oid, context).unwrap_or_default())
                            .map(String::as_str)
                    }),
                    out,
                )?;
            }
            append_projection_edge_rows(
                nodes,
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
                nodes,
                context,
                table_oid,
                entry,
                rows.old.as_ref(),
                MutationOperation::DeleteEdge,
                out,
            )?;
            if prepared.is_node_table {
                append_projection_node_row(
                    table_oid,
                    entry,
                    pk,
                    nodes.deletes.get(&entry.id).copied(),
                    MutationOperation::DeleteNode,
                    tenant_change.old.as_deref(),
                    out,
                )?;
            }
        }
        SyncOp::Truncate => {}
    }
    Ok(())
}

fn append_projection_node_row(
    table_oid: u32,
    entry: &SyncLogEntry,
    pk: &str,
    node_idx: Option<u32>,
    operation: MutationOperation,
    tenant: Option<&str>,
    out: &mut Vec<ProjectionSyncRow>,
) -> safety::GraphResult<()> {
    let Some(node_idx) = node_idx else {
        return Ok(());
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
        relationship_identity: None,
        table_oid: Some(table_oid),
        pk_hash: Some(ResolutionIndexBuilder::hash_pk(pk)),
        primary_key: Some(pk.to_string()),
        node_idx: Some(node_idx),
        filter_column_id: None,
        filter_value: None,
        tenant_hash: tenant.map(|tenant| xxh3_64(tenant.as_bytes())),
        tenant: tenant.map(str::to_string),
    });
    Ok(())
}

fn append_projection_tenant_tombstone(
    entry: &SyncLogEntry,
    node_idx: Option<u32>,
    tenant: Option<&str>,
    out: &mut Vec<ProjectionSyncRow>,
) {
    let (Some(node_idx), Some(tenant)) = (node_idx, tenant) else {
        return;
    };
    out.push(ProjectionSyncRow {
        sync_id: entry.id as u64,
        generation_id: entry.id as u64,
        committed: true,
        operation: MutationOperation::DeleteNode,
        direction: TraversalDirection::Any,
        source: node_idx,
        target: node_idx,
        type_id: 0,
        schema_reversed: false,
        weight: None,
        relationship_identity: None,
        table_oid: None,
        pk_hash: None,
        primary_key: None,
        node_idx: Some(node_idx),
        filter_column_id: None,
        filter_value: None,
        tenant_hash: Some(xxh3_64(tenant.as_bytes())),
        tenant: Some(tenant.to_string()),
    });
}

fn append_projection_edge_rows(
    nodes: &ProjectionNodePlanner<'_>,
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
        let Some((source_oid, from_pk, target_oid, to_pk)) =
            projection_edge_endpoints(context, edge, row)
        else {
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
        let type_id = nodes.engine.edge_type_id(&edge_label).ok_or_else(|| {
            safety::GraphError::UnsupportedOperation {
                operation: "durable projection ingestion".to_string(),
                reason: format!(
                    "sync row {} uses relationship label '{}' absent from the persisted graph; rebuild the graph",
                    entry.id, edge_label
                ),
            }
        })?;
        let source_key = row_pk_value(row, &edge.source_key_columns).ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "mapped relationship sync row {} is missing source key columns",
                entry.id
            ))
        })?;
        let relationship_identity = crate::edge_store::RelationshipIdentity {
            mapping_id: edge.mapping_id,
            source_key,
        };
        let resolve = |preferred_oid, primary_key: &str, source_endpoint: bool| match operation {
            MutationOperation::InsertEdge
                if source_endpoint && preferred_oid == Some(table_oid) =>
            {
                nodes.upserts.get(&entry.id).copied().or_else(|| {
                    nodes.resolve_endpoint_final(
                        preferred_oid,
                        primary_key,
                        &context.all_table_oids,
                    )
                })
            }
            MutationOperation::InsertEdge => {
                nodes.resolve_endpoint_final(preferred_oid, primary_key, &context.all_table_oids)
            }
            MutationOperation::DeleteEdge => nodes.resolve_endpoint_known_before(
                preferred_oid,
                primary_key,
                &context.all_table_oids,
                entry.id,
            ),
            MutationOperation::UpsertNode | MutationOperation::DeleteNode => None,
        };
        let source = resolve(source_oid, &from_pk, true);
        let target = resolve(Some(target_oid), &to_pk, false);
        let (source, target) = source.zip(target).ok_or_else(|| {
            safety::GraphError::UnsupportedOperation {
                operation: "durable projection ingestion".to_string(),
                reason: format!(
                    "sync row {} references a relationship endpoint absent from the persisted graph; rebuild the graph",
                    entry.id
                ),
            }
        })?;
        push_projection_edge_row(
            ProjectionEdgeRow {
                entry,
                source,
                target,
                type_id,
                schema_reversed: false,
                weight,
                relationship_identity: Some(relationship_identity.clone()),
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
                    relationship_identity: Some(relationship_identity),
                    operation,
                },
                out,
            );
        }
    }
    Ok(())
}

fn projection_edge_endpoints(
    context: &SyncReplayContext,
    edge: &builder::RegisteredEdge,
    row: &serde_json::Value,
) -> Option<(Option<u32>, String, u32, String)> {
    let target_oid = edge.to_table_oid;
    if let Some(from_table) = context
        .tables
        .iter()
        .find(|table| table.table_oid == edge.from_table_oid)
    {
        return Some((
            Some(edge.from_table_oid),
            row_pk_value(row, &from_table.id_columns)?,
            target_oid,
            row_text_value(row, &edge.from_column)?,
        ));
    }
    Some((
        context.edge_source_node_oids.get(&edge.mapping_id).copied(),
        row_text_value(row, &edge.from_column)?,
        target_oid,
        row_text_value(row, &edge.to_column)?,
    ))
}

struct ProjectionEdgeRow<'a> {
    entry: &'a SyncLogEntry,
    source: u32,
    target: u32,
    type_id: u8,
    schema_reversed: bool,
    weight: Option<u32>,
    relationship_identity: Option<crate::edge_store::RelationshipIdentity>,
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
        relationship_identity: row.relationship_identity,
        table_oid: None,
        pk_hash: None,
        primary_key: None,
        node_idx: None,
        filter_column_id: None,
        filter_value: None,
        tenant_hash: None,
        tenant: None,
    });
}

fn append_projection_filter_rows(
    nodes: &ProjectionNodePlanner<'_>,
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
                nodes.engine,
                nodes,
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
                    nodes.engine,
                    nodes,
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
                    nodes.engine,
                    nodes,
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
                nodes.engine,
                nodes,
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
    eng: &engine::Engine,
    nodes: &ProjectionNodePlanner<'_>,
    context: &SyncReplayContext,
    table_oid: u32,
    entry: &SyncLogEntry,
    pk: &str,
    row: Option<&serde_json::Value>,
    properties: &HashMap<String, String>,
    operation: MutationOperation,
    out: &mut Vec<ProjectionSyncRow>,
) -> safety::GraphResult<()> {
    let node_idx = match operation {
        MutationOperation::UpsertNode => nodes.resolve_final(table_oid, pk),
        MutationOperation::DeleteNode => nodes.resolve_before(table_oid, pk, entry.id),
        MutationOperation::InsertEdge | MutationOperation::DeleteEdge => None,
    };
    let Some(node_idx) = node_idx else {
        return Ok(());
    };
    for filter in &context.filters {
        if filter.table_oid != table_oid {
            continue;
        }
        let Some(column_idx) = eng
            .filter_index
            .find_column_for_table(table_oid, &filter.column_name)
        else {
            continue;
        };
        let value = persisted_filter_value_from_row_or_properties(
            &filter.column_name,
            eng.filter_index.column_type(column_idx),
            row,
            properties,
        )?;
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
            relationship_identity: None,
            table_oid: None,
            pk_hash: None,
            primary_key: None,
            node_idx: Some(node_idx),
            filter_column_id: Some(column_idx as u32),
            filter_value: Some(value),
            tenant_hash: None,
            tenant: None,
        });
    }
    Ok(())
}

fn persisted_filter_value_from_row_or_properties(
    column_name: &str,
    column_type: Option<FilterColumnType>,
    row: Option<&serde_json::Value>,
    properties: &HashMap<String, String>,
) -> safety::GraphResult<PersistedFilterValue> {
    let raw = raw_filter_value(column_name, row, properties);
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return Ok(PersistedFilterValue::Null);
    };
    let column_type = column_type.ok_or_else(|| {
        safety::GraphError::Internal(format!(
            "registered projection filter column '{column_name}' has no encoded domain"
        ))
    })?;
    match column_type {
        FilterColumnType::Numeric => Ok(PersistedFilterValue::Numeric(json_value_i64(&raw)?)),
        FilterColumnType::Boolean => Ok(PersistedFilterValue::Boolean(json_value_bool(&raw)?)),
        FilterColumnType::Text => Ok(PersistedFilterValue::Text(json_value_text(&raw)?)),
        FilterColumnType::Date => Ok(PersistedFilterValue::Date(encode_date_filter_value(
            &string_filter_value(&raw)?,
        )?)),
        FilterColumnType::Timestamptz => Ok(PersistedFilterValue::Timestamptz(
            encode_timestamptz_filter_value(&string_filter_value(&raw)?)?,
        )),
        FilterColumnType::Uuid => Ok(PersistedFilterValue::Uuid(parse_uuid_u128(
            &json_value_text(&raw)?,
        )?)),
    }
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
        if projection_edge_endpoints(context, edge, row).is_none() {
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
        if filter.table_oid != table_oid {
            continue;
        }
        let Some(column_idx) = eng
            .filter_index
            .find_column_for_table(table_oid, &filter.column_name)
        else {
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
    let raw = raw_filter_value(column_name, row, properties);
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

fn raw_filter_value(
    column_name: &str,
    row: Option<&serde_json::Value>,
    properties: &HashMap<String, String>,
) -> Option<serde_json::Value> {
    row.and_then(|row| row.get(column_name))
        .cloned()
        .or_else(|| {
            properties
                .get(column_name)
                .map(|value| serde_json::Value::String(value.clone()))
        })
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
        let Some((source_oid, from_pk, target_oid, to_pk)) =
            projection_edge_endpoints(context, edge, row)
        else {
            continue;
        };
        let edge_label = edge
            .label_column
            .as_deref()
            .and_then(|column| row_text_value(row, column))
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| edge.label.clone());
        let type_id = eng.register_edge_type(&edge_label)?;
        let source = resolve_sync_endpoint(eng, source_oid, &from_pk, &context.all_table_oids);
        let target = resolve_sync_endpoint(eng, Some(target_oid), &to_pk, &context.all_table_oids);
        if let (Some(source), Some(target)) = (source, target) {
            let Some(source_key) = row_pk_value(row, &edge.source_key_columns) else {
                continue;
            };
            let relationship_id = Some(intern_sync_relationship_identity(
                eng,
                edge.mapping_id,
                source_key,
            )?);
            push_sync_edge_delta(eng, source, target, type_id, false, relationship_id, kind)?;
            if edge.bidirectional {
                push_sync_edge_delta(eng, target, source, type_id, true, relationship_id, kind)?;
            }
        }
    }
    Ok(())
}

fn intern_sync_relationship_identity(
    eng: &mut engine::Engine,
    mapping_id: u64,
    source_key: String,
) -> safety::GraphResult<crate::edge_store::RelationshipId> {
    let identity = crate::edge_store::RelationshipIdentity {
        mapping_id,
        source_key,
    };
    if let Some((relationship_id, _)) = eng
        .relationship_identities
        .iter()
        .enumerate()
        .find(|(_, existing)| existing.as_ref() == Some(&identity))
    {
        return crate::edge_store::RelationshipId::try_from(relationship_id).map_err(|_| {
            safety::GraphError::Internal("relationship identity count exceeds u32".to_string())
        });
    }
    let relationship_id =
        crate::edge_store::RelationshipId::try_from(eng.relationship_identities.len()).map_err(
            |_| safety::GraphError::Internal("relationship identity count exceeds u32".to_string()),
        )?;
    eng.relationship_identities.push(Some(identity));
    Ok(relationship_id)
}

pub(crate) fn push_sync_edge_delta(
    eng: &mut engine::Engine,
    source: u32,
    target: u32,
    type_id: u8,
    schema_reversed: bool,
    relationship_id: Option<crate::edge_store::RelationshipId>,
    kind: engine::MutationKind,
) -> safety::GraphResult<()> {
    eng.push_edge_mutation(engine::EdgeMutation {
        source,
        target,
        type_id,
        schema_reversed,
        relationship_id,
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
        return eng.resolve(oid, pk);
    }
    resolve_unique_endpoint(all_oids, |oid| eng.resolve(oid, pk))
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
    let graph_tenant = selected_or_default_graph_metadata_via_definer()
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
        guard_standalone_endpoint_lifecycle, intern_sync_relationship_identity, parse_sync_op,
        parse_sync_properties, required_sync_i64, required_sync_string, resolve_unique_endpoint,
        tenant_change_from_entry, ParsedSyncRows, PreparedProjectionEntry, ProjectionNodePlanner,
        SyncLogEntry, SyncOp, SyncReplayContext, TenantChange,
    };
    use crate::builder::{PrimaryKeySpec, PropertyColumns, RegisteredEdge, RegisteredTable};
    use crate::engine::Engine;
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
                table_oid: 42,
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
            edge_source_node_oids: HashMap::new(),
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

    #[test]
    fn sync_relationship_identity_interning_uses_mapping_and_source_key() {
        let mut engine = Engine::new();

        let relationship_id = intern_sync_relationship_identity(&mut engine, 9, "r-1".to_string())
            .expect("relationship identity interns");
        let repeated_id = intern_sync_relationship_identity(&mut engine, 9, "r-1".to_string())
            .expect("existing relationship identity resolves");

        assert_eq!(relationship_id, 1);
        assert_eq!(repeated_id, relationship_id);
        assert_eq!(engine.relationship_identities.len(), 2);
        assert_eq!(
            engine.relationship_identities[1],
            Some(crate::edge_store::RelationshipIdentity {
                mapping_id: 9,
                source_key: "r-1".to_string(),
            })
        );
    }

    #[test]
    fn projection_node_planner_allocates_contiguous_non_serving_slots() {
        let mut engine = Engine::new();
        engine.node_store.add_node(42, "base".to_string());
        engine.resolution_insert(42, "base", 0);
        let mut planner = ProjectionNodePlanner {
            engine: &engine,
            timelines: HashMap::new(),
            upserts: HashMap::new(),
            deletes: HashMap::new(),
            next_node_idx: 1,
        };

        planner.plan_upsert(42, "new-a", 1).unwrap();
        planner.plan_upsert(42, "new-b", 2).unwrap();
        assert_eq!(
            planner.resolve_endpoint_final(Some(42), "new-b", &[42]),
            Some(2)
        );
        planner.plan_delete(42, "new-a", 3);
        planner.plan_upsert(42, "new-a", 4).unwrap();
        assert_eq!(planner.upserts.get(&4), Some(&3));
        planner.plan_upsert(42, "future-target", 6).unwrap();
        assert_eq!(
            planner.resolve_endpoint_final(Some(42), "future-target", &[42]),
            Some(4)
        );
        planner.plan_upsert(42, "base", 7).unwrap();
        assert_eq!(planner.upserts.get(&7), Some(&0));
        assert_eq!(engine.node_store.node_count(), 1);
        assert_eq!(engine.resolve(42, "new-a"), None);
    }

    #[test]
    fn endpoint_fallback_requires_one_exact_table_match() {
        assert_eq!(
            resolve_unique_endpoint(&[10, 20], |oid| (oid == 10).then_some(7)),
            Some(7)
        );
        assert_eq!(
            resolve_unique_endpoint(&[10, 20], |oid| Some(oid / 10)),
            None
        );
    }

    #[test]
    fn standalone_edges_reject_node_identity_changes() {
        let table = RegisteredTable {
            table_oid: 42,
            table_name: "public.nodes".to_string(),
            id_columns: PrimaryKeySpec::from_columns(vec!["id".to_string()]),
            columns: PropertyColumns::from_columns(Vec::new()),
            tenant_column: None,
        };
        let edge = RegisteredEdge {
            mapping_id: 9,
            from_table_oid: 84,
            from_table: "public.edges".to_string(),
            from_column: "from_id".to_string(),
            source_key_columns: PrimaryKeySpec::from_columns(vec!["id".to_string()]),
            to_table_oid: 42,
            to_table: "public.nodes".to_string(),
            to_column: "to_id".to_string(),
            label: "linked".to_string(),
            bidirectional: false,
            weight_column: None,
            label_column: None,
        };
        let context = SyncReplayContext {
            tables: vec![table],
            edges: vec![edge],
            filters: Vec::new(),
            table_oids: HashMap::from([
                ("public.nodes".to_string(), 42),
                ("public.edges".to_string(), 84),
            ]),
            all_table_oids: vec![42, 84],
            edge_source_tables: HashSet::from(["public.edges".to_string()]),
            edge_source_oids: HashSet::from([84]),
            edge_source_node_oids: HashMap::new(),
        };
        let entry = SyncLogEntry {
            id: 11,
            op: SyncOp::Delete,
            table_oid: Some(42),
            table_name: "public.nodes".to_string(),
            old_pk: Some("n1".to_string()),
            new_pk: None,
            properties: None,
            old_row: None,
            new_row: None,
        };
        let prepared = PreparedProjectionEntry {
            entry: &entry,
            table_oid: 42,
            is_node_table: true,
            properties: HashMap::new(),
            row_images: ParsedSyncRows {
                old: None,
                new: None,
            },
            tenant_change: TenantChange::default(),
        };

        let error = guard_standalone_endpoint_lifecycle(&context, &[prepared])
            .expect_err("standalone endpoint delete must fail closed");

        assert!(matches!(error, GraphError::UnsupportedOperation { .. }));
        assert!(error.to_string().contains("rebuild the graph"));
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
