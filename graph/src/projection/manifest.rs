//! Durable projection manifest metadata and active-generation heartbeats.
//!
//! A projection manifest is the publication boundary for derived graph
//! artifacts. Readers load a complete generation from a validated manifest
//! instead of discovering segment files directly.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::safety::{GraphError, GraphResult};

/// Current JSON manifest format version.
pub(crate) const MANIFEST_VERSION: u32 = 2;
/// Validation state for a generation whose artifacts are ready to read.
pub(crate) const VALIDATION_STATUS_VALID: &str = "valid";
/// Validation state for a generation that has been marked corrupt.
pub(crate) const VALIDATION_STATUS_CORRUPT: &str = "corrupt";
/// Validation state for a generation that is being repaired.
pub(crate) const VALIDATION_STATUS_REPAIRING: &str = "repairing";
/// Default TTL for backend active-generation heartbeat rows.
pub(crate) const DEFAULT_ACTIVE_GENERATION_TTL: Duration = Duration::from_secs(300);
/// Conservative owned decode workspace per persisted manifest JSON byte.
///
/// This covers serde map/vector structure, owned strings, collection growth,
/// and allocator rounding when a manifest is decoded into Rust-owned state.
pub(crate) const MANIFEST_DECODED_MEMORY_BYTES_PER_JSON_BYTE: usize = 128;

const MANIFEST_FILE_PREFIX: &str = "projection-generation-";
const MANIFEST_FILE_SUFFIX: &str = ".json";
const CURRENT_POINTER_FILE: &str = "projection-current.json";
const PUBLICATION_LOCK_FILE: &str = ".projection-publication.lock";
const CURRENT_POINTER_VERSION: u32 = 1;
const MAX_CURRENT_POINTER_BYTES: usize = 4 * 1024;
const GOVERNED_MANIFEST_IO_WORKSPACE_BYTES: usize = 16 * 1024;

#[cfg(test)]
thread_local! {
    static FAIL_AFTER_POINTER_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Durable pointer to the one projection generation readers should serve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionCurrentPointer {
    version: u32,
    generation_id: u64,
    manifest_checksum: String,
}

/// Human-readable manifest for one durable projection generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionManifest {
    /// Manifest format version.
    pub(crate) version: u32,
    /// Monotonic projection generation identifier.
    pub(crate) generation_id: u64,
    /// Previous generation when this manifest replaces another generation.
    pub(crate) previous_generation_id: Option<u64>,
    /// Base `.pggraph` artifact path used by this generation.
    pub(crate) base_artifact_path: String,
    /// Hex or algorithm-qualified checksum for the base artifact.
    pub(crate) base_artifact_checksum: String,
    /// Base `.pggraph` file-format version.
    pub(crate) base_artifact_version: u32,
    /// Durable segment files layered over the base artifact.
    pub(crate) segments: Vec<ManifestSegmentRef>,
    /// Cumulative post-build relationship identity dictionary artifact.
    #[serde(default)]
    pub(crate) relationship_identities: Option<ManifestIdentityRef>,
    /// Base chunks that are active for this generation.
    pub(crate) base_chunks: Vec<ManifestChunkRef>,
    /// Files that became obsolete when this generation was published.
    pub(crate) obsolete_files: Vec<ManifestFileRef>,
    /// Highest durable sync-log row represented by this generation.
    pub(crate) sync_watermark: i64,
    /// Current validation status for the manifest and referenced files.
    pub(crate) validation_status: String,
    /// Manifest creation timestamp as Unix microseconds.
    pub(crate) created_at_unix_micros: i64,
    /// Last ingestion publication timestamp carried forward across generations.
    #[serde(default)]
    pub(crate) last_ingestion_unix_micros: Option<i64>,
    /// Last compaction publication timestamp carried forward across generations.
    #[serde(default)]
    pub(crate) last_compaction_unix_micros: Option<i64>,
    /// Last repair publication timestamp carried forward across generations.
    #[serde(default)]
    pub(crate) last_repair_unix_micros: Option<i64>,
}

impl ProjectionManifest {
    /// Construct a base-only manifest for tests and initial engine loading.
    pub(crate) fn base_only(
        generation_id: u64,
        base_artifact_path: impl Into<String>,
        base_artifact_checksum: impl Into<String>,
        base_artifact_version: u32,
        sync_watermark: i64,
        created_at_unix_micros: i64,
    ) -> Self {
        Self {
            version: MANIFEST_VERSION,
            generation_id,
            previous_generation_id: None,
            base_artifact_path: base_artifact_path.into(),
            base_artifact_checksum: base_artifact_checksum.into(),
            base_artifact_version,
            segments: Vec::new(),
            relationship_identities: None,
            base_chunks: Vec::new(),
            obsolete_files: Vec::new(),
            sync_watermark,
            validation_status: VALIDATION_STATUS_VALID.to_string(),
            created_at_unix_micros,
            last_ingestion_unix_micros: None,
            last_compaction_unix_micros: None,
            last_repair_unix_micros: None,
        }
    }

    /// Carry forward persisted operation timestamps from the previous manifest.
    pub(crate) fn inherit_operation_timestamps(&mut self, previous: &Self) {
        self.last_ingestion_unix_micros = previous.last_ingestion_unix_micros;
        self.last_compaction_unix_micros = previous.last_compaction_unix_micros;
        self.last_repair_unix_micros = previous.last_repair_unix_micros;
    }

    /// Mark this generation as the result of an ingestion publication.
    pub(crate) fn mark_ingestion(&mut self) {
        self.last_ingestion_unix_micros = Some(self.created_at_unix_micros);
    }

    /// Mark this generation as the result of a compaction publication.
    pub(crate) fn mark_compaction(&mut self) {
        self.last_compaction_unix_micros = Some(self.created_at_unix_micros);
    }

    /// Mark this generation as the result of a repair publication.
    pub(crate) fn mark_repair(&mut self) {
        self.last_repair_unix_micros = Some(self.created_at_unix_micros);
    }

    /// Validate required semantic fields after JSON decoding.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::IncompatibleVersion`] for unsupported manifest
    /// versions. Returns [`GraphError::CorruptFile`] when required string
    /// fields are empty, watermarks are negative, or child references are
    /// incomplete.
    pub(crate) fn validate(&self) -> GraphResult<()> {
        if self.version != MANIFEST_VERSION {
            return Err(GraphError::IncompatibleVersion(format!(
                "projection manifest version {} is unsupported; expected {}",
                self.version, MANIFEST_VERSION
            )));
        }
        if self.generation_id == 0 {
            return Err(manifest_corrupt("generation_id must be positive"));
        }
        if self.base_artifact_path.trim().is_empty() {
            return Err(manifest_corrupt("base_artifact_path is required"));
        }
        if self.base_artifact_checksum.trim().is_empty() {
            return Err(manifest_corrupt("base_artifact_checksum is required"));
        }
        if self.sync_watermark < 0 {
            return Err(manifest_corrupt("sync_watermark must be nonnegative"));
        }
        validate_status(&self.validation_status)?;
        for segment in &self.segments {
            segment.validate()?;
        }
        if let Some(identities) = &self.relationship_identities {
            identities.validate()?;
        }
        for chunk in &self.base_chunks {
            chunk.validate()?;
        }
        for file in &self.obsolete_files {
            file.validate()?;
        }
        Ok(())
    }

    /// Encode this manifest as pretty JSON after validation.
    ///
    /// # Errors
    ///
    /// Returns validation errors from [`ProjectionManifest::validate`] before
    /// encoding. Returns [`GraphError::Internal`] if JSON encoding fails.
    pub(crate) fn to_pretty_json(&self) -> GraphResult<String> {
        self.validate()?;
        serde_json::to_string_pretty(self)
            .map_err(|err| GraphError::Internal(format!("manifest encoding failed: {err}")))
    }

