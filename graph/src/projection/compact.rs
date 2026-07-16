//! Durable projection segment compaction.
//!
//! Compaction reduces manifest segment fanout by publishing a new generation
//! whose higher-level artifacts expose the same layered read output.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::edge_store::{
    EdgeStore, IdentifiedRawEdge, RawEdge, SortedEdgeStoreBuilder, NO_RELATIONSHIP_ID,
};
use crate::projection::chunk::{
    publish_base_chunk_rewrite_with_segments, EdgeStoreChunkSource, SourceRange,
};
use crate::projection::layered::{LayeredNeighbors, LayeredSnapshot};
use crate::projection::manifest::{
    ManifestFileRef, ManifestSegmentRef, ProjectionManifest, ProjectionManifestStore,
};
use crate::projection::neighbors::{NeighborSource, WeightedNeighborSource};
use crate::projection::segment::{DeltaSegment, SegmentEdge, SegmentEdgeWeight, SegmentKind};
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

type CompactionEdgeKey = (u32, u8, bool, Option<crate::edge_store::RelationshipId>);
const COMPACTION_FILESYSTEM_WORKSPACE_BYTES: usize = 64 * 1024;

/// Bounds for one compaction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactionBudgets {
    pub(crate) resident_bytes: crate::resource::ByteCount,
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
            resident_bytes: crate::resource::ByteCount::ZERO,
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
    let row_budget =
        crate::resource::RowCount::new(u64::try_from(budgets.max_rows).map_err(|_| {
            GraphError::Internal("compaction row budget does not fit u64".to_string())
        })?);
    let governor = crate::resource::maintenance_governor(budgets.resident_bytes, row_budget);
    let mut memory = reserve_compaction_memory(
        &governor,
        COMPACTION_FILESYSTEM_WORKSPACE_BYTES,
        budgets.max_bytes,
    )?;
    let segment_file_bytes = preflight_segment_files(root, previous, budgets)?;
    let (edge_segments, edge_segment_bytes) = load_edge_segments(
        root,
        previous,
        &mut memory,
        COMPACTION_FILESYSTEM_WORKSPACE_BYTES,
        budgets.max_bytes,
    )?;
    if edge_segments.is_empty() {
        let clone_bytes = previous.estimated_heap_bytes()?;
        resize_compaction_memory(
            &mut memory,
            COMPACTION_FILESYSTEM_WORKSPACE_BYTES
                .checked_add(clone_bytes)
                .ok_or_else(|| {
                    compaction_resource_limit(
                        "memory bytes",
                        COMPACTION_FILESYSTEM_WORKSPACE_BYTES,
                        clone_bytes,
                        budgets.max_bytes,
                    )
                })?,
            budgets.max_bytes,
        )?;
        return Ok(CompactionResult {
            manifest: previous.clone(),
            segments_compacted: 0,
            chunks_rewritten: 0,
        });
    }
    validate_budgets(&edge_segments, budgets, started)?;
    let rows = segment_rows(&edge_segments)?;
    governor
        .consume_rows(
            crate::resource::ResourcePhase::CompactionMerge,
            crate::resource::RowCount::new(u64::try_from(rows).map_err(|_| {
                GraphError::Internal("compaction row count does not fit u64".to_string())
            })?),
        )
        .map_err(crate::safety::resource_limit_error)?;
    let (ranges, range_heap_bytes) = compacted_source_ranges(
        &edge_segments,
        &mut memory,
        COMPACTION_FILESYSTEM_WORKSPACE_BYTES
            .checked_add(edge_segment_bytes)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    COMPACTION_FILESYSTEM_WORKSPACE_BYTES,
                    edge_segment_bytes,
                    budgets.max_bytes,
                )
            })?,
        budgets.max_bytes,
    )?;
    let base_chunk_file_bytes = relevant_base_chunk_file_bytes(root, previous, &ranges)?;
    let (base_chunks, base_chunk_heap_bytes) = load_relevant_base_chunks(
        root,
        previous,
        &ranges,
        budgets.max_bytes.saturating_sub(segment_file_bytes),
        &mut memory,
        COMPACTION_FILESYSTEM_WORKSPACE_BYTES
            .checked_add(edge_segment_bytes)
            .and_then(|bytes| bytes.checked_add(range_heap_bytes))
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    COMPACTION_FILESYSTEM_WORKSPACE_BYTES,
                    edge_segment_bytes.saturating_add(range_heap_bytes),
                    budgets.max_bytes,
                )
            })?,
        budgets.max_bytes,
    )?;
    let decoded_bytes = edge_segment_bytes
        .checked_add(base_chunk_heap_bytes)
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                edge_segment_bytes,
                base_chunk_file_bytes,
                budgets.max_bytes,
            )
        })?;
    let decoded_and_ranges = decoded_bytes.checked_add(range_heap_bytes).ok_or_else(|| {
        compaction_resource_limit(
            "memory bytes",
            decoded_bytes,
            range_heap_bytes,
            budgets.max_bytes,
        )
    })?;
    let refs_heap_bytes = base_chunks
        .len()
        .checked_add(edge_segments.len())
        .and_then(|count| count.checked_mul(std::mem::size_of::<&DeltaSegment>()))
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                decoded_and_ranges,
                usize::MAX,
                budgets.max_bytes,
            )
        })?;
    resize_compaction_memory(
        &mut memory,
        decoded_and_ranges
            .checked_add(refs_heap_bytes)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    decoded_and_ranges,
                    refs_heap_bytes,
                    budgets.max_bytes,
                )
            })?,
        budgets.max_bytes,
    )?;
    let mut base_chunk_refs = Vec::new();
    base_chunk_refs
        .try_reserve_exact(base_chunks.len())
        .map_err(compaction_allocation_error)?;
    base_chunk_refs.extend(base_chunks.iter());
    let mut segment_refs = Vec::new();
    segment_refs
        .try_reserve_exact(edge_segments.len())
        .map_err(compaction_allocation_error)?;
    segment_refs.extend(edge_segments.iter().map(|loaded| &loaded.segment));
    let snapshot_plan = LayeredSnapshot::build_plan(&base_chunk_refs, &segment_refs)?;
    let auxiliary_live_bytes = COMPACTION_FILESYSTEM_WORKSPACE_BYTES
        .checked_add(range_heap_bytes)
        .and_then(|bytes| bytes.checked_add(refs_heap_bytes))
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                COMPACTION_FILESYSTEM_WORKSPACE_BYTES,
                refs_heap_bytes,
                budgets.max_bytes,
            )
        })?;
    let decoded_with_auxiliary =
        decoded_bytes
            .checked_add(auxiliary_live_bytes)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    decoded_bytes,
                    auxiliary_live_bytes,
                    budgets.max_bytes,
                )
            })?;
    let snapshot_preflight = decoded_with_auxiliary
        .checked_add(snapshot_plan.estimated_peak_bytes())
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                decoded_with_auxiliary,
                snapshot_plan.estimated_peak_bytes(),
                budgets.max_bytes,
            )
        })?;
    resize_compaction_memory(&mut memory, snapshot_preflight, budgets.max_bytes)?;
    let snapshot = LayeredSnapshot::try_build_from_refs_with_plan(
        base,
        &base_chunk_refs,
        &segment_refs,
        snapshot_plan,
    )?;
    let live_snapshot_bytes = decoded_with_auxiliary
        .checked_add(snapshot.estimated_heap_bytes())
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                decoded_with_auxiliary,
                snapshot.estimated_heap_bytes(),
                budgets.max_bytes,
            )
        })?;
    resize_compaction_memory(&mut memory, live_snapshot_bytes, budgets.max_bytes)?;
    let scratch_bytes = compaction_source_scratch_bytes(
        base,
        &base_chunks,
        &edge_segments,
        &mut memory,
        live_snapshot_bytes,
        budgets.max_bytes,
        CompactionExecution {
            governor: &governor,
            started,
            max_elapsed: budgets.max_elapsed,
        },
    )?;
    let live_with_scratch = live_snapshot_bytes
        .checked_add(scratch_bytes)
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                live_snapshot_bytes,
                scratch_bytes,
                budgets.max_bytes,
            )
        })?;
    resize_compaction_memory(&mut memory, live_with_scratch, budgets.max_bytes)?;
    let layered = LayeredNeighbors::from_snapshot(base, &snapshot);
    let (retained_segments, retained_heap_bytes) = retained_segments_governed(
        previous,
        &edge_segments,
        &mut memory,
        live_with_scratch,
        budgets.max_bytes,
    )?;
    let live_workspace_bytes = live_with_scratch
        .checked_add(retained_heap_bytes)
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                live_with_scratch,
                retained_heap_bytes,
                budgets.max_bytes,
            )
        })?;
    if budgets
        .dirty_chunk_segment_threshold
        .is_some_and(|threshold| edge_segments.len() >= threshold)
    {
        let final_store_plan = plan_materialized_store(
            base,
            &layered,
            &ranges,
            CompactionExecution {
                governor: &governor,
                started,
                max_elapsed: budgets.max_elapsed,
            },
        )?;
        let final_store_preflight = final_store_plan.heap_bytes;
        let rewrite_preflight = live_workspace_bytes
            .checked_add(final_store_preflight)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    live_workspace_bytes,
                    final_store_preflight,
                    budgets.max_bytes,
                )
            })?;
        resize_compaction_memory(&mut memory, rewrite_preflight, budgets.max_bytes)?;
        let final_store = materialize_layered_ranges(
            base,
            &layered,
            &ranges,
            final_store_plan.edge_count,
            &governor,
            started,
            budgets.max_elapsed,
        )?;
        let rewrite_bytes = live_workspace_bytes
            .checked_add(final_store_plan.heap_bytes)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    live_workspace_bytes,
                    final_store_plan.heap_bytes,
                    budgets.max_bytes,
                )
            })?;
        resize_compaction_memory(&mut memory, rewrite_bytes, budgets.max_bytes)?;
        let result = publish_base_chunk_rewrite_with_segments(
            root,
            previous,
            &EdgeStoreChunkSource::new(&final_store),
            &ranges,
            retained_segments,
            &governor,
            budgets.max_bytes,
        )?;
        return Ok(CompactionResult {
            manifest: result.manifest,
            segments_compacted: edge_segments.len(),
            chunks_rewritten: result.chunks_rewritten,
        });
    }

    let compacted_plan = plan_compacted_segment(
        base,
        &layered,
        &ranges,
        CompactionExecution {
            governor: &governor,
            started,
            max_elapsed: budgets.max_elapsed,
        },
    )?;
    let compacted_preflight = compacted_plan.heap_bytes;
    let compacted_preflight_live = live_workspace_bytes
        .checked_add(compacted_preflight)
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                live_workspace_bytes,
                compacted_preflight,
                budgets.max_bytes,
            )
        })?;
    resize_compaction_memory(&mut memory, compacted_preflight_live, budgets.max_bytes)?;
    let compacted = build_compacted_segment(
        previous,
        base,
        &layered,
        &edge_segments,
        &ranges,
        compacted_plan,
        CompactionExecution {
            governor: &governor,
            started,
            max_elapsed: budgets.max_elapsed,
        },
    )?;
    let compacted_heap_bytes = estimated_delta_segment_heap_bytes(&compacted)?;
    let compacted_live_bytes = live_workspace_bytes
        .checked_add(compacted_heap_bytes)
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                live_workspace_bytes,
                compacted_heap_bytes,
                budgets.max_bytes,
            )
        })?;
    resize_compaction_memory(&mut memory, compacted_live_bytes, budgets.max_bytes)?;
    let generation_id = previous
        .generation_id
        .checked_add(1)
        .ok_or_else(|| GraphError::Internal("projection generation id overflowed".into()))?;
    let manifest_publication_bytes = compaction_manifest_publication_upper_bound(
        previous,
        &edge_segments,
        COMPACTION_SOURCE_SCRATCH_FIXED_BYTES,
        budgets.max_bytes,
    )?;
    let publication_live_bytes = compacted_live_bytes
        .checked_add(manifest_publication_bytes)
        .ok_or_else(|| {
            compaction_resource_limit(
                "memory bytes",
                compacted_live_bytes,
                manifest_publication_bytes,
                budgets.max_bytes,
            )
        })?;
    resize_compaction_memory(&mut memory, publication_live_bytes, budgets.max_bytes)?;
    let segment_path = root.join(compacted_segment_file_name(generation_id, 0));
    let checksum = write_segment_atomically(
        root,
        &segment_path,
        &compacted,
        &governor,
        budgets.max_bytes,
    )?;
    let segment_ref = segment_ref_with_checksum(root, &segment_path, &compacted, checksum)?;
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
    manifest.relationship_identities = previous.relationship_identities.clone();
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
    if let Err(err) = store.publish_governed(
        &manifest,
        &governor,
        crate::resource::ResourcePhase::CompactionMerge,
        budgets.max_bytes,
    ) {
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

fn preflight_segment_files(
    root: &Path,
    manifest: &ProjectionManifest,
    budgets: CompactionBudgets,
) -> GraphResult<usize> {
    let mut bytes = 0usize;
    let mut segments = 0usize;
    for segment in manifest
        .segments
        .iter()
        .filter(|segment| segment.level <= 2)
    {
        crate::resource::check_postgres_interrupts();
        segments = segments.checked_add(1).ok_or_else(|| {
            GraphError::Internal("projection compaction segment count overflowed".to_string())
        })?;
        if segments > budgets.max_segments {
            return Err(GraphError::ResourceLimit {
                resource: "segments".to_string(),
                phase: crate::resource::ResourcePhase::CompactionMerge
                    .as_str()
                    .to_string(),
                used: u64::try_from(segments - 1).unwrap_or(u64::MAX),
                requested: 1,
                limit: u64::try_from(budgets.max_segments).unwrap_or(u64::MAX),
            });
        }
        let file_bytes = std::fs::metadata(root.join(&segment.path))
            .map_err(|error| {
                GraphError::Internal(format!("projection segment metadata read failed: {error}"))
            })?
            .len();
        let file_bytes = usize::try_from(file_bytes).map_err(|_| GraphError::ResourceLimit {
            resource: "disk bytes".to_string(),
            phase: crate::resource::ResourcePhase::CompactionMerge
                .as_str()
                .to_string(),
            used: u64::try_from(bytes).unwrap_or(u64::MAX),
            requested: u64::MAX,
            limit: u64::try_from(budgets.max_bytes).unwrap_or(u64::MAX),
        })?;
        bytes = bytes
            .checked_add(file_bytes)
            .ok_or_else(|| GraphError::ResourceLimit {
                resource: "disk bytes".to_string(),
                phase: crate::resource::ResourcePhase::CompactionMerge
                    .as_str()
                    .to_string(),
                used: u64::try_from(bytes).unwrap_or(u64::MAX),
                requested: u64::try_from(file_bytes).unwrap_or(u64::MAX),
                limit: u64::try_from(budgets.max_bytes).unwrap_or(u64::MAX),
            })?;
        if bytes > budgets.max_bytes {
            return Err(GraphError::ResourceLimit {
                resource: "disk bytes".to_string(),
                phase: crate::resource::ResourcePhase::CompactionMerge
                    .as_str()
                    .to_string(),
                used: u64::try_from(bytes - file_bytes).unwrap_or(u64::MAX),
                requested: u64::try_from(file_bytes).unwrap_or(u64::MAX),
                limit: u64::try_from(budgets.max_bytes).unwrap_or(u64::MAX),
            });
        }
    }
    Ok(bytes)
}

fn compaction_resource_limit(
    resource: &str,
    used: usize,
    requested: usize,
    limit: usize,
) -> GraphError {
    GraphError::ResourceLimit {
        resource: resource.to_string(),
        phase: crate::resource::ResourcePhase::CompactionMerge
            .as_str()
            .to_string(),
        used: u64::try_from(used).unwrap_or(u64::MAX),
        requested: u64::try_from(requested).unwrap_or(u64::MAX),
        limit: u64::try_from(limit).unwrap_or(u64::MAX),
    }
}

fn reserve_compaction_memory(
    governor: &crate::resource::ResourceGovernor,
    bytes: usize,
    ceiling: usize,
) -> GraphResult<crate::resource::ResourceLease<'_>> {
    if bytes > ceiling {
        return Err(compaction_resource_limit("memory bytes", 0, bytes, ceiling));
    }
    let bytes = crate::resource::ByteCount::from_usize(bytes)
        .ok_or_else(|| compaction_resource_limit("memory bytes", 0, bytes, ceiling))?;
    governor
        .reserve_memory(crate::resource::ResourcePhase::CompactionMerge, bytes)
        .map_err(crate::safety::resource_limit_error)
}

