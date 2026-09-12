//! Transactional authority for immutable graph generations.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use pgrx::prelude::*;
use pgrx::JsonB;
use serde::{Deserialize, Serialize};

use crate::safety::{GraphError, GraphResult};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PublishedGeneration {
    pub(crate) generation_id: u64,
    pub(crate) manifest_checksum: String,
    pub(crate) publication_xid: u32,
}

pub(crate) struct PendingPublication {
    pub(crate) caller_oid: pg_sys::Oid,
    pub(crate) graph_id: String,
    pub(crate) root: PathBuf,
    pub(crate) generation_id: u64,
    pub(crate) manifest_checksum: String,
}

thread_local! {
    static PENDING: RefCell<Option<PendingPublication>> = const { RefCell::new(None) };
}

fn selected_root() -> GraphResult<(String, PathBuf)> {
    let graph_id = crate::catalog::selected_or_default_graph_id_via_definer()?;
    let path = crate::persistence::graph_file_path_for_uncreated(&graph_id)?;
    Ok((
        graph_id,
        crate::persistence::projection_manifest_root(&path),
    ))
}

fn require_selected_root(root: &Path) -> GraphResult<String> {
    let (graph_id, expected) = selected_root()?;
    if root != expected {
        return Err(GraphError::Internal(
            "publication root differs from the selected graph".into(),
        ));
    }
    Ok(graph_id)
}

#[cfg(not(test))]
pub(crate) fn current(root: &Path) -> GraphResult<Option<PublishedGeneration>> {
    require_selected_root(root)?;
    let value = Spi::get_one::<JsonB>("SELECT graph._published_generation_for_current_role()")
        .map_err(|error| GraphError::Internal(format!("publication lookup failed: {error}")))?;
    value
        .map(|value| {
            serde_json::from_value(value.0).map_err(|error| {
                GraphError::Internal(format!("publication metadata is invalid: {error}"))
            })
        })
        .transpose()
}

/// Called only inside the reader's fixed-search-path privilege mediator.
pub(crate) fn read_current_direct(graph_id: &str) -> GraphResult<Option<JsonB>> {
    let path = crate::persistence::graph_file_path_for_uncreated(graph_id)?;
    let root = crate::persistence::projection_manifest_root(&path);
    Spi::get_one_with_args::<JsonB>(
        "SELECT (SELECT jsonb_build_object(
             'generation_id', generation_id,
             'manifest_checksum', manifest_checksum,
             'publication_xid', xmin::text::bigint)
         FROM graph._projection_heads
         WHERE graph_id = $1::uuid AND artifact_root = $2)",
        &[graph_id.into(), root.to_string_lossy().as_ref().into()],
    )
    .map_err(|error| GraphError::Internal(format!("publication catalog read failed: {error}")))
}

/// Called by graph_runtime_status after its per-graph READ privilege check.
pub(crate) fn authorized_artifact_path(graph_id: &str) -> GraphResult<Option<PathBuf>> {
    let Some(value) = read_current_direct(graph_id)? else {
        return Ok(None);
    };
    let published = serde_json::from_value(value.0).map_err(|error| {
        GraphError::Internal(format!("publication metadata is invalid: {error}"))
    })?;
    let path = crate::persistence::graph_file_path_for_uncreated(graph_id)?;
    let root = crate::persistence::projection_manifest_root(&path);
    let manifest =
        super::manifest::ProjectionManifestStore::new(&root).published_metadata(&published)?;
    Ok(Some(root.join(manifest.base_artifact_path)))
}

/// Queue validated metadata for a no-argument SECURITY DEFINER mediator.
#[cfg(not(test))]
pub(crate) fn publish(
    root: &Path,
    generation_id: u64,
    manifest_checksum: String,
) -> GraphResult<()> {
    require_publication_snapshot()?;
    let graph_id = require_selected_root(root)?;
    let caller_oid = crate::catalog::current_role_oid()?;
    let stamp = crate::sql_sync::prepare_backend_replay()?;
    crate::sql_sync::mark_backend_replay(stamp);
    let published = with_pending_publication(
        PendingPublication {
            caller_oid,
            graph_id,
            root: root.to_path_buf(),
            generation_id,
            manifest_checksum,
        },
        || Spi::get_one::<bool>("SELECT graph._publish_generation_for_current_role()"),
    )?
    .map_err(|error| GraphError::Internal(format!("publication catalog write failed: {error}")))?;
    if published != Some(true) {
        return Err(GraphError::Internal(
            "publication mediator did not publish".into(),
        ));
    }
    Ok(())
}

