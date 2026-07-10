//! Durable projection layered read source.
//!
//! Layered reads merge base CSR neighbors, durable delta segments, and the
//! current transaction-local delta into one deterministic source. Public SQL
//! read adoption is handled by a later phase; this module provides the pure
//! runtime surface and real segment loading boundary.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::edge_store::EdgeStore;
use crate::projection::chunk::SourceRange;
use crate::projection::manifest::{ManifestChunkRef, ManifestSegmentRef, ProjectionManifest};
use crate::projection::neighbors::{
    EdgeOverlay, Neighbor, NeighborIter, NeighborSource, OverlayDeletes, OverlayInserts,
    WeightedNeighbor, WeightedNeighborSource,
};
use crate::projection::segment::{DeltaSegment, SegmentKind};
use crate::projection::tx_delta;
use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EdgeKey {
    source: u32,
    target: u32,
    type_id: u8,
    schema_reversed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayeredEdge {
    target: u32,
    type_id: u8,
    schema_reversed: bool,
    weight: Option<u32>,
    relationship_id: Option<crate::edge_store::RelationshipId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableEdgeState {
    Present {
        weight: Option<u32>,
        relationship_id: Option<crate::edge_store::RelationshipId>,
    },
    Deleted,
}

#[derive(Debug, Default)]
struct DurableEdges {
    inserts: Vec<LayeredEdge>,
    deletes: HashSet<(u32, u8, bool)>,
}

/// Source of decoded durable segments for a layered projection snapshot.
pub(crate) trait SegmentProvider {
    /// Load decoded segments that belong to one manifest snapshot.
    ///
    /// # Errors
    ///
    /// Returns segment loading or validation errors from the backing store.
    fn load_segments(&self) -> GraphResult<Vec<DeltaSegment>>;

    /// Load decoded base chunks that replace source-node ranges.
    ///
    /// # Errors
    ///
    /// Returns chunk loading or validation errors from the backing store.
    fn load_base_chunks(&self) -> GraphResult<Vec<DeltaSegment>> {
        Ok(Vec::new())
    }
}

/// Segment provider backed by a manifest and projection artifact directory.
#[allow(
    dead_code,
    reason = "Microphase 8 wires Engine read-path adoption to the manifest segment provider"
)]
pub(crate) struct ManifestSegmentProvider<'a> {
    root: &'a Path,
    manifest: &'a ProjectionManifest,
}

impl<'a> ManifestSegmentProvider<'a> {
    /// Borrow a manifest snapshot for real durable segment loading.
    #[allow(
        dead_code,
        reason = "Microphase 8 wires Engine read-path adoption to the manifest segment provider"
    )]
    pub(crate) fn new(root: &'a Path, manifest: &'a ProjectionManifest) -> Self {
        Self { root, manifest }
    }
}

impl SegmentProvider for ManifestSegmentProvider<'_> {
    fn load_segments(&self) -> GraphResult<Vec<DeltaSegment>> {
        self.manifest
            .segments
            .iter()
            .map(|segment| read_manifest_segment(self.root, segment))
            .collect()
    }

    fn load_base_chunks(&self) -> GraphResult<Vec<DeltaSegment>> {
        self.manifest
            .base_chunks
            .iter()
            .map(|chunk| read_manifest_chunk(self.root, chunk))
            .collect()
    }
}

#[allow(
    dead_code,
    reason = "Microphase 8 wires Engine read-path adoption to the manifest segment provider"
)]
fn read_manifest_segment(root: &Path, segment: &ManifestSegmentRef) -> GraphResult<DeltaSegment> {
    let path = root.join(PathBuf::from(&segment.path));
    let bytes = fs::read(&path)
        .map_err(|err| GraphError::Internal(format!("segment read failed: {err}")))?;
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

fn read_manifest_chunk(root: &Path, chunk: &ManifestChunkRef) -> GraphResult<DeltaSegment> {
    let path = root.join(PathBuf::from(&chunk.path));
    let bytes = fs::read(&path)
        .map_err(|err| GraphError::Internal(format!("base chunk read failed: {err}")))?;
    let checksum = format!("crc32:{:08x}", crc32fast::hash(&bytes));
    if checksum != chunk.checksum {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection base chunk checksum mismatch for {}: expected {}, got {}",
                chunk.path, chunk.checksum, checksum
            ),
        });
    }
    let segment = DeltaSegment::from_bytes(&bytes)?;
    if segment.header.source_start != chunk.source_start
        || segment.header.source_end != chunk.source_end
        || segment.header.kind != SegmentKind::Edge
        || segment.header.direction != TraversalDirection::Out
    {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection base chunk {} metadata does not match manifest range {}..{}",
                chunk.path, chunk.source_start, chunk.source_end
            ),
        });
    }
    Ok(segment)
}

/// Immutable layered neighbor source for one manifest snapshot.
pub(crate) struct LayeredNeighbors<'a> {
    base: &'a EdgeStore,
    base_in: Option<&'a EdgeStore>,
    base_chunk_ranges: Vec<SourceRange>,
    base_chunk_out: HashMap<u32, DurableEdges>,
    base_chunk_in: HashMap<u32, DurableEdges>,
    durable_out: HashMap<u32, DurableEdges>,
    durable_in: HashMap<u32, DurableEdges>,
    committed_out_inserts: OverlayInserts,
    committed_out_deletes: OverlayDeletes,
    committed_in_inserts: OverlayInserts,
    committed_in_deletes: OverlayDeletes,
    active_nodes: HashMap<u32, bool>,
    tenant_memberships: HashMap<u64, HashSet<u32>>,
    tenant_filter: Option<u64>,
}

