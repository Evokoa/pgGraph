//! Durable projection recovery planning and rebuild publication.
//!
//! Recovery validates active manifest metadata and referenced artifacts before
//! deciding whether the projection can keep running, needs targeted chunk
//! repair, or must be rebuilt from PostgreSQL source tables by the SQL layer.

use std::fs;
use std::path::{Path, PathBuf};

use crate::persistence::{
    graph_artifact_checksum_for_path, graph_artifact_version, projection_manifest_root,
};
use crate::projection::chunk::{
    repair_corrupt_base_chunks, BaseChunkRewriteResult, BaseChunkSource,
};
use crate::projection::layered::{ManifestSegmentProvider, SegmentProvider};
use crate::projection::manifest::{
    manifest_file_name, parse_manifest_file_name, ProjectionManifest, ProjectionManifestStore,
};
use crate::safety::{GraphError, GraphResult};

/// Recovery action required for the current durable projection artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionRecoveryAction {
    /// No projection artifacts are present.
    NoProjection,
    /// The active projection manifest and every referenced artifact validate.
    Healthy,
    /// One or more base chunks can be replaced from source table data.
    TargetedChunkRepair,
    /// The projection must be rebuilt from PostgreSQL source tables.
    FullRebuild,
}

/// Recovery inspection result for the active projection artifact root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionRecoveryPlan {
    pub(crate) action: ProjectionRecoveryAction,
    pub(crate) generation_id: Option<u64>,
    pub(crate) reason: Option<String>,
}

impl ProjectionRecoveryPlan {
    fn no_projection() -> Self {
        Self {
            action: ProjectionRecoveryAction::NoProjection,
            generation_id: None,
            reason: None,
        }
    }

    fn healthy(manifest: &ProjectionManifest) -> Self {
        Self {
            action: ProjectionRecoveryAction::Healthy,
            generation_id: Some(manifest.generation_id),
            reason: None,
        }
    }

    fn repair(manifest: &ProjectionManifest, reason: impl Into<String>) -> Self {
        Self {
            action: ProjectionRecoveryAction::TargetedChunkRepair,
            generation_id: Some(manifest.generation_id),
            reason: Some(reason.into()),
        }
    }

    fn rebuild(generation_id: Option<u64>, reason: impl Into<String>) -> Self {
        Self {
            action: ProjectionRecoveryAction::FullRebuild,
            generation_id,
            reason: Some(reason.into()),
        }
    }
}

/// Validate the latest active manifest and every referenced segment/chunk.
pub(crate) fn validate_active_projection(root: &Path) -> GraphResult<Option<ProjectionManifest>> {
    let Some(manifest) = ProjectionManifestStore::new(root).load_latest_current()? else {
        return Ok(None);
    };
    let provider = ManifestSegmentProvider::new(root, &manifest);
    provider.load_segments()?;
    provider.load_base_chunks()?;
    Ok(Some(manifest))
}

/// Decide which recovery action is needed for the current projection root.
pub(crate) fn plan_projection_recovery(root: &Path) -> GraphResult<ProjectionRecoveryPlan> {
    plan_projection_recovery_for_artifact(root, None)
}

/// Decide which recovery action is needed, including base metadata checks.
pub(crate) fn plan_projection_recovery_for_artifact(
    root: &Path,
    graph_path: Option<&Path>,
) -> GraphResult<ProjectionRecoveryPlan> {
    let latest_generation = latest_manifest_generation(root)?;
    let manifest = match ProjectionManifestStore::new(root).load_latest_current_for_recovery() {
        Ok(Some(manifest)) => manifest,
        Ok(None) => return Ok(ProjectionRecoveryPlan::no_projection()),
        Err(err) => {
            return Ok(ProjectionRecoveryPlan::rebuild(
                latest_generation,
                err.to_string(),
            ));
        }
    };

    if let Some(graph_path) = graph_path {
        if let Err(err) = validate_manifest_base_metadata(graph_path, &manifest) {
            return Ok(ProjectionRecoveryPlan::rebuild(
                Some(manifest.generation_id),
                err.to_string(),
            ));
        }
    }

    let provider = ManifestSegmentProvider::new(root, &manifest);
    if let Err(err) = provider.load_segments() {
        return Ok(ProjectionRecoveryPlan::rebuild(
            Some(manifest.generation_id),
            err.to_string(),
        ));
    }
    if let Err(err) = provider.load_base_chunks() {
        return Ok(ProjectionRecoveryPlan::repair(&manifest, err.to_string()));
    }

    Ok(ProjectionRecoveryPlan::healthy(&manifest))
}

