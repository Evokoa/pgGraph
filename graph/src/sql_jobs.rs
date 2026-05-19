//! SQL-layer durable job tracking and background-worker launch helpers.

use crate::api_types::{
    BuildExecutionResult, BuildJobRow, MaintenanceExecutionResult, MaintenanceJobRow,
};
use crate::safety;
use crate::sql_build::{execute_build, execute_maintenance_rebuild};
use crate::sql_sync::current_sync_mode;
use pgrx::bgworkers::{BackgroundWorkerBuilder, BgWorkerStartTime};
use pgrx::prelude::*;

pub(crate) fn create_build_job() -> safety::GraphResult<String> {
    let sync_mode = current_sync_mode()?.as_str().to_string();
    Spi::get_one_with_args::<String>(
        "INSERT INTO graph._build_jobs (build_id, status, sync_mode)
         VALUES (gen_random_uuid()::text, 'queued', $1)
         RETURNING build_id",
        &[sync_mode.into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("build job creation failed: {}", err)))?
    .ok_or_else(|| safety::GraphError::Internal("build job creation returned no id".to_string()))
}

pub(crate) fn build_job_row(build_id: &str) -> safety::GraphResult<Option<BuildJobRow>> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT build_id, status, nodes_loaded, edges_loaded,
                    build_time_ms, memory_used_mb, sync_mode,
                    progress_phase, progress_message,
                    started_at, finished_at, error
             FROM graph._build_jobs
             WHERE build_id = $1",
            None,
            &[build_id.into()],
        )?;
        if let Some(row) = rows.into_iter().next() {
            return Ok::<_, pgrx::spi::SpiError>(Some(BuildJobRow {
                build_id: row
                    .get::<String>(1)?
                    .unwrap_or_else(|| build_id.to_string()),
                status: row
                    .get::<String>(2)?
                    .unwrap_or_else(|| "not_found".to_string()),
                nodes_loaded: row.get::<i64>(3)?,
                edges_loaded: row.get::<i64>(4)?,
                build_time_ms: row.get::<f64>(5)?,
                memory_used_mb: row.get::<f64>(6)?,
                sync_mode: row
                    .get::<String>(7)?
                    .unwrap_or_else(|| "manual".to_string()),
                progress_phase: row
                    .get::<String>(8)?
                    .unwrap_or_else(|| "unknown".to_string()),
                progress_message: row.get::<String>(9)?,
                started_at: row.get::<TimestampWithTimeZone>(10)?,
                finished_at: row.get::<TimestampWithTimeZone>(11)?,
                error: row.get::<String>(12)?,
            }));
        }
        Ok(None)
    })
    .map_err(|err| safety::GraphError::Internal(format!("build job read failed: {}", err)))
}

pub(crate) fn update_build_job_started(build_id: &str) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "UPDATE graph._build_jobs
         SET status = 'running',
             progress_phase = 'building',
             progress_message = 'building graph from registered source tables',
             started_at = COALESCE(started_at, now()),
             worker_pid = pg_backend_pid(),
             updated_at = now()
         WHERE build_id = $1 AND status = 'queued'",
        &[build_id.into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("build job start update failed: {}", err)))
}

pub(crate) fn update_build_job_completed(
    build_id: &str,
    result: &BuildExecutionResult,
) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "UPDATE graph._build_jobs
         SET status = 'completed',
             nodes_loaded = $2,
             edges_loaded = $3,
             build_time_ms = $4,
             memory_used_mb = $5,
             sync_mode = $6,
             progress_phase = 'completed',
             progress_message = 'build completed',
             finished_at = now(),
             updated_at = now(),
             error = NULL
         WHERE build_id = $1",
        &[
            build_id.into(),
            result.nodes_loaded.into(),
            result.edges_loaded.into(),
            result.build_time_ms.into(),
            result.memory_used_mb.into(),
            result.sync_mode.clone().into(),
        ],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("build job completion update failed: {}", err))
    })
}

pub(crate) fn update_build_job_failed(build_id: &str, error: &str) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "UPDATE graph._build_jobs
         SET status = 'failed',
             progress_phase = 'failed',
             progress_message = $2,
             finished_at = now(),
             updated_at = now(),
             error = $2
         WHERE build_id = $1",
        &[build_id.into(), error.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("build job failure update failed: {}", err))
    })
}

pub(crate) fn run_build_job(build_id: &str) -> safety::GraphResult<()> {
    update_build_job_started(build_id)?;
    match execute_build(true) {
        Ok(result) => update_build_job_completed(build_id, &result),
        Err(err) => {
            let message = err.to_string();
            let _ = update_build_job_failed(build_id, &message);
            Err(err)
        }
    }
}

pub(crate) fn create_maintenance_job() -> safety::GraphResult<String> {
    Spi::get_one::<String>(
        "INSERT INTO graph._maintenance_jobs (job_id, status)
         VALUES (gen_random_uuid()::text, 'queued')
         RETURNING job_id",
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("maintenance job creation failed: {}", err))
    })?
    .ok_or_else(|| {
        safety::GraphError::Internal("maintenance job creation returned no id".to_string())
    })
}

