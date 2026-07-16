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
use crate::projection::layered::{
    LayeredNeighbors, LayeredSnapshot, ManifestSegmentProvider, SegmentProvider,
};
use crate::projection::manifest::ProjectionManifest;
use crate::projection::neighbors::{
    CsrNeighbors, EdgeOverlay, OverlayDeletes, OverlayInserts, OverlayNeighbors,
};
use crate::projection::tx_delta;
use crate::relationship_identity_store::RelationshipIdentityStore;
use crate::resolution_index::{ResolutionDeltaIndex, ResolutionIndex, ResolutionIndexBuilder};
use crate::safety::{GraphError, GraphResult};
use crate::types::*;

use pgrx::prelude::TimestampWithTimeZone;
use roaring::RoaringBitmap;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Resolution storage backend.
///
/// - `Builder`: compact append-only entries used during node ingestion.
/// - `Finalized`: Sorted array, used after build, compact memory, binary search.
/// - `MmapBacked`: Resolution section borrowed from the backend-local immutable
///   artifact snapshot.
pub(crate) enum ResolutionStore {
    /// Build-time: compact entries. Converted to Finalized before edge linking.
    Builder(ResolutionIndexBuilder),
    /// Post-build: sorted array in owned memory with binary-search lookups.
    Finalized(Vec<u8>),
    /// Loaded from the backend-local `.pggraph` snapshot. The mapping handle is
    /// held by [`Engine::_mmap`]; lookups borrow that section's bytes.
    MmapBacked(MmapResolutionState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MmapResolutionState {
    offset: usize,
    len: usize,
}

impl MmapResolutionState {
    pub(crate) fn new(offset: usize, len: usize) -> Self {
        Self { offset, len }
    }

    fn range(&self) -> std::ops::Range<usize> {
        self.offset..self.offset + self.len
    }
}

pub(crate) struct MmapBackedGraph {
    pub(crate) node_store: NodeStore,
    pub(crate) edge_store: EdgeStore,
    pub(crate) reverse_edge_store: EdgeStore,
    pub(crate) filter_index: FilterIndex,
    pub(crate) edge_type_registry: Vec<String>,
    pub(crate) relationship_identities: RelationshipIdentityStore,
    pub(crate) mmap: Arc<memmap2::Mmap>,
    pub(crate) resolution_state: MmapResolutionState,
}

/// The core graph engine. Holds all data stores.
pub struct Engine {
    /// Node metadata. After `.pggraph` load, base arrays are mmap-backed until a
    /// sync mutation materializes them into owned arrays.
    pub(crate) node_store: NodeStore,
    /// Forward CSR adjacency. After `.pggraph` load, this store is mmap-backed.
    pub(crate) edge_store: EdgeStore,
    /// Inbound CSR adjacency. Persisted artifacts map it directly.
    pub(crate) reverse_edge_store: EdgeStore,
    /// Traversal filter index. After `.pggraph` load, immutable values and text
    /// dictionaries remain mapped while transaction deltas use bounded heap.
    pub(crate) filter_index: FilterIndex,
    /// Edge label registry. The bounded artifact section is decoded into
    /// backend-local heap after validation.
    pub(crate) edge_type_registry: Vec<String>,
    /// Source-row identities indexed by nonzero CSR relationship IDs. Persisted
    /// base descriptors and keys remain mapped; later identities form a suffix.
    pub(crate) relationship_identities: RelationshipIdentityStore,
    pub(crate) has_unidirectional_edges: bool,
    pub(crate) built: bool,
    pub(crate) sync_status: SyncStatus,
    pub(crate) last_build: Option<TimestampWithTimeZone>,
    pub(crate) last_vacuum: Option<TimestampWithTimeZone>,
    pub(crate) build_resource_budget_bytes: u64,
    pub(crate) build_resource_peak_bytes: u64,
    pub(crate) build_resource_peak_phase: Option<crate::resource::ResourcePhase>,
    pub(crate) build_resource_pressure_events: u64,

    /// Resolution store — switches from compact build entries to sorted array.
    pub(crate) resolution_store: ResolutionStore,

    /// Post-build indexed resolution delta for nodes inserted by sync replay.
    ///
    /// Finalized and mmap-backed resolution indexes are immutable. Inserts that
    /// arrive after build() live here until the next rebuild/vacuum merge.
    pub(crate) resolution_delta: ResolutionDeltaIndex,

    /// Backend-local immutable snapshot used by resolution and accounting.
    /// Mapped node and edge stores retain their own `Arc` clones.
    pub(crate) _mmap: Option<Arc<memmap2::Mmap>>,
    /// Edge mutation buffer for trigger sync.
    /// Pending edge mutations that haven't been merged into CSR yet.
    pub(crate) edge_buffer: Vec<EdgeMutation>,
    /// Runtime projection mode selected at build/load time.
    pub(crate) projection_mode: crate::config::ProjectionMode,
    /// Durable projection generation loaded with the base artifact, if any.
    pub(crate) projection_manifest: Option<ProjectionManifestSnapshot>,
    /// Full durable projection manifest for segment-backed runtime reads.
    pub(crate) projection_manifest_full: Option<ProjectionManifest>,
    /// Projection artifact directory for manifest-referenced segment files.
    pub(crate) projection_manifest_root: Option<PathBuf>,
    /// Fully decoded immutable durable state pinned with the generation.
    pub(crate) projection_snapshot: Option<LayeredSnapshot>,

    /// When true, the engine is in read-only mode.
    /// Sync inserts/updates/deletes are rejected until a rebuild installs a
    /// read-write engine.
    pub(crate) is_read_only: bool,
    /// Operator-facing reason for read-only mode.
    pub(crate) read_only_reason: Option<ReadOnlyReason>,

    /// Last durable sync-log row applied by this backend-local engine.
    pub(crate) applied_sync_id: i64,
    /// Whether pending edge/node deltas require CSR rebuild.
    pub(crate) needs_vacuum: bool,
    /// Whether catalog/schema drift requires graph rebuild.
    pub(crate) needs_rebuild: bool,
    /// Schema state exposed by graph.status().
    pub(crate) schema_state: SchemaState,
    /// Human-readable schema/sync invalidation reason.
    pub(crate) invalid_reason: Option<String>,
    /// Disabled graph trigger count from the last schema/status check.
    pub(crate) disabled_trigger_count: i32,
    /// Fingerprint of registered graph catalog state captured at build time.
    pub(crate) catalog_fingerprint: Option<u64>,
    /// Number of pending durable sync rows from the last status/catch-up check.
    pub(crate) pending_sync_rows: i64,
    /// Active node membership by source table for table-scoped sync operations.
    pub(crate) table_membership: HashMap<u32, RoaringBitmap>,
    /// Tenant membership by tenant value for tenanted table rows.
    pub(crate) tenant_membership: HashMap<String, RoaringBitmap>,
    /// Table OIDs that require tenant scoping.
    pub(crate) tenanted_table_oids: HashSet<u32>,
}

/// A pending edge mutation from trigger sync.
#[derive(Debug, Clone)]
pub struct EdgeMutation {
    pub(crate) source: u32,
    pub(crate) target: u32,
    pub(crate) type_id: u8,
    pub(crate) schema_reversed: bool,
    pub(crate) relationship_id: Option<crate::edge_store::RelationshipId>,
    pub(crate) kind: MutationKind,
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
    EdgeBufferFull,
}

impl std::fmt::Display for ReadOnlyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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

/// Minimal manifest metadata held by a backend-local engine snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionManifestSnapshot {
    pub(crate) generation_id: u64,
    pub(crate) sync_watermark: i64,
}

impl From<&ProjectionManifest> for ProjectionManifestSnapshot {
    fn from(manifest: &ProjectionManifest) -> Self {
        Self {
            generation_id: manifest.generation_id,
            sync_watermark: manifest.sync_watermark,
        }
    }
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
            relationship_identities: RelationshipIdentityStore::default(),
            has_unidirectional_edges: false,
            built: false,
            sync_status: SyncStatus::Idle,
            last_build: None,
            last_vacuum: None,
            build_resource_budget_bytes: 0,
            build_resource_peak_bytes: 0,
            build_resource_peak_phase: None,
            build_resource_pressure_events: 0,
            resolution_store: ResolutionStore::Builder(ResolutionIndexBuilder::new()),
            resolution_delta: ResolutionDeltaIndex::new(),
            _mmap: None,
            edge_buffer: Vec::new(),
            projection_mode: crate::config::ProjectionMode::CsrReadonly,
            projection_manifest: None,
            projection_manifest_full: None,
            projection_manifest_root: None,
            projection_snapshot: None,
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

