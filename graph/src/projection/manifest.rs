//! Durable projection manifest metadata and active-generation heartbeats.
//!
//! A projection manifest is the publication boundary for derived graph
//! artifacts. Readers load a complete generation from a validated manifest
//! instead of discovering segment files directly.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::safety::{GraphError, GraphResult};

/// Current JSON manifest format version.
pub(crate) const MANIFEST_VERSION: u32 = 1;
/// Validation state for a generation whose artifacts are ready to read.
pub(crate) const VALIDATION_STATUS_VALID: &str = "valid";
/// Validation state for a generation that has been marked corrupt.
pub(crate) const VALIDATION_STATUS_CORRUPT: &str = "corrupt";
/// Validation state for a generation that is being repaired.
pub(crate) const VALIDATION_STATUS_REPAIRING: &str = "repairing";
/// Default TTL for backend active-generation heartbeat rows.
pub(crate) const DEFAULT_ACTIVE_GENERATION_TTL: Duration = Duration::from_secs(300);

const MANIFEST_FILE_PREFIX: &str = "projection-generation-";
const MANIFEST_FILE_SUFFIX: &str = ".json";

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
            base_chunks: Vec::new(),
            obsolete_files: Vec::new(),
            sync_watermark,
            validation_status: VALIDATION_STATUS_VALID.to_string(),
            created_at_unix_micros,
        }
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

    /// Whether this generation references only the base `.pggraph` artifact.
    pub(crate) fn is_base_only(&self) -> bool {
        self.segments.is_empty() && self.base_chunks.is_empty()
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

    /// Atomically publish a validated manifest to its generation path.
    ///
    /// # Errors
    ///
    /// Returns validation errors when the manifest is malformed or references
    /// missing active artifacts. Returns [`GraphError::Internal`] when durable
    /// filesystem operations fail.
    pub(crate) fn publish(&self, manifest: &ProjectionManifest) -> GraphResult<PathBuf> {
        manifest.validate()?;
        self.validate_active_references(manifest)?;

        fs::create_dir_all(&self.root)
            .map_err(|err| manifest_io("create manifest directory", &self.root, err))?;
        let json = manifest.to_pretty_json()?;
        let final_path = self.manifest_path(manifest.generation_id);
        if final_path.exists() {
            return Err(manifest_corrupt(format!(
                "generation {} already has a published manifest",
                manifest.generation_id
            )));
        }
        let (tmp_path, mut file) = self.create_temp_manifest_file(manifest.generation_id)?;

        file.write_all(json.as_bytes())
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
        let Some((generation_id, path)) = self.latest_manifest_path()? else {
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

    fn load_manifest_file(&self, path: &Path) -> GraphResult<ProjectionManifest> {
        let raw =
            fs::read_to_string(path).map_err(|err| manifest_io("read manifest", path, err))?;
        ProjectionManifest::from_json(&raw)
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
        for segment in &manifest.segments {
            require_existing_reference(&self.root, &segment.path, "segment")?;
        }
        for chunk in &manifest.base_chunks {
            require_existing_reference(&self.root, &chunk.path, "base chunk")?;
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

fn manifest_file_name(generation_id: u64) -> String {
    format!("{MANIFEST_FILE_PREFIX}{generation_id:020}{MANIFEST_FILE_SUFFIX}")
}

fn parse_manifest_file_name(file_name: &str) -> Option<u64> {
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

fn resolve_manifest_reference(root: &Path, reference: &str) -> GraphResult<PathBuf> {
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
    let generation_id = i64::try_from(generation_id)
        .map_err(|_| GraphError::Internal("projection generation id exceeds BIGINT".into()))?;
    let ttl_micros = i64::try_from(ttl.as_micros())
        .map_err(|_| GraphError::Internal("projection heartbeat TTL is too large".into()))?;
    pgrx::Spi::run_with_args(
        "INSERT INTO graph._projection_generations (
             generation_id, backend_pid, database_oid, heartbeat_at, expires_at,
             sync_watermark, validation_status
         )
         VALUES (
             $1, pg_backend_pid(),
             (SELECT oid FROM pg_database WHERE datname = current_database()),
             now(), now() + ($2::double precision * interval '1 microsecond'),
             $3, $4
         )
         ON CONFLICT (generation_id, backend_pid, database_oid)
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
    let count = pgrx::Spi::get_one::<i64>(
        "SELECT count(*)::bigint
         FROM graph._projection_generations
         WHERE backend_pid <> 0
           AND database_oid = (SELECT oid FROM pg_database WHERE datname = current_database())
           AND expires_at > now()",
    )
    .map_err(|err| GraphError::Internal(format!("projection heartbeat count failed: {err}")))?
    .unwrap_or(0);
    Ok(count.min(i32::MAX as i64) as i32)
}

#[cfg(not(test))]
pub(crate) fn generation_has_active_heartbeat(generation_id: u64) -> GraphResult<bool> {
    let generation_id = i64::try_from(generation_id)
        .map_err(|_| GraphError::Internal("projection generation id exceeds BIGINT".into()))?;
    pgrx::Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
             FROM graph._projection_generations
             WHERE generation_id = $1
               AND backend_pid <> 0
               AND database_oid = (SELECT oid FROM pg_database WHERE datname = current_database())
               AND expires_at > now()
         )",
        &[generation_id.into()],
    )
    .map(|active| active.unwrap_or(false))
    .map_err(|err| GraphError::Internal(format!("projection heartbeat lookup failed: {err}")))
}

#[cfg(test)]
pub(crate) fn generation_has_active_heartbeat(_generation_id: u64) -> GraphResult<bool> {
    Ok(false)
}

#[cfg(not(test))]
pub(crate) fn expire_stale_generation_heartbeats() -> GraphResult<()> {
    pgrx::Spi::run(
        "DELETE FROM graph._projection_generations
         WHERE backend_pid <> 0 AND expires_at <= now()",
    )
    .map_err(|err| GraphError::Internal(format!("projection heartbeat expiration failed: {err}")))
}

#[cfg(test)]
pub(crate) fn expire_stale_generation_heartbeats() -> GraphResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_fixtures::ProjectionArtifactDir;

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
    fn projection_manifest_latest_current_generation_wins() {
        let dir = ProjectionArtifactDir::new("projection_manifest_latest_current_generation_wins");
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
