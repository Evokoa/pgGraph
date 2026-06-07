//! Generation-aware durable projection garbage collection.
//!
//! GC deletes only files that a published manifest has declared obsolete and
//! that are no longer referenced by retained or active manifest generations.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::projection::manifest::{
    parse_manifest_file_name, resolve_manifest_reference, ProjectionManifest,
    VALIDATION_STATUS_VALID,
};
use crate::safety::{GraphError, GraphResult};

/// Policy for one projection GC pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionGcConfig {
    /// Minimum number of highest valid manifest generations to retain.
    pub(crate) retained_generation_floor: usize,
}

impl ProjectionGcConfig {
    /// Build GC policy from current PostgreSQL GUCs.
    pub(crate) fn from_gucs() -> Self {
        Self {
            retained_generation_floor: crate::config::projection_retention_generations(),
        }
    }
}

/// Summary of one generation-aware projection GC pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionGcSummary {
    pub(crate) valid_generations_scanned: usize,
    pub(crate) retained_generations: Vec<u64>,
    pub(crate) active_generations: Vec<u64>,
    pub(crate) obsolete_candidates: usize,
    pub(crate) protected_candidates: usize,
    pub(crate) deleted_files: usize,
    pub(crate) deleted_bytes: u64,
}

#[derive(Debug, Clone)]
struct LoadedManifest {
    generation_id: u64,
    manifest: ProjectionManifest,
}

/// Collect obsolete projection files using current GUC policy and heartbeat rows.
pub(crate) fn collect_projection_garbage(root: &Path) -> GraphResult<ProjectionGcSummary> {
    collect_projection_garbage_with_config(root, ProjectionGcConfig::from_gucs())
}

/// Collect obsolete projection files using explicit policy and heartbeat rows.
pub(crate) fn collect_projection_garbage_with_config(
    root: &Path,
    config: ProjectionGcConfig,
) -> GraphResult<ProjectionGcSummary> {
    collect_projection_garbage_with_active_generation_ids(
        root,
        config,
        crate::projection::manifest::active_generation_ids()?,
    )
}

fn collect_projection_garbage_with_active_generation_ids(
    root: &Path,
    config: ProjectionGcConfig,
    active_generation_ids: Vec<u64>,
) -> GraphResult<ProjectionGcSummary> {
    let manifests = load_valid_manifests(root)?;
    let retained_generations = retained_generation_ids(&manifests, config);
    let valid_generation_ids = manifests
        .iter()
        .map(|loaded| loaded.generation_id)
        .collect::<BTreeSet<_>>();
    let active_generations = active_generation_ids.into_iter().collect::<BTreeSet<_>>();
    let missing_active_generations = active_generations
        .difference(&valid_generation_ids)
        .copied()
        .collect::<Vec<_>>();
    if !missing_active_generations.is_empty() {
        return Err(GraphError::Internal(format!(
            "projection GC refused because active generations have no valid manifest: {:?}",
            missing_active_generations
        )));
    }

    let protected_generations = retained_generations
        .iter()
        .chain(active_generations.iter())
        .copied()
        .collect::<BTreeSet<_>>();
    let protected_paths = protected_manifest_references(root, &manifests, &protected_generations)?;
    let mut obsolete_candidates = BTreeSet::new();
    for loaded in &manifests {
        for obsolete in &loaded.manifest.obsolete_files {
            obsolete_candidates.insert(resolve_manifest_reference(root, &obsolete.path)?);
        }
    }
    let obsolete_candidate_count = obsolete_candidates.len();

    let mut protected_candidates = 0usize;
    let mut deleted_files = 0usize;
    let mut deleted_bytes = 0u64;
    for candidate in obsolete_candidates {
        if protected_paths.contains(&candidate) {
            protected_candidates += 1;
            continue;
        }
        let metadata = match fs::metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(gc_io("stat obsolete projection file", &candidate, err)),
        };
        if !metadata.is_file() {
            protected_candidates += 1;
            continue;
        }
        let bytes = metadata.len();
        fs::remove_file(&candidate)
            .map_err(|err| gc_io("delete obsolete projection file", &candidate, err))?;
        deleted_files += 1;
        deleted_bytes = deleted_bytes.saturating_add(bytes);
    }

    crate::projection::status::record_projection_gc(root)?;

    Ok(ProjectionGcSummary {
        valid_generations_scanned: manifests.len(),
        retained_generations,
        active_generations: active_generations.into_iter().collect(),
        obsolete_candidates: obsolete_candidate_count,
        protected_candidates,
        deleted_files,
        deleted_bytes,
    })
}

fn load_valid_manifests(root: &Path) -> GraphResult<Vec<LoadedManifest>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(gc_io("read projection artifact directory", root, err)),
    };
    let mut manifests = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| gc_io("read projection artifact entry", root, err))?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|err| gc_io("read projection artifact file type", &path, err))?
            .is_file()
        {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(file_generation_id) = parse_manifest_file_name(&file_name) else {
            continue;
        };
        let raw = fs::read_to_string(&path)
            .map_err(|err| gc_io("read projection manifest", &path, err))?;
        let Ok(manifest) = ProjectionManifest::from_json(&raw) else {
            continue;
        };
        if manifest.generation_id != file_generation_id
            || manifest.validation_status != VALIDATION_STATUS_VALID
        {
            continue;
        }
        manifests.push(LoadedManifest {
            generation_id: manifest.generation_id,
            manifest,
        });
    }
    manifests.sort_by_key(|loaded| loaded.generation_id);
    Ok(manifests)
}

