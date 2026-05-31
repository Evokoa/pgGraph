use super::*;

pub(super) fn check_enabled() {
    if !config::ENABLED.get() {
        safety::GraphError::Disabled.report();
    }
}

#[pg_extern(schema = "graph")]
fn test_enabled() -> bool {
    config::ENABLED.get()
}

pub(crate) fn check_enabled_result() -> safety::GraphResult<()> {
    if config::ENABLED.get() {
        Ok(())
    } else {
        Err(safety::GraphError::Disabled)
    }
}

pub(super) fn require_graph_admin_result() -> safety::GraphResult<()> {
    let allowed = Spi::connect(|client| {
        let result = client.select(
            "SELECT
                COALESCE((SELECT rolsuper FROM pg_roles WHERE rolname = current_user), false)
                OR has_schema_privilege(current_user, 'graph', 'CREATE')",
            None,
            &[],
        )?;
        Ok::<_, pgrx::spi::SpiError>(
            result
                .first()
                .get::<bool>(1)
                .ok()
                .flatten()
                .unwrap_or(false),
        )
    })
    .map_err(|err| {
        safety::GraphError::Internal(format!("graph admin privilege check failed: {}", err))
    })?;

    if allowed {
        Ok(())
    } else {
        Err(safety::GraphError::AclDenied {
            table: "graph schema admin".to_string(),
        })
    }
}

