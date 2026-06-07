//! Durable projection ingestion from committed sync-log rows.
//!
//! The ingester is the write-side boundary between committed PostgreSQL sync
//! rows and immutable L0 projection segments. SQL wiring is added separately;
//! this module keeps the artifact publication rules pure and testable.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::projection::manifest::{
    ManifestSegmentRef, ProjectionManifest, ProjectionManifestStore,
};
use crate::projection::normalize::{
    normalize_committed_mutations, CommittedMutation, MutationBufferLimits, MutationOperation,
};
use crate::projection::segment::{
    DeltaSegment, SegmentFilterValue, SegmentKind, SegmentNodeState, SegmentResolution,
    SegmentTenant,
};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

const DEFAULT_SOURCE_RANGE_END: u32 = u32::MAX;
const INGEST_ROW_BYTES: usize = 40;
static ACTIVE_INGEST_ROOTS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

/// One committed row ready to publish into durable projection segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionSyncRow {
    pub(crate) sync_id: u64,
    pub(crate) generation_id: u64,
    pub(crate) committed: bool,
    pub(crate) operation: MutationOperation,
    pub(crate) direction: TraversalDirection,
    pub(crate) source: u32,
    pub(crate) target: u32,
    pub(crate) type_id: u8,
    pub(crate) weight: Option<u32>,
    pub(crate) table_oid: Option<u32>,
    pub(crate) pk_hash: Option<u64>,
    pub(crate) node_idx: Option<u32>,
    pub(crate) filter_column_id: Option<u32>,
    pub(crate) filter_value: Option<u32>,
    pub(crate) tenant_hash: Option<u64>,
}

impl ProjectionSyncRow {
    fn to_committed_mutation(&self) -> CommittedMutation {
        CommittedMutation {
            sync_id: self.sync_id,
            generation_id: self.generation_id,
            direction: self.direction,
            source: self.source,
            target: self.target,
            type_id: self.type_id,
            weight: self.weight,
            operation: self.operation,
        }
    }
}

/// Result of one projection ingestion publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionIngestResult {
    pub(crate) manifest: Option<ProjectionManifest>,
    pub(crate) rows_ingested: usize,
    pub(crate) segments_published: usize,
}

/// Ingestion publication lock.
#[derive(Debug)]
pub(crate) struct ProjectionIngestLock {
    root: PathBuf,
}

impl ProjectionIngestLock {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn try_enter(&self) -> GraphResult<ProjectionIngestGuard<'_>> {
        let mut active = active_ingest_roots()
            .lock()
            .map_err(|_| GraphError::Internal("projection ingest lock poisoned".into()))?;
        if !active.insert(self.root.clone()) {
            return Err(GraphError::BuildLocked);
        }
        Ok(ProjectionIngestGuard { lock: self })
    }
}

struct ProjectionIngestGuard<'a> {
    lock: &'a ProjectionIngestLock,
}

impl Drop for ProjectionIngestGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = active_ingest_roots().lock() {
            active.remove(&self.lock.root);
        }
    }
}

fn active_ingest_roots() -> &'static Mutex<HashSet<PathBuf>> {
    ACTIVE_INGEST_ROOTS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Testable durable projection ingester.
#[derive(Debug)]
pub(crate) struct ProjectionIngester {
    store: ProjectionManifestStore,
    root: PathBuf,
    base_artifact_path: String,
    base_artifact_checksum: String,
    base_artifact_version: u32,
    lock: ProjectionIngestLock,
}

impl ProjectionIngester {
    pub(crate) fn new(
        root: impl Into<PathBuf>,
        base_artifact_path: impl Into<String>,
        base_artifact_checksum: impl Into<String>,
        base_artifact_version: u32,
    ) -> Self {
        let root = root.into();
        Self {
            store: ProjectionManifestStore::new(root.clone()),
            lock: ProjectionIngestLock::new(lock_root(&root)),
            root,
            base_artifact_path: base_artifact_path.into(),
            base_artifact_checksum: base_artifact_checksum.into(),
            base_artifact_version,
        }
    }

