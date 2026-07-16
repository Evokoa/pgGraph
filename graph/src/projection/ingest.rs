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

use crate::edge_store::RelationshipIdentity;
use crate::filter_index::PersistedFilterValue;
use crate::projection::identity::{
    read_identity_artifact, read_manifest_identity_artifact, write_identity_artifact,
    RelationshipIdentityDictionary,
};
use crate::projection::manifest::{
    ManifestFileRef, ManifestIdentityRef, ManifestSegmentRef, ProjectionManifest,
    ProjectionManifestStore, MANIFEST_DECODED_MEMORY_BYTES_PER_JSON_BYTE,
};
use crate::projection::normalize::{
    normalize_committed_mutations, CommittedMutation, MutationBufferLimits, MutationOperation,
    NormalizedMutation,
};
use crate::projection::segment::{
    DeltaSegment, SegmentFilterValue, SegmentKind, SegmentNodeState, SegmentResolution,
    SegmentTenant,
};
use crate::resource::{ByteCount, ResourceGovernor, ResourcePhase};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

const INGEST_ROW_BYTES: usize = std::mem::size_of::<ProjectionSyncRow>();
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
    pub(crate) schema_reversed: bool,
    pub(crate) weight: Option<u32>,
    pub(crate) relationship_identity: Option<RelationshipIdentity>,
    pub(crate) table_oid: Option<u32>,
    pub(crate) pk_hash: Option<u64>,
    pub(crate) primary_key: Option<String>,
    pub(crate) node_idx: Option<u32>,
    pub(crate) filter_column_id: Option<u32>,
    pub(crate) filter_value: Option<PersistedFilterValue>,
    pub(crate) tenant_hash: Option<u64>,
    pub(crate) tenant: Option<String>,
}

impl ProjectionSyncRow {
    fn to_committed_mutation(
        &self,
        dictionary: &RelationshipIdentityDictionary,
    ) -> GraphResult<CommittedMutation> {
        Ok(CommittedMutation {
            sync_id: self.sync_id,
            generation_id: self.generation_id,
            direction: self.direction,
            source: self.source,
            target: self.target,
            type_id: self.type_id,
            schema_reversed: self.schema_reversed,
            weight: self.weight,
            relationship_id: self
                .relationship_identity
                .as_ref()
                .map(|identity| dictionary.id_for(identity))
                .transpose()?,
            operation: self.operation,
        })
    }
}