pub(super) fn with_panic_boundary<T>(_context: &str, f: impl FnOnce() -> T) -> T {
    // pgrx already installs the real panic boundary around #[pg_extern] calls.
    // Catching inside SPI/user-code paths can accidentally intercept pgrx
    // ErrorReport panics and either erase the SQLSTATE or abort the backend, so
    // this helper is deliberately just a uniform call site.
    f()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledMaintenanceInputs {
    pub(crate) pending_sync_rows: i64,
    pub(crate) disabled_trigger_count: i32,
    pub(crate) edge_buffer_used: i32,
    pub(crate) needs_vacuum: bool,
    pub(crate) needs_rebuild: bool,
    pub(crate) read_only: bool,
    pub(crate) compaction_recommended: bool,
}

impl From<&crate::types::EngineStatus> for ScheduledMaintenanceInputs {
    fn from(status: &crate::types::EngineStatus) -> Self {
        Self {
            pending_sync_rows: status.pending_sync_rows,
            disabled_trigger_count: status.disabled_trigger_count,
            edge_buffer_used: status.edge_buffer_used,
            needs_vacuum: status.needs_vacuum,
            needs_rebuild: status.needs_rebuild,
            read_only: status.read_only,
            compaction_recommended: status.compaction_recommended,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledMaintenanceDecision {
    pub(crate) apply_sync: bool,
    pub(crate) start_maintenance: bool,
}

pub(crate) fn scheduled_maintenance_decision(
    inputs: ScheduledMaintenanceInputs,
) -> ScheduledMaintenanceDecision {
    let apply_sync = inputs.pending_sync_rows > 0
        && inputs.disabled_trigger_count == 0
        && !inputs.needs_rebuild
        && !inputs.read_only;
    let start_maintenance = inputs.read_only
        || inputs.needs_rebuild
        || inputs.needs_vacuum
        || inputs.edge_buffer_used > 0
        || inputs.compaction_recommended;

    ScheduledMaintenanceDecision {
        apply_sync,
        start_maintenance,
    }
}

#[cfg(test)]
mod scheduled_maintenance_tests {
    use super::{
        scheduled_maintenance_decision, ScheduledMaintenanceDecision, ScheduledMaintenanceInputs,
    };

    #[test]
    fn scheduled_maintenance_decision_recommends_apply_when_trigger_sync_is_safe() {
        let decision = scheduled_maintenance_decision(ScheduledMaintenanceInputs {
            pending_sync_rows: 2,
            disabled_trigger_count: 0,
            edge_buffer_used: 0,
            needs_vacuum: false,
            needs_rebuild: false,
            read_only: false,
            compaction_recommended: false,
        });

        assert_eq!(
            decision,
            ScheduledMaintenanceDecision {
                apply_sync: true,
                start_maintenance: false,
            }
        );
    }

    #[test]
    fn scheduled_maintenance_decision_blocks_apply_for_rebuild_or_read_only() {
        for mut inputs in [
            ScheduledMaintenanceInputs {
                pending_sync_rows: 2,
                disabled_trigger_count: 1,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 2,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: true,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 2,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: true,
                compaction_recommended: false,
            },
        ] {
            let decision = scheduled_maintenance_decision(inputs);
            assert!(!decision.apply_sync);

            inputs.pending_sync_rows = 0;
            let no_pending_decision = scheduled_maintenance_decision(inputs);
            assert!(!no_pending_decision.apply_sync);
        }
    }

    #[test]
    fn scheduled_maintenance_decision_starts_for_vacuum_overlay_rebuild_or_read_only() {
        for inputs in [
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 1,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: true,
                needs_rebuild: false,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: true,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: true,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: false,
                compaction_recommended: true,
            },
        ] {
            let decision = scheduled_maintenance_decision(inputs);
            assert!(decision.start_maintenance);
        }
    }
}

/// Return current engine status.
///
/// See: `docs/user_guide/api-reference.mdx`
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn status() -> TableIterator<
    'static,
    (
        name!(node_count, i32),
        name!(edge_count, i32),
        name!(memory_used_mb, f64),
        name!(memory_limit_mb, i32),
        name!(sync_mode, String),
        name!(sync_status, String),
        name!(last_build, Option<TimestampWithTimeZone>),
        name!(last_vacuum, Option<TimestampWithTimeZone>),
        name!(edge_types, Vec<String>),
        name!(edge_buffer_used, i32),
        name!(has_unidirectional_edges, bool),
        name!(schema_status, String),
        name!(sync_lag, i64),
        name!(pending_edge_deltas, i32),
        name!(needs_vacuum, bool),
        name!(needs_rebuild, bool),
        name!(applied_sync_id, i64),
        name!(pending_sync_rows, i64),
        name!(invalid_reason, Option<String>),
        name!(disabled_trigger_count, i32),
        name!(read_only, bool),
        name!(read_only_reason, Option<String>),
        name!(projection_mode, String),
        name!(overlay_tombstone_count, i32),
        name!(overlay_memory_bytes, i64),
        name!(compaction_recommended, bool),
        name!(tx_delta_dirty, bool),
        name!(tx_delta_added_nodes, i32),
        name!(tx_delta_deleted_nodes, i32),
        name!(tx_delta_added_edges, i32),
        name!(tx_delta_deleted_edges, i32),
        name!(tx_delta_memory_bytes, i64),
    ),
> {
    with_panic_boundary("status()", || {
        let s = refreshed_engine_status().unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            s.node_count,
            s.edge_count,
            s.memory_used_mb,
            s.memory_limit_mb,
            s.sync_mode,
            s.sync_status,
            s.last_build,
            s.last_vacuum,
            s.edge_types,
            s.edge_buffer_used,
            s.has_unidirectional_edges,
            s.schema_state,
            s.sync_lag,
            s.edge_buffer_used,
            s.needs_vacuum,
            s.needs_rebuild,
            s.applied_sync_id,
            s.pending_sync_rows,
            s.invalid_reason,
            s.disabled_trigger_count,
            s.read_only,
            s.read_only_reason,
            s.projection_mode,
            s.overlay_tombstone_count,
            s.overlay_memory_bytes,
            s.compaction_recommended,
            s.tx_delta_dirty,
            s.tx_delta_added_nodes,
            s.tx_delta_deleted_nodes,
            s.tx_delta_added_edges,
            s.tx_delta_deleted_edges,
            s.tx_delta_memory_bytes,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn sync_health() -> TableIterator<
    'static,
    (
        name!(sync_mode, String),
        name!(query_freshness, String),
        name!(sync_batch_size, i32),
        name!(applied_sync_id, i64),
        name!(max_sync_log_id, i64),
        name!(pending_sync_rows, i64),
        name!(disabled_trigger_count, i32),
        name!(edge_buffer_used, i32),
        name!(edge_buffer_size, i32),
        name!(needs_vacuum, bool),
        name!(needs_rebuild, bool),
        name!(read_only, bool),
        name!(read_only_reason, Option<String>),
        name!(projection_mode, String),
        name!(overlay_tombstone_count, i32),
        name!(overlay_memory_bytes, i64),
        name!(compaction_recommended, bool),
        name!(tx_delta_dirty, bool),
        name!(tx_delta_added_nodes, i32),
        name!(tx_delta_deleted_nodes, i32),
        name!(tx_delta_added_edges, i32),
        name!(tx_delta_deleted_edges, i32),
        name!(tx_delta_memory_bytes, i64),
        name!(apply_sync_recommended, bool),
        name!(maintenance_recommended, bool),
    ),
> {
    with_panic_boundary("sync_health()", || {
        let s = refreshed_engine_status().unwrap_or_else(|err| err.report());
        let max_sync_log_id = max_sync_log_id().unwrap_or_else(|err| err.report());
        let edge_buffer_size = config::EDGE_BUFFER_SIZE.get();
        let decision = scheduled_maintenance_decision((&s).into());

        TableIterator::new(vec![(
            s.sync_mode,
            config::query_freshness(),
            config::sync_batch_size().min(i32::MAX as usize) as i32,
            s.applied_sync_id,
            max_sync_log_id,
            s.pending_sync_rows,
            s.disabled_trigger_count,
            s.edge_buffer_used,
            edge_buffer_size,
            s.needs_vacuum,
            s.needs_rebuild,
            s.read_only,
            s.read_only_reason,
            s.projection_mode,
            s.overlay_tombstone_count,
            s.overlay_memory_bytes,
            s.compaction_recommended,
            s.tx_delta_dirty,
            s.tx_delta_added_nodes,
            s.tx_delta_deleted_nodes,
            s.tx_delta_added_edges,
            s.tx_delta_deleted_edges,
            s.tx_delta_memory_bytes,
            decision.apply_sync,
            decision.start_maintenance,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn run_scheduled_maintenance() -> TableIterator<
    'static,
    (
        name!(applied_sync, bool),
        name!(maintenance_started, bool),
        name!(maintenance_job_id, Option<String>),
        name!(pending_sync_rows, i64),
        name!(edge_buffer_used, i32),
        name!(message, String),
    ),
> {
    with_panic_boundary("run_scheduled_maintenance()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let mut status = refreshed_engine_status().unwrap_or_else(|err| err.report());
        let mut applied_sync = false;

        let mut decision = scheduled_maintenance_decision((&status).into());
        if decision.apply_sync {
            apply_sync_internal().unwrap_or_else(|err| err.report());
            applied_sync = true;
            status = refreshed_engine_status().unwrap_or_else(|err| err.report());
            decision = scheduled_maintenance_decision((&status).into());
        }

        let mut maintenance_job_id = None;
        if decision.start_maintenance {
            let job_id = create_maintenance_job().unwrap_or_else(|err| err.report());
            if let Err(err) = launch_maintenance_worker(&job_id) {
                let _ = update_maintenance_job_failed(&job_id, &err.to_string());
                err.report();
            }
            maintenance_job_id = Some(job_id);
            status = refreshed_engine_status().unwrap_or_else(|err| err.report());
        }

        let maintenance_started = maintenance_job_id.is_some();
        let message = match (applied_sync, maintenance_started) {
            (true, true) => "applied sync and started maintenance",
            (true, false) => "applied sync",
            (false, true) => "started maintenance",
            (false, false) => "no scheduled graph maintenance needed",
        }
        .to_string();

        TableIterator::new(vec![(
            applied_sync,
            maintenance_started,
            maintenance_job_id,
            status.pending_sync_rows,
            status.edge_buffer_used,
            message,
        )])
    })
}

fn refreshed_engine_status() -> safety::GraphResult<crate::types::EngineStatus> {
    let disabled_trigger_count = disabled_graph_trigger_count()?;
    let catalog_state = current_catalog_state();
    let applied_sync_id = ENGINE.with(|e| e.borrow().applied_sync_id);
    let pending = pending_sync_rows(applied_sync_id)?;

    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.refresh_observed_state(disabled_trigger_count, pending, &catalog_state);
        Ok(eng.status())
    })
}

/// Build the graph from registered tables and edges.
///
/// Optionally persists the result to disk based on `graph.persist_on_build`.
///
/// See: `docs/user_guide/build-and-persistence.mdx`
#[pg_extern(schema = "graph")]
pub(super) fn build() -> TableIterator<
    'static,
    (
        name!(nodes_loaded, i64),
        name!(edges_loaded, i64),
        name!(build_time_ms, f64),
        name!(memory_used_mb, f64),
        name!(sync_mode, String),
        name!(projection_mode, String),
    ),
> {
    with_panic_boundary("build()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let result = execute_build(false).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            result.nodes_loaded,
            result.edges_loaded,
            result.build_time_ms,
            result.memory_used_mb,
            result.sync_mode,
            result.projection_mode,
        )])
    })
}

#[pg_guard]
/// Background worker entrypoint for asynchronous graph builds.
///
/// PostgreSQL invokes this function by name after `graph.build(concurrently :=
/// true)` registers a dynamic background worker. Worker metadata is read from
/// pgrx's background-worker `extra` field as typed JSON metadata.
pub extern "C-unwind" fn graph_build_worker_main(_arg: pgrx::pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    let extra = BackgroundWorker::get_extra();
    let metadata = match WorkerMetadata::decode(extra) {
        Ok(metadata) => metadata,
        Err(err) => {
            pgrx::warning!(
                "graph build worker received malformed worker metadata: {}",
                err
            );
            return;
        }
    };

    BackgroundWorker::connect_worker_to_spi(Some(&metadata.database), Some(&metadata.username));

    for _ in 0..50 {
        let job_visible = BackgroundWorker::transaction(|| {
            build_job_row(&metadata.job_id).is_ok_and(|row| row.is_some())
        });
        if job_visible {
            let result = BackgroundWorker::transaction(|| run_build_job(&metadata.job_id));
            if let Err(err) = result {
                let message = err.to_string();
                let record_result = BackgroundWorker::transaction(|| {
                    update_build_job_failed(&metadata.job_id, &message)
                });
                if let Err(record_err) = record_result {
                    pgrx::warning!(
                        "graph concurrent build {} failed and failure status could not be recorded: {}",
                        metadata.job_id,
                        record_err
                    );
                }
                pgrx::warning!(
                    "graph concurrent build {} failed: {}",
                    metadata.job_id,
                    message
                );
            }
            return;
        }
        if !BackgroundWorker::wait_latch(Some(Duration::from_millis(100))) {
            return;
        }
    }

    pgrx::warning!(
        "graph concurrent build {} was not visible to worker before timeout",
        metadata.job_id
    );
}

#[pg_guard]
/// Background worker entrypoint for asynchronous graph maintenance.
///
/// PostgreSQL invokes this function by name after
/// `graph.maintenance(concurrently := true)` registers a dynamic background
/// worker. Worker metadata is read from pgrx's background-worker `extra` field
/// as typed JSON metadata.
pub extern "C-unwind" fn graph_maintenance_worker_main(_arg: pgrx::pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    let extra = BackgroundWorker::get_extra();
    let metadata = match WorkerMetadata::decode(extra) {
        Ok(metadata) => metadata,
        Err(err) => {
            pgrx::warning!(
                "graph maintenance worker received malformed worker metadata: {}",
                err
            );
            return;
        }
    };

    BackgroundWorker::connect_worker_to_spi(Some(&metadata.database), Some(&metadata.username));

    for _ in 0..50 {
        let job_visible = BackgroundWorker::transaction(|| {
            maintenance_job_row(&metadata.job_id).is_ok_and(|row| row.is_some())
        });
        if job_visible {
            let result = BackgroundWorker::transaction(|| run_maintenance_job(&metadata.job_id));
            if let Err(err) = result {
                let message = err.to_string();
                let record_result = BackgroundWorker::transaction(|| {
                    update_maintenance_job_failed(&metadata.job_id, &message)
                });
                if let Err(record_err) = record_result {
                    pgrx::warning!(
                        "graph maintenance {} failed and failure status could not be recorded: {}",
                        metadata.job_id,
                        record_err
                    );
                }
                pgrx::warning!("graph maintenance {} failed: {}", metadata.job_id, message);
            }
            return;
        }
        if !BackgroundWorker::wait_latch(Some(Duration::from_millis(100))) {
            return;
        }
    }

    pgrx::warning!(
        "graph maintenance {} was not visible to worker before timeout",
        metadata.job_id
    );
}

/// Overload for `graph.build(concurrently := bool)`.
///
/// With `concurrently := false`, this delegates to the synchronous build path
/// and wraps the result in durable-job-shaped columns. With
/// `concurrently := true`, it creates a durable build job and launches a
/// dynamic background worker.
#[pg_extern(schema = "graph", name = "build")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_with_concurrently(
    concurrently: bool,
) -> TableIterator<
    'static,
    (
        name!(build_id, String),
        name!(status, String),
        name!(nodes_loaded, Option<i64>),
        name!(edges_loaded, Option<i64>),
        name!(build_time_ms, Option<f64>),
        name!(memory_used_mb, Option<f64>),
        name!(sync_mode, String),
        name!(projection_mode, String),
    ),
> {
    with_panic_boundary("build(concurrently)", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        if concurrently {
            let projection_mode = configured_projection_mode().unwrap_or_else(|err| err.report());
            let build_id = create_build_job(projection_mode).unwrap_or_else(|err| err.report());
            if let Err(err) = launch_build_worker(&build_id) {
                let _ = update_build_job_failed(&build_id, &err.to_string());
                err.report();
            }
            let row = build_job_row(&build_id)
                .unwrap_or_else(|err| err.report())
                .unwrap_or(BuildJobRow {
                    build_id,
                    status: JobStatus::Queued.as_str().to_string(),
                    nodes_loaded: None,
                    edges_loaded: None,
                    build_time_ms: None,
                    memory_used_mb: None,
                    sync_mode: current_sync_mode()
                        .map(|mode| mode.as_str().to_string())
                        .unwrap_or_else(|_| "manual".to_string()),
                    projection_mode: projection_mode.as_str().to_string(),
                    progress_phase: JobStatus::Queued.as_str().to_string(),
                    progress_message: Some("queued for background build".to_string()),
                    started_at: None,
                    finished_at: None,
                    error: None,
                });
            return TableIterator::new(vec![(
                row.build_id,
                row.status,
                row.nodes_loaded,
                row.edges_loaded,
                row.build_time_ms,
                row.memory_used_mb,
                row.sync_mode,
                row.projection_mode,
            )]);
        }
        let rows = build().collect::<Vec<_>>();
        let Some((
            nodes_loaded,
            edges_loaded,
            build_time_ms,
            memory_used_mb,
            sync_mode,
            projection_mode,
        )) = rows.into_iter().next()
        else {
            return TableIterator::new(Vec::new());
        };
        TableIterator::new(vec![(
            "00000000-0000-0000-0000-000000000000".to_string(),
            JobStatus::Completed.as_str().to_string(),
            Some(nodes_loaded),
            Some(edges_loaded),
            Some(build_time_ms),
            Some(memory_used_mb),
            sync_mode,
            projection_mode,
        )])
    })
}

/// Overload for `graph.build(mode := text)`.
#[pg_extern(schema = "graph", name = "build")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_with_mode(
    mode: &str,
) -> TableIterator<
    'static,
    (
        name!(nodes_loaded, i64),
        name!(edges_loaded, i64),
        name!(build_time_ms, f64),
        name!(memory_used_mb, f64),
        name!(sync_mode, String),
        name!(projection_mode, String),
    ),
> {
    with_panic_boundary("build(mode)", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let projection_mode = config::parse_projection_mode(mode).unwrap_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: format!(
                    "unsupported graph projection mode '{mode}'; expected 'csr_readonly' or 'mutable_overlay'"
                ),
            }
            .report()
        });
        let result =
            execute_build_with_mode(false, projection_mode).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            result.nodes_loaded,
            result.edges_loaded,
            result.build_time_ms,
            result.memory_used_mb,
            result.sync_mode,
            result.projection_mode,
        )])
    })
}

