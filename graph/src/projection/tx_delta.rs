//! Transaction-local projection delta storage.
//!
//! Mutable graph writes are applied to PostgreSQL first. After PostgreSQL
//! accepts the write, this module records the backend-local graph delta that
//! makes read-your-own-writes possible until transaction end.

use roaring::RoaringBitmap;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::edge_store::{RelationshipId, RelationshipIdentity};
use crate::edge_type_registry::EdgeTypeRegistry;
use crate::filter_index::EncodedFilterValue;
use crate::projection::neighbors::{EdgeOverlay, OverlayDeletes, OverlayInserts};
use crate::safety::{GraphError, GraphResult};
use crate::types::{EdgeTypeId, TraversalDirection};

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
    pub(crate) type_id: EdgeTypeId,
    /// Whether this row is a synthetic reverse of the schema edge.
    pub(crate) schema_reversed: bool,
    /// Optional weight captured from a mapped edge row.
    pub(crate) weight: Option<u32>,
    /// Stable relationship identity for mapped edge rows created in this
    /// transaction.
    pub(crate) relationship_id: Option<RelationshipId>,
}

/// Per-transaction graph projection delta.
#[derive(Debug, Clone)]
pub(crate) struct TxGraphDelta {
    added_nodes: Vec<AddedNode>,
    max_added_node_primary_key_bytes: usize,
    deleted_nodes: HashSet<u32>,
    added_edges: HashMap<u32, Vec<DeltaEdge>>,
    deleted_edges: HashSet<(u32, u32, EdgeTypeId, bool, Option<RelationshipId>)>,
    filter_updates: HashMap<(usize, u32), Option<EncodedFilterValue>>,
    relationship_identities: Vec<RelationshipIdentity>,
    missing_relationship_identity_edge_types: RoaringBitmap,
    edge_type_base_len: Option<usize>,
    edge_type_base_fingerprint: Option<u64>,
    edge_type_base_payload_bytes: usize,
    appended_edge_type_payload_bytes: usize,
    appended_edge_type_max_label_bytes: usize,
    appended_edge_type_labels: Vec<String>,
    appended_edge_type_ids: HashMap<String, EdgeTypeId>,
}

impl Default for TxGraphDelta {
    fn default() -> Self {
        Self {
            added_nodes: Vec::new(),
            max_added_node_primary_key_bytes: 0,
            deleted_nodes: HashSet::new(),
            added_edges: HashMap::new(),
            deleted_edges: HashSet::new(),
            filter_updates: HashMap::new(),
            relationship_identities: Vec::new(),
            missing_relationship_identity_edge_types: RoaringBitmap::new(),
            edge_type_base_len: None,
            edge_type_base_fingerprint: None,
            edge_type_base_payload_bytes: 0,
            appended_edge_type_payload_bytes: 0,
            appended_edge_type_max_label_bytes: 0,
            appended_edge_type_labels: Vec::new(),
            appended_edge_type_ids: HashMap::new(),
        }
    }
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
    /// Transaction-local filter value updates.
    pub(crate) filter_updates: usize,
    /// Estimated heap bytes owned by the transaction delta.
    pub(crate) memory_bytes: usize,
    /// Whether any graph delta is currently recorded.
    pub(crate) dirty: bool,
}

thread_local! {
    static TX_DELTA: RefCell<Option<TxGraphDelta>> = const { RefCell::new(None) };
    static TX_TOPOLOGY_REVISION: Cell<u64> = const { Cell::new(0) };
    static SUBTRANSACTION_DEPTH: Cell<u32> = const { Cell::new(0) };
    static SUBTRANSACTION_SNAPSHOTS: RefCell<Vec<Option<TxGraphDelta>>> = const { RefCell::new(Vec::new()) };
    #[cfg(test)]
    static TEST_MAX_TX_DELTA_NODES: Cell<usize> = const { Cell::new(100_000) };
    #[cfg(test)]
    static TEST_MAX_TX_DELTA_EDGES: Cell<usize> = const { Cell::new(100_000) };
    #[cfg(test)]
    static TEST_MAX_OVERLAY_MEMORY_BYTES: Cell<usize> = const { Cell::new(256 * 1_048_576) };
}

fn bump_topology_revision() {
    TX_TOPOLOGY_REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
}

/// Return a backend-local monotonic revision for edge and relationship-identity
/// state used by resumable traversal.
pub(crate) fn topology_revision() -> u64 {
    TX_TOPOLOGY_REVISION.with(Cell::get)
}

/// Resolve a transaction-local relationship type appended after the loaded
/// base registry. The base length pins provisional IDs to this transaction's
/// projection generation.
pub(crate) fn edge_type_id(
    base_len: usize,
    base_fingerprint: u64,
    label: &str,
) -> Option<EdgeTypeId> {
    TX_DELTA.with(|delta| {
        delta.borrow().as_ref().and_then(|delta| {
            (delta.edge_type_base_len == Some(base_len)
                && delta.edge_type_base_fingerprint == Some(base_fingerprint))
            .then(|| delta.appended_edge_type_ids.get(label).copied())
            .flatten()
        })
    })
}

/// Resolve a provisional transaction-local relationship type ID to its exact
/// source spelling.
pub(crate) fn edge_type_label(
    base_len: usize,
    base_fingerprint: u64,
    type_id: EdgeTypeId,
) -> Option<String> {
    let id = type_id.get() as usize;
    let offset = id.checked_sub(base_len)?;
    TX_DELTA.with(|delta| {
        delta.borrow().as_ref().and_then(|delta| {
            (delta.edge_type_base_len == Some(base_len)
                && delta.edge_type_base_fingerprint == Some(base_fingerprint))
            .then(|| delta.appended_edge_type_labels.get(offset).cloned())
            .flatten()
        })
    })
}

/// Total logical relationship-type slots visible against the pinned base
/// registry, including the reserved untyped slot.
pub(crate) fn edge_type_count(base_len: usize, base_fingerprint: u64) -> usize {
    TX_DELTA.with(|delta| {
        delta.borrow().as_ref().map_or(base_len, |delta| {
            if delta.edge_type_base_len == Some(base_len)
                && delta.edge_type_base_fingerprint == Some(base_fingerprint)
            {
                base_len.saturating_add(delta.appended_edge_type_labels.len())
            } else {
                base_len
            }
        })
    })
}

pub(crate) fn max_edge_type_label_bytes(base_len: usize, base_fingerprint: u64) -> usize {
    TX_DELTA.with(|delta| {
        delta.borrow().as_ref().map_or(0, |delta| {
            if delta.edge_type_base_len == Some(base_len)
                && delta.edge_type_base_fingerprint == Some(base_fingerprint)
            {
                delta.appended_edge_type_max_label_bytes
            } else {
                0
            }
        })
    })
}

