//! SQL-layer durable job tracking and background-worker launch helpers.

use crate::api_types::{
    BuildExecutionResult, BuildJobRow, MaintenanceExecutionResult, MaintenanceJobRow,
};
use crate::safety;
use crate::sql_build::{execute_build_with_progress, execute_maintenance_rebuild_with_progress};
use crate::sql_sync::current_sync_mode;
use pgrx::bgworkers::{BackgroundWorkerBuilder, BgWorkerStartTime};
use pgrx::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

impl JobStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkerMetadata {
    pub(crate) job_id: String,
    pub(crate) database: String,
    pub(crate) username: String,
}

impl WorkerMetadata {
    pub(crate) fn new(job_id: &str, database: String, username: String) -> Self {
        Self {
            job_id: job_id.to_string(),
            database,
            username,
        }
    }

    pub(crate) fn encode(&self) -> safety::GraphResult<String> {
        serde_json::to_string(self).map_err(|err| {
            safety::GraphError::Internal(format!("worker metadata encoding failed: {}", err))
        })
    }

    pub(crate) fn decode(raw: &str) -> safety::GraphResult<Self> {
        serde_json::from_str(raw).map_err(|err| {
            safety::GraphError::Internal(format!("worker metadata decoding failed: {}", err))
        })
    }
}

fn current_backend_pid() -> i32 {
    // SAFETY: MyProcPid is a PostgreSQL backend global that is valid while this
    // backend registers a dynamic background worker.
    unsafe { pgrx::pg_sys::MyProcPid }
}

pub(crate) fn create_build_job() -> safety::GraphResult<String> {
    let sync_mode = current_sync_mode()?.as_str().to_string();
    let queued = JobStatus::Queued.as_str();
    Spi::get_one_with_args::<String>(
        "INSERT INTO graph._build_jobs (
            build_id, status, sync_mode, progress_phase, progress_message
         )
         VALUES (
            gen_random_uuid()::text, $2, $1, $2,
            'queued for background build'
         )
         RETURNING build_id",
        &[sync_mode.into(), queued.into()],
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
    let queued = JobStatus::Queued.as_str();
    let running = JobStatus::Running.as_str();
    Spi::run_with_args(
        "UPDATE graph._build_jobs
         SET status = $2,
             progress_phase = 'building',
             progress_message = 'building graph from registered source tables',
             started_at = COALESCE(started_at, now()),
             worker_pid = pg_backend_pid(),
             updated_at = now()
         WHERE build_id = $1 AND status = $3",
        &[build_id.into(), running.into(), queued.into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("build job start update failed: {}", err)))
}

pub(crate) fn update_build_job_completed(
    build_id: &str,
    result: &BuildExecutionResult,
) -> safety::GraphResult<()> {
    let completed = JobStatus::Completed.as_str();
    Spi::run_with_args(
        "UPDATE graph._build_jobs
         SET status = $7,
             nodes_loaded = $2,
             edges_loaded = $3,
             build_time_ms = $4,
             memory_used_mb = $5,
             sync_mode = $6,
             progress_phase = $7,
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
            completed.into(),
        ],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("build job completion update failed: {}", err))
    })
}

pub(crate) fn update_build_job_progress(
    build_id: &str,
    phase: &str,
    message: &str,
) -> safety::GraphResult<()> {
    let running = JobStatus::Running.as_str();
    Spi::run_with_args(
        "UPDATE graph._build_jobs
         SET progress_phase = $2,
             progress_message = $3,
             updated_at = now()
         WHERE build_id = $1 AND status = $4",
        &[
            build_id.into(),
            phase.into(),
            message.into(),
            running.into(),
        ],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("build job progress update failed: {}", err))
    })
}

pub(crate) fn update_build_job_failed(build_id: &str, error: &str) -> safety::GraphResult<()> {
    let failed = JobStatus::Failed.as_str();
    let completed = JobStatus::Completed.as_str();
    Spi::run_with_args(
        "UPDATE graph._build_jobs
         SET status = $2,
             progress_phase = $2,
             progress_message = $3,
             finished_at = now(),
             updated_at = now(),
             error = $3
         WHERE build_id = $1 AND status <> $4",
        &[
            build_id.into(),
            failed.into(),
            error.into(),
            completed.into(),
        ],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("build job failure update failed: {}", err))
    })
}

pub(crate) fn run_build_job(build_id: &str) -> safety::GraphResult<()> {
    update_build_job_started(build_id)?;
    let mut progress = |phase, message| update_build_job_progress(build_id, phase, message);
    let result = execute_build_with_progress(true, &mut progress)?;
    update_build_job_completed(build_id, &result)
}