/// Return durable build-job status, or backend-local status for the zero UUID
/// used by synchronous builds.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_status(
    build_id: &str,
) -> TableIterator<
    'static,
    (
        name!(build_id, String),
        name!(status, String),
        name!(nodes_loaded, Option<i64>),
        name!(edges_loaded, Option<i64>),
        name!(build_time_ms, Option<f64>),
        name!(memory_used_mb, Option<f64>),
        name!(progress_phase, String),
        name!(progress_message, Option<String>),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("build_status()", || {
        if let Some(row) = build_job_row(build_id).unwrap_or_else(|err| err.report()) {
            return TableIterator::new(vec![(
                row.build_id,
                row.status,
                row.nodes_loaded,
                row.edges_loaded,
                row.build_time_ms,
                row.memory_used_mb,
                row.progress_phase,
                row.progress_message,
                row.started_at,
                row.finished_at,
                row.error,
            )]);
        }
        let status = ENGINE.with(|e| {
            let eng = e.borrow();
            if eng.built {
                JobStatus::Completed.as_str()
            } else {
                "not_found"
            }
        });
        TableIterator::new(vec![(
            build_id.to_string(),
            status.to_string(),
            None,
            None,
            None,
            None,
            status.to_string(),
            None,
            None,
            None,
            None,
        )])
    })
}