/// Return whether transaction-local relationship types pin the currently
/// loaded projection registry.
pub(crate) fn has_provisional_edge_types() -> bool {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .is_some_and(|delta| !delta.appended_edge_type_labels.is_empty())
    })
}

/// Reject replacing the loaded projection while provisional type IDs depend
/// on its exact registry ordering.
pub(crate) fn ensure_engine_replacement_allowed(operation: &str) -> GraphResult<()> {
    if has_provisional_edge_types() {
        return Err(GraphError::ReadOnly {
            reason: format!(
                "{operation} cannot replace a graph pinned by transaction-local relationship types; commit or roll back first"
            ),
        });
    }
    Ok(())
}

/// Intern one unseen exact source spelling into transaction-local state.
/// PostgreSQL DML must have accepted the relationship row before this is
/// called. Subtransaction snapshots own the appended dictionary automatically.
pub(crate) fn intern_edge_type(
    base_registry: &EdgeTypeRegistry,
    label: &str,
) -> GraphResult<EdgeTypeId> {
    let base_labels = base_registry.as_slice();
    let base_fingerprint = base_registry.fingerprint();
    if label.is_empty() {
        return Err(GraphError::EdgeTypeLimit);
    }
    if let Some(id) = edge_type_id(base_labels.len(), base_fingerprint, label) {
        return Ok(id);
    }
    let base_payload_bytes = base_registry.label_bytes();
    let (appended_count, appended_payload_bytes, initialized_base_len) = TX_DELTA.with(|delta| {
        let borrowed = delta.borrow();
        let delta = borrowed.as_ref();
        (
            delta.map_or(0, |delta| delta.appended_edge_type_labels.len()),
            delta.map_or(0, |delta| delta.appended_edge_type_payload_bytes),
            delta.and_then(|delta| delta.edge_type_base_len),
        )
    });
    let initialized_fingerprint = TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .and_then(|delta| delta.edge_type_base_fingerprint)
    });
    if initialized_base_len.is_some_and(|base_len| base_len != base_labels.len())
        || initialized_fingerprint.is_some_and(|fingerprint| fingerprint != base_fingerprint)
    {
        return Err(GraphError::UnsupportedOperation {
            operation: "transaction-local relationship type".into(),
            reason: "the loaded relationship-type registry changed during this transaction".into(),
        });
    }
    let user_count = base_labels
        .len()
        .saturating_sub(1)
        .saturating_add(appended_count)
        .saturating_add(1);
    let payload_bytes = base_payload_bytes
        .checked_add(appended_payload_bytes)
        .and_then(|bytes| bytes.checked_add(label.len()))
        .ok_or(GraphError::EdgeTypeLimit)?;
    if user_count > crate::edge_type_registry::EdgeTypeRegistry::MAX_USER_EDGE_TYPES
        || label.len() > crate::edge_type_registry::EdgeTypeRegistry::MAX_EDGE_TYPE_LABEL_BYTES
        || payload_bytes
            > crate::edge_type_registry::EdgeTypeRegistry::MAX_EDGE_TYPE_DICTIONARY_BYTES
    {
        return Err(GraphError::EdgeTypeLimit);
    }
    let growth = TX_DELTA.with(|delta| {
        delta.borrow().as_ref().map_or_else(
            || provisional_edge_type_growth_bound(0, 0, 0, 0, label.len()),
            |delta| {
                provisional_edge_type_growth_bound(
                    delta.appended_edge_type_labels.len(),
                    delta.appended_edge_type_labels.capacity(),
                    delta.appended_edge_type_ids.len(),
                    delta.appended_edge_type_ids.capacity(),
                    label.len(),
                )
            },
        )
    })?;
    ensure_write_capacity(0, 0, growth)?;
    let result = TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        if delta.edge_type_base_len.is_none() {
            delta.edge_type_base_len = Some(base_labels.len());
            delta.edge_type_base_fingerprint = Some(base_fingerprint);
            delta.edge_type_base_payload_bytes = base_payload_bytes;
        }
        if let Some(id) = delta.appended_edge_type_ids.get(label).copied() {
            return Ok(id);
        }
        delta
            .appended_edge_type_labels
            .try_reserve(1)
            .map_err(|error| {
                GraphError::Internal(format!("edge type label allocation failed: {error}"))
            })?;
        delta
            .appended_edge_type_ids
            .try_reserve(1)
            .map_err(|error| {
                GraphError::Internal(format!("edge type lookup allocation failed: {error}"))
            })?;
        let raw_id = base_labels
            .len()
            .checked_add(delta.appended_edge_type_labels.len())
            .ok_or(GraphError::EdgeTypeLimit)?;
        let id =
            EdgeTypeId::try_from(u32::try_from(raw_id).map_err(|_| GraphError::EdgeTypeLimit)?)
                .map_err(|_| GraphError::EdgeTypeLimit)?;
        let next_appended_payload_bytes = delta
            .appended_edge_type_payload_bytes
            .checked_add(label.len())
            .ok_or(GraphError::EdgeTypeLimit)?;
        let ordered = try_clone_edge_type_label(label)?;
        let lookup = try_clone_edge_type_label(label)?;
        delta.appended_edge_type_labels.push(ordered);
        delta.appended_edge_type_ids.insert(lookup, id);
        delta.appended_edge_type_payload_bytes = next_appended_payload_bytes;
        delta.appended_edge_type_max_label_bytes =
            delta.appended_edge_type_max_label_bytes.max(label.len());
        Ok(id)
    });
    if result.is_ok() {
        bump_topology_revision();
    }
    result
}

fn try_clone_edge_type_label(label: &str) -> GraphResult<String> {
    let mut owned = String::new();
    owned.try_reserve_exact(label.len()).map_err(|error| {
        GraphError::Internal(format!("edge type label allocation failed: {error}"))
    })?;
    owned.push_str(label);
    Ok(owned)
}