impl<'a> LayeredNeighbors<'a> {
    /// Build a layered source from already-decoded segments.
    pub(crate) fn new(base: &'a EdgeStore, segments: Vec<DeltaSegment>) -> Self {
        Self::new_with_tenant(base, segments, None)
    }

    /// Build a layered source with an optional tenant membership filter.
    pub(crate) fn new_with_tenant(
        base: &'a EdgeStore,
        segments: Vec<DeltaSegment>,
        tenant_filter: Option<u64>,
    ) -> Self {
        Self::new_with_options(base, None, segments, tenant_filter, None, None)
    }

    /// Build a layered source with reverse base CSR and committed overlays.
    pub(crate) fn new_with_options(
        base: &'a EdgeStore,
        base_in: Option<&'a EdgeStore>,
        segments: Vec<DeltaSegment>,
        tenant_filter: Option<u64>,
        committed_out: Option<EdgeOverlay>,
        committed_in: Option<EdgeOverlay>,
    ) -> Self {
        Self::new_with_base_chunks(
            base,
            base_in,
            Vec::new(),
            segments,
            tenant_filter,
            committed_out,
            committed_in,
        )
    }

    fn new_with_base_chunks(
        base: &'a EdgeStore,
        base_in: Option<&'a EdgeStore>,
        base_chunks: Vec<DeltaSegment>,
        segments: Vec<DeltaSegment>,
        tenant_filter: Option<u64>,
        committed_out: Option<EdgeOverlay>,
        committed_in: Option<EdgeOverlay>,
    ) -> Self {
        let base_chunk_ranges = base_chunks
            .iter()
            .map(|chunk| SourceRange {
                start: chunk.header.source_start,
                end: chunk.header.source_end,
            })
            .collect::<Vec<_>>();
        let mut base_chunk_builder = LayeredBuilder::new(base);
        base_chunk_builder.apply_segments(base_chunks);
        let base_chunk_output = base_chunk_builder.finish_replacement();
        let mut builder = LayeredBuilder::new(base);
        builder.apply_segments(segments);
        let output = builder.finish();
        let (committed_out_inserts, committed_out_deletes) = committed_out.unwrap_or_default();
        let (committed_in_inserts, committed_in_deletes) = committed_in.unwrap_or_default();
        Self {
            base,
            base_in,
            base_chunk_ranges,
            base_chunk_out: base_chunk_output.durable_out,
            base_chunk_in: base_chunk_output.durable_in,
            durable_out: output.durable_out,
            durable_in: output.durable_in,
            committed_out_inserts,
            committed_out_deletes,
            committed_in_inserts,
            committed_in_deletes,
            active_nodes: output.active_nodes,
            tenant_memberships: output.tenant_memberships,
            tenant_filter,
        }
    }

    /// Build a layered source from a segment provider.
    ///
    /// # Errors
    ///
    /// Returns provider errors when segment loading fails.
    pub(crate) fn from_provider(
        base: &'a EdgeStore,
        provider: &impl SegmentProvider,
    ) -> GraphResult<Self> {
        Ok(Self::new_with_base_chunks(
            base,
            None,
            provider.load_base_chunks()?,
            provider.load_segments()?,
            None,
            None,
            None,
        ))
    }

    /// Build a layered source from a provider plus Engine-owned read overlays.
    ///
    /// # Errors
    ///
    /// Returns provider errors when segment loading fails.
    pub(crate) fn from_provider_with_overlays(
        base: &'a EdgeStore,
        base_in: &'a EdgeStore,
        provider: &impl SegmentProvider,
        committed_out: EdgeOverlay,
        committed_in: EdgeOverlay,
    ) -> GraphResult<Self> {
        Ok(Self::new_with_base_chunks(
            base,
            Some(base_in),
            provider.load_base_chunks()?,
            provider.load_segments()?,
            None,
            Some(committed_out),
            Some(committed_in),
        ))
    }

    /// Borrow this layered snapshot as a neighbor source for `direction`.
    pub(crate) fn for_direction(
        &self,
        direction: TraversalDirection,
    ) -> DirectionalLayeredNeighbors<'_, 'a> {
        DirectionalLayeredNeighbors {
            layered: self,
            direction,
        }
    }

    /// Return weighted outgoing neighbors after durable deltas and transaction
    /// overlays have been applied.
    pub(crate) fn weighted_neighbors(&self, node_idx: u32) -> Vec<WeightedNeighbor> {
        self.merged_neighbors(TraversalDirection::Out, node_idx, false)
            .into_iter()
            .filter_map(|(target, edge)| {
                edge.weight.map(|weight| WeightedNeighbor {
                    target,
                    type_id: edge.type_id,
                    weight,
                    schema_reversed: edge.schema_reversed,
                })
            })
            .collect()
    }

