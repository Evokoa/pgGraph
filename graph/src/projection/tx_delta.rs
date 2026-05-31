//! Transaction-local projection delta storage.
//!
//! Mutable graph writes are applied to PostgreSQL first. After PostgreSQL
//! accepts the write, this module records the backend-local graph delta that
//! makes read-your-own-writes possible until transaction end.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::filter_index::EncodedFilterValue;
use crate::projection::neighbors::{EdgeOverlay, OverlayDeletes, OverlayInserts};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

/// Transaction-local node created by a graph write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddedNode {
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source table primary key.
    pub(crate) primary_key: String,
    /// Tenant scope active when the node was created.
    pub(crate) tenant: Option<String>,
    /// Assigned graph node index when the topology has materialized this row.
    pub(crate) node_idx: Option<u32>,
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
    filter_updates: HashMap<(usize, u32), Option<EncodedFilterValue>>,
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
    #[cfg(test)]
    static TEST_MAX_TX_DELTA_NODES: Cell<usize> = const { Cell::new(100_000) };
    #[cfg(test)]
    static TEST_MAX_TX_DELTA_EDGES: Cell<usize> = const { Cell::new(100_000) };
    #[cfg(test)]
    static TEST_MAX_OVERLAY_MEMORY_BYTES: Cell<usize> = const { Cell::new(256 * 1_048_576) };
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
        let node_tenant_bytes = self
            .added_nodes
            .iter()
            .filter_map(|node| node.tenant.as_ref())
            .map(String::capacity)
            .sum::<usize>();
        let added_edge_bytes = self
            .added_edges
            .values()
            .map(|edges| edges.capacity() * std::mem::size_of::<DeltaEdge>())
            .sum::<usize>();
        self.added_nodes.capacity() * std::mem::size_of::<AddedNode>()
            + node_pk_bytes
            + node_tenant_bytes
            + self.deleted_nodes.capacity() * std::mem::size_of::<u32>()
            + self.added_edges.capacity()
                * (std::mem::size_of::<u32>() + std::mem::size_of::<Vec<DeltaEdge>>())
            + added_edge_bytes
            + self.deleted_edges.capacity() * std::mem::size_of::<(u32, u32, u8)>()
            + self.filter_updates.capacity()
                * (std::mem::size_of::<(usize, u32)>()
                    + std::mem::size_of::<Option<EncodedFilterValue>>())
    }

    fn is_dirty(&self) -> bool {
        !self.added_nodes.is_empty()
            || !self.deleted_nodes.is_empty()
            || !self.added_edges.is_empty()
            || !self.deleted_edges.is_empty()
            || !self.filter_updates.is_empty()
    }

    #[cfg(test)]
    fn add_node_for_test(&mut self, table_oid: u32, primary_key: &str, node_idx: u32) {
        self.added_nodes.push(AddedNode {
            table_oid,
            primary_key: primary_key.to_string(),
            tenant: None,
            node_idx: Some(node_idx),
        });
    }

    #[cfg(test)]
    fn add_edge_for_test(&mut self, source: u32, edge: DeltaEdge) {
        self.added_edges.entry(source).or_default().push(edge);
    }
}

/// Record a transaction-local node insertion.
pub(crate) fn record_added_node(
    table_oid: u32,
    primary_key: &str,
    tenant: Option<&str>,
) -> GraphResult<()> {
    ensure_write_capacity(1, 0, estimated_added_node_bytes(primary_key, tenant))?;
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        delta.added_nodes.push(AddedNode {
            table_oid,
            primary_key: primary_key.to_string(),
            tenant: tenant.map(str::to_string),
            node_idx: None,
        });
    });
    Ok(())
}