fn provisional_edge_type_growth_bound(
    ordered_len: usize,
    ordered_capacity: usize,
    lookup_len: usize,
    lookup_capacity: usize,
    label_bytes: usize,
) -> GraphResult<usize> {
    let next_ordered = ordered_len
        .checked_add(1)
        .ok_or(GraphError::EdgeTypeLimit)?;
    let next_lookup = lookup_len.checked_add(1).ok_or(GraphError::EdgeTypeLimit)?;
    let ordered_target = provisional_collection_capacity_bound(ordered_capacity, next_ordered)?;
    let lookup_target = provisional_collection_capacity_bound(lookup_capacity, next_lookup)?;
    let ordered_growth = ordered_target
        .saturating_sub(ordered_capacity)
        .checked_mul(std::mem::size_of::<String>())
        .ok_or(GraphError::EdgeTypeLimit)?;
    let lookup_growth = lookup_target
        .saturating_sub(lookup_capacity)
        .checked_mul(std::mem::size_of::<(String, EdgeTypeId)>() + 32)
        .ok_or(GraphError::EdgeTypeLimit)?;
    ordered_growth
        .checked_add(lookup_growth)
        .and_then(|bytes| bytes.checked_add(label_bytes.checked_mul(2)?))
        .ok_or(GraphError::EdgeTypeLimit)
}

fn provisional_collection_capacity_bound(current: usize, required: usize) -> GraphResult<usize> {
    if current >= required {
        return Ok(current);
    }
    required
        .max(1)
        .checked_next_power_of_two()
        .and_then(|capacity| capacity.checked_mul(4))
        .ok_or(GraphError::EdgeTypeLimit)
}

static CALLBACKS_REGISTERED: AtomicBool = AtomicBool::new(false);

impl TxGraphDelta {
    fn estimated_appended_edge_type_heap_bytes(&self) -> usize {
        self.appended_edge_type_labels.capacity() * std::mem::size_of::<String>()
            + self
                .appended_edge_type_labels
                .iter()
                .map(String::capacity)
                .sum::<usize>()
            + self.appended_edge_type_ids.capacity()
                * (std::mem::size_of::<(String, EdgeTypeId)>() + 32)
            + self
                .appended_edge_type_ids
                .keys()
                .map(String::capacity)
                .sum::<usize>()
    }

    fn refresh_relationship_identity_completeness_summary(&mut self) {
        self.missing_relationship_identity_edge_types.clear();
        for edge in self.added_edges.values().flatten() {
            if edge.relationship_id.is_none() {
                self.missing_relationship_identity_edge_types
                    .insert(edge.type_id.get());
            }
        }
    }

    fn stats(&self) -> TxDeltaStats {
        let added_edges = self.added_edges.values().map(Vec::len).sum::<usize>();
        let memory_bytes = self.estimated_heap_bytes();
        TxDeltaStats {
            added_nodes: self.added_nodes.len(),
            deleted_nodes: self.deleted_nodes.len(),
            added_edges,
            deleted_edges: self.deleted_edges.len(),
            filter_updates: self.filter_updates.len(),
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
        let relationship_identity_key_bytes = self
            .relationship_identities
            .iter()
            .map(|identity| identity.source_key.capacity())
            .sum::<usize>();
        self.added_nodes.capacity() * std::mem::size_of::<AddedNode>()
            + node_pk_bytes
            + node_tenant_bytes
            + self.deleted_nodes.capacity() * std::mem::size_of::<u32>()
            + self.added_edges.capacity()
                * (std::mem::size_of::<u32>() + std::mem::size_of::<Vec<DeltaEdge>>())
            + added_edge_bytes
            + self.deleted_edges.capacity()
                * std::mem::size_of::<(u32, u32, EdgeTypeId, bool, Option<RelationshipId>)>()
            + self.filter_updates.capacity()
                * (std::mem::size_of::<(usize, u32)>()
                    + std::mem::size_of::<Option<EncodedFilterValue>>())
            + self.relationship_identities.capacity() * std::mem::size_of::<RelationshipIdentity>()
            + relationship_identity_key_bytes
            + self.estimated_appended_edge_type_heap_bytes()
    }

    fn is_dirty(&self) -> bool {
        !self.added_nodes.is_empty()
            || !self.deleted_nodes.is_empty()
            || !self.added_edges.is_empty()
            || !self.deleted_edges.is_empty()
            || !self.filter_updates.is_empty()
            || !self.relationship_identities.is_empty()
            || !self.appended_edge_type_labels.is_empty()
    }

    #[cfg(test)]
    fn add_node_for_test(&mut self, table_oid: u32, primary_key: &str, node_idx: u32) {
        self.max_added_node_primary_key_bytes =
            self.max_added_node_primary_key_bytes.max(primary_key.len());
        self.added_nodes.push(AddedNode {
            table_oid,
            primary_key: primary_key.to_string(),
            tenant: None,
            node_idx: Some(node_idx),
        });
    }

    #[cfg(test)]
    fn add_edge_for_test(&mut self, source: u32, edge: DeltaEdge) {
        if edge.relationship_id.is_none() {
            self.missing_relationship_identity_edge_types
                .insert(edge.type_id.get());
        }
        self.added_edges.entry(source).or_default().push(edge);
    }
}

/// Record a transaction-local node insertion.
#[allow(
    dead_code,
    reason = "unit tests use the unindexed form to exercise node-only visibility without traversal"
)]
pub(crate) fn record_added_node(
    table_oid: u32,
    primary_key: &str,
    tenant: Option<&str>,
) -> GraphResult<()> {
    ensure_write_capacity(1, 0, estimated_added_node_bytes(primary_key, tenant))?;
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        delta.max_added_node_primary_key_bytes = delta
            .max_added_node_primary_key_bytes
            .max(primary_key.len());
        delta.added_nodes.push(AddedNode {
            table_oid,
            primary_key: primary_key.to_string(),
            tenant: tenant.map(str::to_string),
            node_idx: None,
        });
    });
    bump_topology_revision();
    Ok(())
}

/// Record a transaction-local node insertion and assign a temporary graph index.
pub(crate) fn record_added_node_indexed(
    table_oid: u32,
    primary_key: &str,
    tenant: Option<&str>,
    base_node_count: u32,
) -> GraphResult<u32> {
    ensure_write_capacity(1, 0, estimated_added_node_bytes(primary_key, tenant))?;
    let result = TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        delta.max_added_node_primary_key_bytes = delta
            .max_added_node_primary_key_bytes
            .max(primary_key.len());
        let offset = delta
            .added_nodes
            .iter()
            .filter(|node| node.node_idx.is_some())
            .count();
        let offset = u32::try_from(offset).map_err(|_| GraphError::OverlayLimit {
            kind: "tx_delta_nodes".to_string(),
            requested: usize::MAX,
            limit: max_tx_delta_nodes(),
        })?;
        let node_idx =
            base_node_count
                .checked_add(offset)
                .ok_or_else(|| GraphError::OverlayLimit {
                    kind: "tx_delta_nodes".to_string(),
                    requested: usize::MAX,
                    limit: max_tx_delta_nodes(),
                })?;
        delta.added_nodes.push(AddedNode {
            table_oid,
            primary_key: primary_key.to_string(),
            tenant: tenant.map(str::to_string),
            node_idx: Some(node_idx),
        });
        Ok(node_idx)
    });
    if result.is_ok() {
        bump_topology_revision();
    }
    result
}

