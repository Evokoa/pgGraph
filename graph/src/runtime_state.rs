//! Backend-local graph runtime slot metadata.
//!
//! Each backend keeps one loaded engine slot. The slot is tagged with the graph
//! id it belongs to so selection changes cannot accidentally reuse a different
//! graph's engine.

use std::cell::RefCell;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::catalog::GraphMetadata;
use crate::edge_store::EdgeStore;
use crate::engine::Engine;

#[derive(Debug, Clone)]
pub(crate) struct LoadedGraphSnapshot {
    pub(crate) graph_id: String,
    pub(crate) graph_name: String,
    pub(crate) residency: String,
    pub(crate) node_count: i64,
    pub(crate) edge_count: i64,
    pub(crate) memory_used_mb: f64,
    pub(crate) projection_mode: String,
    pub(crate) last_access_unix_micros: i64,
}

#[derive(Debug, Clone)]
struct LoadedGraphSlot {
    graph_id: String,
    graph_name: String,
    residency: String,
    last_access_unix_micros: i64,
}

/// Durable-publication state that must survive a PostgreSQL error boundary.
///
/// PostgreSQL cancellation can bypass Rust destructors. Keeping this marker in
/// backend-local storage lets the next graph operation compare the generation
/// that was current before replacement with the generation that is current
/// after the error, then preserve or reload the authoritative projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplacementRecovery {
    pub(crate) graph_id: String,
    pub(crate) expected_generation: Option<u64>,
    pub(crate) candidate_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplacementRecoveryAction {
    PreserveResident,
    ReloadPublished,
    MissingPublished,
}

thread_local! {
    static LOADED_GRAPH_SLOT: RefCell<Option<LoadedGraphSlot>> = const { RefCell::new(None) };
    static REPLACEMENT_RECOVERY: RefCell<Vec<ReplacementRecovery>> = const { RefCell::new(Vec::new()) };
    // PostgreSQL ERROR uses longjmp and may skip Rust stack destructors. Keep
    // the compaction mmap owner in backend-local state so the next graph call
    // can recover it instead of leaking one full mapping per cancellation.
    static REPLACEMENT_EDGE_SNAPSHOT: RefCell<Option<Box<EdgeStore>>> = const { RefCell::new(None) };
    #[cfg(feature = "development")]
    static REPLACEMENT_FAULT: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn loaded_graph_id() -> Option<String> {
    LOADED_GRAPH_SLOT.with(|slot| slot.borrow().as_ref().map(|slot| slot.graph_id.clone()))
}

pub(crate) fn mark_loaded_graph(graph: &GraphMetadata) {
    LOADED_GRAPH_SLOT.with(|slot| {
        *slot.borrow_mut() = Some(LoadedGraphSlot {
            graph_id: graph.graph_id.clone(),
            graph_name: graph.graph_name.clone(),
            residency: graph.residency.clone(),
            last_access_unix_micros: now_unix_micros(),
        });
    });
}

pub(crate) fn touch_loaded_graph(graph_id: &str) {
    LOADED_GRAPH_SLOT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(slot) = slot.as_mut() {
            if slot.graph_id == graph_id {
                slot.last_access_unix_micros = now_unix_micros();
            }
        }
    });
}

pub(crate) fn clear_loaded_graph() {
    LOADED_GRAPH_SLOT.with(|slot| {
        *slot.borrow_mut() = None;
    });
    clear_replacement_edge_snapshot();
}

pub(crate) fn mark_replacement_in_progress(
    graph_id: &str,
    expected_generation: Option<u64>,
    candidate_generation: Option<u64>,
) {
    // A previous PostgreSQL longjmp may have retained a compaction mmap owner.
    // It is no longer in use once a new replacement begins.
    clear_replacement_edge_snapshot();
    REPLACEMENT_RECOVERY.with(|recovery| {
        let mut recovery = recovery.borrow_mut();
        recovery.retain(|entry| entry.graph_id != graph_id);
        recovery.push(ReplacementRecovery {
            graph_id: graph_id.to_string(),
            expected_generation,
            candidate_generation,
        });
    });
}