/// Return transaction-local node primary keys for a table and tenant scope.
pub(crate) fn added_node_keys(table_oid: u32, tenant: Option<&str>) -> Vec<String> {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .map(|delta| {
                delta
                    .added_nodes
                    .iter()
                    .filter(|node| node.table_oid == table_oid)
                    .filter(|node| match (tenant, node.tenant.as_deref()) {
                        (Some(active), Some(created)) => active == created,
                        (Some(_), None) => true,
                        (None, _) => true,
                    })
                    .map(|node| node.primary_key.clone())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Record a transaction-local node deletion.
pub(crate) fn record_deleted_node(node_idx: u32) -> GraphResult<()> {
    ensure_write_capacity(1, 0, std::mem::size_of::<u32>())?;
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        delta.deleted_nodes.insert(node_idx);
    });
    Ok(())
}

/// Return whether a node has been deleted in the active transaction.
pub(crate) fn node_deleted(node_idx: u32) -> bool {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .is_some_and(|delta| delta.deleted_nodes.contains(&node_idx))
    })
}

/// Record a transaction-local typed filter-index value update.
pub(crate) fn record_filter_value_update(
    column_idx: usize,
    node_idx: u32,
    value: Option<EncodedFilterValue>,
) -> GraphResult<()> {
    ensure_write_capacity(0, 0, estimated_filter_update_bytes())?;
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        delta.filter_updates.insert((column_idx, node_idx), value);
    });
    Ok(())
}

/// Return a transaction-local typed filter-index value update.
pub(crate) fn filter_value_update(
    column_idx: usize,
    node_idx: u32,
) -> Option<Option<EncodedFilterValue>> {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .and_then(|delta| delta.filter_updates.get(&(column_idx, node_idx)).copied())
    })
}

/// Validate that the current transaction can accept additional graph deltas.
pub(crate) fn ensure_write_capacity(
    additional_nodes: usize,
    additional_edges: usize,
    additional_memory_bytes: usize,
) -> GraphResult<()> {
    let stats = stats();
    enforce_limit(
        "tx_delta_nodes",
        stats
            .added_nodes
            .saturating_add(stats.deleted_nodes)
            .saturating_add(additional_nodes),
        max_tx_delta_nodes(),
    )?;
    enforce_limit(
        "tx_delta_edges",
        stats
            .added_edges
            .saturating_add(stats.deleted_edges)
            .saturating_add(additional_edges),
        max_tx_delta_edges(),
    )?;
    enforce_limit(
        "overlay_memory_bytes",
        stats.memory_bytes.saturating_add(additional_memory_bytes),
        max_overlay_memory_bytes(),
    )?;
    reject_if_subtransaction()
}

#[cfg(not(test))]
fn max_tx_delta_nodes() -> usize {
    crate::config::max_tx_delta_nodes()
}

#[cfg(test)]
fn max_tx_delta_nodes() -> usize {
    TEST_MAX_TX_DELTA_NODES.with(Cell::get)
}

#[cfg(not(test))]
fn max_tx_delta_edges() -> usize {
    crate::config::max_tx_delta_edges()
}

#[cfg(test)]
fn max_tx_delta_edges() -> usize {
    TEST_MAX_TX_DELTA_EDGES.with(Cell::get)
}

#[cfg(not(test))]
fn max_overlay_memory_bytes() -> usize {
    crate::config::max_overlay_memory_bytes()
}

#[cfg(test)]
fn max_overlay_memory_bytes() -> usize {
    TEST_MAX_OVERLAY_MEMORY_BYTES.with(Cell::get)
}

/// Record a transaction-local edge insertion.
#[allow(
    dead_code,
    reason = "Phase 2C write operators call this after PostgreSQL accepts edge DML"
)]
pub(crate) fn record_added_edge(source: u32, edge: DeltaEdge) -> GraphResult<()> {
    ensure_write_capacity(0, 1, estimated_added_edge_bytes())?;
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        delta
            .deleted_edges
            .remove(&(source, edge.target, edge.type_id));
        delta.added_edges.entry(source).or_default().push(edge);
    });
    Ok(())
}

/// Record a transaction-local edge deletion.
#[allow(
    dead_code,
    reason = "Phase 2E write operators call this after PostgreSQL accepts edge DML"
)]
pub(crate) fn record_deleted_edge(source: u32, target: u32, type_id: u8) -> GraphResult<()> {
    ensure_write_capacity(0, 1, estimated_deleted_edge_bytes())?;
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        if let Some(edges) = delta.added_edges.get_mut(&source) {
            edges.retain(|edge| edge.target != target || edge.type_id != type_id);
            if edges.is_empty() {
                delta.added_edges.remove(&source);
            }
        }
        delta.deleted_edges.insert((source, target, type_id));
    });
    Ok(())
}