    /// Decode a manifest from JSON and validate its semantic fields.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] when the JSON is malformed or
    /// required fields are missing. Returns validation errors from
    /// [`ProjectionManifest::validate`] for unsupported versions or incomplete
    /// references.
    pub(crate) fn from_json(raw: &str) -> GraphResult<Self> {
        let manifest = serde_json::from_str::<Self>(raw)
            .map_err(|err| manifest_corrupt(format!("manifest JSON decoding failed: {err}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Return the heap storage retained by this manifest.
    ///
    /// The estimate uses collection capacities, not lengths, so callers can
    /// keep a reservation live for the complete owned manifest allocation.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::ResourceLimit`] if the checked size calculation
    /// overflows the platform address space.
    pub(crate) fn estimated_heap_bytes(&self) -> GraphResult<usize> {
        let mut bytes = 0usize;
        for string in [
            &self.base_artifact_path,
            &self.base_artifact_checksum,
            &self.validation_status,
        ] {
            bytes = checked_manifest_bytes(bytes, string.capacity())?;
        }
        bytes = checked_manifest_bytes(
            bytes,
            self.segments
                .capacity()
                .checked_mul(std::mem::size_of::<ManifestSegmentRef>())
                .ok_or_else(manifest_memory_overflow)?,
        )?;
        for reference in &self.segments {
            bytes = checked_manifest_bytes(bytes, reference.path.capacity())?;
            bytes = checked_manifest_bytes(bytes, reference.checksum.capacity())?;
        }
        bytes = checked_manifest_bytes(
            bytes,
            self.base_chunks
                .capacity()
                .checked_mul(std::mem::size_of::<ManifestChunkRef>())
                .ok_or_else(manifest_memory_overflow)?,
        )?;
        for reference in &self.base_chunks {
            bytes = checked_manifest_bytes(bytes, reference.path.capacity())?;
            bytes = checked_manifest_bytes(bytes, reference.checksum.capacity())?;
        }
        bytes = checked_manifest_bytes(
            bytes,
            self.obsolete_files
                .capacity()
                .checked_mul(std::mem::size_of::<ManifestFileRef>())
                .ok_or_else(manifest_memory_overflow)?,
        )?;
        for reference in &self.obsolete_files {
            bytes = checked_manifest_bytes(bytes, reference.path.capacity())?;
        }
        if let Some(reference) = &self.relationship_identities {
            bytes = checked_manifest_bytes(bytes, reference.path.capacity())?;
            bytes = checked_manifest_bytes(bytes, reference.checksum.capacity())?;
        }
        Ok(bytes)
    }

    /// Whether this generation references only the base `.pggraph` artifact.
    pub(crate) fn is_base_only(&self) -> bool {
        self.segments.is_empty() && self.base_chunks.is_empty()
    }
}

/// Relationship identity dictionary reference stored in a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestIdentityRef {
    /// Dictionary path relative to the projection artifact directory.
    pub(crate) path: String,
    /// CRC32 checksum of the complete dictionary artifact.
    pub(crate) checksum: String,
    /// Number of dictionary slots, including reserved slot zero.
    pub(crate) entry_count: u32,
    /// Artifact byte size used by status and garbage collection accounting.
    pub(crate) bytes: u64,
}

impl ManifestIdentityRef {
    fn validate(&self) -> GraphResult<()> {
        if self.path.trim().is_empty() {
            return Err(manifest_corrupt("relationship identity path is required"));
        }
        if self.checksum.trim().is_empty() {
            return Err(manifest_corrupt(
                "relationship identity checksum is required",
            ));
        }
        if self.entry_count == 0 {
            return Err(manifest_corrupt(
                "relationship identity dictionary must contain reserved slot zero",
            ));
        }
        Ok(())
    }
}

/// Filesystem store for durable projection manifest generations.
#[derive(Debug, Clone)]
pub(crate) struct ProjectionManifestStore {
    root: PathBuf,
}

impl ProjectionManifestStore {
    /// Create a manifest store rooted at `root`.
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Return the final manifest path for `generation_id`.
    pub(crate) fn manifest_path(&self, generation_id: u64) -> PathBuf {
        self.root.join(manifest_file_name(generation_id))
    }

    fn current_pointer_path(&self) -> PathBuf {
        self.root.join(CURRENT_POINTER_FILE)
    }

    /// Atomically publish a validated manifest to its generation path.
    ///
    /// # Errors
    ///
    /// Returns validation errors when the manifest is malformed or references
    /// missing active artifacts. Returns [`GraphError::Internal`] when durable
    /// filesystem operations fail.
    pub(crate) fn publish(&self, manifest: &ProjectionManifest) -> GraphResult<PathBuf> {
        let expected_current = self.current_generation_id()?;
        self.publish_if_current(manifest, expected_current)
    }

    /// Publish `manifest` only when `expected_current` still serves readers.
    ///
    /// The generation manifest is immutable and durable before the current
    /// pointer changes. A failed comparison leaves the serving pointer and all
    /// prior generations unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::BuildLocked`] when another publisher changed the
    /// current generation after the caller planned its candidate. Returns the
    /// validation and filesystem errors documented by
    /// [`ProjectionManifestStore::publish`].
    pub(crate) fn publish_if_current(
        &self,
        manifest: &ProjectionManifest,
        expected_current: Option<u64>,
    ) -> GraphResult<PathBuf> {
        manifest.validate()?;
        self.validate_active_references(manifest)?;

        fs::create_dir_all(&self.root)
            .map_err(|err| manifest_io("create manifest directory", &self.root, err))?;
        let _publication_lock = self.acquire_publication_lock()?;
        self.prepare_current_pointer(expected_current)?;
        let json = manifest.to_pretty_json()?;
        let final_path = self.stage_manifest_file(manifest, json.as_bytes())?;
        if let Err(err) = self.switch_current_generation(manifest.generation_id, expected_current) {
            self.remove_uncommitted_manifest(&final_path, manifest.generation_id);
            return Err(err);
        }

        Ok(final_path)
    }

    /// Publish an authoritative rebuild over a damaged current pointer.
    ///
    /// Recovery compares the raw pointer bytes instead of decoding the pointer,
    /// so corrupt metadata stays in place until a validated replacement is
    /// durable. The caller must hold the graph publication lock.
    pub(crate) fn publish_for_recovery(
        &self,
        manifest: &ProjectionManifest,
    ) -> GraphResult<PathBuf> {
        manifest.validate()?;
        self.validate_active_references(manifest)?;
        fs::create_dir_all(&self.root)
            .map_err(|err| manifest_io("create manifest directory", &self.root, err))?;
        let _publication_lock = self.acquire_publication_lock()?;
        let expected_pointer_token = self.current_pointer_token()?;
        let json = manifest.to_pretty_json()?;
        let final_path = self.stage_manifest_file(manifest, json.as_bytes())?;
        if let Err(err) =
            self.switch_recovery_generation(manifest.generation_id, expected_pointer_token)
        {
            self.remove_uncommitted_manifest(&final_path, manifest.generation_id);
            return Err(err);
        }
        Ok(final_path)
    }

    fn stage_manifest_file(
        &self,
        manifest: &ProjectionManifest,
        encoded: &[u8],
    ) -> GraphResult<PathBuf> {
        let final_path = self.manifest_path(manifest.generation_id);
        if final_path.exists() {
            return Err(manifest_corrupt(format!(
                "generation {} already has a published manifest",
                manifest.generation_id
            )));
        }
        let (tmp_path, mut file) = self.create_temp_manifest_file(manifest.generation_id)?;

        file.write_all(encoded)
            .map_err(|err| manifest_io("write temp manifest", &tmp_path, err))?;
        file.sync_all()
            .map_err(|err| manifest_io("fsync temp manifest", &tmp_path, err))?;
        drop(file);
        sync_directory(&self.root)?;
        if let Err(err) = fs::rename(&tmp_path, &final_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(manifest_io("rename manifest into place", &final_path, err));
        }
        sync_directory(&self.root)?;
        let published = self.load_manifest_file(&final_path)?;
        if published.generation_id != manifest.generation_id {
            return Err(manifest_corrupt(format!(
                "published generation {} reloaded as generation {}",
                manifest.generation_id, published.generation_id
            )));
        }
        self.validate_active_references(&published)?;
        Ok(final_path)
    }

    /// Atomically publish a manifest without materializing JSON or a reload
    /// copy in memory.
    ///
    /// JSON is counted before staging, streamed directly to the temporary
    /// file, and compared byte-for-byte with the published file. The memory
    /// and staging-disk leases remain charged until validation completes.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::ResourceLimit`] before staging when the governed
    /// workspace or encoded manifest exceeds its budget. Returns the same
    /// validation and filesystem errors as [`ProjectionManifestStore::publish`].
    pub(crate) fn publish_governed(
        &self,
        manifest: &ProjectionManifest,
        governor: &crate::resource::ResourceGovernor,
        phase: crate::resource::ResourcePhase,
        max_memory_bytes: usize,
    ) -> GraphResult<PathBuf> {
        let expected_current = self.current_generation_id()?;
        self.publish_governed_if_current(
            manifest,
            expected_current,
            governor,
            phase,
            max_memory_bytes,
        )
    }

    /// Governed form of [`ProjectionManifestStore::publish_if_current`].
    pub(crate) fn publish_governed_if_current(
        &self,
        manifest: &ProjectionManifest,
        expected_current: Option<u64>,
        governor: &crate::resource::ResourceGovernor,
        phase: crate::resource::ResourcePhase,
        max_memory_bytes: usize,
    ) -> GraphResult<PathBuf> {
        manifest.validate()?;
        let encoded_len = manifest_pretty_json_len(manifest, phase)?;
        let used = governor.memory_used().as_u64().min(usize::MAX as u64) as usize;
        let next = used
            .checked_add(GOVERNED_MANIFEST_IO_WORKSPACE_BYTES)
            .ok_or_else(|| manifest_resource_limit(phase, used, usize::MAX, max_memory_bytes))?;
        if next > max_memory_bytes {
            return Err(manifest_resource_limit(
                phase,
                used,
                GOVERNED_MANIFEST_IO_WORKSPACE_BYTES,
                max_memory_bytes,
            ));
        }
        let workspace =
            crate::resource::ByteCount::from_usize(GOVERNED_MANIFEST_IO_WORKSPACE_BYTES)
                .ok_or_else(|| {
                    manifest_resource_limit(phase, used, usize::MAX, max_memory_bytes)
                })?;
        let _workspace = governor
            .reserve_memory(phase, workspace)
            .map_err(crate::safety::resource_limit_error)?;
        let encoded_bytes = crate::resource::ByteCount::from_usize(encoded_len)
            .ok_or_else(|| manifest_resource_limit(phase, 0, usize::MAX, usize::MAX))?;
        let _staging_disk = governor
            .reserve_disk(phase, encoded_bytes)
            .map_err(crate::safety::resource_limit_error)?;

        self.validate_active_references(manifest)?;
        fs::create_dir_all(&self.root)
            .map_err(|err| manifest_io("create manifest directory", &self.root, err))?;
        let _publication_lock = self.acquire_publication_lock()?;
        self.prepare_current_pointer(expected_current)?;
        let final_path = self.manifest_path(manifest.generation_id);
        if final_path.exists() {
            return Err(manifest_corrupt(format!(
                "generation {} already has a published manifest",
                manifest.generation_id
            )));
        }
        let (tmp_path, mut file) = self.create_temp_manifest_file(manifest.generation_id)?;
        let mut renamed = false;
        let publication = (|| {
            serde_json::to_writer_pretty(&mut file, manifest)
                .map_err(|err| GraphError::Internal(format!("manifest encoding failed: {err}")))?;
            file.sync_all()
                .map_err(|err| manifest_io("fsync temp manifest", &tmp_path, err))?;
            drop(file);
            sync_directory(&self.root)?;
            fs::rename(&tmp_path, &final_path)
                .map_err(|err| manifest_io("rename manifest into place", &final_path, err))?;
            renamed = true;
            sync_directory(&self.root)?;
            verify_manifest_file_matches(&final_path, manifest, encoded_len)?;
            self.validate_active_references(manifest)?;
            self.switch_current_generation(manifest.generation_id, expected_current)
        })();
        if publication.is_err() {
            let _ = fs::remove_file(&tmp_path);
            if renamed {
                self.remove_uncommitted_manifest(&final_path, manifest.generation_id);
            }
        }
        publication?;
        Ok(final_path)
    }

    /// Load the highest-generation valid manifest in this store.
    ///
    /// Unreferenced temporary and unrelated files are ignored. The selected
    /// manifest must validate and reference existing active artifacts.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] for malformed selected manifests or
    /// missing active references. Returns [`GraphError::Internal`] for
    /// directory read failures other than a missing store directory.
    pub(crate) fn load_latest_current(&self) -> GraphResult<Option<ProjectionManifest>> {
        let Some((generation_id, path)) = self.current_manifest_path()? else {
            return Ok(None);
        };
        let manifest = self.load_manifest_file(&path)?;
        if manifest.generation_id != generation_id {
            return Err(manifest_corrupt(format!(
                "manifest filename generation {generation_id} does not match JSON generation {}",
                manifest.generation_id
            )));
        }
        self.validate_active_references(&manifest)?;
        Ok(Some(manifest))
    }

    /// Load the latest active manifest while allowing missing base chunks.
    ///
    /// Recovery uses this path because chunk metadata can be sufficient to
    /// rebuild missing or corrupt chunk files from the source base graph.
    pub(crate) fn load_latest_current_for_recovery(
        &self,
    ) -> GraphResult<Option<ProjectionManifest>> {
        let Some((generation_id, path)) = self.current_manifest_path()? else {
            return Ok(None);
        };
        let manifest = self.load_manifest_file(&path)?;
        if manifest.generation_id != generation_id {
            return Err(manifest_corrupt(format!(
                "manifest filename generation {generation_id} does not match JSON generation {}",
                manifest.generation_id
            )));
        }
        self.validate_active_references_for_recovery(&manifest)?;
        Ok(Some(manifest))
    }

    /// Load the latest manifest metadata without validating referenced files.
    ///
    /// A full PostgreSQL rebuild uses this to retain generation linkage and
    /// obsolete-file accounting even when a referenced segment or chunk is the
    /// reason the current generation must be replaced.
    pub(crate) fn load_latest_metadata(&self) -> GraphResult<Option<ProjectionManifest>> {
        let Some((generation_id, path)) = self.current_manifest_path()? else {
            return Ok(None);
        };
        let manifest = self.load_manifest_file(&path)?;
        if manifest.generation_id != generation_id {
            return Err(manifest_corrupt(format!(
                "manifest filename generation {generation_id} does not match JSON generation {}",
                manifest.generation_id
            )));
        }
        Ok(Some(manifest))
    }

    fn load_manifest_file(&self, path: &Path) -> GraphResult<ProjectionManifest> {
        let raw =
            fs::read_to_string(path).map_err(|err| manifest_io("read manifest", path, err))?;
        ProjectionManifest::from_json(&raw)
    }

    pub(crate) fn current_generation_id(&self) -> GraphResult<Option<u64>> {
        if let Some(pointer) = self.load_current_pointer()? {
            return Ok(Some(pointer.generation_id));
        }
        self.latest_manifest_path()
            .map(|latest| latest.map(|(generation_id, _)| generation_id))
    }

    fn current_manifest_path(&self) -> GraphResult<Option<(u64, PathBuf)>> {
        let Some(generation_id) = self.current_generation_id()? else {
            return Ok(None);
        };
        Ok(Some((generation_id, self.manifest_path(generation_id))))
    }

    fn load_current_pointer(&self) -> GraphResult<Option<ProjectionCurrentPointer>> {
        let path = self.current_pointer_path();
        let Some(raw) =
            read_bounded_optional_file(&path, MAX_CURRENT_POINTER_BYTES, "read current pointer")?
        else {
            return Ok(None);
        };
        let pointer = serde_json::from_slice::<ProjectionCurrentPointer>(&raw)
            .map_err(|err| manifest_corrupt(format!("current pointer decoding failed: {err}")))?;
        if pointer.version != CURRENT_POINTER_VERSION || pointer.generation_id == 0 {
            return Err(manifest_corrupt(format!(
                "current pointer is invalid: version={}, generation_id={}",
                pointer.version, pointer.generation_id
            )));
        }
        if !self.manifest_path(pointer.generation_id).is_file() {
            return Err(manifest_corrupt(format!(
                "current pointer references missing generation {}",
                pointer.generation_id
            )));
        }
        let manifest_path = self.manifest_path(pointer.generation_id);
        let actual_checksum = manifest_checksum_for_path(&manifest_path)?;
        if pointer.manifest_checksum != actual_checksum {
            return Err(manifest_corrupt(format!(
                "current pointer checksum mismatch for generation {}",
                pointer.generation_id
            )));
        }
        Ok(Some(pointer))
    }

    fn prepare_current_pointer(&self, expected_current: Option<u64>) -> GraphResult<()> {
        let direct_current = self
            .load_current_pointer()?
            .map(|pointer| pointer.generation_id);
        if direct_current.is_none() {
            if let Some(generation_id) = expected_current {
                self.write_current_pointer(generation_id)?;
            }
        }
        let actual = self
            .load_current_pointer()?
            .map(|pointer| pointer.generation_id);
        if actual == expected_current {
            Ok(())
        } else {
            Err(GraphError::BuildLocked)
        }
    }

    fn switch_current_generation(
        &self,
        generation_id: u64,
        expected_current: Option<u64>,
    ) -> GraphResult<()> {
        let actual = self
            .load_current_pointer()?
            .map(|pointer| pointer.generation_id);
        if actual != expected_current {
            return Err(GraphError::BuildLocked);
        }
        self.write_current_pointer(generation_id)
    }

    fn current_pointer_token(&self) -> GraphResult<Option<u32>> {
        let path = self.current_pointer_path();
        read_bounded_optional_file(
            &path,
            MAX_CURRENT_POINTER_BYTES,
            "read current pointer token",
        )
        .map(|raw| raw.map(|bytes| crc32fast::hash(&bytes)))
    }

    fn switch_recovery_generation(
        &self,
        generation_id: u64,
        expected_pointer_token: Option<u32>,
    ) -> GraphResult<()> {
        if self.current_pointer_token()? != expected_pointer_token {
            return Err(GraphError::BuildLocked);
        }
        self.write_current_pointer(generation_id)
    }

    /// Hold the cross-process exclusive artifact publication lock.
    pub(crate) fn acquire_publication_lock(&self) -> GraphResult<File> {
        self.acquire_file_lock(false)
    }

    /// Hold a shared artifact-generation lock while loading referenced files.
    pub(crate) fn acquire_reader_lock(&self) -> GraphResult<File> {
        self.acquire_file_lock(true)
    }

    fn acquire_file_lock(&self, shared: bool) -> GraphResult<File> {
        fs::create_dir_all(&self.root)
            .map_err(|err| manifest_io("create manifest directory", &self.root, err))?;
        let path = self.root.join(PUBLICATION_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| manifest_io("open publication lock", &path, err))?;
        if shared {
            file.lock_shared()
                .map_err(|err| manifest_io("acquire generation reader lock", &path, err))?;
        } else {
            file.lock()
                .map_err(|err| manifest_io("acquire publication lock", &path, err))?;
        }
        Ok(file)
    }

    fn remove_uncommitted_manifest(&self, path: &Path, generation_id: u64) {
        let candidate_is_proven_not_current = match self.load_current_pointer() {
            Ok(None) => true,
            Ok(Some(pointer)) => pointer.generation_id != generation_id,
            Err(_) => false,
        };
        if candidate_is_proven_not_current {
            let _ = fs::remove_file(path);
            let _ = sync_directory(&self.root);
        }
    }

    fn write_current_pointer(&self, generation_id: u64) -> GraphResult<()> {
        let manifest_path = self.manifest_path(generation_id);
        let pointer = ProjectionCurrentPointer {
            version: CURRENT_POINTER_VERSION,
            generation_id,
            manifest_checksum: manifest_checksum_for_path(&manifest_path)?,
        };
        let encoded = serde_json::to_vec_pretty(&pointer).map_err(|err| {
            GraphError::Internal(format!("current pointer encoding failed: {err}"))
        })?;
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?
            .as_nanos();
        let tmp_path = self.root.join(format!(
            "{CURRENT_POINTER_FILE}.tmp-{}-{created_at}",
            std::process::id()
        ));
        let final_path = self.current_pointer_path();
        let publication = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)
                .map_err(|err| manifest_io("create current pointer", &tmp_path, err))?;
            file.write_all(&encoded)
                .map_err(|err| manifest_io("write current pointer", &tmp_path, err))?;
            file.sync_all()
                .map_err(|err| manifest_io("fsync current pointer", &tmp_path, err))?;
            drop(file);
            sync_directory(&self.root)?;
            fs::rename(&tmp_path, &final_path)
                .map_err(|err| manifest_io("switch current pointer", &final_path, err))?;
            #[cfg(test)]
            if FAIL_AFTER_POINTER_RENAME.with(|fail| fail.replace(false)) {
                return Err(GraphError::Internal(
                    "injected current-pointer failure after rename".into(),
                ));
            }
            sync_directory(&self.root)
        })();
        if publication.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        publication
    }