pub(crate) fn create_maintenance_job() -> safety::GraphResult<String> {
    let queued = JobStatus::Queued.as_str();
    Spi::get_one_with_args::<String>(
        "INSERT INTO graph._maintenance_jobs (
            job_id, status, progress_phase, progress_message
         )
         VALUES (
            gen_random_uuid()::text, $1, $1,
            'queued for background maintenance'
         )
         RETURNING job_id",
        &[queued.into()],
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
    let queued = JobStatus::Queued.as_str();
    let running = JobStatus::Running.as_str();
    Spi::run_with_args(
        "UPDATE graph._maintenance_jobs
         SET status = $2,
             progress_phase = 'rebuilding',
             progress_message = 'rebuilding graph for maintenance',
             started_at = COALESCE(started_at, now()),
             worker_pid = pg_backend_pid(),
             updated_at = now()
         WHERE job_id = $1 AND status = $3",
        &[job_id.into(), running.into(), queued.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("maintenance job start update failed: {}", err))
    })
}

pub(crate) fn update_maintenance_job_completed(
    job_id: &str,
    result: &MaintenanceExecutionResult,
) -> safety::GraphResult<()> {
    let completed = JobStatus::Completed.as_str();
    Spi::run_with_args(
        "UPDATE graph._maintenance_jobs
         SET status = $6,
             sync_rows_applied = $2,
             nodes_after = $3,
             edges_after = $4,
             vacuum_time_ms = $5,
             progress_phase = $6,
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
            completed.into(),
        ],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("maintenance job completion update failed: {}", err))
    })
}

pub(crate) fn update_maintenance_job_progress(
    job_id: &str,
    phase: &str,
    message: &str,
) -> safety::GraphResult<()> {
    let running = JobStatus::Running.as_str();
    Spi::run_with_args(
        "UPDATE graph._maintenance_jobs
         SET progress_phase = $2,
             progress_message = $3,
             updated_at = now()
         WHERE job_id = $1 AND status = $4",
        &[job_id.into(), phase.into(), message.into(), running.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("maintenance job progress update failed: {}", err))
    })
}

pub(crate) fn update_maintenance_job_failed(job_id: &str, error: &str) -> safety::GraphResult<()> {
    let failed = JobStatus::Failed.as_str();
    let completed = JobStatus::Completed.as_str();
    Spi::run_with_args(
        "UPDATE graph._maintenance_jobs
         SET status = $2,
             progress_phase = $2,
             progress_message = $3,
             finished_at = now(),
             updated_at = now(),
             error = $3
         WHERE job_id = $1 AND status <> $4",
        &[job_id.into(), failed.into(), error.into(), completed.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("maintenance job failure update failed: {}", err))
    })
}

pub(crate) fn run_maintenance_job(job_id: &str) -> safety::GraphResult<()> {
    update_maintenance_job_started(job_id)?;
    let mut progress = |phase, message| update_maintenance_job_progress(job_id, phase, message);
    let result = execute_maintenance_rebuild_with_progress(true, &mut progress)?;
    update_maintenance_job_completed(job_id, &result)
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
    let extra = WorkerMetadata::new(build_id, database, username).encode()?;
    BackgroundWorkerBuilder::new("graph concurrent build")
        .set_function("graph_build_worker_main")
        .set_library("graph")
        .enable_spi_access()
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(None)
        .set_notify_pid(current_backend_pid())
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
    let extra = WorkerMetadata::new(job_id, database, username).encode()?;
    BackgroundWorkerBuilder::new("graph maintenance")
        .set_function("graph_maintenance_worker_main")
        .set_library("graph")
        .enable_spi_access()
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(None)
        .set_notify_pid(current_backend_pid())
        .set_extra(&extra)
        .load_dynamic()
        .map(|_| ())
        .map_err(|_| {
            safety::GraphError::Internal(
                "could not start graph maintenance worker; check max_worker_processes".to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{JobStatus, WorkerMetadata};

    #[test]
    fn job_status_as_str_matches_sql_contract_values() {
        assert_eq!(JobStatus::Queued.as_str(), "queued");
        assert_eq!(JobStatus::Running.as_str(), "running");
        assert_eq!(JobStatus::Completed.as_str(), "completed");
        assert_eq!(JobStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn worker_metadata_round_trips_json_with_delimiters() {
        let metadata = WorkerMetadata::new("job|1", "db|name".to_string(), "user|name".to_string());
        let encoded = metadata.encode().expect("metadata encodes");
        assert_eq!(
            WorkerMetadata::decode(&encoded).expect("metadata decodes"),
            metadata
        );
    }
}