/// Register a table for graph indexing.
#[pg_extern(schema = "graph")]
fn add_table(
    table_name: pgrx::pg_sys::Oid,
    id_column: &str,
    columns: default!(Option<Vec<String>>, "NULL"),
    tenant_column: default!(Option<String>, "NULL"),
) {
    with_panic_boundary("add_table()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        validate_registered_table(
            table_name.to_u32(),
            id_column,
            columns.as_deref(),
            tenant_column.as_deref(),
        )
        .unwrap_or_else(|err| err.report());

        let table_regclass = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        let id_columns = builder::PrimaryKeySpec::from_catalog_text(id_column);
        let cols = builder::PropertyColumns::from_columns(columns.unwrap_or_default());

        insert_registered_table(
            &table_regclass,
            &id_columns,
            &cols,
            tenant_column.as_deref(),
        )
        .unwrap_or_else(|err| err.report());
    });
}

/// Register a table for graph indexing using one or more primary-key columns.
#[pg_extern(schema = "graph", name = "add_table")]
fn add_table_with_id_columns(
    table_name: pgrx::pg_sys::Oid,
    id_columns: Vec<String>,
    columns: default!(Option<Vec<String>>, "NULL"),
    tenant_column: default!(Option<String>, "NULL"),
) {
    let id_column = builder::PrimaryKeySpec::from_columns(id_columns).as_catalog_text();
    add_table(table_name, &id_column, columns, tenant_column);
}

