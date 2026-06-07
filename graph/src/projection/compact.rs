//! Durable projection segment compaction.
//!
//! Compaction reduces manifest segment fanout by publishing a new generation
//! whose higher-level artifacts expose the same layered read output.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::edge_store::{EdgeStore, RawEdge};
use crate::projection::chunk::{
    publish_base_chunk_rewrite_with_segments, EdgeStoreChunkSource, SourceRange,
};
use crate::projection::layered::{LayeredNeighbors, ManifestSegmentProvider};
use crate::projection::manifest::{
    ManifestFileRef, ManifestSegmentRef, ProjectionManifest, ProjectionManifestStore,
};
use crate::projection::neighbors::{NeighborSource, WeightedNeighborSource};
use crate::projection::segment::{DeltaSegment, SegmentEdge, SegmentEdgeWeight, SegmentKind};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

/// Bounds for one compaction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactionBudgets {
    pub(crate) max_rows: usize,
    pub(crate) max_bytes: usize,
    pub(crate) max_segments: usize,
    pub(crate) max_elapsed: Duration,
    pub(crate) dirty_chunk_segment_threshold: Option<usize>,
}

impl CompactionBudgets {
    #[cfg(test)]
    fn generous() -> Self {
        Self {
            max_rows: 10_000,
            max_bytes: 10_000_000,
            max_segments: 1_000,
            max_elapsed: Duration::from_secs(60),
            dirty_chunk_segment_threshold: None,
        }
    }
}

/// Result of one compaction publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionResult {
    pub(crate) manifest: ProjectionManifest,
    pub(crate) segments_compacted: usize,
    pub(crate) chunks_rewritten: usize,
}

#[derive(Debug, Clone)]
struct LoadedEdgeSegment {
    reference: ManifestSegmentRef,
    segment: DeltaSegment,
}

/// Compact segment fanout for one manifest generation.
pub(crate) fn compact_generation(
    root: &Path,
    previous: &ProjectionManifest,
    base: &EdgeStore,
    budgets: CompactionBudgets,
) -> GraphResult<CompactionResult> {
    let started = Instant::now();
    let edge_segments = load_edge_segments(root, previous)?;
    if edge_segments.is_empty() {
        return Ok(CompactionResult {
            manifest: previous.clone(),
            segments_compacted: 0,
            chunks_rewritten: 0,
        });
    }
    validate_budgets(&edge_segments, budgets, started)?;
    let ranges = compacted_source_ranges(&edge_segments)?;
    let retained_segments = retained_segments(previous, &edge_segments);
    if budgets
        .dirty_chunk_segment_threshold
        .is_some_and(|threshold| edge_segments.len() >= threshold)
    {
        let final_store = materialize_layered_store(root, previous, base)?;
        let result = publish_base_chunk_rewrite_with_segments(
            root,
            previous,
            &EdgeStoreChunkSource::new(&final_store),
            &ranges,
            retained_segments,
        )?;
        return Ok(CompactionResult {
            manifest: result.manifest,
            segments_compacted: edge_segments.len(),
            chunks_rewritten: result.chunks_rewritten,
        });
    }

    let compacted = build_compacted_segment(root, previous, base, &edge_segments, &ranges)?;
    let generation_id = previous
        .generation_id
        .checked_add(1)
        .ok_or_else(|| GraphError::Internal("projection generation id overflowed".into()))?;
    let segment_path = root.join(compacted_segment_file_name(generation_id, 0));
    write_segment_atomically(root, &segment_path, &compacted)?;
    let segment_ref = segment_ref(root, &segment_path, &compacted)?;
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
    manifest.mark_compaction();
    manifest.base_chunks = previous.base_chunks.clone();
    manifest.segments = retained_segments;
    manifest.segments.push(segment_ref);
    manifest.obsolete_files = previous.obsolete_files.clone();
    manifest
        .obsolete_files
        .extend(edge_segments.iter().map(|loaded| ManifestFileRef {
            path: loaded.reference.path.clone(),
            bytes: 0,
        }));
    let store = ProjectionManifestStore::new(root);
    if let Err(err) = store.publish(&manifest) {
        if !store.manifest_path(generation_id).exists() {
            let _ = std::fs::remove_file(&segment_path);
        }
        return Err(err);
    }

    Ok(CompactionResult {
        manifest,
        segments_compacted: edge_segments.len(),
        chunks_rewritten: 0,
    })
}