/// Repair corrupt active base chunks by publishing a replacement generation.
pub(crate) fn repair_active_base_chunks(
    root: &Path,
    source: &impl BaseChunkSource,
) -> GraphResult<Option<BaseChunkRewriteResult>> {
    let Some(manifest) = ProjectionManifestStore::new(root).load_latest_current_for_recovery()?
    else {
        return Ok(None);
    };
    if manifest.base_chunks.is_empty() {
        return Ok(None);
    }
    let result = repair_corrupt_base_chunks(root, &manifest, source)?;
    Ok(Some(result))
}

/// Publish a fresh base-only manifest after a successful PostgreSQL rebuild.
pub(crate) fn publish_rebuilt_base_manifest(
    graph_path: &Path,
    generation_id: u64,
    sync_watermark: i64,
) -> GraphResult<ProjectionManifest> {
    let root = projection_manifest_root(graph_path);
    let base_artifact_path = graph_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| GraphError::Internal("graph artifact path has no file name".into()))?;
    let mut manifest = ProjectionManifest::base_only(
        generation_id,
        base_artifact_path,
        graph_artifact_checksum_for_path(graph_path)?,
        graph_artifact_version(),
        sync_watermark,
        now_unix_micros()?,
    );
    manifest.mark_repair();
    ProjectionManifestStore::new(root).publish(&manifest)?;
    Ok(manifest)
}

fn validate_manifest_base_metadata(
    graph_path: &Path,
    manifest: &ProjectionManifest,
) -> GraphResult<()> {
    let expected_base = graph_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| GraphError::Internal("graph artifact path has no file name".into()))?;
    if manifest.base_artifact_path != expected_base {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection manifest: base artifact '{}' does not match loaded artifact '{}'",
                manifest.base_artifact_path, expected_base
            ),
        });
    }
    if manifest.base_artifact_version != graph_artifact_version() {
        return Err(GraphError::IncompatibleVersion(format!(
            "projection manifest references base artifact version {}; expected {}",
            manifest.base_artifact_version,
            graph_artifact_version()
        )));
    }
    let expected_checksum = graph_artifact_checksum_for_path(graph_path)?;
    if manifest.base_artifact_checksum != expected_checksum {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection manifest: base artifact checksum '{}' does not match loaded artifact checksum '{}'",
                manifest.base_artifact_checksum, expected_checksum
            ),
        });
    }
    Ok(())
}

/// Return the next generation id after every final manifest file in `root`.
pub(crate) fn next_rebuild_generation_id(root: &Path) -> GraphResult<u64> {
    latest_manifest_generation(root)?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| GraphError::Internal("projection generation id overflowed".into()))
}

/// Move the latest final manifest aside so a full rebuild can reload safely.
pub(crate) fn quarantine_latest_manifest(root: &Path) -> GraphResult<Option<PathBuf>> {
    let Some(generation_id) = latest_manifest_generation(root)? else {
        return Ok(None);
    };
    let path = root.join(manifest_file_name(generation_id));
    if !path.is_file() {
        return Ok(None);
    }
    for attempt in 0..128 {
        let quarantine = root.join(format!(
            "{}.invalid-{attempt}",
            manifest_file_name(generation_id)
        ));
        match fs::rename(&path, &quarantine) {
            Ok(()) => return Ok(Some(quarantine)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(GraphError::Internal(format!(
                    "projection recovery quarantine failed for {}: {err}",
                    path.display()
                )));
            }
        }
    }
    Err(GraphError::Internal(
        "projection recovery quarantine path kept colliding".into(),
    ))
}

