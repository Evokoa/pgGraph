//! # Engine — Graph engine orchestrator
//!
//! Owns all data stores and provides the high-level API consumed by
//! the `#[pg_extern]` SQL functions.
//!
//! See: `docs/contributor_guide/engine-internals.mdx`

use crate::bfs;
use crate::edge_store::EdgeStore;
use crate::filter_index::FilterIndex;
use crate::node_store::NodeStore;
use crate::path_finder;
use crate::resolution_index::{ResolutionDeltaIndex, ResolutionIndex, ResolutionIndexBuilder};
use crate::safety::{GraphError, GraphResult};
use crate::types::*;

use pgrx::prelude::TimestampWithTimeZone;
use roaring::RoaringBitmap;
use std::collections::{HashMap, HashSet};

type OverlayInserts = HashMap<u32, Vec<(u32, u8)>>;
type OverlayDeletes = HashMap<u32, HashSet<(u32, u8)>>;
type TraversalEdgeOverlay = (OverlayInserts, OverlayDeletes);

/// Resolution storage backend.
///
/// - `Builder`: compact append-only entries used during node ingestion.
/// - `Finalized`: Sorted array, used after build, compact memory, binary search.
/// - `MmapBacked`: Resolution section borrowed from the `.pggraph` mmap and
///   shared across backends via the OS page cache.
pub enum ResolutionStore {
    /// Build-time: compact entries. Converted to Finalized before edge linking.
    Builder(ResolutionIndexBuilder),
    /// Post-build: sorted array in owned memory with binary-search lookups.
    Finalized(Vec<u8>),
    /// Loaded from `.pggraph` file via mmap. The mmap handle is held by
    /// [`Engine::_mmap`]; resolution lookups borrow that section's bytes.
    MmapBacked(MmapResolutionState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmapResolutionState {
    offset: usize,
    len: usize,
}

impl MmapResolutionState {
    pub fn new(offset: usize, len: usize) -> Self {
        Self { offset, len }
    }

    fn range(&self) -> std::ops::Range<usize> {
        self.offset..self.offset + self.len
    }
}

/// The core graph engine. Holds all data stores.
pub struct Engine {
    /// Node metadata. After `.pggraph` load, base arrays are mmap-backed until a
    /// sync mutation materializes them into owned arrays.
    pub node_store: NodeStore,
    /// Forward CSR adjacency. After `.pggraph` load, this store is mmap-backed.
    pub edge_store: EdgeStore,
    /// Reverse CSR adjacency. This is derived into owned heap per backend.
    pub reverse_edge_store: EdgeStore,
    /// Traversal filter index. After `.pggraph` load, this is deserialized from a
    /// bincode section into backend-local heap.
    pub filter_index: FilterIndex,
    /// Edge label registry. After `.pggraph` load, this is deserialized from a
    /// bincode section into backend-local heap.
    pub edge_type_registry: Vec<String>,
    pub has_unidirectional_edges: bool,
    pub built: bool,
    pub sync_status: SyncStatus,
    pub last_build: Option<TimestampWithTimeZone>,
    pub last_vacuum: Option<TimestampWithTimeZone>,

    /// Resolution store — switches from compact build entries to sorted array.
    pub resolution_store: ResolutionStore,

    /// Post-build indexed resolution delta for nodes inserted by sync replay.
    ///
    /// Finalized and mmap-backed resolution indexes are immutable. Inserts that
    /// arrive after build() live here until the next rebuild/vacuum merge.
    pub resolution_delta: ResolutionDeltaIndex,

    /// mmap handle. Keeps the mapping alive for mmap-backed NodeStore arrays,
    /// forward EdgeStore arrays, and ResolutionIndex bytes.
    pub _mmap: Option<memmap2::Mmap>,
    /// Edge mutation buffer for trigger sync.
    /// Pending edge mutations that haven't been merged into CSR yet.
    pub edge_buffer: Vec<EdgeMutation>,

    /// When true, the engine is in read-only mode.
    /// Sync inserts/updates/deletes are rejected until a rebuild installs a
    /// read-write engine.
    pub is_read_only: bool,
    /// Operator-facing reason for read-only mode.
    pub read_only_reason: Option<ReadOnlyReason>,

    /// Last durable sync-log row applied by this backend-local engine.
    pub applied_sync_id: i64,
    /// Whether pending edge/node deltas require CSR rebuild.
    pub needs_vacuum: bool,
    /// Whether catalog/schema drift requires graph rebuild.
    pub needs_rebuild: bool,
    /// Schema state exposed by graph.status().
    pub schema_state: SchemaState,
    /// Human-readable schema/sync invalidation reason.
    pub invalid_reason: Option<String>,
    /// Disabled graph trigger count from the last schema/status check.
    pub disabled_trigger_count: i32,
    /// Fingerprint of registered graph catalog state captured at build time.
    pub catalog_fingerprint: Option<u64>,
    /// Number of pending durable sync rows from the last status/catch-up check.
    pub pending_sync_rows: i64,
    /// Active node membership by source table for table-scoped sync operations.
    pub table_membership: HashMap<u32, RoaringBitmap>,
    /// Tenant membership by tenant value for tenanted table rows.
    pub tenant_membership: HashMap<String, RoaringBitmap>,
    /// Table OIDs that require tenant scoping.
    pub tenanted_table_oids: HashSet<u32>,
}

/// A pending edge mutation from trigger sync.
#[derive(Debug, Clone)]
pub struct EdgeMutation {
    pub source: u32,
    pub target: u32,
    pub type_id: u8,
    pub kind: MutationKind,
}

/// Engine sync status. Replaces raw strings for type safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Idle,
    Syncing,
    ReadOnly,
}

/// Reason an engine has entered read-only mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    MemoryLimit,
    EdgeBufferFull,
}

impl std::fmt::Display for ReadOnlyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadOnlyReason::MemoryLimit => write!(f, "memory_limit"),
            ReadOnlyReason::EdgeBufferFull => write!(f, "edge_buffer_full"),
        }
    }
}