fn validate_budgets(
    segments: &[LoadedEdgeSegment],
    budgets: CompactionBudgets,
    started: Instant,
) -> GraphResult<()> {
    if segments.len() > budgets.max_segments {
        return Err(GraphError::OverlayLimit {
            kind: "projection_compaction_segments".to_string(),
            requested: segments.len(),
            limit: budgets.max_segments,
        });
    }
    let rows = segments
        .iter()
        .map(|loaded| segment_row_count(&loaded.segment))
        .sum::<usize>();
    if rows > budgets.max_rows {
        return Err(GraphError::OverlayLimit {
            kind: "projection_compaction_rows".to_string(),
            requested: rows,
            limit: budgets.max_rows,
        });
    }
    let bytes = rows.checked_mul(32).ok_or_else(|| {
        GraphError::Internal("projection compaction byte estimate overflowed".into())
    })?;
    if bytes > budgets.max_bytes {
        return Err(GraphError::OverlayLimit {
            kind: "projection_compaction_bytes".to_string(),
            requested: bytes,
            limit: budgets.max_bytes,
        });
    }
    if started.elapsed() > budgets.max_elapsed {
        return Err(GraphError::OverlayLimit {
            kind: "projection_compaction_elapsed".to_string(),
            requested: started.elapsed().as_micros() as usize,
            limit: budgets.max_elapsed.as_micros() as usize,
        });
    }
    Ok(())
}

fn load_edge_segments(
    root: &Path,
    manifest: &ProjectionManifest,
) -> GraphResult<Vec<LoadedEdgeSegment>> {
    manifest
        .segments
        .iter()
        .filter(|segment| segment.level <= 2)
        .map(|reference| {
            read_manifest_segment(root, reference).map(|segment| LoadedEdgeSegment {
                reference: reference.clone(),
                segment,
            })
        })
        .filter_map(|loaded| match loaded {
            Ok(loaded) if loaded.segment.header.kind == SegmentKind::Edge => Some(Ok(loaded)),
            Ok(_) => None,
            Err(err) => Some(Err(err)),
        })
        .collect()
}

fn retained_segments(
    manifest: &ProjectionManifest,
    compacted: &[LoadedEdgeSegment],
) -> Vec<ManifestSegmentRef> {
    let compacted_paths = compacted
        .iter()
        .map(|loaded| loaded.reference.path.as_str())
        .collect::<BTreeSet<_>>();
    manifest
        .segments
        .iter()
        .filter(|segment| !compacted_paths.contains(segment.path.as_str()))
        .cloned()
        .collect()
}

fn read_manifest_segment(root: &Path, segment: &ManifestSegmentRef) -> GraphResult<DeltaSegment> {
    let path = root.join(&segment.path);
    let bytes = std::fs::read(&path)
        .map_err(|err| GraphError::Internal(format!("projection segment read failed: {err}")))?;
    let checksum = format!("crc32:{:08x}", crc32fast::hash(&bytes));
    if checksum != segment.checksum {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection segment checksum mismatch for {}: expected {}, got {}",
                segment.path, segment.checksum, checksum
            ),
        });
    }
    DeltaSegment::from_bytes(&bytes)
}

fn compacted_source_ranges(segments: &[LoadedEdgeSegment]) -> GraphResult<Vec<SourceRange>> {
    let mut ranges = Vec::<(u32, u32)>::new();
    for segment in segments {
        let start = segment.segment.header.source_start;
        let end = segment.segment.header.source_end;
        if start < end {
            ranges.push((start, end));
        }
    }
    ranges.sort_unstable_by_key(|&(start, end)| (start, end));
    let mut merged = Vec::<SourceRange>::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut() {
            if start < last.end {
                last.end = last.end.max(end);
                continue;
            }
        }
        merged.push(SourceRange::new(start, end)?);
    }
    Ok(merged)
}

