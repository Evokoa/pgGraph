//! Backend-local graph runtime slot metadata.
//!
//! Each backend keeps one loaded engine slot. The slot is tagged with the graph
//! id it belongs to so selection changes cannot accidentally reuse a different
//! graph's engine.

use std::cell::RefCell;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::catalog::GraphMetadata;
use crate::engine::Engine;

#[derive(Debug, Clone)]
pub(crate) struct LoadedGraphSnapshot {
    pub(crate) graph_id: String,
    pub(crate) graph_name: String,
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
    last_access_unix_micros: i64,
}

thread_local! {
    static LOADED_GRAPH_SLOT: RefCell<Option<LoadedGraphSlot>> = const { RefCell::new(None) };
}

pub(crate) fn loaded_graph_id() -> Option<String> {
    LOADED_GRAPH_SLOT.with(|slot| slot.borrow().as_ref().map(|slot| slot.graph_id.clone()))
}

pub(crate) fn mark_loaded_graph(graph: &GraphMetadata) {
    LOADED_GRAPH_SLOT.with(|slot| {
        *slot.borrow_mut() = Some(LoadedGraphSlot {
            graph_id: graph.graph_id.clone(),
            graph_name: graph.graph_name.clone(),
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
