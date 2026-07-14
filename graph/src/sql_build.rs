//! SQL-layer build, vacuum, and maintenance execution helpers.

use crate::api_types::{BuildExecutionResult, MaintenanceExecutionResult, VacuumExecutionResult};
use crate::catalog::{catalog_fingerprint, read_catalog, selected_or_default_graph_metadata};
use crate::sql_sync::{
    current_sync_mode, install_sync_triggers, max_sync_log_id, remove_sync_triggers,
};
use crate::{acl, builder, config, engine, persistence, safety, ENGINE};
use pgrx::prelude::*;

/// Advisory lock namespace for pgGraph build/vacuum operations.
///
/// The two-int PostgreSQL advisory lock API is used so the key remains stable
/// across 32-bit and 64-bit platforms. The class id `0x7260_8553` is the
/// reserved pgGraph advisory-lock namespace. The object id is derived from the
/// selected graph id so builds for the same graph serialize while independent
/// graphs can proceed without sharing the old global build lock.
pub(crate) const BUILD_LOCK_CLASS_ID: i32 = 1_918_928_211;

pub(crate) fn build_lock_query_for_graph(graph_id: &str) -> String {
    format!(
        "SELECT pg_try_advisory_xact_lock({BUILD_LOCK_CLASS_ID}, {})",
        graph_build_lock_object_id(graph_id)
    )
}