    fn merged_neighbors(
        &self,
        direction: TraversalDirection,
        node_idx: u32,
        reversed: bool,
    ) -> Vec<(u32, LayeredEdge)> {
        if !self.node_visible(node_idx) {
            return Vec::new();
        }

        // Do not route an untouched base projection through the topology-keyed
        // merge map. A base CSR may contain parallel source relationship rows,
        // and the map is reserved for the legacy delta representation until
        // durable relationship keys reach segments and overlays.
        if direction == TraversalDirection::Out
            && self.base_chunk_out.is_empty()
            && self.durable_out.is_empty()
            && self.committed_out_inserts.is_empty()
            && self.committed_out_deletes.is_empty()
            && self.active_nodes.is_empty()
            && self.tenant_memberships.is_empty()
            && !tx_delta::edge_delta_dirty()
        {
            let (targets, type_ids, schema_reversed) = self.base.neighbors_with_schema(node_idx);
            let weights = base_weight_slice(self.base, node_idx);
            let mut neighbors = targets
                .iter()
                .zip(type_ids.iter())
                .zip(schema_reversed.iter())
                .enumerate()
                .filter_map(|(idx, ((&target, &type_id), &schema_reversed))| {
                    self.node_visible(target).then_some((
                        target,
                        LayeredEdge {
                            target,
                            type_id,
                            schema_reversed: schema_reversed != 0,
                            weight: weights.and_then(|values| values.get(idx)).copied(),
                            relationship_id: None,
                        },
                    ))
                })
                .collect::<Vec<_>>();
            if reversed {
                neighbors.reverse();
            }
            return neighbors;
        }

        let mut merged = BTreeMap::<(u32, u8, bool), LayeredEdge>::new();
        self.merge_base(direction, node_idx, &mut merged);
        self.merge_durable(direction, node_idx, &mut merged);
        self.merge_committed_overlay(direction, node_idx, &mut merged);
        self.merge_tx_delta(direction, node_idx, &mut merged);

        let mut neighbors = merged
            .into_iter()
            .filter(|((target, _, _), _)| self.node_visible(*target))
            .map(|((target, _, _), edge)| (target, edge))
            .collect::<Vec<_>>();
        if reversed {
            neighbors.reverse();
        }
        neighbors
    }

    fn merge_base(
        &self,
        direction: TraversalDirection,
        node_idx: u32,
        merged: &mut BTreeMap<(u32, u8, bool), LayeredEdge>,
    ) {
        match direction {
            TraversalDirection::Out => {
                if self.base_chunk_covers(node_idx) {
                    merge_durable_map(&self.base_chunk_out, node_idx, merged);
                } else {
                    let (targets, type_ids, schema_reversed) =
                        self.base.neighbors_with_schema(node_idx);
                    let weights = base_weight_slice(self.base, node_idx);
                    for (idx, ((&target, &type_id), &schema_reversed)) in targets
                        .iter()
                        .zip(type_ids.iter())
                        .zip(schema_reversed.iter())
                        .enumerate()
                    {
                        merged.insert(
                            (target, type_id, schema_reversed != 0),
                            LayeredEdge {
                                target,
                                type_id,
                                schema_reversed: schema_reversed != 0,
                                weight: weights.and_then(|weights| weights.get(idx).copied()),
                                relationship_id: None,
                            },
                        );
                    }
                }
            }
            TraversalDirection::In => {
                if let Some(base_in) = self.base_in {
                    let (targets, type_ids, schema_reversed) =
                        base_in.neighbors_with_schema(node_idx);
                    let weights = base_weight_slice(base_in, node_idx);
                    for (idx, ((&target, &type_id), &schema_reversed)) in targets
                        .iter()
                        .zip(type_ids.iter())
                        .zip(schema_reversed.iter())
                        .enumerate()
                    {
                        if self.base_chunk_covers(target) {
                            continue;
                        }
                        merged.insert(
                            (target, type_id, schema_reversed != 0),
                            LayeredEdge {
                                target,
                                type_id,
                                schema_reversed: schema_reversed != 0,
                                weight: weights.and_then(|weights| weights.get(idx).copied()),
                                relationship_id: None,
                            },
                        );
                    }
                } else {
                    self.merge_base_in_by_scan(node_idx, merged);
                }
                merge_durable_map(&self.base_chunk_in, node_idx, merged);
            }
            TraversalDirection::Any => {
                self.merge_base(TraversalDirection::Out, node_idx, merged);
                self.merge_base(TraversalDirection::In, node_idx, merged);
            }
        }
    }

    fn merge_base_in_by_scan(
        &self,
        node_idx: u32,
        merged: &mut BTreeMap<(u32, u8, bool), LayeredEdge>,
    ) {
        for source in 0..self.base.node_count() {
            if self.base_chunk_covers(source) {
                continue;
            }
            let (targets, type_ids, schema_reversed) = self.base.neighbors_with_schema(source);
            let weights = base_weight_slice(self.base, source);
            for (idx, ((&target, &type_id), &schema_reversed)) in targets
                .iter()
                .zip(type_ids.iter())
                .zip(schema_reversed.iter())
                .enumerate()
            {
                if target == node_idx {
                    merged.insert(
                        (source, type_id, schema_reversed != 0),
                        LayeredEdge {
                            target: source,
                            type_id,
                            schema_reversed: schema_reversed != 0,
                            weight: weights.and_then(|weights| weights.get(idx).copied()),
                            relationship_id: None,
                        },
                    );
                }
            }
        }
        merge_durable_map(&self.base_chunk_in, node_idx, merged);
    }

    fn base_chunk_covers(&self, source: u32) -> bool {
        self.base_chunk_ranges
            .iter()
            .any(|range| range.contains(source))
    }

    fn merge_durable(
        &self,
        direction: TraversalDirection,
        node_idx: u32,
        merged: &mut BTreeMap<(u32, u8, bool), LayeredEdge>,
    ) {
        match direction {
            TraversalDirection::Out => merge_durable_map(&self.durable_out, node_idx, merged),
            TraversalDirection::In => merge_durable_map(&self.durable_in, node_idx, merged),
            TraversalDirection::Any => {
                merge_durable_map(&self.durable_out, node_idx, merged);
                merge_durable_map(&self.durable_in, node_idx, merged);
            }
        }
    }