#[cfg(not(test))]
fn with_pending_publication<R>(
    pending: PendingPublication,
    operation: impl FnOnce() -> R + std::panic::UnwindSafe,
) -> GraphResult<R> {
    PENDING.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err(GraphError::Internal("nested projection publication".into()));
        }
        *slot = Some(pending);
        Ok(())
    })?;
    Ok(pg_sys::PgTryBuilder::new(operation)
        .finally(|| {
            PENDING.with(|slot| {
                slot.borrow_mut().take();
            });
        })
        .execute())
}

#[cfg(all(not(test), feature = "development"))]
pub(crate) fn test_publication_error_after_arming() -> bool {
    let caller_oid = crate::catalog::current_role_oid().unwrap_or_else(|error| error.report());
    let (graph_id, root) = selected_root().unwrap_or_else(|error| error.report());
    pg_sys::PgTryBuilder::new(|| {
        with_pending_publication(
            PendingPublication {
                caller_oid,
                graph_id,
                root,
                generation_id: u64::MAX,
                manifest_checksum: "injected cancellation".into(),
            },
            || {
                pgrx::ereport!(
                    ERROR,
                    pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
                    "injected publication cancellation"
                );
            },
        )
        .unwrap_or_else(|error| error.report());
        true
    })
    .catch_when(pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED, |_| false)
    .execute()
}

pub(crate) fn take_pending() -> GraphResult<PendingPublication> {
    PENDING
        .with(|pending| pending.borrow_mut().take())
        .ok_or_else(|| GraphError::AclDenied {
            table: "internal publication mediator".into(),
        })
}

/// A fixed transaction snapshot cannot authorize removal of newer history.
pub(crate) fn uses_fixed_snapshot() -> bool {
    // SAFETY: PostgreSQL initializes this backend-local transaction setting
    // before invoking extension functions. Reading it does not retain a pointer.
    unsafe { pg_sys::XactIsoLevel >= pg_sys::XACT_REPEATABLE_READ as i32 }
}

pub(crate) fn require_publication_snapshot() -> GraphResult<()> {
    if uses_fixed_snapshot() {
        return Err(GraphError::UnsupportedOperation {
            operation: "durable generation publication".into(),
            reason: "use READ COMMITTED; a fixed transaction snapshot cannot publish a durable sync watermark".into(),
        });
    }
    Ok(())
}

/// Keep history until PostgreSQL's synchronized global horizon passes the head.
/// Callers must hold the graph publisher transaction lock during reclamation.
#[cfg(not(test))]
pub(crate) fn historical_generations_required(root: &Path) -> GraphResult<bool> {
    if uses_fixed_snapshot() {
        return Ok(true);
    }
    let Some(published) = current(root)? else {
        return Ok(true);
    };
    // SAFETY: PostgreSQL explicitly accepts NULL to select the conservative
    // shared-relation horizon. The server synchronizes this scan with snapshot
    // import, prepared transactions and replication slots. XID comparison is
    // wraparound-safe and both calls run inside the active backend.
    Ok(unsafe {
        let oldest = pg_sys::GetOldestNonRemovableTransactionId(std::ptr::null_mut());
        pg_sys::TransactionIdPrecedesOrEquals(oldest, published.publication_xid.into())
    })
}

/// Clear publication after the caller authorizes and locks graph administration.
/// Immutable files remain available to transactions that still see the old head.
pub(crate) fn clear_direct(graph_id: &str) -> GraphResult<()> {
    let stamp = crate::sql_sync::prepare_backend_replay()?;
    crate::sql_sync::mark_backend_replay(stamp);
    Spi::run_with_args(
        "DELETE FROM graph._projection_heads WHERE graph_id = $1::uuid",
        &[graph_id.into()],
    )
    .map_err(|error| GraphError::Internal(format!("publication reset failed: {error}")))
}

/// The mediator verifies caller and graph before entering this write boundary.
pub(crate) fn publish_direct(pending: &PendingPublication) -> GraphResult<()> {
    if require_selected_root(&pending.root)? != pending.graph_id {
        return Err(GraphError::AclDenied {
            table: "internal publication graph".into(),
        });
    }
    let generation_id = i64::try_from(pending.generation_id)
        .map_err(|_| GraphError::Internal("publication generation exceeds BIGINT".into()))?;
    Spi::run_with_args(
        "INSERT INTO graph._projection_heads(graph_id, artifact_root, generation_id, manifest_checksum)
         VALUES ($1::uuid, $2, $3, $4)
         ON CONFLICT (graph_id, artifact_root) DO UPDATE SET
             generation_id = EXCLUDED.generation_id,
             manifest_checksum = EXCLUDED.manifest_checksum",
        &[pending.graph_id.as_str().into(), pending.root.to_string_lossy().as_ref().into(),
          generation_id.into(), pending.manifest_checksum.as_str().into()],
    ).map_err(|error| GraphError::Internal(format!("publication catalog update failed: {error}")))
}