fn resize_compaction_memory(
    lease: &mut crate::resource::ResourceLease<'_>,
    target: usize,
    ceiling: usize,
) -> GraphResult<()> {
    if target > ceiling {
        return Err(compaction_resource_limit(
            "memory bytes",
            lease.amount().as_u64().min(usize::MAX as u64) as usize,
            target.saturating_sub(lease.amount().as_u64().min(usize::MAX as u64) as usize),
            ceiling,
        ));
    }
    let target = crate::resource::ByteCount::from_usize(target).ok_or_else(|| {
        compaction_resource_limit(
            "memory bytes",
            lease.amount().as_u64().min(usize::MAX as u64) as usize,
            usize::MAX,
            ceiling,
        )
    })?;
    lease
        .try_resize(target)
        .map_err(crate::safety::resource_limit_error)
}

fn relevant_base_chunk_file_bytes(
    root: &Path,
    manifest: &ProjectionManifest,
    ranges: &[SourceRange],
) -> GraphResult<usize> {
    manifest
        .base_chunks
        .iter()
        .filter(|chunk| {
            ranges
                .iter()
                .any(|range| chunk.source_start < range.end && range.start < chunk.source_end)
        })
        .try_fold(0usize, |total, chunk| {
            let bytes = std::fs::metadata(root.join(&chunk.path))
                .map_err(|error| {
                    GraphError::Internal(format!(
                        "projection base chunk metadata read failed: {error}"
                    ))
                })?
                .len();
            let bytes = usize::try_from(bytes).map_err(|_| {
                compaction_resource_limit("memory bytes", total, usize::MAX, usize::MAX)
            })?;
            total
                .checked_add(bytes)
                .ok_or_else(|| compaction_resource_limit("memory bytes", total, bytes, usize::MAX))
        })
}