    fn latest_manifest_path(&self) -> GraphResult<Option<(u64, PathBuf)>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(manifest_io("read manifest directory", &self.root, err)),
        };
        let mut latest = None;
        for entry in entries {
            let entry = entry
                .map_err(|err| manifest_io("read manifest directory entry", &self.root, err))?;
            if !entry
                .file_type()
                .map_err(|err| manifest_io("read manifest file type", &entry.path(), err))?
                .is_file()
            {
                continue;
            }
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(generation_id) = parse_manifest_file_name(&file_name) else {
                continue;
            };
            if latest
                .as_ref()
                .is_none_or(|(current_generation, _)| generation_id > *current_generation)
            {
                latest = Some((generation_id, entry.path()));
            }
        }
        Ok(latest)
    }

    fn validate_active_references(&self, manifest: &ProjectionManifest) -> GraphResult<()> {
        require_existing_reference(&self.root, &manifest.base_artifact_path, "base artifact")?;
        if let Some(identities) = &manifest.relationship_identities {
            require_existing_reference(
                &self.root,
                &identities.path,
                "relationship identity dictionary",
            )?;
        }
        for segment in &manifest.segments {
            require_existing_reference(&self.root, &segment.path, "segment")?;
        }
        for chunk in &manifest.base_chunks {
            require_existing_reference(&self.root, &chunk.path, "base chunk")?;
        }
        Ok(())
    }

    fn validate_active_references_for_recovery(
        &self,
        manifest: &ProjectionManifest,
    ) -> GraphResult<()> {
        require_existing_reference(&self.root, &manifest.base_artifact_path, "base artifact")?;
        if let Some(identities) = &manifest.relationship_identities {
            require_existing_reference(
                &self.root,
                &identities.path,
                "relationship identity dictionary",
            )?;
        }
        for segment in &manifest.segments {
            require_existing_reference(&self.root, &segment.path, "segment")?;
        }
        Ok(())
    }

    fn create_temp_manifest_file(&self, generation_id: u64) -> GraphResult<(PathBuf, File)> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?
            .as_nanos();
        for attempt in 0..128 {
            let path = self.root.join(format!(
                "{}{generation_id:020}.tmp-{}-{created_at}-{attempt}",
                MANIFEST_FILE_PREFIX,
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(manifest_io("create temp manifest", &path, err)),
            }
        }
        Err(GraphError::Internal(
            "projection manifest temp path kept colliding".into(),
        ))
    }
}

