//! Durable projection status and operator diagnostics.
//!
//! Status collection reads manifest metadata and lightweight artifact metadata
//! so SQL diagnostics can recommend ingestion, compaction, GC, or repair.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::projection::manifest::{
    resolve_manifest_reference, ProjectionManifest, ProjectionManifestStore,
    VALIDATION_STATUS_VALID,
};
use crate::projection::recovery::{
    plan_projection_recovery_for_artifact, ProjectionRecoveryAction,
};
use crate::projection::segment::{DeltaSegment, SegmentKind};
use crate::safety::{GraphError, GraphResult};

const PROJECTION_OPERATION_STATUS_FILE: &str = "projection-status.json";

/// Durable projection diagnostic row exposed to SQL.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProjectionStatus {
    pub(crate) manifest_generation: Option<i64>,
    pub(crate) manifest_watermark: Option<i64>,
    pub(crate) pending_durable_rows: i64,
    pub(crate) segment_count: i32,
    pub(crate) segment_bytes: i64,
    pub(crate) l0_segment_count: i32,
    pub(crate) l1_segment_count: i32,
    pub(crate) l2_segment_count: i32,
    pub(crate) edge_segment_count: i32,
    pub(crate) node_segment_count: i32,
    pub(crate) dirty_chunk_count: i32,
    pub(crate) dirty_chunk_bytes: i64,
    pub(crate) tombstone_ratio: f64,
    pub(crate) compaction_backlog: i32,
    pub(crate) obsolete_file_count: i32,
    pub(crate) obsolete_bytes: i64,
    pub(crate) active_generation_count: i32,
    pub(crate) artifact_validation_state: String,
    pub(crate) last_ingestion_unix_micros: Option<i64>,
    pub(crate) last_compaction_unix_micros: Option<i64>,
    pub(crate) last_gc_unix_micros: Option<i64>,
    pub(crate) last_repair_unix_micros: Option<i64>,
    pub(crate) ingest_recommended: bool,
    pub(crate) compaction_recommended: bool,
    pub(crate) gc_recommended: bool,
    pub(crate) repair_recommended: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionOperationStatus {
    #[serde(default)]
    last_gc_unix_micros: Option<i64>,
}

impl ProjectionStatus {
    fn empty(
        pending_durable_rows: i64,
        active_generation_count: i32,
        artifact_validation_state: impl Into<String>,
    ) -> Self {
        Self {
            manifest_generation: None,
            manifest_watermark: None,
            pending_durable_rows,
            segment_count: 0,
            segment_bytes: 0,
            l0_segment_count: 0,
            l1_segment_count: 0,
            l2_segment_count: 0,
            edge_segment_count: 0,
            node_segment_count: 0,
            dirty_chunk_count: 0,
            dirty_chunk_bytes: 0,
            tombstone_ratio: 0.0,
            compaction_backlog: 0,
            obsolete_file_count: 0,
            obsolete_bytes: 0,
            active_generation_count,
            artifact_validation_state: artifact_validation_state.into(),
            last_ingestion_unix_micros: None,
            last_compaction_unix_micros: None,
            last_gc_unix_micros: None,
            last_repair_unix_micros: None,
            ingest_recommended: pending_durable_rows > 0,
            compaction_recommended: false,
            gc_recommended: false,
            repair_recommended: false,
        }
    }
}

/// Collect projection status for the latest generation under `root`.
pub(crate) fn collect_projection_status(
    root: &Path,
    graph_path: Option<&Path>,
    max_sync_log_id: i64,
    active_generation_count: i32,
    compaction_threshold: usize,
) -> GraphResult<ProjectionStatus> {
    let operation_status = load_operation_status(root).unwrap_or_default();
    let recovery = plan_projection_recovery_for_artifact(root, graph_path)?;
    let action = recovery_action_text(recovery.action);
    let store = ProjectionManifestStore::new(root);
    let manifest = match store.load_latest_current_for_recovery() {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return Ok(ProjectionStatus::empty(
                max_sync_log_id.max(0),
                active_generation_count,
                action,
            ));
        }
        Err(_) => {
            let mut status =
                ProjectionStatus::empty(max_sync_log_id.max(0), active_generation_count, action);
            status.manifest_generation = recovery.generation_id.and_then(u64_to_i64);
            status.repair_recommended = recovery.action == ProjectionRecoveryAction::FullRebuild;
            return Ok(status);
        }
    };

    let mut status = status_from_manifest(
        root,
        &manifest,
        max_sync_log_id,
        active_generation_count,
        compaction_threshold,
        operation_status.last_gc_unix_micros,
        StatusScan::Full,
        action,
    )?;
    status.repair_recommended = matches!(
        recovery.action,
        ProjectionRecoveryAction::TargetedChunkRepair | ProjectionRecoveryAction::FullRebuild
    );
    Ok(status)
}