fn estimated_segments_heap_bytes(segments: &[LoadedEdgeSegment]) -> GraphResult<usize> {
    let wrappers = segments
        .len()
        .checked_mul(std::mem::size_of::<LoadedEdgeSegment>())
        .ok_or_else(|| compaction_resource_limit("memory bytes", 0, usize::MAX, usize::MAX))?;
    segments.iter().try_fold(wrappers, |total, loaded| {
        let reference_strings = loaded
            .reference
            .path
            .capacity()
            .checked_add(loaded.reference.checksum.capacity())
            .ok_or_else(|| {
                compaction_resource_limit("memory bytes", total, usize::MAX, usize::MAX)
            })?;
        let segment = estimated_delta_segment_heap_bytes(&loaded.segment)?;
        total
            .checked_add(reference_strings)
            .and_then(|value| value.checked_add(segment))
            .ok_or_else(|| compaction_resource_limit("memory bytes", total, segment, usize::MAX))
    })
}

fn estimated_delta_segments_heap_bytes(segments: &[DeltaSegment]) -> GraphResult<usize> {
    let wrappers = segments
        .len()
        .checked_mul(std::mem::size_of::<DeltaSegment>())
        .ok_or_else(|| compaction_resource_limit("memory bytes", 0, usize::MAX, usize::MAX))?;
    segments.iter().try_fold(wrappers, |total, segment| {
        let bytes = estimated_delta_segment_heap_bytes(segment)?;
        total
            .checked_add(bytes)
            .ok_or_else(|| compaction_resource_limit("memory bytes", total, bytes, usize::MAX))
    })
}

fn estimated_delta_segment_heap_bytes(segment: &DeltaSegment) -> GraphResult<usize> {
    let fixed = [
        segment
            .edge_inserts
            .capacity()
            .checked_mul(std::mem::size_of::<SegmentEdge>()),
        segment
            .edge_deletes
            .capacity()
            .checked_mul(std::mem::size_of::<SegmentEdge>()),
        segment
            .edge_weights
            .capacity()
            .checked_mul(std::mem::size_of::<SegmentEdgeWeight>()),
        segment
            .node_states
            .capacity()
            .checked_mul(std::mem::size_of::<
                crate::projection::segment::SegmentNodeState,
            >()),
        segment
            .resolutions
            .capacity()
            .checked_mul(std::mem::size_of::<
                crate::projection::segment::SegmentResolution,
            >()),
        segment.filters.capacity().checked_mul(std::mem::size_of::<
            crate::projection::segment::SegmentFilterValue,
        >()),
        segment
            .tenants
            .capacity()
            .checked_mul(std::mem::size_of::<crate::projection::segment::SegmentTenant>()),
    ]
    .into_iter()
    .try_fold(0usize, |total, bytes| {
        let bytes = bytes.ok_or(())?;
        total.checked_add(bytes).ok_or(())
    })
    .map_err(|()| compaction_resource_limit("memory bytes", 0, usize::MAX, usize::MAX))?;
    let strings = segment
        .resolutions
        .iter()
        .filter_map(|row| row.primary_key.as_ref())
        .map(String::capacity)
        .chain(
            segment
                .tenants
                .iter()
                .filter_map(|row| row.tenant.as_ref())
                .map(String::capacity),
        )
        .chain(segment.filters.iter().map(|row| row.value.heap_bytes()))
        .try_fold(0usize, |total, bytes| total.checked_add(bytes).ok_or(()))
        .map_err(|()| compaction_resource_limit("memory bytes", fixed, usize::MAX, usize::MAX))?;
    fixed
        .checked_add(strings)
        .ok_or_else(|| compaction_resource_limit("memory bytes", fixed, strings, usize::MAX))
}

fn compaction_manifest_publication_upper_bound(
    previous: &ProjectionManifest,
    compacted: &[LoadedEdgeSegment],
    filesystem_workspace_bytes: usize,
    max_bytes: usize,
) -> GraphResult<usize> {
    let previous_heap = previous.estimated_heap_bytes()?;
    let replacement_peak = previous_heap
        .checked_mul(2)
        .ok_or_else(|| compaction_resource_limit("memory bytes", 0, usize::MAX, max_bytes))?;
    let obsolete_growth = compacted.iter().try_fold(
        compacted
            .len()
            .checked_mul(std::mem::size_of::<ManifestFileRef>())
            .ok_or_else(|| compaction_resource_limit("memory bytes", 0, usize::MAX, max_bytes))?,
        |bytes, loaded| {
            bytes
                .checked_add(loaded.reference.path.len())
                .ok_or_else(|| {
                    compaction_resource_limit("memory bytes", bytes, usize::MAX, max_bytes)
                })
        },
    )?;
    replacement_peak
        .checked_add(obsolete_growth)
        .and_then(|bytes| bytes.checked_add(filesystem_workspace_bytes))
        .ok_or_else(|| {
            compaction_resource_limit("memory bytes", replacement_peak, usize::MAX, max_bytes)
        })
}

fn segment_rows(segments: &[LoadedEdgeSegment]) -> GraphResult<usize> {
    segments.iter().try_fold(0usize, |total, loaded| {
        let rows = segment_row_count(&loaded.segment);
        total
            .checked_add(rows)
            .ok_or_else(|| compaction_resource_limit("rows", total, rows, usize::MAX))
    })
}

fn load_relevant_base_chunks(
    root: &Path,
    manifest: &ProjectionManifest,
    ranges: &[SourceRange],
    remaining_bytes: usize,
    memory: &mut crate::resource::ResourceLease<'_>,
    existing_live_bytes: usize,
    max_bytes: usize,
) -> GraphResult<(Vec<DeltaSegment>, usize)> {
    let selected_count = manifest
        .base_chunks
        .iter()
        .filter(|chunk| {
            ranges
                .iter()
                .any(|range| chunk.source_start < range.end && range.start < chunk.source_end)
        })
        .count();
    let file_bytes = manifest
        .base_chunks
        .iter()
        .filter(|chunk| {
            ranges
                .iter()
                .any(|range| chunk.source_start < range.end && range.start < chunk.source_end)
        })
        .try_fold(0usize, |total, chunk| {
            let bytes = manifest_file_bytes(root, &chunk.path)?;
            total.checked_add(bytes).ok_or_else(|| {
                compaction_resource_limit("disk bytes", total, bytes, remaining_bytes)
            })
        })?;
    if file_bytes > remaining_bytes {
        return Err(GraphError::ResourceLimit {
            resource: "disk bytes".to_string(),
            phase: crate::resource::ResourcePhase::CompactionMerge
                .as_str()
                .to_string(),
            used: 0,
            requested: file_bytes as u64,
            limit: remaining_bytes as u64,
        });
    }
    let wrapper_bytes = selected_count
        .checked_mul(std::mem::size_of::<DeltaSegment>())
        .ok_or_else(|| {
            compaction_resource_limit("memory bytes", existing_live_bytes, usize::MAX, max_bytes)
        })?;
    let mut retained_bytes = wrapper_bytes;
    resize_compaction_memory(
        memory,
        existing_live_bytes
            .checked_add(retained_bytes)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    existing_live_bytes,
                    retained_bytes,
                    max_bytes,
                )
            })?,
        max_bytes,
    )?;
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(selected_count)
        .map_err(compaction_allocation_error)?;
    for reference in manifest.base_chunks.iter().filter(|chunk| {
        ranges
            .iter()
            .any(|range| chunk.source_start < range.end && range.start < chunk.source_end)
    }) {
        let file_bytes = manifest_file_bytes(root, &reference.path)?;
        let raw_target = existing_live_bytes
            .checked_add(retained_bytes)
            .and_then(|live| live.checked_add(file_bytes))
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    existing_live_bytes + retained_bytes,
                    file_bytes,
                    max_bytes,
                )
            })?;
        resize_compaction_memory(memory, raw_target, max_bytes)?;
        let raw =
            read_manifest_segment_bytes(root, &reference.path, &reference.checksum, file_bytes)?;
        let decoded_upper = DeltaSegment::decoded_heap_upper_bound(&raw)?;
        let decode_target = raw_target.checked_add(decoded_upper).ok_or_else(|| {
            compaction_resource_limit("memory bytes", raw_target, decoded_upper, max_bytes)
        })?;
        resize_compaction_memory(memory, decode_target, max_bytes)?;
        let decoded = DeltaSegment::from_bytes(&raw)?;
        drop(raw);
        let decoded_bytes = estimated_delta_segment_heap_bytes(&decoded)?;
        retained_bytes = retained_bytes.checked_add(decoded_bytes).ok_or_else(|| {
            compaction_resource_limit("memory bytes", retained_bytes, decoded_bytes, max_bytes)
        })?;
        chunks.push(decoded);
        resize_compaction_memory(
            memory,
            existing_live_bytes
                .checked_add(retained_bytes)
                .ok_or_else(|| {
                    compaction_resource_limit(
                        "memory bytes",
                        existing_live_bytes,
                        retained_bytes,
                        max_bytes,
                    )
                })?,
            max_bytes,
        )?;
    }
    Ok((chunks, retained_bytes))
}