    /// Replace both forward and reverse CSR stores from one validated forward store.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::Oom` if the reverse CSR cannot be allocated.
    pub fn replace_edge_stores(&mut self, edge_store: EdgeStore) -> GraphResult<()> {
        let reverse_edge_store = edge_store.try_reversed()?;
        self.edge_store = edge_store;
        self.reverse_edge_store = reverse_edge_store;
        Ok(())
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

    pub fn set_projection_mode(&mut self, projection_mode: crate::config::ProjectionMode) {
        self.projection_mode = projection_mode;
    }

    pub(crate) fn install_projection_manifest(
        &mut self,
        manifest: &ProjectionManifest,
        root: impl Into<PathBuf>,
    ) -> crate::safety::GraphResult<()> {
        self.install_projection_manifest_internal(manifest, root.into(), true)
    }

    pub(crate) fn install_projection_candidate(
        &mut self,
        manifest: &ProjectionManifest,
        root: impl Into<PathBuf>,
    ) -> crate::safety::GraphResult<()> {
        self.install_projection_manifest_internal(manifest, root.into(), false)
    }

    fn install_projection_manifest_internal(
        &mut self,
        manifest: &ProjectionManifest,
        root: PathBuf,
        record_heartbeat: bool,
    ) -> crate::safety::GraphResult<()> {
        let provider = ManifestSegmentProvider::new(&root, manifest);
        let segments = provider.load_segments()?;
        let base_chunks = provider.load_base_chunks()?;
        let mut node_store = self.node_store.to_owned_store();
        let mut resolution_delta = ResolutionDeltaIndex::new();
        let mut filter_index = self.filter_index.clone();
        let mut table_membership = self.table_membership.clone();
        let mut tenant_membership = self.tenant_membership.clone();
        let mut tenanted_table_oids = self.tenanted_table_oids.clone();

        for segment in &segments {
            for resolution in &segment.resolutions {
                let exact_primary_key = match resolution.primary_key.as_deref() {
                    Some(primary_key) => primary_key.to_string(),
                    None => {
                        let primary_key =
                            node_store.primary_key(resolution.node_idx).ok_or_else(|| {
                                GraphError::CorruptFile {
                                reason:
                                    "version 5 projection resolution references a missing base node"
                                        .to_string(),
                            }
                            })?;
                        if node_store.table_oid(resolution.node_idx) != Some(resolution.table_oid)
                            || ResolutionIndexBuilder::hash_pk(primary_key) != resolution.pk_hash
                        {
                            return Err(GraphError::CorruptFile {
                                reason:
                                    "version 5 projection resolution does not match its base node"
                                        .to_string(),
                            });
                        }
                        primary_key.to_string()
                    }
                };
                let node_count = node_store.node_count();
                if resolution.node_idx == node_count {
                    node_store.add_node(resolution.table_oid, exact_primary_key.clone());
                } else if resolution.node_idx > node_count {
                    return Err(GraphError::CorruptFile {
                        reason: format!(
                            "projection node allocation contains an index gap: slot {} ({}, table {}) follows node count {}",
                            resolution.node_idx, exact_primary_key, resolution.table_oid, node_count
                        ),
                    });
                } else if node_store.table_oid(resolution.node_idx) != Some(resolution.table_oid)
                    || node_store.primary_key(resolution.node_idx)
                        != Some(exact_primary_key.as_str())
                {
                    return Err(GraphError::CorruptFile {
                        reason: "projection node slot changes an existing identity".to_string(),
                    });
                }

                resolution_delta.insert(
                    resolution.table_oid,
                    &exact_primary_key,
                    resolution.node_idx,
                );

                if resolution.tombstone {
                    node_store.deactivate(resolution.node_idx);
                    if let Some(nodes) = table_membership.get_mut(&resolution.table_oid) {
                        nodes.remove(resolution.node_idx);
                    }
                } else {
                    if !node_store.is_active(resolution.node_idx) {
                        return Err(GraphError::CorruptFile {
                            reason: "projection cannot reactivate an inactive node slot"
                                .to_string(),
                        });
                    }
                    table_membership
                        .entry(resolution.table_oid)
                        .or_default()
                        .insert(resolution.node_idx);
                }
            }
            for state in &segment.node_states {
                if state.node_idx >= node_store.node_count() {
                    return Err(GraphError::CorruptFile {
                        reason: "projection node state references a missing node".to_string(),
                    });
                }
                if state.active != node_store.is_active(state.node_idx) {
                    return Err(GraphError::CorruptFile {
                        reason: "projection node state disagrees with exact identity state"
                            .to_string(),
                    });
                }
            }
        }

        for segment in &segments {
            for relationship_id in segment
                .edge_inserts
                .iter()
                .chain(segment.edge_deletes.iter())
                .filter_map(|edge| edge.relationship_id)
                .chain(
                    segment
                        .edge_weights
                        .iter()
                        .filter_map(|edge| edge.relationship_id),
                )
            {
                if self.relationship_identities.get(relationship_id).is_none() {
                    return Err(GraphError::CorruptFile {
                        reason: format!(
                            "projection segment relationship ID {relationship_id} is outside the active identity dictionary"
                        ),
                    });
                }
            }
            for edge in segment
                .edge_inserts
                .iter()
                .chain(segment.edge_deletes.iter())
            {
                if edge.source >= node_store.node_count() || edge.target >= node_store.node_count()
                {
                    return Err(GraphError::CorruptFile {
                        reason: "projection edge references a missing node".to_string(),
                    });
                }
            }
            for edge in &segment.edge_weights {
                if edge.source >= node_store.node_count() || edge.target >= node_store.node_count()
                {
                    return Err(GraphError::CorruptFile {
                        reason: "projection edge weight references a missing node".to_string(),
                    });
                }
            }
            for tenant in &segment.tenants {
                let exact_tenant = tenant.tenant.as_deref().ok_or_else(|| {
                    GraphError::IncompatibleVersion(
                        "version 5 tenant projection lacks exact identity; rebuild required"
                            .to_string(),
                    )
                })?;
                let table_oid = node_store.table_oid(tenant.node_idx).ok_or_else(|| {
                    GraphError::CorruptFile {
                        reason: "projection tenant references a missing node".to_string(),
                    }
                })?;
                tenanted_table_oids.insert(table_oid);
                let membership = tenant_membership
                    .entry(exact_tenant.to_string())
                    .or_default();
                if tenant.tombstone {
                    membership.remove(tenant.node_idx);
                } else {
                    membership.insert(tenant.node_idx);
                }
            }
            for filter in &segment.filters {
                let column_idx =
                    usize::try_from(filter.column_id).map_err(|_| GraphError::CorruptFile {
                        reason: "projection filter column id exceeds usize".to_string(),
                    })?;
                filter_index.apply_persisted_value(
                    column_idx,
                    filter.node_idx,
                    node_store.node_count(),
                    &filter.value,
                    filter.tombstone,
                )?;
            }
        }
        if record_heartbeat {
            crate::projection::manifest::record_loaded_generation_heartbeat(manifest)?;
        }
        let projection_snapshot = LayeredSnapshot::build(&self.edge_store, &base_chunks, &segments);
        self.node_store = node_store;
        self.resolution_delta = resolution_delta;
        self.filter_index = filter_index;
        self.table_membership = table_membership;
        self.tenant_membership = tenant_membership;
        self.tenanted_table_oids = tenanted_table_oids;
        self.projection_manifest = Some(ProjectionManifestSnapshot::from(manifest));
        self.projection_manifest_full = Some(manifest.clone());
        self.projection_manifest_root = Some(root);
        self.projection_snapshot = Some(projection_snapshot);
        Ok(())
    }

    pub(crate) fn base_projection_manifest_status(&self) -> (Option<i64>, Option<i64>) {
        let generation = self
            .projection_manifest
            .as_ref()
            .and_then(|manifest| i64::try_from(manifest.generation_id).ok());
        let watermark = self
            .projection_manifest
            .as_ref()
            .map(|manifest| manifest.sync_watermark);
        (generation, watermark)
    }

    fn segment_backed_projection_manifest(&self) -> Option<(&ProjectionManifest, &PathBuf)> {
        if self.projection_mode == crate::config::ProjectionMode::CsrReadonly {
            return None;
        }
        let manifest = self.projection_manifest_full.as_ref()?;
        if manifest.segments.is_empty() && manifest.base_chunks.is_empty() {
            return None;
        }
        Some((manifest, self.projection_manifest_root.as_ref()?))
    }

    pub(crate) fn layered_neighbors(&self) -> GraphResult<Option<LayeredNeighbors<'_>>> {
        let Some((_manifest, _root)) = self.segment_backed_projection_manifest() else {
            return Ok(None);
        };
        Ok(Some(LayeredNeighbors::from_snapshot_with_overlays(
            &self.edge_store,
            &self.reverse_edge_store,
            self.projection_snapshot.as_ref().ok_or_else(|| {
                GraphError::Internal("projection snapshot is missing".to_string())
            })?,
            self.traversal_edge_overlay(TraversalDirection::Out),
            self.traversal_edge_overlay(TraversalDirection::In),
        )))
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

    pub(crate) fn record_build_resource_stats(
        &mut self,
        budget: crate::resource::ByteCount,
        peak: crate::resource::ByteCount,
        peak_phase: Option<crate::resource::ResourcePhase>,
        pressure_events: u64,
    ) {
        self.build_resource_budget_bytes = budget.as_u64();
        self.build_resource_peak_bytes = peak.as_u64();
        self.build_resource_peak_phase = peak_phase;
        self.build_resource_pressure_events = pressure_events;
    }

    pub(crate) fn install_mmap_backed_graph(&mut self, graph: MmapBackedGraph) -> GraphResult<()> {
        self.node_store = graph.node_store;
        self.edge_store = graph.edge_store;
        self.reverse_edge_store = graph.reverse_edge_store;
        self.filter_index = graph.filter_index;
        self.edge_type_registry = graph.edge_type_registry;
        self.relationship_identities = graph.relationship_identities;
        self.rebuild_table_membership();
        self.finish_build(None);
        self.resolution_store = ResolutionStore::MmapBacked(graph.resolution_state);
        self._mmap = Some(graph.mmap);
        Ok(())
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

    pub(crate) fn edge_type_id(&self, label: &str) -> Option<u8> {
        self.edge_type_registry
            .iter()
            .position(|registered| registered == label)
            .map(|idx| idx as u8)
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
                && !tx_delta::node_deleted(idx)
                && self.node_store.table_oid(idx) == Some(table_oid)
                && self.node_store.primary_key(idx) == Some(pk)
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

    pub(crate) fn resolve_identity(&self, table_oid: u32, pk: &str) -> Option<u32> {
        let verify = |idx: u32| {
            idx < self.node_store.node_count()
                && self.node_store.table_oid(idx) == Some(table_oid)
                && self.node_store.primary_key(idx) == Some(pk)
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
                ResolutionIndex::from_bytes(&mmap[resolution_state.range()])?
                    .resolve_verified(table_oid, pk, verify)
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

    /// Fallibly insert into the build-time resolution accumulator.
    pub(crate) fn try_build_resolution_insert(
        &mut self,
        table_oid: u32,
        pk: &str,
        node_idx: u32,
    ) -> GraphResult<()> {
        match &mut self.resolution_store {
            ResolutionStore::Builder(builder) => builder.try_insert(table_oid, pk, node_idx),
            _ => Err(GraphError::Internal(
                "build resolution insertion requires builder mode".to_string(),
            )),
        }
    }

    pub fn insert_tenant_membership(&mut self, tenant: &str, node_idx: u32) {
        self.tenant_membership
            .entry(tenant.to_string())
            .or_default()
            .insert(node_idx);
    }

    pub(crate) fn try_build_insert_tenant_membership(
        &mut self,
        tenant: &str,
        node_idx: u32,
    ) -> GraphResult<()> {
        self.tenant_membership
            .try_reserve(1)
            .map_err(|_| GraphError::Oom {
                used_mb: 0,
                need_mb: 1,
                limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
            })?;
        self.insert_tenant_membership(tenant, node_idx);
        Ok(())
    }

    pub fn insert_table_membership(&mut self, table_oid: u32, node_idx: u32) {
        self.table_membership
            .entry(table_oid)
            .or_default()
            .insert(node_idx);
    }

    pub(crate) fn try_build_insert_table_membership(
        &mut self,
        table_oid: u32,
        node_idx: u32,
    ) -> GraphResult<()> {
        self.table_membership
            .try_reserve(1)
            .map_err(|_| GraphError::Oom {
                used_mb: 0,
                need_mb: 1,
                limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
            })?;
        self.insert_table_membership(table_oid, node_idx);
        Ok(())
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
                if let Some(table_oid) = self.node_store.table_oid(node_idx) {
                    self.insert_table_membership(table_oid, node_idx);
                }
            }
        }
    }

    #[cfg(feature = "development")]
    #[allow(
        dead_code,
        reason = "development SQL helpers keep tenant-removal behavior available for targeted replay checks"
    )]
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

    /// Prepare node storage for a sync operation that may mutate node state.
    pub fn prepare_sync_node_mutation(&mut self) {
        self.materialize_mmap_node_store_for_sync();
    }

    /// Insert an active sync node, materializing mmap-backed node arrays first.
    pub fn insert_sync_node(&mut self, table_oid: u32, pk: &str) -> u32 {
        self.materialize_mmap_node_store_for_sync();
        self.node_store.add_node(table_oid, pk.to_string())
    }

    /// Tombstone an active sync node, materializing mmap-backed arrays first.
    pub fn tombstone_sync_node(&mut self, table_oid: u32, node_idx: u32) -> bool {
        self.materialize_mmap_node_store_for_sync();
        if self.node_store.is_active(node_idx)
            && self.node_store.table_oid(node_idx) == Some(table_oid)
        {
            self.node_store.deactivate(node_idx);
            return true;
        }
        false
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
        if filter_condition.is_some() {
            return Err(GraphError::InvalidFilter {
                reason: "legacy raw traversal filters have been removed; use structured JSONB filters through SQL traversal helpers".to_string(),
            });
        }
        let filter_ops = Vec::new();

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
    #[allow(dead_code, reason = "compatibility entry point")]
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
        let governor = self.query_resource_governor()?;
        self.traverse_with_filter_ops_governed(
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
            &governor,
        )
    }

    /// Traverses with a caller-owned resource governor.
    ///
    /// SQL callers use this variant to keep execution, result conversion,
    /// blocking operators, and hydration inside one operation budget.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn traverse_with_filter_ops_governed(
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
        governor: &crate::resource::ResourceGovernor,
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

        let traversal_bytes = bfs::estimated_workspace_bytes(
            self.node_store.node_count() as usize,
            max_nodes,
            max_frontier,
        )?;
        let _workspace = governor
            .reserve_memory(
                crate::resource::ResourcePhase::QueryFrontier,
                traversal_bytes,
            )
            .map_err(crate::safety::resource_limit_error)?;
        let overlay_bytes = self.estimated_traversal_overlay_clone_bytes()?;
        let _overlay_workspace = governor
            .reserve_memory(crate::resource::ResourcePhase::QueryExpand, overlay_bytes)
            .map_err(crate::safety::resource_limit_error)?;

        let layered_neighbors = self.layered_neighbors()?;
        let (overlay_insert_edges, overlay_deleted_edges) = if layered_neighbors.is_some() {
            (HashMap::new(), HashMap::new())
        } else {
            self.traversal_edge_overlay(direction)
        };
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

        let bfs_result = match (strategy, layered_neighbors.as_ref()) {
            (TraversalStrategy::Bfs, Some(layered)) => {
                let neighbors = layered.for_direction(direction);
                bfs::execute_with_neighbors_governed(
                    &self.node_store,
                    &neighbors,
                    &self.filter_index,
                    &config,
                    governor,
                )?
            }
            (TraversalStrategy::Dfs, Some(layered)) => {
                let neighbors = layered.for_direction(direction);
                bfs::execute_dfs_with_neighbors_governed(
                    &self.node_store,
                    &neighbors,
                    &self.filter_index,
                    &config,
                    governor,
                )?
            }
            (TraversalStrategy::Bfs, None) => bfs::execute_governed(
                &self.node_store,
                edge_store,
                &self.filter_index,
                &config,
                governor,
            )?,
            (TraversalStrategy::Dfs, None) => bfs::execute_dfs_governed(
                &self.node_store,
                edge_store,
                &self.filter_index,
                &config,
                governor,
            )?,
        };
        let output_bytes = self.traversal_result_upper_bound(&bfs_result, max_depth)?;
        let output_work = bfs_result
            .visited
            .len()
            .checked_mul(
                u64::try_from(max_depth.max(0))
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .ok_or_else(|| GraphError::Internal("traversal output work overflowed".to_string()))?;
        governor
            .consume_work(
                crate::resource::ResourcePhase::QueryCandidates,
                crate::resource::WorkUnits::new(output_work),
            )
            .map_err(crate::safety::resource_limit_error)?;
        let output_lease = governor
            .reserve_memory(
                crate::resource::ResourcePhase::QueryCandidates,
                output_bytes,
            )
            .map_err(crate::safety::resource_limit_error)?;
        let results =
            bfs::to_traversal_results(&bfs_result, &self.node_store, &self.edge_type_registry)?;
        output_lease.retain_until_governor_drop();
        Ok(results)
    }

    fn traversal_result_upper_bound(
        &self,
        bfs_result: &bfs::BfsResult,
        max_depth: i32,
    ) -> GraphResult<crate::resource::ByteCount> {
        let rows = bfs_result.visited.len();
        let path_width = u64::try_from(max_depth.max(0))
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let primary_key = u64::try_from(self.node_store.max_primary_key_bytes())
            .map_err(|_| GraphError::Internal("primary-key width does not fit u64".to_string()))?;
        let edge_label = self
            .edge_type_registry
            .iter()
            .map(String::len)
            .max()
            .map(u64::try_from)
            .transpose()
            .map_err(|_| GraphError::Internal("edge-label width does not fit u64".to_string()))?
            .unwrap_or(3);
        let row_bytes = u64::try_from(std::mem::size_of::<TraversalResult>())
            .unwrap_or(u64::MAX)
            .checked_add(primary_key)
            .and_then(|bytes| {
                path_width
                    .checked_mul(
                        u64::try_from(std::mem::size_of::<crate::types::PathCoordinate>())
                            .unwrap_or(u64::MAX)
                            .saturating_add(primary_key)
                            .saturating_add(
                                u64::try_from(std::mem::size_of::<String>()).unwrap_or(u64::MAX),
                            )
                            .saturating_add(edge_label),
                    )
                    .and_then(|paths| bytes.checked_add(paths))
            })
            .ok_or_else(|| {
                GraphError::Internal("traversal result estimate overflowed".to_string())
            })?;
        rows.checked_mul(row_bytes)
            .map(crate::resource::ByteCount::from_bytes)
            .ok_or_else(|| GraphError::Internal("traversal result estimate overflowed".to_string()))
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

    pub(crate) fn has_edge_overlay(&self) -> bool {
        !self.edge_buffer.is_empty() || tx_delta::edge_delta_dirty()
    }

    pub(crate) fn traversal_edge_overlay(&self, direction: TraversalDirection) -> EdgeOverlay {
        let mut inserts: HashSet<(
            u32,
            u32,
            u8,
            bool,
            Option<crate::edge_store::RelationshipId>,
        )> = HashSet::new();
        let mut deletes: HashSet<(
            u32,
            u32,
            u8,
            bool,
            Option<crate::edge_store::RelationshipId>,
        )> = HashSet::new();

        for mutation in &self.edge_buffer {
            let (source, target, type_id) = match direction {
                TraversalDirection::Any | TraversalDirection::Out => {
                    (mutation.source, mutation.target, mutation.type_id)
                }
                TraversalDirection::In => (mutation.target, mutation.source, mutation.type_id),
            };
            match mutation.kind {
                MutationKind::Insert => {
                    deletes.retain(|deleted| {
                        deleted.0 != source
                            || deleted.1 != target
                            || deleted.2 != type_id
                            || deleted.3 != mutation.schema_reversed
                            || deleted.4 != mutation.relationship_id
                    });
                    inserts.insert((
                        source,
                        target,
                        type_id,
                        mutation.schema_reversed,
                        mutation.relationship_id,
                    ));
                }
                MutationKind::Delete => {
                    inserts.retain(|inserted| {
                        inserted.0 != source
                            || inserted.1 != target
                            || inserted.2 != type_id
                            || inserted.3 != mutation.schema_reversed
                            || (mutation.relationship_id.is_some()
                                && inserted.4 != mutation.relationship_id)
                    });
                    deletes.insert((
                        source,
                        target,
                        type_id,
                        mutation.schema_reversed,
                        mutation.relationship_id,
                    ));
                }
            }
        }

        let (tx_inserts, tx_deletes) = tx_delta::edge_overlay(direction);
        for (source, targets) in tx_deletes {
            for (target, type_id, schema_reversed, relationship_id) in targets {
                inserts.retain(|inserted| {
                    inserted.0 != source
                        || inserted.1 != target
                        || inserted.2 != type_id
                        || inserted.3 != schema_reversed
                        || (relationship_id.is_some() && inserted.4 != relationship_id)
                });
                deletes.insert((source, target, type_id, schema_reversed, relationship_id));
            }
        }
        for (source, targets) in tx_inserts {
            for (target, type_id, schema_reversed, relationship_id) in targets {
                deletes.retain(|deleted| {
                    deleted.0 != source
                        || deleted.1 != target
                        || deleted.2 != type_id
                        || deleted.3 != schema_reversed
                        || deleted.4 != relationship_id
                });
                inserts.insert((source, target, type_id, schema_reversed, relationship_id));
            }
        }

        let mut insert_map: OverlayInserts = HashMap::new();
        for (source, target, type_id, schema_reversed, relationship_id) in inserts {
            insert_map.entry(source).or_default().push((
                target,
                type_id,
                schema_reversed,
                relationship_id,
            ));
        }
        let mut delete_map: OverlayDeletes = HashMap::new();
        for (source, target, type_id, schema_reversed, relationship_id) in deletes {
            delete_map.entry(source).or_default().insert((
                target,
                type_id,
                schema_reversed,
                relationship_id,
            ));
        }
        (insert_map, delete_map)
    }

    pub(crate) fn estimated_traversal_overlay_clone_bytes(
        &self,
    ) -> GraphResult<crate::resource::ByteCount> {
        let tx_stats = tx_delta::stats();
        let edges = self
            .edge_buffer
            .len()
            .checked_add(tx_stats.added_edges)
            .and_then(|count| count.checked_add(tx_stats.deleted_edges))
            .ok_or_else(|| GraphError::Internal("overlay edge count overflow".to_string()))?;
        // Each logical edge may appear in both orientation maps. The estimate
        // includes hash buckets, vector/set allocation, and allocator slack.
        let bytes = edges
            .checked_mul(2)
            .and_then(|count| count.checked_mul(160))
            .ok_or_else(|| GraphError::Internal("overlay workspace overflow".to_string()))?;
        crate::resource::ByteCount::from_usize(bytes)
            .ok_or_else(|| GraphError::Internal("overlay workspace does not fit u64".to_string()))
    }

    /// Find shortest path between two nodes.
    #[allow(dead_code, reason = "compatibility entry point")]
    pub fn shortest_path(
        &self,
        source_table_oid: u32,
        source_id: &str,
        target_table_oid: u32,
        target_id: &str,
        max_depth: i32,
    ) -> GraphResult<Vec<PathStep>> {
        let governor = self.query_resource_governor()?;
        self.shortest_path_governed(
            source_table_oid,
            source_id,
            target_table_oid,
            target_id,
            max_depth,
            &governor,
        )
    }

    /// Finds an unweighted path within a caller-owned operation budget.
    pub(crate) fn shortest_path_governed(
        &self,
        source_table_oid: u32,
        source_id: &str,
        target_table_oid: u32,
        target_id: &str,
        max_depth: i32,
        governor: &crate::resource::ResourceGovernor,
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

        let workspace = whole_graph_workspace_bytes(self.node_store.node_count(), 80)?;
        let workspace_lease = governor
            .reserve_memory(crate::resource::ResourcePhase::QueryPaths, workspace)
            .map_err(crate::safety::resource_limit_error)?;
        let _overlay_workspace = governor
            .reserve_memory(
                crate::resource::ResourcePhase::QueryExpand,
                self.estimated_traversal_overlay_clone_bytes()?,
            )
            .map_err(crate::safety::resource_limit_error)?;

        let result = if !self.has_edge_overlay() {
            if let Some(neighbors) = self.layered_neighbors()? {
                path_finder::shortest_path_with_neighbors_governed(
                    &self.node_store,
                    &neighbors,
                    path_finder::UnweightedPathRequest {
                        source,
                        target,
                        max_depth,
                        has_unidirectional_edges: true,
                        edge_type_registry: &self.edge_type_registry,
                    },
                    governor,
                )
            } else {
                let neighbors = CsrNeighbors::new(&self.edge_store);
                path_finder::shortest_path_with_neighbors_governed(
                    &self.node_store,
                    &neighbors,
                    path_finder::UnweightedPathRequest {
                        source,
                        target,
                        max_depth,
                        has_unidirectional_edges: self.has_unidirectional_edges,
                        edge_type_registry: &self.edge_type_registry,
                    },
                    governor,
                )
            }
        } else if let Some(neighbors) = self.layered_neighbors()? {
            path_finder::shortest_path_with_neighbors_governed(
                &self.node_store,
                &neighbors,
                path_finder::UnweightedPathRequest {
                    source,
                    target,
                    max_depth,
                    has_unidirectional_edges: true,
                    edge_type_registry: &self.edge_type_registry,
                },
                governor,
            )
        } else {
            let (overlay_insert_edges, overlay_deleted_edges) =
                self.traversal_edge_overlay(TraversalDirection::Out);
            let neighbors = OverlayNeighbors::new(
                &self.edge_store,
                &overlay_insert_edges,
                &overlay_deleted_edges,
            );
            path_finder::shortest_path_with_neighbors_governed(
                &self.node_store,
                &neighbors,
                path_finder::UnweightedPathRequest {
                    source,
                    target,
                    max_depth,
                    has_unidirectional_edges: self.has_unidirectional_edges,
                    edge_type_registry: &self.edge_type_registry,
                },
                governor,
            )
        };

        let path = result?.unwrap_or_default();
        governor
            .consume_work(
                crate::resource::ResourcePhase::QueryPaths,
                crate::resource::WorkUnits::new(u64::try_from(path.len()).unwrap_or(u64::MAX)),
            )
            .map_err(crate::safety::resource_limit_error)?;
        workspace_lease.retain_until_governor_drop();
        Ok(path)
    }

    /// Find weighted shortest path between two nodes.
    #[allow(dead_code, reason = "compatibility entry point")]
    pub fn weighted_shortest_path(
        &self,
        source_table_oid: u32,
        source_id: &str,
        target_table_oid: u32,
        target_id: &str,
    ) -> GraphResult<Vec<WeightedPathStep>> {
        let governor = self.query_resource_governor()?;
        self.weighted_shortest_path_governed(
            source_table_oid,
            source_id,
            target_table_oid,
            target_id,
            &governor,
        )
    }

    /// Finds a weighted path within a caller-owned operation budget.
    pub(crate) fn weighted_shortest_path_governed(
        &self,
        source_table_oid: u32,
        source_id: &str,
        target_table_oid: u32,
        target_id: &str,
        governor: &crate::resource::ResourceGovernor,
    ) -> GraphResult<Vec<WeightedPathStep>> {
        if !self.built {
            return Err(GraphError::NotBuilt);
        }
        let node_workspace = whole_graph_workspace_bytes(self.node_store.node_count(), 64)?;
        let neighbor_workspace = whole_graph_workspace_bytes(self.edge_store.edge_count(), 24)?;
        let workspace = node_workspace
            .checked_add(neighbor_workspace)
            .ok_or_else(|| {
                GraphError::Internal("weighted path workspace estimate overflowed".to_string())
            })?;
        let workspace_lease = governor
            .reserve_memory(crate::resource::ResourcePhase::QueryPaths, workspace)
            .map_err(crate::safety::resource_limit_error)?;
        let _overlay_workspace = governor
            .reserve_memory(
                crate::resource::ResourcePhase::QueryExpand,
                self.estimated_traversal_overlay_clone_bytes()?,
            )
            .map_err(crate::safety::resource_limit_error)?;
        let layered_neighbors = self.layered_neighbors()?;
        if self.has_edge_overlay() && layered_neighbors.is_none() {
            return Err(GraphError::UnsupportedOperation {
                operation: "graph.weighted_shortest_path() with pending edge overlays"
                    .to_string(),
                reason: "pending edge overlays do not carry stable edge weights until graph.vacuum() or graph.maintenance() merges them".to_string(),
            });
        }
        if !self.edge_buffer.is_empty() && layered_neighbors.is_some() {
            return Err(GraphError::UnsupportedOperation {
                operation: "graph.weighted_shortest_path() with pending edge overlays"
                    .to_string(),
                reason: "pending committed edge_buffer overlays do not carry stable edge weights until graph.vacuum() or graph.maintenance() merges them".to_string(),
            });
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

        let path = if let Some(neighbors) = layered_neighbors {
            path_finder::weighted_shortest_path_with_neighbors_governed(
                &self.node_store,
                &neighbors,
                source,
                target,
                &self.edge_type_registry,
                governor,
            )
        } else {
            path_finder::weighted_shortest_path_with_neighbors_governed(
                &self.node_store,
                &self.edge_store,
                source,
                target,
                &self.edge_type_registry,
                governor,
            )
        };
        let path = path?.unwrap_or_default();
        governor
            .consume_work(
                crate::resource::ResourcePhase::QueryPaths,
                crate::resource::WorkUnits::new(u64::try_from(path.len()).unwrap_or(u64::MAX)),
            )
            .map_err(crate::safety::resource_limit_error)?;
        workspace_lease.retain_until_governor_drop();
        Ok(path)
    }

    /// Get engine status.
    pub fn status(&self) -> EngineStatus {
        let tx_delta_stats = tx_delta::stats();
        let edge_buffer_tombstones = self
            .edge_buffer
            .iter()
            .filter(|mutation| mutation.kind == MutationKind::Delete)
            .count();
        let tx_delta_tombstones = tx_delta_stats
            .deleted_nodes
            .saturating_add(tx_delta_stats.deleted_edges);
        let overlay_tombstones = edge_buffer_tombstones.saturating_add(tx_delta_tombstones);
        let edge_buffer_memory_bytes = self
            .edge_buffer
            .capacity()
            .saturating_mul(std::mem::size_of::<EdgeMutation>());
        let overlay_memory_bytes = tx_delta_stats
            .memory_bytes
            .saturating_add(edge_buffer_memory_bytes);
        let durable_overlay_memory_bytes = edge_buffer_memory_bytes;
        let compaction_threshold = crate::config::compaction_threshold();
        let compaction_recommended = self.needs_vacuum
            || self.edge_buffer.len() >= compaction_threshold
            || edge_buffer_tombstones >= compaction_threshold
            || durable_overlay_memory_bytes >= crate::config::max_overlay_memory_bytes();
        let (base_manifest_generation, base_manifest_sync_watermark) =
            self.base_projection_manifest_status();
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
            projection_mode: self.projection_mode.as_str().to_string(),
            build_resource_budget_bytes: self.build_resource_budget_bytes.min(i64::MAX as u64)
                as i64,
            build_resource_peak_bytes: self.build_resource_peak_bytes.min(i64::MAX as u64) as i64,
            build_resource_peak_phase: self
                .build_resource_peak_phase
                .map(crate::resource::ResourcePhase::as_str)
                .map(ToString::to_string),
            build_resource_pressure_events: self.build_resource_pressure_events.min(i64::MAX as u64)
                as i64,
            base_manifest_generation,
            base_manifest_sync_watermark,
            overlay_tombstone_count: overlay_tombstones.min(i32::MAX as usize) as i32,
            overlay_memory_bytes: overlay_memory_bytes.min(i64::MAX as usize) as i64,
            compaction_recommended,
            tx_delta_dirty: tx_delta_stats.dirty,
            tx_delta_added_nodes: tx_delta_stats.added_nodes.min(i32::MAX as usize) as i32,
            tx_delta_deleted_nodes: tx_delta_stats.deleted_nodes.min(i32::MAX as usize) as i32,
            tx_delta_added_edges: tx_delta_stats.added_edges.min(i32::MAX as usize) as i32,
            tx_delta_deleted_edges: tx_delta_stats.deleted_edges.min(i32::MAX as usize) as i32,
            tx_delta_memory_bytes: tx_delta_stats.memory_bytes.min(i64::MAX as usize) as i64,
        }
    }

    pub fn estimated_memory_used_mb(&self) -> f64 {
        self.estimated_memory_used_bytes() as f64 / 1_048_576.0
    }

    /// Return the conservative backend-private engine residency in bytes.
    pub(crate) fn estimated_memory_used_bytes(&self) -> usize {
        self.estimated_heap_bytes()
            .saturating_add(self.estimated_mmap_bytes())
            .max(self.estimated_logical_bytes())
    }

    pub fn memory_profile(&self, concurrent_backends: i32, memory_limit_mb: i32) -> MemoryProfile {
        let private_bytes = self
            .estimated_heap_bytes()
            .saturating_add(self.estimated_mmap_bytes());
        // The public columns remain for 1.x compatibility. Artifact snapshots
        // are backend-local now, so no mapped bytes are reported as shared.
        let shared_bytes = 0;
        let backend_count = concurrent_backends.max(1) as usize;
        let instance_private_bytes = private_bytes.saturating_mul(backend_count);
        let instance_total_bytes = instance_private_bytes.saturating_add(shared_bytes);

        MemoryProfile {
            active_backend_private_mb: bytes_to_mb(private_bytes),
            active_backend_shared_mb: bytes_to_mb(shared_bytes),
            active_backend_total_mb: bytes_to_mb(private_bytes.saturating_add(shared_bytes)),
            estimated_instance_private_mb: bytes_to_mb(instance_private_bytes),
            estimated_instance_shared_mb: bytes_to_mb(shared_bytes),
            estimated_instance_total_mb: bytes_to_mb(instance_total_bytes),
            memory_limit_mb,
            assumed_concurrent_backends: backend_count.min(i32::MAX as usize) as i32,
        }
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
        let projection_snapshot_bytes = self
            .projection_snapshot
            .as_ref()
            .map_or(0, LayeredSnapshot::estimated_heap_bytes);

        self.node_store.estimated_heap_bytes()
            + self.edge_store.estimated_heap_bytes()
            + self.reverse_edge_store.estimated_heap_bytes()
            + self.filter_index.estimated_heap_bytes()
            + self.relationship_identities.estimated_heap_bytes()
            + resolution_bytes
            + registry_bytes
            + edge_buffer_bytes
            + table_membership_bytes
            + tenant_bytes
            + tenanted_oid_bytes
            + projection_snapshot_bytes
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

    fn estimated_mmap_bytes(&self) -> usize {
        if let Some(mmap) = &self._mmap {
            return mmap.len();
        }

        let resolution_bytes = match &self.resolution_store {
            ResolutionStore::MmapBacked(state) => state.len,
            ResolutionStore::Builder(_) | ResolutionStore::Finalized(_) => 0,
        };

        self.node_store
            .estimated_mmap_bytes()
            .saturating_add(self.edge_store.estimated_mmap_bytes())
            .saturating_add(self.relationship_identities.estimated_mmap_bytes())
            .saturating_add(resolution_bytes)
    }

    /// Compute connected components.
    #[allow(dead_code, reason = "compatibility entry point")]
    pub fn connected_components(
        &self,
    ) -> GraphResult<crate::connected_components::ComponentResult> {
        let governor = self.analytics_resource_governor()?;
        self.connected_components_governed(&governor)
    }

    /// Computes connected components within a caller-owned analytics budget.
    pub(crate) fn connected_components_governed(
        &self,
        governor: &crate::resource::ResourceGovernor,
    ) -> GraphResult<crate::connected_components::ComponentResult> {
        if !self.built {
            return Err(GraphError::NotBuilt);
        }
        let workspace = whole_graph_workspace_bytes(self.node_store.node_count(), 40)?;
        let workspace_lease = governor
            .reserve_memory(
                crate::resource::ResourcePhase::AnalyticsWorkspace,
                workspace,
            )
            .map_err(crate::safety::resource_limit_error)?;
        let _overlay_workspace = governor
            .reserve_memory(
                crate::resource::ResourcePhase::AnalyticsWorkspace,
                self.estimated_traversal_overlay_clone_bytes()?,
            )
            .map_err(crate::safety::resource_limit_error)?;
        let result = if let Some(neighbors) = self.layered_neighbors()? {
            crate::connected_components::compute_components_with_neighbors_governed(
                &self.node_store,
                &neighbors,
                governor,
            )?
        } else if !self.has_edge_overlay() {
            let neighbors = CsrNeighbors::new(&self.edge_store);
            crate::connected_components::compute_components_with_neighbors_governed(
                &self.node_store,
                &neighbors,
                governor,
            )?
        } else {
            let (overlay_insert_edges, overlay_deleted_edges) =
                self.traversal_edge_overlay(TraversalDirection::Out);
            let neighbors = OverlayNeighbors::new(
                &self.edge_store,
                &overlay_insert_edges,
                &overlay_deleted_edges,
            );
            crate::connected_components::compute_components_with_neighbors_governed(
                &self.node_store,
                &neighbors,
                governor,
            )?
        };
        workspace_lease.retain_until_governor_drop();
        Ok(result)
    }

    pub(crate) fn query_resource_governor(&self) -> GraphResult<crate::resource::ResourceGovernor> {
        let resident = crate::resource::ByteCount::from_usize(self.estimated_memory_used_bytes())
            .ok_or_else(|| {
            GraphError::Internal("engine residency does not fit u64".to_string())
        })?;
        Ok(crate::resource::query_governor(resident))
    }

    pub(crate) fn analytics_resource_governor(
        &self,
    ) -> GraphResult<crate::resource::ResourceGovernor> {
        let resident = crate::resource::ByteCount::from_usize(self.estimated_memory_used_bytes())
            .ok_or_else(|| {
            GraphError::Internal("engine residency does not fit u64".to_string())
        })?;
        Ok(crate::resource::analytics_governor(resident))
    }
}

fn whole_graph_workspace_bytes(
    node_count: u32,
    bytes_per_node: u64,
) -> GraphResult<crate::resource::ByteCount> {
    u64::from(node_count)
        .checked_mul(bytes_per_node)
        .map(crate::resource::ByteCount::from_bytes)
        .ok_or_else(|| GraphError::Internal("query workspace estimate overflowed".to_string()))
}

fn bytes_to_mb(bytes: usize) -> f64 {
    bytes as f64 / 1_048_576.0
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
                schema_reversed: false,
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
    fn memory_profile_multiplies_backend_private_memory() {
        let mut engine = Engine::new();
        for i in 0..10u32 {
            engine.node_store.add_node(1, format!("n-{}", i));
        }
        engine.edge_store = crate::edge_store::EdgeStore::from_edges(
            10,
            vec![crate::edge_store::RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            }],
            false,
        );

        let one_backend = engine.memory_profile(1, 2048);
        let four_backends = engine.memory_profile(4, 2048);

        assert!(one_backend.active_backend_private_mb > 0.0);
        assert_eq!(one_backend.active_backend_shared_mb, 0.0);
        assert_eq!(four_backends.assumed_concurrent_backends, 4);
        assert!(
            four_backends.estimated_instance_private_mb
                >= one_backend.active_backend_private_mb * 4.0
        );
        assert_eq!(
            four_backends.estimated_instance_shared_mb,
            one_backend.active_backend_shared_mb
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
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Insert,
        });
        engine.edge_buffer.push(crate::engine::EdgeMutation {
            source: 1,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Delete,
        });

        assert_eq!(engine.edge_buffer.len(), 2);
        assert_eq!(engine.edge_buffer[0].kind, MutationKind::Insert);
        assert_eq!(engine.edge_buffer[1].kind, MutationKind::Delete);
    }

    #[test]
    fn committed_wildcard_delete_coexists_with_identified_insert() {
        let mut engine = Engine::new();
        engine.edge_buffer.push(EdgeMutation {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Delete,
        });
        engine.edge_buffer.push(EdgeMutation {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: Some(9),
            kind: MutationKind::Insert,
        });

        let (inserts, deletes) = engine.traversal_edge_overlay(TraversalDirection::Out);
        assert_eq!(inserts.get(&0), Some(&vec![(1, 1, false, Some(9))]));
        assert_eq!(deletes.get(&0), Some(&HashSet::from([(1, 1, false, None)])));
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
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 3,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 4,
                type_id: 2,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 4,
                target: 0,
                type_id: 2,
                weight: None,
                schema_reversed: false,
            },
        ];
        eng.edge_store = crate::edge_store::EdgeStore::from_edges(5, edges, false);
        eng.reverse_edge_store = eng.edge_store.reversed();
        eng.built = true;
        eng
    }