    fn merge_committed_overlay(
        &self,
        direction: TraversalDirection,
        node_idx: u32,
        merged: &mut BTreeMap<(u32, u8, bool), LayeredEdge>,
    ) {
        match direction {
            TraversalDirection::Out => merge_committed_overlay_maps(
                &self.committed_out_inserts,
                &self.committed_out_deletes,
                node_idx,
                merged,
            ),
            TraversalDirection::In => merge_committed_overlay_maps(
                &self.committed_in_inserts,
                &self.committed_in_deletes,
                node_idx,
                merged,
            ),
            TraversalDirection::Any => {
                merge_committed_overlay_maps(
                    &self.committed_out_inserts,
                    &self.committed_out_deletes,
                    node_idx,
                    merged,
                );
                merge_committed_overlay_maps(
                    &self.committed_in_inserts,
                    &self.committed_in_deletes,
                    node_idx,
                    merged,
                );
            }
        }
    }

    fn merge_tx_delta(
        &self,
        direction: TraversalDirection,
        node_idx: u32,
        merged: &mut BTreeMap<(u32, u8, bool), LayeredEdge>,
    ) {
        let (inserts, deletes) = tx_delta::weighted_edge_overlay(direction);
        if let Some(deleted) = deletes.get(&node_idx) {
            for &(target, type_id) in deleted {
                merged.retain(|&(edge_target, edge_type_id, _), _| {
                    edge_target != target || edge_type_id != type_id
                });
            }
        }
        if let Some(inserted) = inserts.get(&node_idx) {
            for edge in inserted {
                merged.insert(
                    (edge.target, edge.type_id, edge.schema_reversed),
                    LayeredEdge {
                        type_id: edge.type_id,
                        target: edge.target,
                        schema_reversed: edge.schema_reversed,
                        weight: edge.weight,
                        relationship_id: edge.relationship_id,
                    },
                );
            }
        }
    }

    fn node_visible(&self, node_idx: u32) -> bool {
        if tx_delta::node_deleted(node_idx) {
            return false;
        }
        if self
            .active_nodes
            .get(&node_idx)
            .is_some_and(|active| !*active)
        {
            return false;
        }
        match self.tenant_filter {
            Some(tenant_hash) => self
                .tenant_memberships
                .get(&tenant_hash)
                .is_some_and(|members| members.contains(&node_idx)),
            None => true,
        }
    }
}

impl NeighborSource for LayeredNeighbors<'_> {
    fn neighbors(&self, node_idx: u32) -> NeighborIter<'_> {
        NeighborIter::Owned(
            self.merged_neighbors(TraversalDirection::Out, node_idx, false)
                .into_iter()
                .map(|(target, edge)| Neighbor {
                    target,
                    type_id: edge.type_id,
                    schema_reversed: edge.schema_reversed,
                    relationship_id: edge.relationship_id,
                })
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }

    fn neighbors_reversed(&self, node_idx: u32) -> NeighborIter<'_> {
        NeighborIter::Owned(
            self.merged_neighbors(TraversalDirection::Out, node_idx, true)
                .into_iter()
                .map(|(target, edge)| Neighbor {
                    target,
                    type_id: edge.type_id,
                    schema_reversed: edge.schema_reversed,
                    relationship_id: edge.relationship_id,
                })
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }
}

/// Direction-specific neighbor source backed by one layered snapshot.
pub(crate) struct DirectionalLayeredNeighbors<'a, 'b> {
    layered: &'a LayeredNeighbors<'b>,
    direction: TraversalDirection,
}

impl NeighborSource for DirectionalLayeredNeighbors<'_, '_> {
    fn neighbors(&self, node_idx: u32) -> NeighborIter<'_> {
        NeighborIter::Owned(
            self.layered
                .merged_neighbors(self.direction, node_idx, false)
                .into_iter()
                .map(|(target, edge)| Neighbor {
                    target,
                    type_id: edge.type_id,
                    schema_reversed: edge.schema_reversed,
                    relationship_id: None,
                })
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }

    fn neighbors_reversed(&self, node_idx: u32) -> NeighborIter<'_> {
        NeighborIter::Owned(
            self.layered
                .merged_neighbors(self.direction, node_idx, true)
                .into_iter()
                .map(|(target, edge)| Neighbor {
                    target,
                    type_id: edge.type_id,
                    schema_reversed: edge.schema_reversed,
                    relationship_id: None,
                })
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }
}

impl WeightedNeighborSource for LayeredNeighbors<'_> {
    fn has_weighted_edges(&self) -> bool {
        self.base.has_weights()
            || self
                .base_chunk_out
                .values()
                .any(|edges| edges.inserts.iter().any(|edge| edge.weight.is_some()))
            || self
                .durable_out
                .values()
                .any(|edges| edges.inserts.iter().any(|edge| edge.weight.is_some()))
            || tx_delta::weighted_edge_overlay(TraversalDirection::Out)
                .0
                .values()
                .any(|edges| edges.iter().any(|edge| edge.weight.is_some()))
    }

    fn weighted_neighbors(&self, node_idx: u32) -> Vec<WeightedNeighbor> {
        self.weighted_neighbors(node_idx)
    }
}