pub(crate) fn replacement_recovery_for(graph_id: &str) -> Option<ReplacementRecovery> {
    REPLACEMENT_RECOVERY.with(|recovery| {
        recovery
            .borrow()
            .iter()
            .find(|entry| entry.graph_id == graph_id)
            .cloned()
    })
}

pub(crate) fn clear_replacement_recovery_for(graph_id: &str) {
    REPLACEMENT_RECOVERY.with(|recovery| {
        recovery
            .borrow_mut()
            .retain(|entry| entry.graph_id != graph_id);
    });
}

/// Run one replacement step with a backend-recoverable mmap snapshot.
///
/// The closure is invoked without a live `RefCell` guard. The boxed value keeps
/// the raw address stable across the call. If PostgreSQL longjmps out of the
/// closure, the box remains owned by backend-local state and is released by
/// [`clear_replacement_edge_snapshot`] on the next graph operation.
///
/// # Safety
///
/// `operation` must not call `clear_loaded_graph`,
/// `mark_replacement_in_progress`, or `clear_replacement_edge_snapshot`,
/// because each can release or replace the Box backing its `&EdgeStore`.
pub(crate) unsafe fn with_replacement_edge_snapshot<T>(
    snapshot: EdgeStore,
    operation: impl FnOnce(&EdgeStore) -> T,
) -> T {
    let snapshot_ptr = REPLACEMENT_EDGE_SNAPSHOT.with(|slot| {
        let snapshot = Box::new(snapshot);
        let snapshot_ptr = std::ptr::from_ref(snapshot.as_ref());
        let mut slot = slot.borrow_mut();
        *slot = Some(snapshot);
        snapshot_ptr
    });
    // SAFETY: `snapshot_ptr` points into a Box owned by the backend-local slot.
    // No code can mutate that private slot while `operation` runs. The slot is
    // cleared only after the closure returns; a PostgreSQL longjmp skips that
    // clear and therefore keeps the allocation alive for later reconciliation.
    let result = operation(unsafe { &*snapshot_ptr });
    clear_replacement_edge_snapshot();
    result
}

pub(crate) fn clear_replacement_edge_snapshot() {
    REPLACEMENT_EDGE_SNAPSHOT.with(|snapshot| {
        *snapshot.borrow_mut() = None;
    });
}

#[cfg(feature = "development")]
pub(crate) fn arm_replacement_fault(stage: &str) -> crate::safety::GraphResult<()> {
    const STAGES: &[&str] = &[
        "source_scan",
        "candidate_write",
        "validation",
        "before_publication",
        "after_publication",
        "compaction_wait",
    ];
    if !STAGES.contains(&stage) {
        return Err(crate::safety::GraphError::InvalidFilter {
            reason: format!(
                "replacement fault stage must be one of: {}",
                STAGES.join(", ")
            ),
        });
    }
    REPLACEMENT_FAULT.with(|fault| {
        *fault.borrow_mut() = Some(stage.to_string());
    });
    Ok(())
}

#[cfg(feature = "development")]
pub(crate) fn inject_replacement_fault(stage: &str) -> crate::safety::GraphResult<()> {
    let armed = take_replacement_fault(stage);
    if armed {
        return Err(crate::safety::GraphError::Internal(format!(
            "injected replacement fault at {stage}"
        )));
    }
    Ok(())
}

#[cfg(feature = "development")]
fn take_replacement_fault(stage: &str) -> bool {
    REPLACEMENT_FAULT.with(|fault| {
        let mut fault = fault.borrow_mut();
        if fault.as_deref() == Some(stage) {
            fault.take();
            true
        } else {
            false
        }
    })
}

/// Hold a development compaction call inside PostgreSQL until cancellation.
///
/// This deterministic hook exists so the cancellation regression can deliver
/// a real `statement_timeout` while compaction owns its immutable edge-store
/// snapshot. It is not compiled into release builds.
#[cfg(all(feature = "development", not(test)))]
pub(crate) fn wait_on_replacement_fault(stage: &str) -> crate::safety::GraphResult<()> {
    if take_replacement_fault(stage) {
        pgrx::Spi::run("SELECT pg_catalog.pg_sleep(60)").map_err(|err| {
            crate::safety::GraphError::Internal(format!(
                "injected replacement wait at {stage} failed: {err}"
            ))
        })?;
    }
    Ok(())
}