/// Return whether transaction-local edge deltas are present.
pub(crate) fn edge_delta_dirty() -> bool {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .is_some_and(|delta| !delta.added_edges.is_empty() || !delta.deleted_edges.is_empty())
    })
}

/// Return edge overlay maps for the requested traversal direction.
pub(crate) fn edge_overlay(direction: TraversalDirection) -> EdgeOverlay {
    TX_DELTA.with(|delta| {
        let borrowed = delta.borrow();
        let Some(delta) = borrowed.as_ref() else {
            return (OverlayInserts::new(), OverlayDeletes::new());
        };

        let mut inserts = OverlayInserts::new();
        for (&source, edges) in &delta.added_edges {
            for edge in edges {
                let (source, target) = orient_edge(direction, source, edge.target);
                inserts
                    .entry(source)
                    .or_default()
                    .push((target, edge.type_id));
            }
        }

        let mut deletes = OverlayDeletes::new();
        for &(source, target, type_id) in &delta.deleted_edges {
            let (source, target) = orient_edge(direction, source, target);
            deletes.entry(source).or_default().insert((target, type_id));
        }

        (inserts, deletes)
    })
}

fn enforce_limit(kind: &str, requested: usize, limit: usize) -> GraphResult<()> {
    if requested > limit {
        return Err(GraphError::OverlayLimit {
            kind: kind.to_string(),
            requested,
            limit,
        });
    }
    Ok(())
}

fn estimated_added_node_bytes(primary_key: &str, tenant: Option<&str>) -> usize {
    std::mem::size_of::<AddedNode>()
        .saturating_add(primary_key.len())
        .saturating_add(tenant.map(str::len).unwrap_or_default())
}

fn estimated_added_edge_bytes() -> usize {
    std::mem::size_of::<u32>()
        .saturating_add(std::mem::size_of::<Vec<DeltaEdge>>())
        .saturating_add(std::mem::size_of::<DeltaEdge>())
}

fn estimated_deleted_edge_bytes() -> usize {
    std::mem::size_of::<(u32, u32, u8)>()
}

fn estimated_filter_update_bytes() -> usize {
    std::mem::size_of::<(usize, u32)>()
        .saturating_add(std::mem::size_of::<Option<EncodedFilterValue>>())
}