/// Restore a manifest previously moved by [`quarantine_latest_manifest`].
pub(crate) fn restore_quarantined_manifest(quarantine_path: &Path) -> GraphResult<()> {
    if !quarantine_path.exists() {
        return Ok(());
    }
    let file_name = quarantine_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            GraphError::Internal("projection quarantine path has no file name".into())
        })?;
    let Some(original_name) = file_name.split(".invalid-").next() else {
        return Err(GraphError::Internal(format!(
            "projection quarantine path has invalid name: {}",
            quarantine_path.display()
        )));
    };
    if original_name == file_name {
        return Err(GraphError::Internal(format!(
            "projection quarantine path has no invalid suffix: {}",
            quarantine_path.display()
        )));
    }
    let original_path = quarantine_path.with_file_name(original_name);
    fs::rename(quarantine_path, &original_path).map_err(|err| {
        GraphError::Internal(format!(
            "projection recovery restore failed for {}: {err}",
            quarantine_path.display()
        ))
    })
}

fn latest_manifest_generation(root: &Path) -> GraphResult<Option<u64>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(GraphError::Internal(format!(
                "projection recovery read artifact directory failed for {}: {err}",
                root.display()
            )));
        }
    };
    let mut latest = None;
    for entry in entries {
        let entry = entry.map_err(|err| {
            GraphError::Internal(format!(
                "projection recovery read artifact entry failed for {}: {err}",
                root.display()
            ))
        })?;
        if !entry
            .file_type()
            .map_err(|err| {
                GraphError::Internal(format!(
                    "projection recovery read artifact file type failed for {}: {err}",
                    entry.path().display()
                ))
            })?
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
        if latest.is_none_or(|current| generation_id > current) {
            latest = Some(generation_id);
        }
    }
    Ok(latest)
}

