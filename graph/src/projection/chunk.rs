//! Base chunk rewrite and repair helpers for durable projections.
//!
//! A base chunk is a checked replacement for a source-node range in the base
//! CSR artifact. Publishing a new manifest with replacement chunks keeps older
//! generations readable while allowing targeted repair and dirty-range rewrites.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::edge_store::{EdgeStore, RawEdge};
use crate::projection::manifest::{
    ManifestChunkRef, ManifestFileRef, ProjectionManifest, ProjectionManifestStore,
};
use crate::projection::segment::{DeltaSegment, SegmentEdge, SegmentEdgeWeight, SegmentKind};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

/// Inclusive/exclusive source-node range covered by one base chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceRange {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

impl SourceRange {
    /// Build a non-empty source range.
    pub(crate) fn new(start: u32, end: u32) -> GraphResult<Self> {
        if start >= end {
            return Err(GraphError::CorruptFile {
                reason: "base chunk source range must be non-empty".to_string(),
            });
        }
        Ok(Self { start, end })
    }

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub(crate) fn contains(self, source: u32) -> bool {
        self.start <= source && source < self.end
    }
}

/// Source of full base edges for a dirty source range.
pub(crate) trait BaseChunkSource {
    /// Total node count represented by this source.
    fn node_count(&self) -> u32;

    /// Return full replacement edges for `range`.
    fn edges_in_range(&self, range: SourceRange) -> GraphResult<Vec<RawEdge>>;
}

/// Base chunk source backed by an [`EdgeStore`].
pub(crate) struct EdgeStoreChunkSource<'a> {
    store: &'a EdgeStore,
}

impl<'a> EdgeStoreChunkSource<'a> {
    pub(crate) fn new(store: &'a EdgeStore) -> Self {
        Self { store }
    }
}

impl BaseChunkSource for EdgeStoreChunkSource<'_> {
    fn node_count(&self) -> u32 {
        self.store.node_count()
    }

    fn edges_in_range(&self, range: SourceRange) -> GraphResult<Vec<RawEdge>> {
        if range.end > self.store.node_count() {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "base chunk range {}..{} exceeds node count {}",
                    range.start,
                    range.end,
                    self.store.node_count()
                ),
            });
        }
        let mut edges = Vec::new();
        for source in range.start..range.end {
            let (targets, type_ids, weights) = self.store.neighbors_weighted(source);
            for (idx, (&target, &type_id)) in targets.iter().zip(type_ids.iter()).enumerate() {
                edges.push(RawEdge {
                    source,
                    target,
                    type_id,
                    weight: weights.get(idx).copied(),
                });
            }
        }
        Ok(edges)
    }
}

/// Result of publishing replacement base chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaseChunkRewriteResult {
    pub(crate) manifest: ProjectionManifest,
    pub(crate) chunks_rewritten: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseChunkRewriteReason {
    None,
    Compaction,
    Repair,
}

/// Publish a new generation with replacement base chunks for dirty ranges.
pub(crate) fn publish_base_chunk_rewrite(
    root: &Path,
    previous: &ProjectionManifest,
    source: &impl BaseChunkSource,
    dirty_ranges: &[SourceRange],
) -> GraphResult<BaseChunkRewriteResult> {
    publish_base_chunk_rewrite_with_segments_and_reason(
        root,
        previous,
        source,
        dirty_ranges,
        previous.segments.clone(),
        BaseChunkRewriteReason::None,
    )
}

pub(crate) fn publish_base_chunk_rewrite_with_segments(
    root: &Path,
    previous: &ProjectionManifest,
    source: &impl BaseChunkSource,
    dirty_ranges: &[SourceRange],
    retained_segments: Vec<crate::projection::manifest::ManifestSegmentRef>,
) -> GraphResult<BaseChunkRewriteResult> {
    publish_base_chunk_rewrite_with_segments_and_reason(
        root,
        previous,
        source,
        dirty_ranges,
        retained_segments,
        BaseChunkRewriteReason::Compaction,
    )
}