fn graph_build_lock_object_id(graph_id: &str) -> i32 {
    let mut hash = 0x811c_9dc5_u32;
    for byte in graph_id.bytes().filter(|byte| *byte != b'-') {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    i32::from_ne_bytes(hash.to_ne_bytes())
}

pub(crate) type ProgressCallback<'a> =
    dyn FnMut(&'static str, &'static str) -> safety::GraphResult<()> + 'a;

fn report_progress(
    progress: &mut ProgressCallback<'_>,
    phase: &'static str,
    message: &'static str,
) -> safety::GraphResult<()> {
    progress(phase, message)
}

struct EngineRuntimeMetadata {
    catalog_fingerprint: Option<u64>,
    is_read_only: bool,
    read_only_reason: Option<engine::ReadOnlyReason>,
    sync_status: engine::SyncStatus,
    last_build: Option<pgrx::prelude::TimestampWithTimeZone>,
    last_vacuum: Option<pgrx::prelude::TimestampWithTimeZone>,
    applied_sync_id: i64,
    needs_vacuum: bool,
    projection_mode: config::ProjectionMode,
}

impl EngineRuntimeMetadata {
    fn capture(source: &engine::Engine) -> Self {
        Self {
            catalog_fingerprint: source.catalog_fingerprint,
            is_read_only: source.is_read_only,
            read_only_reason: source.read_only_reason,
            sync_status: source.sync_status,
            last_build: source.last_build,
            last_vacuum: source.last_vacuum,
            applied_sync_id: source.applied_sync_id,
            needs_vacuum: source.needs_vacuum,
            projection_mode: source.projection_mode,
        }
    }

    fn apply_to(self, target: &mut engine::Engine) {
        target.catalog_fingerprint = self.catalog_fingerprint;
        target.is_read_only = self.is_read_only;
        target.read_only_reason = self.read_only_reason;
        target.sync_status = self.sync_status;
        target.last_build = self.last_build;
        target.last_vacuum = self.last_vacuum;
        target.applied_sync_id = self.applied_sync_id;
        target.needs_vacuum = self.needs_vacuum;
        target.projection_mode = self.projection_mode;
    }
}

fn persist_and_reload_engine(
    operation: &str,
    source: engine::Engine,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<engine::Engine> {
    let path = persistence::graph_file_path()?;
    report_progress(
        progress,
        "persisting",
        "writing and fsyncing graph artifact",
    )?;
    persistence::write_graph_file_with_interrupt_checks(&source, &path).map_err(|err| {
        safety::GraphError::Internal(format!("graph.{operation}(): persistence failed: {err}"))
    })?;
    let projection_root = persistence::projection_manifest_root(&path);
    if source.projection_mode == config::ProjectionMode::MutableOverlay
        || crate::projection::recovery::has_projection_generation(&projection_root)?
    {
        crate::projection::recovery::publish_rebuilt_base_manifest(&path, source.applied_sync_id)
            .map_err(|err| {
            safety::GraphError::Internal(format!(
                "graph.{operation}(): projection manifest rebase failed: {err}"
            ))
        })?;
    }

    let file_size = std::fs::metadata(&path)
        .map(|m| m.len() as f64 / 1_048_576.0)
        .unwrap_or(0.0);
    pgrx::log!(
        "graph: persisted to {} ({:.1} MB)",
        path.display(),
        file_size
    );

    let metadata = EngineRuntimeMetadata::capture(&source);
    drop(source);

    report_progress(
        progress,
        "validating_persistence",
        "validating persisted graph artifact",
    )?;
    let mut loaded = persistence::load_graph_file(&path).map_err(|err| {
        safety::GraphError::Internal(format!(
            "graph.{operation}(): persisted mmap reload failed: {err}"
        ))
    })?;
    metadata.apply_to(&mut loaded);

    Ok(loaded)
}

pub(crate) fn execute_build(force_persist: bool) -> safety::GraphResult<BuildExecutionResult> {
    let mut progress = |_, _| Ok(());
    let mode = configured_projection_mode()?;
    execute_build_with_mode_and_progress(force_persist, mode, &mut progress)
}

pub(crate) fn execute_build_with_mode(
    force_persist: bool,
    projection_mode: config::ProjectionMode,
) -> safety::GraphResult<BuildExecutionResult> {
    let mut progress = |_, _| Ok(());
    execute_build_with_mode_and_progress(force_persist, projection_mode, &mut progress)
}

pub(crate) fn execute_build_with_mode_and_progress(
    force_persist: bool,
    projection_mode: config::ProjectionMode,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<BuildExecutionResult> {
    execute_build_inner(
        force_persist,
        projection_mode,
        ProjectionModeGate::CheckGuc,
        "building",
        "building graph from registered source tables",
        progress,
    )
}

pub(crate) fn execute_build_with_prevalidated_mode_and_progress(
    force_persist: bool,
    projection_mode: config::ProjectionMode,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<BuildExecutionResult> {
    execute_build_inner(
        force_persist,
        projection_mode,
        ProjectionModeGate::Prevalidated,
        "building",
        "building graph from registered source tables",
        progress,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionModeGate {
    CheckGuc,
    Prevalidated,
}

fn execute_build_inner(
    force_persist: bool,
    projection_mode: config::ProjectionMode,
    mode_gate: ProjectionModeGate,
    build_phase: &'static str,
    build_message: &'static str,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<BuildExecutionResult> {
    let start = std::time::Instant::now();
    let graph = selected_or_default_graph_metadata()?;
    let sync_mode = current_sync_mode()?;
    if mode_gate == ProjectionModeGate::CheckGuc {
        validate_projection_mode_enabled(projection_mode)?;
    }

    acquire_build_lock()?;
    let (tables, edges, filter_columns) = read_catalog()?;

    if tables.is_empty() {
        pgrx::warning!("graph.build(): no tables registered. Call graph.add_table() first.");
        return Ok(BuildExecutionResult {
            nodes_loaded: 0,
            edges_loaded: 0,
            build_time_ms: 0.0,
            memory_used_mb: 0.0,
            sync_mode: sync_mode.as_str().to_string(),
            projection_mode: projection_mode.as_str().to_string(),
        });
    }

    prepare_source_snapshot_boundary(sync_mode, "build")?;

    check_build_acls_result(&tables, &edges)?;
    let memory_plan = guard_build_memory_headroom(&tables, &edges)?;

    report_progress(progress, build_phase, build_message)?;
    apply_build_memory_plan(memory_plan)?;
    let mut new_engine = build_replacement_engine(
        &tables,
        &edges,
        &filter_columns,
        memory_plan.force_read_only,
    )?;
    new_engine.set_projection_mode(projection_mode);

    let nodes_loaded = new_engine.node_store.node_count() as i64;
    let edges_loaded = new_engine.edge_store.edge_count() as i64;
    let build_time_ms = start.elapsed().as_secs_f64() * 1000.0;
    let memory_used_mb = new_engine.estimated_memory_used_mb();

    let new_engine = if force_persist || config::PERSIST_ON_BUILD.get() {
        persist_and_reload_engine("build", new_engine, progress)?
    } else {
        new_engine.finalize_resolution();
        new_engine
    };

    ENGINE.with(|e| {
        *e.borrow_mut() = new_engine;
    });
    crate::runtime_state::mark_loaded_graph(&graph);

    Ok(BuildExecutionResult {
        nodes_loaded,
        edges_loaded,
        build_time_ms,
        memory_used_mb,
        sync_mode: sync_mode.as_str().to_string(),
        projection_mode: projection_mode.as_str().to_string(),
    })
}

pub(crate) fn execute_maintenance_rebuild(
    force_persist: bool,
) -> safety::GraphResult<MaintenanceExecutionResult> {
    let mut progress = |_, _| Ok(());
    execute_maintenance_rebuild_with_progress(force_persist, &mut progress)
}

pub(crate) fn execute_maintenance_rebuild_with_progress(
    force_persist: bool,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<MaintenanceExecutionResult> {
    let previous_applied_sync_id = ENGINE.with(|e| e.borrow().applied_sync_id);
    let build = execute_build_inner(
        force_persist,
        configured_projection_mode()?,
        ProjectionModeGate::CheckGuc,
        "rebuilding",
        "rebuilding graph for maintenance",
        progress,
    )?;
    let after = max_sync_log_id()?;
    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.mark_vacuum_complete(Some(pgrx::datetime::transaction_timestamp()));
    });
    Ok(MaintenanceExecutionResult {
        sync_rows_applied: after.saturating_sub(previous_applied_sync_id),
        nodes_after: build.nodes_loaded,
        edges_after: build.edges_loaded,
        vacuum_time_ms: build.build_time_ms,
    })
}

pub(crate) fn execute_vacuum(force_persist: bool) -> safety::GraphResult<VacuumExecutionResult> {
    let start = std::time::Instant::now();
    acquire_build_lock()?;
    let sync_mode = current_sync_mode()?;

    let (nodes_before, active_before) = ENGINE.with(|e| {
        let eng = e.borrow();
        if !eng.built {
            return (0i64, 0i64);
        }
        (
            eng.node_store.node_count() as i64,
            eng.node_store.active_count() as i64,
        )
    });

    if nodes_before == 0 {
        return Ok(VacuumExecutionResult {
            nodes_before: 0,
            nodes_after: 0,
            tombstones_removed: 0,
            edges_rebuilt: 0,
            vacuum_time_ms: 0.0,
        });
    }

    let (tables, edges, filter_columns) = read_catalog()?;
    prepare_source_snapshot_boundary(sync_mode, "vacuum")?;
    check_build_acls_result(&tables, &edges)?;
    let memory_plan = guard_build_memory_headroom(&tables, &edges)?;
    apply_build_memory_plan(memory_plan)?;

    let mut new_engine = build_replacement_engine(
        &tables,
        &edges,
        &filter_columns,
        memory_plan.force_read_only,
    )?;

    let nodes_after = new_engine.node_store.node_count() as i64;
    let edges_rebuilt = new_engine.edge_store.edge_count() as i64;
    let tombstones_removed = nodes_before - active_before;
    new_engine.mark_vacuum_complete(Some(pgrx::datetime::transaction_timestamp()));

    let new_engine = if force_persist || config::PERSIST_ON_BUILD.get() {
        let mut progress = |_, _| Ok(());
        persist_and_reload_engine("vacuum", new_engine, &mut progress)?
    } else {
        new_engine.finalize_resolution();
        new_engine
    };

    ENGINE.with(|e| {
        *e.borrow_mut() = new_engine;
    });

    Ok(VacuumExecutionResult {
        nodes_before,
        nodes_after,
        tombstones_removed,
        edges_rebuilt,
        vacuum_time_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

fn prepare_source_snapshot_boundary(
    sync_mode: config::SyncMode,
    operation: &str,
) -> safety::GraphResult<()> {
    // Reconcile sync triggers before taking the source snapshot. Trigger DDL
    // establishes a clean table-lock boundary for older definitions; current
    // definitions acquire the writer barrier first and refresh only function
    // bodies, keeping active-writer contention fail-fast.
    let writer_barrier_held = match sync_mode {
        config::SyncMode::Manual => {
            remove_sync_triggers()?;
            false
        }
        config::SyncMode::Trigger => {
            let current_barrier = crate::sql_sync::sync_writer_barrier_triggers_current()?;
            if current_barrier {
                crate::sql_sync::acquire_sync_writer_barrier()?;
                crate::sql_sync::ensure_no_current_transaction_sync_rows(0)?;
            }
            let installed = install_sync_triggers()?;
            pgrx::warning!(
                "graph.{operation}(): graph.sync_mode = 'trigger' installed graph sync triggers on {} registered table(s); set graph.sync_mode = 'manual' before graph.{operation}() to opt out",
                installed
            );
            current_barrier
        }
        config::SyncMode::Wal => unreachable!("current_sync_mode rejects reserved wal mode"),
    };
    if !writer_barrier_held {
        crate::sql_sync::acquire_sync_writer_barrier()?;
        crate::sql_sync::ensure_no_current_transaction_sync_rows(0)?;
    }
    Ok(())
}

pub(crate) fn acquire_build_lock() -> safety::GraphResult<()> {
    let graph = selected_or_default_graph_metadata()?;
    let acquired = Spi::get_one::<bool>(&build_lock_query_for_graph(&graph.graph_id))
        .map_err(|err| {
            safety::GraphError::Internal(format!(
                "could not acquire build/vacuum advisory lock: {}",
                err
            ))
        })?
        .unwrap_or(false);
    if acquired {
        Ok(())
    } else {
        Err(safety::GraphError::BuildLocked)
    }
}

fn build_replacement_engine(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filter_columns: &[builder::RegisteredFilterColumn],
    force_read_only: bool,
) -> safety::GraphResult<engine::Engine> {
    let mut new_engine = builder::build_graph(tables, edges, filter_columns)?;
    if force_read_only {
        new_engine.mark_read_only(engine::ReadOnlyReason::MemoryLimit);
    }
    new_engine.set_catalog_fingerprint(catalog_fingerprint(tables, edges, filter_columns));
    new_engine.record_applied_sync_id(max_sync_log_id()?);
    Ok(new_engine)
}

pub(crate) fn configured_projection_mode() -> safety::GraphResult<config::ProjectionMode> {
    config::default_projection_mode().ok_or_else(|| safety::GraphError::InvalidFilter {
        reason: format!(
            "unsupported graph.default_projection_mode '{}'; expected 'csr_readonly' or 'mutable_overlay'",
            config::DEFAULT_PROJECTION_MODE
                .get()
                .as_ref()
                .and_then(|c| c.to_str().ok())
                .unwrap_or("csr_readonly")
        ),
    })
}

pub(crate) fn validate_projection_mode_enabled(
    projection_mode: config::ProjectionMode,
) -> safety::GraphResult<()> {
    if matches!(projection_mode, config::ProjectionMode::MutableOverlay)
        && !config::MUTABLE_ENABLED.get()
    {
        return Err(safety::GraphError::UnsupportedOperation {
            operation: "graph.build(mode => 'mutable_overlay')".to_string(),
            reason: "graph.mutable_enabled is off".to_string(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuildMemoryPlan {
    pub(crate) force_read_only: bool,
    pub(crate) unload_existing: bool,
}

pub(crate) fn guard_build_memory_headroom(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
) -> safety::GraphResult<BuildMemoryPlan> {
    let estimate = builder::estimate_graph_memory(tables, edges)?;
    let existing_mb = ENGINE.with(|e| {
        let eng = e.borrow();
        if eng.built {
            eng.estimated_memory_used_mb()
        } else {
            0.0
        }
    });
    let build_peak_mb = conservative_build_peak_mb(estimate.memory_mb);
    let total_mb = build_peak_mb + existing_mb;
    let limit_mb = config::MEMORY_LIMIT_MB.get() as f64;
    let low_memory_build = config::LOW_MEMORY_BUILD.get();
    if low_memory_build && existing_mb > 0.0 && build_peak_mb <= limit_mb {
        pgrx::warning!(
            "graph: low-memory build unloading current backend graph before rebuild ({:.0} MB existing, {:.0} MB peak replacement, limit {:.0} MB).",
            existing_mb,
            build_peak_mb,
            limit_mb
        );
        return Ok(BuildMemoryPlan {
            force_read_only: false,
            unload_existing: true,
        });
    }
    if total_mb <= limit_mb {
        return Ok(BuildMemoryPlan {
            force_read_only: false,
            unload_existing: false,
        });
    }

    match config::oom_action() {
        config::OomAction::ReadOnly => {
            let unload_existing = low_memory_build && existing_mb > 0.0;
            pgrx::warning!(
                "graph: build headroom estimate ({:.0} MB peak new + {:.0} MB existing = {:.0} MB) exceeds limit ({:.0} MB). Building in read-only mode.",
                build_peak_mb,
                existing_mb,
                total_mb,
                limit_mb
            );
            Ok(BuildMemoryPlan {
                force_read_only: true,
                unload_existing,
            })
        }
        config::OomAction::Error => Err(safety::GraphError::Oom {
            used_mb: existing_mb.ceil() as u64,
            need_mb: build_peak_mb.ceil() as u64,
            limit_mb: config::MEMORY_LIMIT_MB.get() as u64,
        }),
    }
}

fn apply_build_memory_plan(plan: BuildMemoryPlan) -> safety::GraphResult<()> {
    if plan.unload_existing {
        ENGINE.with(|e| {
            *e.borrow_mut() = engine::Engine::new();
        });
        crate::runtime_state::clear_loaded_graph();
    }
    Ok(())
}

fn conservative_build_peak_mb(final_graph_mb: f64) -> f64 {
    const BUILD_OVERHEAD_MULTIPLIER: f64 = 1.25;
    const MIN_BUILD_OVERHEAD_MB: f64 = 64.0;

    (final_graph_mb * BUILD_OVERHEAD_MULTIPLIER).max(final_graph_mb + MIN_BUILD_OVERHEAD_MB)
}

pub(crate) fn check_build_acls_result(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
) -> safety::GraphResult<()> {
    for table in tables {
        let oid = table.table_oid;
        acl::check_table_acl(oid)?;
    }
    for edge in edges {
        let from_oid = edge.from_table_oid;
        let to_oid = edge.to_table_oid;
        acl::check_table_acl(from_oid)?;
        acl::check_table_acl(to_oid)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_lock_query_for_graph, conservative_build_peak_mb, validate_projection_mode_enabled,
        BUILD_LOCK_CLASS_ID,
    };
    use crate::config::ProjectionMode;

    #[test]
    fn build_lock_query_uses_named_advisory_lock_class() {
        assert_eq!(BUILD_LOCK_CLASS_ID, 1_918_928_211);
        assert!(
            build_lock_query_for_graph("00000000-0000-0000-0000-000000000001")
                .starts_with("SELECT pg_try_advisory_xact_lock(1918928211, ")
        );
    }

    #[test]
    fn graph_build_lock_query_is_stable_per_graph() {
        let graph_a = "00000000-0000-0000-0000-000000000001";
        let graph_b = "00000000-0000-0000-0000-000000000002";

        assert_eq!(
            build_lock_query_for_graph(graph_a),
            build_lock_query_for_graph(graph_a)
        );
        assert_ne!(
            build_lock_query_for_graph(graph_a),
            build_lock_query_for_graph(graph_b)
        );
    }

    #[test]
    fn csr_readonly_projection_mode_is_always_allowed() {
        validate_projection_mode_enabled(ProjectionMode::CsrReadonly)
            .expect("csr_readonly should be allowed");
    }

    #[test]
    fn build_peak_estimate_keeps_minimum_rebuild_overhead() {
        assert_eq!(conservative_build_peak_mb(100.0), 164.0);
    }

    #[test]
    fn build_peak_estimate_scales_large_graphs() {
        assert_eq!(conservative_build_peak_mb(1024.0), 1280.0);
    }
}
