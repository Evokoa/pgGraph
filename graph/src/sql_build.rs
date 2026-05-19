//! SQL-layer build, vacuum, and maintenance execution helpers.

use crate::api_types::{BuildExecutionResult, MaintenanceExecutionResult, VacuumExecutionResult};
use crate::catalog::{catalog_fingerprint, read_catalog, table_oid_from_name};
use crate::sql_sync::{
    current_sync_mode, install_sync_triggers, max_sync_log_id, remove_sync_triggers,
};
use crate::{acl, builder, config, engine, persistence, safety, ENGINE};
use pgrx::prelude::*;

fn persist_and_reload_engine(
    operation: &str,
    source: &engine::Engine,
) -> safety::GraphResult<engine::Engine> {
    let path = persistence::graph_file_path()?;
    persistence::write_graph_file(source, &path).map_err(|err| {
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

    let mut loaded = persistence::load_graph_file(&path).map_err(|err| {
        safety::GraphError::Internal(format!(
            "graph.{operation}(): persisted mmap reload failed: {err}"
        ))
    })?;
    loaded.catalog_fingerprint = source.catalog_fingerprint;
    loaded.is_read_only = source.is_read_only;
    loaded.read_only_reason = source.read_only_reason;
    loaded.sync_status = source.sync_status;
    loaded.last_build = source.last_build;
    loaded.last_vacuum = source.last_vacuum;
    loaded.applied_sync_id = source.applied_sync_id;
    loaded.needs_vacuum = source.needs_vacuum;

    Ok(loaded)
}

pub(crate) fn execute_build(force_persist: bool) -> safety::GraphResult<BuildExecutionResult> {
    let start = std::time::Instant::now();
    let sync_mode = current_sync_mode()?;

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
        });
    }

    check_build_acls_result(&tables, &edges)?;
    let force_read_only = guard_build_memory_headroom(&tables, &edges)?;

    let mut new_engine = builder::build_graph(&tables, &edges, &filter_columns)?;
    if force_read_only {
        new_engine.mark_read_only(engine::ReadOnlyReason::MemoryLimit);
    }
    new_engine.catalog_fingerprint = Some(catalog_fingerprint(&tables, &edges, &filter_columns));
    new_engine.applied_sync_id = max_sync_log_id()?;

    let nodes_loaded = new_engine.node_store.node_count() as i64;
    let edges_loaded = new_engine.edge_store.edge_count() as i64;
    let build_time_ms = start.elapsed().as_secs_f64() * 1000.0;
    let memory_used_mb = new_engine.estimated_memory_used_mb();

    let persisted_engine = if force_persist || config::PERSIST_ON_BUILD.get() {
        Some(persist_and_reload_engine("build", &new_engine)?)
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
    })
}

pub(crate) fn execute_maintenance_rebuild(
    force_persist: bool,
) -> safety::GraphResult<MaintenanceExecutionResult> {
    let previous_applied_sync_id = ENGINE.with(|e| e.borrow().applied_sync_id);
    let build = execute_build(force_persist)?;
    let after = max_sync_log_id()?;
    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.needs_vacuum = false;
        eng.last_vacuum = Some(pgrx::datetime::transaction_timestamp());
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

    let mut new_engine = builder::build_graph(&tables, &edges, &filter_columns)?;
    if force_read_only {
        new_engine.mark_read_only(engine::ReadOnlyReason::MemoryLimit);
    }
    new_engine.catalog_fingerprint = Some(catalog_fingerprint(&tables, &edges, &filter_columns));
    new_engine.applied_sync_id = max_sync_log_id()?;
    new_engine.needs_vacuum = false;

    let nodes_after = new_engine.node_store.node_count() as i64;
    let edges_rebuilt = new_engine.edge_store.edge_count() as i64;
    let tombstones_removed = nodes_before - active_before;
    new_engine.last_vacuum = Some(pgrx::datetime::transaction_timestamp());

    let persisted_engine = if force_persist || config::PERSIST_ON_BUILD.get() {
        Some(persist_and_reload_engine("vacuum", &new_engine)?)
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
    let acquired = Spi::get_one::<bool>("SELECT pg_try_advisory_xact_lock(1918928211, 1735552871)")
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