/// Register an edge relationship.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    reason = "pgrx SQL ABI exposes each SQL argument"
)]
fn add_edge(
    from_table: pgrx::pg_sys::Oid,
    from_column: &str,
    to_table: pgrx::pg_sys::Oid,
    to_column: &str,
    label: &str,
    bidirectional: default!(bool, true),
    weight_column: default!(Option<String>, "NULL"),
    label_column: default!(Option<String>, "NULL"),
) {
    with_panic_boundary("add_edge()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        validate_column_exists(from_table.to_u32(), from_column).unwrap_or_else(|err| err.report());
        if validate_column_exists(to_table.to_u32(), to_column).is_err()
            && validate_column_exists(from_table.to_u32(), to_column).is_err()
        {
            safety::GraphError::Internal(format!(
                "to_column '{}' must exist on target table OID {} for FK-style edges or source table OID {} for edge-table edges",
                to_column,
                to_table.to_u32(),
                from_table.to_u32()
            ))
            .report();
        }
        if let Some(weight) = weight_column.as_deref() {
            validate_column_exists(from_table.to_u32(), weight).unwrap_or_else(|err| err.report());
        }
        if let Some(label_column) = label_column.as_deref() {
            validate_column_exists(from_table.to_u32(), label_column)
                .unwrap_or_else(|err| err.report());
        }

        let from_table = regclass_text(from_table.to_u32()).unwrap_or_else(|err| err.report());
        let to_table = regclass_text(to_table.to_u32()).unwrap_or_else(|err| err.report());
        insert_registered_edge(RegisteredEdgeInsert {
            from_table: &from_table,
            from_column,
            to_table: &to_table,
            to_column,
            label,
            bidirectional,
            weight_column: weight_column.as_deref(),
            label_column: label_column.as_deref(),
        })
        .unwrap_or_else(|err| err.report());
    });
}

/// List tables registered for graph indexing.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn registered_tables() -> TableIterator<
    'static,
    (
        name!(table_name, String),
        name!(id_columns, Vec<String>),
        name!(columns, Vec<String>),
        name!(tenant_column, Option<String>),
    ),
> {
    with_panic_boundary("registered_tables()", || {
        let rows = Spi::connect(|client| {
            let result = client.select(
                "SELECT table_name, id_column, columns, tenant_column
                 FROM graph._registered_tables
                 ORDER BY table_name",
                None,
                &[],
            )?;
            let mut rows = Vec::new();
            for row in result {
                let table_name = row.get::<String>(1)?.unwrap_or_default();
                let id_column = row.get::<String>(2)?.unwrap_or_default();
                let columns = row.get::<String>(3)?.unwrap_or_default();
                let tenant_column = row.get::<String>(4)?.filter(|s| !s.is_empty());
                rows.push((
                    table_name,
                    split_catalog_columns(&id_column),
                    split_catalog_columns(&columns),
                    tenant_column,
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(rows)
        })
        .unwrap_or_else(|err| {
            pgrx::error!("graph.registered_tables() failed: {}", err);
        });

        TableIterator::new(rows)
    })
}

/// List edge relationships registered for graph indexing.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn registered_edges() -> TableIterator<
    'static,
    (
        name!(from_table, String),
        name!(from_column, String),
        name!(to_table, String),
        name!(to_column, String),
        name!(label, String),
        name!(bidirectional, bool),
        name!(weight_column, Option<String>),
        name!(label_column, Option<String>),
    ),
> {
    with_panic_boundary("registered_edges()", || {
        let rows = Spi::connect(|client| {
            let result = client.select(
                "SELECT from_table, from_column, to_table, to_column, label, bidirectional, weight_column, label_column
                 FROM graph._registered_edges
                 ORDER BY from_table, from_column, to_table, to_column, label",
                None,
                &[],
            )?;
            let mut rows = Vec::new();
            for row in result {
                rows.push((
                    row.get::<String>(1)?.unwrap_or_default(),
                    row.get::<String>(2)?.unwrap_or_default(),
                    row.get::<String>(3)?.unwrap_or_default(),
                    row.get::<String>(4)?.unwrap_or_default(),
                    row.get::<String>(5)?.unwrap_or_default(),
                    row.get::<bool>(6)?.unwrap_or(true),
                    row.get::<String>(7)?.filter(|s| !s.is_empty()),
                    row.get::<String>(8)?.filter(|s| !s.is_empty()),
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(rows)
        })
        .unwrap_or_else(|err| {
            pgrx::error!("graph.registered_edges() failed: {}", err);
        });

        TableIterator::new(rows)
    })
}

/// Register a column for traversal-time filters.
#[pg_extern(schema = "graph")]
fn add_filter_column(
    table_name: pgrx::pg_sys::Oid,
    column_name: &str,
    column_type: default!(&str, "'numeric'"),
) {
    with_panic_boundary("add_filter_column()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        validate_column_exists(table_name.to_u32(), column_name).unwrap_or_else(|err| err.report());
        validate_filter_column_type(table_name.to_u32(), column_name, column_type)
            .unwrap_or_else(|err| err.report());
        let table_regclass = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        Spi::run_with_args(
            "INSERT INTO graph._registered_filter_columns (table_name, column_name, column_type)
             VALUES ($1, $2, $3)
             ON CONFLICT (table_name, column_name) DO UPDATE SET column_type = EXCLUDED.column_type",
            &[
                table_regclass.into(),
                column_name.into(),
                column_type.to_ascii_lowercase().into(),
            ],
        )
        .unwrap_or_else(|e| {
            pgrx::error!("graph.add_filter_column() failed: {}", e);
        });
    });
}

/// Build a structured equality filter for `graph.traverse(filter := ...)`.
#[pg_extern(schema = "graph")]
fn equals(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "eq", value)
}

#[pg_extern(schema = "graph", name = "equals")]
fn equals_text(column_name: &str, value: &str) -> pgrx::JsonB {
    equals(
        column_name,
        pgrx::JsonB(serde_json::Value::String(value.to_string())),
    )
}

#[pg_extern(schema = "graph", name = "equals")]
fn equals_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    equals(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Alias for `graph.equals()`.
#[pg_extern(schema = "graph")]
fn eq(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    equals(column_name, value)
}

#[pg_extern(schema = "graph", name = "eq")]
fn eq_text(column_name: &str, value: &str) -> pgrx::JsonB {
    equals_text(column_name, value)
}

#[pg_extern(schema = "graph", name = "eq")]
fn eq_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    equals_i64(column_name, value)
}

/// Build a structured inequality filter for `graph.traverse(filter := ...)`.
#[pg_extern(schema = "graph")]
fn not_equals(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "neq", value)
}

/// Alias for `graph.not_equals()`.
#[pg_extern(schema = "graph")]
fn neq(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    not_equals(column_name, value)
}

#[pg_extern(schema = "graph", name = "neq")]
fn neq_text(column_name: &str, value: &str) -> pgrx::JsonB {
    not_equals(
        column_name,
        pgrx::JsonB(serde_json::Value::String(value.to_string())),
    )
}

#[pg_extern(schema = "graph", name = "neq")]
fn neq_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    not_equals(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Build a structured membership filter for `graph.traverse(filter := ...)`.
#[pg_extern(schema = "graph", name = "in")]
fn in_filter(column_name: &str, values: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "in", values)
}

/// Build a structured negative membership filter.
#[pg_extern(schema = "graph")]
fn not_in(column_name: &str, values: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "not_in", values)
}

/// Build a structured substring filter for text traversal filters.
#[pg_extern(schema = "graph")]
fn contains_text(column_name: &str, value: &str) -> pgrx::JsonB {
    filter_helper(
        column_name,
        "contains",
        pgrx::JsonB(serde_json::Value::String(value.to_string())),
    )
}

/// Build a structured prefix filter for text traversal filters.
#[pg_extern(schema = "graph")]
fn prefix_text(column_name: &str, value: &str) -> pgrx::JsonB {
    filter_helper(
        column_name,
        "prefix",
        pgrx::JsonB(serde_json::Value::String(value.to_string())),
    )
}

/// Build a structured SQL NULL filter.
#[pg_extern(schema = "graph")]
fn is_null(column_name: &str) -> pgrx::JsonB {
    filter_helper(column_name, "is_null", pgrx::JsonB(serde_json::Value::Null))
}

/// Build a structured SQL NOT NULL filter.
#[pg_extern(schema = "graph")]
fn is_not_null(column_name: &str) -> pgrx::JsonB {
    filter_helper(
        column_name,
        "is_not_null",
        pgrx::JsonB(serde_json::Value::Null),
    )
}

/// Build a structured greater-than filter for `graph.traverse(filter := ...)`.
#[pg_extern(schema = "graph")]
fn greater_than(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "gt", value)
}

#[pg_extern(schema = "graph", name = "greater_than")]
fn greater_than_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    greater_than(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Alias for `graph.greater_than()`.
#[pg_extern(schema = "graph")]
fn gt(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    greater_than(column_name, value)
}

#[pg_extern(schema = "graph", name = "gt")]
fn gt_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    greater_than_i64(column_name, value)
}

/// Build a structured greater-than-or-equal filter.
#[pg_extern(schema = "graph")]
fn at_least(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "gte", value)
}