fn manifest_checksum_for_path(path: &Path) -> GraphResult<String> {
    let mut file =
        File::open(path).map_err(|err| manifest_io("open current manifest checksum", path, err))?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = [0u8; GOVERNED_MANIFEST_IO_WORKSPACE_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| manifest_io("read current manifest checksum", path, err))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("crc32:{:08x}", hasher.finalize()))
}

fn read_bounded_optional_file(
    path: &Path,
    max_bytes: usize,
    operation: &str,
) -> GraphResult<Option<Vec<u8>>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(manifest_io(operation, path, err)),
    };
    let read_limit = u64::try_from(max_bytes)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or_else(|| GraphError::Internal("current pointer byte limit overflowed".into()))?;
    let mut bytes = Vec::with_capacity(max_bytes.min(256));
    Read::by_ref(&mut file)
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|err| manifest_io(operation, path, err))?;
    if bytes.len() > max_bytes {
        return Err(manifest_corrupt(format!(
            "current pointer exceeds {max_bytes} bytes"
        )));
    }
    Ok(Some(bytes))
}

/// Segment file reference stored in a projection manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestSegmentRef {
    /// Segment file path relative to the projection artifact directory.
    pub(crate) path: String,
    /// Segment file checksum.
    pub(crate) checksum: String,
    /// Segment level, where L0 is the newest un-compacted level.
    pub(crate) level: u8,
    /// Inclusive source-node range start covered by the segment.
    pub(crate) source_start: u32,
    /// Exclusive source-node range end covered by the segment.
    pub(crate) source_end: u32,
    /// Highest sync-log row represented by the segment.
    pub(crate) sync_watermark: i64,
}

impl ManifestSegmentRef {
    fn validate(&self) -> GraphResult<()> {
        if self.path.trim().is_empty() {
            return Err(manifest_corrupt("segment path is required"));
        }
        if self.checksum.trim().is_empty() {
            return Err(manifest_corrupt("segment checksum is required"));
        }
        if self.source_start > self.source_end {
            return Err(manifest_corrupt(
                "segment source_start must not exceed source_end",
            ));
        }
        if self.sync_watermark < 0 {
            return Err(manifest_corrupt(
                "segment sync_watermark must be nonnegative",
            ));
        }
        Ok(())
    }
}