fn build_compacted_segment(
    root: &Path,
    previous: &ProjectionManifest,
    base: &EdgeStore,
    segments: &[LoadedEdgeSegment],
    ranges: &[SourceRange],
) -> GraphResult<DeltaSegment> {
    let provider = ManifestSegmentProvider::new(root, previous);
    let layered = LayeredNeighbors::from_provider(base, &provider)?;
    let source_start = ranges.iter().map(|range| range.start).min().unwrap_or(0);
    let source_end = ranges.iter().map(|range| range.end).max().unwrap_or(0);
    let next_level = segments
        .iter()
        .map(|loaded| loaded.segment.header.level)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .min(2);
    let mut compacted = DeltaSegment::new(
        SegmentKind::Edge,
        next_level,
        TraversalDirection::Out,
        source_start,
        source_end,
        previous.sync_watermark,
    )?;
    for range in ranges {
        for source in range.start..range.end {
            let base_edges = edge_set(base, source);
            let final_edges = weighted_edge_map_from_layered(&layered, source);
            for &(target, type_id) in base_edges.keys() {
                if final_edges.contains_key(&(target, type_id)) {
                    continue;
                }
                compacted.edge_deletes.push(SegmentEdge {
                    source,
                    target,
                    type_id,
                });
            }
            for (&(target, type_id), &weight) in final_edges.iter() {
                if base_edges.get(&(target, type_id)) == Some(&weight) {
                    continue;
                }
                compacted.edge_inserts.push(SegmentEdge {
                    source,
                    target,
                    type_id,
                });
                if let Some(weight) = weight {
                    compacted.edge_weights.push(SegmentEdgeWeight {
                        source,
                        target,
                        type_id,
                        weight,
                    });
                }
            }
        }
    }
    Ok(compacted)
}

fn materialize_layered_store(
    root: &Path,
    previous: &ProjectionManifest,
    base: &EdgeStore,
) -> GraphResult<EdgeStore> {
    let provider = ManifestSegmentProvider::new(root, previous);
    let layered = LayeredNeighbors::from_provider(base, &provider)?;
    let mut edges = Vec::new();
    let has_weights = layered.has_weighted_edges();
    for source in 0..base.node_count() {
        let weights = layered
            .weighted_neighbors(source)
            .into_iter()
            .map(|neighbor| ((neighbor.target, neighbor.type_id), neighbor.weight))
            .collect::<BTreeMap<_, _>>();
        edges.extend(layered.neighbors(source).map(|neighbor| RawEdge {
            source,
            target: neighbor.target,
            type_id: neighbor.type_id,
            weight: weights.get(&(neighbor.target, neighbor.type_id)).copied(),
        }));
    }
    EdgeStore::try_from_edges(base.node_count(), edges, has_weights)
}

fn edge_set(store: &EdgeStore, source: u32) -> BTreeMap<(u32, u8), Option<u32>> {
    let (targets, type_ids, weights) = store.neighbors_weighted(source);
    targets
        .iter()
        .zip(type_ids.iter())
        .enumerate()
        .map(|(idx, (&target, &type_id))| {
            (
                (target, type_id),
                store
                    .has_weights()
                    .then(|| weights.get(idx).copied())
                    .flatten(),
            )
        })
        .collect()
}

fn weighted_edge_map_from_layered(
    layered: &LayeredNeighbors<'_>,
    source: u32,
) -> BTreeMap<(u32, u8), Option<u32>> {
    let weights = layered
        .weighted_neighbors(source)
        .into_iter()
        .map(|neighbor| ((neighbor.target, neighbor.type_id), neighbor.weight))
        .collect::<BTreeMap<_, _>>();
    layered
        .neighbors(source)
        .map(|neighbor| {
            (
                (neighbor.target, neighbor.type_id),
                weights.get(&(neighbor.target, neighbor.type_id)).copied(),
            )
        })
        .collect()
}