fn publish_base_chunk_rewrite_with_segments_and_reason(
    root: &Path,
    previous: &ProjectionManifest,
    source: &impl BaseChunkSource,
    dirty_ranges: &[SourceRange],
    retained_segments: Vec<crate::projection::manifest::ManifestSegmentRef>,
    reason: BaseChunkRewriteReason,
) -> GraphResult<BaseChunkRewriteResult> {
    if dirty_ranges.is_empty() {
        return Ok(BaseChunkRewriteResult {
            manifest: previous.clone(),
            chunks_rewritten: 0,
        });
    }
    let dirty_ranges = expand_dirty_ranges(previous, dirty_ranges)?;
    validate_ranges(source, &dirty_ranges)?;
    let generation_id = previous
        .generation_id
        .checked_add(1)
        .ok_or_else(|| GraphError::Internal("projection generation id overflowed".into()))?;
    let mut rewritten = Vec::new();
    for (chunk_id, range) in dirty_ranges.iter().copied().enumerate() {
        let segment = build_base_chunk_segment(source, range, previous.sync_watermark)?;
        let path = root.join(base_chunk_file_name(generation_id, chunk_id as u32));
        write_chunk_atomically(root, &path, &segment)?;
        rewritten.push(chunk_ref(
            root,
            &path,
            range,
            source.edges_in_range(range)?.len(),
        )?);
    }

    let mut obsolete_files = previous.obsolete_files.clone();
    let mut base_chunks = previous
        .base_chunks
        .iter()
        .filter(|chunk| {
            let range = SourceRange {
                start: chunk.source_start,
                end: chunk.source_end,
            };
            let replaced = dirty_ranges.iter().any(|dirty| dirty.overlaps(range));
            if replaced {
                obsolete_files.push(ManifestFileRef {
                    path: chunk.path.clone(),
                    bytes: 0,
                });
            }
            !replaced
        })
        .cloned()
        .collect::<Vec<_>>();
    let new_chunks = rewritten.clone();
    base_chunks.extend(rewritten);
    base_chunks.sort_by_key(|chunk| (chunk.source_start, chunk.source_end, chunk.path.clone()));

    let mut manifest = ProjectionManifest::base_only(
        generation_id,
        previous.base_artifact_path.clone(),
        previous.base_artifact_checksum.clone(),
        previous.base_artifact_version,
        previous.sync_watermark,
        now_unix_micros()?,
    );
    manifest.previous_generation_id = Some(previous.generation_id);
    manifest.inherit_operation_timestamps(previous);
    match reason {
        BaseChunkRewriteReason::None => {}
        BaseChunkRewriteReason::Compaction => manifest.mark_compaction(),
        BaseChunkRewriteReason::Repair => manifest.mark_repair(),
    }
    manifest.segments = retained_segments;
    manifest.base_chunks = base_chunks;
    manifest.obsolete_files = obsolete_files;
    let store = ProjectionManifestStore::new(root);
    if let Err(err) = store.publish(&manifest) {
        if !store.manifest_path(generation_id).exists() {
            cleanup_chunk_refs(root, &new_chunks)?;
        }
        return Err(err);
    }

    Ok(BaseChunkRewriteResult {
        chunks_rewritten: dirty_ranges.len(),
        manifest,
    })
}

/// Repair corrupted base chunk files by publishing a new replacement generation.
pub(crate) fn repair_corrupt_base_chunks(
    root: &Path,
    manifest: &ProjectionManifest,
    source: &impl BaseChunkSource,
) -> GraphResult<BaseChunkRewriteResult> {
    let dirty_ranges = manifest
        .base_chunks
        .iter()
        .filter_map(|chunk| match chunk_checksum_matches(root, chunk) {
            Ok(true) => None,
            Ok(false) | Err(_) => Some(SourceRange {
                start: chunk.source_start,
                end: chunk.source_end,
            }),
        })
        .collect::<Vec<_>>();
    publish_base_chunk_rewrite_with_segments_and_reason(
        root,
        manifest,
        source,
        &dirty_ranges,
        manifest.segments.clone(),
        BaseChunkRewriteReason::Repair,
    )
}