/// Collect lightweight projection recommendations without full artifact decode.
pub(crate) fn collect_projection_metadata_status(
    root: &Path,
    max_sync_log_id: i64,
    active_generation_count: i32,
    compaction_threshold: usize,
) -> GraphResult<ProjectionStatus> {
    let operation_status = load_operation_status(root).unwrap_or_default();
    let store = ProjectionManifestStore::new(root);
    let manifest = match store.load_latest_current_for_recovery() {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return Ok(ProjectionStatus::empty(
                max_sync_log_id.max(0),
                active_generation_count,
                "no_projection",
            ));
        }
        Err(_) => {
            let mut status = ProjectionStatus::empty(
                max_sync_log_id.max(0),
                active_generation_count,
                "full_rebuild",
            );
            status.repair_recommended = true;
            status.last_gc_unix_micros = operation_status.last_gc_unix_micros;
            return Ok(status);
        }
    };
    let mut status = status_from_manifest(
        root,
        &manifest,
        max_sync_log_id,
        active_generation_count,
        compaction_threshold,
        operation_status.last_gc_unix_micros,
        StatusScan::MetadataOnly,
        validation_status_text(&manifest),
    )?;
    status.repair_recommended = manifest.validation_status != VALIDATION_STATUS_VALID;
    Ok(status)
}

/// Persist a successful projection GC timestamp for later diagnostics.
pub(crate) fn record_projection_gc(root: &Path) -> GraphResult<()> {
    let mut status = load_operation_status(root).unwrap_or_default();
    status.last_gc_unix_micros = Some(now_unix_micros()?);
    write_operation_status(root, &status)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusScan {
    Full,
    MetadataOnly,
}

#[allow(
    clippy::too_many_arguments,
    reason = "status assembly keeps each release-gate field explicit at the manifest boundary"
)]
fn status_from_manifest(
    root: &Path,
    manifest: &ProjectionManifest,
    max_sync_log_id: i64,
    active_generation_count: i32,
    compaction_threshold: usize,
    last_gc_unix_micros: Option<i64>,
    scan: StatusScan,
    artifact_validation_state: &str,
) -> GraphResult<ProjectionStatus> {
    let mut segment_bytes = 0_i64;
    let mut l0_segment_count = 0_i32;
    let mut l1_segment_count = 0_i32;
    let mut l2_segment_count = 0_i32;
    let mut edge_segment_count = 0_i32;
    let mut node_segment_count = 0_i32;
    let mut mutation_rows = 0_usize;
    let mut tombstone_rows = 0_usize;

    for segment in &manifest.segments {
        let path = resolve_manifest_reference(root, &segment.path)?;
        segment_bytes = segment_bytes.saturating_add(file_len_i64(&path));
        match segment.level {
            0 => l0_segment_count = l0_segment_count.saturating_add(1),
            1 => l1_segment_count = l1_segment_count.saturating_add(1),
            _ => l2_segment_count = l2_segment_count.saturating_add(1),
        }
        if scan == StatusScan::Full {
            if let Ok(decoded) = DeltaSegment::read_from_path(&path) {
                match decoded.header.kind {
                    SegmentKind::Edge => edge_segment_count = edge_segment_count.saturating_add(1),
                    SegmentKind::Node => node_segment_count = node_segment_count.saturating_add(1),
                }
                mutation_rows = mutation_rows.saturating_add(segment_row_count(&decoded));
                tombstone_rows = tombstone_rows.saturating_add(segment_tombstone_count(&decoded));
            }
        }
    }

    let mut dirty_chunk_bytes = 0_i64;
    for chunk in &manifest.base_chunks {
        let path = resolve_manifest_reference(root, &chunk.path)?;
        dirty_chunk_bytes = dirty_chunk_bytes.saturating_add(file_len_i64(&path));
    }

    let obsolete_file_count = manifest.obsolete_files.len().min(i32::MAX as usize) as i32;
    let obsolete_bytes = manifest.obsolete_files.iter().fold(0_i64, |acc, file| {
        acc.saturating_add(file.bytes.min(i64::MAX as u64) as i64)
    });
    let segment_count = manifest.segments.len().min(i32::MAX as usize) as i32;
    let dirty_chunk_count = manifest.base_chunks.len().min(i32::MAX as usize) as i32;
    let compaction_backlog = manifest
        .segments
        .len()
        .saturating_sub(compaction_threshold)
        .min(i32::MAX as usize) as i32;
    let tombstone_ratio = if mutation_rows == 0 {
        0.0
    } else {
        tombstone_rows as f64 / mutation_rows as f64
    };
    let pending_durable_rows = max_sync_log_id
        .saturating_sub(manifest.sync_watermark)
        .max(0);

    Ok(ProjectionStatus {
        manifest_generation: u64_to_i64(manifest.generation_id),
        manifest_watermark: Some(manifest.sync_watermark),
        pending_durable_rows,
        segment_count,
        segment_bytes,
        l0_segment_count,
        l1_segment_count,
        l2_segment_count,
        edge_segment_count,
        node_segment_count,
        dirty_chunk_count,
        dirty_chunk_bytes,
        tombstone_ratio,
        compaction_backlog,
        obsolete_file_count,
        obsolete_bytes,
        active_generation_count,
        artifact_validation_state: artifact_validation_state.to_string(),
        last_ingestion_unix_micros: manifest.last_ingestion_unix_micros,
        last_compaction_unix_micros: manifest.last_compaction_unix_micros,
        last_gc_unix_micros,
        last_repair_unix_micros: manifest.last_repair_unix_micros,
        ingest_recommended: pending_durable_rows > 0,
        compaction_recommended: compaction_backlog > 0,
        gc_recommended: obsolete_file_count > 0 && obsolete_bytes > 0,
        repair_recommended: false,
    })
}