fn now_unix_micros() -> GraphResult<i64> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?;
    i64::try_from(duration.as_micros())
        .map_err(|_| GraphError::Internal("current timestamp exceeds i64 micros".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::chunk::EdgeStoreChunkSource;
    use crate::projection::manifest::{ManifestChunkRef, ManifestSegmentRef};
    use crate::projection::segment::{DeltaSegment, SegmentEdge, SegmentKind};
    use crate::projection::test_fixtures::{edge_store_from_tuples, ProjectionArtifactDir};
    use crate::types::TraversalDirection;

    #[test]
    fn load_corrupt_active_segment_repairs_or_rebuilds() {
        let dir = ProjectionArtifactDir::new("load_corrupt_active_segment_repairs_or_rebuilds");
        write_file(dir.path().join("base.pggraph"), b"base");
        let segment_path = dir.path().join("active.pggraph-delta");
        let segment = edge_segment(1, 0, &[(0, 1, 1)]);
        segment
            .write_to_path(&segment_path)
            .expect("segment writes");
        let mut manifest = base_manifest(1);
        manifest
            .segments
            .push(segment_ref(dir.path(), &segment_path, "crc32:00000000"));
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");

        let plan = plan_projection_recovery(dir.path()).expect("recovery plans");

        assert_eq!(plan.action, ProjectionRecoveryAction::FullRebuild);
        assert_eq!(plan.generation_id, Some(1));
    }

    #[test]
    fn load_missing_referenced_segment_is_rejected() {
        let dir = ProjectionArtifactDir::new("load_missing_referenced_segment_is_rejected");
        write_file(dir.path().join("base.pggraph"), b"base");
        let mut manifest = base_manifest(1);
        manifest.segments.push(ManifestSegmentRef {
            path: "missing.pggraph-delta".to_string(),
            checksum: "crc32:missing".to_string(),
            level: 0,
            source_start: 0,
            source_end: 1,
            sync_watermark: 1,
        });
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect_err("missing referenced segment rejects");
    }

    #[test]
    fn load_missing_unref_temp_segment_is_ignored() {
        let dir = ProjectionArtifactDir::new("load_missing_unref_temp_segment_is_ignored");
        write_file(dir.path().join("base.pggraph"), b"base");
        write_file(
            dir.path()
                .join("projection-generation-00000000000000000003-segment-00000000.tmp"),
            b"partial",
        );
        ProjectionManifestStore::new(dir.path())
            .publish(&base_manifest(1))
            .expect("manifest publishes");

        let loaded = validate_active_projection(dir.path())
            .expect("validation ignores temp")
            .expect("manifest exists");

        assert_eq!(loaded.generation_id, 1);
    }

    #[test]
    fn base_chunk_corruption_repairs_from_postgresql() {
        let dir = ProjectionArtifactDir::new("base_chunk_corruption_repairs_from_postgresql");
        write_file(dir.path().join("base.pggraph"), b"base");
        let source = edge_store_from_tuples(3, &[(0, 1, 1), (1, 2, 1)]);
        let chunk_path = dir.path().join("active.pggraph-chunk");
        let chunk = edge_segment(1, 0, &[(0, 1, 1)]);
        chunk.write_to_path(&chunk_path).expect("chunk writes");
        let checksum = checksum_for_path(&chunk_path);
        let mut manifest = base_manifest(1);
        manifest.base_chunks.push(ManifestChunkRef {
            path: relative_path(dir.path(), &chunk_path),
            checksum,
            source_start: 0,
            source_end: 2,
            dirty_source_count: 2,
            dirty_edge_count: 2,
        });
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");
        write_file(&chunk_path, b"corrupt");

        let plan = plan_projection_recovery(dir.path()).expect("recovery plans");
        assert_eq!(plan.action, ProjectionRecoveryAction::TargetedChunkRepair);
        assert_eq!(plan.generation_id, Some(1));

        let repaired = repair_active_base_chunks(dir.path(), &EdgeStoreChunkSource::new(&source))
            .expect("chunk repair runs")
            .expect("chunk repair publishes")
            .manifest;

        assert_eq!(repaired.previous_generation_id, Some(1));
        assert_eq!(repaired.base_chunks.len(), 1);
        assert_ne!(repaired.base_chunks[0].path, manifest.base_chunks[0].path);
    }

    #[test]
    fn missing_base_chunk_repairs_from_postgresql_when_metadata_validates() {
        let dir = ProjectionArtifactDir::new(
            "missing_base_chunk_repairs_from_postgresql_when_metadata_validates",
        );
        write_file(dir.path().join("base.pggraph"), b"base");
        let source = edge_store_from_tuples(3, &[(0, 1, 1), (1, 2, 1)]);
        let chunk_path = dir.path().join("missing.pggraph-chunk");
        let mut manifest = base_manifest(1);
        manifest.base_chunks.push(ManifestChunkRef {
            path: relative_path(dir.path(), &chunk_path),
            checksum: "crc32:missing".to_string(),
            source_start: 0,
            source_end: 2,
            dirty_source_count: 2,
            dirty_edge_count: 2,
        });
        write_file(&chunk_path, b"placeholder");
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");
        fs::remove_file(&chunk_path).expect("chunk file removed");

        let plan = plan_projection_recovery(dir.path()).expect("recovery plans");
        assert_eq!(plan.action, ProjectionRecoveryAction::TargetedChunkRepair);
        assert_eq!(plan.generation_id, Some(1));

        let repaired = repair_active_base_chunks(dir.path(), &EdgeStoreChunkSource::new(&source))
            .expect("chunk repair runs")
            .expect("chunk repair publishes")
            .manifest;

        assert_eq!(repaired.previous_generation_id, Some(1));
        assert_eq!(repaired.base_chunks.len(), 1);
        assert!(dir.path().join(&repaired.base_chunks[0].path).exists());
    }

    #[test]
    fn corrupt_manifest_triggers_full_projection_rebuild() {
        let dir = ProjectionArtifactDir::new("corrupt_manifest_triggers_full_projection_rebuild");
        write_file(dir.path().join(manifest_file_name(3)), b"{not json");

        let plan = plan_projection_recovery(dir.path()).expect("recovery plans");

        assert_eq!(plan.action, ProjectionRecoveryAction::FullRebuild);
        assert_eq!(plan.generation_id, Some(3));
    }

    #[test]
    fn full_rebuild_restores_valid_projection_generation() {
        use crate::engine::Engine;
        use crate::persistence::write_graph_file;

        let dir = ProjectionArtifactDir::new("full_rebuild_restores_valid_projection_generation");
        let graph_path = dir.path().join("main.pggraph");
        let mut engine = Engine::new();
        engine.finish_build(None);
        write_graph_file(&engine, &graph_path).expect("base graph writes");
        write_file(dir.path().join(manifest_file_name(4)), b"{not json");

        let generation_id = next_rebuild_generation_id(dir.path()).expect("next generation id");
        let manifest = publish_rebuilt_base_manifest(&graph_path, generation_id, 42)
            .expect("rebuilt projection manifest publishes");
        let loaded = validate_active_projection(dir.path())
            .expect("rebuilt generation validates")
            .expect("rebuilt manifest exists");

        assert_eq!(generation_id, 5);
        assert_eq!(manifest.generation_id, 5);
        assert_eq!(loaded.generation_id, 5);
        assert_eq!(loaded.base_artifact_path, "main.pggraph");
        assert_eq!(loaded.sync_watermark, 42);
    }

    #[test]
    fn stale_base_artifact_checksum_triggers_full_projection_rebuild() {
        use crate::engine::Engine;
        use crate::persistence::write_graph_file;

        let dir = ProjectionArtifactDir::new(
            "stale_base_artifact_checksum_triggers_full_projection_rebuild",
        );
        let graph_path = dir.path().join("main.pggraph");
        let mut engine = Engine::new();
        engine.finish_build(None);
        write_graph_file(&engine, &graph_path).expect("base graph writes");
        let manifest = ProjectionManifest::base_only(1, "main.pggraph", "crc32:stale", 1, 1, 1);
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");

        let plan = plan_projection_recovery_for_artifact(dir.path(), Some(&graph_path))
            .expect("recovery plans");

        assert_eq!(plan.action, ProjectionRecoveryAction::FullRebuild);
        assert_eq!(plan.generation_id, Some(1));
    }

    fn base_manifest(generation_id: u64) -> ProjectionManifest {
        ProjectionManifest::base_only(generation_id, "base.pggraph", "crc32:base", 1, 1, 1)
    }

    fn edge_segment(
        generation_id: u64,
        source_start: u32,
        edges: &[(u32, u32, u8)],
    ) -> DeltaSegment {
        let source_end = edges
            .iter()
            .map(|(source, _, _)| source + 1)
            .max()
            .unwrap_or(source_start + 1);
        let mut segment = DeltaSegment::new(
            SegmentKind::Edge,
            0,
            TraversalDirection::Out,
            source_start,
            source_end,
            i64::try_from(generation_id).expect("generation fits i64"),
        )
        .expect("segment creates");
        for &(source, target, type_id) in edges {
            segment.edge_inserts.push(SegmentEdge {
                source,
                target,
                type_id,
            });
        }
        segment
    }

    fn segment_ref(root: &Path, path: &Path, checksum: &str) -> ManifestSegmentRef {
        ManifestSegmentRef {
            path: relative_path(root, path),
            checksum: checksum.to_string(),
            level: 0,
            source_start: 0,
            source_end: 1,
            sync_watermark: 1,
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
            crc32fast::hash(&fs::read(path).expect("file reads"))
        )
    }

    fn write_file(path: impl Into<PathBuf>, bytes: &[u8]) {
        fs::write(path.into(), bytes).expect("file writes");
    }
}