fn validate_budgets(
    segments: &[LoadedEdgeSegment],
    budgets: CompactionBudgets,
    started: Instant,
) -> GraphResult<()> {
    if segments.len() > budgets.max_segments {
        return Err(compaction_resource_limit(
            "segments",
            0,
            segments.len(),
            budgets.max_segments,
        ));
    }
    let rows = segment_rows(segments)?;
    if rows > budgets.max_rows {
        return Err(compaction_resource_limit("rows", 0, rows, budgets.max_rows));
    }
    let bytes = rows.checked_mul(32).ok_or_else(|| {
        compaction_resource_limit("memory bytes", 0, usize::MAX, budgets.max_bytes)
    })?;
    if bytes > budgets.max_bytes {
        return Err(compaction_resource_limit(
            "memory bytes",
            0,
            bytes,
            budgets.max_bytes,
        ));
    }
    if started.elapsed() > budgets.max_elapsed {
        let elapsed = started.elapsed().as_micros().min(usize::MAX as u128) as usize;
        let limit = budgets.max_elapsed.as_micros().min(usize::MAX as u128) as usize;
        return Err(compaction_resource_limit(
            "elapsed microseconds",
            elapsed,
            0,
            limit,
        ));
    }
    Ok(())
}

fn load_edge_segments(
    root: &Path,
    manifest: &ProjectionManifest,
    memory: &mut crate::resource::ResourceLease<'_>,
    fixed_live_bytes: usize,
    max_bytes: usize,
) -> GraphResult<(Vec<LoadedEdgeSegment>, usize)> {
    let selected_count = manifest
        .segments
        .iter()
        .filter(|segment| segment.level <= 2)
        .count();
    let wrapper_bytes = selected_count
        .checked_mul(std::mem::size_of::<LoadedEdgeSegment>())
        .ok_or_else(|| compaction_resource_limit("memory bytes", 0, usize::MAX, max_bytes))?;
    resize_compaction_memory(
        memory,
        fixed_live_bytes.checked_add(wrapper_bytes).ok_or_else(|| {
            compaction_resource_limit("memory bytes", fixed_live_bytes, wrapper_bytes, max_bytes)
        })?,
        max_bytes,
    )?;
    let mut retained_bytes = wrapper_bytes;
    let mut loaded = Vec::new();
    loaded
        .try_reserve_exact(selected_count)
        .map_err(compaction_allocation_error)?;
    for reference in manifest
        .segments
        .iter()
        .filter(|segment| segment.level <= 2)
    {
        let file_bytes = manifest_file_bytes(root, &reference.path)?;
        let raw_target = fixed_live_bytes
            .checked_add(retained_bytes)
            .and_then(|bytes| bytes.checked_add(file_bytes))
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    fixed_live_bytes.saturating_add(retained_bytes),
                    file_bytes,
                    max_bytes,
                )
            })?;
        resize_compaction_memory(memory, raw_target, max_bytes)?;
        let raw =
            read_manifest_segment_bytes(root, &reference.path, &reference.checksum, file_bytes)?;
        let decoded_upper = DeltaSegment::decoded_heap_upper_bound(&raw)?;
        let reference_bytes = reference
            .path
            .len()
            .checked_add(reference.checksum.len())
            .ok_or_else(|| {
                compaction_resource_limit("memory bytes", raw_target, usize::MAX, max_bytes)
            })?;
        let decode_target = raw_target
            .checked_add(decoded_upper)
            .and_then(|target| target.checked_add(reference_bytes))
            .ok_or_else(|| {
                compaction_resource_limit("memory bytes", raw_target, decoded_upper, max_bytes)
            })?;
        resize_compaction_memory(memory, decode_target, max_bytes)?;
        let segment = DeltaSegment::from_bytes(&raw)?;
        drop(raw);
        if segment.header.kind == SegmentKind::Edge {
            let segment_bytes = estimated_delta_segment_heap_bytes(&segment)?;
            retained_bytes = retained_bytes
                .checked_add(reference_bytes)
                .and_then(|bytes| bytes.checked_add(segment_bytes))
                .ok_or_else(|| {
                    compaction_resource_limit(
                        "memory bytes",
                        retained_bytes,
                        segment_bytes,
                        max_bytes,
                    )
                })?;
            loaded.push(LoadedEdgeSegment {
                reference: reference.clone(),
                segment,
            });
        }
        resize_compaction_memory(
            memory,
            fixed_live_bytes
                .checked_add(retained_bytes)
                .ok_or_else(|| {
                    compaction_resource_limit(
                        "memory bytes",
                        fixed_live_bytes,
                        retained_bytes,
                        max_bytes,
                    )
                })?,
            max_bytes,
        )?;
    }
    Ok((loaded, retained_bytes))
}

fn retained_segments_governed(
    manifest: &ProjectionManifest,
    compacted: &[LoadedEdgeSegment],
    memory: &mut crate::resource::ResourceLease<'_>,
    existing_live_bytes: usize,
    max_bytes: usize,
) -> GraphResult<(Vec<ManifestSegmentRef>, usize)> {
    let retained_count = manifest
        .segments
        .iter()
        .filter(|segment| {
            !compacted
                .iter()
                .any(|loaded| loaded.reference.path == segment.path)
        })
        .count();
    let heap_bytes = manifest
        .segments
        .iter()
        .filter(|segment| {
            !compacted
                .iter()
                .any(|loaded| loaded.reference.path == segment.path)
        })
        .try_fold(
            retained_count
                .checked_mul(std::mem::size_of::<ManifestSegmentRef>())
                .ok_or_else(|| {
                    compaction_resource_limit(
                        "memory bytes",
                        existing_live_bytes,
                        usize::MAX,
                        max_bytes,
                    )
                })?,
            |bytes, segment| {
                bytes
                    .checked_add(segment.path.len())
                    .and_then(|bytes| bytes.checked_add(segment.checksum.len()))
                    .ok_or_else(|| {
                        compaction_resource_limit(
                            "memory bytes",
                            existing_live_bytes,
                            usize::MAX,
                            max_bytes,
                        )
                    })
            },
        )?;
    resize_compaction_memory(
        memory,
        existing_live_bytes.checked_add(heap_bytes).ok_or_else(|| {
            compaction_resource_limit("memory bytes", existing_live_bytes, heap_bytes, max_bytes)
        })?,
        max_bytes,
    )?;
    let mut retained = Vec::new();
    retained
        .try_reserve_exact(retained_count)
        .map_err(compaction_allocation_error)?;
    retained.extend(
        manifest
            .segments
            .iter()
            .filter(|segment| {
                !compacted
                    .iter()
                    .any(|loaded| loaded.reference.path == segment.path)
            })
            .cloned(),
    );
    Ok((retained, heap_bytes))
}

fn manifest_file_bytes(root: &Path, path: &str) -> GraphResult<usize> {
    usize::try_from(
        std::fs::metadata(root.join(path))
            .map_err(|err| {
                GraphError::Internal(format!("projection segment metadata read failed: {err}"))
            })?
            .len(),
    )
    .map_err(|_| compaction_resource_limit("memory bytes", 0, usize::MAX, usize::MAX))
}

fn read_manifest_segment_bytes(
    root: &Path,
    path: &str,
    expected_checksum: &str,
    expected_bytes: usize,
) -> GraphResult<Vec<u8>> {
    let path = root.join(path);
    let mut file = std::fs::File::open(&path)
        .map_err(|err| GraphError::Internal(format!("projection segment open failed: {err}")))?;
    let mut bytes = vec![0_u8; expected_bytes];
    file.read_exact(&mut bytes)
        .map_err(|err| GraphError::Internal(format!("projection segment read failed: {err}")))?;
    let mut extra = [0_u8; 1];
    if file
        .read(&mut extra)
        .map_err(|err| GraphError::Internal(format!("projection segment read failed: {err}")))?
        != 0
    {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection segment {} changed size while it was being read",
                path.display()
            ),
        });
    }
    let checksum = crc32fast::hash(&bytes);
    let checksum_matches = expected_checksum
        .strip_prefix("crc32:")
        .and_then(|value| u32::from_str_radix(value, 16).ok())
        == Some(checksum);
    if !checksum_matches {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection segment checksum mismatch for {}: expected {}, got crc32:{checksum:08x}",
                path.display(),
                expected_checksum
            ),
        });
    }
    Ok(bytes)
}