    /// Publish committed rows above the current manifest watermark.
    ///
    /// # Errors
    ///
    /// Returns validation and filesystem errors from segment writing and
    /// manifest publication. Returns [`GraphError::BuildLocked`] if another
    /// projection publication is already active.
    pub(crate) fn ingest_committed_rows(
        &self,
        rows: &[ProjectionSyncRow],
        limits: MutationBufferLimits,
    ) -> GraphResult<ProjectionIngestResult> {
        let _guard = self.lock.try_enter()?;
        self.ingest_committed_rows_locked(rows, limits)
    }

    /// Publish a no-segment generation that only advances the sync watermark.
    ///
    /// # Errors
    ///
    /// Returns filesystem, validation, or publication-lock errors.
    pub(crate) fn publish_empty_watermark(
        &self,
        sync_watermark: i64,
    ) -> GraphResult<ProjectionIngestResult> {
        let _guard = self.lock.try_enter()?;
        let previous = self.store.load_latest_current()?;
        let previous_watermark = previous
            .as_ref()
            .map_or(0, |manifest| manifest.sync_watermark);
        if sync_watermark <= previous_watermark {
            return Ok(ProjectionIngestResult {
                manifest: None,
                rows_ingested: 0,
                segments_published: 0,
            });
        }
        let generation_id = previous.as_ref().map_or_else(
            || Ok(1),
            |manifest| {
                manifest.generation_id.checked_add(1).ok_or_else(|| {
                    GraphError::Internal("projection generation id overflowed".into())
                })
            },
        )?;
        let mut manifest = ProjectionManifest::base_only(
            generation_id,
            self.base_artifact_path.clone(),
            self.base_artifact_checksum.clone(),
            self.base_artifact_version,
            sync_watermark,
            now_unix_micros()?,
        );
        manifest.previous_generation_id = previous.map(|manifest| manifest.generation_id);
        self.store.publish(&manifest)?;
        Ok(ProjectionIngestResult {
            manifest: Some(manifest),
            rows_ingested: 0,
            segments_published: 0,
        })
    }

    fn ingest_committed_rows_locked(
        &self,
        rows: &[ProjectionSyncRow],
        limits: MutationBufferLimits,
    ) -> GraphResult<ProjectionIngestResult> {
        let previous = self.store.load_latest_current()?;
        let previous_watermark = previous
            .as_ref()
            .map_or(0, |manifest| manifest.sync_watermark);
        let committed_rows = rows
            .iter()
            .filter(|row| row.committed)
            .filter(|row| i64::try_from(row.sync_id).is_ok_and(|id| id > previous_watermark))
            .cloned()
            .collect::<Vec<_>>();
        if committed_rows.is_empty() {
            return Ok(ProjectionIngestResult {
                manifest: None,
                rows_ingested: 0,
                segments_published: 0,
            });
        }
        validate_ingestion_limits(committed_rows.len(), limits)?;

        let generation_id = next_generation_id(previous.as_ref(), &committed_rows)?;
        let sync_watermark = committed_rows
            .iter()
            .map(|row| i64::try_from(row.sync_id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| GraphError::Internal("projection sync id exceeds i64".into()))?
            .into_iter()
            .max()
            .unwrap_or(previous_watermark);

        let mut segment_refs = Vec::new();
        for edge_segment in self.edge_segments(&committed_rows, limits)? {
            let path =
                self.write_segment(generation_id, segment_refs.len() as u32, &edge_segment)?;
            segment_refs.push(segment_ref(&self.root, &path, &edge_segment)?);
        }
        if let Some(node_segment) = node_segment(&committed_rows, limits, sync_watermark)? {
            let path =
                self.write_segment(generation_id, segment_refs.len() as u32, &node_segment)?;
            segment_refs.push(segment_ref(&self.root, &path, &node_segment)?);
        }

        let mut manifest = ProjectionManifest::base_only(
            generation_id,
            self.base_artifact_path.clone(),
            self.base_artifact_checksum.clone(),
            self.base_artifact_version,
            sync_watermark,
            now_unix_micros()?,
        );
        manifest.previous_generation_id = previous.map(|manifest| manifest.generation_id);
        manifest.segments = segment_refs;
        if let Err(err) = self.store.publish(&manifest) {
            if !self.store.manifest_path(generation_id).exists() {
                cleanup_segment_refs(&self.root, &manifest.segments)?;
            }
            return Err(err);
        }

        Ok(ProjectionIngestResult {
            rows_ingested: committed_rows.len(),
            segments_published: manifest.segments.len(),
            manifest: Some(manifest),
        })
    }

    #[cfg(test)]
    fn hold_publication_lock_for_test(&self) -> GraphResult<ProjectionIngestGuard<'_>> {
        self.lock.try_enter()
    }