/// Base chunk reference stored in a projection manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestChunkRef {
    /// Chunk file path relative to the projection artifact directory.
    pub(crate) path: String,
    /// Chunk file checksum.
    pub(crate) checksum: String,
    /// Inclusive source-node range start covered by the chunk.
    pub(crate) source_start: u32,
    /// Exclusive source-node range end covered by the chunk.
    pub(crate) source_end: u32,
    /// Dirty source-node count that caused this chunk rewrite.
    #[serde(default)]
    pub(crate) dirty_source_count: u32,
    /// Dirty edge-row count that caused this chunk rewrite.
    #[serde(default)]
    pub(crate) dirty_edge_count: u32,
}

impl ManifestChunkRef {
    fn validate(&self) -> GraphResult<()> {
        if self.path.trim().is_empty() {
            return Err(manifest_corrupt("base chunk path is required"));
        }
        if self.checksum.trim().is_empty() {
            return Err(manifest_corrupt("base chunk checksum is required"));
        }
        if self.source_start >= self.source_end {
            return Err(manifest_corrupt(
                "base chunk source_start must be less than source_end",
            ));
        }
        Ok(())
    }
}

/// Obsolete file reference retained for generation-aware cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestFileRef {
    /// Obsolete file path relative to the projection artifact directory.
    pub(crate) path: String,
    /// Number of bytes occupied by the obsolete file when known.
    pub(crate) bytes: u64,
}

impl ManifestFileRef {
    fn validate(&self) -> GraphResult<()> {
        if self.path.trim().is_empty() {
            return Err(manifest_corrupt("obsolete file path is required"));
        }
        Ok(())
    }
}

/// Active backend heartbeat row used by generation-aware cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionGenerationHeartbeat {
    /// PostgreSQL backend PID that is using the generation.
    pub(crate) backend_pid: i32,
    /// PostgreSQL database OID for the backend.
    pub(crate) database_oid: u32,
    /// Active manifest generation identifier.
    pub(crate) generation_id: u64,
    /// Heartbeat timestamp as Unix microseconds.
    pub(crate) heartbeat_at_unix_micros: i64,
    /// Expiration timestamp as Unix microseconds.
    pub(crate) expires_at_unix_micros: i64,
}

impl ProjectionGenerationHeartbeat {
    /// Return whether this heartbeat is stale at `now_unix_micros`.
    pub(crate) fn is_expired_at(self, now_unix_micros: i64) -> bool {
        self.expires_at_unix_micros <= now_unix_micros
    }

    /// Return a refreshed copy of this heartbeat.
    pub(crate) fn refreshed_at(self, now_unix_micros: i64, ttl: Duration) -> GraphResult<Self> {
        let ttl_micros = i64::try_from(ttl.as_micros())
            .map_err(|_| GraphError::Internal("projection heartbeat TTL is too large".into()))?;
        let expires_at_unix_micros = now_unix_micros
            .checked_add(ttl_micros)
            .ok_or_else(|| GraphError::Internal("projection heartbeat expiry overflowed".into()))?;
        Ok(Self {
            heartbeat_at_unix_micros: now_unix_micros,
            expires_at_unix_micros,
            ..self
        })
    }
}

pub(crate) fn record_loaded_generation_heartbeat(manifest: &ProjectionManifest) -> GraphResult<()> {
    record_active_generation_heartbeat(
        manifest.generation_id,
        DEFAULT_ACTIVE_GENERATION_TTL,
        manifest.sync_watermark,
        &manifest.validation_status,
    )
}

fn validate_status(status: &str) -> GraphResult<()> {
    match status {
        VALIDATION_STATUS_VALID | VALIDATION_STATUS_CORRUPT | VALIDATION_STATUS_REPAIRING => Ok(()),
        other => Err(manifest_corrupt(format!(
            "unsupported validation_status '{other}'"
        ))),
    }
}

fn manifest_corrupt(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: format!("projection manifest: {}", reason.into()),
    }
}

fn checked_manifest_bytes(current: usize, additional: usize) -> GraphResult<usize> {
    current
        .checked_add(additional)
        .ok_or_else(manifest_memory_overflow)
}

fn manifest_memory_overflow() -> GraphError {
    manifest_resource_limit(
        crate::resource::ResourcePhase::CompactionMerge,
        0,
        usize::MAX,
        usize::MAX,
    )
}

