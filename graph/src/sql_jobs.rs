//! SQL-layer durable job tracking and background-worker launch helpers.

use crate::api_types::{
    BuildExecutionResult, BuildJobRow, MaintenanceExecutionResult, MaintenanceJobRow,
};
use crate::catalog;
use crate::config;
use crate::safety;
use crate::sql_build::{
    execute_build_with_prevalidated_mode_and_progress, execute_maintenance_rebuild_with_progress,
    validate_projection_mode_enabled,
};
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
    Disabled,
    RetryableFailure,
    PermanentFailure,
    #[allow(
        dead_code,
        reason = "quota_blocked is part of the durable SQL status contract before runtime quota execution produces it"
    )]
    QuotaBlocked,
    LockSkipped,
}

impl JobStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Disabled => "disabled",
            JobStatus::RetryableFailure => "retryable_failed",
            JobStatus::PermanentFailure => "permanent_failed",
            JobStatus::QuotaBlocked => "quota_blocked",
            JobStatus::LockSkipped => "lock_skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkerMetadata {
    #[serde(rename = "j")]
    pub(crate) job_id: String,
    #[serde(rename = "d")]
    pub(crate) database_oid: u32,
    #[serde(rename = "u")]
    pub(crate) user_oid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulerWorkerMetadata {
    #[serde(rename = "m")]
    pub(crate) max_jobs: i32,
    #[serde(rename = "d")]
    pub(crate) database_oid: u32,
    #[serde(rename = "u")]
    pub(crate) user_oid: u32,
}

impl SchedulerWorkerMetadata {
    pub(crate) fn new(max_jobs: i32, database_oid: u32, user_oid: u32) -> Self {
        Self {
            max_jobs,
            database_oid,
            user_oid,
        }
    }

    pub(crate) fn encode(&self) -> safety::GraphResult<String> {
        encode_background_worker_metadata(self, "scheduler worker")
    }

    pub(crate) fn decode(raw: &str) -> safety::GraphResult<Self> {
        serde_json::from_str(raw).map_err(|err| {
            safety::GraphError::Internal(format!(
                "scheduler worker metadata decoding failed: {}",
                err
            ))
        })
    }
}

impl WorkerMetadata {
    pub(crate) fn new(job_id: &str, database_oid: u32, user_oid: u32) -> Self {
        Self {
            job_id: job_id.to_string(),
            database_oid,
            user_oid,
        }
    }

    pub(crate) fn encode(&self) -> safety::GraphResult<String> {
        encode_background_worker_metadata(self, "worker")
    }

    pub(crate) fn decode(raw: &str) -> safety::GraphResult<Self> {
        serde_json::from_str(raw).map_err(|err| {
            safety::GraphError::Internal(format!("worker metadata decoding failed: {}", err))
        })
    }
}

const MAX_BACKGROUND_WORKER_EXTRA_BYTES: usize = pgrx::pg_sys::BGW_EXTRALEN as usize - 1;

fn validate_background_worker_extra(raw: &str, kind: &str) -> safety::GraphResult<()> {
    if raw.as_bytes().contains(&0) {
        return Err(safety::GraphError::Internal(format!(
            "{kind} metadata contains an embedded NUL byte"
        )));
    }
    if raw.len() > MAX_BACKGROUND_WORKER_EXTRA_BYTES {
        return Err(safety::GraphError::Internal(format!(
            "{kind} metadata is {} bytes; PostgreSQL background-worker metadata allows at most {} bytes",
            raw.len(),
            MAX_BACKGROUND_WORKER_EXTRA_BYTES
        )));
    }
    Ok(())
}

fn encode_background_worker_metadata<T: Serialize>(
    metadata: &T,
    kind: &str,
) -> safety::GraphResult<String> {
    let encoded = serde_json::to_string(metadata).map_err(|err| {
        safety::GraphError::Internal(format!("{kind} metadata encoding failed: {err}"))
    })?;
    validate_background_worker_extra(&encoded, kind)?;
    Ok(encoded)
}

fn current_backend_pid() -> i32 {
    // SAFETY: MyProcPid is a PostgreSQL backend global that is valid while this
    // backend registers a dynamic background worker.
    unsafe { pgrx::pg_sys::MyProcPid }
}