fn base_weight_slice(base: &EdgeStore, node_idx: u32) -> Option<&[u32]> {
    let (_, _, weights) = base.neighbors_weighted(node_idx);
    (!weights.is_empty()).then_some(weights)
}

fn merge_durable_map(
    durable: &HashMap<u32, DurableEdges>,
    node_idx: u32,
    merged: &mut BTreeMap<(u32, u8, bool), LayeredEdge>,
) {
    if let Some(edges) = durable.get(&node_idx) {
        for &(target, type_id, schema_reversed) in &edges.deletes {
            merged.remove(&(target, type_id, schema_reversed));
        }
        for edge in &edges.inserts {
            merged.insert((edge.target, edge.type_id, edge.schema_reversed), *edge);
        }
    }
}

fn merge_committed_overlay_maps(
    inserts: &OverlayInserts,
    deletes: &OverlayDeletes,
    node_idx: u32,
    merged: &mut BTreeMap<(u32, u8, bool), LayeredEdge>,
) {
    if let Some(deleted) = deletes.get(&node_idx) {
        for &(target, type_id) in deleted {
            merged.retain(|&(edge_target, edge_type_id, _), _| {
                edge_target != target || edge_type_id != type_id
            });
        }
    }
    if let Some(inserted) = inserts.get(&node_idx) {
        for &(target, type_id, schema_reversed, relationship_id) in inserted {
            merged.insert(
                (target, type_id, schema_reversed),
                LayeredEdge {
                    target,
                    type_id,
                    schema_reversed,
                    weight: None,
                    relationship_id,
                },
            );
        }
    }
}

struct LayeredBuilder<'a> {
    base: &'a EdgeStore,
    out_edges: BTreeMap<EdgeKey, DurableEdgeState>,
    in_edges: BTreeMap<EdgeKey, DurableEdgeState>,
    active_nodes: HashMap<u32, bool>,
    tenant_memberships: HashMap<u64, HashSet<u32>>,
}

struct LayeredBuildOutput {
    durable_out: HashMap<u32, DurableEdges>,
    durable_in: HashMap<u32, DurableEdges>,
    active_nodes: HashMap<u32, bool>,
    tenant_memberships: HashMap<u64, HashSet<u32>>,
}

impl<'a> LayeredBuilder<'a> {
    fn new(base: &'a EdgeStore) -> Self {
        Self {
            base,
            out_edges: BTreeMap::new(),
            in_edges: BTreeMap::new(),
            active_nodes: HashMap::new(),
            tenant_memberships: HashMap::new(),
        }
    }

    fn apply_segments(&mut self, mut segments: Vec<DeltaSegment>) {
        segments.sort_by_key(|segment| {
            (
                segment.header.sync_watermark,
                segment.header.level,
                segment.header.source_start,
                segment.header.source_end,
            )
        });
        for segment in segments {
            match segment.header.kind {
                SegmentKind::Edge => self.apply_edge_segment(&segment),
                SegmentKind::Node => self.apply_node_segment(&segment),
            }
        }
    }

    fn apply_edge_segment(&mut self, segment: &DeltaSegment) {
        let weights = segment
            .edge_weights
            .iter()
            .map(|row| {
                (
                    EdgeKey {
                        source: row.source,
                        target: row.target,
                        type_id: row.type_id,
                        schema_reversed: row.schema_reversed,
                    },
                    row.weight,
                )
            })
            .collect::<HashMap<_, _>>();
        for edge in &segment.edge_deletes {
            self.remove_edge(
                segment.header.direction,
                edge.source,
                edge.target,
                edge.type_id,
                edge.schema_reversed,
            );
        }
        for edge in &segment.edge_inserts {
            let layered_edge = LayeredEdge {
                target: edge.target,
                type_id: edge.type_id,
                schema_reversed: edge.schema_reversed,
                weight: weights
                    .get(&EdgeKey {
                        source: edge.source,
                        target: edge.target,
                        type_id: edge.type_id,
                        schema_reversed: edge.schema_reversed,
                    })
                    .copied(),
                relationship_id: edge.relationship_id,
            };
            self.insert_edge(segment.header.direction, edge.source, layered_edge);
        }
    }

    fn apply_node_segment(&mut self, segment: &DeltaSegment) {
        for node in &segment.node_states {
            self.active_nodes.insert(node.node_idx, node.active);
        }
        for tenant in &segment.tenants {
            let members = self
                .tenant_memberships
                .entry(tenant.tenant_hash)
                .or_default();
            if tenant.tombstone {
                members.remove(&tenant.node_idx);
            } else {
                members.insert(tenant.node_idx);
            }
        }
    }

    fn insert_edge(&mut self, direction: TraversalDirection, source: u32, edge: LayeredEdge) {
        match direction {
            TraversalDirection::Out => {
                self.out_edges.insert(
                    EdgeKey {
                        source,
                        target: edge.target,
                        type_id: edge.type_id,
                        schema_reversed: edge.schema_reversed,
                    },
                    DurableEdgeState::Present {
                        weight: edge.weight,
                        relationship_id: edge.relationship_id,
                    },
                );
                self.in_edges.insert(
                    EdgeKey {
                        source: edge.target,
                        target: source,
                        type_id: edge.type_id,
                        schema_reversed: edge.schema_reversed,
                    },
                    DurableEdgeState::Present {
                        weight: edge.weight,
                        relationship_id: edge.relationship_id,
                    },
                );
            }
            TraversalDirection::In => {
                self.in_edges.insert(
                    EdgeKey {
                        source,
                        target: edge.target,
                        type_id: edge.type_id,
                        schema_reversed: edge.schema_reversed,
                    },
                    DurableEdgeState::Present {
                        weight: edge.weight,
                        relationship_id: edge.relationship_id,
                    },
                );
                self.out_edges.insert(
                    EdgeKey {
                        source: edge.target,
                        target: source,
                        type_id: edge.type_id,
                        schema_reversed: edge.schema_reversed,
                    },
                    DurableEdgeState::Present {
                        weight: edge.weight,
                        relationship_id: edge.relationship_id,
                    },
                );
            }
            TraversalDirection::Any => {
                self.insert_edge(TraversalDirection::Out, source, edge);
                self.insert_edge(
                    TraversalDirection::In,
                    edge.target,
                    LayeredEdge {
                        target: source,
                        ..edge
                    },
                );
            }
        }
    }