/// Return transaction-local node primary keys for a table and tenant scope.
pub(crate) fn added_node_keys(
    table_oid: u32,
    tenant: Option<&str>,
    table_is_tenanted: bool,
) -> Vec<String> {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .map(|delta| {
                delta
                    .added_nodes
                    .iter()
                    .filter(|node| node.table_oid == table_oid)
                    .filter(
                        |node| match (tenant, node.tenant.as_deref(), table_is_tenanted) {
                            (Some(active), Some(created), true) => active == created,
                            (Some(_), None, true) => false,
                            (Some(_), _, false) => true,
                            (None, _, _) => true,
                        },
                    )
                    .map(|node| node.primary_key.clone())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Return transaction-local node indexes for a table and tenant scope.
pub(crate) fn added_node_indexes(
    table_oid: u32,
    tenant: Option<&str>,
    table_is_tenanted: bool,
) -> Vec<u32> {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .map(|delta| {
                delta
                    .added_nodes
                    .iter()
                    .filter(|node| node.table_oid == table_oid)
                    .filter(
                        |node| match (tenant, node.tenant.as_deref(), table_is_tenanted) {
                            (Some(active), Some(created), true) => active == created,
                            (Some(_), None, true) => false,
                            (Some(_), _, false) => true,
                            (None, _, _) => true,
                        },
                    )
                    .filter_map(|node| node.node_idx)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Return the largest transaction-local node primary-key byte length.
pub(crate) fn max_added_node_primary_key_bytes() -> usize {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .map(|delta| delta.max_added_node_primary_key_bytes)
            .unwrap_or_default()
    })
}

/// Resolve a transaction-local node to its temporary graph index.
pub(crate) fn resolve_added_node(
    table_oid: u32,
    primary_key: &str,
    tenant: Option<&str>,
    table_is_tenanted: bool,
) -> Option<u32> {
    TX_DELTA.with(|delta| {
        delta.borrow().as_ref().and_then(|delta| {
            delta
                .added_nodes
                .iter()
                .find(|node| {
                    node.table_oid == table_oid
                        && node.primary_key == primary_key
                        && match (tenant, node.tenant.as_deref(), table_is_tenanted) {
                            (Some(active), Some(created), true) => active == created,
                            (Some(_), None, true) => false,
                            (Some(_), _, false) => true,
                            (None, _, _) => true,
                        }
                })
                .and_then(|node| node.node_idx)
        })
    })
}

/// Return metadata for a transaction-local temporary graph index.
pub(crate) fn added_node_by_index(node_idx: u32) -> Option<AddedNode> {
    TX_DELTA.with(|delta| {
        delta.borrow().as_ref().and_then(|delta| {
            delta
                .added_nodes
                .iter()
                .find(|node| node.node_idx == Some(node_idx))
                .cloned()
        })
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
    bump_topology_revision();
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
    bump_topology_revision();
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
    ensure_transaction_callbacks_registered();
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
        stats
            .memory_bytes
            .saturating_add(pending_snapshot_bytes())
            .saturating_add(additional_memory_bytes),
        max_overlay_memory_bytes(),
    )?;
    snapshot_active_subtransactions();
    Ok(())
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
    let cancels_delete = TX_DELTA.with(|delta| {
        delta.borrow().as_ref().is_some_and(|delta| {
            delta.deleted_edges.iter().any(
                |&(deleted_source, target, type_id, schema_reversed, relationship_id)| {
                    deleted_source == source
                        && target == edge.target
                        && type_id == edge.type_id
                        && schema_reversed == edge.schema_reversed
                        && relationship_id == edge.relationship_id
                },
            )
        })
    });
    if cancels_delete {
        ensure_write_capacity(0, 0, 0)?;
    } else {
        ensure_write_capacity(0, 1, estimated_added_edge_bytes())?;
    }
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        let deleted_key = delta.deleted_edges.iter().copied().find(|deleted| {
            deleted.0 == source
                && deleted.1 == edge.target
                && deleted.2 == edge.type_id
                && deleted.3 == edge.schema_reversed
                && deleted.4 == edge.relationship_id
        });
        if deleted_key.is_some_and(|key| delta.deleted_edges.remove(&key)) {
            return;
        }
        if edge.relationship_id.is_none() {
            delta
                .missing_relationship_identity_edge_types
                .insert(edge.type_id.get());
        }
        delta.added_edges.entry(source).or_default().push(edge);
    });
    bump_topology_revision();
    Ok(())
}

/// Intern a transaction-local relationship identity after PostgreSQL accepts
/// the source edge row.
pub(crate) fn record_relationship_identity(
    base_identity_count: usize,
    identity: RelationshipIdentity,
) -> GraphResult<RelationshipId> {
    ensure_write_capacity(0, 0, estimated_relationship_identity_bytes(&identity))?;
    let result = TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        let offset = delta.relationship_identities.len();
        let id = base_identity_count.checked_add(offset).ok_or_else(|| {
            GraphError::Internal("relationship identity count overflowed usize".to_string())
        })?;
        let id = RelationshipId::try_from(id).map_err(|_| {
            GraphError::Internal(format!("relationship identity `{id}` is out of range"))
        })?;
        delta.relationship_identities.push(identity);
        Ok(id)
    });
    if result.is_ok() {
        bump_topology_revision();
    }
    result
}

/// Return transaction-local relationship identities in allocation order.
#[cfg(test)]
pub(crate) fn relationship_identities() -> Vec<RelationshipIdentity> {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .map(|delta| delta.relationship_identities.clone())
            .unwrap_or_default()
    })
}

/// Borrow one transaction-local relationship identity by allocation offset.
pub(crate) fn with_relationship_identity<R>(
    offset: usize,
    read: impl FnOnce(Option<&RelationshipIdentity>) -> R,
) -> R {
    TX_DELTA.with(|delta| {
        let borrowed = delta.borrow();
        read(
            borrowed
                .as_ref()
                .and_then(|delta| delta.relationship_identities.get(offset)),
        )
    })
}

/// Visit transaction-local relationship identities with their projection IDs.
pub(crate) fn for_each_relationship_identity(
    base_identity_count: usize,
    mut visit: impl FnMut(RelationshipId, &RelationshipIdentity),
) {
    TX_DELTA.with(|delta| {
        let borrowed = delta.borrow();
        let Some(delta) = borrowed.as_ref() else {
            return;
        };
        for (offset, identity) in delta.relationship_identities.iter().enumerate() {
            let Some(index) = base_identity_count.checked_add(offset) else {
                continue;
            };
            let Ok(id) = RelationshipId::try_from(index) else {
                continue;
            };
            visit(id, identity);
        }
    });
}