/// Schema validity state for registered graph metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaState {
    Current,
    Stale,
    Invalid,
}

impl std::fmt::Display for SchemaState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaState::Current => write!(f, "current"),
            SchemaState::Stale => write!(f, "stale"),
            SchemaState::Invalid => write!(f, "invalid"),
        }
    }
}

impl std::fmt::Display for SyncStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncStatus::Idle => write!(f, "idle"),
            SyncStatus::Syncing => write!(f, "syncing"),
            SyncStatus::ReadOnly => write!(f, "read_only"),
        }
    }
}

/// Edge mutation type. Replaces bool `is_delete` for self-documenting code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    Insert,
    Delete,
}

impl Engine {
    pub fn new() -> Self {
        let edge_type_registry = vec!["".to_string()];
        // Index 0 = untyped (reserved)

        Self {
            node_store: NodeStore::new(),
            edge_store: EdgeStore::new(),
            reverse_edge_store: EdgeStore::new(),
            filter_index: FilterIndex::new(),
            edge_type_registry,
            has_unidirectional_edges: false,
            built: false,
            sync_status: SyncStatus::Idle,
            last_build: None,
            last_vacuum: None,
            resolution_store: ResolutionStore::Builder(ResolutionIndexBuilder::new()),
            resolution_delta: ResolutionDeltaIndex::new(),
            _mmap: None,
            edge_buffer: Vec::new(),
            is_read_only: false,
            read_only_reason: None,
            applied_sync_id: 0,
            needs_vacuum: false,
            needs_rebuild: false,
            schema_state: SchemaState::Current,
            invalid_reason: None,
            disabled_trigger_count: 0,
            catalog_fingerprint: None,
            pending_sync_rows: 0,
            table_membership: HashMap::new(),
            tenant_membership: HashMap::new(),
            tenanted_table_oids: HashSet::new(),
        }
    }

    /// Refresh status-only observations without replacing graph data stores.
    pub fn refresh_observed_state(
        &mut self,
        disabled_trigger_count: i32,
        pending_sync_rows: i64,
        catalog_state: &GraphResult<(u64, Option<String>)>,
    ) {
        self.disabled_trigger_count = disabled_trigger_count;
        self.pending_sync_rows = pending_sync_rows;

        if disabled_trigger_count > 0 && matches!(self.schema_state, SchemaState::Current) {
            self.mark_schema_stale(format!(
                "{} graph sync trigger(s) are disabled",
                disabled_trigger_count
            ));
        }

        if !self.built {
            return;
        }

        match catalog_state {
            Ok((_current_fingerprint, Some(reason))) => {
                self.mark_schema_invalid(reason.clone());
            }
            Ok((current_fingerprint, None))
                if self.catalog_fingerprint.is_some()
                    && self.catalog_fingerprint != Some(*current_fingerprint) =>
            {
                self.mark_schema_invalid(
                    "registered graph catalog changed since graph.build(); rebuild required",
                );
            }
            Err(err) => {
                self.mark_schema_invalid(format!(
                    "registered graph schema validation failed: {}",
                    err
                ));
            }
            _ => {}
        }
    }

    fn mark_schema_stale(&mut self, reason: impl Into<String>) {
        self.schema_state = SchemaState::Stale;
        self.invalid_reason = Some(reason.into());
    }

    fn mark_schema_invalid(&mut self, reason: impl Into<String>) {
        self.needs_rebuild = true;
        self.schema_state = SchemaState::Invalid;
        self.invalid_reason = Some(reason.into());
    }

    pub fn replace_edge_stores(&mut self, edge_store: EdgeStore) {
        let reverse_edge_store = edge_store.reversed();
        self.edge_store = edge_store;
        self.reverse_edge_store = reverse_edge_store;
    }

    pub fn mark_has_unidirectional_edges(&mut self) {
        self.has_unidirectional_edges = true;
    }

    pub fn finish_build(&mut self, built_at: Option<TimestampWithTimeZone>) {
        self.built = true;
        self.last_build = built_at;
    }

    pub fn set_catalog_fingerprint(&mut self, catalog_fingerprint: u64) {
        self.catalog_fingerprint = Some(catalog_fingerprint);
    }

    pub fn inherit_runtime_metadata_from(&mut self, source: &Self) {
        self.catalog_fingerprint = source.catalog_fingerprint;
        self.is_read_only = source.is_read_only;
        self.read_only_reason = source.read_only_reason;
        self.sync_status = source.sync_status;
        self.last_build = source.last_build;
        self.last_vacuum = source.last_vacuum;
        self.applied_sync_id = source.applied_sync_id;
        self.needs_vacuum = source.needs_vacuum;
    }

    pub fn record_applied_sync_id(&mut self, sync_id: i64) {
        self.applied_sync_id = sync_id;
    }

    pub fn record_pending_sync_rows(&mut self, pending_sync_rows: i64) {
        self.pending_sync_rows = pending_sync_rows;
    }

    pub fn mark_syncing(&mut self) {
        if !self.is_read_only {
            self.sync_status = SyncStatus::Syncing;
        }
    }

    pub fn mark_idle_if_writable(&mut self) {
        if !self.is_read_only {
            self.sync_status = SyncStatus::Idle;
        }
    }

    pub fn mark_vacuum_required(&mut self) {
        self.needs_vacuum = true;
    }

    pub fn mark_vacuum_complete(&mut self, vacuumed_at: Option<TimestampWithTimeZone>) {
        self.needs_vacuum = false;
        self.last_vacuum = vacuumed_at;
    }