fn validate_ranges(source: &impl BaseChunkSource, ranges: &[SourceRange]) -> GraphResult<()> {
    for range in ranges {
        if range.end > source.node_count() {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "base chunk range {}..{} exceeds node count {}",
                    range.start,
                    range.end,
                    source.node_count()
                ),
            });
        }
    }
    for (idx, range) in ranges.iter().enumerate() {
        if ranges
            .iter()
            .skip(idx + 1)
            .any(|other| range.overlaps(*other))
        {
            return Err(GraphError::CorruptFile {
                reason: "base chunk dirty ranges must not overlap".to_string(),
            });
        }
    }
    Ok(())
}

fn expand_dirty_ranges(
    previous: &ProjectionManifest,
    dirty_ranges: &[SourceRange],
) -> GraphResult<Vec<SourceRange>> {
    let mut expanded = dirty_ranges.to_vec();
    loop {
        let mut changed = false;
        for chunk in &previous.base_chunks {
            let chunk_range = SourceRange::new(chunk.source_start, chunk.source_end)?;
            let overlapping = expanded
                .iter()
                .copied()
                .filter(|range| range.overlaps(chunk_range))
                .collect::<Vec<_>>();
            if overlapping.is_empty() {
                continue;
            }
            let start = overlapping
                .iter()
                .map(|range| range.start)
                .min()
                .unwrap_or(chunk_range.start)
                .min(chunk_range.start);
            let end = overlapping
                .iter()
                .map(|range| range.end)
                .max()
                .unwrap_or(chunk_range.end)
                .max(chunk_range.end);
            if start == chunk_range.start
                && end == chunk_range.end
                && overlapping.len() == 1
                && overlapping[0] == chunk_range
            {
                continue;
            }
            let union = SourceRange::new(start, end)?;
            expanded.retain(|range| !range.overlaps(union));
            expanded.push(union);
            changed = true;
            break;
        }
        if !changed {
            break;
        }
    }
    expanded.sort_by_key(|range| (range.start, range.end));
    Ok(expanded)
}

fn build_base_chunk_segment(
    source: &impl BaseChunkSource,
    range: SourceRange,
    sync_watermark: i64,
) -> GraphResult<DeltaSegment> {
    let edges = source.edges_in_range(range)?;
    let mut segment = DeltaSegment::new(
        SegmentKind::Edge,
        0,
        TraversalDirection::Out,
        range.start,
        range.end,
        sync_watermark,
    )?;
    for edge in edges {
        segment.edge_inserts.push(SegmentEdge {
            source: edge.source,
            target: edge.target,
            type_id: edge.type_id,
        });
        if let Some(weight) = edge.weight {
            segment.edge_weights.push(SegmentEdgeWeight {
                source: edge.source,
                target: edge.target,
                type_id: edge.type_id,
                weight,
            });
        }
    }
    Ok(segment)
}

fn chunk_ref(
    root: &Path,
    path: &Path,
    range: SourceRange,
    dirty_edge_count: usize,
) -> GraphResult<ManifestChunkRef> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| GraphError::Internal("projection chunk path escaped artifact root".into()))?
        .to_string_lossy()
        .to_string();
    Ok(ManifestChunkRef {
        path: relative,
        checksum: chunk_checksum(path)?,
        source_start: range.start,
        source_end: range.end,
        dirty_source_count: range.end - range.start,
        dirty_edge_count: u32::try_from(dirty_edge_count)
            .map_err(|_| GraphError::Internal("dirty edge count exceeds u32".into()))?,
    })
}

fn chunk_checksum_matches(root: &Path, chunk: &ManifestChunkRef) -> GraphResult<bool> {
    let path = root.join(&chunk.path);
    Ok(path.is_file() && chunk_checksum(&path)? == chunk.checksum)
}

fn chunk_checksum(path: &Path) -> GraphResult<String> {
    let bytes = std::fs::read(path).map_err(|err| {
        GraphError::Internal(format!("read projection base chunk checksum: {err}"))
    })?;
    Ok(format!("crc32:{:08x}", crc32fast::hash(&bytes)))
}

fn cleanup_chunk_refs(root: &Path, chunks: &[ManifestChunkRef]) -> GraphResult<()> {
    let mut removed_any = false;
    for chunk in chunks {
        let path = root.join(&chunk.path);
        match std::fs::remove_file(&path) {
            Ok(()) => removed_any = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(GraphError::Internal(format!(
                    "remove unpublished projection base chunk: {err}"
                )));
            }
        }
    }
    if removed_any {
        sync_directory(root)?;
    }
    Ok(())
}