/// Return whether a transaction-local inserted edge of a requested type lacks
/// a stable relationship identity.
///
/// Transaction deltas are bounded independently of the base projection. This
/// check deliberately stays inside the delta owner so subtransaction restore
/// and abort semantics remain authoritative.
pub(crate) fn has_missing_relationship_identity_for_types(
    active_edge_types: &RoaringBitmap,
) -> bool {
    TX_DELTA.with(|delta| {
        delta.borrow().as_ref().is_some_and(|delta| {
            active_edge_types.iter().any(|edge_type| {
                delta
                    .missing_relationship_identity_edge_types
                    .contains(edge_type)
            })
        })
    })
}

pub(crate) fn has_any_missing_relationship_identity() -> bool {
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .is_some_and(|delta| !delta.missing_relationship_identity_edge_types.is_empty())
    })
}

/// Resolve a transaction-local relationship source identity.
pub(crate) fn find_relationship_identity_id(
    base_identity_count: usize,
    mapping_id: u64,
    source_key: &str,
) -> Option<RelationshipId> {
    let mut found = None;
    for_each_relationship_identity(base_identity_count, |id, identity| {
        if found.is_none() && identity.mapping_id == mapping_id && identity.source_key == source_key
        {
            found = Some(id);
        }
    });
    found
}

/// Record a transaction-local edge deletion.
#[allow(
    dead_code,
    reason = "Phase 2E write operators call this after PostgreSQL accepts edge DML"
)]
pub(crate) fn record_deleted_edge(
    source: u32,
    target: u32,
    type_id: EdgeTypeId,
) -> GraphResult<()> {
    record_deleted_edge_with_identity(source, target, type_id, false, None)
}

/// Record a transaction-local edge deletion with optional source-row identity.
pub(crate) fn record_deleted_edge_with_identity(
    source: u32,
    target: u32,
    type_id: EdgeTypeId,
    schema_reversed: bool,
    relationship_id: Option<RelationshipId>,
) -> GraphResult<()> {
    let cancels_insert = TX_DELTA.with(|delta| {
        delta.borrow().as_ref().is_some_and(|delta| {
            delta.added_edges.get(&source).is_some_and(|edges| {
                edges.iter().any(|edge| {
                    edge.target == target
                        && edge.type_id == type_id
                        && edge.schema_reversed == schema_reversed
                        && (relationship_id.is_none() || edge.relationship_id == relationship_id)
                })
            })
        })
    });
    if cancels_insert {
        ensure_write_capacity(0, 0, 0)?;
    } else {
        ensure_write_capacity(0, 1, estimated_deleted_edge_bytes())?;
    }
    TX_DELTA.with(|delta| {
        let mut borrowed = delta.borrow_mut();
        let delta = borrowed.get_or_insert_with(TxGraphDelta::default);
        if let Some(edges) = delta.added_edges.get_mut(&source) {
            edges.retain(|edge| {
                edge.target != target
                    || edge.type_id != type_id
                    || edge.schema_reversed != schema_reversed
                    || (relationship_id.is_some() && edge.relationship_id != relationship_id)
            });
            if edges.is_empty() {
                delta.added_edges.remove(&source);
            }
            if cancels_insert {
                delta.refresh_relationship_identity_completeness_summary();
                return;
            }
        }
        delta
            .deleted_edges
            .insert((source, target, type_id, schema_reversed, relationship_id));
    });
    bump_topology_revision();
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
                inserts.entry(source).or_default().push((
                    target,
                    edge.type_id,
                    edge.schema_reversed,
                    edge.relationship_id,
                ));
            }
        }

        let mut deletes = OverlayDeletes::new();
        for &(source, target, type_id, schema_reversed, relationship_id) in &delta.deleted_edges {
            let (source, target) = orient_edge(direction, source, target);
            deletes.entry(source).or_default().insert((
                target,
                type_id,
                schema_reversed,
                relationship_id,
            ));
        }

        (inserts, deletes)
    })
}

/// Return transaction-local edge overlays while preserving inserted weights.
pub(crate) fn weighted_edge_overlay(
    direction: TraversalDirection,
) -> (HashMap<u32, Vec<DeltaEdge>>, OverlayDeletes) {
    TX_DELTA.with(|delta| {
        let borrowed = delta.borrow();
        let Some(delta) = borrowed.as_ref() else {
            return (HashMap::new(), OverlayDeletes::new());
        };

        let mut inserts = HashMap::<u32, Vec<DeltaEdge>>::new();
        for (&source, edges) in &delta.added_edges {
            for edge in edges {
                let (source, target) = orient_edge(direction, source, edge.target);
                inserts.entry(source).or_default().push(DeltaEdge {
                    target,
                    type_id: edge.type_id,
                    schema_reversed: edge.schema_reversed,
                    weight: edge.weight,
                    relationship_id: edge.relationship_id,
                });
            }
        }

        let mut deletes = OverlayDeletes::new();
        for &(source, target, type_id, schema_reversed, relationship_id) in &delta.deleted_edges {
            let (source, target) = orient_edge(direction, source, target);
            deletes.entry(source).or_default().insert((
                target,
                type_id,
                schema_reversed,
                relationship_id,
            ));
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
    type DeletedEdge = (u32, u32, EdgeTypeId, bool, Option<RelationshipId>);
    let (len, capacity) = TX_DELTA.with(|delta| {
        delta.borrow().as_ref().map_or((0, 0), |delta| {
            (delta.deleted_edges.len(), delta.deleted_edges.capacity())
        })
    });
    if len.saturating_add(1) <= capacity {
        return 0;
    }
    let target_capacity = len
        .saturating_add(1)
        .next_power_of_two()
        .saturating_mul(2)
        .max(4);
    target_capacity
        .saturating_sub(capacity)
        .saturating_mul(std::mem::size_of::<DeletedEdge>() * 2)
}

fn estimated_filter_update_bytes() -> usize {
    std::mem::size_of::<(usize, u32)>()
        .saturating_add(std::mem::size_of::<Option<EncodedFilterValue>>())
}

fn estimated_relationship_identity_bytes(identity: &RelationshipIdentity) -> usize {
    std::mem::size_of::<RelationshipIdentity>().saturating_add(identity.source_key.len())
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
            let nesting = pgrx::pg_sys::GetCurrentTransactionNestLevel();
            SUBTRANSACTION_DEPTH.with(|depth| {
                depth.set(u32::try_from(nesting.saturating_sub(1)).unwrap_or_default());
            });
            pgrx::pg_sys::RegisterXactCallback(Some(xact_callback), std::ptr::null_mut());
            pgrx::pg_sys::RegisterSubXactCallback(Some(subxact_callback), std::ptr::null_mut());
        }
    }
    #[cfg(test)]
    {
        CALLBACKS_REGISTERED.store(true, Ordering::SeqCst);
    }
}

fn ensure_transaction_callbacks_registered() {
    if !CALLBACKS_REGISTERED.load(Ordering::SeqCst) {
        register_transaction_callbacks();
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
        XactEvent::XACT_EVENT_PREPARE
            | XactEvent::XACT_EVENT_COMMIT
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
            finish_subtransaction(false);
        }
        SubXactEvent::SUBXACT_EVENT_ABORT_SUB => {
            finish_subtransaction(true);
        }
        SubXactEvent::SUBXACT_EVENT_PRE_COMMIT_SUB => {}
        _ => {}
    }
}