/// Alias for `graph.at_least()`.
#[pg_extern(schema = "graph")]
fn gte(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    at_least(column_name, value)
}

#[pg_extern(schema = "graph", name = "gte")]
fn gte_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    at_least(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Build a structured less-than filter.
#[pg_extern(schema = "graph")]
fn less_than(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "lt", value)
}

/// Alias for `graph.less_than()`.
#[pg_extern(schema = "graph")]
fn lt(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    less_than(column_name, value)
}

#[pg_extern(schema = "graph", name = "lt")]
fn lt_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    less_than(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Build a structured less-than-or-equal filter.
#[pg_extern(schema = "graph")]
fn at_most(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "lte", value)
}

/// Alias for `graph.at_most()`.
#[pg_extern(schema = "graph")]
fn lte(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    at_most(column_name, value)
}

#[pg_extern(schema = "graph", name = "lte")]
fn lte_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    at_most(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Build a structured inclusive range filter.
#[pg_extern(schema = "graph")]
fn between(column_name: &str, lower: pgrx::JsonB, upper: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(
        column_name,
        "between",
        pgrx::JsonB(serde_json::Value::Array(vec![lower.0, upper.0])),
    )
}

/// Wrap a filter in the node scope expected by traversal.
#[pg_extern(schema = "graph")]
fn on_node(filter: pgrx::JsonB) -> pgrx::JsonB {
    let Some(where_clause) = filter.0.get("where").cloned() else {
        return filter;
    };
    pgrx::JsonB(serde_json::json!({ "node": { "where": where_clause } }))
}

/// Construct the canonical SDK-friendly node reference string.
#[pg_extern(schema = "graph")]
fn node_ref_string(table_name: pgrx::pg_sys::Oid, node_id: &str) -> String {
    with_panic_boundary("node_ref_string()", || {
        canonical_node_ref_string(table_name.to_u32(), node_id).unwrap_or_else(|err| err.report())
    })
}

/// Format a traversal `path` + `edge_path` pair as readable hop text.
#[pg_extern(schema = "graph")]
fn format_path(
    path: pgrx::JsonB,
    edge_path: pgrx::JsonB,
    separator: default!(&str, "' | '"),
) -> String {
    with_panic_boundary("format_path()", || {
        format_path_value(&path.0, &edge_path.0, separator).unwrap_or_else(|err| err.report())
    })
}

/// Combine structured filters with logical AND.
#[pg_extern(schema = "graph")]
fn all(filters: Vec<pgrx::JsonB>) -> pgrx::JsonB {
    let mut merged = serde_json::Map::new();
    for filter in filters {
        let Some(where_clause) = filter
            .0
            .get("node")
            .and_then(|node| node.get("where"))
            .or_else(|| filter.0.get("where"))
            .and_then(|value| value.as_object())
        else {
            continue;
        };
        for (column, predicate) in where_clause {
            merged.insert(column.clone(), predicate.clone());
        }
    }
    pgrx::JsonB(serde_json::json!({ "where": merged }))
}

/// Unregister a table from graph indexing.
///
/// The graph must be rebuilt after removal.
///
/// See: `docs/user_guide/schema-registration.mdx`
#[pg_extern(schema = "graph")]
fn remove_table(table_name: pgrx::pg_sys::Oid) {
    with_panic_boundary("remove_table()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let table = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        Spi::run_with_args(
            "DELETE FROM graph._registered_tables WHERE table_name = $1",
            &[table.clone().into()],
        )
        .unwrap_or_else(|e| {
            pgrx::error!("graph.remove_table() failed: {}", e);
        });
        // Also remove associated filter columns
        Spi::run_with_args(
            "DELETE FROM graph._registered_filter_columns WHERE table_name = $1",
            &[table.clone().into()],
        )
        .ok();
        Spi::run_with_args(
            "DELETE FROM graph._registered_edges WHERE from_table = $1 OR to_table = $1",
            &[table.clone().into()],
        )
        .ok();
        pgrx::notice!(
            "graph: unregistered table {}. Call graph.build() to rebuild.",
            table
        );
    });
}