fn orient_edge(direction: TraversalDirection, source: u32, target: u32) -> (u32, u32) {
    match direction {
        TraversalDirection::Any | TraversalDirection::Out => (source, target),
        TraversalDirection::In => (target, source),
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

fn subtransaction_active() -> bool {
    SUBTRANSACTION_DEPTH.with(|depth| depth.get() > 0)
}

fn reject_if_subtransaction() -> GraphResult<()> {
    if subtransaction_active() {
        return Err(GraphError::UnsupportedOperation {
            operation: "mutable graph write inside a subtransaction".to_string(),
            reason:
                "transaction-local graph overlays reject SAVEPOINT and PL subtransaction writes"
                    .to_string(),
        });
    }
    Ok(())
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
fn set_test_limits(nodes: usize, edges: usize, memory_bytes: usize) {
    TEST_MAX_TX_DELTA_NODES.with(|cell| cell.set(nodes));
    TEST_MAX_TX_DELTA_EDGES.with(|cell| cell.set(edges));
    TEST_MAX_OVERLAY_MEMORY_BYTES.with(|cell| cell.set(memory_bytes));
}

#[cfg(test)]
pub(crate) fn clear_for_test() {
    clear_current_transaction_state();
    set_test_limits(100_000, 100_000, 256 * 1_048_576);
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn stats_include_tenant_string_heap_for_added_nodes() {
        clear_current_transaction_state();
        record_added_node(100, "n1", Some("tenant-a")).expect("record tenant node");

        let with_tenant = stats().memory_bytes;

        clear_current_transaction_state();
        record_added_node(100, "n1", None).expect("record unscoped node");
        let without_tenant = stats().memory_bytes;

        assert!(with_tenant > without_tenant);
        clear_current_transaction_state();
    }

    #[test]
    fn added_node_keys_respect_recorded_tenant_scope() {
        clear_current_transaction_state();
        record_added_node(100, "a1", Some("tenant-a")).expect("record tenant-a");
        record_added_node(100, "b1", Some("tenant-b")).expect("record tenant-b");
        record_added_node(200, "other", Some("tenant-a")).expect("record other table");

        assert_eq!(added_node_keys(100, Some("tenant-a")), vec!["a1"]);
        assert_eq!(added_node_keys(100, Some("tenant-b")), vec!["b1"]);
        assert_eq!(added_node_keys(100, None), vec!["a1", "b1"]);

        clear_current_transaction_state();
    }

    #[test]
    fn filter_value_updates_are_transaction_local() {
        clear_current_transaction_state();
        record_filter_value_update(2, 42, Some(EncodedFilterValue::Numeric(101)))
            .expect("record filter update");

        assert_eq!(
            filter_value_update(2, 42),
            Some(Some(EncodedFilterValue::Numeric(101)))
        );
        assert!(stats().dirty);

        clear_current_transaction_state();
        assert_eq!(filter_value_update(2, 42), None);
    }

    #[test]
    fn deleted_nodes_are_transaction_local() {
        clear_current_transaction_state();
        record_deleted_node(42).expect("record node tombstone");

        assert!(node_deleted(42));
        assert!(!node_deleted(43));
        assert_eq!(stats().deleted_nodes, 1);

        clear_current_transaction_state();
        assert!(!node_deleted(42));
    }

    #[test]
    fn edge_overlay_normalizes_local_insert_delete_order() {
        clear_current_transaction_state();

        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: 1,
                weight: None,
            },
        )
        .expect("record insert");
        record_deleted_edge(1, 2, 1).expect("record delete");
        let (inserts, deletes) = edge_overlay(TraversalDirection::Out);
        assert!(inserts.is_empty());
        assert!(deletes.get(&1).is_some_and(|edges| edges.contains(&(2, 1))));

        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: 1,
                weight: None,
            },
        )
        .expect("record insert after delete");
        let (inserts, deletes) = edge_overlay(TraversalDirection::In);
        assert!(deletes.is_empty());
        assert!(inserts.get(&2).is_some_and(|edges| edges.contains(&(1, 1))));
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
    fn subtransaction_abort_preserves_outer_delta_and_depth() {
        clear_current_transaction_state();
        with_delta_for_test(|delta| delta.add_node_for_test(100, "new-node", 42));
        set_subtransaction_depth_for_test(2);

        decrement_subtransaction_depth();

        assert!(stats().dirty);
        assert!(subtransaction_active());
    }

    #[test]
    fn subtransaction_rejection_is_explicit() {
        set_subtransaction_depth_for_test(1);

        let err = record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: 1,
                weight: None,
            },
        )
        .expect_err("subtransaction should be rejected");

        assert!(matches!(err, GraphError::UnsupportedOperation { .. }));
        set_subtransaction_depth_for_test(0);
    }

    #[test]
    fn capacity_rejection_reports_overlay_limit_before_subtransaction_guard() {
        clear_current_transaction_state();
        set_subtransaction_depth_for_test(1);

        let err = ensure_write_capacity(100_001, 0, 0)
            .expect_err("node capacity should reject before subtransaction");

        assert!(matches!(
            err,
            GraphError::OverlayLimit { kind, .. } if kind == "tx_delta_nodes"
        ));
        set_subtransaction_depth_for_test(0);
    }

    #[test]
    fn overlay_memory_limit_rejects_before_recording_delta() {
        clear_current_transaction_state();
        set_test_limits(100_000, 100_000, 8);

        let err =
            record_added_node(100, "long-primary-key", None).expect_err("memory cap should reject");

        assert!(matches!(
            err,
            GraphError::OverlayLimit { kind, .. } if kind == "overlay_memory_bytes"
        ));
        assert_eq!(stats(), TxDeltaStats::default());
        clear_for_test();
    }
}