fn segment_row_count(segment: &DeltaSegment) -> usize {
    segment.edge_inserts.len()
        + segment.edge_deletes.len()
        + segment.edge_weights.len()
        + segment.node_states.len()
        + segment.resolutions.len()
        + segment.filters.len()
        + segment.tenants.len()
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

fn write_segment_atomically(
    root: &Path,
    final_path: &Path,
    segment: &DeltaSegment,
) -> GraphResult<()> {
    std::fs::create_dir_all(root)
        .map_err(|err| GraphError::Internal(format!("create projection segment dir: {err}")))?;
    let tmp_path = temp_segment_path(root, final_path)?;
    let bytes = segment.to_bytes()?;
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

fn compacted_segment_file_name(generation_id: u64, segment_id: u32) -> String {
    format!(
        "projection-generation-{generation_id:020}-compact-segment-{segment_id:08}.pggraph-delta"
    )
}

fn now_unix_micros() -> GraphResult<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?;
    i64::try_from(duration.as_micros())
        .map_err(|_| GraphError::Internal("system time exceeds i64 micros".into()))
}

fn sync_directory(path: &Path) -> GraphResult<()> {
    let dir = std::fs::File::open(path)
        .map_err(|err| GraphError::Internal(format!("open projection segment dir: {err}")))?;
    dir.sync_all()
        .map_err(|err| GraphError::Internal(format!("fsync projection segment dir: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::layered::{LayeredNeighbors, ManifestSegmentProvider};
    use crate::projection::neighbors::CsrNeighbors;
    use crate::projection::segment::SegmentNodeState;
    use crate::projection::test_fixtures::{
        assert_full_csr_equivalence, edge_store_from_tuples, weighted_edge_store_from_tuples,
        ProjectionArtifactDir,
    };

    #[test]
    fn compaction_l0_to_l1_preserves_layered_neighbors() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_l0_to_l1_preserves_layered_neighbors",
            vec![
                segment(1, 0, 0, &[(0, 2, 1)], &[]),
                segment(2, 0, 0, &[(1, 3, 1)], &[]),
            ],
        );

        let result =
            compact_generation(dir.path(), &manifest, &base, CompactionBudgets::generous())
                .expect("compaction publishes");
        let expected_provider = ManifestSegmentProvider::new(dir.path(), &manifest);
        let expected =
            LayeredNeighbors::from_provider(&base, &expected_provider).expect("old loads");
        let actual_provider = ManifestSegmentProvider::new(dir.path(), &result.manifest);
        let actual = LayeredNeighbors::from_provider(&base, &actual_provider).expect("new loads");

        assert_eq!(result.segments_compacted, 2);
        assert_eq!(result.manifest.segments[0].level, 1);
        assert_full_csr_equivalence(base.node_count(), &expected, &actual);
    }

    #[test]
    fn compaction_l1_to_l2_preserves_layered_neighbors() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_l1_to_l2_preserves_layered_neighbors",
            vec![
                segment(1, 1, 0, &[(0, 2, 1)], &[]),
                segment(2, 1, 0, &[(2, 3, 1)], &[]),
            ],
        );

        let result =
            compact_generation(dir.path(), &manifest, &base, CompactionBudgets::generous())
                .expect("compaction publishes");

        assert_eq!(result.manifest.segments[0].level, 2);
        assert_compacted_matches_previous(dir.path(), &base, &manifest, &result.manifest);
    }

    #[test]
    fn compaction_preserves_tombstone_precedence() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_preserves_tombstone_precedence",
            vec![
                segment(1, 0, 0, &[(0, 2, 1)], &[]),
                segment(2, 0, 0, &[], &[(0, 1, 1), (0, 2, 1)]),
            ],
        );

        let result =
            compact_generation(dir.path(), &manifest, &base, CompactionBudgets::generous())
                .expect("compaction publishes");
        let expected_store = edge_store_from_tuples(4, &[(1, 2, 1), (2, 3, 1)]);
        let expected = CsrNeighbors::new(&expected_store);
        let provider = ManifestSegmentProvider::new(dir.path(), &result.manifest);
        let actual = LayeredNeighbors::from_provider(&base, &provider).expect("compacted loads");

        assert_full_csr_equivalence(4, &expected, &actual);
    }

    #[test]
    fn compaction_preserves_non_edge_segments() {
        let mut node_segment =
            DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 4, 3)
                .expect("node segment");
        node_segment.node_states.push(SegmentNodeState {
            node_idx: 2,
            active: false,
        });
        let (dir, base, manifest) = fixture_manifest(
            "compaction_preserves_non_edge_segments",
            vec![segment(1, 0, 0, &[(0, 2, 1)], &[]), node_segment],
        );

        let result =
            compact_generation(dir.path(), &manifest, &base, CompactionBudgets::generous())
                .expect("compaction publishes");
        let provider = ManifestSegmentProvider::new(dir.path(), &result.manifest);
        let actual = LayeredNeighbors::from_provider(&base, &provider).expect("compacted loads");

        assert_eq!(result.manifest.segments.len(), 2);
        assert!(actual.neighbors(2).collect::<Vec<_>>().is_empty());
    }

    #[test]
    fn compaction_preserves_weighted_edge_state() {
        let dir = ProjectionArtifactDir::new("compaction_preserves_weighted_edge_state");
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = weighted_edge_store_from_tuples(4, &[(0, 1, 1, 7)]);
        let mut weighted = segment(1, 0, 0, &[(0, 1, 1), (0, 2, 1)], &[]);
        weighted.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 1,
            type_id: 1,
            weight: 11,
        });
        weighted.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 2,
            type_id: 1,
            weight: 13,
        });
        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        let path = dir.segment_path(1, 0);
        weighted.write_to_path(&path).expect("segment writes");
        manifest
            .segments
            .push(segment_ref(dir.path(), &path, &weighted).expect("segment ref"));
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");

        let result =
            compact_generation(dir.path(), &manifest, &base, CompactionBudgets::generous())
                .expect("compaction publishes");
        let provider = ManifestSegmentProvider::new(dir.path(), &result.manifest);
        let actual = LayeredNeighbors::from_provider(&base, &provider).expect("compacted loads");

        assert_eq!(
            actual.weighted_neighbors(0),
            vec![
                crate::projection::neighbors::WeightedNeighbor {
                    target: 1,
                    type_id: 1,
                    weight: 11,
                },
                crate::projection::neighbors::WeightedNeighbor {
                    target: 2,
                    type_id: 1,
                    weight: 13,
                },
            ]
        );
    }

    #[test]
    fn compaction_dirty_chunk_rewrite_reduces_segment_pressure() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_dirty_chunk_rewrite_reduces_segment_pressure",
            vec![
                segment(1, 0, 0, &[(0, 2, 1)], &[]),
                segment(2, 0, 1, &[(1, 3, 1)], &[]),
            ],
        );
        let budgets = CompactionBudgets {
            dirty_chunk_segment_threshold: Some(2),
            ..CompactionBudgets::generous()
        };

        let result = compact_generation(dir.path(), &manifest, &base, budgets)
            .expect("chunk rewrite publishes");

        assert_eq!(result.chunks_rewritten, 2);
        assert!(result.manifest.segments.is_empty());
        assert_eq!(result.manifest.base_chunks.len(), 2);
        assert_compacted_matches_previous(dir.path(), &base, &manifest, &result.manifest);
    }

    #[test]
    fn compaction_dirty_chunk_rewrite_merges_overlapping_ranges() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_dirty_chunk_rewrite_merges_overlapping_ranges",
            vec![
                segment(1, 0, 0, &[(1, 3, 1)], &[]),
                segment(2, 0, 1, &[(2, 0, 1)], &[]),
            ],
        );
        let budgets = CompactionBudgets {
            dirty_chunk_segment_threshold: Some(2),
            ..CompactionBudgets::generous()
        };

        let result = compact_generation(dir.path(), &manifest, &base, budgets)
            .expect("chunk rewrite publishes");

        assert_eq!(result.chunks_rewritten, 1);
        assert_compacted_matches_previous(dir.path(), &base, &manifest, &result.manifest);
    }

    #[test]
    fn compaction_dirty_chunk_rewrite_preserves_weights_and_non_edge_segments() {
        let dir = ProjectionArtifactDir::new(
            "compaction_dirty_chunk_rewrite_preserves_weights_and_non_edge_segments",
        );
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = weighted_edge_store_from_tuples(4, &[(0, 1, 1, 7), (2, 3, 1, 5)]);
        let mut edge_segment = segment(1, 0, 0, &[(0, 1, 1), (0, 3, 1)], &[]);
        edge_segment.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 1,
            type_id: 1,
            weight: 11,
        });
        edge_segment.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 3,
            type_id: 1,
            weight: 13,
        });
        let mut node_segment =
            DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 4, 2)
                .expect("node segment");
        node_segment.node_states.push(SegmentNodeState {
            node_idx: 2,
            active: false,
        });
        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        for (idx, segment) in [edge_segment, node_segment].into_iter().enumerate() {
            let path = dir.segment_path(1, idx as u32);
            segment.write_to_path(&path).expect("segment writes");
            manifest
                .segments
                .push(segment_ref(dir.path(), &path, &segment).expect("segment ref"));
        }
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");
        let budgets = CompactionBudgets {
            dirty_chunk_segment_threshold: Some(1),
            ..CompactionBudgets::generous()
        };

        let result = compact_generation(dir.path(), &manifest, &base, budgets)
            .expect("chunk rewrite publishes");
        let provider = ManifestSegmentProvider::new(dir.path(), &result.manifest);
        let actual = LayeredNeighbors::from_provider(&base, &provider).expect("compacted loads");

        assert_eq!(result.chunks_rewritten, 1);
        assert_eq!(result.manifest.segments.len(), 1);
        assert_eq!(
            actual.weighted_neighbors(0),
            vec![
                crate::projection::neighbors::WeightedNeighbor {
                    target: 1,
                    type_id: 1,
                    weight: 11,
                },
                crate::projection::neighbors::WeightedNeighbor {
                    target: 3,
                    type_id: 1,
                    weight: 13,
                },
            ]
        );
        assert!(actual.neighbors(2).collect::<Vec<_>>().is_empty());
    }

    #[test]
    fn compaction_interruption_keeps_previous_generation_current() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_interruption_keeps_previous_generation_current",
            vec![segment(1, 0, 0, &[(0, 2, 1)], &[])],
        );
        let budgets = CompactionBudgets {
            max_segments: 0,
            ..CompactionBudgets::generous()
        };

        let err = compact_generation(dir.path(), &manifest, &base, budgets)
            .expect_err("budget interruption rejects");
        let current = ProjectionManifestStore::new(dir.path())
            .load_latest_current()
            .expect("manifest loads")
            .expect("current exists");

        assert!(matches!(err, GraphError::OverlayLimit { .. }));
        assert_eq!(current.generation_id, manifest.generation_id);
    }

    fn fixture_manifest(
        test_name: &str,
        segments: Vec<DeltaSegment>,
    ) -> (ProjectionArtifactDir, EdgeStore, ProjectionManifest) {
        let dir = ProjectionArtifactDir::new(test_name);
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = edge_store_from_tuples(4, &[(0, 1, 1), (1, 2, 1), (2, 3, 1)]);
        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        for (idx, segment) in segments.into_iter().enumerate() {
            let path = dir.segment_path(1, idx as u32);
            segment.write_to_path(&path).expect("segment writes");
            manifest
                .segments
                .push(segment_ref(dir.path(), &path, &segment).expect("segment ref"));
        }
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");
        (dir, base, manifest)
    }

    fn segment(
        watermark: i64,
        level: u8,
        source_start: u32,
        inserts: &[(u32, u32, u8)],
        deletes: &[(u32, u32, u8)],
    ) -> DeltaSegment {
        let source_end = inserts
            .iter()
            .chain(deletes.iter())
            .map(|(source, _, _)| source + 1)
            .max()
            .unwrap_or(source_start + 1);
        let mut segment = DeltaSegment::new(
            SegmentKind::Edge,
            level,
            TraversalDirection::Out,
            source_start,
            source_end,
            watermark,
        )
        .expect("segment builds");
        segment.edge_inserts.extend(
            inserts
                .iter()
                .map(|&(source, target, type_id)| SegmentEdge {
                    source,
                    target,
                    type_id,
                }),
        );
        segment.edge_deletes.extend(
            deletes
                .iter()
                .map(|&(source, target, type_id)| SegmentEdge {
                    source,
                    target,
                    type_id,
                }),
        );
        segment
    }

    fn assert_compacted_matches_previous(
        root: &Path,
        base: &EdgeStore,
        previous: &ProjectionManifest,
        compacted: &ProjectionManifest,
    ) {
        let old_provider = ManifestSegmentProvider::new(root, previous);
        let old = LayeredNeighbors::from_provider(base, &old_provider).expect("old loads");
        let new_provider = ManifestSegmentProvider::new(root, compacted);
        let new = LayeredNeighbors::from_provider(base, &new_provider).expect("new loads");
        assert_full_csr_equivalence(base.node_count(), &old, &new);
    }
}