    pub fn install_mmap_backed_graph(
        &mut self,
        node_store: NodeStore,
        edge_store: EdgeStore,
        filter_index: FilterIndex,
        edge_type_registry: Vec<String>,
        mmap: memmap2::Mmap,
        resolution_state: MmapResolutionState,
    ) {
        let reverse_edge_store = edge_store.reversed();
        self.node_store = node_store;
        self.edge_store = edge_store;
        self.reverse_edge_store = reverse_edge_store;
        self.filter_index = filter_index;
        self.edge_type_registry = edge_type_registry;
        self.rebuild_table_membership();
        self.finish_build(None);
        self.resolution_store = ResolutionStore::MmapBacked(resolution_state);
        self._mmap = Some(mmap);
    }

    /// Register a new edge type label. Returns the u8 type ID.
    pub fn register_edge_type(&mut self, label: &str) -> GraphResult<u8> {
        // Check if already registered
        if let Some(pos) = self.edge_type_registry.iter().position(|l| l == label) {
            return Ok(pos as u8);
        }
        if self.edge_type_registry.len() >= 255 {
            return Err(GraphError::EdgeTypeLimit);
        }
        let id = self.edge_type_registry.len() as u8;
        self.edge_type_registry.push(label.to_string());
        Ok(id)
    }

    /// Resolve a (table_oid, pk) → node_idx.
    ///
    /// Dispatches to the active resolution backend:
    /// - Builder: compact delta lookup
    /// - Finalized: binary search on sorted array (post-build)
    /// - MmapBacked: binary search on mmap'd bytes (file-loaded)
    pub fn resolve(&self, table_oid: u32, pk: &str) -> Option<u32> {
        let verify = |idx: u32| {
            idx < self.node_store.node_count()
                && self.node_store.is_active(idx)
                && self.node_store.table_oid(idx) == table_oid
                && self.node_store.primary_key(idx) == pk
        };
        if let Some(idx) = self
            .resolution_delta
            .resolve_verified(table_oid, pk, verify)
        {
            return Some(idx);
        }
        match &self.resolution_store {
            ResolutionStore::Builder(builder) => builder.resolve_verified(table_oid, pk, verify),
            ResolutionStore::Finalized(bytes) => {
                ResolutionIndex::from_bytes(bytes)?.resolve_verified(table_oid, pk, verify)
            }
            ResolutionStore::MmapBacked(resolution_state) => {
                let mmap = self._mmap.as_ref()?;
                let data = &mmap[resolution_state.range()];
                ResolutionIndex::from_bytes(data)?.resolve_verified(table_oid, pk, verify)
            }
        }
    }

    /// Insert into resolution index. Only valid during build (Builder mode).
    pub fn resolution_insert(&mut self, table_oid: u32, pk: &str, node_idx: u32) {
        match &mut self.resolution_store {
            ResolutionStore::Builder(builder) => {
                builder.insert(table_oid, pk, node_idx);
            }
            _ => {
                self.resolution_delta.insert(table_oid, pk, node_idx);
            }
        }
    }

    pub fn insert_tenant_membership(&mut self, tenant: &str, node_idx: u32) {
        self.tenant_membership
            .entry(tenant.to_string())
            .or_default()
            .insert(node_idx);
    }

    pub fn insert_table_membership(&mut self, table_oid: u32, node_idx: u32) {
        self.table_membership
            .entry(table_oid)
            .or_default()
            .insert(node_idx);
    }

    pub fn remove_table_membership(&mut self, table_oid: u32, node_idx: u32) {
        if let Some(bitmap) = self.table_membership.get_mut(&table_oid) {
            bitmap.remove(node_idx);
        }
    }

    pub fn rebuild_table_membership(&mut self) {
        self.table_membership.clear();
        for node_idx in 0..self.node_store.node_count() {
            if self.node_store.is_active(node_idx) {
                self.insert_table_membership(self.node_store.table_oid(node_idx), node_idx);
            }
        }
    }

    #[cfg(feature = "development")]
    pub fn remove_tenant_membership(&mut self, tenant: &str, node_idx: u32) {
        if let Some(bitmap) = self.tenant_membership.get_mut(tenant) {
            bitmap.remove(node_idx);
        }
    }

    pub fn materialize_mmap_node_store_for_sync(&mut self) {
        if self.node_store.is_mmap_backed() {
            self.node_store = self.node_store.to_owned_store();
        }
    }

    #[cfg(test)]
    pub(crate) fn install_mmap_node_store_for_test(&mut self, node_store: NodeStore) {
        debug_assert!(node_store.is_mmap_backed());
        self.node_store = node_store;
    }

    /// Finalize the resolution index: convert compact entries → sorted array.
    /// Called after node ingestion. Drops the build accumulator before edge linking.
    pub fn finalize_resolution(&mut self) {
        if let ResolutionStore::Builder(builder) = &self.resolution_store {
            let bytes = builder.to_bytes();
            #[cfg(not(test))]
            pgrx::log!(
                "graph: resolution index finalized — {} entries, {} bytes",
                builder.len(),
                bytes.len()
            );
            self.resolution_store = ResolutionStore::Finalized(bytes);
        }
    }