    fn install_edge_segment_manifest(
        engine: &mut Engine,
        test_name: &str,
        fill: impl FnOnce(&mut crate::projection::segment::DeltaSegment),
    ) -> crate::projection::test_fixtures::ProjectionArtifactDir {
        let dir = crate::projection::test_fixtures::ProjectionArtifactDir::new(test_name);
        let root = dir.path().to_path_buf();
        let segment_path = dir.segment_path(1, 0);
        let mut segment = crate::projection::segment::DeltaSegment::new(
            crate::projection::segment::SegmentKind::Edge,
            0,
            TraversalDirection::Out,
            0,
            engine.node_store.node_count(),
            1,
        )
        .expect("segment builds");
        fill(&mut segment);
        segment
            .write_to_path(&segment_path)
            .expect("segment writes");
        let bytes = std::fs::read(&segment_path).expect("segment reads for checksum");
        let relative = segment_path
            .strip_prefix(&root)
            .expect("segment stays inside root")
            .to_string_lossy()
            .to_string();
        let mut manifest = ProjectionManifest::base_only(
            1,
            "base.pggraph",
            "crc32:base",
            crate::persistence::graph_artifact_version(),
            segment.header.sync_watermark,
            1,
        );
        manifest
            .segments
            .push(crate::projection::manifest::ManifestSegmentRef {
                path: relative,
                checksum: format!("crc32:{:08x}", crc32fast::hash(&bytes)),
                level: segment.header.level,
                source_start: segment.header.source_start,
                source_end: segment.header.source_end,
                sync_watermark: segment.header.sync_watermark,
            });
        engine.set_projection_mode(crate::config::ProjectionMode::MutableOverlay);
        engine
            .install_projection_manifest(&manifest, root)
            .expect("projection manifest installs");
        dir
    }