/// Return current transaction-delta statistics.
pub(crate) fn stats() -> TxDeltaStats {
    let mut stats = TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .map(TxGraphDelta::stats)
            .unwrap_or_default()
    });
    stats.memory_bytes = stats
        .memory_bytes
        .saturating_add(SUBTRANSACTION_SNAPSHOTS.with(|snapshots| {
            snapshots
                .borrow()
                .iter()
                .flatten()
                .map(TxGraphDelta::estimated_heap_bytes)
                .sum::<usize>()
        }));
    stats
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
    bump_topology_revision();
    SUBTRANSACTION_DEPTH.with(|depth| depth.set(0));
    SUBTRANSACTION_SNAPSHOTS.with(|snapshots| snapshots.borrow_mut().clear());
}

fn snapshot_active_subtransactions() {
    let depth = SUBTRANSACTION_DEPTH.with(Cell::get) as usize;
    if depth == 0 {
        return;
    }
    let current = TX_DELTA.with(|delta| delta.borrow().clone().unwrap_or_default());
    SUBTRANSACTION_SNAPSHOTS.with(|snapshots| {
        let mut snapshots = snapshots.borrow_mut();
        while snapshots.len() < depth {
            snapshots.push(Some(current.clone()));
        }
    });
}

fn pending_snapshot_bytes() -> usize {
    let depth = SUBTRANSACTION_DEPTH.with(Cell::get) as usize;
    let captured = SUBTRANSACTION_SNAPSHOTS.with(|snapshots| snapshots.borrow().len());
    let missing = depth.saturating_sub(captured);
    TX_DELTA.with(|delta| {
        delta
            .borrow()
            .as_ref()
            .map(TxGraphDelta::estimated_heap_bytes)
            .unwrap_or_default()
            .saturating_mul(missing)
    })
}

