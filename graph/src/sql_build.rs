//! SQL-layer build, vacuum, and maintenance execution helpers.

use crate::api_types::{BuildExecutionResult, MaintenanceExecutionResult, VacuumExecutionResult};
use crate::catalog::{catalog_fingerprint, read_catalog, table_oid_from_name};
use crate::sql_sync::{
    current_sync_mode, install_sync_triggers, max_sync_log_id, remove_sync_triggers,
};
use crate::{acl, builder, config, engine, persistence, safety, ENGINE};
use pgrx::prelude::*;

/// Advisory lock namespace for pgGraph build/vacuum operations.
///
/// The two-int PostgreSQL advisory lock API is used so the key remains stable
/// across 32-bit and 64-bit platforms. The class id `0x7260_8553` is the
/// reserved pgGraph advisory-lock namespace. The object id `0x6772_6f67`
/// identifies build/vacuum exclusion within that namespace and must remain
/// stable so concurrent extension versions do not take different locks.
pub(crate) const BUILD_LOCK_CLASS_ID: i32 = 1_918_928_211;
pub(crate) const BUILD_LOCK_OBJECT_ID: i32 = 1_735_552_871;

pub(crate) fn build_lock_query() -> String {
    format!("SELECT pg_try_advisory_xact_lock({BUILD_LOCK_CLASS_ID}, {BUILD_LOCK_OBJECT_ID})")
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

fn persist_and_reload_engine(
    operation: &str,
    source: &engine::Engine,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<engine::Engine> {
    let path = persistence::graph_file_path()?;
    report_progress(
        progress,
        "persisting",
        "writing and fsyncing graph artifact",
    )?;
    persistence::write_graph_file_with_interrupt_checks(source, &path).map_err(|err| {
        safety::GraphError::Internal(format!("graph.{operation}(): persistence failed: {err}"))
    })?;

    let file_size = std::fs::metadata(&path)
        .map(|m| m.len() as f64 / 1_048_576.0)
        .unwrap_or(0.0);
    pgrx::log!(
        "graph: persisted to {} ({:.1} MB)",
        path.display(),
        file_size
    );

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
    loaded.inherit_runtime_metadata_from(source);

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
            sync_mode: "manual".to_string(),
            projection_mode: projection_mode.as_str().to_string(),
        });
    }

    check_build_acls_result(&tables, &edges)?;
    let force_read_only = guard_build_memory_headroom(&tables, &edges)?;

    report_progress(progress, build_phase, build_message)?;
    let mut new_engine =
        build_replacement_engine(&tables, &edges, &filter_columns, force_read_only)?;
    new_engine.set_projection_mode(projection_mode);

    let nodes_loaded = new_engine.node_store.node_count() as i64;
    let edges_loaded = new_engine.edge_store.edge_count() as i64;
    let build_time_ms = start.elapsed().as_secs_f64() * 1000.0;
    let memory_used_mb = new_engine.estimated_memory_used_mb();

    let persisted_engine = if force_persist || config::PERSIST_ON_BUILD.get() {
        Some(persist_and_reload_engine("build", &new_engine, progress)?)
    } else {
        None
    };

    new_engine.finalize_resolution();

    ENGINE.with(|e| {
        *e.borrow_mut() = persisted_engine.unwrap_or(new_engine);
    });

    match sync_mode {
        config::SyncMode::Manual => {
            remove_sync_triggers()?;
        }
        config::SyncMode::Trigger => {
            install_sync_triggers()?;
        }
        config::SyncMode::Wal => unreachable!("current_sync_mode rejects reserved wal mode"),
    }

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
    check_build_acls_result(&tables, &edges)?;
    let force_read_only = guard_build_memory_headroom(&tables, &edges)?;

    let mut new_engine =
        build_replacement_engine(&tables, &edges, &filter_columns, force_read_only)?;

    let nodes_after = new_engine.node_store.node_count() as i64;
    let edges_rebuilt = new_engine.edge_store.edge_count() as i64;
    let tombstones_removed = nodes_before - active_before;
    new_engine.mark_vacuum_complete(Some(pgrx::datetime::transaction_timestamp()));

    let persisted_engine = if force_persist || config::PERSIST_ON_BUILD.get() {
        let mut progress = |_, _| Ok(());
        Some(persist_and_reload_engine(
            "vacuum",
            &new_engine,
            &mut progress,
        )?)
    } else {
        None
    };

    new_engine.finalize_resolution();

    ENGINE.with(|e| {
        *e.borrow_mut() = persisted_engine.unwrap_or(new_engine);
    });

    Ok(VacuumExecutionResult {
        nodes_before,
        nodes_after,
        tombstones_removed,
        edges_rebuilt,
        vacuum_time_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

pub(crate) fn acquire_build_lock() -> safety::GraphResult<()> {
    let acquired = Spi::get_one::<bool>(&build_lock_query())
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

pub(crate) fn guard_build_memory_headroom(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
) -> safety::GraphResult<bool> {
    let estimate = builder::estimate_graph_memory(tables, edges)?;
    let existing_mb = ENGINE.with(|e| {
        let eng = e.borrow();
        if eng.built {
            eng.estimated_memory_used_mb()
        } else {
            0.0
        }
    });
    let total_mb = estimate.memory_mb + existing_mb;
    let limit_mb = config::MEMORY_LIMIT_MB.get() as f64;
    if total_mb <= limit_mb {
        return Ok(false);
    }

    match config::oom_action() {
        config::OomAction::ReadOnly => {
            pgrx::warning!(
                "graph: build headroom estimate ({:.0} MB new + {:.0} MB existing = {:.0} MB) exceeds limit ({:.0} MB). Building in read-only mode.",
                estimate.memory_mb,
                existing_mb,
                total_mb,
                limit_mb
            );
            Ok(true)
        }
        config::OomAction::Error => Err(safety::GraphError::Oom {
            used_mb: existing_mb.ceil() as u64,
            need_mb: estimate.memory_mb.ceil() as u64,
            limit_mb: config::MEMORY_LIMIT_MB.get() as u64,
        }),
    }
}

pub(crate) fn check_build_acls_result(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
) -> safety::GraphResult<()> {
    for table in tables {
        let oid = table_oid_from_name(&table.table_name)?;
        acl::check_table_acl(oid)?;
    }
    for edge in edges {
        let from_oid = table_oid_from_name(&edge.from_table)?;
        let to_oid = table_oid_from_name(&edge.to_table)?;
        acl::check_table_acl(from_oid)?;
        acl::check_table_acl(to_oid)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_lock_query, validate_projection_mode_enabled, BUILD_LOCK_CLASS_ID,
        BUILD_LOCK_OBJECT_ID,
    };
    use crate::config::ProjectionMode;

    #[test]
    fn build_lock_query_uses_named_advisory_lock_keys() {
        assert_eq!(BUILD_LOCK_CLASS_ID, 1_918_928_211);
        assert_eq!(BUILD_LOCK_OBJECT_ID, 1_735_552_871);
        assert_eq!(
            build_lock_query(),
            "SELECT pg_try_advisory_xact_lock(1918928211, 1735552871)"
        );
    }

    #[test]
    fn csr_readonly_projection_mode_is_always_allowed() {
        validate_projection_mode_enabled(ProjectionMode::CsrReadonly)
            .expect("csr_readonly should be allowed");
    }
}