    fn write_projection_manifest(
        test_name: &str,
        segments: Vec<crate::projection::segment::DeltaSegment>,
    ) -> (
        crate::projection::test_fixtures::ProjectionArtifactDir,
        ProjectionManifest,
    ) {
        let dir = crate::projection::test_fixtures::ProjectionArtifactDir::new(test_name);
        let mut manifest = ProjectionManifest::base_only(
            1,
            "base.pggraph",
            "crc32:base",
            crate::persistence::graph_artifact_version(),
            segments
                .iter()
                .map(|segment| segment.header.sync_watermark)
                .max()
                .unwrap_or(0),
            1,
        );
        for (segment_id, segment) in segments.into_iter().enumerate() {
            let segment_path = dir.segment_path(1, segment_id as u32);
            segment
                .write_to_path(&segment_path)
                .expect("projection segment writes");
            let bytes = std::fs::read(&segment_path).expect("projection segment reads");
            manifest
                .segments
                .push(crate::projection::manifest::ManifestSegmentRef {
                    path: segment_path
                        .strip_prefix(dir.path())
                        .expect("segment stays inside projection root")
                        .to_string_lossy()
                        .to_string(),
                    checksum: format!("crc32:{:08x}", crc32fast::hash(&bytes)),
                    level: segment.header.level,
                    source_start: segment.header.source_start,
                    source_end: segment.header.source_end,
                    sync_watermark: segment.header.sync_watermark,
                });
        }
        (dir, manifest)
    }