/// Unregister an edge relationship by label.
///
/// The graph must be rebuilt after removal.
///
/// See: `docs/user_guide/schema-registration.mdx`
#[pg_extern(schema = "graph")]
fn remove_edge(label: &str) {
    with_panic_boundary("remove_edge()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        Spi::run_with_args(
            "DELETE FROM graph._registered_edges WHERE label = $1",
            &[label.into()],
        )
        .unwrap_or_else(|e| {
            pgrx::error!("graph.remove_edge() failed: {}", e);
        });
        pgrx::notice!(
            "graph: unregistered edge '{}'. Call graph.build() to rebuild.",
            label
        );
    });
}

/// Estimate RAM requirements without building the graph.
///
/// Returns projected node count, edge count, and memory usage based on
/// `pg_class.reltuples` estimates from registered tables.
///
/// See: `docs/user_guide/api-reference.mdx`
#[pg_extern(schema = "graph")]
fn estimate() -> TableIterator<
    'static,
    (
        name!(estimated_nodes, i64),
        name!(estimated_edges, i64),
        name!(estimated_memory_mb, f64),
        name!(memory_limit_mb, i32),
        name!(fits_in_memory, bool),
    ),
> {
    with_panic_boundary("estimate()", || {
        let (tables, edges, _filter_columns) = read_catalog().unwrap_or_else(|err| err.report());
        let mut est_nodes: i64 = 0;
        let mut est_edges: i64 = 0;
        let mut table_counts = std::collections::HashMap::new();

        for table in &tables {
            let count = cached_estimated_table_rows(&mut table_counts, &table.table_name);
            est_nodes += count;
        }

        for edge in &edges {
            let count = cached_estimated_table_rows(&mut table_counts, &edge.from_table);
            let multiplier = if edge.bidirectional { 2 } else { 1 };
            est_edges += count * multiplier;
        }

        // Memory formula: graph topology plus resolution index.
        // NodeStore estimate: table OID + active bit + average primary-key bytes.
        // EdgeStore estimate: forward offsets plus target/type arrays.
        // ResolutionIndex estimate: 16 bytes/node.
        let node_bytes = est_nodes as f64 * (44.0 + 16.0);
        let edge_bytes = (est_nodes as f64 * 4.0) + (est_edges as f64 * 5.0);
        let est_memory_mb = (node_bytes + edge_bytes) / 1_048_576.0;

        let limit = config::MEMORY_LIMIT_MB.get();
        let fits = est_memory_mb <= limit as f64;

        TableIterator::new(vec![(est_nodes, est_edges, est_memory_mb, limit, fits)])
    })
}

/// Apply pending durable sync-log rows, plus any legacy sync-buffer rows, to
/// the backend-local graph.
///
/// See: `docs/user_guide/sync-and-maintenance.mdx`
#[pg_extern(schema = "graph")]
fn apply_sync() -> TableIterator<
    'static,
    (
        name!(inserts_applied, i64),
        name!(updates_applied, i64),
        name!(deletes_applied, i64),
    ),
> {
    with_panic_boundary("apply_sync()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let stats = apply_sync_internal().unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(stats.inserts, stats.updates, stats.deletes)])
    })
}

/// Vacuum the graph by rebuilding from source tables.
///
/// The CSR is immutable, so reclaiming tombstones and merging edge overlays
/// requires reconstructing the active engine.
///
/// **Double memory tax:** During vacuum, both the old and new engine
/// exist in memory simultaneously until the swap completes. Ensure
/// `graph.memory_limit_mb` has ≥2× headroom.
///
/// See: `docs/user_guide/sync-and-maintenance.mdx`
#[pg_extern(schema = "graph")]
fn vacuum() -> TableIterator<
    'static,
    (
        name!(nodes_before, i64),
        name!(nodes_after, i64),
        name!(tombstones_removed, i64),
        name!(edges_rebuilt, i64),
        name!(vacuum_time_ms, f64),
    ),