fn write_chunk_atomically(
    root: &Path,
    final_path: &Path,
    segment: &DeltaSegment,
) -> GraphResult<()> {
    std::fs::create_dir_all(root)
        .map_err(|err| GraphError::Internal(format!("create projection chunk dir: {err}")))?;
    let bytes = segment.to_bytes()?;
    let tmp_path = temp_chunk_path(root, final_path)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|err| GraphError::Internal(format!("create temp projection chunk: {err}")))?;
        file.write_all(&bytes)
            .map_err(|err| GraphError::Internal(format!("write temp projection chunk: {err}")))?;
        file.sync_all()
            .map_err(|err| GraphError::Internal(format!("fsync temp projection chunk: {err}")))?;
        drop(file);
        sync_directory(root)?;
        std::fs::hard_link(&tmp_path, final_path).map_err(|err| {
            GraphError::Internal(format!("publish projection chunk without overwrite: {err}"))
        })?;
        sync_directory(root)?;
        std::fs::remove_file(&tmp_path)
            .map_err(|err| GraphError::Internal(format!("remove temp projection chunk: {err}")))?;
        sync_directory(root)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn temp_chunk_path(root: &Path, final_path: &Path) -> GraphResult<PathBuf> {
    let final_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| GraphError::Internal("projection chunk path has no file name".into()))?;
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
        "projection chunk temp path kept colliding".into(),
    ))
}

fn now_unix_micros() -> GraphResult<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?;
    i64::try_from(duration.as_micros())
        .map_err(|_| GraphError::Internal("system time exceeds i64 micros".into()))
}

fn base_chunk_file_name(generation_id: u64, chunk_id: u32) -> String {
    format!("projection-generation-{generation_id:020}-base-chunk-{chunk_id:08}.pggraph-chunk")
}