fn finish_subtransaction(aborted: bool) {
    let depth = SUBTRANSACTION_DEPTH.with(Cell::get) as usize;
    if depth == 0 {
        return;
    }
    let snapshot = SUBTRANSACTION_SNAPSHOTS.with(|snapshots| {
        let mut snapshots = snapshots.borrow_mut();
        let snapshot = snapshots.get_mut(depth - 1).and_then(Option::take);
        snapshots.truncate(depth - 1);
        snapshot
    });
    if aborted {
        if let Some(snapshot) = snapshot {
            TX_DELTA.with(|delta| *delta.borrow_mut() = Some(snapshot));
            bump_topology_revision();
        }
    }
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
    SUBTRANSACTION_SNAPSHOTS.with(|snapshots| snapshots.borrow_mut().clear());
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

    fn base_edge_type_registry() -> EdgeTypeRegistry {
        EdgeTypeRegistry::try_from_labels(vec![String::new(), "base".to_string()])
            .expect("base registry")
    }

    #[test]
    fn tx_unseen_labels_allocate_deterministically_without_mutating_base_registry() {
        clear_for_test();
        let base = base_edge_type_registry();
        let zeta = intern_edge_type(&base, "zeta").expect("intern zeta");
        let alpha = intern_edge_type(&base, "alpha").expect("intern alpha");
        assert_eq!((zeta.get(), alpha.get()), (2, 3));
        assert_eq!(
            edge_type_id(base.len(), base.fingerprint(), "zeta"),
            Some(zeta)
        );
        assert_eq!(
            edge_type_label(base.len(), base.fingerprint(), alpha).as_deref(),
            Some("alpha")
        );
        assert_eq!(base.as_slice(), base_edge_type_registry().as_slice());
        clear_for_test();
    }

    #[test]
    fn tx_unseen_label_savepoint_abort_restores_dictionary_and_edges() {
        clear_for_test();
        let base = base_edge_type_registry();
        set_subtransaction_depth_for_test(1);
        let type_id = intern_edge_type(&base, "aborted").expect("intern in savepoint");
        record_added_edge(
            0,
            DeltaEdge {
                target: 1,
                type_id,
                schema_reversed: false,
                weight: None,
                relationship_id: Some(1),
            },
        )
        .expect("record edge in savepoint");
        finish_subtransaction(true);
        assert_eq!(
            edge_type_id(base.len(), base.fingerprint(), "aborted"),
            None
        );
        assert_eq!(stats().added_edges, 0);
        clear_for_test();
    }

    #[test]
    fn tx_unseen_label_savepoint_release_and_nested_abort_preserve_outer_slots() {
        clear_for_test();
        let base = base_edge_type_registry();
        set_subtransaction_depth_for_test(1);
        let outer = intern_edge_type(&base, "outer").expect("intern outer");
        finish_subtransaction(false);
        set_subtransaction_depth_for_test(1);
        let nested = intern_edge_type(&base, "nested").expect("intern nested");
        finish_subtransaction(true);
        assert_eq!(
            edge_type_id(base.len(), base.fingerprint(), "outer"),
            Some(outer)
        );
        assert_eq!(edge_type_id(base.len(), base.fingerprint(), "nested"), None);
        assert_eq!(
            edge_type_label(base.len(), base.fingerprint(), nested),
            None
        );
        clear_for_test();
    }

    #[test]
    fn tx_unseen_label_top_abort_discards_every_provisional_slot() {
        clear_for_test();
        let base = base_edge_type_registry();
        intern_edge_type(&base, "temporary").expect("intern temporary");
        clear_current_transaction_state();
        assert_eq!(
            edge_type_id(base.len(), base.fingerprint(), "temporary"),
            None
        );
        assert!(!stats().dirty);
    }

    #[test]
    fn tx_unseen_label_policy_and_resource_failures_are_atomic() {
        clear_for_test();
        let base = base_edge_type_registry();
        let oversized =
            "x".repeat(crate::edge_type_registry::EdgeTypeRegistry::MAX_EDGE_TYPE_LABEL_BYTES + 1);
        assert!(matches!(
            intern_edge_type(&base, &oversized),
            Err(GraphError::EdgeTypeLimit)
        ));
        set_test_limits(100, 100, 1);
        assert!(matches!(
            intern_edge_type(&base, "resource-limited"),
            Err(GraphError::OverlayLimit { .. })
        ));
        assert_eq!(
            edge_type_id(base.len(), base.fingerprint(), "resource-limited"),
            None
        );
        clear_for_test();
    }

    #[test]
    fn provisional_edge_type_growth_bound_covers_capacity_transitions() {
        clear_for_test();
        let base = base_edge_type_registry();
        for index in 0..64 {
            let before = stats().memory_bytes;
            let (ordered_len, ordered_capacity, lookup_len, lookup_capacity) =
                TX_DELTA.with(|slot| {
                    let borrowed = slot.borrow();
                    let delta = borrowed.as_ref();
                    (
                        delta.map_or(0, |value| value.appended_edge_type_labels.len()),
                        delta.map_or(0, |value| value.appended_edge_type_labels.capacity()),
                        delta.map_or(0, |value| value.appended_edge_type_ids.len()),
                        delta.map_or(0, |value| value.appended_edge_type_ids.capacity()),
                    )
                });
            let label = format!("provisional_{index}");
            let allowance = provisional_edge_type_growth_bound(
                ordered_len,
                ordered_capacity,
                lookup_len,
                lookup_capacity,
                label.len(),
            )
            .expect("growth bound");
            intern_edge_type(&base, &label).expect("intern provisional label");
            assert!(stats().memory_bytes <= before.saturating_add(allowance));
        }
        clear_for_test();
    }

    #[test]
    fn tx_unseen_label_durable_apply_conflict_remaps_or_fails_without_aliasing() {
        clear_for_test();
        let base = base_edge_type_registry();
        let provisional = intern_edge_type(&base, "pending").expect("intern pending");
        assert!(matches!(
            ensure_engine_replacement_allowed("test replacement"),
            Err(GraphError::ReadOnly { .. })
        ));
        let rebuilt = EdgeTypeRegistry::try_from_labels(vec![String::new(), "other".to_string()])
            .expect("rebuilt registry");
        assert!(matches!(
            intern_edge_type(&rebuilt, "pending"),
            Err(GraphError::UnsupportedOperation { .. })
        ));
        assert_eq!(
            edge_type_label(base.len(), base.fingerprint(), provisional).as_deref(),
            Some("pending")
        );
        assert_eq!(
            edge_type_id(rebuilt.len(), rebuilt.fingerprint(), "pending"),
            None
        );
        clear_for_test();
    }

    #[test]
    fn empty_delta_reports_clean_stats() {
        clear_current_transaction_state();

        assert_eq!(stats(), TxDeltaStats::default());
    }

    #[test]
    fn topology_revision_changes_for_same_cardinality_edge_substitutions() {
        clear_current_transaction_state();
        record_added_edge(
            0,
            DeltaEdge {
                target: 1,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("first edge insert");
        let first_revision = topology_revision();
        record_deleted_edge_with_identity(0, 1, crate::types::EdgeTypeId::test_v6(1), false, None)
            .expect("cancel first edge insert");
        record_added_edge(
            0,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("replacement edge insert");

        assert_eq!(stats().added_edges, 1);
        assert!(topology_revision() > first_revision);
        clear_current_transaction_state();
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
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    weight: Some(3),
                    schema_reversed: false,
                    relationship_id: None,
                },
            );
            delta
                .deleted_edges
                .insert((1, 2, EdgeTypeId::test_v6(1), false, None));
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
    fn relationship_identity_summary_tracks_transaction_insert_and_cancellation() {
        clear_current_transaction_state();
        let active = roaring::RoaringBitmap::from_iter([3]);
        record_added_edge(
            0,
            DeltaEdge {
                target: 1,
                type_id: EdgeTypeId::from_v6_storage(3).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record missing transaction identity");
        assert!(has_missing_relationship_identity_for_types(&active));
        record_deleted_edge_with_identity(0, 1, EdgeTypeId::test_v6(3), false, None)
            .expect("cancel transaction edge");
        assert!(!has_missing_relationship_identity_for_types(&active));
        clear_current_transaction_state();
    }

    #[test]
    fn deleted_edge_preflight_covers_the_actual_widened_hash_entry() {
        clear_current_transaction_state();
        for index in 0..64_u32 {
            let before = stats().memory_bytes;
            let allowance = estimated_deleted_edge_bytes();
            record_deleted_edge_with_identity(
                index,
                index + 1,
                EdgeTypeId::try_from(65_534_u32).expect("logical type is valid"),
                true,
                Some(index + 1),
            )
            .expect("record widened logical edge deletion");
            let growth = stats().memory_bytes.saturating_sub(before);
            assert!(
                growth <= allowance,
                "insertion {index} grew deleted-edge storage by {growth} beyond {allowance}"
            );
        }
        clear_current_transaction_state();
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
    fn stats_include_transaction_relationship_identities() {
        clear_current_transaction_state();

        let id = record_relationship_identity(
            10,
            RelationshipIdentity {
                mapping_id: 7,
                source_key: "relationship-source-key".to_string(),
            },
        )
        .expect("record relationship identity");

        assert_eq!(id, 10);
        assert_eq!(
            relationship_identities(),
            vec![RelationshipIdentity {
                mapping_id: 7,
                source_key: "relationship-source-key".to_string(),
            }]
        );
        let stats = stats();
        assert!(stats.dirty);
        assert!(stats.memory_bytes >= std::mem::size_of::<RelationshipIdentity>());
        clear_current_transaction_state();
    }

    #[test]
    fn relationship_identity_memory_limit_rejects_before_recording() {
        clear_current_transaction_state();
        set_test_limits(100_000, 100_000, 8);

        let err = record_relationship_identity(
            0,
            RelationshipIdentity {
                mapping_id: 1,
                source_key: "long-relationship-source-key".to_string(),
            },
        )
        .expect_err("memory cap should reject relationship identity");

        assert!(matches!(
            err,
            GraphError::OverlayLimit { kind, .. } if kind == "overlay_memory_bytes"
        ));
        assert!(relationship_identities().is_empty());
        assert_eq!(stats(), TxDeltaStats::default());
        clear_for_test();
    }

    #[test]
    fn added_node_keys_respect_recorded_tenant_scope() {
        clear_current_transaction_state();
        record_added_node(100, "a1", Some("tenant-a")).expect("record tenant-a");
        record_added_node(100, "b1", Some("tenant-b")).expect("record tenant-b");
        record_added_node(100, "global", None).expect("record unscoped");
        record_added_node(200, "other", Some("tenant-a")).expect("record other table");

        assert_eq!(added_node_keys(100, Some("tenant-a"), true), vec!["a1"]);
        assert_eq!(added_node_keys(100, Some("tenant-b"), true), vec!["b1"]);
        assert_eq!(
            added_node_keys(100, Some("tenant-a"), false),
            vec!["a1", "b1", "global"]
        );
        assert_eq!(added_node_keys(100, None, true), vec!["a1", "b1", "global"]);

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
    fn edge_overlay_cancels_local_insert_delete_pairs() {
        clear_current_transaction_state();

        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record insert");
        record_deleted_edge(1, 2, crate::types::EdgeTypeId::test_v6(1)).expect("record delete");
        let (inserts, deletes) = edge_overlay(TraversalDirection::Out);
        assert!(inserts.is_empty());
        assert!(deletes.is_empty());

        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record insert after delete");
        let (inserts, deletes) = edge_overlay(TraversalDirection::In);
        assert!(deletes.is_empty());
        assert!(inserts.get(&2).is_some_and(|edges| edges.contains(&(
            1,
            crate::types::EdgeTypeId::test_v6(1),
            false,
            None
        ))));

        record_deleted_edge(1, 2, crate::types::EdgeTypeId::test_v6(1))
            .expect("record delete after insert");
        let (inserts, deletes) = edge_overlay(TraversalDirection::Out);
        assert!(inserts.is_empty());
        assert!(deletes.is_empty());
    }

    #[test]
    fn identified_delete_preserves_parallel_transaction_sibling() {
        clear_current_transaction_state();
        for relationship_id in [11, 12] {
            record_added_edge(
                1,
                DeltaEdge {
                    target: 2,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    weight: None,
                    schema_reversed: false,
                    relationship_id: Some(relationship_id),
                },
            )
            .expect("record parallel insert");
        }

        record_deleted_edge_with_identity(
            1,
            2,
            crate::types::EdgeTypeId::test_v6(1),
            false,
            Some(11),
        )
        .expect("record identified delete");
        let (inserts, deletes) = edge_overlay(TraversalDirection::Out);

        assert_eq!(
            inserts.get(&1),
            Some(&vec![(
                2,
                crate::types::EdgeTypeId::test_v6(1),
                false,
                Some(12)
            )])
        );
        assert!(deletes.is_empty());
        clear_current_transaction_state();
    }

    #[test]
    fn wildcard_delete_coexists_with_later_identified_insert() {
        clear_current_transaction_state();
        record_deleted_edge(1, 2, crate::types::EdgeTypeId::test_v6(1))
            .expect("record wildcard delete");
        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: Some(12),
            },
        )
        .expect("record identified insert");

        let (inserts, deletes) = edge_overlay(TraversalDirection::Out);
        assert_eq!(
            inserts.get(&1),
            Some(&vec![(
                2,
                crate::types::EdgeTypeId::test_v6(1),
                false,
                Some(12)
            )])
        );
        assert_eq!(
            deletes.get(&1),
            Some(&HashSet::from([(
                2,
                crate::types::EdgeTypeId::test_v6(1),
                false,
                None
            )]))
        );
        clear_current_transaction_state();
    }

    #[test]
    fn edge_delta_capacity_allows_net_neutral_pairs_at_limit() {
        clear_current_transaction_state();
        set_test_limits(100_000, 1, 256 * 1_048_576);

        record_deleted_edge(1, 2, crate::types::EdgeTypeId::test_v6(1))
            .expect("record delete at limit");
        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("delete plus add should be net neutral at limit");
        assert_eq!(stats().deleted_edges, 0);
        assert_eq!(stats().added_edges, 0);

        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record insert at limit");
        record_deleted_edge(1, 2, crate::types::EdgeTypeId::test_v6(1))
            .expect("insert plus delete should be net neutral at limit");
        assert_eq!(stats().deleted_edges, 0);
        assert_eq!(stats().added_edges, 0);

        clear_for_test();
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

        finish_subtransaction(false);

        assert!(subtransaction_active());
        finish_subtransaction(false);
        assert!(!subtransaction_active());
    }

    #[test]
    fn subtransaction_abort_restores_outer_delta_and_depth() {
        clear_current_transaction_state();
        with_delta_for_test(|delta| delta.add_node_for_test(100, "new-node", 42));
        set_subtransaction_depth_for_test(1);
        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("subtransaction write records");

        finish_subtransaction(true);

        assert_eq!(stats().added_nodes, 1);
        assert_eq!(stats().added_edges, 0);
        assert!(!subtransaction_active());
    }

    #[test]
    fn subtransaction_commit_keeps_delta() {
        clear_current_transaction_state();
        set_subtransaction_depth_for_test(1);

        record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("subtransaction write records");
        finish_subtransaction(false);

        assert_eq!(stats().added_edges, 1);
        assert!(!subtransaction_active());
    }

    #[test]
    fn capacity_rejection_does_not_create_subtransaction_snapshot() {
        clear_current_transaction_state();
        set_subtransaction_depth_for_test(1);

        let err = ensure_write_capacity(100_001, 0, 0)
            .expect_err("node capacity should reject before subtransaction");

        assert!(matches!(
            err,
            GraphError::OverlayLimit { kind, .. } if kind == "tx_delta_nodes"
        ));
        assert!(SUBTRANSACTION_SNAPSHOTS.with(|snapshots| snapshots.borrow().is_empty()));
        set_subtransaction_depth_for_test(0);
    }

    #[test]
    fn nested_snapshot_memory_is_rejected_before_cloning() {
        clear_current_transaction_state();
        with_delta_for_test(|delta| delta.add_node_for_test(100, "outer-node", 42));
        let current_bytes = stats().memory_bytes;
        set_test_limits(
            100_000,
            100_000,
            current_bytes.saturating_add(estimated_added_edge_bytes()),
        );
        set_subtransaction_depth_for_test(2);

        let err = record_added_edge(
            1,
            DeltaEdge {
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect_err("snapshot copies must be charged before cloning");

        assert!(matches!(
            err,
            GraphError::OverlayLimit { kind, .. } if kind == "overlay_memory_bytes"
        ));
        assert!(SUBTRANSACTION_SNAPSHOTS.with(|snapshots| snapshots.borrow().is_empty()));
        assert_eq!(stats().added_nodes, 1);
        assert_eq!(stats().added_edges, 0);
        clear_for_test();
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