/// Result of one projection ingestion publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionIngestResult {
    pub(crate) manifest: Option<ProjectionManifest>,
    pub(crate) rows_ingested: usize,
    pub(crate) segments_published: usize,
    /// Cumulative identity dictionary installed by this publication.
    pub(crate) relationship_identities: Option<Vec<Option<RelationshipIdentity>>>,
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
    #[allow(
        dead_code,
        reason = "test and benchmark callers use the base-only convenience path"
    )]
    pub(crate) fn ingest_committed_rows(
        &self,
        rows: &[ProjectionSyncRow],
        limits: MutationBufferLimits,
    ) -> GraphResult<ProjectionIngestResult> {
        self.ingest_committed_rows_with_identities(rows, limits, &[None])
    }

    /// Publish committed rows using the base artifact's identity dictionary.
    pub(crate) fn ingest_committed_rows_with_identities(
        &self,
        rows: &[ProjectionSyncRow],
        limits: MutationBufferLimits,
        base_relationship_identities: &[Option<RelationshipIdentity>],
    ) -> GraphResult<ProjectionIngestResult> {
        self.ingest_committed_rows_with_identities_validated(
            rows,
            limits,
            base_relationship_identities,
            |_| Ok(()),
        )
        .map(|(result, _)| result)
    }

    /// Publish committed rows after validating the complete candidate manifest.
    pub(crate) fn ingest_committed_rows_with_identities_validated<F, T>(
        &self,
        rows: &[ProjectionSyncRow],
        limits: MutationBufferLimits,
        base_relationship_identities: &[Option<RelationshipIdentity>],
        validate_candidate: F,
    ) -> GraphResult<(ProjectionIngestResult, Option<T>)>
    where
        F: FnOnce(&ProjectionManifest) -> GraphResult<T>,
    {
        let _guard = self.lock.try_enter()?;
        self.ingest_committed_rows_locked(
            rows,
            limits,
            base_relationship_identities,
            None,
            validate_candidate,
        )
    }

    /// Publish committed rows under one statement-local resource governor.
    ///
    /// The ingester reserves a conservative peak for its retained maps,
    /// identity dictionary, segments, serialization buffers, and validation
    /// reloads before any of those allocations are made. Artifact and staging
    /// bytes remain charged until manifest publication has completed.
    pub(crate) fn ingest_committed_rows_with_identities_governed<F, T>(
        &self,
        rows: &[ProjectionSyncRow],
        limits: MutationBufferLimits,
        base_relationship_identities: &[Option<RelationshipIdentity>],
        governor: &ResourceGovernor,
        validate_candidate: F,
    ) -> GraphResult<(ProjectionIngestResult, Option<T>)>
    where
        F: FnOnce(&ProjectionManifest) -> GraphResult<T>,
    {
        let _guard = self.lock.try_enter()?;
        self.ingest_committed_rows_locked(
            rows,
            limits,
            base_relationship_identities,
            Some(governor),
            validate_candidate,
        )
    }

    fn ingest_committed_rows_locked<F, T>(
        &self,
        rows: &[ProjectionSyncRow],
        limits: MutationBufferLimits,
        base_relationship_identities: &[Option<RelationshipIdentity>],
        governor: Option<&ResourceGovernor>,
        validate_candidate: F,
    ) -> GraphResult<(ProjectionIngestResult, Option<T>)>
    where
        F: FnOnce(&ProjectionManifest) -> GraphResult<T>,
    {
        let _ingest_memory = governor
            .map(|governor| {
                // Cover directory iteration and metadata inspection used by
                // the allocation-free size preflight itself.
                let mut lease = governor
                    .reserve_memory(
                        ResourcePhase::SyncIngest,
                        ByteCount::from_bytes(1024 * 1024),
                    )
                    .map_err(crate::safety::resource_limit_error)?;
                let peak =
                    ingestion_memory_upper_bound(&self.root, rows, base_relationship_identities)?;
                lease
                    .try_resize(peak)
                    .map_err(crate::safety::resource_limit_error)?;
                Ok::<_, GraphError>(lease)
            })
            .transpose()?;
        let previous = self.store.load_latest_current()?;
        let previous_watermark = previous
            .as_ref()
            .map_or(0, |manifest| manifest.sync_watermark);
        let committed_rows = rows
            .iter()
            .filter(|row| row.committed)
            .filter(|row| i64::try_from(row.sync_id).is_ok_and(|id| id > previous_watermark))
            .collect::<Vec<_>>();
        if committed_rows.is_empty() {
            return Ok((
                ProjectionIngestResult {
                    manifest: None,
                    rows_ingested: 0,
                    segments_published: 0,
                    relationship_identities: None,
                },
                None,
            ));
        }
        validate_ingestion_limits(committed_rows.iter().copied(), limits)?;

        let generation_id = next_generation_id(previous.as_ref(), &committed_rows)?;
        let mut identity_dictionary =
            self.load_identity_dictionary(previous.as_ref(), base_relationship_identities)?;
        let prior_identity_count = identity_dictionary.identities().len();
        identity_dictionary.intern_all(
            committed_rows
                .iter()
                .filter_map(|row| row.relationship_identity.clone()),
        )?;
        let identity_changed = identity_dictionary.identities().len() > prior_identity_count;
        let identity_artifact_required = identity_changed
            || (previous
                .as_ref()
                .and_then(|manifest| manifest.relationship_identities.as_ref())
                .is_none()
                && committed_rows
                    .iter()
                    .any(|row| row.relationship_identity.is_some()));
        let sync_watermark = committed_rows
            .iter()
            .map(|row| i64::try_from(row.sync_id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| GraphError::Internal("projection sync id exceeds i64".into()))?
            .into_iter()
            .max()
            .unwrap_or(previous_watermark);

        let mut segments = self.edge_segments(&committed_rows, limits, &identity_dictionary)?;
        if let Some(node_segment) = node_segment(&committed_rows, limits, sync_watermark)? {
            segments.push(node_segment);
        }
        let identity_artifact_bytes = identity_artifact_encoded_len(&identity_dictionary)?;
        let initial_artifact_bytes = if identity_artifact_required {
            identity_artifact_bytes
        } else {
            0
        };
        let artifact_bytes =
            segments
                .iter()
                .try_fold(initial_artifact_bytes, |total, segment| {
                    total
                        .checked_add(segment.encoded_len()?)
                        .ok_or_else(ingest_resource_size_overflow)
                })?;
        let _artifact_disk = governor
            .map(|governor| {
                let bytes = ByteCount::from_usize(artifact_bytes)
                    .ok_or_else(ingest_resource_size_overflow)?;
                governor
                    .reserve_disk(ResourcePhase::SyncIngest, bytes)
                    .map_err(crate::safety::resource_limit_error)
            })
            .transpose()?;

        let mut new_segment_refs = Vec::with_capacity(segments.len());
        for edge_segment in segments {
            let path =
                self.write_segment(generation_id, new_segment_refs.len() as u32, &edge_segment)?;
            new_segment_refs.push(segment_ref(&self.root, &path, &edge_segment)?);
        }
        let identity_ref = if identity_artifact_required {
            match self.write_identity_dictionary(generation_id, &identity_dictionary) {
                Ok(reference) => {
                    if reference.bytes != identity_artifact_bytes as u64 {
                        cleanup_candidate_artifacts(
                            &self.root,
                            &new_segment_refs,
                            Some(&reference),
                        )?;
                        return Err(GraphError::CorruptFile {
                            reason: "relationship identity artifact size changed after resource preflight"
                                .to_string(),
                        });
                    }
                    Some(reference)
                }
                Err(err) => {
                    cleanup_segment_refs(&self.root, &new_segment_refs)?;
                    return Err(err);
                }
            }
        } else {
            previous
                .as_ref()
                .and_then(|manifest| manifest.relationship_identities.clone())
        };

        let mut manifest = ProjectionManifest::base_only(
            generation_id,
            self.base_artifact_path.clone(),
            self.base_artifact_checksum.clone(),
            self.base_artifact_version,
            sync_watermark,
            now_unix_micros()?,
        );
        if let Some(previous) = previous.as_ref() {
            manifest.inherit_operation_timestamps(previous);
            manifest.segments = previous.segments.clone();
            manifest.base_chunks = previous.base_chunks.clone();
            manifest.obsolete_files = previous.obsolete_files.clone();
        }
        manifest.relationship_identities = identity_ref.clone();
        if identity_artifact_required {
            if let Some(previous_identity) = previous
                .as_ref()
                .and_then(|manifest| manifest.relationship_identities.as_ref())
            {
                manifest.obsolete_files.push(ManifestFileRef {
                    path: previous_identity.path.clone(),
                    bytes: previous_identity.bytes,
                });
            }
        }
        manifest.previous_generation_id = previous.as_ref().map(|manifest| manifest.generation_id);
        manifest.mark_ingestion();
        manifest.segments.extend(new_segment_refs.iter().cloned());
        let validated = match validate_candidate(&manifest) {
            Ok(validated) => validated,
            Err(err) => {
                cleanup_candidate_artifacts(
                    &self.root,
                    &new_segment_refs,
                    identity_artifact_required
                        .then_some(identity_ref.as_ref())
                        .flatten(),
                )?;
                return Err(err);
            }
        };
        let expected_current = previous.as_ref().map(|manifest| manifest.generation_id);
        let publish_result = governor.map_or_else(
            || self.store.publish_if_current(&manifest, expected_current),
            |governor| {
                self.store.publish_governed_if_current(
                    &manifest,
                    expected_current,
                    governor,
                    ResourcePhase::SyncIngest,
                    usize::try_from(governor.memory_limit().as_u64()).unwrap_or(usize::MAX),
                )
            },
        );
        if let Err(err) = publish_result {
            if !self.store.manifest_path(generation_id).exists() {
                cleanup_candidate_artifacts(
                    &self.root,
                    &new_segment_refs,
                    identity_artifact_required
                        .then_some(identity_ref.as_ref())
                        .flatten(),
                )?;
            }
            return Err(err);
        }

        Ok((
            ProjectionIngestResult {
                rows_ingested: committed_rows.len(),
                segments_published: new_segment_refs.len(),
                manifest: Some(manifest),
                relationship_identities: Some(identity_dictionary.identities().to_vec()),
            },
            Some(validated),
        ))
    }

    #[cfg(test)]
    fn hold_publication_lock_for_test(&self) -> GraphResult<ProjectionIngestGuard<'_>> {
        self.lock.try_enter()
    }

    fn edge_segments(
        &self,
        rows: &[&ProjectionSyncRow],
        limits: MutationBufferLimits,
        identity_dictionary: &RelationshipIdentityDictionary,
    ) -> GraphResult<Vec<DeltaSegment>> {
        let mut segments = Vec::new();
        for direction in [
            TraversalDirection::Out,
            TraversalDirection::In,
            TraversalDirection::Any,
        ] {
            let edge_rows = rows
                .iter()
                .copied()
                .filter(|row| row.operation.is_edge())
                .filter(|row| row.direction == direction)
                .map(|row| row.to_committed_mutation(identity_dictionary))
                .collect::<GraphResult<Vec<_>>>()?;
            if edge_rows.is_empty() {
                continue;
            }
            let normalized = normalize_committed_mutations(&edge_rows, limits)?;
            if normalized.rows.is_empty() {
                continue;
            }
            let (source_start, source_end) = normalized_edge_source_range(&normalized.rows)?
                .ok_or_else(|| GraphError::Internal("non-empty edge batch had no range".into()))?;
            segments.push(DeltaSegment::from_normalized_edges(
                &normalized,
                0,
                direction,
                source_start,
                source_end,
            )?);
        }
        Ok(segments)
    }

    fn load_identity_dictionary(
        &self,
        previous: Option<&ProjectionManifest>,
        base_relationship_identities: &[Option<RelationshipIdentity>],
    ) -> GraphResult<RelationshipIdentityDictionary> {
        if let Some(reference) =
            previous.and_then(|manifest| manifest.relationship_identities.as_ref())
        {
            return read_manifest_identity_artifact(
                &self.root.join(&reference.path),
                &reference.checksum,
                reference.bytes,
                reference.entry_count,
            );
        }
        RelationshipIdentityDictionary::try_from_identities(base_relationship_identities.to_vec())
    }

    fn write_identity_dictionary(
        &self,
        generation_id: u64,
        dictionary: &RelationshipIdentityDictionary,
    ) -> GraphResult<ManifestIdentityRef> {
        let file_name = format!("relationship-identities-{generation_id:020}.bin");
        let path = self.root.join(&file_name);
        let (checksum, bytes) = write_identity_artifact(&self.root, &path, dictionary)?;
        let decoded = match read_identity_artifact(&path, &checksum) {
            Ok(decoded) => decoded,
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                return Err(err);
            }
        };
        if decoded != *dictionary {
            let _ = std::fs::remove_file(&path);
            return Err(GraphError::CorruptFile {
                reason: "relationship identity artifact validation changed dictionary contents"
                    .to_string(),
            });
        }
        Ok(ManifestIdentityRef {
            path: file_name,
            checksum,
            entry_count: u32::try_from(dictionary.identities().len()).map_err(|_| {
                GraphError::Internal("relationship identity count exceeds u32".to_string())
            })?,
            bytes,
        })
    }

    fn write_segment(
        &self,
        generation_id: u64,
        segment_id: u32,
        segment: &DeltaSegment,
    ) -> GraphResult<PathBuf> {
        let path = self.root.join(segment_file_name(generation_id, segment_id));
        write_segment_atomically(&self.root, &path, segment)?;
        let mut decoded = match DeltaSegment::read_from_path(&path) {
            Ok(decoded) => decoded,
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                return Err(err);
            }
        };
        decoded.header.checksum = segment.header.checksum;
        if decoded != *segment {
            let _ = std::fs::remove_file(&path);
            return Err(GraphError::CorruptFile {
                reason: "projection ingest segment validation changed its contents".to_string(),
            });
        }
        Ok(path)
    }
}

