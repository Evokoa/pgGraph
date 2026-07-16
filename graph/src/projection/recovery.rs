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
    manifest_file_name, parse_manifest_file_name, ManifestFileRef, ProjectionManifest,
    ProjectionManifestStore,
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
    let store = ProjectionManifestStore::new(root);
    let _reader_lock = store.acquire_reader_lock()?;
    let Some(manifest) = store.load_latest_current()? else {
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
    let store = ProjectionManifestStore::new(root);
    let _reader_lock = store.acquire_reader_lock()?;
    let latest_generation = latest_manifest_generation(root)?;
    let manifest = match store.load_latest_current_for_recovery() {
        Ok(Some(manifest)) => manifest,
        Ok(None) => return Ok(ProjectionRecoveryPlan::no_projection()),
        Err(err) => {
            return Ok(ProjectionRecoveryPlan::rebuild(
                latest_generation,
                err.to_string(),
            ));
        }
    };

    if graph_path.is_some() {
        let current_base_path = root.join(&manifest.base_artifact_path);
        if let Err(err) = validate_manifest_base_metadata(&current_base_path, &manifest) {
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

/// Unpublished generation-specific base planned against one current generation.
///
/// The generated path is confined to the projection root and remains invisible
/// to readers until [`publish_generation_specific_rebuilt_base`] wins the
/// current-generation compare-and-swap.
#[derive(Debug, Clone)]
pub(crate) struct RebuiltBasePublicationPlan {
    root: PathBuf,
    expected_current_generation: Option<u64>,
    predecessor_generation: Option<u64>,
    recovery_publication: bool,
    generation_id: u64,
    candidate_base_name: String,
    candidate_base_path: PathBuf,
    previous: Option<ProjectionManifest>,
}

/// Opaque manifest prepared for one generation-specific rebuilt base.
///
/// Callers may inspect the manifest for candidate loading, but only this
/// module can construct or mutate the value accepted by the publication API.
#[derive(Debug)]
pub(crate) struct PreparedRebuiltBaseManifest {
    manifest: ProjectionManifest,
}

impl PreparedRebuiltBaseManifest {
    /// Borrow the exact manifest that must be validated before publication.
    pub(crate) const fn manifest(&self) -> &ProjectionManifest {
        &self.manifest
    }
}

impl RebuiltBasePublicationPlan {
    /// Return the generation observed before candidate staging began.
    pub(crate) const fn expected_current_generation(&self) -> Option<u64> {
        self.expected_current_generation
    }

    /// Return the generation reserved by this publication plan.
    pub(crate) const fn generation_id(&self) -> u64 {
        self.generation_id
    }

    /// Return the generated root-relative base artifact name.
    pub(crate) fn candidate_base_name(&self) -> &str {
        &self.candidate_base_name
    }

    /// Return the unpublished candidate path under the projection root.
    pub(crate) fn candidate_base_path(&self) -> &Path {
        &self.candidate_base_path
    }

    /// Return whether the current pointer names this plan's generation.
    ///
    /// Callers use this after an ambiguous publication error to avoid deleting
    /// an immutable base that may already be serving readers.
    pub(crate) fn candidate_is_current(&self) -> GraphResult<bool> {
        Ok(
            ProjectionManifestStore::new(&self.root).current_generation_id()?
                == Some(self.generation_id),
        )
    }
}

/// Plan a generation-specific base replacement for a healthy projection root.
///
/// This captures the expected current generation before the caller creates or
/// writes the candidate. The caller must create the returned path with
/// create-new semantics and keep it private until publication succeeds.
///
/// # Errors
///
/// Returns an error when current-generation metadata is corrupt, changes while
/// being captured, the generation counter overflows, or the artifact root
/// cannot be inspected.
pub(crate) fn plan_generation_specific_rebuilt_base(
    root: &Path,
) -> GraphResult<RebuiltBasePublicationPlan> {
    let store = ProjectionManifestStore::new(root);
    let expected_current_generation = store.current_generation_id()?;
    let previous = store.load_latest_metadata()?;
    if previous.as_ref().map(|manifest| manifest.generation_id) != expected_current_generation {
        return Err(GraphError::BuildLocked);
    }
    let generation_id = next_rebuild_generation_id(root)?;
    let candidate_base_name = format!("projection-generation-{generation_id:020}-base.pggraph");
    let candidate_base_path = root.join(&candidate_base_name);
    Ok(RebuiltBasePublicationPlan {
        root: root.to_path_buf(),
        expected_current_generation,
        predecessor_generation: previous.as_ref().map(|manifest| manifest.generation_id),
        recovery_publication: false,
        generation_id,
        candidate_base_name,
        candidate_base_path,
        previous,
    })
}

/// Plan a generation replacement when the current pointer or manifest cannot
/// be decoded safely.
///
/// The generation number remains monotonic across readable manifest filenames,
/// while publication uses the manifest store's raw-pointer recovery switch.
/// No corrupt metadata is trusted for artifact references or timestamps.
pub(crate) fn plan_generation_specific_rebuilt_base_for_recovery(
    root: &Path,
) -> GraphResult<RebuiltBasePublicationPlan> {
    let predecessor_generation = latest_manifest_generation(root)?;
    let generation_id = next_rebuild_generation_id(root)?;
    let candidate_base_name = format!("projection-generation-{generation_id:020}-base.pggraph");
    let candidate_base_path = root.join(&candidate_base_name);
    Ok(RebuiltBasePublicationPlan {
        root: root.to_path_buf(),
        expected_current_generation: None,
        predecessor_generation,
        recovery_publication: true,
        generation_id,
        candidate_base_name,
        candidate_base_path,
        previous: None,
    })
}

/// Prepare the manifest for a staged generation-specific base.
///
/// The candidate must already be complete and fsynced. This function computes
/// its persisted checksum,
/// creates the base-only manifest, inherits operation timestamps, and records
/// all superseded artifacts without changing the current pointer. The caller
/// can therefore pass the returned manifest to the production candidate loader
/// before invoking [`publish_prepared_generation_specific_rebuilt_base`].
///
/// # Errors
///
/// Returns validation or I/O errors when the candidate is missing or invalid,
/// its checksum cannot be read, or manifest metadata cannot be created.
pub(crate) fn prepare_generation_specific_rebuilt_base_manifest(
    plan: &RebuiltBasePublicationPlan,
    sync_watermark: i64,
) -> GraphResult<PreparedRebuiltBaseManifest> {
    if !plan.candidate_base_path.is_file() {
        return Err(GraphError::Internal(format!(
            "rebuilt base candidate does not exist: {}",
            plan.candidate_base_path.display()
        )));
    }
    let mut manifest = ProjectionManifest::base_only(
        plan.generation_id,
        plan.candidate_base_name.clone(),
        graph_artifact_checksum_for_path(&plan.candidate_base_path)?,
        graph_artifact_version(),
        sync_watermark,
        now_unix_micros()?,
    );
    manifest.previous_generation_id = plan.predecessor_generation;
    if let Some(previous) = plan.previous.as_ref() {
        manifest.inherit_operation_timestamps(previous);
        manifest.obsolete_files = previous.obsolete_files.clone();
        append_superseded_projection_files(
            &plan.root,
            previous,
            &manifest.base_artifact_path,
            &mut manifest.obsolete_files,
        );
    }
    Ok(PreparedRebuiltBaseManifest { manifest })
}

/// Publish a prepared rebuilt-base manifest through normal manifest CAS.
///
/// The manifest must be the exact candidate described by `plan`. Its
/// generation, base name, artifact version, checksum, predecessor, and current
/// generation expectation are revalidated immediately before publication.
///
/// # Errors
///
/// Returns [`GraphError::BuildLocked`] when another publisher changed the
/// current generation. Returns [`GraphError::CorruptFile`] when the prepared
/// manifest does not match the plan or the staged candidate bytes. Returns
/// durable manifest publication errors from
/// [`ProjectionManifestStore::publish_if_current`].
pub(crate) fn publish_prepared_generation_specific_rebuilt_base(
    plan: &RebuiltBasePublicationPlan,
    prepared: &PreparedRebuiltBaseManifest,
) -> GraphResult<()> {
    let manifest = prepared.manifest();
    let store = ProjectionManifestStore::new(&plan.root);
    if !plan.recovery_publication
        && store.current_generation_id()? != plan.expected_current_generation
    {
        return Err(GraphError::BuildLocked);
    }
    if manifest.generation_id != plan.generation_id
        || manifest.base_artifact_path != plan.candidate_base_name
        || manifest.base_artifact_version != graph_artifact_version()
        || manifest.previous_generation_id != plan.predecessor_generation
    {
        return Err(GraphError::CorruptFile {
            reason: "prepared rebuilt-base manifest does not match its publication plan".into(),
        });
    }
    let checksum = graph_artifact_checksum_for_path(&plan.candidate_base_path)?;
    if manifest.base_artifact_checksum != checksum {
        return Err(GraphError::CorruptFile {
            reason: "prepared rebuilt-base manifest checksum does not match its candidate".into(),
        });
    }
    if plan.recovery_publication {
        store.publish_for_recovery(manifest)?;
    } else {
        store.publish_if_current(manifest, plan.expected_current_generation)?;
    }
    Ok(())
}

/// Prepare and publish a staged generation-specific base through normal CAS.
///
/// Callers that must validate the candidate through the production loader
/// before publication should use
/// [`prepare_generation_specific_rebuilt_base_manifest`] followed by
/// [`publish_prepared_generation_specific_rebuilt_base`].
///
/// # Errors
///
/// Returns the preparation and CAS publication errors documented by those two
/// functions.
pub(crate) fn publish_generation_specific_rebuilt_base(
    plan: &RebuiltBasePublicationPlan,
    sync_watermark: i64,
) -> GraphResult<ProjectionManifest> {
    let prepared = prepare_generation_specific_rebuilt_base_manifest(plan, sync_watermark)?;
    publish_prepared_generation_specific_rebuilt_base(plan, &prepared)?;
    Ok(prepared.manifest)
}

/// Publish a fresh base-only manifest after a successful PostgreSQL rebuild.
pub(crate) fn publish_rebuilt_base_manifest(
    graph_path: &Path,
    sync_watermark: i64,
) -> GraphResult<ProjectionManifest> {
    let root = projection_manifest_root(graph_path);
    let store = ProjectionManifestStore::new(&root);
    let previous_generation_id = latest_manifest_generation(&root)?;
    let previous = match store.load_latest_metadata() {
        Ok(previous) => previous,
        Err(GraphError::CorruptFile { .. } | GraphError::IncompatibleVersion(_)) => None,
        Err(err) => return Err(err),
    };
    let generation_id = next_rebuild_generation_id(&root)?;
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
    if let Some(previous) = previous.as_ref() {
        manifest.previous_generation_id = Some(previous.generation_id);
        manifest.inherit_operation_timestamps(previous);
        manifest.obsolete_files = previous.obsolete_files.clone();
        append_superseded_projection_files(
            &root,
            previous,
            &manifest.base_artifact_path,
            &mut manifest.obsolete_files,
        );
    } else {
        manifest.previous_generation_id = previous_generation_id;
    }
    store.publish_for_recovery(&manifest)?;
    Ok(manifest)
}

fn append_superseded_projection_files(
    root: &Path,
    previous: &ProjectionManifest,
    replacement_base_path: &str,
    obsolete: &mut Vec<ManifestFileRef>,
) {
    let mut paths = obsolete
        .iter()
        .map(|reference| reference.path.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut push = |path: &str, known_bytes: Option<u64>| {
        if !paths.insert(path.to_string()) {
            return;
        }
        let bytes = known_bytes.unwrap_or_else(|| {
            std::fs::metadata(root.join(path))
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        });
        obsolete.push(ManifestFileRef {
            path: path.to_string(),
            bytes,
        });
    };
    for segment in &previous.segments {
        push(&segment.path, None);
    }
    if let Some(identity) = previous.relationship_identities.as_ref() {
        push(&identity.path, Some(identity.bytes));
    }
    for chunk in &previous.base_chunks {
        push(&chunk.path, None);
    }
    if previous.base_artifact_path != replacement_base_path {
        push(&previous.base_artifact_path, None);
        for sidecar in [".sync", ".projection_mode"] {
            push(&format!("{}{sidecar}", previous.base_artifact_path), None);
        }
    }
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

/// Return the next generation id after every reserved generation in `root`.
///
/// A generation-specific base or sidecar reserves its identifier even when a
/// crash occurs before the manifest is created. Skipping that identifier keeps
/// the next candidate compatible with create-new publication semantics.
pub(crate) fn next_rebuild_generation_id(root: &Path) -> GraphResult<u64> {
    latest_known_generation(root)?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| GraphError::Internal("projection generation id overflowed".into()))
}

/// Return whether the artifact root contains a published or quarantined
/// projection generation that a persisted rebuild must supersede.
pub(crate) fn has_projection_generation(root: &Path) -> GraphResult<bool> {
    latest_known_generation(root).map(|generation| generation.is_some())
}

fn latest_known_generation(root: &Path) -> GraphResult<Option<u64>> {
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
        let generation_id = parse_manifest_file_name(&file_name)
            .or_else(|| parse_rebuilt_base_generation(&file_name))
            .or_else(|| {
                file_name
                    .split_once(".invalid-")
                    .and_then(|(original, _)| parse_manifest_file_name(original))
            });
        if let Some(generation_id) = generation_id {
            if latest.is_none_or(|current| generation_id > current) {
                latest = Some(generation_id);
            }
        }
    }
    Ok(latest)
}

fn parse_rebuilt_base_generation(file_name: &str) -> Option<u64> {
    const PREFIX: &str = "projection-generation-";
    const SUFFIX: &str = "-base.pggraph";
    let base_name = file_name
        .strip_suffix(".sync")
        .or_else(|| file_name.strip_suffix(".projection_mode"))
        .unwrap_or(file_name);
    let generation = base_name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    if generation.len() != 20 || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    generation.parse().ok()
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
        quarantine_latest_manifest(dir.path()).expect("corrupt manifest quarantines");
        let manifest = publish_rebuilt_base_manifest(&graph_path, 42)
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
    fn healthy_rebuild_plan_uses_a_generation_specific_private_base_path() {
        let dir = ProjectionArtifactDir::new(
            "healthy_rebuild_plan_uses_a_generation_specific_private_base_path",
        );
        let plan = plan_generation_specific_rebuilt_base(dir.path())
            .expect("healthy rebuilt-base generation plans");

        assert_eq!(plan.expected_current_generation(), None);
        assert_eq!(plan.generation_id(), 1);
        assert_eq!(
            plan.candidate_base_name(),
            "projection-generation-00000000000000000001-base.pggraph"
        );
        assert_eq!(
            plan.candidate_base_path(),
            dir.path().join(plan.candidate_base_name())
        );
    }

    #[test]
    fn orphan_rebuilt_base_reserves_generation_after_pre_manifest_crash() {
        let dir = ProjectionArtifactDir::new(
            "orphan_rebuilt_base_reserves_generation_after_pre_manifest_crash",
        );
        let interrupted = plan_generation_specific_rebuilt_base(dir.path())
            .expect("interrupted generation plans");
        write_file(interrupted.candidate_base_path(), b"fsynced candidate");

        let resumed = plan_generation_specific_rebuilt_base(dir.path())
            .expect("post-crash generation replans");

        assert_eq!(interrupted.generation_id(), 1);
        assert_eq!(resumed.generation_id(), 2);
        assert_ne!(
            resumed.candidate_base_path(),
            interrupted.candidate_base_path()
        );
    }

    #[test]
    fn rebuilt_base_sidecars_also_reserve_generation_ids() {
        let dir = ProjectionArtifactDir::new("rebuilt_base_sidecars_reserve_generation_ids");
        write_file(
            dir.path()
                .join("projection-generation-00000000000000000008-base.pggraph.projection_mode"),
            b"csr_readonly",
        );

        assert_eq!(
            next_rebuild_generation_id(dir.path()).expect("sidecar generation scans"),
            9
        );
    }

    #[test]
    fn healthy_rebuild_manifest_retains_previous_base_as_obsolete() {
        use crate::engine::Engine;
        use crate::persistence::write_graph_file;

        let dir = ProjectionArtifactDir::new(
            "healthy_rebuild_manifest_retains_previous_base_as_obsolete",
        );
        let old_path = dir
            .path()
            .join("projection-generation-00000000000000000001-base.pggraph");
        let mut engine = Engine::new();
        engine.finish_build(None);
        write_graph_file(&engine, &old_path).expect("old base writes");
        let mut previous = ProjectionManifest::base_only(
            1,
            old_path.file_name().unwrap().to_string_lossy(),
            graph_artifact_checksum_for_path(&old_path).expect("old checksum reads"),
            graph_artifact_version(),
            7,
            1,
        );
        previous.last_ingestion_unix_micros = Some(123);
        ProjectionManifestStore::new(dir.path())
            .publish(&previous)
            .expect("previous generation publishes");

        let plan = plan_generation_specific_rebuilt_base(dir.path())
            .expect("replacement generation plans");
        write_graph_file(&engine, plan.candidate_base_path()).expect("candidate base writes");
        let manifest = prepare_generation_specific_rebuilt_base_manifest(&plan, 9)
            .expect("replacement manifest prepares");
        assert_eq!(
            ProjectionManifestStore::new(dir.path())
                .current_generation_id()
                .expect("current generation reads"),
            Some(1)
        );
        publish_prepared_generation_specific_rebuilt_base(&plan, &manifest)
            .expect("replacement generation publishes");

        assert_eq!(manifest.manifest().previous_generation_id, Some(1));
        assert_eq!(manifest.manifest().last_ingestion_unix_micros, Some(123));
        assert!(manifest.manifest().obsolete_files.iter().any(|reference| {
            reference.path == previous.base_artifact_path
                && reference.bytes == fs::metadata(&old_path).unwrap().len()
        }));
    }

    #[test]
    fn prepared_rebuild_publish_rejects_candidate_changed_after_preparation() {
        use crate::engine::Engine;
        use crate::persistence::write_graph_file;

        let dir = ProjectionArtifactDir::new(
            "prepared_rebuild_publish_rejects_plan_and_checksum_mismatches",
        );
        let base_path = dir.path().join("base.pggraph");
        let mut engine = Engine::new();
        engine.finish_build(None);
        write_graph_file(&engine, &base_path).expect("base writes");
        let first = ProjectionManifest::base_only(
            1,
            "base.pggraph",
            graph_artifact_checksum_for_path(&base_path).expect("base checksum reads"),
            graph_artifact_version(),
            1,
            1,
        );
        let store = ProjectionManifestStore::new(dir.path());
        store.publish(&first).expect("first generation publishes");
        let plan = plan_generation_specific_rebuilt_base(dir.path())
            .expect("replacement generation plans");
        write_graph_file(&engine, plan.candidate_base_path()).expect("candidate base writes");
        let prepared =
            prepare_generation_specific_rebuilt_base_manifest(&plan, 2).expect("manifest prepares");

        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(plan.candidate_base_path())
            .expect("candidate opens")
            .write_all(&[0])
            .expect("candidate mutates");
        assert!(matches!(
            publish_prepared_generation_specific_rebuilt_base(&plan, &prepared),
            Err(GraphError::CorruptFile { .. })
        ));
        assert_eq!(
            store
                .current_generation_id()
                .expect("current generation reads"),
            Some(1)
        );
        write_graph_file(&engine, plan.candidate_base_path()).expect("candidate rewrites");
        let prepared = prepare_generation_specific_rebuilt_base_manifest(&plan, 2)
            .expect("replacement manifest prepares again");
        publish_prepared_generation_specific_rebuilt_base(&plan, &prepared)
            .expect("matching manifest publishes");
    }

    #[test]
    fn healthy_rebuild_cas_loser_preserves_the_winning_generation() {
        use crate::engine::Engine;
        use crate::persistence::write_graph_file;

        let dir = ProjectionArtifactDir::new(
            "healthy_rebuild_cas_loser_preserves_the_winning_generation",
        );
        let base_path = dir.path().join("base.pggraph");
        let mut engine = Engine::new();
        engine.finish_build(None);
        write_graph_file(&engine, &base_path).expect("base writes");
        let first = ProjectionManifest::base_only(
            1,
            "base.pggraph",
            graph_artifact_checksum_for_path(&base_path).expect("base checksum reads"),
            graph_artifact_version(),
            1,
            1,
        );
        let store = ProjectionManifestStore::new(dir.path());
        store.publish(&first).expect("first generation publishes");
        let loser =
            plan_generation_specific_rebuilt_base(dir.path()).expect("losing generation plans");
        write_graph_file(&engine, loser.candidate_base_path()).expect("loser base writes");

        let winner = ProjectionManifest::base_only(
            3,
            "base.pggraph",
            graph_artifact_checksum_for_path(&base_path).expect("winner checksum reads"),
            graph_artifact_version(),
            2,
            2,
        );
        store
            .publish_if_current(&winner, Some(1))
            .expect("winner publishes");

        assert!(matches!(
            publish_generation_specific_rebuilt_base(&loser, 3),
            Err(GraphError::BuildLocked)
        ));
        assert_eq!(
            store
                .load_latest_current()
                .expect("current reads")
                .expect("winner remains")
                .generation_id,
            3
        );
    }

    #[test]
    fn repeated_rebuild_rebases_checksum_and_obsoletes_projection_files() {
        use crate::engine::Engine;
        use crate::persistence::{load_graph_file, write_graph_file};

        let dir = ProjectionArtifactDir::new(
            "repeated_rebuild_rebases_checksum_and_obsoletes_projection_files",
        );
        let graph_path = dir.path().join("main.pggraph");
        let mut engine = Engine::new();
        engine.finish_build(None);
        write_graph_file(&engine, &graph_path).expect("initial base graph writes");

        let segment_path = dir.path().join("superseded.pggraph-delta");
        edge_segment(1, 0, &[(0, 0, 1)])
            .write_to_path(&segment_path)
            .expect("superseded segment writes");
        let mut previous = ProjectionManifest::base_only(
            1,
            "main.pggraph",
            graph_artifact_checksum_for_path(&graph_path).expect("initial checksum reads"),
            graph_artifact_version(),
            7,
            1,
        );
        previous.last_ingestion_unix_micros = Some(123);
        previous
            .segments
            .push(segment_ref(dir.path(), &segment_path, "crc32:00000000"));
        ProjectionManifestStore::new(dir.path())
            .publish(&previous)
            .expect("previous manifest publishes");
        assert_eq!(
            plan_projection_recovery_for_artifact(dir.path(), Some(&graph_path))
                .expect("corrupt segment recovery plans")
                .action,
            ProjectionRecoveryAction::FullRebuild
        );

        crate::sync::sync_insert(&mut engine, 42, "post-build", None)
            .expect("replacement base changes");
        engine.edge_store = crate::edge_store::EdgeStore::from_edges(
            engine.node_store.node_count(),
            Vec::new(),
            false,
        );
        engine.record_applied_sync_id(9);
        write_graph_file(&engine, &graph_path).expect("replacement base graph writes");
        let rebased =
            publish_rebuilt_base_manifest(&graph_path, 9).expect("replacement manifest publishes");
        let loaded = load_graph_file(&graph_path).expect("rebased base reloads");

        assert_eq!(rebased.generation_id, 2);
        assert_eq!(rebased.previous_generation_id, Some(1));
        assert_eq!(rebased.sync_watermark, 9);
        assert_eq!(rebased.last_ingestion_unix_micros, Some(123));
        assert!(rebased.segments.is_empty());
        assert!(rebased.base_chunks.is_empty());
        assert!(rebased.relationship_identities.is_none());
        assert_eq!(rebased.obsolete_files.len(), 1);
        assert_eq!(rebased.obsolete_files[0].path, "superseded.pggraph-delta");
        assert_eq!(
            rebased.base_artifact_checksum,
            graph_artifact_checksum_for_path(&graph_path).expect("replacement checksum reads")
        );
        assert_eq!(loaded.applied_sync_id, 9);
        assert!(loaded.resolve(42, "post-build").is_some());
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
                schema_reversed: false,
                relationship_id: None,
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