pub(crate) fn maintenance_job_row(job_id: &str) -> safety::GraphResult<Option<MaintenanceJobRow>> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT job_id, status, sync_rows_applied, nodes_after, edges_after,
                    vacuum_time_ms, progress_phase, progress_message,
                    started_at, finished_at, error
             FROM graph._maintenance_jobs
             WHERE job_id = $1",
            None,
            &[job_id.into()],
        )?;
        if let Some(row) = rows.into_iter().next() {
            return Ok::<_, pgrx::spi::SpiError>(Some(MaintenanceJobRow {
                job_id: row.get::<String>(1)?.unwrap_or_else(|| job_id.to_string()),
                status: row
                    .get::<String>(2)?
                    .unwrap_or_else(|| "not_found".to_string()),
                sync_rows_applied: row.get::<i64>(3)?,
                nodes_after: row.get::<i64>(4)?,
                edges_after: row.get::<i64>(5)?,
                vacuum_time_ms: row.get::<f64>(6)?,
                progress_phase: row
                    .get::<String>(7)?
                    .unwrap_or_else(|| "unknown".to_string()),
                progress_message: row.get::<String>(8)?,
                started_at: row.get::<TimestampWithTimeZone>(9)?,
                finished_at: row.get::<TimestampWithTimeZone>(10)?,
                error: row.get::<String>(11)?,
            }));
        }
        Ok(None)
    })
    .map_err(|err| safety::GraphError::Internal(format!("maintenance job read failed: {}", err)))
}

pub(crate) fn update_maintenance_job_started(job_id: &str) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "UPDATE graph._maintenance_jobs
         SET status = 'running',
             progress_phase = 'rebuilding',
             progress_message = 'rebuilding graph for maintenance',
             started_at = COALESCE(started_at, now()),
             worker_pid = pg_backend_pid(),
             updated_at = now()
         WHERE job_id = $1 AND status = 'queued'",
        &[job_id.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("maintenance job start update failed: {}", err))
    })
}

pub(crate) fn update_maintenance_job_completed(
    job_id: &str,
    result: &MaintenanceExecutionResult,
) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "UPDATE graph._maintenance_jobs
         SET status = 'completed',
             sync_rows_applied = $2,
             nodes_after = $3,
             edges_after = $4,
             vacuum_time_ms = $5,
             progress_phase = 'completed',
             progress_message = 'maintenance completed',
             finished_at = now(),
             updated_at = now(),
             error = NULL
         WHERE job_id = $1",
        &[
            job_id.into(),
            result.sync_rows_applied.into(),
            result.nodes_after.into(),
            result.edges_after.into(),
            result.vacuum_time_ms.into(),
        ],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("maintenance job completion update failed: {}", err))
    })
}

pub(crate) fn update_maintenance_job_failed(job_id: &str, error: &str) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "UPDATE graph._maintenance_jobs
         SET status = 'failed',
             progress_phase = 'failed',
             progress_message = $2,
             finished_at = now(),
             updated_at = now(),
             error = $2
         WHERE job_id = $1",
        &[job_id.into(), error.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("maintenance job failure update failed: {}", err))
    })
}

pub(crate) fn run_maintenance_job(job_id: &str) -> safety::GraphResult<()> {
    update_maintenance_job_started(job_id)?;
    match execute_maintenance_rebuild(true) {
        Ok(result) => update_maintenance_job_completed(job_id, &result),
        Err(err) => {
            let message = err.to_string();
            let _ = update_maintenance_job_failed(job_id, &message);
            Err(err)
        }
    }
}

pub(crate) fn current_database_and_user() -> safety::GraphResult<(String, String)> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT current_database()::text, current_user::text",
            None,
            &[],
        )?;
        let row = rows.first();
        Ok::<_, pgrx::spi::SpiError>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .map_err(|err| {
        safety::GraphError::Internal(format!("current database/user lookup failed: {}", err))
    })
}

pub(crate) fn launch_build_worker(build_id: &str) -> safety::GraphResult<()> {
    let (database, username) = current_database_and_user()?;
    let extra = format!("{}|{}|{}", build_id, database, username);
    BackgroundWorkerBuilder::new("graph concurrent build")
        .set_function("graph_build_worker_main")
        .set_library("graph")
        .enable_spi_access()
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(None)
        // SAFETY: MyProcPid is a Postgres backend global valid while this
        // backend is launching and registering the background worker.
        .set_notify_pid(unsafe { pgrx::pg_sys::MyProcPid })
        .set_extra(&extra)
        .load_dynamic()
        .map(|_| ())
        .map_err(|_| {
            safety::GraphError::Internal(
                "could not start graph concurrent build worker; check max_worker_processes"
                    .to_string(),
            )
        })
}

pub(crate) fn launch_maintenance_worker(job_id: &str) -> safety::GraphResult<()> {
    let (database, username) = current_database_and_user()?;
    let extra = format!("{}|{}|{}", job_id, database, username);
    BackgroundWorkerBuilder::new("graph maintenance")
        .set_function("graph_maintenance_worker_main")
        .set_library("graph")
        .enable_spi_access()
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(None)
        // SAFETY: MyProcPid is a Postgres backend global valid while this
        // backend is launching and registering the background worker.
        .set_notify_pid(unsafe { pgrx::pg_sys::MyProcPid })
        .set_extra(&extra)
        .load_dynamic()
        .map(|_| ())
        .map_err(|_| {
            safety::GraphError::Internal(
                "could not start graph maintenance worker; check max_worker_processes".to_string(),
            )
        })
}