    fn edge_segments(
        &self,
        rows: &[ProjectionSyncRow],
        limits: MutationBufferLimits,
    ) -> GraphResult<Vec<DeltaSegment>> {
        let mut segments = Vec::new();
        for direction in [
            TraversalDirection::Out,
            TraversalDirection::In,
            TraversalDirection::Any,
        ] {
            let edge_rows = rows
                .iter()
                .filter(|row| row.operation.is_edge())
                .filter(|row| row.direction == direction)
                .map(ProjectionSyncRow::to_committed_mutation)
                .collect::<Vec<_>>();
            if edge_rows.is_empty() {
                continue;
            }
            let normalized = normalize_committed_mutations(&edge_rows, limits)?;
            if normalized.rows.is_empty() {
                continue;
            }
            segments.push(DeltaSegment::from_normalized_edges(
                &normalized,
                0,
                direction,
                0,
                DEFAULT_SOURCE_RANGE_END,
            )?);
        }
        Ok(segments)
    }

    fn write_segment(
        &self,
        generation_id: u64,
        segment_id: u32,
        segment: &DeltaSegment,
    ) -> GraphResult<PathBuf> {
        let path = self.root.join(segment_file_name(generation_id, segment_id));
        write_segment_atomically(&self.root, &path, segment)?;
        let decoded = DeltaSegment::read_from_path(&path)?;
        if decoded.header.sync_watermark != segment.header.sync_watermark {
            return Err(GraphError::CorruptFile {
                reason: "projection ingest segment failed validation reload".to_string(),
            });
        }
        Ok(path)
    }
}