#[cfg(any(not(feature = "development"), all(feature = "development", test)))]
#[inline]
pub(crate) fn wait_on_replacement_fault(_stage: &str) -> crate::safety::GraphResult<()> {
    Ok(())
}

#[cfg(not(feature = "development"))]
#[inline]
pub(crate) fn inject_replacement_fault(_stage: &str) -> crate::safety::GraphResult<()> {
    Ok(())
}

pub(crate) fn replacement_recovery_action(
    expected_generation: Option<u64>,
    current_generation: Option<u64>,
    resident_matches: bool,
    persisted_available: bool,
) -> ReplacementRecoveryAction {
    if expected_generation == current_generation && resident_matches {
        ReplacementRecoveryAction::PreserveResident
    } else if persisted_available {
        ReplacementRecoveryAction::ReloadPublished
    } else {
        ReplacementRecoveryAction::MissingPublished
    }
}

pub(crate) fn selected_graph_matches_loaded_slot(graph_id: &str) -> bool {
    loaded_graph_id().as_deref() == Some(graph_id)
}

pub(crate) fn loaded_graph_snapshot(engine: &Engine) -> Option<LoadedGraphSnapshot> {
    LOADED_GRAPH_SLOT.with(|slot| {
        let slot = slot.borrow();
        let slot = slot.as_ref()?;
        if !engine.built {
            return None;
        }
        Some(LoadedGraphSnapshot {
            graph_id: slot.graph_id.clone(),
            graph_name: slot.graph_name.clone(),
            residency: slot.residency.clone(),
            node_count: engine.node_store.node_count() as i64,
            edge_count: engine.edge_store.edge_count() as i64,
            memory_used_mb: engine.estimated_memory_used_mb(),
            projection_mode: engine.projection_mode.as_str().to_string(),
            last_access_unix_micros: slot.last_access_unix_micros,
        })
    })
}

fn now_unix_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_micros()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        clear_replacement_recovery_for, mark_replacement_in_progress, replacement_recovery_action,
        replacement_recovery_for, ReplacementRecoveryAction,
    };

    #[test]
    fn replacement_recovery_is_retained_independently_per_graph() {
        clear_replacement_recovery_for("graph-a");
        clear_replacement_recovery_for("graph-b");
        mark_replacement_in_progress("graph-a", Some(1), Some(2));
        mark_replacement_in_progress("graph-b", Some(7), Some(8));

        assert_eq!(
            replacement_recovery_for("graph-a").map(|entry| entry.candidate_generation),
            Some(Some(2))
        );
        assert_eq!(
            replacement_recovery_for("graph-b").map(|entry| entry.candidate_generation),
            Some(Some(8))
        );

        clear_replacement_recovery_for("graph-b");
        assert!(replacement_recovery_for("graph-b").is_none());
        assert!(replacement_recovery_for("graph-a").is_some());
        clear_replacement_recovery_for("graph-a");
    }

    #[test]
    fn replacement_recovery_preserves_unchanged_resident_generation() {
        assert_eq!(
            replacement_recovery_action(Some(7), Some(7), true, true),
            ReplacementRecoveryAction::PreserveResident
        );
    }

    #[test]
    fn replacement_recovery_reloads_generation_published_before_error() {
        assert_eq!(
            replacement_recovery_action(Some(7), Some(8), true, true),
            ReplacementRecoveryAction::ReloadPublished
        );
    }

    #[test]
    fn replacement_recovery_reloads_evicted_previous_generation() {
        assert_eq!(
            replacement_recovery_action(Some(7), Some(7), false, true),
            ReplacementRecoveryAction::ReloadPublished
        );
    }

    #[test]
    fn replacement_recovery_reloads_compatibility_artifact_without_manifest() {
        assert_eq!(
            replacement_recovery_action(None, None, false, true),
            ReplacementRecoveryAction::ReloadPublished
        );
    }

    #[test]
    fn replacement_recovery_reports_missing_authoritative_artifact() {
        assert_eq!(
            replacement_recovery_action(Some(7), Some(7), false, false),
            ReplacementRecoveryAction::MissingPublished
        );
    }
}