> {
    with_panic_boundary("vacuum()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let result = execute_vacuum(false).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            result.nodes_before,
            result.nodes_after,
            result.tombstones_removed,
            result.edges_rebuilt,
            result.vacuum_time_ms,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn maintenance(
    concurrently: default!(bool, false),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(status, String),
        name!(sync_rows_applied, Option<i64>),
        name!(nodes_after, Option<i64>),
        name!(edges_after, Option<i64>),
        name!(vacuum_time_ms, Option<f64>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("maintenance()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        if concurrently {
            let job_id = create_maintenance_job().unwrap_or_else(|err| err.report());
            if let Err(err) = launch_maintenance_worker(&job_id) {
                let _ = update_maintenance_job_failed(&job_id, &err.to_string());
                err.report();
            }
            let row = maintenance_job_row(&job_id)
                .unwrap_or_else(|err| err.report())
                .unwrap_or(MaintenanceJobRow {
                    job_id,
                    status: JobStatus::Queued.as_str().to_string(),
                    sync_rows_applied: None,
                    nodes_after: None,
                    edges_after: None,
                    vacuum_time_ms: None,
                    progress_phase: JobStatus::Queued.as_str().to_string(),
                    progress_message: Some("queued for background maintenance".to_string()),
                    started_at: None,
                    finished_at: None,
                    error: None,
                });
            return TableIterator::new(vec![(
                row.job_id,
                row.status,
                row.sync_rows_applied,
                row.nodes_after,
                row.edges_after,
                row.vacuum_time_ms,
                row.error,
            )]);
        }

        let result = execute_maintenance_rebuild(true).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            "00000000-0000-0000-0000-000000000000".to_string(),
            JobStatus::Completed.as_str().to_string(),
            Some(result.sync_rows_applied),
            Some(result.nodes_after),
            Some(result.edges_after),
            Some(result.vacuum_time_ms),
            None,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn maintenance_status(
    job_id: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(status, String),
        name!(sync_rows_applied, Option<i64>),
        name!(nodes_after, Option<i64>),
        name!(edges_after, Option<i64>),
        name!(vacuum_time_ms, Option<f64>),
        name!(progress_phase, String),
        name!(progress_message, Option<String>),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("maintenance_status()", || {
        if let Some(job_id) = job_id {
            if let Some(row) = maintenance_job_row(job_id).unwrap_or_else(|err| err.report()) {
                return TableIterator::new(vec![(
                    row.job_id,
                    row.status,
                    row.sync_rows_applied,
                    row.nodes_after,
                    row.edges_after,
                    row.vacuum_time_ms,
                    row.progress_phase,
                    row.progress_message,
                    row.started_at,
                    row.finished_at,
                    row.error,
                )]);
            }
            return TableIterator::new(vec![(
                job_id.to_string(),
                "not_found".to_string(),
                None,
                None,
                None,
                None,
                "not_found".to_string(),
                None,
                None,
                None,
                None,
            )]);
        }

        let rows = Spi::connect(|client| {
            let selected = client.select(
                "SELECT job_id, status, sync_rows_applied, nodes_after, edges_after,
                        vacuum_time_ms, progress_phase, progress_message,
                        started_at, finished_at, error
                 FROM graph._maintenance_jobs
                 ORDER BY created_at DESC
                 LIMIT 50",
                None,
                &[],
            )?;
            let mut out = Vec::new();
            for row in selected {
                out.push((
                    row.get::<String>(1)?.unwrap_or_default(),
                    row.get::<String>(2)?
                        .unwrap_or_else(|| "not_found".to_string()),
                    row.get::<i64>(3)?,
                    row.get::<i64>(4)?,
                    row.get::<i64>(5)?,
                    row.get::<f64>(6)?,
                    row.get::<String>(7)?
                        .unwrap_or_else(|| "unknown".to_string()),
                    row.get::<String>(8)?,
                    row.get::<TimestampWithTimeZone>(9)?,
                    row.get::<TimestampWithTimeZone>(10)?,
                    row.get::<String>(11)?,
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(out)
        })
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("maintenance status read failed: {}", err))
                .report()
        });
        TableIterator::new(rows)
    })
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_run_build_job")]
fn test_run_build_job(build_id: &str) -> Option<String> {
    with_panic_boundary("_test_run_build_job()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        run_build_job(build_id).err().map(|err| {
            let message = err.to_string();
            if let Err(record_err) = update_build_job_failed(build_id, &message) {
                pgrx::warning!(
                    "graph test build job {} failed and failure status could not be recorded: {}",
                    build_id,
                    record_err
                );
            }
            message
        })
    })
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_run_maintenance_job")]
fn test_run_maintenance_job(job_id: &str) -> Option<String> {
    with_panic_boundary("_test_run_maintenance_job()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        run_maintenance_job(job_id).err().map(|err| {
            let message = err.to_string();
            if let Err(record_err) = update_maintenance_job_failed(job_id, &message) {
                pgrx::warning!(
                    "graph test maintenance job {} failed and failure status could not be recorded: {}",
                    job_id,
                    record_err
                );
            }
            message
        })
    })
}

fn cached_estimated_table_rows(
    table_counts: &mut std::collections::HashMap<String, i64>,
    table_name: &str,
) -> i64 {
    if let Some(count) = table_counts.get(table_name) {
        return *count;
    }
    let count = catalog::estimated_table_rows(table_name).unwrap_or(0);
    table_counts.insert(table_name.to_string(), count);
    count
}

/// Enable trigger-based sync for all registered tables.
///
/// Ensures sync catalog tables exist and attaches triggers that write to
/// `graph._sync_log`.
#[pg_extern(schema = "graph")]
fn enable_sync() {
    with_panic_boundary("enable_sync()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let installed = install_sync_triggers().unwrap_or_else(|err| err.report());
        pgrx::notice!("graph: sync enabled for {} tables", installed);
    });
}