fn compacted_source_ranges(
    segments: &[LoadedEdgeSegment],
    memory: &mut crate::resource::ResourceLease<'_>,
    existing_live_bytes: usize,
    max_bytes: usize,
) -> GraphResult<(Vec<SourceRange>, usize)> {
    let range_capacity_bytes = segments
        .len()
        .checked_mul(std::mem::size_of::<(u32, u32)>())
        .and_then(|bytes| {
            segments
                .len()
                .checked_mul(std::mem::size_of::<SourceRange>())
                .and_then(|merged| bytes.checked_add(merged))
        })
        .ok_or_else(|| {
            compaction_resource_limit("memory bytes", existing_live_bytes, usize::MAX, max_bytes)
        })?;
    resize_compaction_memory(
        memory,
        existing_live_bytes
            .checked_add(range_capacity_bytes)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    existing_live_bytes,
                    range_capacity_bytes,
                    max_bytes,
                )
            })?,
        max_bytes,
    )?;
    let mut ranges = Vec::<(u32, u32)>::new();
    ranges
        .try_reserve_exact(segments.len())
        .map_err(compaction_allocation_error)?;
    for segment in segments {
        let start = segment.segment.header.source_start;
        let end = segment.segment.header.source_end;
        if start < end {
            ranges.push((start, end));
        }
    }
    ranges.sort_unstable_by_key(|&(start, end)| (start, end));
    let mut merged = Vec::<SourceRange>::new();
    merged
        .try_reserve_exact(segments.len())
        .map_err(compaction_allocation_error)?;
    for (start, end) in ranges.drain(..) {
        if let Some(last) = merged.last_mut() {
            if start < last.end {
                last.end = last.end.max(end);
                continue;
            }
        }
        merged.push(SourceRange::new(start, end)?);
    }
    drop(ranges);
    let retained_bytes = merged
        .capacity()
        .checked_mul(std::mem::size_of::<SourceRange>())
        .ok_or_else(|| {
            compaction_resource_limit("memory bytes", existing_live_bytes, usize::MAX, max_bytes)
        })?;
    resize_compaction_memory(
        memory,
        existing_live_bytes
            .checked_add(retained_bytes)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    existing_live_bytes,
                    retained_bytes,
                    max_bytes,
                )
            })?,
        max_bytes,
    )?;
    Ok((merged, retained_bytes))
}

#[derive(Clone, Copy)]
struct CompactionExecution<'a> {
    governor: &'a crate::resource::ResourceGovernor,
    started: Instant,
    max_elapsed: Duration,
}

const COMPACTION_SOURCE_SCRATCH_FIXED_BYTES: usize = 64 * 1024;
// One source can transiently coexist in the layered merge map, its owned
// iterator, weighted lookup maps, and the base/final comparison maps. A 1.5 KiB
// allowance per possible edge is deliberately larger than the sum of their
// key/value layouts and B-tree node slack on supported 64-bit targets.
const COMPACTION_SOURCE_SCRATCH_BYTES_PER_EDGE: usize = 1_536;

fn compaction_allocation_error(_error: std::collections::TryReserveError) -> GraphError {
    GraphError::ResourceLimit {
        resource: "memory bytes".to_string(),
        phase: crate::resource::ResourcePhase::CompactionMerge
            .as_str()
            .to_string(),
        used: 0,
        requested: u64::MAX,
        limit: u64::MAX,
    }
}

fn compaction_source_scratch_bytes(
    base: &EdgeStore,
    base_chunks: &[DeltaSegment],
    segments: &[LoadedEdgeSegment],
    memory: &mut crate::resource::ResourceLease<'_>,
    existing_live_bytes: usize,
    max_bytes: usize,
    execution: CompactionExecution<'_>,
) -> GraphResult<usize> {
    let insert_count = base_chunks
        .iter()
        .map(|segment| segment.edge_inserts.len())
        .chain(
            segments
                .iter()
                .map(|loaded| loaded.segment.edge_inserts.len()),
        )
        .try_fold(0usize, |total, rows| {
            total
                .checked_add(rows)
                .ok_or_else(|| compaction_resource_limit("memory bytes", total, rows, max_bytes))
        })?;
    let source_bytes = insert_count
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| {
            compaction_resource_limit("memory bytes", existing_live_bytes, usize::MAX, max_bytes)
        })?;
    resize_compaction_memory(
        memory,
        existing_live_bytes
            .checked_add(source_bytes)
            .ok_or_else(|| {
                compaction_resource_limit(
                    "memory bytes",
                    existing_live_bytes,
                    source_bytes,
                    max_bytes,
                )
            })?,
        max_bytes,
    )?;
    let mut inserted_sources = Vec::new();
    inserted_sources
        .try_reserve_exact(insert_count)
        .map_err(compaction_allocation_error)?;
    inserted_sources.extend(
        base_chunks
            .iter()
            .flat_map(|segment| segment.edge_inserts.iter().map(|edge| edge.source)),
    );
    inserted_sources.extend(
        segments
            .iter()
            .flat_map(|loaded| loaded.segment.edge_inserts.iter().map(|edge| edge.source)),
    );
    inserted_sources.sort_unstable();

    let mut max_degree = 0usize;
    for source in 0..base.node_count() {
        compaction_tick(execution.governor, execution.started, execution.max_elapsed)?;
        max_degree = max_degree.max(base.neighbors(source).0.len());
    }
    let mut index = 0usize;
    while index < inserted_sources.len() {
        let source = inserted_sources[index];
        let mut end = index + 1;
        while end < inserted_sources.len() && inserted_sources[end] == source {
            end += 1;
        }
        let inserts = end - index;
        let degree = base
            .neighbors(source)
            .0
            .len()
            .checked_add(inserts)
            .ok_or_else(|| compaction_resource_limit("memory bytes", 0, usize::MAX, max_bytes))?;
        max_degree = max_degree.max(degree);
        index = end;
    }
    drop(inserted_sources);
    max_degree
        .checked_mul(COMPACTION_SOURCE_SCRATCH_BYTES_PER_EDGE)
        .and_then(|bytes| bytes.checked_add(COMPACTION_SOURCE_SCRATCH_FIXED_BYTES))
        .ok_or_else(|| {
            compaction_resource_limit("memory bytes", existing_live_bytes, usize::MAX, max_bytes)
        })
}

#[derive(Debug, Clone, Copy)]
struct MaterializedStorePlan {
    edge_count: usize,
    heap_bytes: usize,
}

fn plan_materialized_store(
    base: &EdgeStore,
    layered: &LayeredNeighbors<'_>,
    ranges: &[SourceRange],
    execution: CompactionExecution<'_>,
) -> GraphResult<MaterializedStorePlan> {
    let edge_count = ranges
        .iter()
        .flat_map(|range| range.start..range.end)
        .try_fold(0usize, |total, source| {
            compaction_tick(execution.governor, execution.started, execution.max_elapsed)?;
            let degree = layered.neighbors(source).count();
            total
                .checked_add(degree)
                .ok_or_else(|| compaction_resource_limit("memory bytes", total, degree, usize::MAX))
        })?;
    let heap_bytes = SortedEdgeStoreBuilder::planned_storage_bytes(
        base.node_count(),
        edge_count,
        layered.has_weighted_edges(),
    )?;
    Ok(MaterializedStorePlan {
        edge_count,
        heap_bytes,
    })
}

#[derive(Debug, Clone, Copy)]
struct CompactedSegmentPlan {
    deletes: usize,
    inserts: usize,
    weights: usize,
    heap_bytes: usize,
}

fn plan_compacted_segment(
    base: &EdgeStore,
    layered: &LayeredNeighbors<'_>,
    ranges: &[SourceRange],
    execution: CompactionExecution<'_>,
) -> GraphResult<CompactedSegmentPlan> {
    let (base_edges, final_edges) = ranges
        .iter()
        .flat_map(|range| range.start..range.end)
        .try_fold((0usize, 0usize), |(base_total, final_total), source| {
            compaction_tick(execution.governor, execution.started, execution.max_elapsed)?;
            let base_degree = base.neighbors(source).0.len();
            let final_degree = layered.neighbors(source).count();
            let base_total = base_total.checked_add(base_degree).ok_or_else(|| {
                compaction_resource_limit("memory bytes", base_total, base_degree, usize::MAX)
            })?;
            let final_total = final_total.checked_add(final_degree).ok_or_else(|| {
                compaction_resource_limit("memory bytes", final_total, final_degree, usize::MAX)
            })?;
            Ok((base_total, final_total))
        })?;
    let deletes = base_edges;
    let inserts = final_edges;
    let edges = deletes
        .checked_mul(std::mem::size_of::<SegmentEdge>())
        .and_then(|bytes| {
            inserts
                .checked_mul(std::mem::size_of::<SegmentEdge>())
                .and_then(|insert_bytes| bytes.checked_add(insert_bytes))
        })
        .ok_or_else(|| compaction_resource_limit("memory bytes", 0, usize::MAX, usize::MAX))?;
    let weights = final_edges
        .checked_mul(std::mem::size_of::<SegmentEdgeWeight>())
        .ok_or_else(|| compaction_resource_limit("memory bytes", edges, usize::MAX, usize::MAX))?;
    let heap_bytes = edges
        .checked_add(weights)
        .ok_or_else(|| compaction_resource_limit("memory bytes", edges, weights, usize::MAX))?;
    Ok(CompactedSegmentPlan {
        deletes,
        inserts,
        weights: final_edges,
        heap_bytes,
    })
}