pub(crate) fn create_build_job(
    projection_mode: config::ProjectionMode,
) -> safety::GraphResult<String> {
    let graph = catalog::selected_or_default_graph_metadata()?;
    validate_projection_mode_enabled(projection_mode)?;
    let sync_mode = current_sync_mode()?.as_str().to_string();
    let projection_mode = projection_mode.as_str().to_string();
    let queued = JobStatus::Queued.as_str();
    Spi::get_one_with_args::<String>(
        "INSERT INTO graph._build_jobs (
            build_id, graph_id, status, sync_mode, projection_mode,
            progress_phase, progress_message
         )
         VALUES (
            gen_random_uuid()::text, $4::uuid, $3, $1, $2, $3,
            'queued for background build'
         )
         RETURNING build_id",
        &[
            sync_mode.into(),
            projection_mode.into(),
            queued.into(),
            graph.graph_id.into(),
        ],
    )
    .map_err(|err| safety::GraphError::Internal(format!("build job creation failed: {}", err)))?
    .ok_or_else(|| safety::GraphError::Internal("build job creation returned no id".to_string()))
}

pub(crate) fn build_job_row(build_id: &str) -> safety::GraphResult<Option<BuildJobRow>> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT build_id, graph_id::text, status, nodes_loaded, edges_loaded,
                    build_time_ms, memory_used_mb, sync_mode, projection_mode,
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
                graph_id: row.get::<String>(2)?.unwrap_or_default(),
                status: row
                    .get::<String>(3)?
                    .unwrap_or_else(|| "not_found".to_string()),
                nodes_loaded: row.get::<i64>(4)?,
                edges_loaded: row.get::<i64>(5)?,
                build_time_ms: row.get::<f64>(6)?,
                memory_used_mb: row.get::<f64>(7)?,
                sync_mode: row
                    .get::<String>(8)?
                    .unwrap_or_else(|| "manual".to_string()),
                projection_mode: row
                    .get::<String>(9)?
                    .unwrap_or_else(|| config::ProjectionMode::CsrReadonly.as_str().to_string()),
                progress_phase: row
                    .get::<String>(10)?
                    .unwrap_or_else(|| "unknown".to_string()),
                progress_message: row.get::<String>(11)?,
                started_at: row.get::<TimestampWithTimeZone>(12)?,
                finished_at: row.get::<TimestampWithTimeZone>(13)?,
                error: row.get::<String>(14)?,
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
             projection_mode = $8,
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
            result.projection_mode.clone().into(),
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
    let row = build_job_row(build_id)?.ok_or_else(|| {
        safety::GraphError::Internal(format!("build job '{}' was not found", build_id))
    })?;
    catalog::set_selected_graph_id(&row.graph_id)?;
    let projection_mode = config::parse_projection_mode(&row.projection_mode).ok_or_else(|| {
        safety::GraphError::InvalidFilter {
            reason: format!(
                "unsupported stored projection mode '{}'; expected 'csr_readonly' or 'mutable_overlay'",
                row.projection_mode
            ),
        }
    })?;
    update_build_job_started(build_id)?;
    let mut progress = |phase, message| update_build_job_progress(build_id, phase, message);
    let result =
        execute_build_with_prevalidated_mode_and_progress(true, projection_mode, &mut progress)?;
    update_build_job_completed(build_id, &result)
}

pub(crate) fn create_maintenance_job() -> safety::GraphResult<String> {
    let graph = catalog::selected_or_default_graph_metadata()?;
    let queued = JobStatus::Queued.as_str();
    Spi::get_one_with_args::<String>(
        "INSERT INTO graph._maintenance_jobs (
            job_id, graph_id, status, progress_phase, progress_message
         )
         VALUES (
            gen_random_uuid()::text, $2::uuid, $1, $1,
            'queued for background maintenance'
         )
         RETURNING job_id",
        &[queued.into(), graph.graph_id.into()],
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
            "SELECT job_id, graph_id::text, status, sync_rows_applied, nodes_after, edges_after,
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
                graph_id: row.get::<String>(2)?.unwrap_or_default(),
                status: row
                    .get::<String>(3)?
                    .unwrap_or_else(|| "not_found".to_string()),
                sync_rows_applied: row.get::<i64>(4)?,
                nodes_after: row.get::<i64>(5)?,
                edges_after: row.get::<i64>(6)?,
                vacuum_time_ms: row.get::<f64>(7)?,
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
    let row = maintenance_job_row(job_id)?.ok_or_else(|| {
        safety::GraphError::Internal(format!("maintenance job '{}' was not found", job_id))
    })?;
    catalog::set_selected_graph_id(&row.graph_id)?;
    update_maintenance_job_started(job_id)?;
    let mut progress = |phase, message| update_maintenance_job_progress(job_id, phase, message);
    let result = execute_maintenance_rebuild_with_progress(true, &mut progress)?;
    update_maintenance_job_completed(job_id, &result)
}