fn sync_directory(path: &Path) -> GraphResult<()> {
    let dir = std::fs::File::open(path)
        .map_err(|err| GraphError::Internal(format!("open projection chunk dir: {err}")))?;
    dir.sync_all()
        .map_err(|err| GraphError::Internal(format!("fsync projection chunk dir: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::layered::{LayeredNeighbors, ManifestSegmentProvider};
    use crate::projection::manifest::{
        ManifestChunkRef, ProjectionManifest, ProjectionManifestStore,
    };
    use crate::projection::neighbors::CsrNeighbors;
    use crate::projection::test_fixtures::{
        assert_full_csr_equivalence, edge_store_from_tuples, ProjectionArtifactDir,
    };

    #[test]
    fn base_chunk_manifest_roundtrips_source_node_ranges() {
        let mut manifest = ProjectionManifest::base_only(2, "base.pggraph", "crc32:base", 1, 42, 1);
        manifest.base_chunks.push(ManifestChunkRef {
            path: "projection-generation-00000000000000000002-base-chunk-00000000.pggraph-chunk"
                .to_string(),
            checksum: "crc32:chunk".to_string(),
            source_start: 16,
            source_end: 32,
            dirty_source_count: 16,
            dirty_edge_count: 7,
        });

        let json = manifest.to_pretty_json().expect("manifest encodes");
        let decoded = ProjectionManifest::from_json(&json).expect("manifest decodes");

        assert_eq!(decoded.base_chunks, manifest.base_chunks);
    }

    #[test]
    fn base_chunk_rewrite_preserves_full_rebuild_equivalence() {
        let dir =
            ProjectionArtifactDir::new("base_chunk_rewrite_preserves_full_rebuild_equivalence");
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let previous = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        ProjectionManifestStore::new(dir.path())
            .publish(&previous)
            .expect("base manifest publishes");
        let base = edge_store_from_tuples(4, &[(0, 1, 1), (1, 2, 1), (2, 3, 1)]);
        let rebuilt = edge_store_from_tuples(4, &[(0, 1, 1), (1, 3, 1), (2, 0, 1)]);

        let result = publish_base_chunk_rewrite(
            dir.path(),
            &previous,
            &EdgeStoreChunkSource::new(&rebuilt),
            &[SourceRange::new(1, 3).expect("range")],
        )
        .expect("chunk rewrite publishes");
        let provider = ManifestSegmentProvider::new(dir.path(), &result.manifest);
        let layered = LayeredNeighbors::from_provider(&base, &provider).expect("chunks load");
        let expected = CsrNeighbors::new(&rebuilt);

        assert_eq!(result.chunks_rewritten, 1);
        assert_full_csr_equivalence(4, &expected, &layered);
    }

    #[test]
    fn base_chunk_rewrite_preserves_unchanged_edges_inside_rewritten_range() {
        let dir = ProjectionArtifactDir::new(
            "base_chunk_rewrite_preserves_unchanged_edges_inside_rewritten_range",
        );
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let previous = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        ProjectionManifestStore::new(dir.path())
            .publish(&previous)
            .expect("base manifest publishes");
        let base = edge_store_from_tuples(3, &[(0, 1, 1), (1, 2, 1)]);
        let rebuilt = edge_store_from_tuples(3, &[(0, 1, 1), (1, 2, 1), (1, 0, 1)]);

        let result = publish_base_chunk_rewrite(
            dir.path(),
            &previous,
            &EdgeStoreChunkSource::new(&rebuilt),
            &[SourceRange::new(1, 2).expect("range")],
        )
        .expect("chunk rewrite publishes");
        let provider = ManifestSegmentProvider::new(dir.path(), &result.manifest);
        let layered = LayeredNeighbors::from_provider(&base, &provider).expect("chunks load");
        let expected = CsrNeighbors::new(&rebuilt);

        assert_full_csr_equivalence(3, &expected, &layered);
    }

    #[test]
    fn base_chunk_loader_rejects_non_outbound_chunk_file() {
        let dir = ProjectionArtifactDir::new("base_chunk_loader_rejects_non_outbound_chunk_file");
        let path = dir.path().join("bad.pggraph-chunk");
        let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::In, 0, 1, 10)
            .expect("segment builds");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
        });
        segment.write_to_path(&path).expect("segment writes");
        let mut manifest = ProjectionManifest::base_only(2, "base.pggraph", "crc32:base", 1, 10, 1);
        manifest.base_chunks.push(ManifestChunkRef {
            path: path
                .strip_prefix(dir.path())
                .expect("relative path")
                .to_string_lossy()
                .to_string(),
            checksum: chunk_checksum(&path).expect("chunk checksum"),
            source_start: 0,
            source_end: 1,
            dirty_source_count: 1,
            dirty_edge_count: 1,
        });
        let base = edge_store_from_tuples(2, &[(0, 1, 1)]);
        let provider = ManifestSegmentProvider::new(dir.path(), &manifest);

        let err = match LayeredNeighbors::from_provider(&base, &provider) {
            Ok(_) => panic!("inbound base chunk should reject"),
            Err(err) => err,
        };

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn base_chunk_rewrite_keeps_old_generation_readable() {
        let dir = ProjectionArtifactDir::new("base_chunk_rewrite_keeps_old_generation_readable");
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = edge_store_from_tuples(3, &[(0, 1, 1), (1, 2, 1)]);
        let first_rebuild = edge_store_from_tuples(3, &[(0, 2, 1), (1, 2, 1)]);
        let second_rebuild = edge_store_from_tuples(3, &[(0, 1, 1), (1, 0, 1)]);
        let previous = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        ProjectionManifestStore::new(dir.path())
            .publish(&previous)
            .expect("base manifest publishes");
        let first = publish_base_chunk_rewrite(
            dir.path(),
            &previous,
            &EdgeStoreChunkSource::new(&first_rebuild),
            &[SourceRange::new(0, 1).expect("range")],
        )
        .expect("first chunk publishes")
        .manifest;
        let second = publish_base_chunk_rewrite(
            dir.path(),
            &first,
            &EdgeStoreChunkSource::new(&second_rebuild),
            &[SourceRange::new(1, 2).expect("range")],
        )
        .expect("second chunk publishes")
        .manifest;

        let old_provider = ManifestSegmentProvider::new(dir.path(), &first);
        let old_layered = LayeredNeighbors::from_provider(&base, &old_provider).expect("old loads");
        let old_expected = CsrNeighbors::new(&first_rebuild);
        let new_provider = ManifestSegmentProvider::new(dir.path(), &second);
        let new_layered = LayeredNeighbors::from_provider(&base, &new_provider).expect("new loads");
        let new_expected_store = edge_store_from_tuples(3, &[(0, 2, 1), (1, 0, 1)]);
        let new_expected = CsrNeighbors::new(&new_expected_store);

        assert!(dir.path().join(&first.base_chunks[0].path).is_file());
        assert_full_csr_equivalence(3, &old_expected, &old_layered);
        assert_full_csr_equivalence(3, &new_expected, &new_layered);
    }

    #[test]
    fn base_chunk_rewrite_expands_partial_overlap_with_existing_chunk() {
        let dir = ProjectionArtifactDir::new(
            "base_chunk_rewrite_expands_partial_overlap_with_existing_chunk",
        );
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = edge_store_from_tuples(4, &[(0, 1, 1), (1, 2, 1), (2, 3, 1)]);
        let first_rebuild = edge_store_from_tuples(4, &[(0, 3, 1), (1, 3, 1), (2, 0, 1)]);
        let second_rebuild = edge_store_from_tuples(4, &[(0, 2, 1), (1, 0, 1), (2, 1, 1)]);
        let previous = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        ProjectionManifestStore::new(dir.path())
            .publish(&previous)
            .expect("base manifest publishes");
        let first = publish_base_chunk_rewrite(
            dir.path(),
            &previous,
            &EdgeStoreChunkSource::new(&first_rebuild),
            &[SourceRange::new(0, 3).expect("range")],
        )
        .expect("first chunk publishes")
        .manifest;

        let second = publish_base_chunk_rewrite(
            dir.path(),
            &first,
            &EdgeStoreChunkSource::new(&second_rebuild),
            &[SourceRange::new(1, 2).expect("range")],
        )
        .expect("second chunk publishes")
        .manifest;
        let provider = ManifestSegmentProvider::new(dir.path(), &second);
        let layered = LayeredNeighbors::from_provider(&base, &provider).expect("chunks load");
        let expected = CsrNeighbors::new(&second_rebuild);

        assert_eq!(second.base_chunks[0].source_start, 0);
        assert_eq!(second.base_chunks[0].source_end, 3);
        assert_full_csr_equivalence(4, &expected, &layered);
    }

    #[test]
    fn base_chunk_corruption_triggers_chunk_repair() {
        let dir = ProjectionArtifactDir::new("base_chunk_corruption_triggers_chunk_repair");
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let previous = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        ProjectionManifestStore::new(dir.path())
            .publish(&previous)
            .expect("base manifest publishes");
        let base = edge_store_from_tuples(3, &[(0, 1, 1), (1, 2, 1)]);
        let repaired = edge_store_from_tuples(3, &[(0, 2, 1), (1, 0, 1)]);
        let published = publish_base_chunk_rewrite(
            dir.path(),
            &previous,
            &EdgeStoreChunkSource::new(&repaired),
            &[SourceRange::new(0, 2).expect("range")],
        )
        .expect("chunk publishes")
        .manifest;
        let corrupt_path = dir.path().join(&published.base_chunks[0].path);
        std::fs::write(&corrupt_path, b"corrupt").expect("chunk corrupts");
        let corrupt_provider = ManifestSegmentProvider::new(dir.path(), &published);
        let err = match LayeredNeighbors::from_provider(&base, &corrupt_provider) {
            Ok(_) => panic!("corrupt chunk should reject"),
            Err(err) => err,
        };

        let repair = repair_corrupt_base_chunks(
            dir.path(),
            &published,
            &EdgeStoreChunkSource::new(&repaired),
        )
        .expect("chunk repair publishes");
        let provider = ManifestSegmentProvider::new(dir.path(), &repair.manifest);
        let layered = LayeredNeighbors::from_provider(&base, &provider).expect("repaired loads");
        let expected = CsrNeighbors::new(&repaired);

        assert!(matches!(err, crate::safety::GraphError::CorruptFile { .. }));
        assert_eq!(repair.chunks_rewritten, 1);
        assert_full_csr_equivalence(3, &expected, &layered);
    }
}