fn retained_generation_ids(manifests: &[LoadedManifest], config: ProjectionGcConfig) -> Vec<u64> {
    let retained = config.retained_generation_floor.max(1).min(manifests.len());
    manifests
        .iter()
        .rev()
        .take(retained)
        .map(|loaded| loaded.generation_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn protected_manifest_references(
    root: &Path,
    manifests: &[LoadedManifest],
    protected_generations: &BTreeSet<u64>,
) -> GraphResult<BTreeSet<PathBuf>> {
    let mut protected = BTreeSet::new();
    for loaded in manifests {
        if !protected_generations.contains(&loaded.generation_id) {
            continue;
        }
        insert_manifest_references(root, &loaded.manifest, &mut protected)?;
    }
    Ok(protected)
}

fn insert_manifest_references(
    root: &Path,
    manifest: &ProjectionManifest,
    protected: &mut BTreeSet<PathBuf>,
) -> GraphResult<()> {
    protected.insert(resolve_manifest_reference(
        root,
        &manifest.base_artifact_path,
    )?);
    for segment in &manifest.segments {
        protected.insert(resolve_manifest_reference(root, &segment.path)?);
    }
    for chunk in &manifest.base_chunks {
        protected.insert(resolve_manifest_reference(root, &chunk.path)?);
    }
    Ok(())
}

fn gc_io(operation: &str, path: &Path, err: std::io::Error) -> GraphError {
    GraphError::Internal(format!(
        "projection GC {operation} failed for {}: {err}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::manifest::{
        ManifestFileRef, ManifestSegmentRef, ProjectionManifestStore,
    };
    use crate::projection::test_fixtures::ProjectionArtifactDir;

    #[test]
    fn projection_gc_refuses_referenced_files() {
        let dir = ProjectionArtifactDir::new("projection_gc_refuses_referenced_files");
        let old_segment = write_file(dir.path().join("old.pggraph-delta"), b"old");
        let new_segment = write_file(dir.path().join("new.pggraph-delta"), b"new");
        let old = manifest_with_segment(dir.path(), 1, &old_segment, Vec::new());
        let new = manifest_with_segment(
            dir.path(),
            2,
            &new_segment,
            vec![obsolete_ref(dir.path(), &old_segment)],
        );
        publish(dir.path(), &old);
        publish(dir.path(), &new);

        let summary = collect_projection_garbage_with_active_generation_ids(
            dir.path(),
            ProjectionGcConfig {
                retained_generation_floor: 2,
            },
            Vec::new(),
        )
        .expect("gc runs");

        assert!(old_segment.exists());
        assert_eq!(summary.deleted_files, 0);
        assert_eq!(summary.protected_candidates, 1);
        assert_eq!(summary.retained_generations, vec![1, 2]);
    }

    #[test]
    fn projection_gc_refuses_active_generation_files() {
        let dir = ProjectionArtifactDir::new("projection_gc_refuses_active_generation_files");
        let old_segment = write_file(dir.path().join("old-active.pggraph-delta"), b"old");
        let middle_segment = write_file(dir.path().join("middle.pggraph-delta"), b"middle");
        let current_segment = write_file(dir.path().join("current.pggraph-delta"), b"current");
        publish(
            dir.path(),
            &manifest_with_segment(dir.path(), 1, &old_segment, Vec::new()),
        );
        publish(
            dir.path(),
            &manifest_with_segment(
                dir.path(),
                2,
                &middle_segment,
                vec![obsolete_ref(dir.path(), &old_segment)],
            ),
        );
        publish(
            dir.path(),
            &manifest_with_segment(
                dir.path(),
                3,
                &current_segment,
                vec![obsolete_ref(dir.path(), &middle_segment)],
            ),
        );

        let summary = collect_projection_garbage_with_active_generation_ids(
            dir.path(),
            ProjectionGcConfig {
                retained_generation_floor: 1,
            },
            vec![1],
        )
        .expect("gc runs");

        assert!(old_segment.exists());
        assert!(!middle_segment.exists());
        assert!(current_segment.exists());
        assert_eq!(summary.active_generations, vec![1]);
        assert_eq!(summary.deleted_files, 1);
        assert_eq!(summary.protected_candidates, 1);
    }

    #[test]
    fn projection_gc_removes_obsolete_unreferenced_segments_after_retention() {
        let dir = ProjectionArtifactDir::new(
            "projection_gc_removes_obsolete_unreferenced_segments_after_retention",
        );
        let old_segment = write_file(dir.path().join("old-obsolete.pggraph-delta"), b"obsolete");
        let current_segment = write_file(dir.path().join("current-retained.pggraph-delta"), b"now");
        publish(
            dir.path(),
            &manifest_with_segment(dir.path(), 1, &old_segment, Vec::new()),
        );
        publish(
            dir.path(),
            &manifest_with_segment(
                dir.path(),
                2,
                &current_segment,
                vec![obsolete_ref(dir.path(), &old_segment)],
            ),
        );

        let summary = collect_projection_garbage_with_active_generation_ids(
            dir.path(),
            ProjectionGcConfig {
                retained_generation_floor: 1,
            },
            Vec::new(),
        )
        .expect("gc runs");

        assert!(!old_segment.exists());
        assert!(current_segment.exists());
        assert_eq!(summary.deleted_files, 1);
        assert_eq!(summary.deleted_bytes, b"obsolete".len() as u64);
        assert_eq!(summary.retained_generations, vec![2]);

        let second = collect_projection_garbage_with_active_generation_ids(
            dir.path(),
            ProjectionGcConfig {
                retained_generation_floor: 1,
            },
            Vec::new(),
        )
        .expect("gc is idempotent");
        assert_eq!(second.deleted_files, 0);
    }

    #[test]
    fn projection_gc_crash_does_not_invalidate_current_generation() {
        let dir = ProjectionArtifactDir::new(
            "projection_gc_crash_does_not_invalidate_current_generation",
        );
        let old_segment = write_file(dir.path().join("old-crash.pggraph-delta"), b"old");
        let current_segment =
            write_file(dir.path().join("current-crash.pggraph-delta"), b"current");
        publish(
            dir.path(),
            &manifest_with_segment(dir.path(), 1, &old_segment, Vec::new()),
        );
        publish(
            dir.path(),
            &manifest_with_segment(
                dir.path(),
                2,
                &current_segment,
                vec![obsolete_ref(dir.path(), &old_segment)],
            ),
        );

        let summary = collect_projection_garbage_with_active_generation_ids(
            dir.path(),
            ProjectionGcConfig {
                retained_generation_floor: 1,
            },
            Vec::new(),
        )
        .expect("gc runs");
        let current = ProjectionManifestStore::new(dir.path())
            .load_latest_current()
            .expect("current loads")
            .expect("current exists");

        assert_eq!(summary.deleted_files, 1);
        assert_eq!(current.generation_id, 2);
        assert!(current_segment.exists());
        assert!(!old_segment.exists());
    }

    #[test]
    fn projection_gc_refuses_unmatched_active_generation_files() {
        let dir =
            ProjectionArtifactDir::new("projection_gc_refuses_unmatched_active_generation_files");
        let old_segment = write_file(dir.path().join("old-unmatched.pggraph-delta"), b"old");
        let current_segment = write_file(
            dir.path().join("current-unmatched.pggraph-delta"),
            b"current",
        );
        publish(
            dir.path(),
            &manifest_with_segment(dir.path(), 1, &old_segment, Vec::new()),
        );
        publish(
            dir.path(),
            &manifest_with_segment(
                dir.path(),
                2,
                &current_segment,
                vec![obsolete_ref(dir.path(), &old_segment)],
            ),
        );
        fs::remove_file(dir.manifest_path(1)).expect("old manifest is removed");

        let err = collect_projection_garbage_with_active_generation_ids(
            dir.path(),
            ProjectionGcConfig {
                retained_generation_floor: 1,
            },
            vec![1],
        )
        .expect_err("unmatched active generation refuses GC");

        assert!(matches!(err, GraphError::Internal(_)));
        assert!(old_segment.exists());
        assert!(current_segment.exists());
    }

    fn manifest_with_segment(
        root: &Path,
        generation_id: u64,
        segment_path: &Path,
        obsolete_files: Vec<ManifestFileRef>,
    ) -> ProjectionManifest {
        let mut manifest = ProjectionManifest::base_only(
            generation_id,
            "base.pggraph",
            "crc32:base",
            1,
            generation_id as i64,
            generation_id as i64,
        );
        manifest.previous_generation_id = generation_id.checked_sub(1);
        manifest.segments.push(ManifestSegmentRef {
            path: relative_path(root, segment_path),
            checksum: format!("crc32:segment-{generation_id}"),
            level: 0,
            source_start: 0,
            source_end: 1,
            sync_watermark: generation_id as i64,
        });
        manifest.obsolete_files = obsolete_files;
        manifest
    }

    fn publish(root: &Path, manifest: &ProjectionManifest) {
        write_file(root.join("base.pggraph"), b"base");
        ProjectionManifestStore::new(root)
            .publish(manifest)
            .expect("manifest publishes");
    }

    fn obsolete_ref(root: &Path, path: &Path) -> ManifestFileRef {
        ManifestFileRef {
            path: relative_path(root, path),
            bytes: path.metadata().expect("obsolete file exists").len(),
        }
    }

    fn relative_path(root: &Path, path: &Path) -> String {
        path.strip_prefix(root)
            .expect("path is inside artifact root")
            .to_string_lossy()
            .into_owned()
    }

    fn write_file(path: PathBuf, bytes: &[u8]) -> PathBuf {
        fs::write(&path, bytes).expect("test file writes");
        path
    }
}