fn current_database_and_user_oids() -> safety::GraphResult<(u32, u32)> {
    // SAFETY: launch helpers run inside a connected PostgreSQL backend.
    // MyDatabaseId identifies that backend's database, and GetUserId returns
    // the effective current_user, including SET ROLE and SECURITY DEFINER.
    let (database_oid, user_oid) =
        unsafe { (pgrx::pg_sys::MyDatabaseId, pgrx::pg_sys::GetUserId()) };
    if database_oid == pgrx::pg_sys::Oid::INVALID || user_oid == pgrx::pg_sys::Oid::INVALID {
        return Err(safety::GraphError::Internal(
            "background worker launch requires a connected database and effective role".to_string(),
        ));
    }
    Ok((database_oid.to_u32(), user_oid.to_u32()))
}

pub(crate) fn launch_build_worker(build_id: &str) -> safety::GraphResult<()> {
    let (database_oid, user_oid) = current_database_and_user_oids()?;
    let extra = WorkerMetadata::new(build_id, database_oid, user_oid).encode()?;
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
    let (database_oid, user_oid) = current_database_and_user_oids()?;
    let extra = WorkerMetadata::new(job_id, database_oid, user_oid).encode()?;
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

pub(crate) fn launch_due_jobs_worker(max_jobs: i32) -> safety::GraphResult<()> {
    let (database_oid, user_oid) = current_database_and_user_oids()?;
    let extra = SchedulerWorkerMetadata::new(max_jobs, database_oid, user_oid).encode()?;
    BackgroundWorkerBuilder::new("graph due jobs")
        .set_function("graph_due_jobs_worker_main")
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
                "could not start graph due jobs worker; check max_worker_processes".to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        validate_background_worker_extra, JobStatus, SchedulerWorkerMetadata, WorkerMetadata,
        MAX_BACKGROUND_WORKER_EXTRA_BYTES,
    };

    #[test]
    fn job_status_as_str_matches_sql_contract_values() {
        assert_eq!(JobStatus::Queued.as_str(), "queued");
        assert_eq!(JobStatus::Running.as_str(), "running");
        assert_eq!(JobStatus::Completed.as_str(), "completed");
        assert_eq!(JobStatus::Failed.as_str(), "failed");
        assert_eq!(JobStatus::Disabled.as_str(), "disabled");
        assert_eq!(JobStatus::RetryableFailure.as_str(), "retryable_failed");
        assert_eq!(JobStatus::PermanentFailure.as_str(), "permanent_failed");
        assert_eq!(JobStatus::QuotaBlocked.as_str(), "quota_blocked");
        assert_eq!(JobStatus::LockSkipped.as_str(), "lock_skipped");
    }

    #[test]
    fn worker_metadata_round_trips_with_worst_case_oids() {
        let metadata =
            WorkerMetadata::new("00000000-0000-0000-0000-000000000001", u32::MAX, u32::MAX);
        let encoded = metadata.encode().expect("metadata encodes");
        assert!(encoded.len() <= MAX_BACKGROUND_WORKER_EXTRA_BYTES);
        assert_eq!(
            WorkerMetadata::decode(&encoded).expect("metadata decodes"),
            metadata
        );
    }

    #[test]
    fn scheduler_worker_metadata_round_trips_json() {
        let metadata = SchedulerWorkerMetadata::new(i32::MAX, u32::MAX, u32::MAX);
        let encoded = metadata.encode().expect("metadata encodes");
        assert!(encoded.len() <= MAX_BACKGROUND_WORKER_EXTRA_BYTES);
        assert_eq!(
            SchedulerWorkerMetadata::decode(&encoded).expect("metadata decodes"),
            metadata
        );
    }

    #[test]
    fn background_worker_extra_enforces_postgresql_byte_limit() {
        let exact_limit = "x".repeat(MAX_BACKGROUND_WORKER_EXTRA_BYTES);
        validate_background_worker_extra(&exact_limit, "test").expect("127 bytes fit");

        let oversized = "x".repeat(MAX_BACKGROUND_WORKER_EXTRA_BYTES + 1);
        assert!(validate_background_worker_extra(&oversized, "test").is_err());

        let utf8_at_limit = format!(
            "{}x",
            "é".repeat((MAX_BACKGROUND_WORKER_EXTRA_BYTES - 1) / 2)
        );
        assert_eq!(utf8_at_limit.len(), MAX_BACKGROUND_WORKER_EXTRA_BYTES);
        validate_background_worker_extra(&utf8_at_limit, "test").expect("UTF-8 bytes fit");
        assert!(validate_background_worker_extra(&format!("{utf8_at_limit}é"), "test").is_err());
        assert!(validate_background_worker_extra("metadata\0tail", "test").is_err());
    }

    #[test]
    fn worker_metadata_rejects_oversized_job_identifier() {
        let metadata = WorkerMetadata::new(
            &"x".repeat(MAX_BACKGROUND_WORKER_EXTRA_BYTES),
            u32::MAX,
            u32::MAX,
        );
        assert!(metadata.encode().is_err());
    }
}
