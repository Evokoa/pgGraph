//! Transaction-local projection delta storage.
//!
//! Mutable graph writes are applied to PostgreSQL first. After PostgreSQL
//! accepts the write, this module records the backend-local graph delta that
//! makes read-your-own-writes possible until transaction end.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// Transaction-local node created by a graph write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddedNode {
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source table primary key.
    pub(crate) primary_key: String,
    /// Assigned graph node index.
    pub(crate) node_idx: u32,
}

/// Transaction-local edge created by a graph write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeltaEdge {
    /// Target graph node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: u8,
    /// Optional weight captured from a mapped edge row.
    pub(crate) weight: Option<u32>,
}

/// Per-transaction graph projection delta.
#[derive(Debug, Default)]
pub(crate) struct TxGraphDelta {
    added_nodes: Vec<AddedNode>,
    deleted_nodes: HashSet<u32>,
    added_edges: HashMap<u32, Vec<DeltaEdge>>,
    deleted_edges: HashSet<(u32, u32, u8)>,
}

/// Lightweight statistics exposed through graph status surfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TxDeltaStats {
    /// Added node count.
    pub(crate) added_nodes: usize,
    /// Deleted/tombstoned node count.
    pub(crate) deleted_nodes: usize,
    /// Added edge count.
    pub(crate) added_edges: usize,
    /// Deleted edge tombstone count.
    pub(crate) deleted_edges: usize,
    /// Estimated heap bytes owned by the transaction delta.
    pub(crate) memory_bytes: usize,
    /// Whether any graph delta is currently recorded.
    pub(crate) dirty: bool,
}

thread_local! {
    static TX_DELTA: RefCell<Option<TxGraphDelta>> = const { RefCell::new(None) };
    static SUBTRANSACTION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

static CALLBACKS_REGISTERED: AtomicBool = AtomicBool::new(false);

impl TxGraphDelta {
    fn stats(&self) -> TxDeltaStats {
        let added_edges = self.added_edges.values().map(Vec::len).sum::<usize>();
        let memory_bytes = self.estimated_heap_bytes();
        TxDeltaStats {
            added_nodes: self.added_nodes.len(),
            deleted_nodes: self.deleted_nodes.len(),
            added_edges,
            deleted_edges: self.deleted_edges.len(),
            memory_bytes,
            dirty: self.is_dirty(),
        }
    }

    fn estimated_heap_bytes(&self) -> usize {
        let node_pk_bytes = self
            .added_nodes
            .iter()
            .map(|node| node.primary_key.capacity())
            .sum::<usize>();
        let added_edge_bytes = self
            .added_edges
            .values()
            .map(|edges| edges.capacity() * std::mem::size_of::<DeltaEdge>())
            .sum::<usize>();
        self.added_nodes.capacity() * std::mem::size_of::<AddedNode>()
            + node_pk_bytes
            + self.deleted_nodes.capacity() * std::mem::size_of::<u32>()
            + self.added_edges.capacity()
                * (std::mem::size_of::<u32>() + std::mem::size_of::<Vec<DeltaEdge>>())
            + added_edge_bytes
            + self.deleted_edges.capacity() * std::mem::size_of::<(u32, u32, u8)>()
    }

    fn is_dirty(&self) -> bool {
        !self.added_nodes.is_empty()
            || !self.deleted_nodes.is_empty()
            || !self.added_edges.is_empty()
            || !self.deleted_edges.is_empty()
    }

    #[cfg(test)]
    fn add_node_for_test(&mut self, table_oid: u32, primary_key: &str, node_idx: u32) {
        self.added_nodes.push(AddedNode {
            table_oid,
            primary_key: primary_key.to_string(),
            node_idx,
        });
    }

    #[cfg(test)]
    fn add_edge_for_test(&mut self, source: u32, edge: DeltaEdge) {
        self.added_edges.entry(source).or_default().push(edge);
    }
}

/// Register transaction callbacks used to clear backend-local deltas.
pub(crate) fn register_transaction_callbacks() {
    #[cfg(not(test))]
    {
        if CALLBACKS_REGISTERED.swap(true, Ordering::SeqCst) {
            return;
        }
        // SAFETY: These callbacks are permanent backend-local PostgreSQL
        // transaction hooks. The callback functions below do not allocate
        // through PostgreSQL, do not call SPI, and do not raise errors.
        unsafe {
            pgrx::pg_sys::RegisterXactCallback(Some(xact_callback), std::ptr::null_mut());
            pgrx::pg_sys::RegisterSubXactCallback(Some(subxact_callback), std::ptr::null_mut());
        }
    }
    #[cfg(test)]
    {
        CALLBACKS_REGISTERED.store(true, Ordering::SeqCst);
    }
}

#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn xact_callback(
    event: pgrx::pg_sys::XactEvent::Type,
    _arg: *mut std::ffi::c_void,
) {
    use pgrx::pg_sys::XactEvent;
    if matches!(
        event,
        XactEvent::XACT_EVENT_COMMIT
            | XactEvent::XACT_EVENT_ABORT
            | XactEvent::XACT_EVENT_PARALLEL_COMMIT
            | XactEvent::XACT_EVENT_PARALLEL_ABORT
    ) {
        clear_current_transaction_state();
    }
}