    /// Serialize resolution index to bytes (for persistence).
    pub fn resolution_to_bytes(&self) -> Vec<u8> {
        match &self.resolution_store {
            ResolutionStore::Builder(builder) => builder.to_bytes(),
            ResolutionStore::Finalized(bytes) => bytes.clone(),
            ResolutionStore::MmapBacked(resolution_state) => {
                if let Some(mmap) = &self._mmap {
                    mmap[resolution_state.range()].to_vec()
                } else {
                    ResolutionIndexBuilder::new().to_bytes()
                }
            }
        }
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn traverse(
        &self,
        seed_table_oid: u32,
        seed_id: &str,
        max_depth: i32,
        max_nodes: u32,
        max_frontier: u32,
        edge_types: Option<Vec<String>>,
        filter_condition: Option<&str>,
        tenant: Option<&str>,
        strategy: TraversalStrategy,
        direction: TraversalDirection,
    ) -> GraphResult<Vec<TraversalResult>> {
        let filter_ops = match filter_condition {
            Some(cond) => self
                .filter_index
                .parse_condition(cond)
                .map_err(|reason| GraphError::InvalidFilter { reason })?,
            None => Vec::new(),
        };

        self.traverse_with_filter_ops(
            seed_table_oid,
            seed_id,
            max_depth,
            max_nodes,
            max_frontier,
            edge_types,
            filter_ops,
            tenant,
            strategy,
            direction,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn traverse_with_filter_ops(
        &self,
        seed_table_oid: u32,
        seed_id: &str,
        max_depth: i32,
        max_nodes: u32,
        max_frontier: u32,
        edge_types: Option<Vec<String>>,
        filter_ops: Vec<FilterOp>,
        tenant: Option<&str>,
        strategy: TraversalStrategy,
        direction: TraversalDirection,
    ) -> GraphResult<Vec<TraversalResult>> {
        if !self.built {
            return Err(GraphError::NotBuilt);
        }

        // Resolve seed node
        let seed_node =
            self.resolve(seed_table_oid, seed_id)
                .ok_or_else(|| GraphError::NodeNotFound {
                    table: format!("{}", seed_table_oid),
                    pk: seed_id.to_string(),
                })?;

        // Resolve edge type filter
        let edge_type_filter = match edge_types {
            Some(types) => {
                let mut set = HashSet::new();
                for t in &types {
                    let Some(pos) = self.edge_type_registry.iter().position(|l| l == t) else {
                        return Err(GraphError::InvalidFilter {
                            reason: format!("unknown edge type '{}'", t),
                        });
                    };
                    set.insert(pos as u8);
                }
                if set.is_empty() {
                    EdgeTypeFilter::NoneMatched
                } else {
                    EdgeTypeFilter::Only(set)
                }
            }
            None => EdgeTypeFilter::All,
        };

        let (overlay_insert_edges, overlay_deleted_edges) = self.traversal_edge_overlay(direction);
        let config = bfs::BfsConfig {
            seed_node,
            max_depth,
            max_nodes,
            max_frontier,
            edge_type_filter,
            filter_ops,
            tenant: tenant.map(ToString::to_string),
            tenanted_table_oids: self.tenanted_table_oids.clone(),
            tenant_membership: self.tenant_membership.clone(),
            overlay_insert_edges,
            overlay_deleted_edges,
        };

        let edge_store = match direction {
            TraversalDirection::Any | TraversalDirection::Out => &self.edge_store,
            TraversalDirection::In => &self.reverse_edge_store,
        };

        let bfs_result = match strategy {
            TraversalStrategy::Bfs => {
                bfs::execute(&self.node_store, edge_store, &self.filter_index, &config)
            }
            TraversalStrategy::Dfs => {
                bfs::execute_dfs(&self.node_store, edge_store, &self.filter_index, &config)
            }
        };
        Ok(bfs::to_traversal_results(
            &bfs_result,
            &self.node_store,
            &self.edge_type_registry,
        ))
    }

    pub fn push_edge_mutation(&mut self, mutation: EdgeMutation) -> GraphResult<()> {
        self.reserve_edge_mutation_capacity(1)?;
        self.edge_buffer.push(mutation);
        self.needs_vacuum = true;
        Ok(())
    }

    pub fn reserve_edge_mutation_capacity(&mut self, additional: usize) -> GraphResult<()> {
        let limit = crate::config::EDGE_BUFFER_SIZE.get() as usize;
        if self.edge_buffer.len().saturating_add(additional) > limit {
            self.mark_read_only(ReadOnlyReason::EdgeBufferFull);
            return Err(GraphError::EdgeBufferFull {
                size: self.edge_buffer.len(),
            });
        }
        Ok(())
    }

    pub fn mark_read_only(&mut self, reason: ReadOnlyReason) {
        self.is_read_only = true;
        self.read_only_reason = Some(reason);
        self.sync_status = SyncStatus::ReadOnly;
    }

    pub fn read_only_error(&self) -> GraphError {
        GraphError::ReadOnly {
            reason: self
                .read_only_reason
                .map(|reason| reason.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        }
    }

    fn traversal_edge_overlay(&self, direction: TraversalDirection) -> TraversalEdgeOverlay {
        let mut inserts: HashSet<(u32, u32, u8)> = HashSet::new();
        let mut deletes: HashSet<(u32, u32, u8)> = HashSet::new();

        for mutation in &self.edge_buffer {
            let key = match direction {
                TraversalDirection::Any | TraversalDirection::Out => {
                    (mutation.source, mutation.target, mutation.type_id)
                }
                TraversalDirection::In => (mutation.target, mutation.source, mutation.type_id),
            };
            match mutation.kind {
                MutationKind::Insert => {
                    deletes.remove(&key);
                    inserts.insert(key);
                }
                MutationKind::Delete => {
                    inserts.remove(&key);
                    deletes.insert(key);
                }
            }
        }

        let mut insert_map: OverlayInserts = HashMap::new();
        for (source, target, type_id) in inserts {
            insert_map
                .entry(source)
                .or_default()
                .push((target, type_id));
        }
        let mut delete_map: OverlayDeletes = HashMap::new();
        for (source, target, type_id) in deletes {
            delete_map
                .entry(source)
                .or_default()
                .insert((target, type_id));
        }
        (insert_map, delete_map)
    }

    /// Find shortest path between two nodes.
    pub fn shortest_path(
        &self,
        source_table_oid: u32,
        source_id: &str,
        target_table_oid: u32,
        target_id: &str,
        max_depth: i32,
    ) -> GraphResult<Vec<PathStep>> {
        if !self.built {
            return Err(GraphError::NotBuilt);
        }

        let source =
            self.resolve(source_table_oid, source_id)
                .ok_or_else(|| GraphError::NodeNotFound {
                    table: format!("{}", source_table_oid),
                    pk: source_id.to_string(),
                })?;

        let target =
            self.resolve(target_table_oid, target_id)
                .ok_or_else(|| GraphError::NodeNotFound {
                    table: format!("{}", target_table_oid),
                    pk: target_id.to_string(),
                })?;

        let result = path_finder::shortest_path(
            &self.node_store,
            &self.edge_store,
            source,
            target,
            max_depth,
            self.has_unidirectional_edges,
            &self.edge_type_registry,
        );

        Ok(result.unwrap_or_default())
    }

    /// Find weighted shortest path between two nodes.
    pub fn weighted_shortest_path(
        &self,
        source_table_oid: u32,
        source_id: &str,
        target_table_oid: u32,
        target_id: &str,
    ) -> GraphResult<Vec<WeightedPathStep>> {
        if !self.built {
            return Err(GraphError::NotBuilt);
        }

        let source =
            self.resolve(source_table_oid, source_id)
                .ok_or_else(|| GraphError::NodeNotFound {
                    table: format!("{}", source_table_oid),
                    pk: source_id.to_string(),
                })?;

        let target =
            self.resolve(target_table_oid, target_id)
                .ok_or_else(|| GraphError::NodeNotFound {
                    table: format!("{}", target_table_oid),
                    pk: target_id.to_string(),
                })?;

        Ok(path_finder::weighted_shortest_path(
            &self.node_store,
            &self.edge_store,
            source,
            target,
            &self.edge_type_registry,
        ))
        .map(|path| path.unwrap_or_default())
    }

    /// Get engine status.
    pub fn status(&self) -> EngineStatus {
        EngineStatus {
            node_count: self.node_store.node_count() as i32,
            edge_count: self.edge_store.edge_count() as i32,
            memory_used_mb: self.estimated_memory_used_mb(),
            memory_limit_mb: crate::config::MEMORY_LIMIT_MB.get(),
            sync_mode: crate::config::sync_mode(),
            sync_status: self.sync_status.to_string(),
            last_build: self.last_build,
            last_vacuum: self.last_vacuum,
            edge_types: self.edge_type_registry[1..].to_vec(), // skip index 0
            edge_buffer_used: self.edge_buffer.len() as i32,
            has_unidirectional_edges: self.has_unidirectional_edges,
            applied_sync_id: self.applied_sync_id,
            pending_sync_rows: self.pending_sync_rows,
            sync_lag: self.pending_sync_rows,
            needs_vacuum: self.needs_vacuum,
            needs_rebuild: self.needs_rebuild,
            schema_state: self.schema_state.to_string(),
            invalid_reason: self.invalid_reason.clone(),
            disabled_trigger_count: self.disabled_trigger_count,
            read_only: self.is_read_only,
            read_only_reason: self.read_only_reason.map(|reason| reason.to_string()),
        }
    }

    pub fn estimated_memory_used_mb(&self) -> f64 {
        self.estimated_heap_bytes()
            .max(self.estimated_logical_bytes()) as f64
            / 1_048_576.0
    }

    pub fn estimated_heap_bytes(&self) -> usize {
        let resolution_bytes = match &self.resolution_store {
            ResolutionStore::Builder(builder) => builder.estimated_heap_bytes(),
            ResolutionStore::Finalized(bytes) => bytes.capacity(),
            ResolutionStore::MmapBacked(_) => 0,
        } + self.resolution_delta.estimated_heap_bytes();
        let registry_bytes = self.edge_type_registry.capacity() * std::mem::size_of::<String>()
            + self
                .edge_type_registry
                .iter()
                .map(String::capacity)
                .sum::<usize>();
        let edge_buffer_bytes = self.edge_buffer.capacity() * std::mem::size_of::<EdgeMutation>();
        let table_membership_bytes = self.table_membership.capacity()
            * (std::mem::size_of::<u32>() + std::mem::size_of::<RoaringBitmap>());
        let tenant_bytes = self.tenant_membership.capacity()
            * (std::mem::size_of::<String>() + std::mem::size_of::<RoaringBitmap>())
            + self
                .tenant_membership
                .keys()
                .map(String::capacity)
                .sum::<usize>();
        let tenanted_oid_bytes = self.tenanted_table_oids.capacity() * std::mem::size_of::<u32>();

        self.node_store.estimated_heap_bytes()
            + self.edge_store.estimated_heap_bytes()
            + self.reverse_edge_store.estimated_heap_bytes()
            + self.filter_index.estimated_heap_bytes()
            + resolution_bytes
            + registry_bytes
            + edge_buffer_bytes
            + table_membership_bytes
            + tenant_bytes
            + tenanted_oid_bytes
    }

    fn estimated_logical_bytes(&self) -> usize {
        let nodes = self.node_store.node_count() as usize;
        let edges = self.edge_store.edge_count() as usize;
        let reverse_edges = self.reverse_edge_store.edge_count() as usize;
        let weight_width = if self.edge_store.has_weights() { 4 } else { 0 };

        let node_bytes = nodes * (4 + 32) + nodes.div_ceil(8);
        let forward_edge_bytes = (nodes + 1) * 4 + edges * (4 + 1 + weight_width);
        let reverse_edge_bytes = (nodes + 1) * 4 + reverse_edges * (4 + 1 + weight_width);
        let resolution_bytes = nodes * crate::resolution_index::ENTRY_SIZE;
        let filter_bytes = self.filter_index.estimated_heap_bytes();
        let edge_buffer_bytes = self.edge_buffer.len() * std::mem::size_of::<EdgeMutation>();

        node_bytes
            + forward_edge_bytes
            + reverse_edge_bytes
            + resolution_bytes
            + filter_bytes
            + edge_buffer_bytes
    }

    /// Compute connected components.
    pub fn connected_components(
        &self,
    ) -> GraphResult<crate::connected_components::ComponentResult> {
        if !self.built {
            return Err(GraphError::NotBuilt);
        }
        Ok(crate::connected_components::compute_components(
            &self.node_store,
            &self.edge_store,
        ))
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! Covers engine-level graph mutations, traversal, search modes, and index
    //! consistency across build-time and sync-time operations.

    use super::*;

    #[test]
    fn resolution_delta_keeps_post_build_inserts_resolvable() {
        let mut engine = Engine::new();
        engine.node_store.add_node(42, "original".to_string());
        engine.resolution_insert(42, "original", 0);
        engine.finalize_resolution();

        engine.node_store.add_node(42, "synced".to_string());
        engine.resolution_insert(42, "synced", 1);

        assert_eq!(engine.resolve(42, "original"), Some(0));
        assert_eq!(engine.resolve(42, "synced"), Some(1));
        assert_eq!(engine.resolve(42, "missing"), None);
    }

    #[test]
    fn new_engine_is_empty() {
        let engine = Engine::new();
        assert_eq!(engine.node_store.node_count(), 0);
        assert_eq!(engine.edge_store.edge_count(), 0);
        assert!(!engine.built);
        assert!(engine.last_build.is_none());
        assert!(engine.last_vacuum.is_none());
        assert!(!engine.is_read_only);
        assert!(engine.edge_buffer.is_empty());
    }

    #[test]
    fn memory_estimation_increases_with_nodes() {
        let mut engine = Engine::new();
        let empty_mem = engine.estimated_memory_used_mb();

        for i in 0..100u32 {
            engine.node_store.add_node(1, format!("n-{}", i));
        }
        engine.edge_store = crate::edge_store::EdgeStore::from_edges(100, vec![], false);

        let with_nodes = engine.estimated_memory_used_mb();
        assert!(
            with_nodes > empty_mem,
            "100 nodes should use more memory: empty={} with_nodes={}",
            empty_mem,
            with_nodes
        );
    }

    #[test]
    fn memory_estimation_accounts_for_edges() {
        let mut engine = Engine::new();
        for i in 0..10u32 {
            engine.node_store.add_node(1, format!("n-{}", i));
        }
        engine.edge_store = crate::edge_store::EdgeStore::from_edges(10, vec![], false);
        let no_edges = engine.estimated_memory_used_mb();

        // Now add edges
        let mut edges = Vec::new();
        for i in 0..9u32 {
            edges.push(crate::edge_store::RawEdge {
                source: i,
                target: i + 1,
                type_id: 1,
                weight: None,
            });
        }
        engine.edge_store = crate::edge_store::EdgeStore::from_edges(10, edges, false);
        let with_edges = engine.estimated_memory_used_mb();

        assert!(
            with_edges > no_edges,
            "edges should increase memory: no_edges={} with_edges={}",
            no_edges,
            with_edges
        );
    }

    #[test]
    fn resolve_miss_returns_none() {
        let engine = Engine::new();
        assert_eq!(engine.resolve(999, "nonexistent"), None);
    }

    #[test]
    fn resolve_wrong_table_oid_returns_none() {
        let mut engine = Engine::new();
        engine.resolution_insert(42, "key", 0);
        // Same key, different table OID
        assert_eq!(engine.resolve(99, "key"), None);
    }

    #[test]
    fn engine_state_fields_reflect_mutations() {
        let mut engine = Engine::new();
        engine.node_store.add_node(10, "A".to_string());
        engine.node_store.add_node(10, "B".to_string());
        engine.edge_type_registry.push("test_edge".to_string());
        engine.has_unidirectional_edges = true;
        engine.built = true;

        // Verify state without calling status() (which uses GUC FFI)
        assert_eq!(engine.node_store.node_count(), 2);
        assert!(engine.has_unidirectional_edges);
        assert!(engine.built);
        assert!(engine.edge_type_registry.contains(&"test_edge".to_string()));
        assert!(engine.estimated_memory_used_mb() > 0.0);
    }

    #[test]
    fn refresh_observed_state_marks_disabled_triggers_stale() {
        let mut engine = Engine::new();
        let catalog_state = Ok((42, None));

        engine.refresh_observed_state(2, 5, &catalog_state);

        assert_eq!(engine.disabled_trigger_count, 2);
        assert_eq!(engine.pending_sync_rows, 5);
        assert_eq!(engine.schema_state, SchemaState::Stale);
        assert_eq!(
            engine.invalid_reason.as_deref(),
            Some("2 graph sync trigger(s) are disabled")
        );
        assert!(!engine.needs_rebuild);
    }

    #[test]
    fn refresh_observed_state_marks_built_graph_invalid_on_catalog_drift() {
        let mut engine = Engine::new();
        engine.built = true;
        engine.catalog_fingerprint = Some(41);
        let catalog_state = Ok((42, None));

        engine.refresh_observed_state(0, 0, &catalog_state);

        assert_eq!(engine.schema_state, SchemaState::Invalid);
        assert!(engine.needs_rebuild);
        assert_eq!(
            engine.invalid_reason.as_deref(),
            Some("registered graph catalog changed since graph.build(); rebuild required")
        );
    }

    #[test]
    fn lifecycle_helpers_update_sync_and_vacuum_state() {
        let mut engine = Engine::new();

        engine.finish_build(None);
        engine.set_catalog_fingerprint(42);
        engine.record_applied_sync_id(7);
        engine.record_pending_sync_rows(3);
        engine.mark_syncing();
        engine.mark_vacuum_required();

        assert!(engine.built);
        assert_eq!(engine.catalog_fingerprint, Some(42));
        assert_eq!(engine.applied_sync_id, 7);
        assert_eq!(engine.pending_sync_rows, 3);
        assert_eq!(engine.sync_status, SyncStatus::Syncing);
        assert!(engine.needs_vacuum);

        engine.record_pending_sync_rows(0);
        engine.mark_idle_if_writable();
        engine.mark_vacuum_complete(None);

        assert_eq!(engine.pending_sync_rows, 0);
        assert_eq!(engine.sync_status, SyncStatus::Idle);
        assert!(!engine.needs_vacuum);
    }

    #[test]
    fn edge_buffer_tracks_pending_mutations() {
        let mut engine = Engine::new();
        assert!(engine.edge_buffer.is_empty());

        engine.edge_buffer.push(crate::engine::EdgeMutation {
            source: 0,
            target: 1,
            type_id: 1,
            kind: MutationKind::Insert,
        });
        engine.edge_buffer.push(crate::engine::EdgeMutation {
            source: 1,
            target: 2,
            type_id: 1,
            kind: MutationKind::Delete,
        });

        assert_eq!(engine.edge_buffer.len(), 2);
        assert_eq!(engine.edge_buffer[0].kind, MutationKind::Insert);
        assert_eq!(engine.edge_buffer[1].kind, MutationKind::Delete);
    }

    #[test]
    fn finalize_resolution_preserves_existing_lookups() {
        let mut engine = Engine::new();
        engine.node_store.add_node(10, "first".to_string());
        engine.node_store.add_node(10, "second".to_string());
        engine.node_store.add_node(20, "other".to_string());
        engine.resolution_insert(10, "first", 0);
        engine.resolution_insert(10, "second", 1);
        engine.resolution_insert(20, "other", 2);

        engine.finalize_resolution();

        assert_eq!(engine.resolve(10, "first"), Some(0));
        assert_eq!(engine.resolve(10, "second"), Some(1));
        assert_eq!(engine.resolve(20, "other"), Some(2));
        assert_eq!(engine.resolve(10, "missing"), None);
    }

    // ─── Test helper ───

    /// Build a small test engine with 5 nodes (A-E), edges, and resolution.
    /// Graph: A→B→C→D, A→E (type 2). All bidirectional.
    #[track_caller]
    fn build_test_engine() -> Engine {
        use crate::edge_store::RawEdge;
        let mut eng = Engine::new();
        let oid = 100u32;
        for (i, pk) in ["A", "B", "C", "D", "E"].iter().enumerate() {
            eng.node_store.add_node(oid, pk.to_string());
            eng.resolution_insert(oid, pk, i as u32);
        }
        eng.register_edge_type("linked").unwrap(); // type_id=1
        eng.register_edge_type("owns").unwrap(); // type_id=2

        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 3,
                target: 2,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 4,
                type_id: 2,
                weight: None,
            },
            RawEdge {
                source: 4,
                target: 0,
                type_id: 2,
                weight: None,
            },
        ];
        eng.edge_store = crate::edge_store::EdgeStore::from_edges(5, edges, false);
        eng.reverse_edge_store = eng.edge_store.reversed();
        eng.built = true;
        eng
    }

    // ─── traverse() ───

    #[test]
    fn traverse_not_built_returns_error() {
        let engine = Engine::new();
        let result = engine.traverse(
            1,
            "A",
            5,
            100000,
            100000,
            None,
            None,
            None,
            TraversalStrategy::Bfs,
            TraversalDirection::Out,
        );
        assert!(matches!(result, Err(GraphError::NotBuilt)));
    }

    #[test]
    fn traverse_nonexistent_seed_returns_node_not_found() {
        let eng = build_test_engine();
        let result = eng.traverse(
            100,
            "GHOST",
            5,
            100000,
            100000,
            None,
            None,
            None,
            TraversalStrategy::Bfs,
            TraversalDirection::Out,
        );
        match result {
            Err(GraphError::NodeNotFound { pk, .. }) => assert_eq!(pk, "GHOST"),
            other => panic!("expected NodeNotFound, got {:?}", other),
        }
    }

    #[test]
    fn traverse_returns_seed_at_depth_zero() {
        let eng = build_test_engine();
        let results = eng
            .traverse(
                100,
                "A",
                5,
                100000,
                100000,
                None,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();
        let seed = results.iter().find(|r| r.node_id == "A").unwrap();
        assert_eq!(seed.depth, 0);
    }

    #[test]
    fn traverse_discovers_all_nodes() {
        let eng = build_test_engine();
        let results = eng
            .traverse(
                100,
                "A",
                10,
                100000,
                100000,
                None,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn traverse_dfs_discovers_all_nodes() {
        let eng = build_test_engine();
        let results = eng
            .traverse(
                100,
                "A",
                10,
                100000,
                100000,
                None,
                None,
                None,
                TraversalStrategy::Dfs,
                TraversalDirection::Out,
            )
            .unwrap();
        assert_eq!(results.len(), 5);
        assert!(results.iter().any(|row| row.node_id == "D"));
    }

    #[test]
    fn traverse_respects_max_depth() {
        let eng = build_test_engine();
        let results = eng
            .traverse(
                100,
                "A",
                1,
                100000,
                100000,
                None,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();
        // depth 0: A, depth 1: B, E
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn traverse_edge_type_filter() {
        let eng = build_test_engine();
        // Only "linked" edges — should not reach E
        let types = Some(vec!["linked".to_string()]);
        let results = eng
            .traverse(
                100,
                "A",
                10,
                100000,
                100000,
                types,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();
        assert!(!results.iter().any(|r| r.node_id == "E"));
        assert_eq!(results.len(), 4); // A, B, C, D
    }

    #[test]
    fn traverse_in_direction_uses_reverse_csr_and_reversed_overlay() {
        let mut eng = Engine::new();
        eng.node_store.add_node(100, "A".to_string());
        eng.node_store.add_node(100, "B".to_string());
        eng.resolution_insert(100, "A", 0);
        eng.resolution_insert(100, "B", 1);
        eng.register_edge_type("follows").unwrap();
        eng.edge_store = crate::edge_store::EdgeStore::from_edges(
            2,
            vec![crate::edge_store::RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
            }],
            false,
        );
        eng.reverse_edge_store = eng.edge_store.reversed();
        eng.built = true;

        let inbound = eng
            .traverse(
                100,
                "B",
                1,
                100000,
                100000,
                Some(vec!["follows".to_string()]),
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::In,
            )
            .unwrap();
        assert!(inbound.iter().any(|row| row.node_id == "A"));

        eng.push_edge_mutation(EdgeMutation {
            source: 1,
            target: 0,
            type_id: 1,
            kind: MutationKind::Insert,
        })
        .unwrap();
        let inbound_overlay = eng
            .traverse(
                100,
                "A",
                1,
                100000,
                100000,
                Some(vec!["follows".to_string()]),
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::In,
            )
            .unwrap();
        assert!(inbound_overlay.iter().any(|row| row.node_id == "B"));
    }

    #[test]
    fn traverse_invalid_filter_returns_error() {
        let eng = build_test_engine();
        let result = eng.traverse(
            100,
            "A",
            5,
            100000,
            100000,
            None,
            Some("bad >>= lol"),
            None,
            TraversalStrategy::Bfs,
            TraversalDirection::Out,
        );
        assert!(matches!(result, Err(GraphError::InvalidFilter { .. })));
    }

    #[test]
    fn traverse_unknown_edge_type_traverses_nothing_extra() {
        let eng = build_test_engine();
        let types = Some(vec!["nonexistent_type".to_string()]);
        let result = eng.traverse(
            100,
            "A",
            10,
            100000,
            100000,
            types,
            None,
            None,
            TraversalStrategy::Bfs,
            TraversalDirection::Out,
        );
        assert!(matches!(result, Err(GraphError::InvalidFilter { .. })));
    }

    #[test]
    fn traverse_seed_without_csr_slot_is_safe() {
        let mut eng = Engine::new();
        let oid = 100u32;
        eng.node_store.add_node(oid, "orphan".to_string());
        eng.resolution_insert(oid, "orphan", 0);
        eng.built = true;

        let results = eng
            .traverse(
                oid,
                "orphan",
                3,
                100_000,
                100_000,
                None,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "orphan");
        assert_eq!(results[0].depth, 0);
    }

    // ─── shortest_path() ───

    #[test]
    fn shortest_path_not_built_returns_error() {
        let engine = Engine::new();
        let result = engine.shortest_path(1, "A", 1, "B", 10);
        assert!(matches!(result, Err(GraphError::NotBuilt)));
    }

    #[test]
    fn shortest_path_nonexistent_source() {
        let eng = build_test_engine();
        let result = eng.shortest_path(100, "GHOST", 100, "A", 10);
        assert!(matches!(result, Err(GraphError::NodeNotFound { .. })));
    }

    #[test]
    fn shortest_path_nonexistent_target() {
        let eng = build_test_engine();
        let result = eng.shortest_path(100, "A", 100, "GHOST", 10);
        assert!(matches!(result, Err(GraphError::NodeNotFound { .. })));
    }

    #[test]
    fn shortest_path_returns_correct_path() {
        let eng = build_test_engine();
        let steps = eng.shortest_path(100, "A", 100, "D", 10).unwrap();
        assert!(!steps.is_empty());
        assert_eq!(steps.first().unwrap().node_id, "A");
        assert_eq!(steps.last().unwrap().node_id, "D");
    }

    // ─── weighted_shortest_path() ───

    #[test]
    fn weighted_shortest_path_not_built_returns_error() {
        let engine = Engine::new();
        let result = engine.weighted_shortest_path(1, "A", 1, "B");
        assert!(matches!(result, Err(GraphError::NotBuilt)));
    }

    #[test]
    fn weighted_shortest_path_unweighted_graph_returns_none() {
        let eng = build_test_engine(); // unweighted
        let result = eng.weighted_shortest_path(100, "A", 100, "D").unwrap();
        assert!(result.is_empty());
    }

    // ─── connected_components() ───

    #[test]
    fn connected_components_not_built_returns_error() {
        let engine = Engine::new();
        let result = engine.connected_components();
        assert!(matches!(result, Err(GraphError::NotBuilt)));
    }

    #[test]
    fn connected_components_single_component() {
        let eng = build_test_engine();
        let result = eng.connected_components().unwrap();
        assert_eq!(result.num_components, 1);
        assert_eq!(result.largest_component_size, 5);
    }

    // ─── register_edge_type() ───

    #[test]
    fn register_edge_type_returns_existing_id_for_duplicate() {
        let mut engine = Engine::new();
        let id1 = engine.register_edge_type("friend").unwrap();
        let id2 = engine.register_edge_type("friend").unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn register_edge_type_limit_at_255() {
        let mut engine = Engine::new();
        // Index 0 is already "" (untyped). Fill up to 255 total.
        for i in 1..255u16 {
            engine.register_edge_type(&format!("type_{}", i)).unwrap();
        }
        assert_eq!(engine.edge_type_registry.len(), 255);
        let result = engine.register_edge_type("one_too_many");
        assert!(matches!(result, Err(GraphError::EdgeTypeLimit)));
    }

    #[test]
    #[ignore = "Scale tests are slow and should only be run manually with --ignored"]
    fn scale_test_1m_nodes() {
        let mut engine = Engine::new();
        let node_count = 1_000_000;
        let mut edges = Vec::with_capacity(20_000);

        for i in 0..node_count {
            let pk = format!("NODE-{}", i);
            let idx = engine.node_store.add_node(100, pk.clone());
            engine.resolution_insert(100, &pk, idx);
            if i > 0 && i <= 20_000 {
                edges.push(crate::edge_store::RawEdge {
                    source: i - 1,
                    target: i,
                    type_id: 1,
                    weight: Some(1),
                });
            }
        }

        engine.edge_store = crate::edge_store::EdgeStore::from_edges(node_count, edges, true);
        engine.built = true;
        let results = engine
            .traverse(
                100,
                "NODE-0",
                5,
                1000,
                1000,
                None,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();

        let node_store_bytes = engine.node_store.node_count() * 16;
        assert!(node_store_bytes > 0);
        assert_eq!(engine.edge_store.edge_count(), 20_000);
        assert!(results.iter().any(|row| row.node_id == "NODE-5"));
    }
}