    fn remove_edge(
        &mut self,
        direction: TraversalDirection,
        source: u32,
        target: u32,
        type_id: u8,
        schema_reversed: bool,
    ) {
        match direction {
            TraversalDirection::Out => {
                self.out_edges.insert(
                    EdgeKey {
                        source,
                        target,
                        type_id,
                        schema_reversed,
                    },
                    DurableEdgeState::Deleted,
                );
                self.in_edges.insert(
                    EdgeKey {
                        source: target,
                        target: source,
                        type_id,
                        schema_reversed,
                    },
                    DurableEdgeState::Deleted,
                );
            }
            TraversalDirection::In => {
                self.in_edges.insert(
                    EdgeKey {
                        source,
                        target,
                        type_id,
                        schema_reversed,
                    },
                    DurableEdgeState::Deleted,
                );
                self.out_edges.insert(
                    EdgeKey {
                        source: target,
                        target: source,
                        type_id,
                        schema_reversed,
                    },
                    DurableEdgeState::Deleted,
                );
            }
            TraversalDirection::Any => {
                self.remove_edge(
                    TraversalDirection::Out,
                    source,
                    target,
                    type_id,
                    schema_reversed,
                );
                self.remove_edge(
                    TraversalDirection::In,
                    target,
                    source,
                    type_id,
                    schema_reversed,
                );
            }
        }
    }

    fn finish(self) -> LayeredBuildOutput {
        self.finish_with_base_duplicate_filter(true)
    }

    fn finish_replacement(self) -> LayeredBuildOutput {
        self.finish_with_base_duplicate_filter(false)
    }

    fn finish_with_base_duplicate_filter(
        self,
        suppress_base_duplicates: bool,
    ) -> LayeredBuildOutput {
        let Self {
            base,
            out_edges,
            in_edges,
            active_nodes,
            tenant_memberships,
        } = self;
        LayeredBuildOutput {
            durable_out: finish_direction(base, out_edges, suppress_base_duplicates),
            durable_in: finish_direction(base, in_edges, suppress_base_duplicates),
            active_nodes,
            tenant_memberships,
        }
    }
}

fn finish_direction(
    base: &EdgeStore,
    edges: BTreeMap<EdgeKey, DurableEdgeState>,
    suppress_base_duplicates: bool,
) -> HashMap<u32, DurableEdges> {
    let mut out = HashMap::<u32, DurableEdges>::new();
    for (key, state) in edges {
        match state {
            DurableEdgeState::Present {
                weight,
                relationship_id,
            } => {
                if suppress_base_duplicates && base_edge_exists(base, key) && weight.is_none() {
                    continue;
                }
                out.entry(key.source)
                    .or_default()
                    .inserts
                    .push(LayeredEdge {
                        target: key.target,
                        type_id: key.type_id,
                        schema_reversed: key.schema_reversed,
                        weight,
                        relationship_id,
                    });
            }
            DurableEdgeState::Deleted => {
                out.entry(key.source).or_default().deletes.insert((
                    key.target,
                    key.type_id,
                    key.schema_reversed,
                ));
            }
        }
    }
    out
}