fn build_compacted_segment(
    previous: &ProjectionManifest,
    base: &EdgeStore,
    layered: &LayeredNeighbors<'_>,
    segments: &[LoadedEdgeSegment],
    ranges: &[SourceRange],
    plan: CompactedSegmentPlan,
    execution: CompactionExecution<'_>,
) -> GraphResult<DeltaSegment> {
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
    compacted
        .edge_deletes
        .try_reserve_exact(plan.deletes)
        .map_err(compaction_allocation_error)?;
    compacted
        .edge_inserts
        .try_reserve_exact(plan.inserts)
        .map_err(compaction_allocation_error)?;
    compacted
        .edge_weights
        .try_reserve_exact(plan.weights)
        .map_err(compaction_allocation_error)?;
    for range in ranges {
        for source in range.start..range.end {
            compaction_tick(execution.governor, execution.started, execution.max_elapsed)?;
            let base_edges = edge_set(base, source);
            let final_edges = edge_map_from_layered(layered, source);
            for &(target, type_id, schema_reversed, relationship_id) in base_edges.keys() {
                if final_edges.contains_key(&(target, type_id, schema_reversed, relationship_id)) {
                    continue;
                }
                compacted.edge_deletes.push(SegmentEdge {
                    source,
                    target,
                    type_id,
                    schema_reversed,
                    relationship_id,
                });
            }
            for (&(target, type_id, schema_reversed, relationship_id), edge) in final_edges.iter() {
                if base_edges.get(&(target, type_id, schema_reversed, relationship_id))
                    == Some(&edge.weight)
                {
                    continue;
                }
                compacted.edge_inserts.push(SegmentEdge {
                    source,
                    target,
                    type_id,
                    schema_reversed,
                    relationship_id,
                });
                if let Some(weight) = edge.weight {
                    compacted.edge_weights.push(SegmentEdgeWeight {
                        source,
                        target,
                        type_id,
                        schema_reversed,
                        relationship_id,
                        weight,
                    });
                }
            }
        }
    }
    Ok(compacted)
}

fn materialize_layered_ranges(
    base: &EdgeStore,
    layered: &LayeredNeighbors<'_>,
    ranges: &[SourceRange],
    edge_count: usize,
    governor: &crate::resource::ResourceGovernor,
    started: Instant,
    max_elapsed: Duration,
) -> GraphResult<EdgeStore> {
    let has_weights = layered.has_weighted_edges();
    let mut builder = SortedEdgeStoreBuilder::try_new_with_edge_capacity(
        base.node_count(),
        edge_count,
        has_weights,
    )?;
    for source in ranges.iter().flat_map(|range| range.start..range.end) {
        compaction_tick(governor, started, max_elapsed)?;
        let weights = layered
            .weighted_neighbors(source)
            .into_iter()
            .map(|neighbor| {
                (
                    (
                        neighbor.target,
                        neighbor.type_id,
                        neighbor.schema_reversed,
                        neighbor.relationship_id,
                    ),
                    neighbor.weight,
                )
            })
            .collect::<BTreeMap<_, _>>();
        for neighbor in layered.neighbors(source) {
            builder.try_push_identified(IdentifiedRawEdge {
                edge: RawEdge {
                    source,
                    target: neighbor.target,
                    type_id: neighbor.type_id,
                    weight: weights
                        .get(&(
                            neighbor.target,
                            neighbor.type_id,
                            neighbor.schema_reversed,
                            neighbor.relationship_id,
                        ))
                        .copied(),
                    schema_reversed: neighbor.schema_reversed,
                },
                relationship_id: neighbor.relationship_id.unwrap_or(NO_RELATIONSHIP_ID),
            })?;
        }
    }
    Ok(builder.finish())
}

fn compaction_tick(
    governor: &crate::resource::ResourceGovernor,
    started: Instant,
    max_elapsed: Duration,
) -> GraphResult<()> {
    governor
        .consume_work(
            crate::resource::ResourcePhase::CompactionMerge,
            crate::resource::WorkUnits::new(1),
        )
        .map_err(crate::safety::resource_limit_error)?;
    let elapsed = started.elapsed();
    if elapsed > max_elapsed {
        return Err(GraphError::ResourceLimit {
            resource: "elapsed microseconds".to_string(),
            phase: crate::resource::ResourcePhase::CompactionMerge
                .as_str()
                .to_string(),
            used: elapsed.as_micros().min(u128::from(u64::MAX)) as u64,
            requested: 0,
            limit: max_elapsed.as_micros().min(u128::from(u64::MAX)) as u64,
        });
    }
    if governor.work_used().as_u64().is_multiple_of(1_024) {
        crate::resource::check_postgres_interrupts();
        governor
            .check_elapsed(crate::resource::ResourcePhase::CompactionMerge)
            .map_err(crate::safety::resource_limit_error)?;
    }
    Ok(())
}