fn validation_status_text(manifest: &ProjectionManifest) -> &'static str {
    if manifest.validation_status == VALIDATION_STATUS_VALID {
        "healthy"
    } else {
        "full_rebuild"
    }
}

fn recovery_action_text(action: ProjectionRecoveryAction) -> &'static str {
    match action {
        ProjectionRecoveryAction::NoProjection => "no_projection",
        ProjectionRecoveryAction::Healthy => "healthy",
        ProjectionRecoveryAction::TargetedChunkRepair => "targeted_chunk_repair",
        ProjectionRecoveryAction::FullRebuild => "full_rebuild",
    }
}

fn segment_row_count(segment: &DeltaSegment) -> usize {
    segment
        .edge_inserts
        .len()
        .saturating_add(segment.edge_deletes.len())
        .saturating_add(segment.edge_weights.len())
        .saturating_add(segment.node_states.len())
        .saturating_add(segment.resolutions.len())
        .saturating_add(segment.filters.len())
        .saturating_add(segment.tenants.len())
}

fn segment_tombstone_count(segment: &DeltaSegment) -> usize {
    segment
        .edge_deletes
        .len()
        .saturating_add(segment.node_states.iter().filter(|row| !row.active).count())
        .saturating_add(
            segment
                .resolutions
                .iter()
                .filter(|row| row.tombstone)
                .count(),
        )
        .saturating_add(segment.filters.iter().filter(|row| row.tombstone).count())
        .saturating_add(segment.tenants.iter().filter(|row| row.tombstone).count())
}

fn file_len_i64(path: &Path) -> i64 {
    fs::metadata(path)
        .map(|metadata| metadata.len().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn u64_to_i64(value: u64) -> Option<i64> {
    i64::try_from(value).ok()
}

fn load_operation_status(root: &Path) -> GraphResult<ProjectionOperationStatus> {
    let path = operation_status_path(root);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectionOperationStatus::default());
        }
        Err(err) => return Err(status_io("read projection status", &path, err)),
    };
    serde_json::from_str(&raw).map_err(|err| GraphError::CorruptFile {
        reason: format!("projection operation status JSON decoding failed: {err}"),
    })
}

fn write_operation_status(root: &Path, status: &ProjectionOperationStatus) -> GraphResult<()> {
    fs::create_dir_all(root)
        .map_err(|err| status_io("create projection status directory", root, err))?;
    let path = operation_status_path(root);
    let tmp_path = temp_operation_status_path(root)?;
    let json = serde_json::to_string_pretty(status)
        .map_err(|err| GraphError::Internal(format!("projection status encoding failed: {err}")))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .map_err(|err| status_io("create temp projection status", &tmp_path, err))?;
    file.write_all(json.as_bytes())
        .map_err(|err| status_io("write temp projection status", &tmp_path, err))?;
    file.sync_all()
        .map_err(|err| status_io("fsync temp projection status", &tmp_path, err))?;
    drop(file);
    if let Err(err) = fs::rename(&tmp_path, &path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(status_io("rename projection status", &path, err));
    }
    Ok(())
}