fn base_edge_exists(base: &EdgeStore, key: EdgeKey) -> bool {
    let (targets, type_ids, schema_reversed) = base.neighbors_with_schema(key.source);
    targets
        .iter()
        .zip(type_ids.iter())
        .zip(schema_reversed.iter())
        .any(|((&target, &type_id), &schema_reversed)| {
            target == key.target
                && type_id == key.type_id
                && (schema_reversed != 0) == key.schema_reversed
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::manifest::{ManifestSegmentRef, ProjectionManifest};
    use crate::projection::neighbors::{CsrNeighbors, Neighbor, WeightedNeighbor};
    use crate::projection::segment::{
        SegmentEdge, SegmentEdgeWeight, SegmentNodeState, SegmentTenant,
    };
    use crate::projection::test_fixtures::{
        assert_full_csr_equivalence, edge_store_from_tuples, weighted_edge_store_from_tuples,
        ProjectionArtifactDir,
    };
    use proptest::prelude::*;

    #[test]
    fn layered_neighbors_equal_full_rebuild_for_insert_delete_sequence() {
        let base = edge_store_from_tuples(4, &[(0, 1, 1), (0, 2, 1)]);
        let mut insert = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 4, 1)
            .expect("insert segment");
        insert.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 3,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        let mut delete = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 4, 2)
            .expect("delete segment");
        delete.edge_deletes.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        let layered = LayeredNeighbors::new(&base, vec![insert, delete]);
        let full_rebuild = edge_store_from_tuples(4, &[(0, 2, 1), (0, 3, 1)]);
        let expected = CsrNeighbors::new(&full_rebuild);

        assert_full_csr_equivalence(4, &expected, &layered);
    }

    #[test]
    fn layered_neighbors_preserve_segment_relationship_id() {
        let base = edge_store_from_tuples(4, &[(0, 1, 1)]);
        let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 4, 1)
            .expect("insert segment");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 3,
            type_id: 1,
            schema_reversed: false,
            relationship_id: Some(77),
        });
        let layered = LayeredNeighbors::new(&base, vec![segment]);

        assert_eq!(
            layered.neighbors(0).collect::<Vec<_>>(),
            vec![
                Neighbor {
                    target: 1,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                },
                Neighbor {
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: Some(77),
                },
            ]
        );
    }

    #[test]
    fn layered_neighbors_tx_delta_wins_over_durable_segments() {
        tx_delta::clear_for_test();
        let base = edge_store_from_tuples(4, &[(0, 1, 1)]);
        let mut durable = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 4, 1)
            .expect("durable segment");
        durable.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        tx_delta::record_deleted_edge(0, 2, 1).expect("record tx delete");
        tx_delta::record_added_edge(
            0,
            tx_delta::DeltaEdge {
                target: 3,
                type_id: 1,
                weight: None,
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record tx insert");
        let layered = LayeredNeighbors::new(&base, vec![durable]);

        assert_eq!(
            layered.neighbors(0).collect::<Vec<_>>(),
            vec![
                Neighbor {
                    target: 1,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                },
                Neighbor {
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                },
            ]
        );
        tx_delta::clear_for_test();
    }

    #[test]
    fn layered_neighbors_inbound_direction_matches_full_rebuild() {
        let base = edge_store_from_tuples(4, &[(1, 0, 1)]);
        let mut inbound = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::In, 0, 4, 1)
            .expect("inbound segment");
        inbound.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        let layered = LayeredNeighbors::new(&base, vec![inbound]);
        let actual = layered.merged_neighbors(TraversalDirection::In, 0, false);

        assert_eq!(
            actual
                .into_iter()
                .map(|(target, edge)| Neighbor {
                    target,
                    type_id: edge.type_id,
                    schema_reversed: edge.schema_reversed,
                    relationship_id: None,
                })
                .collect::<Vec<_>>(),
            vec![
                Neighbor {
                    target: 1,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                },
                Neighbor {
                    target: 2,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                },
            ]
        );
    }

    #[test]
    fn layered_neighbors_suppresses_duplicates_across_layers() {
        let base = edge_store_from_tuples(3, &[(0, 1, 1)]);
        let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 3, 1)
            .expect("segment");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        let layered = LayeredNeighbors::new(&base, vec![segment]);

        assert_eq!(
            layered.neighbors(0).collect::<Vec<_>>(),
            vec![
                Neighbor {
                    target: 1,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                },
                Neighbor {
                    target: 2,
                    type_id: 1,
                    schema_reversed: false,
                    relationship_id: None,
                },
            ]
        );
    }

    #[test]
    fn weighted_shortest_path_uses_durable_weight_segments() {
        let base = weighted_edge_store_from_tuples(4, &[(0, 1, 1, 10)]);
        let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 4, 1)
            .expect("segment");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 2,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        segment.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 2,
            type_id: 1,
            weight: 3,
            schema_reversed: false,
        });
        let layered = LayeredNeighbors::new(&base, vec![segment]);

        assert_eq!(
            layered.weighted_neighbors(0),
            vec![
                WeightedNeighbor {
                    target: 1,
                    type_id: 1,
                    weight: 10,
                    schema_reversed: false,
                },
                WeightedNeighbor {
                    target: 2,
                    type_id: 1,
                    weight: 3,
                    schema_reversed: false,
                },
            ]
        );
    }

    #[test]
    fn layered_reads_hide_transaction_deleted_nodes() {
        tx_delta::clear_for_test();
        let base = edge_store_from_tuples(4, &[(0, 1, 1), (0, 2, 1), (3, 0, 1)]);
        tx_delta::record_deleted_node(0).expect("record source node delete");
        let source_deleted = LayeredNeighbors::new(&base, Vec::new());

        assert_eq!(source_deleted.neighbors(0).collect::<Vec<_>>(), Vec::new());

        tx_delta::clear_for_test();
        tx_delta::record_deleted_node(2).expect("record target node delete");
        let target_deleted = LayeredNeighbors::new(&base, Vec::new());

        assert_eq!(
            target_deleted.neighbors(0).collect::<Vec<_>>(),
            vec![Neighbor {
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: None,
            }]
        );
        tx_delta::clear_for_test();
    }

    #[test]
    fn weighted_neighbors_preserve_transaction_local_insert_weights() {
        tx_delta::clear_for_test();
        let base = weighted_edge_store_from_tuples(4, &[(0, 1, 1, 10)]);
        tx_delta::record_added_edge(
            0,
            tx_delta::DeltaEdge {
                target: 2,
                type_id: 1,
                weight: Some(4),
                schema_reversed: false,
                relationship_id: None,
            },
        )
        .expect("record weighted tx insert");
        let layered = LayeredNeighbors::new(&base, Vec::new());

        assert_eq!(
            layered.weighted_neighbors(0),
            vec![
                WeightedNeighbor {
                    target: 1,
                    type_id: 1,
                    weight: 10,
                    schema_reversed: false,
                },
                WeightedNeighbor {
                    target: 2,
                    type_id: 1,
                    weight: 4,
                    schema_reversed: false,
                },
            ]
        );
        tx_delta::clear_for_test();
    }

    #[test]
    fn layered_reads_apply_tenant_filter_and_node_visibility_segments() {
        let base = edge_store_from_tuples(4, &[(0, 1, 1), (0, 2, 1), (0, 3, 1)]);
        let mut node_segment =
            DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 4, 1)
                .expect("node segment");
        node_segment.tenants.push(SegmentTenant {
            node_idx: 0,
            tenant_hash: 42,
            tombstone: false,
        });
        node_segment.node_states.push(SegmentNodeState {
            node_idx: 2,
            active: false,
        });
        node_segment.tenants.push(SegmentTenant {
            node_idx: 1,
            tenant_hash: 42,
            tombstone: false,
        });
        node_segment.tenants.push(SegmentTenant {
            node_idx: 2,
            tenant_hash: 42,
            tombstone: false,
        });
        let layered = LayeredNeighbors::new_with_tenant(&base, vec![node_segment], Some(42));

        assert_eq!(
            layered.neighbors(0).collect::<Vec<_>>(),
            vec![Neighbor {
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: None,
            }]
        );
    }

    #[test]
    fn manifest_segment_provider_loads_real_segments() {
        let dir = ProjectionArtifactDir::new("manifest_segment_provider_loads_real_segments");
        let base = edge_store_from_tuples(2, &[]);
        let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 2, 1)
            .expect("segment");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        let segment_path = dir.segment_path(1, 0);
        segment
            .write_to_path(&segment_path)
            .expect("segment writes");
        let manifest = manifest_with_segment(&dir, 1, &segment_path, &segment);
        let provider = ManifestSegmentProvider::new(dir.path(), &manifest);
        let layered =
            LayeredNeighbors::from_provider(&base, &provider).expect("provider loads segments");

        assert_eq!(
            layered.neighbors(0).collect::<Vec<_>>(),
            vec![Neighbor {
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: None,
            }]
        );
    }

    #[test]
    fn manifest_segment_provider_rejects_checksum_mismatch() {
        let dir = ProjectionArtifactDir::new("manifest_segment_provider_rejects_checksum_mismatch");
        let base = edge_store_from_tuples(2, &[]);
        let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 2, 1)
            .expect("segment");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        let segment_path = dir.segment_path(1, 0);
        segment
            .write_to_path(&segment_path)
            .expect("segment writes");
        let mut manifest = manifest_with_segment(&dir, 1, &segment_path, &segment);
        manifest.segments[0].checksum = "crc32:00000000".to_string();
        let provider = ManifestSegmentProvider::new(dir.path(), &manifest);

        let err = match LayeredNeighbors::from_provider(&base, &provider) {
            Ok(_) => panic!("checksum mismatch should reject segment"),
            Err(err) => err,
        };

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn durable_delete_then_later_insert_reactivates_edge() {
        let base = edge_store_from_tuples(2, &[(0, 1, 1)]);
        let mut delete = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 2, 1)
            .expect("delete segment");
        delete.edge_deletes.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        let mut insert = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 2, 2)
            .expect("insert segment");
        insert.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 1,
            schema_reversed: false,
            relationship_id: None,
        });
        let layered = LayeredNeighbors::new(&base, vec![insert, delete]);

        assert_eq!(
            layered.neighbors(0).collect::<Vec<_>>(),
            vec![Neighbor {
                target: 1,
                type_id: 1,
                schema_reversed: false,
                relationship_id: None,
            }]
        );
    }

    proptest! {
        #[test]
        fn layered_without_segments_matches_base_csr(
            node_count in 1u32..16,
            raw_edges in prop::collection::vec((0u32..16, 0u32..16, 0u8..4), 0..96),
            query_node in 0u32..16,
        ) {
            tx_delta::clear_for_test();
            let edges = raw_edges
                .into_iter()
                .filter(|(source, target, _)| *source < node_count && *target < node_count)
                .collect::<Vec<_>>();
            let base = edge_store_from_tuples(node_count, &edges);
            let csr = CsrNeighbors::new(&base);
            let layered = LayeredNeighbors::new(&base, Vec::new());

            prop_assert_eq!(
                csr.neighbors(query_node).collect::<Vec<_>>(),
                layered.neighbors(query_node).collect::<Vec<_>>()
            );
            prop_assert_eq!(
                csr.neighbors_reversed(query_node).collect::<Vec<_>>(),
                layered.neighbors_reversed(query_node).collect::<Vec<_>>()
            );
        }
    }

    fn manifest_with_segment(
        dir: &ProjectionArtifactDir,
        generation_id: u64,
        segment_path: &Path,
        segment: &DeltaSegment,
    ) -> ProjectionManifest {
        let relative = segment_path
            .strip_prefix(dir.path())
            .expect("segment is inside artifact dir")
            .to_string_lossy()
            .to_string();
        let bytes = fs::read(segment_path).expect("segment bytes read");
        let mut manifest = ProjectionManifest::base_only(
            generation_id,
            "base.pggraph",
            "crc32:base",
            1,
            segment.header.sync_watermark,
            1,
        );
        manifest.segments.push(ManifestSegmentRef {
            path: relative,
            checksum: format!("crc32:{:08x}", crc32fast::hash(&bytes)),
            level: segment.header.level,
            source_start: segment.header.source_start,
            source_end: segment.header.source_end,
            sync_watermark: segment.header.sync_watermark,
        });
        manifest
    }
}