fn edge_set(store: &EdgeStore, source: u32) -> BTreeMap<CompactionEdgeKey, Option<u32>> {
    let (targets, type_ids, schema_reversed, weights) =
        store.neighbors_weighted_with_schema(source);
    let (_, _, _, relationship_ids) = store.neighbors_with_schema_and_relationship_ids(source);
    targets
        .iter()
        .zip(type_ids.iter())
        .zip(schema_reversed.iter())
        .enumerate()
        .map(|(idx, ((&target, &type_id), &schema_reversed))| {
            (
                (
                    target,
                    type_id,
                    schema_reversed != 0,
                    relationship_ids
                        .get(idx)
                        .copied()
                        .filter(|&id| id != crate::edge_store::NO_RELATIONSHIP_ID),
                ),
                store
                    .has_weights()
                    .then(|| weights.get(idx).copied())
                    .flatten(),
            )
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactedEdge {
    weight: Option<u32>,
    relationship_id: Option<crate::edge_store::RelationshipId>,
}

fn edge_map_from_layered(
    layered: &LayeredNeighbors<'_>,
    source: u32,
) -> BTreeMap<CompactionEdgeKey, CompactedEdge> {
    let weights = layered
        .weighted_neighbors(source)
        .into_iter()
        .map(|neighbor| {
            (
                (
                    neighbor.target,
                    neighbor.type_id,
                    neighbor.schema_reversed,
                    neighbor.relationship_id,
                ),
                neighbor.weight,
            )
        })
        .collect::<BTreeMap<_, _>>();
    layered
        .neighbors(source)
        .map(|neighbor| {
            (
                (
                    neighbor.target,
                    neighbor.type_id,
                    neighbor.schema_reversed,
                    neighbor.relationship_id,
                ),
                CompactedEdge {
                    weight: weights
                        .get(&(
                            neighbor.target,
                            neighbor.type_id,
                            neighbor.schema_reversed,
                            neighbor.relationship_id,
                        ))
                        .copied(),
                    relationship_id: neighbor.relationship_id,
                },
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
    let checksum = segment_checksum(path)?;
    segment_ref_with_checksum(root, path, segment, checksum)
}

fn segment_ref_with_checksum(
    root: &Path,
    path: &Path,
    segment: &DeltaSegment,
    checksum: String,
) -> GraphResult<ManifestSegmentRef> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| GraphError::Internal("projection segment path escaped artifact root".into()))?
        .to_string_lossy()
        .to_string();
    Ok(ManifestSegmentRef {
        path: relative,
        checksum,
        level: segment.header.level,
        source_start: segment.header.source_start,
        source_end: segment.header.source_end,
        sync_watermark: segment.header.sync_watermark,
    })
}

fn segment_checksum(path: &Path) -> GraphResult<String> {
    let mut file = std::fs::File::open(path)
        .map_err(|err| GraphError::Internal(format!("read projection segment checksum: {err}")))?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|err| {
            GraphError::Internal(format!("read projection segment checksum: {err}"))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("crc32:{:08x}", hasher.finalize()))
}

fn write_segment_atomically(
    root: &Path,
    final_path: &Path,
    segment: &DeltaSegment,
    governor: &crate::resource::ResourceGovernor,
    max_bytes: usize,
) -> GraphResult<String> {
    std::fs::create_dir_all(root)
        .map_err(|err| GraphError::Internal(format!("create projection segment dir: {err}")))?;
    let tmp_path = temp_segment_path(root, final_path)?;
    let encoded_len = segment.encoded_len()?;
    let serialization_bytes = encoded_len
        .checked_mul(2)
        .ok_or_else(|| compaction_resource_limit("memory bytes", 0, usize::MAX, max_bytes))?;
    let used = governor.memory_used().as_u64().min(usize::MAX as u64) as usize;
    if used
        .checked_add(serialization_bytes)
        .is_none_or(|next| next > max_bytes)
    {
        return Err(compaction_resource_limit(
            "memory bytes",
            used,
            serialization_bytes,
            max_bytes,
        ));
    }
    let serialization_bytes = crate::resource::ByteCount::from_usize(serialization_bytes)
        .ok_or_else(|| compaction_resource_limit("memory bytes", used, usize::MAX, max_bytes))?;
    let _encoded_memory = governor
        .reserve_memory(
            crate::resource::ResourcePhase::CompactionMerge,
            serialization_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
    let encoded_bytes = crate::resource::ByteCount::from_usize(encoded_len)
        .ok_or_else(|| compaction_resource_limit("disk bytes", 0, usize::MAX, max_bytes))?;
    let _staging_disk = governor
        .reserve_disk(
            crate::resource::ResourcePhase::CompactionMerge,
            encoded_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
    let bytes = segment.to_bytes()?;
    debug_assert_eq!(bytes.len(), encoded_len);
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
    result?;
    Ok(format!("crc32:{:08x}", crc32fast::hash(&bytes)))
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
            relationship_id: None,
            weight: 11,
            schema_reversed: false,
        });
        weighted.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 2,
            type_id: 1,
            relationship_id: None,
            weight: 13,
            schema_reversed: false,
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
                    relationship_id: None,
                    weight: 11,
                    schema_reversed: false,
                },
                crate::projection::neighbors::WeightedNeighbor {
                    target: 2,
                    type_id: 1,
                    relationship_id: None,
                    weight: 13,
                    schema_reversed: false,
                },
            ]
        );
    }

    #[test]
    fn compaction_preserves_segment_relationship_ids() {
        let dir = ProjectionArtifactDir::new("compaction_preserves_segment_relationship_ids");
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = edge_store_from_tuples(4, &[]);
        let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 1, 1)
            .expect("edge segment");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: Some(91),
        });
        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        let path = dir.segment_path(1, 0);
        segment.write_to_path(&path).expect("segment writes");
        manifest
            .segments
            .push(segment_ref(dir.path(), &path, &segment).expect("segment ref"));
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");

        let result =
            compact_generation(dir.path(), &manifest, &base, CompactionBudgets::generous())
                .expect("compaction publishes");
        let provider = ManifestSegmentProvider::new(dir.path(), &result.manifest);
        let actual = LayeredNeighbors::from_provider(&base, &provider).expect("compacted loads");

        assert_eq!(
            actual.neighbors(0).collect::<Vec<_>>(),
            vec![crate::projection::neighbors::Neighbor {
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: Some(91),
            }]
        );
    }

    #[test]
    fn compaction_preserves_parallel_relationship_rows_and_identity_delete() {
        let dir = ProjectionArtifactDir::new(
            "compaction_preserves_parallel_relationship_rows_and_identity_delete",
        );
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = edge_store_from_tuples(4, &[]);

        let mut inserted =
            DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 1, 1)
                .expect("insert segment");
        for relationship_id in [91, 92] {
            inserted.edge_inserts.push(SegmentEdge {
                source: 0,
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: Some(relationship_id),
            });
            inserted.edge_weights.push(SegmentEdgeWeight {
                source: 0,
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: Some(relationship_id),
                weight: relationship_id,
            });
        }
        let mut deleted = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 1, 2)
            .expect("delete segment");
        deleted.edge_deletes.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: Some(91),
        });

        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        for (index, segment) in [&inserted, &deleted].into_iter().enumerate() {
            let path = dir.segment_path(1, index as u32);
            segment.write_to_path(&path).expect("segment writes");
            manifest
                .segments
                .push(segment_ref(dir.path(), &path, segment).expect("segment ref"));
        }
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");

        let before = LayeredNeighbors::from_provider(
            &base,
            &ManifestSegmentProvider::new(dir.path(), &manifest),
        )
        .expect("layered input loads")
        .neighbors(0)
        .collect::<Vec<_>>();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].relationship_id, Some(92));

        let result = compact_generation(
            dir.path(),
            &manifest,
            &base,
            CompactionBudgets {
                dirty_chunk_segment_threshold: Some(1),
                ..CompactionBudgets::generous()
            },
        )
        .expect("compaction publishes");
        assert_eq!(result.manifest.base_chunks.len(), 1);
        let after = LayeredNeighbors::from_provider(
            &base,
            &ManifestSegmentProvider::new(dir.path(), &result.manifest),
        )
        .expect("compacted output loads")
        .neighbors(0)
        .collect::<Vec<_>>();

        assert_eq!(after, before);
        assert_eq!(
            LayeredNeighbors::from_provider(
                &base,
                &ManifestSegmentProvider::new(dir.path(), &result.manifest),
            )
            .expect("rewritten weighted chunk loads")
            .weighted_neighbors(0)
            .into_iter()
            .map(|neighbor| (neighbor.relationship_id, neighbor.weight))
            .collect::<Vec<_>>(),
            vec![(Some(92), 92)]
        );
    }

    #[test]
    fn compaction_keeps_identical_endpoint_parallel_relationships() {
        let dir = ProjectionArtifactDir::new(
            "compaction_keeps_identical_endpoint_parallel_relationships",
        );
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = edge_store_from_tuples(4, &[]);
        let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 1, 1)
            .expect("edge segment");
        for (relationship_id, weight) in [(91, 11), (92, 13)] {
            segment.edge_inserts.push(SegmentEdge {
                source: 0,
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: Some(relationship_id),
            });
            segment.edge_weights.push(SegmentEdgeWeight {
                source: 0,
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: Some(relationship_id),
                weight,
            });
        }
        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        let path = dir.segment_path(1, 0);
        segment.write_to_path(&path).expect("segment writes");
        manifest
            .segments
            .push(segment_ref(dir.path(), &path, &segment).expect("segment ref"));
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("manifest publishes");

        let result =
            compact_generation(dir.path(), &manifest, &base, CompactionBudgets::generous())
                .expect("compaction publishes");
        let relationship_ids = LayeredNeighbors::from_provider(
            &base,
            &ManifestSegmentProvider::new(dir.path(), &result.manifest),
        )
        .expect("compacted output loads")
        .neighbors(0)
        .map(|neighbor| neighbor.relationship_id)
        .collect::<Vec<_>>();

        assert_eq!(relationship_ids, vec![Some(91), Some(92)]);
        assert_eq!(
            LayeredNeighbors::from_provider(
                &base,
                &ManifestSegmentProvider::new(dir.path(), &result.manifest),
            )
            .expect("compacted weighted output loads")
            .weighted_neighbors(0)
            .into_iter()
            .map(|neighbor| (neighbor.relationship_id, neighbor.weight))
            .collect::<Vec<_>>(),
            vec![(Some(91), 11), (Some(92), 13)]
        );
    }

    #[test]
    fn compaction_preserves_weighted_schema_reversed_parallel_edges() {
        let dir = ProjectionArtifactDir::new(
            "compaction_preserves_weighted_schema_reversed_parallel_edges",
        );
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = weighted_edge_store_from_tuples(4, &[(2, 3, 1, 5)]);
        let mut weighted =
            DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 1, 1)
                .expect("edge segment");
        weighted.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        weighted.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: true,
            relationship_id: None,
        });
        weighted.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            weight: 11,
        });
        weighted.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: true,
            relationship_id: None,
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
                    relationship_id: None,
                    weight: 11,
                    schema_reversed: false,
                },
                crate::projection::neighbors::WeightedNeighbor {
                    target: 1,
                    type_id: 1,
                    relationship_id: None,
                    weight: 13,
                    schema_reversed: true,
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
    fn compaction_accounts_for_live_engine_residency_before_allocating() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_accounts_for_live_engine_residency_before_allocating",
            vec![segment(1, 0, 0, &[(0, 2, 1)], &[])],
        );
        let budgets = CompactionBudgets {
            resident_bytes: crate::resource::ByteCount::from_mib(2_048)
                .expect("configured test memory fits"),
            ..CompactionBudgets::generous()
        };

        let error = compact_generation(dir.path(), &manifest, &base, budgets)
            .expect_err("resident graph must consume maintenance headroom");

        assert!(matches!(error, GraphError::ResourceLimit { .. }));
        let current = ProjectionManifestStore::new(dir.path().to_path_buf())
            .load_latest_current()
            .expect("manifest lookup succeeds")
            .expect("fixture manifest remains current");
        assert_eq!(current.generation_id, manifest.generation_id);
    }

    #[test]
    fn compaction_treats_max_bytes_as_a_ceiling_for_tiny_input() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_treats_max_bytes_as_a_ceiling_for_tiny_input",
            vec![segment(1, 0, 0, &[(0, 2, 1)], &[])],
        );
        let budgets = CompactionBudgets {
            max_bytes: 1_000_000_000,
            ..CompactionBudgets::generous()
        };

        let result = compact_generation(dir.path(), &manifest, &base, budgets)
            .expect("a high ceiling must not be charged as live memory");

        assert_eq!(result.segments_compacted, 1);
        assert!(result.manifest.generation_id > manifest.generation_id);
    }

    #[test]
    fn compaction_rejects_encoded_and_decoded_segment_overlap_before_decode() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_rejects_encoded_and_decoded_segment_overlap_before_decode",
            vec![segment(1, 0, 0, &[(0, 2, 1)], &[])],
        );
        let encoded_bytes = manifest_file_bytes(dir.path(), &manifest.segments[0].path)
            .expect("segment metadata reads");
        let budgets = CompactionBudgets {
            max_bytes: encoded_bytes,
            ..CompactionBudgets::generous()
        };

        let error = compact_generation(dir.path(), &manifest, &base, budgets)
            .expect_err("raw plus decoded segment must exceed an encoded-only ceiling");

        assert!(matches!(
            &error,
            GraphError::ResourceLimit { resource, .. } if resource == "memory bytes"
        ));
        assert_eq!(error.diagnostic_code().as_str(), "PG007");
    }

    #[test]
    fn base_chunk_loader_rejects_encoded_and_decoded_overlap_before_decode() {
        let (dir, base, manifest) = fixture_manifest(
            "base_chunk_loader_rejects_encoded_and_decoded_overlap_before_decode",
            vec![segment(1, 0, 0, &[(0, 2, 1)], &[])],
        );
        let rewritten = compact_generation(
            dir.path(),
            &manifest,
            &base,
            CompactionBudgets {
                dirty_chunk_segment_threshold: Some(1),
                ..CompactionBudgets::generous()
            },
        )
        .expect("fixture chunk rewrite publishes");
        let chunk = rewritten
            .manifest
            .base_chunks
            .first()
            .expect("rewrite publishes a base chunk");
        let encoded_bytes =
            manifest_file_bytes(dir.path(), &chunk.path).expect("chunk metadata reads");
        let governor = crate::resource::maintenance_governor(
            crate::resource::ByteCount::ZERO,
            crate::resource::RowCount::UNLIMITED,
        );
        let mut memory = reserve_compaction_memory(&governor, 0, encoded_bytes)
            .expect("empty reservation succeeds");
        let ranges = [
            SourceRange::new(chunk.source_start, chunk.source_end).expect("chunk range is valid")
        ];

        let error = load_relevant_base_chunks(
            dir.path(),
            &rewritten.manifest,
            &ranges,
            encoded_bytes,
            &mut memory,
            0,
            encoded_bytes,
        )
        .expect_err("raw plus decoded chunk must exceed an encoded-only ceiling");

        assert!(matches!(
            &error,
            GraphError::ResourceLimit { resource, .. } if resource == "memory bytes"
        ));
        assert_eq!(error.diagnostic_code().as_str(), "PG007");
    }

    #[test]
    fn compaction_near_residency_limit_preserves_current_generation() {
        let (dir, base, manifest) = fixture_manifest(
            "compaction_near_residency_limit_preserves_current_generation",
            vec![segment(1, 0, 0, &[(0, 2, 1)], &[])],
        );
        let budgets = CompactionBudgets {
            resident_bytes: crate::resource::ByteCount::from_bytes(2_048_u64 * 1_048_576 - 1),
            ..CompactionBudgets::generous()
        };

        let error = compact_generation(dir.path(), &manifest, &base, budgets)
            .expect_err("live structures must not exceed remaining memory");
        let current = ProjectionManifestStore::new(dir.path().to_path_buf())
            .load_latest_current()
            .expect("manifest lookup succeeds")
            .expect("fixture manifest remains current");

        assert_eq!(error.diagnostic_code().as_str(), "PG007");
        assert_eq!(current.generation_id, manifest.generation_id);
    }

    #[test]
    fn compaction_high_degree_scratch_rejects_before_publication() {
        let dir =
            ProjectionArtifactDir::new("compaction_high_degree_scratch_rejects_before_publication");
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let edges = (1..=2_048).map(|target| (0, target, 1)).collect::<Vec<_>>();
        let base = edge_store_from_tuples(2_050, &edges);
        let mutation = segment(1, 0, 0, &[(0, 2_049, 1)], &[]);
        let path = dir.segment_path(1, 0);
        mutation.write_to_path(&path).expect("segment writes");
        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        manifest
            .segments
            .push(segment_ref(dir.path(), &path, &mutation).expect("segment ref"));
        ProjectionManifestStore::new(dir.path())
            .publish(&manifest)
            .expect("fixture manifest publishes");

        let error = compact_generation(
            dir.path(),
            &manifest,
            &base,
            CompactionBudgets {
                max_bytes: 2_500_000,
                ..CompactionBudgets::generous()
            },
        )
        .expect_err("per-source merge maps must fit before materialization");
        let current = ProjectionManifestStore::new(dir.path())
            .load_latest_current()
            .expect("manifest lookup succeeds")
            .expect("fixture generation remains current");

        assert!(matches!(error, GraphError::ResourceLimit { .. }));
        assert_eq!(error.diagnostic_code().as_str(), "PG007");
        assert_eq!(current.generation_id, manifest.generation_id);
        assert!(!ProjectionManifestStore::new(dir.path())
            .manifest_path(2)
            .exists());
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("artifact directory reads")
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .contains("compact-segment"))
                .count(),
            0,
            "scratch rejection must happen before staging compacted output"
        );
    }

    #[test]
    fn compaction_public_budget_failures_use_resource_limit_diagnostic() {
        let cases = [
            CompactionBudgets {
                max_segments: 0,
                ..CompactionBudgets::generous()
            },
            CompactionBudgets {
                max_rows: 0,
                ..CompactionBudgets::generous()
            },
            CompactionBudgets {
                max_bytes: 1,
                ..CompactionBudgets::generous()
            },
            CompactionBudgets {
                max_elapsed: Duration::ZERO,
                ..CompactionBudgets::generous()
            },
        ];

        for (case, budgets) in cases.into_iter().enumerate() {
            let (dir, base, manifest) = fixture_manifest(
                &format!("compaction_resource_limit_diagnostic_{case}"),
                vec![segment(1, 0, 0, &[(0, 2, 1)], &[])],
            );

            let error = compact_generation(dir.path(), &manifest, &base, budgets)
                .expect_err("tight public budget must reject compaction");

            assert!(
                matches!(error, GraphError::ResourceLimit { .. }),
                "case {case} returned {error:?}"
            );
            assert_eq!(error.diagnostic_code().as_str(), "PG007");
        }
    }

    #[test]
    fn governed_segment_publication_rejects_before_staging() {
        let dir = ProjectionArtifactDir::new("governed_segment_publication_rejects_before_staging");
        let compacted = segment(1, 1, 0, &[(0, 2, 1)], &[]);
        let path = dir.path().join("governed-segment.pggraph-delta");
        let encoded_len = compacted.encoded_len().expect("encoded length preflights");
        let max_bytes = encoded_len
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_sub(1))
            .expect("fixture serialization budget fits usize");
        let governor = crate::resource::maintenance_governor(
            crate::resource::ByteCount::ZERO,
            crate::resource::RowCount::UNLIMITED,
        );

        let error = write_segment_atomically(dir.path(), &path, &compacted, &governor, max_bytes)
            .expect_err("serialization must be rejected before staging");

        assert_eq!(error.diagnostic_code().as_str(), "PG007");
        assert!(!path.exists());
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
            relationship_id: None,
            weight: 11,
            schema_reversed: false,
        });
        edge_segment.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 3,
            type_id: 1,
            relationship_id: None,
            weight: 13,
            schema_reversed: false,
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
                    relationship_id: None,
                    weight: 11,
                    schema_reversed: false,
                },
                crate::projection::neighbors::WeightedNeighbor {
                    target: 3,
                    type_id: 1,
                    relationship_id: None,
                    weight: 13,
                    schema_reversed: false,
                },
            ]
        );
        assert!(actual.neighbors(2).collect::<Vec<_>>().is_empty());
    }

    #[test]
    fn compaction_dirty_chunk_rewrite_preserves_schema_reversed_weights() {
        let dir = ProjectionArtifactDir::new(
            "compaction_dirty_chunk_rewrite_preserves_schema_reversed_weights",
        );
        std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base writes");
        let base = weighted_edge_store_from_tuples(4, &[(2, 3, 1, 5)]);
        let mut edge_segment =
            DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 1, 1)
                .expect("edge segment");
        edge_segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        edge_segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: true,
            relationship_id: None,
        });
        edge_segment.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
            weight: 11,
        });
        edge_segment.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: true,
            relationship_id: None,
            weight: 13,
        });
        let mut manifest = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 10, 1);
        let path = dir.segment_path(1, 0);
        edge_segment.write_to_path(&path).expect("segment writes");
        manifest
            .segments
            .push(segment_ref(dir.path(), &path, &edge_segment).expect("segment ref"));
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
        assert_eq!(
            actual.weighted_neighbors(0),
            vec![
                crate::projection::neighbors::WeightedNeighbor {
                    target: 1,
                    type_id: 1,
                    relationship_id: None,
                    weight: 11,
                    schema_reversed: false,
                },
                crate::projection::neighbors::WeightedNeighbor {
                    target: 1,
                    type_id: 1,
                    relationship_id: None,
                    weight: 13,
                    schema_reversed: true,
                },
            ]
        );
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

        assert!(matches!(err, GraphError::ResourceLimit { .. }));
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
                    schema_reversed: false,
                    relationship_id: None,
                }),
        );
        segment.edge_deletes.extend(
            deletes
                .iter()
                .map(|&(source, target, type_id)| SegmentEdge {
                    source,
                    target,
                    type_id,
                    schema_reversed: false,
                    relationship_id: None,
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