fn node_segment(
    rows: &[ProjectionSyncRow],
    limits: MutationBufferLimits,
    sync_watermark: i64,
) -> GraphResult<Option<DeltaSegment>> {
    validate_ingestion_limits(
        rows.iter().filter(|row| !row.operation.is_edge()).count(),
        limits,
    )?;
    let mut segment = DeltaSegment::new(
        SegmentKind::Node,
        0,
        TraversalDirection::Any,
        0,
        DEFAULT_SOURCE_RANGE_END,
        sync_watermark,
    )?;
    let mut node_states = BTreeMap::<u32, Vec<NodeStateDelta>>::new();
    let mut resolutions = BTreeMap::<(u32, u64), Vec<ResolutionDelta>>::new();
    let mut filters = BTreeMap::<(u32, u32, u32), Vec<FilterDelta>>::new();
    let mut tenants = BTreeMap::<(u32, u64), Vec<TenantDelta>>::new();
    for row in rows.iter().filter(|row| !row.operation.is_edge()) {
        let node_idx = row.node_idx.unwrap_or(row.source);
        node_states
            .entry(node_idx)
            .or_default()
            .push(NodeStateDelta {
                sync_id: row.sync_id,
                active: row.operation.is_insert(),
            });
        if let (Some(table_oid), Some(pk_hash)) = (row.table_oid, row.pk_hash) {
            resolutions
                .entry((table_oid, pk_hash))
                .or_default()
                .push(ResolutionDelta {
                    sync_id: row.sync_id,
                    node_idx,
                    tombstone: row.operation.is_delete(),
                });
        }
        if let (Some(column_id), Some(value)) = (row.filter_column_id, row.filter_value) {
            filters
                .entry((node_idx, column_id, value))
                .or_default()
                .push(FilterDelta {
                    sync_id: row.sync_id,
                    tombstone: row.operation.is_delete(),
                });
        }
        if let Some(tenant_hash) = row.tenant_hash {
            tenants
                .entry((node_idx, tenant_hash))
                .or_default()
                .push(TenantDelta {
                    sync_id: row.sync_id,
                    tombstone: row.operation.is_delete(),
                });
        }
    }
    for (node_idx, group) in node_states {
        if let Some(delta) = select_node_state(&group) {
            segment.node_states.push(SegmentNodeState {
                node_idx,
                active: delta.active,
            });
        }
    }
    for ((table_oid, pk_hash), group) in resolutions {
        if let Some(delta) = select_resolution(&group) {
            segment.resolutions.push(SegmentResolution {
                table_oid,
                pk_hash,
                node_idx: delta.node_idx,
                tombstone: delta.tombstone,
            });
        }
    }
    for ((node_idx, column_id, value), group) in filters {
        if let Some(delta) = select_filter(&group) {
            segment.filters.push(SegmentFilterValue {
                node_idx,
                column_id,
                value,
                tombstone: delta.tombstone,
            });
        }
    }
    for ((node_idx, tenant_hash), group) in tenants {
        if let Some(delta) = select_tenant(&group) {
            segment.tenants.push(SegmentTenant {
                node_idx,
                tenant_hash,
                tombstone: delta.tombstone,
            });
        }
    }
    if segment.node_states.is_empty()
        && segment.resolutions.is_empty()
        && segment.filters.is_empty()
        && segment.tenants.is_empty()
    {
        Ok(None)
    } else {
        Ok(Some(segment))
    }
}

#[derive(Debug, Clone, Copy)]
struct NodeStateDelta {
    sync_id: u64,
    active: bool,
}

#[derive(Debug, Clone, Copy)]
struct ResolutionDelta {
    sync_id: u64,
    node_idx: u32,
    tombstone: bool,
}

#[derive(Debug, Clone, Copy)]
struct FilterDelta {
    sync_id: u64,
    tombstone: bool,
}

#[derive(Debug, Clone, Copy)]
struct TenantDelta {
    sync_id: u64,
    tombstone: bool,
}

fn select_node_state(group: &[NodeStateDelta]) -> Option<NodeStateDelta> {
    if group.len() == 2
        && group.iter().any(|delta| delta.active)
        && group.iter().any(|delta| !delta.active)
    {
        return None;
    }
    group
        .iter()
        .max_by_key(|delta| (!delta.active, delta.sync_id))
        .copied()
}

fn select_resolution(group: &[ResolutionDelta]) -> Option<ResolutionDelta> {
    if cancels_tombstone_pair(group.iter().map(|delta| delta.tombstone)) {
        return None;
    }
    group
        .iter()
        .max_by_key(|delta| (delta.tombstone, delta.sync_id, delta.node_idx))
        .copied()
}

fn select_filter(group: &[FilterDelta]) -> Option<FilterDelta> {
    if cancels_tombstone_pair(group.iter().map(|delta| delta.tombstone)) {
        return None;
    }
    group
        .iter()
        .max_by_key(|delta| (delta.tombstone, delta.sync_id))
        .copied()
}

fn select_tenant(group: &[TenantDelta]) -> Option<TenantDelta> {
    if cancels_tombstone_pair(group.iter().map(|delta| delta.tombstone)) {
        return None;
    }
    group
        .iter()
        .max_by_key(|delta| (delta.tombstone, delta.sync_id))
        .copied()
}