    fn write_raw_projection_manifest(
        test_name: &str,
        bytes: &[u8],
        segment: &crate::projection::segment::DeltaSegment,
    ) -> (
        crate::projection::test_fixtures::ProjectionArtifactDir,
        ProjectionManifest,
    ) {
        let dir = crate::projection::test_fixtures::ProjectionArtifactDir::new(test_name);
        let segment_path = dir.segment_path(1, 0);
        std::fs::write(&segment_path, bytes).expect("raw projection segment writes");
        let mut manifest = ProjectionManifest::base_only(
            1,
            "base.pggraph",
            "crc32:base",
            crate::persistence::graph_artifact_version(),
            segment.header.sync_watermark,
            1,
        );
        manifest
            .segments
            .push(crate::projection::manifest::ManifestSegmentRef {
                path: segment_path
                    .strip_prefix(dir.path())
                    .expect("segment stays inside projection root")
                    .to_string_lossy()
                    .to_string(),
                checksum: format!("crc32:{:08x}", crc32fast::hash(bytes)),
                level: segment.header.level,
                source_start: segment.header.source_start,
                source_end: segment.header.source_end,
                sync_watermark: segment.header.sync_watermark,
            });
        (dir, manifest)
    }

    #[test]
    fn projection_manifest_installs_new_exact_node_identity_and_tenant() {
        use crate::projection::segment::{
            DeltaSegment, SegmentKind, SegmentNodeState, SegmentResolution, SegmentTenant,
        };

        let mut engine = Engine::new();
        engine.node_store.add_node(42, "base".to_string());
        engine.resolution_insert(42, "base", 0);
        engine.rebuild_table_membership();
        let mut segment = DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 1, 2, 1)
            .expect("node segment builds");
        segment.node_states.push(SegmentNodeState {
            node_idx: 1,
            active: true,
        });
        segment.resolutions.push(SegmentResolution {
            table_oid: 42,
            pk_hash: ResolutionIndexBuilder::hash_pk("新-node"),
            primary_key: Some("新-node".to_string()),
            node_idx: 1,
            tombstone: false,
        });
        segment.tenants.push(SegmentTenant {
            node_idx: 1,
            tenant_hash: xxhash_rust::xxh3::xxh3_64("tenant-ß".as_bytes()),
            tenant: Some("tenant-ß".to_string()),
            tombstone: false,
        });
        let (dir, manifest) =
            write_projection_manifest("engine_installs_exact_node_identity", vec![segment]);