#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn subxact_callback(
    event: pgrx::pg_sys::SubXactEvent::Type,
    _my_subid: pgrx::pg_sys::SubTransactionId,
    _parent_subid: pgrx::pg_sys::SubTransactionId,
    _arg: *mut std::ffi::c_void,
) {
    use pgrx::pg_sys::SubXactEvent;
    match event {
        SubXactEvent::SUBXACT_EVENT_START_SUB => {
            SUBTRANSACTION_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        }
        SubXactEvent::SUBXACT_EVENT_COMMIT_SUB => {
            decrement_subtransaction_depth();
        }
        SubXactEvent::SUBXACT_EVENT_ABORT_SUB => {
            clear_current_delta();
            decrement_subtransaction_depth();
        }
        SubXactEvent::SUBXACT_EVENT_PRE_COMMIT_SUB => {}
        _ => {}
    }
}

/// Return current transaction-delta statistics.
pub(crate) fn stats() -> TxDeltaStats {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .map(TxGraphDelta::stats)
            .unwrap_or_default()
    })
}

#[cfg(test)]
fn subtransaction_active() -> bool {
    SUBTRANSACTION_DEPTH.with(|depth| depth.get() > 0)
}

fn clear_current_delta() {
    TX_DELTA.with(|delta| {
        delta.borrow_mut().take();
    });
}

fn clear_current_transaction_state() {
    clear_current_delta();
    SUBTRANSACTION_DEPTH.with(|depth| depth.set(0));
}

fn decrement_subtransaction_depth() {
    SUBTRANSACTION_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
}

#[cfg(test)]
fn with_delta_for_test(mut f: impl FnMut(&mut TxGraphDelta)) {
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        f(delta);
    });
}

#[cfg(test)]
fn set_subtransaction_depth_for_test(depth: u32) {
    SUBTRANSACTION_DEPTH.with(|cell| cell.set(depth));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::GraphError;

    fn reject_if_subtransaction_for_test() -> Result<(), GraphError> {
        if subtransaction_active() {
            return Err(GraphError::UnsupportedOperation {
                operation: "mutable graph write inside a subtransaction".to_string(),
                reason: "transaction-local graph overlays do not yet support SAVEPOINT or PL subtransaction rollback".to_string(),
            });
        }
        Ok(())
    }

    #[test]
    fn empty_delta_reports_clean_stats() {
        clear_current_transaction_state();

        assert_eq!(stats(), TxDeltaStats::default());
    }

    #[test]
    fn stats_reflect_recorded_delta_contents() {
        clear_current_transaction_state();
        with_delta_for_test(|delta| {
            delta.add_node_for_test(100, "new-node", 42);
            delta.deleted_nodes.insert(7);
            delta.add_edge_for_test(
                42,
                DeltaEdge {
                    target: 7,
                    type_id: 1,
                    weight: Some(3),
                },
            );
            delta.deleted_edges.insert((1, 2, 1));
        });

        let stats = stats();

        assert_eq!(stats.added_nodes, 1);
        assert_eq!(stats.deleted_nodes, 1);
        assert_eq!(stats.added_edges, 1);
        assert_eq!(stats.deleted_edges, 1);
        assert!(stats.memory_bytes > 0);
        assert!(stats.dirty);
    }

    #[test]
    fn transaction_end_clears_delta_and_subtransaction_flag() {
        clear_current_transaction_state();
        with_delta_for_test(|delta| delta.add_node_for_test(100, "new-node", 42));
        set_subtransaction_depth_for_test(2);

        clear_current_transaction_state();

        assert_eq!(stats(), TxDeltaStats::default());
        assert!(!subtransaction_active());
    }

    #[test]
    fn nested_subtransaction_depth_survives_inner_commit() {
        clear_current_transaction_state();
        set_subtransaction_depth_for_test(2);

        decrement_subtransaction_depth();

        assert!(subtransaction_active());
        decrement_subtransaction_depth();
        assert!(!subtransaction_active());
    }

    #[test]
    fn subtransaction_abort_clears_delta_but_preserves_outer_depth() {
        clear_current_transaction_state();
        with_delta_for_test(|delta| delta.add_node_for_test(100, "new-node", 42));
        set_subtransaction_depth_for_test(2);

        clear_current_delta();
        decrement_subtransaction_depth();

        assert_eq!(stats(), TxDeltaStats::default());
        assert!(subtransaction_active());
    }

    #[test]
    fn subtransaction_rejection_is_explicit() {
        set_subtransaction_depth_for_test(1);

        let err =
            reject_if_subtransaction_for_test().expect_err("subtransaction should be rejected");

        assert!(matches!(err, GraphError::UnsupportedOperation { .. }));
        set_subtransaction_depth_for_test(0);
    }
}