fn manifest_resource_limit(
    phase: crate::resource::ResourcePhase,
    used: usize,
    requested: usize,
    limit: usize,
) -> GraphError {
    GraphError::ResourceLimit {
        resource: "memory bytes".to_string(),
        phase: phase.as_str().to_string(),
        used: u64::try_from(used).unwrap_or(u64::MAX),
        requested: u64::try_from(requested).unwrap_or(u64::MAX),
        limit: u64::try_from(limit).unwrap_or(u64::MAX),
    }
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| std::io::Error::other("manifest encoded length overflowed"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn manifest_pretty_json_len(
    manifest: &ProjectionManifest,
    phase: crate::resource::ResourcePhase,
) -> GraphResult<usize> {
    let mut counter = CountingWriter::default();
    serde_json::to_writer_pretty(&mut counter, manifest)
        .map_err(|_| manifest_resource_limit(phase, counter.bytes, usize::MAX, usize::MAX))?;
    Ok(counter.bytes)
}

struct ComparingWriter {
    file: File,
    matched: usize,
}

impl Write for ComparingWriter {
    fn write(&mut self, expected: &[u8]) -> std::io::Result<usize> {
        let mut offset = 0usize;
        let mut actual = [0_u8; 8 * 1024];
        while offset < expected.len() {
            let take = (expected.len() - offset).min(actual.len());
            self.file.read_exact(&mut actual[..take])?;
            if actual[..take] != expected[offset..offset + take] {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "published manifest differs from staged JSON",
                ));
            }
            offset += take;
        }
        self.matched = self
            .matched
            .checked_add(expected.len())
            .ok_or_else(|| std::io::Error::other("manifest comparison length overflowed"))?;
        Ok(expected.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn verify_manifest_file_matches(
    path: &Path,
    manifest: &ProjectionManifest,
    encoded_len: usize,
) -> GraphResult<()> {
    let file_bytes = usize::try_from(
        fs::metadata(path)
            .map_err(|err| manifest_io("read published manifest metadata", path, err))?
            .len(),
    )
    .map_err(|_| manifest_corrupt("published manifest length exceeds address space"))?;
    if file_bytes != encoded_len {
        return Err(manifest_corrupt(format!(
            "published manifest length changed: expected {encoded_len}, got {file_bytes}"
        )));
    }
    let file = File::open(path)
        .map_err(|err| manifest_io("open published manifest for validation", path, err))?;
    let mut writer = ComparingWriter { file, matched: 0 };
    serde_json::to_writer_pretty(&mut writer, manifest)
        .map_err(|err| manifest_corrupt(format!("published manifest validation failed: {err}")))?;
    if writer.matched != encoded_len {
        return Err(manifest_corrupt(format!(
            "published manifest validation compared {} bytes; expected {encoded_len}",
            writer.matched
        )));
    }
    let mut extra = [0_u8; 1];
    if writer
        .file
        .read(&mut extra)
        .map_err(|err| manifest_io("validate published manifest length", path, err))?
        != 0
    {
        return Err(manifest_corrupt("published manifest has trailing bytes"));
    }
    Ok(())
}

pub(crate) fn manifest_file_name(generation_id: u64) -> String {
    format!("{MANIFEST_FILE_PREFIX}{generation_id:020}{MANIFEST_FILE_SUFFIX}")
}

pub(crate) fn parse_manifest_file_name(file_name: &str) -> Option<u64> {
    let generation = file_name
        .strip_prefix(MANIFEST_FILE_PREFIX)?
        .strip_suffix(MANIFEST_FILE_SUFFIX)?;
    if generation.len() != 20 || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    generation.parse().ok()
}

fn require_existing_reference(root: &Path, reference: &str, label: &str) -> GraphResult<()> {
    let path = resolve_manifest_reference(root, reference)?;
    if path.is_file() {
        Ok(())
    } else {
        Err(manifest_corrupt(format!(
            "{label} reference '{}' is missing",
            path.display()
        )))
    }
}

pub(crate) fn resolve_manifest_reference(root: &Path, reference: &str) -> GraphResult<PathBuf> {
    let path = Path::new(reference);
    if path.is_absolute() {
        return Err(manifest_corrupt(format!(
            "reference '{reference}' must be relative to the artifact directory"
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(manifest_corrupt(format!(
                    "reference '{reference}' must stay inside the artifact directory"
                )));
            }
        }
    }
    Ok(root.join(path))
}

fn sync_directory(path: &Path) -> GraphResult<()> {
    let dir = File::open(path).map_err(|err| manifest_io("open manifest directory", path, err))?;
    dir.sync_all()
        .map_err(|err| manifest_io("fsync manifest directory", path, err))
}

fn manifest_io(operation: &str, path: &Path, err: std::io::Error) -> GraphError {
    GraphError::Internal(format!(
        "projection manifest {operation} failed for {}: {err}",
        path.display()
    ))
}

#[cfg(not(test))]
pub(crate) fn record_active_generation_heartbeat(
    generation_id: u64,
    ttl: Duration,
    sync_watermark: i64,
    validation_status: &str,
) -> GraphResult<()> {
    validate_status(validation_status)?;
    let graph_id = current_graph_id()?;
    let generation_id = i64::try_from(generation_id)
        .map_err(|_| GraphError::Internal("projection generation id exceeds BIGINT".into()))?;
    let ttl_micros = i64::try_from(ttl.as_micros())
        .map_err(|_| GraphError::Internal("projection heartbeat TTL is too large".into()))?;
    pgrx::Spi::run_with_args(
        "INSERT INTO graph._projection_generations (
             graph_id, generation_id, backend_pid, database_oid, heartbeat_at, expires_at,
             sync_watermark, validation_status
         )
         VALUES (
             $5::uuid, $1, pg_backend_pid(),
             (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()),
             now(), now() + ($2::double precision * interval '1 microsecond'),
             $3, $4
         )
         ON CONFLICT (graph_id, generation_id, backend_pid, database_oid)
         DO UPDATE SET
             heartbeat_at = EXCLUDED.heartbeat_at,
             expires_at = EXCLUDED.expires_at,
             sync_watermark = EXCLUDED.sync_watermark,
             validation_status = EXCLUDED.validation_status,
             updated_at = now()",
        &[
            generation_id.into(),
            ttl_micros.into(),
            sync_watermark.into(),
            validation_status.into(),
            graph_id.into(),
        ],
    )
    .map_err(|err| GraphError::Internal(format!("projection heartbeat update failed: {err}")))
}

#[cfg(test)]
pub(crate) fn record_active_generation_heartbeat(
    _generation_id: u64,
    _ttl: Duration,
    _sync_watermark: i64,
    _validation_status: &str,
) -> GraphResult<()> {
    Ok(())
}

#[cfg(test)]
pub(crate) fn active_generation_count() -> GraphResult<i32> {
    Ok(0)
}

#[cfg(not(test))]
pub(crate) fn active_generation_count() -> GraphResult<i32> {
    let graph_id = current_graph_id()?;
    let count = pgrx::Spi::get_one_with_args::<i64>(
        "SELECT count(*)::bigint
         FROM graph._projection_generations
         WHERE graph_id = $1::uuid
           AND backend_pid <> 0
           AND database_oid = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
           AND expires_at > now()",
        &[graph_id.into()],
    )
    .map_err(|err| GraphError::Internal(format!("projection heartbeat count failed: {err}")))?
    .unwrap_or(0);
    Ok(count.min(i32::MAX as i64) as i32)
}

#[cfg(not(test))]
pub(crate) fn generation_has_active_heartbeat(generation_id: u64) -> GraphResult<bool> {
    let graph_id = current_graph_id()?;
    let generation_id = i64::try_from(generation_id)
        .map_err(|_| GraphError::Internal("projection generation id exceeds BIGINT".into()))?;
    pgrx::Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
             FROM graph._projection_generations
             WHERE graph_id = $2::uuid
               AND generation_id = $1
               AND backend_pid <> 0
               AND database_oid = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
               AND expires_at > now()
         )",
        &[generation_id.into(), graph_id.into()],
    )
    .map(|active| active.unwrap_or(false))
    .map_err(|err| GraphError::Internal(format!("projection heartbeat lookup failed: {err}")))
}

#[cfg(test)]
pub(crate) fn generation_has_active_heartbeat(_generation_id: u64) -> GraphResult<bool> {
    Ok(false)
}

#[cfg(not(test))]
pub(crate) fn active_generation_ids() -> GraphResult<Vec<u64>> {
    let graph_id = current_graph_id()?;
    let rows = pgrx::Spi::connect(|client| {
        let result = client.select(
            "SELECT DISTINCT generation_id
             FROM graph._projection_generations
             WHERE graph_id = $1::uuid
               AND backend_pid <> 0
               AND database_oid = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
               AND expires_at > now()
             ORDER BY generation_id",
            None,
            &[graph_id.into()],
        )?;
        let mut generations = Vec::new();
        for row in result {
            generations.push(row.get::<i64>(1)?.unwrap_or(0));
        }
        Ok::<_, pgrx::spi::SpiError>(generations)
    })
    .map_err(|err| GraphError::Internal(format!("projection heartbeat scan failed: {err}")))?;
    rows.into_iter()
        .map(|generation_id| {
            u64::try_from(generation_id)
                .map_err(|_| GraphError::Internal("projection generation id is negative".into()))
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn active_generation_ids() -> GraphResult<Vec<u64>> {
    Ok(Vec::new())
}

#[cfg(not(test))]
pub(crate) fn expire_stale_generation_heartbeats() -> GraphResult<()> {
    let graph_id = current_graph_id()?;
    pgrx::Spi::run_with_args(
        "DELETE FROM graph._projection_generations
         WHERE graph_id = $1::uuid
           AND backend_pid <> 0
           AND expires_at <= now()",
        &[graph_id.into()],
    )
    .map_err(|err| GraphError::Internal(format!("projection heartbeat expiration failed: {err}")))
}

#[cfg(not(test))]
fn current_graph_id() -> GraphResult<String> {
    crate::catalog::selected_or_default_graph_metadata().map(|graph| graph.graph_id)
}

#[cfg(test)]
pub(crate) fn expire_stale_generation_heartbeats() -> GraphResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_fixtures::ProjectionArtifactDir;
    use std::sync::{Arc, Barrier};

    #[test]
    fn projection_manifest_roundtrips_base_only_generation() {
        let manifest =
            ProjectionManifest::base_only(1, "base.pggraph", "xxh3:abcd", 2, 42, 1_700_000);

        let json = manifest.to_pretty_json().expect("manifest encodes");
        let decoded = ProjectionManifest::from_json(&json).expect("manifest decodes");

        assert_eq!(decoded, manifest);
        assert!(decoded.segments.is_empty());
        assert_eq!(decoded.validation_status, VALIDATION_STATUS_VALID);
    }

    #[test]
    fn projection_manifest_rejects_missing_required_fields() {
        let raw = serde_json::json!({
            "version": MANIFEST_VERSION,
            "generation_id": 1,
            "base_artifact_checksum": "xxh3:abcd",
            "base_artifact_version": 2,
            "segments": [],
            "base_chunks": [],
            "obsolete_files": [],
            "sync_watermark": 0,
            "validation_status": VALIDATION_STATUS_VALID,
            "created_at_unix_micros": 1
        })
        .to_string();

        let err = ProjectionManifest::from_json(&raw).expect_err("missing path should reject");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn projection_manifest_rejects_unsupported_version() {
        let mut manifest =
            ProjectionManifest::base_only(1, "base.pggraph", "xxh3:abcd", 2, 42, 1_700_000);
        manifest.version = MANIFEST_VERSION + 1;

        let err = manifest
            .to_pretty_json()
            .expect_err("unsupported version should reject");

        assert!(matches!(err, GraphError::IncompatibleVersion(_)));
    }

    #[test]
    fn projection_manifest_rejects_unknown_fields() {
        let raw = serde_json::json!({
            "version": MANIFEST_VERSION,
            "generation_id": 1,
            "previous_generation_id": null,
            "base_artifact_path": "base.pggraph",
            "base_artifact_checksum": "xxh3:abcd",
            "base_artifact_version": 2,
            "segments": [],
            "base_chunks": [],
            "obsolete_files": [],
            "sync_watermark": 0,
            "validation_status": VALIDATION_STATUS_VALID,
            "created_at_unix_micros": 1,
            "unexpected": true
        })
        .to_string();

        let err = ProjectionManifest::from_json(&raw).expect_err("unknown manifest field rejects");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn projection_manifest_rejects_unknown_nested_fields() {
        let raw = serde_json::json!({
            "version": MANIFEST_VERSION,
            "generation_id": 1,
            "previous_generation_id": null,
            "base_artifact_path": "base.pggraph",
            "base_artifact_checksum": "xxh3:abcd",
            "base_artifact_version": 2,
            "segments": [
                {
                    "path": "segments/l0.pggraphseg",
                    "checksum": "xxh3:segment",
                    "level": 0,
                    "source_start": 0,
                    "source_end": 10,
                    "sync_watermark": 42,
                    "unexpected": true
                }
            ],
            "base_chunks": [],
            "obsolete_files": [],
            "sync_watermark": 42,
            "validation_status": VALIDATION_STATUS_VALID,
            "created_at_unix_micros": 1
        })
        .to_string();

        let err = ProjectionManifest::from_json(&raw).expect_err("unknown nested field rejects");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn projection_manifest_rejects_partial_references() {
        let mut manifest =
            ProjectionManifest::base_only(1, "base.pggraph", "xxh3:abcd", 2, 42, 1_700_000);
        manifest.segments.push(ManifestSegmentRef {
            path: String::new(),
            checksum: "xxh3:segment".to_string(),
            level: 0,
            source_start: 0,
            source_end: 10,
            sync_watermark: 42,
        });

        let err = manifest.validate().expect_err("empty segment path rejects");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn projection_generation_heartbeat_expires_stale_backend() {
        let heartbeat = ProjectionGenerationHeartbeat {
            backend_pid: 123,
            database_oid: 456,
            generation_id: 7,
            heartbeat_at_unix_micros: 1_000,
            expires_at_unix_micros: 2_000,
        };

        assert!(!heartbeat.is_expired_at(1_999));
        assert!(heartbeat.is_expired_at(2_000));

        let refreshed = heartbeat
            .refreshed_at(3_000, Duration::from_millis(250))
            .expect("heartbeat refreshes");
        assert_eq!(refreshed.heartbeat_at_unix_micros, 3_000);
        assert_eq!(refreshed.expires_at_unix_micros, 253_000);
    }

    #[test]
    fn projection_manifest_ignores_unreferenced_temp_files() {
        let dir = ProjectionArtifactDir::new("projection_manifest_ignores_unreferenced_temp_files");
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        write_artifact(
            dir.path()
                .join("projection-generation-99999999999999999999.tmp-orphan"),
            b"partial",
        );
        let manifest = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:base", 2, 0, 1);
        store.publish(&manifest).expect("manifest publishes");

        let loaded = store
            .load_latest_current()
            .expect("manifest loads")
            .expect("current manifest exists");

        assert_eq!(loaded.generation_id, 1);
        assert_eq!(loaded, manifest);
    }

    #[test]
    fn projection_manifest_published_current_generation_wins() {
        let dir =
            ProjectionArtifactDir::new("projection_manifest_published_current_generation_wins");
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let first = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        let second = ProjectionManifest::base_only(2, "base.pggraph", "xxh3:second", 2, 20, 2);
        store.publish(&first).expect("first manifest publishes");
        store.publish(&second).expect("second manifest publishes");

        let loaded = store
            .load_latest_current()
            .expect("manifest loads")
            .expect("current manifest exists");

        assert_eq!(loaded, second);
    }

    #[test]
    fn projection_manifest_unpublished_generation_cannot_become_current() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_unpublished_generation_cannot_become_current",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let current = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        let orphan = ProjectionManifest::base_only(99, "base.pggraph", "xxh3:orphan", 2, 99, 99);
        store.publish(&current).expect("current manifest publishes");
        write_artifact(
            store.manifest_path(orphan.generation_id),
            orphan
                .to_pretty_json()
                .expect("orphan manifest encodes")
                .as_bytes(),
        );

        let loaded = store
            .load_latest_current()
            .expect("current manifest loads")
            .expect("current manifest exists");

        assert_eq!(loaded, current);
    }

    #[test]
    fn projection_manifest_stale_publisher_loses_compare_and_swap() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_stale_publisher_loses_compare_and_swap",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let first = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        let winner = ProjectionManifest::base_only(2, "base.pggraph", "xxh3:winner", 2, 20, 2);
        let stale = ProjectionManifest::base_only(3, "base.pggraph", "xxh3:stale", 2, 30, 3);
        store.publish(&first).expect("first manifest publishes");
        store
            .publish_if_current(&winner, Some(first.generation_id))
            .expect("winning candidate publishes");

        let err = store
            .publish_if_current(&stale, Some(first.generation_id))
            .expect_err("stale candidate must lose CAS");
        let loaded = store
            .load_latest_current()
            .expect("current manifest loads")
            .expect("current manifest exists");

        assert!(matches!(err, GraphError::BuildLocked));
        assert_eq!(loaded, winner);
        assert!(!store.manifest_path(stale.generation_id).exists());
    }

    #[test]
    fn projection_manifest_concurrent_publishers_commit_exactly_one_candidate() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_concurrent_publishers_commit_exactly_one_candidate",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let first = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        store.publish(&first).expect("first manifest publishes");
        let barrier = Arc::new(Barrier::new(3));
        let candidates = [
            ProjectionManifest::base_only(2, "base.pggraph", "xxh3:second", 2, 20, 2),
            ProjectionManifest::base_only(3, "base.pggraph", "xxh3:third", 2, 30, 3),
        ];
        let handles = candidates.map(|candidate| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let generation_id = candidate.generation_id;
                (generation_id, store.publish_if_current(&candidate, Some(1)))
            })
        });
        barrier.wait();
        let results = handles.map(|handle| handle.join().expect("publisher thread joins"));

        assert_eq!(
            results.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|(_, result)| matches!(result, Err(GraphError::BuildLocked)))
                .count(),
            1
        );
        let winner = results
            .iter()
            .find_map(|(generation_id, result)| result.is_ok().then_some(*generation_id))
            .expect("one publisher wins");
        let loser = results
            .iter()
            .find_map(|(generation_id, result)| result.is_err().then_some(*generation_id))
            .expect("one publisher loses");
        let loaded = store
            .load_latest_current()
            .expect("current loads")
            .expect("current exists");

        assert_eq!(loaded.generation_id, winner);
        assert!(store.manifest_path(winner).exists());
        assert!(!store.manifest_path(loser).exists());
    }

    #[test]
    fn projection_manifest_corrupt_current_pointer_fails_closed() {
        let dir =
            ProjectionArtifactDir::new("projection_manifest_corrupt_current_pointer_fails_closed");
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let manifest = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        store.publish(&manifest).expect("manifest publishes");
        write_artifact(store.current_pointer_path(), b"{\"version\":1");

        let err = store
            .load_latest_current()
            .expect_err("partial current pointer must fail closed");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn projection_manifest_oversized_current_pointer_fails_bounded() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_oversized_current_pointer_fails_bounded",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(
            store.current_pointer_path(),
            &vec![b'x'; MAX_CURRENT_POINTER_BYTES + 1],
        );

        let err = store
            .load_latest_current()
            .expect_err("oversized current pointer must fail closed");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn projection_manifest_pointer_rejects_rewritten_current_manifest() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_pointer_rejects_rewritten_current_manifest",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let manifest = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        store.publish(&manifest).expect("manifest publishes");
        let rewritten = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:changed", 2, 10, 1);
        write_artifact(
            store.manifest_path(rewritten.generation_id),
            rewritten
                .to_pretty_json()
                .expect("rewritten manifest encodes")
                .as_bytes(),
        );

        let err = store
            .load_latest_current()
            .expect_err("rewritten current manifest must fail checksum validation");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn projection_manifest_recovery_replaces_corrupt_pointer_after_staging() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_recovery_replaces_corrupt_pointer_after_staging",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let first = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        let recovered =
            ProjectionManifest::base_only(2, "base.pggraph", "xxh3:recovered", 2, 20, 2);
        store.publish(&first).expect("first manifest publishes");
        write_artifact(store.current_pointer_path(), b"partial-current-pointer");

        store
            .publish_for_recovery(&recovered)
            .expect("recovery manifest publishes");
        let loaded = store
            .load_latest_current()
            .expect("recovered current loads")
            .expect("recovered current exists");

        assert_eq!(loaded, recovered);
        assert!(store.manifest_path(first.generation_id).exists());
    }

    #[test]
    fn projection_manifest_post_rename_failure_never_deletes_current_candidate() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_post_rename_failure_never_deletes_current_candidate",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let first = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        let candidate =
            ProjectionManifest::base_only(2, "base.pggraph", "xxh3:candidate", 2, 20, 2);
        store.publish(&first).expect("first manifest publishes");
        FAIL_AFTER_POINTER_RENAME.with(|fail| fail.set(true));

        let err = store
            .publish_if_current(&candidate, Some(first.generation_id))
            .expect_err("injected post-rename sync boundary fails");
        let loaded = store
            .load_latest_current()
            .expect("current remains structurally valid")
            .expect("one current generation exists");

        assert!(matches!(err, GraphError::Internal(_)));
        assert_eq!(loaded, candidate);
        assert!(store.manifest_path(candidate.generation_id).exists());
        assert!(store.manifest_path(first.generation_id).exists());
    }

    #[test]
    fn projection_manifest_publish_failure_keeps_previous_generation_current() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_publish_failure_keeps_previous_generation_current",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let first = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        let invalid =
            ProjectionManifest::base_only(2, "missing-base.pggraph", "xxh3:second", 2, 20, 2);
        store.publish(&first).expect("first manifest publishes");

        let err = store
            .publish(&invalid)
            .expect_err("missing base artifact rejects before publish");
        let loaded = store
            .load_latest_current()
            .expect("manifest loads")
            .expect("current manifest exists");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
        assert_eq!(loaded, first);
        assert!(!store.manifest_path(2).exists());
        assert_eq!(
            manifest_temp_file_count(dir.path()),
            0,
            "failed validation should not leave temp manifests"
        );
    }

    #[test]
    fn projection_manifest_publish_rejects_duplicate_generation() {
        let dir =
            ProjectionArtifactDir::new("projection_manifest_publish_rejects_duplicate_generation");
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let first = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:first", 2, 10, 1);
        let duplicate = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:second", 2, 20, 2);
        store.publish(&first).expect("first manifest publishes");

        let err = store
            .publish(&duplicate)
            .expect_err("duplicate generation rejects");
        let loaded = store
            .load_latest_current()
            .expect("manifest loads")
            .expect("current manifest exists");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
        assert_eq!(loaded, first);
        assert_eq!(manifest_temp_file_count(dir.path()), 0);
    }

    #[test]
    fn projection_manifest_rejects_references_outside_artifact_root() {
        let dir = ProjectionArtifactDir::new(
            "projection_manifest_rejects_references_outside_artifact_root",
        );
        let outside = ProjectionArtifactDir::new(
            "projection_manifest_rejects_references_outside_artifact_root_outside",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        write_artifact(outside.path().join("outside.pggraph"), b"outside");

        let absolute = ProjectionManifest::base_only(
            1,
            outside.path().join("outside.pggraph").to_string_lossy(),
            "xxh3:absolute",
            2,
            0,
            1,
        );
        let parent = ProjectionManifest::base_only(2, "../outside.pggraph", "xxh3:parent", 2, 0, 2);

        let absolute_err = store
            .publish(&absolute)
            .expect_err("absolute reference rejects");
        let parent_err = store
            .publish(&parent)
            .expect_err("parent traversal reference rejects");

        assert!(matches!(absolute_err, GraphError::CorruptFile { .. }));
        assert!(matches!(parent_err, GraphError::CorruptFile { .. }));
        assert!(store
            .load_latest_current()
            .expect("empty store loads")
            .is_none());
    }

    #[test]
    fn projection_manifest_rejects_missing_referenced_file() {
        let dir = ProjectionArtifactDir::new("projection_manifest_rejects_missing_referenced_file");
        let store = ProjectionManifestStore::new(dir.path());
        let manifest =
            ProjectionManifest::base_only(1, "missing-base.pggraph", "xxh3:base", 2, 0, 1);
        write_artifact(
            store.manifest_path(1),
            manifest
                .to_pretty_json()
                .expect("manifest encodes")
                .as_bytes(),
        );

        let err = store
            .load_latest_current()
            .expect_err("missing referenced base rejects");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn projection_manifest_rejects_missing_identity_dictionary() {
        let dir =
            ProjectionArtifactDir::new("projection_manifest_rejects_missing_identity_dictionary");
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let store = ProjectionManifestStore::new(dir.path());
        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "xxh3:base", 2, 0, 1);
        manifest.relationship_identities = Some(ManifestIdentityRef {
            path: "missing-identities.bin".to_string(),
            checksum: "crc32:00000000".to_string(),
            entry_count: 1,
            bytes: 32,
        });

        let err = store
            .publish(&manifest)
            .expect_err("missing identity dictionary rejects");
        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn governed_large_manifest_rejects_before_staging_and_preserves_current() {
        let dir = ProjectionArtifactDir::new(
            "governed_large_manifest_rejects_before_staging_and_preserves_current",
        );
        let store = ProjectionManifestStore::new(dir.path());
        write_artifact(dir.path().join("base.pggraph"), b"base");
        let first = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 0, 1);
        store.publish(&first).expect("first manifest publishes");

        let mut candidate = ProjectionManifest::base_only(2, "base.pggraph", "crc32:base", 1, 0, 2);
        candidate
            .obsolete_files
            .try_reserve_exact(2_048)
            .expect("test manifest reference allocation succeeds");
        candidate
            .obsolete_files
            .extend((0..2_048).map(|index| ManifestFileRef {
                path: format!("obsolete-generation-{index:08}.pggraph-delta"),
                bytes: index,
            }));
        let encoded_len =
            manifest_pretty_json_len(&candidate, crate::resource::ResourcePhase::CompactionMerge)
                .expect("JSON length preflights");
        assert!(
            encoded_len > 100_000,
            "fixture must exercise a large manifest"
        );

        let memory_governor =
            crate::resource::ResourceGovernor::new(crate::resource::ResourceLimits::new(
                crate::resource::MemoryBudget::new(crate::resource::ByteCount::from_bytes(
                    (GOVERNED_MANIFEST_IO_WORKSPACE_BYTES - 1) as u64,
                )),
                crate::resource::DiskBudget::UNLIMITED,
                crate::resource::RowCount::UNLIMITED,
                crate::resource::WorkUnits::UNLIMITED,
                crate::resource::ElapsedBudget::new(Duration::from_secs(1)),
            ));
        let memory_error = store
            .publish_governed(
                &candidate,
                &memory_governor,
                crate::resource::ResourcePhase::CompactionMerge,
                GOVERNED_MANIFEST_IO_WORKSPACE_BYTES - 1,
            )
            .expect_err("workspace must reject before staging");
        assert_eq!(memory_error.diagnostic_code().as_str(), "PG007");
        assert!(!store.manifest_path(2).exists());
        assert_eq!(manifest_temp_file_count(dir.path()), 0);

        let disk_governor =
            crate::resource::ResourceGovernor::new(crate::resource::ResourceLimits::new(
                crate::resource::MemoryBudget::new(crate::resource::ByteCount::from_bytes(
                    GOVERNED_MANIFEST_IO_WORKSPACE_BYTES as u64,
                )),
                crate::resource::DiskBudget::new(crate::resource::ByteCount::from_bytes(
                    u64::try_from(encoded_len - 1).expect("encoded length fits u64"),
                )),
                crate::resource::RowCount::UNLIMITED,
                crate::resource::WorkUnits::UNLIMITED,
                crate::resource::ElapsedBudget::new(Duration::from_secs(1)),
            ));
        let disk_error = store
            .publish_governed(
                &candidate,
                &disk_governor,
                crate::resource::ResourcePhase::CompactionMerge,
                GOVERNED_MANIFEST_IO_WORKSPACE_BYTES,
            )
            .expect_err("encoded manifest must fit staging disk");
        assert_eq!(disk_error.diagnostic_code().as_str(), "PG007");
        assert!(!store.manifest_path(2).exists());
        assert_eq!(manifest_temp_file_count(dir.path()), 0);
        assert_eq!(
            store
                .load_latest_current()
                .expect("manifest lookup succeeds")
                .expect("first generation remains current"),
            first
        );
    }

    fn write_artifact(path: impl AsRef<Path>, bytes: &[u8]) {
        fs::write(path, bytes).expect("test artifact writes");
    }

    fn manifest_temp_file_count(path: &Path) -> usize {
        fs::read_dir(path)
            .expect("test artifact dir reads")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count()
    }
}