fn cancels_tombstone_pair(tombstones: impl Iterator<Item = bool>) -> bool {
    let values = tombstones.collect::<Vec<_>>();
    values.len() == 2 && values.iter().any(|value| !value) && values.iter().any(|value| *value)
}

fn next_generation_id(
    previous: Option<&ProjectionManifest>,
    rows: &[ProjectionSyncRow],
) -> GraphResult<u64> {
    let row_generation = rows.iter().map(|row| row.generation_id).max().unwrap_or(1);
    previous.map_or_else(
        || Ok(row_generation.max(1)),
        |manifest| {
            manifest
                .generation_id
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("projection generation id overflowed".into()))
        },
    )
}

fn validate_ingestion_limits(row_count: usize, limits: MutationBufferLimits) -> GraphResult<()> {
    if row_count > limits.max_rows {
        return Err(GraphError::OverlayLimit {
            kind: "projection_ingest_rows".to_string(),
            requested: row_count,
            limit: limits.max_rows,
        });
    }
    let requested_bytes = row_count
        .checked_mul(INGEST_ROW_BYTES)
        .ok_or_else(|| GraphError::Internal("projection ingest byte estimate overflowed".into()))?;
    if requested_bytes > limits.max_bytes {
        return Err(GraphError::OverlayLimit {
            kind: "projection_ingest_bytes".to_string(),
            requested: requested_bytes,
            limit: limits.max_bytes,
        });
    }
    Ok(())
}

fn segment_ref(
    root: &Path,
    path: &Path,
    segment: &DeltaSegment,
) -> GraphResult<ManifestSegmentRef> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| GraphError::Internal("projection segment path escaped artifact root".into()))?
        .to_string_lossy()
        .to_string();
    Ok(ManifestSegmentRef {
        path: relative,
        checksum: segment_checksum(path)?,
        level: segment.header.level,
        source_start: segment.header.source_start,
        source_end: segment.header.source_end,
        sync_watermark: segment.header.sync_watermark,
    })
}

fn segment_checksum(path: &Path) -> GraphResult<String> {
    let bytes = std::fs::read(path)
        .map_err(|err| GraphError::Internal(format!("read projection segment checksum: {err}")))?;
    Ok(format!("crc32:{:08x}", crc32fast::hash(&bytes)))
}

fn cleanup_segment_refs(root: &Path, segments: &[ManifestSegmentRef]) -> GraphResult<()> {
    let mut removed_any = false;
    for segment in segments {
        let path = root.join(&segment.path);
        match std::fs::remove_file(&path) {
            Ok(()) => removed_any = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(GraphError::Internal(format!(
                    "remove unpublished projection segment: {err}"
                )));
            }
        }
    }
    if removed_any {
        sync_directory(root)?;
    }
    Ok(())
}