        engine
            .install_projection_manifest(&manifest, dir.path())
            .expect("exact node projection installs");

        assert_eq!(engine.node_store.node_count(), 2);
        assert_eq!(engine.resolve(42, "新-node"), Some(1));
        assert!(engine.node_store.is_active(1));
        assert!(engine
            .tenant_membership
            .get("tenant-ß")
            .is_some_and(|nodes| nodes.contains(1)));
    }

    #[test]
    fn projection_manifest_rejects_node_gap_without_mutating_engine() {
        use crate::projection::segment::{DeltaSegment, SegmentKind, SegmentResolution};

        let mut engine = Engine::new();
        engine.node_store.add_node(42, "base".to_string());
        engine.resolution_insert(42, "base", 0);
        let mut segment = DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 3, 4, 1)
            .expect("node segment builds");
        segment.resolutions.push(SegmentResolution {
            table_oid: 42,
            pk_hash: ResolutionIndexBuilder::hash_pk("gap"),
            primary_key: Some("gap".to_string()),
            node_idx: 3,
            tombstone: false,
        });
        let (dir, manifest) =
            write_projection_manifest("engine_rejects_projection_node_gap", vec![segment]);

        let error = engine
            .install_projection_manifest(&manifest, dir.path())
            .expect_err("node allocation gap is rejected");

        assert!(matches!(error, GraphError::CorruptFile { .. }));
        assert_eq!(engine.node_store.node_count(), 1);
        assert_eq!(engine.resolve(42, "base"), Some(0));
        assert_eq!(engine.resolve(42, "gap"), None);
        assert!(engine.projection_manifest.is_none());
    }

    #[test]
    fn projection_manifest_delete_then_reinsert_uses_new_node_slot() {
        use crate::projection::segment::{
            DeltaSegment, SegmentKind, SegmentNodeState, SegmentResolution,
        };

        let mut engine = Engine::new();
        engine.node_store.add_node(42, "same".to_string());
        engine.resolution_insert(42, "same", 0);
        engine.rebuild_table_membership();
        let mut deletion =
            DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 1, 1)
                .expect("delete segment builds");
        deletion.node_states.push(SegmentNodeState {
            node_idx: 0,
            active: false,
        });
        deletion.resolutions.push(SegmentResolution {
            table_oid: 42,
            pk_hash: ResolutionIndexBuilder::hash_pk("same"),
            primary_key: Some("same".to_string()),
            node_idx: 0,
            tombstone: true,
        });
        let mut insertion =
            DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 1, 2, 2)
                .expect("insert segment builds");
        insertion.node_states.push(SegmentNodeState {
            node_idx: 1,
            active: true,
        });
        insertion.resolutions.push(SegmentResolution {
            table_oid: 42,
            pk_hash: ResolutionIndexBuilder::hash_pk("same"),
            primary_key: Some("same".to_string()),
            node_idx: 1,
            tombstone: false,
        });
        let (dir, manifest) = write_projection_manifest(
            "engine_projection_delete_reinsert",
            vec![deletion, insertion],
        );

        engine
            .install_projection_manifest(&manifest, dir.path())
            .expect("delete and reinsert projection installs");

        assert!(!engine.node_store.is_active(0));
        assert!(engine.node_store.is_active(1));
        assert_eq!(engine.resolve(42, "same"), Some(1));
    }

    #[test]
    fn projection_manifest_preserves_transient_node_slot_as_inactive() {
        use crate::projection::segment::{
            DeltaSegment, SegmentKind, SegmentNodeState, SegmentResolution,
        };

        let mut engine = Engine::new();
        let mut segment = DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 1, 2)
            .expect("node segment builds");
        segment.node_states.push(SegmentNodeState {
            node_idx: 0,
            active: false,
        });
        segment.resolutions.push(SegmentResolution {
            table_oid: 42,
            pk_hash: ResolutionIndexBuilder::hash_pk("transient"),
            primary_key: Some("transient".to_string()),
            node_idx: 0,
            tombstone: true,
        });
        let (dir, manifest) =
            write_projection_manifest("engine_projection_transient_node", vec![segment]);

        engine
            .install_projection_manifest(&manifest, dir.path())
            .expect("transient allocation installs as a durable tombstone");

        assert_eq!(engine.node_store.node_count(), 1);
        assert!(!engine.node_store.is_active(0));
        assert_eq!(engine.resolve(42, "transient"), None);
    }

    #[test]
    fn projection_manifest_accepts_v5_resolution_for_existing_base_node() {
        use crate::projection::segment::{DeltaSegment, SegmentKind, SegmentResolution};

        let mut engine = Engine::new();
        engine.node_store.add_node(42, "legacy".to_string());
        engine.resolution_insert(42, "legacy", 0);
        let mut segment = DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 1, 1)
            .expect("legacy segment builds");
        segment.resolutions.push(SegmentResolution {
            table_oid: 42,
            pk_hash: ResolutionIndexBuilder::hash_pk("legacy"),
            primary_key: Some("legacy".to_string()),
            node_idx: 0,
            tombstone: false,
        });
        let bytes = crate::projection::segment::encode_version_5_segment_for_test(&segment);
        let (dir, manifest) =
            write_raw_projection_manifest("engine_projection_v5_resolution", &bytes, &segment);

        engine
            .install_projection_manifest(&manifest, dir.path())
            .expect("v5 base resolution installs");

        assert_eq!(engine.resolve(42, "legacy"), Some(0));
    }

    #[test]
    fn projection_manifest_rejects_v5_tenant_without_exact_identity() {
        use crate::projection::segment::{DeltaSegment, SegmentKind, SegmentTenant};

        let mut engine = Engine::new();
        engine.node_store.add_node(42, "legacy".to_string());
        engine.resolution_insert(42, "legacy", 0);
        let mut segment = DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 1, 1)
            .expect("legacy tenant segment builds");
        segment.tenants.push(SegmentTenant {
            node_idx: 0,
            tenant_hash: xxhash_rust::xxh3::xxh3_64(b"legacy-tenant"),
            tenant: Some("legacy-tenant".to_string()),
            tombstone: false,
        });
        let bytes = crate::projection::segment::encode_version_5_segment_for_test(&segment);
        let (dir, manifest) =
            write_raw_projection_manifest("engine_projection_v5_tenant", &bytes, &segment);

        let error = engine
            .install_projection_manifest(&manifest, dir.path())
            .expect_err("v5 tenant without exact identity is rejected");

        assert!(matches!(error, GraphError::IncompatibleVersion(_)));
        assert!(engine.tenant_membership.is_empty());
        assert!(engine.projection_manifest.is_none());
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
                schema_reversed: false,
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
            schema_reversed: false,
            relationship_id: None,
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
    fn traverse_uses_transaction_delta_edge_insert() {
        let eng = build_test_engine();
        tx_delta::clear_for_test();
        tx_delta::record_added_edge(
            4,
            tx_delta::DeltaEdge {
                target: 3,
                type_id: 1,
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record tx edge insert");

        let results = eng
            .traverse(
                100,
                "E",
                1,
                100000,
                100000,
                Some(vec!["linked".to_string()]),
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();

        assert!(results.iter().any(|row| row.node_id == "D"));
        tx_delta::clear_for_test();
    }

    #[test]
    fn traverse_hides_transaction_delta_edge_delete() {
        let eng = build_test_engine();
        tx_delta::clear_for_test();
        tx_delta::record_deleted_edge(1, 2, 1).expect("record tx edge delete");

        let results = eng
            .traverse(
                100,
                "A",
                10,
                100000,
                100000,
                Some(vec!["linked".to_string()]),
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();

        assert!(!results.iter().any(|row| row.node_id == "C"));
        assert!(!results.iter().any(|row| row.node_id == "D"));
        tx_delta::clear_for_test();
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

    #[test]
    fn shortest_path_uses_pending_edge_overlay() {
        let mut eng = build_test_engine();
        eng.has_unidirectional_edges = true;
        eng.edge_buffer.push(EdgeMutation {
            source: 4,
            target: 3,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Insert,
        });

        let steps = eng.shortest_path(100, "A", 100, "D", 10).unwrap();

        assert_eq!(
            steps
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "E", "D"]
        );
    }

    #[test]
    fn shortest_path_hides_deleted_overlay_edges() {
        let mut eng = build_test_engine();
        eng.has_unidirectional_edges = true;
        eng.edge_buffer.push(EdgeMutation {
            source: 1,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Delete,
        });

        let steps = eng.shortest_path(100, "A", 100, "D", 10).unwrap();

        assert!(steps.is_empty());
    }

    #[test]
    fn shortest_path_uses_transaction_delta_edge_insert() {
        let mut eng = build_test_engine();
        eng.has_unidirectional_edges = true;
        tx_delta::clear_for_test();
        tx_delta::record_added_edge(
            4,
            tx_delta::DeltaEdge {
                target: 3,
                type_id: 1,
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record tx edge insert");

        let steps = eng.shortest_path(100, "A", 100, "D", 10).unwrap();

        assert_eq!(
            steps
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "E", "D"]
        );
        tx_delta::clear_for_test();
    }

    #[test]
    fn shortest_path_hides_transaction_delta_edge_delete() {
        let mut eng = build_test_engine();
        eng.has_unidirectional_edges = true;
        tx_delta::clear_for_test();
        tx_delta::record_deleted_edge(1, 2, 1).expect("record tx edge delete");

        let steps = eng.shortest_path(100, "A", 100, "D", 10).unwrap();

        assert!(steps.is_empty());
        tx_delta::clear_for_test();
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

    #[test]
    fn weighted_shortest_path_rejects_pending_edge_overlay() {
        let mut eng = build_test_engine();
        eng.edge_store = crate::edge_store::EdgeStore::from_edges(
            5,
            vec![crate::edge_store::RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: Some(1),
                schema_reversed: false,
            }],
            true,
        );
        eng.edge_buffer.push(EdgeMutation {
            source: 1,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Insert,
        });

        let result = eng.weighted_shortest_path(100, "A", 100, "C");

        assert!(matches!(
            result,
            Err(GraphError::UnsupportedOperation { .. })
        ));
    }

    #[test]
    fn weighted_shortest_path_rejects_transaction_delta_edge_overlay() {
        let mut eng = build_test_engine();
        eng.edge_store = crate::edge_store::EdgeStore::from_edges(
            5,
            vec![crate::edge_store::RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: Some(1),
                schema_reversed: false,
            }],
            true,
        );
        tx_delta::clear_for_test();
        tx_delta::record_added_edge(
            1,
            tx_delta::DeltaEdge {
                target: 2,
                type_id: 1,
                weight: Some(1),
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record tx edge insert");

        let result = eng.weighted_shortest_path(100, "A", 100, "C");

        assert!(matches!(
            result,
            Err(GraphError::UnsupportedOperation { .. })
        ));
        tx_delta::clear_for_test();
    }

    #[test]
    fn shortest_path_uses_layered_manifest_snapshot() {
        let mut eng = build_test_engine();
        let dir = install_edge_segment_manifest(&mut eng, "shortest_path_layered", |segment| {
            segment
                .edge_inserts
                .push(crate::projection::segment::SegmentEdge {
                    source: 0,
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                });
        });

        let segment_path = dir.path().join(
            &eng.projection_manifest_full
                .as_ref()
                .expect("manifest is installed")
                .segments[0]
                .path,
        );
        std::fs::remove_file(segment_path).expect("installed snapshot releases segment file");

        let steps = eng.shortest_path(100, "A", 100, "D", 10).unwrap();
        let repeated = eng.shortest_path(100, "A", 100, "D", 10).unwrap();

        assert_eq!(
            steps
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "D"]
        );
        assert_eq!(
            steps
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            repeated
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn csr_readonly_base_only_manifest_bypasses_segment_lookup() {
        let mut eng = build_test_engine();
        let _dir = install_edge_segment_manifest(&mut eng, "csr_readonly_bypass", |segment| {
            segment
                .edge_inserts
                .push(crate::projection::segment::SegmentEdge {
                    source: 0,
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                });
        });
        eng.set_projection_mode(crate::config::ProjectionMode::CsrReadonly);

        let steps = eng.shortest_path(100, "A", 100, "D", 10).unwrap();

        assert_eq!(
            steps
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B", "C", "D"]
        );
    }

    #[test]
    fn traversal_uses_layered_manifest_snapshot() {
        let mut eng = build_test_engine();
        let _dir = install_edge_segment_manifest(&mut eng, "traversal_layered", |segment| {
            segment
                .edge_inserts
                .push(crate::projection::segment::SegmentEdge {
                    source: 0,
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                });
        });

        let rows = eng
            .traverse(
                100,
                "A",
                1,
                100,
                100,
                None,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();

        assert!(rows.iter().any(|row| row.node_id == "D"));
    }

    #[test]
    fn traversal_layered_manifest_preserves_pending_edge_buffer_overlay() {
        let mut eng = build_test_engine();
        let _dir = install_edge_segment_manifest(&mut eng, "traversal_layered_edge_buffer", |_| {});
        eng.edge_buffer.push(EdgeMutation {
            source: 4,
            target: 3,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Insert,
        });

        let rows = eng
            .traverse(
                100,
                "E",
                1,
                100,
                100,
                Some(vec!["linked".to_string()]),
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();

        assert!(rows.iter().any(|row| row.node_id == "D"));
    }

    #[test]
    fn traversal_in_direction_uses_layered_manifest_snapshot() {
        let mut eng = build_test_engine();
        let _dir = install_edge_segment_manifest(&mut eng, "traversal_in_layered", |segment| {
            segment
                .edge_inserts
                .push(crate::projection::segment::SegmentEdge {
                    source: 0,
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                });
        });

        let rows = eng
            .traverse(
                100,
                "D",
                1,
                100,
                100,
                None,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::In,
            )
            .unwrap();

        assert!(rows.iter().any(|row| row.node_id == "A"));
    }

    #[test]
    fn traversal_in_direction_uses_layered_base_reverse_csr() {
        let mut eng = build_test_engine();
        let _dir = install_edge_segment_manifest(&mut eng, "traversal_in_layered_base", |_| {});

        let rows = eng
            .traverse(
                100,
                "D",
                1,
                100,
                100,
                Some(vec!["linked".to_string()]),
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::In,
            )
            .unwrap();

        assert!(rows.iter().any(|row| row.node_id == "C"));
    }

    #[test]
    fn shortest_path_layered_manifest_preserves_pending_edge_buffer_overlay() {
        let mut eng = build_test_engine();
        let _dir =
            install_edge_segment_manifest(&mut eng, "shortest_path_layered_edge_buffer", |_| {});
        eng.has_unidirectional_edges = true;
        eng.edge_buffer.push(EdgeMutation {
            source: 4,
            target: 3,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Insert,
        });

        let steps = eng.shortest_path(100, "A", 100, "D", 10).unwrap();

        assert_eq!(
            steps
                .iter()
                .map(|step| step.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "E", "D"]
        );
    }

    #[test]
    fn weighted_shortest_path_uses_layered_manifest_snapshot() {
        let mut eng = build_test_engine();
        eng.edge_store = crate::edge_store::EdgeStore::from_edges(
            5,
            vec![crate::edge_store::RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: Some(10),
                schema_reversed: false,
            }],
            true,
        );
        eng.reverse_edge_store = eng.edge_store.reversed();
        let _dir = install_edge_segment_manifest(&mut eng, "weighted_layered", |segment| {
            segment
                .edge_inserts
                .push(crate::projection::segment::SegmentEdge {
                    source: 0,
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                });
            segment
                .edge_weights
                .push(crate::projection::segment::SegmentEdgeWeight {
                    source: 0,
                    target: 3,
                    type_id: 1,
                    relationship_id: None,
                    weight: 2,
                    schema_reversed: false,
                });
        });

        let steps = eng.weighted_shortest_path(100, "A", 100, "D").unwrap();

        assert_eq!(
            steps
                .iter()
                .map(|step| (step.node_id.as_str(), step.step_cost))
                .collect::<Vec<_>>(),
            vec![("A", 0), ("D", 2)]
        );
    }

    #[test]
    fn components_use_layered_manifest_snapshot() {
        let mut eng = Engine::new();
        for (idx, pk) in ["A", "B", "C", "D"].iter().enumerate() {
            eng.node_store.add_node(100, pk.to_string());
            eng.resolution_insert(100, pk, idx as u32);
        }
        eng.register_edge_type("linked").unwrap();
        eng.edge_store = crate::edge_store::EdgeStore::from_edges(
            4,
            vec![crate::edge_store::RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            }],
            false,
        );
        eng.reverse_edge_store = eng.edge_store.reversed();
        eng.built = true;
        let _dir = install_edge_segment_manifest(&mut eng, "components_layered", |segment| {
            segment
                .edge_inserts
                .push(crate::projection::segment::SegmentEdge {
                    source: 2,
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                });
        });

        let result = eng.connected_components().unwrap();

        assert_eq!(result.num_components, 2);
        assert_eq!(result.largest_component_size, 2);
    }

    #[test]
    fn components_layered_manifest_preserves_pending_edge_buffer_overlay() {
        let mut eng = Engine::new();
        for (idx, pk) in ["A", "B", "C", "D"].iter().enumerate() {
            eng.node_store.add_node(100, pk.to_string());
            eng.resolution_insert(100, pk, idx as u32);
        }
        eng.register_edge_type("linked").unwrap();
        eng.edge_store = crate::edge_store::EdgeStore::from_edges(
            4,
            vec![crate::edge_store::RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            }],
            false,
        );
        eng.reverse_edge_store = eng.edge_store.reversed();
        eng.built = true;
        let _dir =
            install_edge_segment_manifest(&mut eng, "components_layered_edge_buffer", |_| {});
        eng.edge_buffer.push(EdgeMutation {
            source: 2,
            target: 3,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            kind: MutationKind::Insert,
        });

        let result = eng.connected_components().unwrap();

        assert_eq!(result.num_components, 2);
        assert_eq!(result.largest_component_size, 2);
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

    #[test]
    fn connected_components_honor_pending_edge_deletes() {
        let mut eng = build_test_engine();
        for (source, target) in [(1, 2), (2, 1)] {
            eng.edge_buffer.push(EdgeMutation {
                source,
                target,
                type_id: 1,
                schema_reversed: false,
                relationship_id: None,
                kind: MutationKind::Delete,
            });
        }

        let result = eng.connected_components().unwrap();

        assert_eq!(result.num_components, 2);
    }

    #[test]
    fn connected_components_honor_transaction_delta_edge_deletes() {
        let eng = build_test_engine();
        tx_delta::clear_for_test();
        for (source, target) in [(1, 2), (2, 1)] {
            tx_delta::record_deleted_edge(source, target, 1).expect("record tx edge delete");
        }

        let result = eng.connected_components().unwrap();

        assert_eq!(result.num_components, 2);
        tx_delta::clear_for_test();
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
                    schema_reversed: false,
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