fn temp_operation_status_path(root: &Path) -> GraphResult<PathBuf> {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?
        .as_nanos();
    for attempt in 0..128 {
        let path = root.join(format!(
            "{PROJECTION_OPERATION_STATUS_FILE}.tmp-{}-{created_at}-{attempt}",
            std::process::id()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(GraphError::Internal(
        "projection status temp path kept colliding".into(),
    ))
}

fn operation_status_path(root: &Path) -> PathBuf {
    root.join(PROJECTION_OPERATION_STATUS_FILE)
}

fn status_io(operation: &str, path: &Path, err: std::io::Error) -> GraphError {
    GraphError::Internal(format!("{operation} failed for {}: {err}", path.display()))
}

fn now_unix_micros() -> GraphResult<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?;
    i64::try_from(duration.as_micros())
        .map_err(|_| GraphError::Internal("system time exceeds i64 microseconds".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::manifest::{ManifestFileRef, ProjectionManifestStore};
    use crate::projection::segment::{SegmentEdge, SegmentNodeState};
    use crate::projection::test_fixtures::ProjectionArtifactDir;
    use crate::types::TraversalDirection;

    #[test]
    fn status_reports_manifest_watermark_segments_chunks_gc_and_repair() {
        let dir = ProjectionArtifactDir::new(
            "status_reports_manifest_watermark_segments_chunks_gc_and_repair",
        );
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let edge_path = dir.path().join("l0-edge.pggraph-delta");
        let mut edge_segment =
            DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 2, 5)
                .expect("edge segment creates");
        edge_segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
        });
        edge_segment.edge_deletes.push(SegmentEdge {
            source: 1,
            target: 0,
            type_id: 1,
        });
        edge_segment
            .write_to_path(&edge_path)
            .expect("edge segment writes");
        let node_path = dir.path().join("l1-node.pggraph-delta");
        let mut node_segment =
            DeltaSegment::new(SegmentKind::Node, 1, TraversalDirection::Any, 0, 2, 5)
                .expect("node segment creates");
        node_segment.node_states.push(SegmentNodeState {
            node_idx: 1,
            active: false,
        });
        node_segment
            .write_to_path(&node_path)
            .expect("node segment writes");
        let chunk_path = dir.path().join("base.pggraph-chunk");
        edge_segment
            .write_to_path(&chunk_path)
            .expect("chunk writes");
        let obsolete_path = dir.path().join("old.pggraph-delta");
        std::fs::write(&obsolete_path, b"old").expect("obsolete writes");

        let mut manifest = ProjectionManifest::base_only(3, "base.pggraph", "crc32:base", 1, 5, 9);
        manifest.mark_ingestion();
        manifest.mark_compaction();
        manifest.mark_repair();
        manifest
            .segments
            .push(segment_ref(dir.path(), &edge_path, 0));
        manifest
            .segments
            .push(segment_ref(dir.path(), &node_path, 1));
        manifest
            .base_chunks
            .push(crate::projection::manifest::ManifestChunkRef {
                path: relative_path(dir.path(), &chunk_path),
                checksum: checksum_for_path(&chunk_path),
                source_start: 0,
                source_end: 2,
                dirty_source_count: 2,
                dirty_edge_count: 1,
            });
        manifest.obsolete_files.push(ManifestFileRef {
            path: relative_path(dir.path(), &obsolete_path),
            bytes: 3,
        });
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");
        record_projection_gc(dir.path()).expect("GC timestamp records");

        let status = collect_projection_status(dir.path(), None, 8, 2, 1).expect("status collects");

        assert_eq!(status.manifest_generation, Some(3));
        assert_eq!(status.manifest_watermark, Some(5));
        assert_eq!(status.pending_durable_rows, 3);
        assert_eq!(status.segment_count, 2);
        assert_eq!(status.l0_segment_count, 1);
        assert_eq!(status.l1_segment_count, 1);
        assert_eq!(status.edge_segment_count, 1);
        assert_eq!(status.node_segment_count, 1);
        assert_eq!(status.dirty_chunk_count, 1);
        assert_eq!(status.obsolete_file_count, 1);
        assert_eq!(status.obsolete_bytes, 3);
        assert_eq!(status.active_generation_count, 2);
        assert_eq!(status.artifact_validation_state, "healthy");
        assert_eq!(status.last_ingestion_unix_micros, Some(9));
        assert_eq!(status.last_compaction_unix_micros, Some(9));
        assert!(status.last_gc_unix_micros.is_some());
        assert_eq!(status.last_repair_unix_micros, Some(9));
        assert!(status.tombstone_ratio > 0.0);
        assert!(status.compaction_recommended);
        assert!(status.gc_recommended);
        assert!(status.ingest_recommended);
    }

    fn segment_ref(
        root: &Path,
        path: &Path,
        level: u8,
    ) -> crate::projection::manifest::ManifestSegmentRef {
        crate::projection::manifest::ManifestSegmentRef {
            path: relative_path(root, path),
            checksum: checksum_for_path(path),
            level,
            source_start: 0,
            source_end: 2,
            sync_watermark: 5,
        }
    }

    fn relative_path(root: &Path, path: &Path) -> String {
        path.strip_prefix(root)
            .expect("path is under root")
            .to_string_lossy()
            .into_owned()
    }

    fn checksum_for_path(path: &Path) -> String {
        format!(
            "crc32:{:08x}",
            crc32fast::hash(&std::fs::read(path).expect("file reads"))
        )
    }
}