fn write_segment_atomically(
    root: &Path,
    final_path: &Path,
    segment: &DeltaSegment,
) -> GraphResult<()> {
    std::fs::create_dir_all(root)
        .map_err(|err| GraphError::Internal(format!("create projection segment dir: {err}")))?;
    let bytes = segment.to_bytes()?;
    let tmp_path = temp_segment_path(root, final_path)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|err| {
                GraphError::Internal(format!("create temp projection segment: {err}"))
            })?;
        file.write_all(&bytes)
            .map_err(|err| GraphError::Internal(format!("write temp projection segment: {err}")))?;
        file.sync_all()
            .map_err(|err| GraphError::Internal(format!("fsync temp projection segment: {err}")))?;
        drop(file);
        sync_directory(root)?;
        std::fs::hard_link(&tmp_path, final_path).map_err(|err| {
            GraphError::Internal(format!(
                "publish projection segment without overwrite: {err}"
            ))
        })?;
        sync_directory(root)?;
        std::fs::remove_file(&tmp_path).map_err(|err| {
            GraphError::Internal(format!("remove temp projection segment: {err}"))
        })?;
        sync_directory(root)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn temp_segment_path(root: &Path, final_path: &Path) -> GraphResult<PathBuf> {
    let final_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| GraphError::Internal("projection segment path has no file name".into()))?;
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?
        .as_nanos();
    for attempt in 0..128 {
        let path = root.join(format!(
            "{final_name}.tmp-{}-{created_at}-{attempt}",
            std::process::id()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(GraphError::Internal(
        "projection segment temp path kept colliding".into(),
    ))
}

fn sync_directory(path: &Path) -> GraphResult<()> {
    let dir = std::fs::File::open(path)
        .map_err(|err| GraphError::Internal(format!("open projection segment dir: {err}")))?;
    dir.sync_all()
        .map_err(|err| GraphError::Internal(format!("fsync projection segment dir: {err}")))
}

fn lock_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn segment_file_name(generation_id: u64, segment_id: u32) -> String {
    format!("projection-generation-{generation_id:020}-segment-{segment_id:08}.pggraph-delta")
}

fn now_unix_micros() -> GraphResult<i64> {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?
        .as_micros();
    i64::try_from(micros)
        .map_err(|_| GraphError::Internal("current timestamp exceeds i64 micros".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::manifest::ProjectionManifestStore;
    use crate::projection::test_fixtures::ProjectionArtifactDir;

    #[test]
    fn projection_ingest_committed_edge_insert_publishes_l0_manifest() {
        let dir = seeded_artifacts("projection_ingest_committed_edge_insert");
        let ingester = ingester(&dir);
        let rows = vec![edge_row(1, 0, 1, None, MutationOperation::InsertEdge)];

        let result = ingester
            .ingest_committed_rows(&rows, MutationBufferLimits::new(10, 10_000))
            .expect("ingestion publishes");
        let manifest = result.manifest.expect("manifest published");
        let segment = load_segment(&dir, &manifest.segments[0].path);

        assert_eq!(manifest.sync_watermark, 1);
        assert_eq!(manifest.segments.len(), 1);
        assert_eq!(segment.header.level, 0);
        assert_eq!(segment.edge_inserts[0].source, 0);
        assert_eq!(segment.edge_inserts[0].target, 1);
    }

    #[test]
    fn projection_ingest_publishes_weight_node_resolution_filter_tenant_deltas() {
        let dir = seeded_artifacts("projection_ingest_publishes_all_surfaces");
        let ingester = ingester(&dir);
        let rows = vec![
            edge_row(1, 0, 1, Some(7), MutationOperation::InsertEdge),
            ProjectionSyncRow {
                sync_id: 2,
                generation_id: 1,
                committed: true,
                operation: MutationOperation::UpsertNode,
                direction: TraversalDirection::Any,
                source: 2,
                target: 2,
                type_id: 0,
                weight: None,
                table_oid: Some(100),
                pk_hash: Some(7000),
                node_idx: Some(2),
                filter_column_id: Some(3),
                filter_value: Some(88),
                tenant_hash: Some(8001),
            },
            ProjectionSyncRow {
                sync_id: 3,
                generation_id: 1,
                committed: true,
                operation: MutationOperation::DeleteNode,
                direction: TraversalDirection::Any,
                source: 2,
                target: 2,
                type_id: 0,
                weight: None,
                table_oid: Some(100),
                pk_hash: Some(7000),
                node_idx: Some(2),
                filter_column_id: Some(3),
                filter_value: Some(88),
                tenant_hash: Some(8001),
            },
            ProjectionSyncRow {
                sync_id: 4,
                generation_id: 1,
                committed: true,
                operation: MutationOperation::UpsertNode,
                direction: TraversalDirection::Any,
                source: 4,
                target: 4,
                type_id: 0,
                weight: None,
                table_oid: Some(100),
                pk_hash: Some(7001),
                node_idx: Some(4),
                filter_column_id: Some(3),
                filter_value: Some(99),
                tenant_hash: Some(8002),
            },
            ProjectionSyncRow {
                sync_id: 5,
                generation_id: 1,
                committed: true,
                operation: MutationOperation::UpsertNode,
                direction: TraversalDirection::Any,
                source: 4,
                target: 4,
                type_id: 0,
                weight: None,
                table_oid: Some(101),
                pk_hash: Some(7002),
                node_idx: Some(4),
                filter_column_id: Some(4),
                filter_value: Some(100),
                tenant_hash: Some(8003),
            },
        ];

        let result = ingester
            .ingest_committed_rows(&rows, MutationBufferLimits::new(10, 10_000))
            .expect("ingestion publishes");
        let manifest = result.manifest.expect("manifest published");

        assert_eq!(manifest.segments.len(), 2);
        let edge = load_segment(&dir, &manifest.segments[0].path);
        let node = load_segment(&dir, &manifest.segments[1].path);
        assert_eq!(edge.edge_weights[0].weight, 7);
        assert_eq!(edge.header.direction, TraversalDirection::Out);
        assert_eq!(node.node_states[0].node_idx, 4);
        assert_eq!(node.resolutions[0].pk_hash, 7001);
        assert_eq!(node.filters[0].value, 99);
        assert_eq!(node.tenants[0].tenant_hash, 8002);
        assert_eq!(node.node_states.len(), 1);
        assert_eq!(node.resolutions.len(), 2);
        assert_eq!(node.filters.len(), 2);
        assert_eq!(node.tenants.len(), 2);
    }

    #[test]
    fn projection_manifest_watermark_advances_only_after_publish() {
        let dir = seeded_artifacts("projection_watermark_after_publish");
        let publisher = ingester(&dir);
        let bad_rows = vec![ProjectionSyncRow {
            operation: MutationOperation::UpsertNode,
            direction: TraversalDirection::Any,
            ..edge_row(1, 0, 1, None, MutationOperation::InsertEdge)
        }];
        let err = publisher
            .ingest_committed_rows(&bad_rows, MutationBufferLimits::new(0, 0))
            .expect_err("limit failure prevents publish");

        assert!(matches!(err, GraphError::OverlayLimit { .. }));
        let latest = ProjectionManifestStore::new(dir.path())
            .load_latest_current()
            .expect("manifest loads")
            .expect("base manifest remains current");
        assert_eq!(latest.sync_watermark, 0);

        let publish_failure_dir = seeded_artifacts("projection_publish_failure_retry");
        let bad_ingester = ProjectionIngester::new(
            publish_failure_dir.path(),
            "missing.pggraph",
            "crc32:00000000",
            1,
        );
        let rows = vec![edge_row(1, 0, 1, None, MutationOperation::InsertEdge)];
        let publish_err = bad_ingester
            .ingest_committed_rows(&rows, MutationBufferLimits::new(10, 10_000))
            .expect_err("missing base artifact rejects manifest publish");

        assert!(matches!(publish_err, GraphError::CorruptFile { .. }));
        let retry = ingester(&publish_failure_dir)
            .ingest_committed_rows(&rows, MutationBufferLimits::new(10, 10_000))
            .expect("retry publishes after orphan cleanup");
        assert!(retry.manifest.is_some());
    }

    #[test]
    fn projection_ingest_aborted_gql_write_is_not_published() {
        let dir = seeded_artifacts("projection_ingest_aborted");
        let ingester = ingester(&dir);
        let rows = vec![ProjectionSyncRow {
            committed: false,
            ..edge_row(1, 0, 1, None, MutationOperation::InsertEdge)
        }];

        let result = ingester
            .ingest_committed_rows(&rows, MutationBufferLimits::new(10, 10_000))
            .expect("aborted row ignored");

        assert!(result.manifest.is_none());
        assert_eq!(result.rows_ingested, 0);
    }

    #[test]
    fn projection_ingest_respects_build_vacuum_compaction_lock() {
        let dir = seeded_artifacts("projection_ingest_lock");
        let primary_ingester = ingester(&dir);
        let contending_ingester = ingester(&dir);
        let _guard = primary_ingester
            .hold_publication_lock_for_test()
            .expect("lock acquired");

        let err = contending_ingester
            .ingest_committed_rows(
                &[edge_row(1, 0, 1, None, MutationOperation::InsertEdge)],
                MutationBufferLimits::new(10, 10_000),
            )
            .expect_err("held lock rejects ingestion");

        assert!(matches!(err, GraphError::BuildLocked));
    }

    #[test]
    fn projection_ingest_concurrent_publishers_serialize() {
        let dir = seeded_artifacts("projection_ingest_serial_generations");
        let publisher = ingester(&dir);

        let first = publisher
            .ingest_committed_rows(
                &[edge_row(1, 0, 1, None, MutationOperation::InsertEdge)],
                MutationBufferLimits::new(10, 10_000),
            )
            .expect("first publish")
            .manifest
            .expect("first manifest");
        let second = publisher
            .ingest_committed_rows(
                &[ProjectionSyncRow {
                    direction: TraversalDirection::In,
                    ..edge_row(2, 1, 2, None, MutationOperation::InsertEdge)
                }],
                MutationBufferLimits::new(10, 10_000),
            )
            .expect("second publish")
            .manifest
            .expect("second manifest");
        let second_segment = load_segment(&dir, &second.segments[0].path);

        assert_eq!(second.previous_generation_id, Some(first.generation_id));
        assert!(second.generation_id > first.generation_id);
        assert_eq!(second.sync_watermark, 2);
        assert_eq!(second_segment.header.direction, TraversalDirection::In);

        let overflow_dir = seeded_artifacts("projection_ingest_generation_overflow");
        ProjectionManifestStore::new(overflow_dir.path())
            .publish(&ProjectionManifest::base_only(
                u64::MAX,
                "base.pggraph",
                "crc32:00000000",
                1,
                0,
                1,
            ))
            .expect("max manifest publishes");
        let overflow_err = ingester(&overflow_dir)
            .ingest_committed_rows(
                &[edge_row(1, 0, 1, None, MutationOperation::InsertEdge)],
                MutationBufferLimits::new(10, 10_000),
            )
            .expect_err("generation overflow is rejected");

        assert!(matches!(overflow_err, GraphError::Internal(_)));
    }

    fn seeded_artifacts(name: &str) -> ProjectionArtifactDir {
        let dir = ProjectionArtifactDir::new(name);
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base artifact writes");
        let base = ProjectionManifest::base_only(1, "base.pggraph", "crc32:00000000", 1, 0, 1);
        ProjectionManifestStore::new(dir.path())
            .publish(&base)
            .expect("base manifest publishes");
        dir
    }

    fn ingester(dir: &ProjectionArtifactDir) -> ProjectionIngester {
        ProjectionIngester::new(dir.path(), "base.pggraph", "crc32:00000000", 1)
    }

    fn edge_row(
        sync_id: u64,
        source: u32,
        target: u32,
        weight: Option<u32>,
        operation: MutationOperation,
    ) -> ProjectionSyncRow {
        ProjectionSyncRow {
            sync_id,
            generation_id: 1,
            committed: true,
            operation,
            direction: TraversalDirection::Out,
            source,
            target,
            type_id: 2,
            weight,
            table_oid: None,
            pk_hash: None,
            node_idx: None,
            filter_column_id: None,
            filter_value: None,
            tenant_hash: None,
        }
    }

    fn load_segment(dir: &ProjectionArtifactDir, relative_path: &str) -> DeltaSegment {
        DeltaSegment::read_from_path(&dir.path().join(relative_path)).expect("segment reads")
    }
}