fn node_segment(
    rows: &[&ProjectionSyncRow],
    limits: MutationBufferLimits,
    sync_watermark: i64,
) -> GraphResult<Option<DeltaSegment>> {
    validate_ingestion_limits(
        rows.iter().copied().filter(|row| !row.operation.is_edge()),
        limits,
    )?;
    let mut segment = DeltaSegment::new(
        SegmentKind::Node,
        0,
        TraversalDirection::Any,
        0,
        0,
        sync_watermark,
    )?;
    let mut node_states = BTreeMap::<u32, Vec<NodeStateDelta>>::new();
    let mut resolutions = BTreeMap::<(u32, u32, String), Vec<ResolutionDelta>>::new();
    let mut filters = BTreeMap::<(u32, u32), Vec<FilterDelta<'_>>>::new();
    let mut tenants = BTreeMap::<(u32, String), Vec<TenantDelta>>::new();
    for (sequence, row) in rows
        .iter()
        .filter(|row| !row.operation.is_edge())
        .enumerate()
    {
        let node_idx = row.node_idx.unwrap_or(row.source);
        if let (Some(table_oid), Some(pk_hash), Some(primary_key)) =
            (row.table_oid, row.pk_hash, row.primary_key.as_ref())
        {
            if pk_hash != crate::resolution_index::ResolutionIndexBuilder::hash_pk(primary_key) {
                return Err(GraphError::CorruptFile {
                    reason: "projection node primary key does not match its hash".to_string(),
                });
            }
            node_states
                .entry(node_idx)
                .or_default()
                .push(NodeStateDelta {
                    sync_id: row.sync_id,
                    sequence,
                    active: row.operation.is_insert(),
                });
            resolutions
                .entry((node_idx, table_oid, primary_key.clone()))
                .or_default()
                .push(ResolutionDelta {
                    sync_id: row.sync_id,
                    sequence,
                    node_idx,
                    tombstone: row.operation.is_delete(),
                });
        }
        if let (Some(column_id), Some(value)) = (row.filter_column_id, row.filter_value.as_ref()) {
            filters
                .entry((node_idx, column_id))
                .or_default()
                .push(FilterDelta {
                    sync_id: row.sync_id,
                    sequence,
                    value,
                    tombstone: row.operation.is_delete(),
                });
        }
        if let (Some(tenant_hash), Some(tenant)) = (row.tenant_hash, row.tenant.as_ref()) {
            if tenant_hash != xxhash_rust::xxh3::xxh3_64(tenant.as_bytes()) {
                return Err(GraphError::CorruptFile {
                    reason: "projection tenant identifier does not match its hash".to_string(),
                });
            }
            tenants
                .entry((node_idx, tenant.clone()))
                .or_default()
                .push(TenantDelta {
                    sync_id: row.sync_id,
                    sequence,
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
    for ((_node_idx, table_oid, primary_key), group) in resolutions {
        if let Some(delta) = select_resolution(&group) {
            segment.resolutions.push(SegmentResolution {
                table_oid,
                pk_hash: crate::resolution_index::ResolutionIndexBuilder::hash_pk(&primary_key),
                primary_key: Some(primary_key),
                node_idx: delta.node_idx,
                tombstone: delta.tombstone,
            });
        }
    }
    for ((node_idx, column_id), group) in filters {
        if let Some(delta) = select_filter(&group) {
            segment.filters.push(SegmentFilterValue {
                node_idx,
                column_id,
                value: delta.value.clone(),
                tombstone: delta.tombstone,
            });
        }
    }
    for ((node_idx, tenant), group) in tenants {
        if let Some(delta) = select_tenant(&group) {
            segment.tenants.push(SegmentTenant {
                node_idx,
                tenant_hash: xxhash_rust::xxh3::xxh3_64(tenant.as_bytes()),
                tenant: Some(tenant),
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
        let (source_start, source_end) = node_segment_source_range(&segment)?.ok_or_else(|| {
            GraphError::Internal("non-empty node segment had no source range".into())
        })?;
        segment.header.source_start = source_start;
        segment.header.source_end = source_end;
        Ok(Some(segment))
    }
}

#[derive(Debug, Default)]
struct DirtySourceRange {
    start: Option<u32>,
    end: u32,
}

impl DirtySourceRange {
    fn include(&mut self, source: u32) -> GraphResult<()> {
        let exclusive_end = source
            .checked_add(1)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: "dirty source range exceeds projection segment format".to_string(),
            })?;
        self.start = Some(self.start.map_or(source, |start| start.min(source)));
        self.end = self.end.max(exclusive_end);
        Ok(())
    }

    fn finish(self) -> Option<(u32, u32)> {
        self.start.map(|start| (start, self.end))
    }
}

fn normalized_edge_source_range(rows: &[NormalizedMutation]) -> GraphResult<Option<(u32, u32)>> {
    let mut range = DirtySourceRange::default();
    for row in rows {
        range.include(row.source)?;
    }
    Ok(range.finish())
}

fn node_segment_source_range(segment: &DeltaSegment) -> GraphResult<Option<(u32, u32)>> {
    let mut range = DirtySourceRange::default();
    for row in &segment.node_states {
        range.include(row.node_idx)?;
    }
    for row in &segment.resolutions {
        range.include(row.node_idx)?;
    }
    for row in &segment.filters {
        range.include(row.node_idx)?;
    }
    for row in &segment.tenants {
        range.include(row.node_idx)?;
    }
    Ok(range.finish())
}

#[derive(Debug, Clone, Copy)]
struct NodeStateDelta {
    sync_id: u64,
    sequence: usize,
    active: bool,
}

#[derive(Debug, Clone, Copy)]
struct ResolutionDelta {
    sync_id: u64,
    sequence: usize,
    node_idx: u32,
    tombstone: bool,
}

#[derive(Debug, Clone, Copy)]
struct FilterDelta<'a> {
    sync_id: u64,
    sequence: usize,
    value: &'a PersistedFilterValue,
    tombstone: bool,
}

#[derive(Debug, Clone, Copy)]
struct TenantDelta {
    sync_id: u64,
    sequence: usize,
    tombstone: bool,
}

fn select_node_state(group: &[NodeStateDelta]) -> Option<NodeStateDelta> {
    group
        .iter()
        .max_by_key(|delta| (delta.sync_id, delta.sequence))
        .copied()
}

fn select_resolution(group: &[ResolutionDelta]) -> Option<ResolutionDelta> {
    group
        .iter()
        .max_by_key(|delta| (delta.sync_id, delta.sequence))
        .copied()
}

fn select_filter<'a>(group: &[FilterDelta<'a>]) -> Option<FilterDelta<'a>> {
    group
        .iter()
        .max_by_key(|delta| (delta.sync_id, delta.sequence))
        .copied()
}

fn select_tenant(group: &[TenantDelta]) -> Option<TenantDelta> {
    group
        .iter()
        .max_by_key(|delta| (delta.sync_id, delta.sequence))
        .copied()
}

fn next_generation_id(
    previous: Option<&ProjectionManifest>,
    rows: &[&ProjectionSyncRow],
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

fn validate_ingestion_limits<'a>(
    rows: impl IntoIterator<Item = &'a ProjectionSyncRow>,
    limits: MutationBufferLimits,
) -> GraphResult<()> {
    let mut row_count = 0_usize;
    let mut requested_bytes = 0_usize;
    for row in rows {
        row_count = row_count
            .checked_add(1)
            .ok_or_else(|| GraphError::Internal("projection ingest row count overflowed".into()))?;
        if row_count > limits.max_rows {
            return Err(GraphError::OverlayLimit {
                kind: "projection_ingest_rows".to_string(),
                requested: row_count,
                limit: limits.max_rows,
            });
        }
        requested_bytes = requested_bytes
            .checked_add(INGEST_ROW_BYTES)
            .and_then(|total| {
                total.checked_add(
                    row.filter_value
                        .as_ref()
                        .map_or(0, PersistedFilterValue::heap_bytes),
                )
            })
            .and_then(|total| {
                total.checked_add(
                    row.relationship_identity
                        .as_ref()
                        .map_or(0, |identity| identity.source_key.len()),
                )
            })
            .and_then(|total| total.checked_add(row.primary_key.as_ref().map_or(0, String::len)))
            .and_then(|total| total.checked_add(row.tenant.as_ref().map_or(0, String::len)))
            .ok_or_else(|| {
                GraphError::Internal("projection ingest byte estimate overflowed".into())
            })?;
        if requested_bytes > limits.max_bytes {
            return Err(GraphError::OverlayLimit {
                kind: "projection_ingest_bytes".to_string(),
                requested: requested_bytes,
                limit: limits.max_bytes,
            });
        }
    }
    Ok(())
}

const INGEST_PREFLIGHT_FIXED_BYTES: usize = 1024 * 1024;
const INGEST_ROW_PEAK_MULTIPLIER: usize = 32;
const INGEST_ROW_FIXED_PEAK_BYTES: usize = 4 * 1024;
const INGEST_IDENTITY_FIXED_PEAK_BYTES: usize = 512;
const INGEST_IDENTITY_PEAK_COPIES: usize = 8;

/// Bound the simultaneously live ingestion workspace before constructing it.
///
/// The row term covers direction partitioning, normalization, node B-trees,
/// retained segment vectors, exact-capacity serialization, and decoded segment
/// validation. The identity term covers source-key clones in the cumulative
/// vector and hash map, sorting, encoding, and validation reload. Existing
/// manifest JSON receives a deliberately high expansion factor for decoded
/// strings, vectors, inherited-reference clones, and the next manifest.
fn ingestion_memory_upper_bound(
    root: &Path,
    rows: &[ProjectionSyncRow],
    base_relationship_identities: &[Option<RelationshipIdentity>],
) -> GraphResult<ByteCount> {
    let row_dynamic = rows.iter().try_fold(0usize, |total, row| {
        INGEST_ROW_BYTES
            .checked_add(
                row.filter_value
                    .as_ref()
                    .map_or(0, PersistedFilterValue::heap_bytes),
            )
            .and_then(|bytes| {
                bytes.checked_add(
                    row.relationship_identity
                        .as_ref()
                        .map_or(0, |identity| identity.source_key.len()),
                )
            })
            .and_then(|bytes| bytes.checked_add(row.primary_key.as_ref().map_or(0, String::len)))
            .and_then(|bytes| bytes.checked_add(row.tenant.as_ref().map_or(0, String::len)))
            .and_then(|bytes| total.checked_add(bytes))
            .ok_or_else(ingest_resource_size_overflow)
    })?;
    let row_peak = row_dynamic
        .checked_mul(INGEST_ROW_PEAK_MULTIPLIER)
        .and_then(|bytes| {
            rows.len()
                .checked_mul(INGEST_ROW_FIXED_PEAK_BYTES)
                .and_then(|fixed| bytes.checked_add(fixed))
        })
        .ok_or_else(ingest_resource_size_overflow)?;

    let identity_count = base_relationship_identities
        .len()
        .checked_add(
            rows.iter()
                .filter(|row| row.relationship_identity.is_some())
                .count(),
        )
        .ok_or_else(ingest_resource_size_overflow)?;
    let identity_strings = base_relationship_identities
        .iter()
        .filter_map(Option::as_ref)
        .chain(
            rows.iter()
                .filter_map(|row| row.relationship_identity.as_ref()),
        )
        .try_fold(0usize, |total, identity| {
            total
                .checked_add(identity.source_key.len())
                .ok_or_else(ingest_resource_size_overflow)
        })?;
    let identity_peak = identity_count
        .checked_mul(INGEST_IDENTITY_FIXED_PEAK_BYTES)
        .and_then(|fixed| fixed.checked_add(identity_strings))
        .and_then(|bytes| bytes.checked_mul(INGEST_IDENTITY_PEAK_COPIES))
        .ok_or_else(ingest_resource_size_overflow)?;

    let manifest_json_bytes = largest_manifest_file_bytes(root)?;
    let manifest_peak = manifest_json_bytes
        .checked_mul(MANIFEST_DECODED_MEMORY_BYTES_PER_JSON_BYTE)
        .ok_or_else(ingest_resource_size_overflow)?;
    let total = INGEST_PREFLIGHT_FIXED_BYTES
        .checked_add(row_peak)
        .and_then(|bytes| bytes.checked_add(identity_peak))
        .and_then(|bytes| bytes.checked_add(manifest_peak))
        .ok_or_else(ingest_resource_size_overflow)?;
    ByteCount::from_usize(total).ok_or_else(ingest_resource_size_overflow)
}

fn largest_manifest_file_bytes(root: &Path) -> GraphResult<usize> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(GraphError::Internal(format!(
                "read projection manifest directory during resource preflight: {err}"
            )))
        }
    };
    let mut largest = 0usize;
    for entry in entries {
        crate::resource::check_postgres_interrupts();
        let entry = entry.map_err(|err| {
            GraphError::Internal(format!(
                "read projection manifest entry during resource preflight: {err}"
            ))
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("projection-generation-") || !name.ends_with(".json") {
            continue;
        }
        let bytes = usize::try_from(
            entry
                .metadata()
                .map_err(|err| {
                    GraphError::Internal(format!(
                        "stat projection manifest during resource preflight: {err}"
                    ))
                })?
                .len(),
        )
        .map_err(|_| ingest_resource_size_overflow())?;
        largest = largest.max(bytes);
    }
    Ok(largest)
}

fn identity_artifact_encoded_len(
    dictionary: &RelationshipIdentityDictionary,
) -> GraphResult<usize> {
    let mut counter = CountingWriter::default();
    bincode::serde::encode_into_std_write(
        dictionary.identities(),
        &mut counter,
        bincode::config::standard(),
    )
    .map_err(|err| {
        GraphError::Internal(format!(
            "relationship identity resource preflight encoding failed: {err}"
        ))
    })?;
    32usize
        .checked_add(counter.bytes)
        .ok_or_else(ingest_resource_size_overflow)
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("encoded identity length overflowed"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn ingest_resource_size_overflow() -> GraphError {
    GraphError::ResourceLimit {
        resource: "memory".to_string(),
        phase: ResourcePhase::SyncIngest.as_str().to_string(),
        used: 0,
        requested: u64::MAX,
        limit: u64::MAX,
    }
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

fn cleanup_candidate_artifacts(
    root: &Path,
    segments: &[ManifestSegmentRef],
    identity: Option<&ManifestIdentityRef>,
) -> GraphResult<()> {
    cleanup_segment_refs(root, segments)?;
    let Some(identity) = identity else {
        return Ok(());
    };
    match std::fs::remove_file(root.join(&identity.path)) {
        Ok(()) => sync_directory(root),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(GraphError::Internal(format!(
            "remove unpublished projection identity dictionary: {err}"
        ))),
    }
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
    fn projection_ingest_edges_publish_dirty_source_ranges() {
        let dir = seeded_artifacts("projection_ingest_edges_publish_dirty_ranges");
        let ingester = ingester(&dir);
        let rows = vec![
            edge_row(1, 7, 8, None, MutationOperation::InsertEdge),
            edge_row(2, 8, 9, None, MutationOperation::DeleteEdge),
        ];

        let result = ingester
            .ingest_committed_rows(&rows, MutationBufferLimits::new(10, 10_000))
            .expect("ingestion publishes");
        let manifest = result.manifest.expect("manifest published");
        let segment = load_segment(&dir, &manifest.segments[0].path);

        assert_eq!(manifest.segments[0].source_start, 7);
        assert_eq!(manifest.segments[0].source_end, 9);
        assert_eq!(segment.header.source_start, 7);
        assert_eq!(segment.header.source_end, 9);
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
                relationship_identity: None,
                table_oid: Some(100),
                pk_hash: Some(crate::resolution_index::ResolutionIndexBuilder::hash_pk(
                    "node-2",
                )),
                primary_key: Some("node-2".to_string()),
                node_idx: Some(2),
                filter_column_id: Some(3),
                filter_value: Some(PersistedFilterValue::Numeric(88)),
                tenant_hash: Some(xxhash_rust::xxh3::xxh3_64(b"tenant-a")),
                tenant: Some("tenant-a".to_string()),
                schema_reversed: false,
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
                relationship_identity: None,
                table_oid: Some(100),
                pk_hash: Some(crate::resolution_index::ResolutionIndexBuilder::hash_pk(
                    "node-2",
                )),
                primary_key: Some("node-2".to_string()),
                node_idx: Some(2),
                filter_column_id: Some(3),
                filter_value: Some(PersistedFilterValue::Numeric(88)),
                tenant_hash: Some(xxhash_rust::xxh3::xxh3_64(b"tenant-a")),
                tenant: Some("tenant-a".to_string()),
                schema_reversed: false,
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
                relationship_identity: None,
                table_oid: Some(100),
                pk_hash: Some(crate::resolution_index::ResolutionIndexBuilder::hash_pk(
                    "node-4",
                )),
                primary_key: Some("node-4".to_string()),
                node_idx: Some(4),
                filter_column_id: Some(3),
                filter_value: Some(PersistedFilterValue::Numeric(99)),
                tenant_hash: Some(xxhash_rust::xxh3::xxh3_64(b"tenant-b")),
                tenant: Some("tenant-b".to_string()),
                schema_reversed: false,
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
                relationship_identity: None,
                table_oid: Some(101),
                pk_hash: Some(crate::resolution_index::ResolutionIndexBuilder::hash_pk(
                    "node-4",
                )),
                primary_key: Some("node-4".to_string()),
                node_idx: Some(4),
                filter_column_id: Some(4),
                filter_value: Some(PersistedFilterValue::Numeric(100)),
                tenant_hash: Some(xxhash_rust::xxh3::xxh3_64(b"tenant-c")),
                tenant: Some("tenant-c".to_string()),
                schema_reversed: false,
            },
        ];

        let result = ingester
            .ingest_committed_rows(&rows, MutationBufferLimits::new(10, 10_000))
            .expect("ingestion publishes");
        let manifest = result.manifest.expect("manifest published");

        assert_eq!(manifest.segments.len(), 2);
        let edge = load_segment(&dir, &manifest.segments[0].path);
        let node = load_segment(&dir, &manifest.segments[1].path);
        assert_eq!(edge.header.source_start, 0);
        assert_eq!(edge.header.source_end, 1);
        assert_eq!(node.header.source_start, 2);
        assert_eq!(node.header.source_end, 5);
        assert_eq!(edge.edge_weights[0].weight, 7);
        assert_eq!(edge.header.direction, TraversalDirection::Out);
        assert!(node
            .node_states
            .iter()
            .any(|row| row.node_idx == 2 && !row.active));
        assert!(node
            .node_states
            .iter()
            .any(|row| row.node_idx == 4 && row.active));
        assert!(node
            .resolutions
            .iter()
            .any(|row| row.primary_key.as_deref() == Some("node-4") && !row.tombstone));
        assert_eq!(
            node.filters
                .iter()
                .find(|row| row.node_idx == 4 && row.column_id == 3)
                .map(|row| &row.value),
            Some(&PersistedFilterValue::Numeric(99))
        );
        assert!(node
            .filters
            .iter()
            .any(|row| row.node_idx == 2 && row.tombstone));
        assert!(node
            .tenants
            .iter()
            .any(|row| row.tenant.as_deref() == Some("tenant-b") && !row.tombstone));
        assert_eq!(node.node_states.len(), 2);
        assert_eq!(node.resolutions.len(), 3);
        assert_eq!(node.filters.len(), 3);
        assert_eq!(node.tenants.len(), 3);
    }

    #[test]
    fn node_segment_keeps_latest_typed_filter_value_and_null() {
        let mut old = edge_row(7, 3, 3, None, MutationOperation::DeleteNode);
        old.direction = TraversalDirection::Any;
        old.node_idx = Some(3);
        old.filter_column_id = Some(4);
        old.filter_value = Some(PersistedFilterValue::Numeric(i64::MIN + 1));

        let mut updated = old.clone();
        updated.operation = MutationOperation::UpsertNode;
        updated.filter_value = Some(PersistedFilterValue::Numeric(i64::MAX));

        let mut cleared = updated.clone();
        cleared.filter_column_id = Some(5);
        cleared.filter_value = Some(PersistedFilterValue::Null);

        let rows = [old, updated, cleared];
        let rows = rows.iter().collect::<Vec<_>>();
        let segment = node_segment(&rows, MutationBufferLimits::new(10, 10_000), 7)
            .expect("node segment builds")
            .expect("node segment is present");

        assert_eq!(segment.filters.len(), 2);
        assert_eq!(
            segment
                .filters
                .iter()
                .find(|row| row.column_id == 4)
                .map(|row| (&row.value, row.tombstone)),
            Some((&PersistedFilterValue::Numeric(i64::MAX), false))
        );
        assert_eq!(
            segment
                .filters
                .iter()
                .find(|row| row.column_id == 5)
                .map(|row| (&row.value, row.tombstone)),
            Some((&PersistedFilterValue::Null, false))
        );
        assert!(
            segment.node_states.is_empty(),
            "filter-only rows must not be interpreted as node lifecycle deltas"
        );
    }

    #[test]
    fn node_segment_serializes_new_identities_in_allocation_order() {
        let mut lexical_last = edge_row(1, 0, 0, None, MutationOperation::UpsertNode);
        lexical_last.direction = TraversalDirection::Any;
        lexical_last.table_oid = Some(42);
        lexical_last.primary_key = Some("z-last".to_string());
        lexical_last.pk_hash = Some(crate::resolution_index::ResolutionIndexBuilder::hash_pk(
            "z-last",
        ));
        lexical_last.node_idx = Some(1);
        let mut lexical_first = lexical_last.clone();
        lexical_first.sync_id = 2;
        lexical_first.source = 2;
        lexical_first.target = 2;
        lexical_first.primary_key = Some("a-first".to_string());
        lexical_first.pk_hash = Some(crate::resolution_index::ResolutionIndexBuilder::hash_pk(
            "a-first",
        ));
        lexical_first.node_idx = Some(2);
        let rows = [&lexical_last, &lexical_first];

        let segment = node_segment(&rows, MutationBufferLimits::new(10, 10_000), 2)
            .expect("node segment builds")
            .expect("node segment exists");

        assert_eq!(
            segment
                .resolutions
                .iter()
                .map(|resolution| resolution.node_idx)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn filter_rows_do_not_tombstone_an_updated_node() {
        let mut deleted_node = edge_row(7, 3, 3, None, MutationOperation::DeleteNode);
        deleted_node.direction = TraversalDirection::Any;
        deleted_node.node_idx = Some(3);
        deleted_node.table_oid = Some(100);
        deleted_node.pk_hash = Some(7003);

        let mut upserted_node = deleted_node.clone();
        upserted_node.operation = MutationOperation::UpsertNode;

        let mut old_filter = deleted_node.clone();
        old_filter.table_oid = None;
        old_filter.pk_hash = None;
        old_filter.filter_column_id = Some(4);
        old_filter.filter_value = Some(PersistedFilterValue::Text("old".into()));

        let mut new_filter = old_filter.clone();
        new_filter.operation = MutationOperation::UpsertNode;
        new_filter.filter_value = Some(PersistedFilterValue::Text("new".into()));

        let rows = [deleted_node, upserted_node, old_filter, new_filter];
        let rows = rows.iter().collect::<Vec<_>>();
        let segment = node_segment(&rows, MutationBufferLimits::new(10, 10_000), 7)
            .expect("node segment builds")
            .expect("node segment is present");

        assert!(
            segment.node_states.is_empty(),
            "an UPDATE preserves node state"
        );
        assert_eq!(
            segment.filters,
            vec![SegmentFilterValue {
                node_idx: 3,
                column_id: 4,
                value: PersistedFilterValue::Text("new".into()),
                tombstone: false,
            }]
        );
    }

    #[test]
    fn projection_ingest_budget_counts_persisted_text_bytes() {
        let mut row = edge_row(1, 0, 0, None, MutationOperation::UpsertNode);
        row.direction = TraversalDirection::Any;
        row.node_idx = Some(0);
        row.filter_column_id = Some(0);
        row.filter_value = Some(PersistedFilterValue::Text("x".repeat(128)));
        let base_bytes = std::mem::size_of::<ProjectionSyncRow>();

        let err = validate_ingestion_limits(
            std::iter::once(&row),
            MutationBufferLimits::new(1, base_bytes + 127),
        )
        .expect_err("text heap bytes must count against ingest budget");

        assert!(matches!(err, GraphError::OverlayLimit { .. }));
    }

    #[test]
    fn governed_ingest_rejects_large_identity_before_publication() {
        let dir = seeded_artifacts("projection_governed_large_identity");
        let publisher = ingester(&dir);
        let mut row = edge_row(1, 0, 1, Some(7), MutationOperation::InsertEdge);
        row.relationship_identity = Some(RelationshipIdentity {
            mapping_id: 7,
            source_key: "identity".repeat(256 * 1024),
        });
        let governor = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(
                ByteCount::from_mib(2).expect("test memory budget fits"),
            )),
        );

        let error = publisher
            .ingest_committed_rows_with_identities_governed(
                &[row],
                MutationBufferLimits::new(2, 4 * 1024 * 1024),
                &[None],
                &governor,
                |_| Ok(()),
            )
            .expect_err("identity peak exceeds statement memory");

        assert!(matches!(&error, GraphError::ResourceLimit { .. }));
        assert_eq!(error.diagnostic_code().as_str(), "PG007");
        let latest = publisher
            .store
            .load_latest_current()
            .expect("current manifest remains readable")
            .expect("base generation remains current");
        assert_eq!(latest.generation_id, 1);
        assert_eq!(latest.sync_watermark, 0);
    }

    #[test]
    fn governed_ingest_rejects_large_inherited_manifest_before_clone() {
        let dir = seeded_artifacts("projection_governed_large_manifest");
        let mut inherited =
            ProjectionManifest::base_only(2, "base.pggraph", "crc32:00000000", 1, 0, 2);
        inherited.previous_generation_id = Some(1);
        inherited.obsolete_files = (0..256)
            .map(|index| ManifestFileRef {
                path: format!("obsolete-{index}-{}", "x".repeat(1024)),
                bytes: 1,
            })
            .collect();
        ProjectionManifestStore::new(dir.path())
            .publish(&inherited)
            .expect("large prior manifest publishes for the regression fixture");
        let publisher = ingester(&dir);
        let governor = crate::resource::ResourceGovernor::new(
            crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(
                ByteCount::from_mib(4).expect("test memory budget fits"),
            )),
        );

        let error = publisher
            .ingest_committed_rows_with_identities_governed(
                &[edge_row(1, 0, 1, None, MutationOperation::InsertEdge)],
                MutationBufferLimits::new(2, 64 * 1024),
                &[None],
                &governor,
                |_| Ok(()),
            )
            .expect_err("inherited manifest peak exceeds statement memory");

        assert!(matches!(&error, GraphError::ResourceLimit { .. }));
        assert_eq!(error.diagnostic_code().as_str(), "PG007");
        let latest = publisher
            .store
            .load_latest_current()
            .expect("current manifest remains readable")
            .expect("large prior generation remains current");
        assert_eq!(latest.generation_id, 2);
        assert_eq!(latest.sync_watermark, 0);
    }

    #[test]
    fn governed_ingest_rejects_artifact_disk_before_writing() {
        use std::time::Duration;

        let dir = seeded_artifacts("projection_governed_artifact_disk");
        let publisher = ingester(&dir);
        let governor =
            crate::resource::ResourceGovernor::new(crate::resource::ResourceLimits::bounded(
                crate::resource::MemoryBudget::new(
                    ByteCount::from_mib(32).expect("test memory budget fits"),
                ),
                crate::resource::DiskBudget::new(ByteCount::from_bytes(1)),
                crate::resource::RowCount::UNLIMITED,
                crate::resource::WorkUnits::UNLIMITED,
                crate::resource::ElapsedBudget::new(Duration::from_secs(60)),
            ));

        let error = publisher
            .ingest_committed_rows_with_identities_governed(
                &[edge_row(1, 0, 1, Some(7), MutationOperation::InsertEdge)],
                MutationBufferLimits::new(2, 64 * 1024),
                &[None],
                &governor,
                |_| Ok(()),
            )
            .expect_err("artifact exceeds disk allowance");

        assert!(matches!(&error, GraphError::ResourceLimit { .. }));
        assert_eq!(error.diagnostic_code().as_str(), "PG007");
        assert!(!dir.path().join(segment_file_name(2, 0)).exists());
        let latest = publisher
            .store
            .load_latest_current()
            .expect("current manifest remains readable")
            .expect("base generation remains current");
        assert_eq!(latest.generation_id, 1);
        assert_eq!(latest.sync_watermark, 0);
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
    fn candidate_validation_failure_keeps_previous_manifest_current() {
        let dir = seeded_artifacts("projection_candidate_validation_failure");
        let publisher = ingester(&dir);
        let first = publisher
            .ingest_committed_rows(
                &[edge_row(1, 0, 1, Some(7), MutationOperation::InsertEdge)],
                MutationBufferLimits::new(10, 10_000),
            )
            .expect("initial generation publishes")
            .manifest
            .expect("initial manifest exists");

        let error = publisher
            .ingest_committed_rows_with_identities_validated(
                &[edge_row(2, 1, 2, Some(7), MutationOperation::InsertEdge)],
                MutationBufferLimits::new(10, 10_000),
                &[None],
                |_| {
                    Err::<(), _>(GraphError::CorruptFile {
                        reason: "candidate semantic validation failed".to_string(),
                    })
                },
            )
            .expect_err("invalid candidate is not published");

        assert!(matches!(error, GraphError::CorruptFile { .. }));
        let current = publisher
            .store
            .load_latest_current()
            .expect("current manifest loads")
            .expect("previous manifest remains current");
        assert_eq!(current.generation_id, first.generation_id);
        assert_eq!(current.sync_watermark, first.sync_watermark);
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
        let first_segment = load_segment(&dir, &second.segments[0].path);
        let second_segment = load_segment(&dir, &second.segments[1].path);

        assert_eq!(second.previous_generation_id, Some(first.generation_id));
        assert!(second.generation_id > first.generation_id);
        assert_eq!(second.sync_watermark, 2);
        assert_eq!(second.segments.len(), 2);
        assert_eq!(second.segments[0], first.segments[0]);
        assert_eq!(first_segment.header.direction, TraversalDirection::Out);
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

    #[test]
    fn projection_ingest_persists_parallel_relationship_identity_dictionary() {
        let dir = seeded_artifacts("projection_ingest_parallel_relationship_identities");
        let ingester = ingester(&dir);
        let identity = |source_key: &str| RelationshipIdentity {
            mapping_id: 7,
            source_key: source_key.to_string(),
        };
        let mut first = edge_row(1, 0, 1, Some(11), MutationOperation::InsertEdge);
        first.relationship_identity = Some(identity("edge-a"));
        let mut second = edge_row(2, 0, 1, Some(13), MutationOperation::InsertEdge);
        second.relationship_identity = Some(identity("edge-b"));

        let result = ingester
            .ingest_committed_rows_with_identities(
                &[second, first],
                MutationBufferLimits::new(10, 10_000),
                &[None],
            )
            .expect("parallel identities publish");
        let manifest = result.manifest.clone().expect("manifest publishes");
        let identity_ref = manifest
            .relationship_identities
            .as_ref()
            .expect("identity dictionary is referenced");
        let dictionary =
            read_identity_artifact(&dir.path().join(&identity_ref.path), &identity_ref.checksum)
                .expect("identity dictionary reads");
        assert_eq!(
            dictionary.id_for(&identity("edge-a")).expect("edge-a id"),
            1
        );
        assert_eq!(
            dictionary.id_for(&identity("edge-b")).expect("edge-b id"),
            2
        );

        let segment = load_segment(&dir, &manifest.segments[0].path);
        assert_eq!(segment.edge_inserts.len(), 2);
        assert_eq!(
            segment
                .edge_inserts
                .iter()
                .map(|edge| edge.relationship_id)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2)]
        );
        assert_eq!(
            result.relationship_identities,
            Some(dictionary.identities().to_vec())
        );
    }

    #[test]
    fn projection_ingest_publishes_preinterned_identity_without_prior_dictionary() {
        let dir = seeded_artifacts("projection_ingest_preinterned_relationship_identity");
        let ingester = ingester(&dir);
        let identity = RelationshipIdentity {
            mapping_id: 7,
            source_key: "edge-a".to_string(),
        };
        let mut row = edge_row(1, 0, 1, None, MutationOperation::InsertEdge);
        row.relationship_identity = Some(identity.clone());

        let result = ingester
            .ingest_committed_rows_with_identities(
                &[row],
                MutationBufferLimits::new(10, 10_000),
                &[None, Some(identity.clone())],
            )
            .expect("preinterned identity publishes");
        let manifest = result.manifest.expect("manifest publishes");
        let reference = manifest
            .relationship_identities
            .expect("first relationship segment forces a durable dictionary");
        let dictionary =
            read_identity_artifact(&dir.path().join(reference.path), &reference.checksum)
                .expect("durable dictionary reads");

        assert_eq!(dictionary.id_for(&identity).expect("identity resolves"), 1);
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
            relationship_identity: None,
            table_oid: None,
            pk_hash: None,
            primary_key: None,
            node_idx: None,
            filter_column_id: None,
            filter_value: None,
            tenant_hash: None,
            tenant: None,
            schema_reversed: false,
        }
    }

    fn load_segment(dir: &ProjectionArtifactDir, relative_path: &str) -> DeltaSegment {
        DeltaSegment::read_from_path(&dir.path().join(relative_path)).expect("segment reads")
    }
}
