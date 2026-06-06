//! Durable projection manifest metadata and active-generation heartbeats.
//!
//! A projection manifest is the publication boundary for derived graph
//! artifacts. Readers load a complete generation from a validated manifest
//! instead of discovering segment files directly.

use std::time::Duration;

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
}

impl ManifestChunkRef {
    fn validate(&self) -> GraphResult<()> {
        if self.path.trim().is_empty() {
            return Err(manifest_corrupt("base chunk path is required"));
        }
        if self.checksum.trim().is_empty() {
            return Err(manifest_corrupt("base chunk checksum is required"));
        }
        if self.source_start > self.source_end {
            return Err(manifest_corrupt(
                "base chunk source_start must not exceed source_end",
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

#[cfg(not(test))]
pub(crate) fn expire_stale_generation_heartbeats() -> GraphResult<()> {
    pgrx::Spi::run(
        "DELETE FROM graph._projection_generations
         WHERE backend_pid <> 0 AND expires_at <= now()",
    )
    .map_err(|err| GraphError::Internal(format!("projection heartbeat expiration failed: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
