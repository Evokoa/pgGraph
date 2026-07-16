//! # EdgeStore — Compressed Sparse Row (CSR) edge storage
//!
//! Stores all edges in parallel flat arrays. For node `i`, its outgoing edges
//! are `targets[edge_offsets[i]..edge_offsets[i+1]]`.
//!
//! ## Modes
//!
//! - **Owned** (build time): Data in `Vec<T>`, supports construction from raw edges.
//! - **Mmap** (load time): Forward CSR arrays are read from a backend-local
//!   immutable artifact snapshot.
//!
//! The engine currently derives a separate owned reverse CSR per backend from
//! the mmap-backed forward CSR so inbound traversal remains direct.
//!
//! ## Invariants
//!
//! - `edge_offsets.len() == node_count + 1`
//! - `edge_offsets` is monotonically non-decreasing
//! - `targets.len() == type_ids.len() == total_edge_count`
//! - `weights.len()` is either 0 (unweighted) or `total_edge_count`
//!
//! See: `docs/contributor_guide/memory-model.mdx`

use crate::mapped_bytes::MappedBytes;
use crate::safety::{GraphError, GraphResult};
use std::ops::Range;

/// Dense identifier for one relationship source row in a graph projection.
pub(crate) type RelationshipId = u32;
/// Reserved identity for internal/test edges with no PostgreSQL source row.
pub(crate) const NO_RELATIONSHIP_ID: RelationshipId = 0;

/// Durable source identity for one relationship row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct RelationshipIdentity {
    pub(crate) mapping_id: u64,
    pub(crate) source_key: String,
}

const EMPTY_U32_SLICE: [u32; 0] = [];
const EMPTY_U8_SLICE: [u8; 0] = [];

/// Validated, owning metadata for mmap-backed CSR edge arrays.
#[derive(Clone)]
pub(crate) struct MmapEdgeArrays {
    mmap: MappedBytes,
    offsets_range: Range<usize>,
    targets_range: Range<usize>,
    type_ids_range: Range<usize>,
    schema_reversed_range: Range<usize>,
    weights_range: Option<Range<usize>>,
    node_count: u32,
    edge_count: u32,
}

/// Byte ranges for mmap-backed edge arrays.
#[derive(Clone, Debug)]
pub(crate) struct MmapEdgeArrayParts {
    /// Read-only mapping retained by every store built from these ranges.
    pub(crate) mmap: MappedBytes,
    /// `node_count + 1` CSR offsets.
    pub(crate) offsets_range: Range<usize>,
    /// `edge_count` target node indexes.
    pub(crate) targets_range: Range<usize>,
    /// `edge_count` edge type identifiers.
    pub(crate) type_ids_range: Range<usize>,
    /// `edge_count` schema-reversed flags.
    pub(crate) schema_reversed_range: Range<usize>,
    /// `edge_count` weights when present.
    pub(crate) weights_range: Option<Range<usize>>,
    /// Number of nodes represented by the CSR offset table.
    pub(crate) node_count: u32,
    /// Number of edges represented by the parallel edge arrays.
    pub(crate) edge_count: u32,
}

impl MmapEdgeArrays {
    /// Create validated, owning mmap metadata.
    ///
    /// Validation covers byte ranges, typed alignment, monotonic CSR
    /// offsets, the terminal edge count, target bounds, and direction flags.
    pub(crate) fn new_for_artifact(
        parts: MmapEdgeArrayParts,
        _validated_artifact: &crate::persistence::ValidatedMappedGraphToken,
    ) -> Option<Self> {
        Self::validate(parts)
    }

    #[cfg(test)]
    pub(crate) fn new(parts: MmapEdgeArrayParts) -> Option<Self> {
        Self::validate(parts)
    }

    fn validate(parts: MmapEdgeArrayParts) -> Option<Self> {
        if !cfg!(target_endian = "little") {
            return None;
        }
        let offset_bytes = (parts.node_count as usize)
            .checked_add(1)?
            .checked_mul(std::mem::size_of::<u32>())?;
        let target_bytes = (parts.edge_count as usize).checked_mul(std::mem::size_of::<u32>())?;
        let type_id_bytes = parts.edge_count as usize;
        let schema_reversed_bytes = parts.edge_count as usize;
        let weight_bytes = (parts.edge_count as usize).checked_mul(std::mem::size_of::<u32>())?;
        if parts.offsets_range.len() != offset_bytes
            || parts.targets_range.len() != target_bytes
            || parts.type_ids_range.len() != type_id_bytes
            || parts.schema_reversed_range.len() != schema_reversed_bytes
            || parts
                .weights_range
                .as_ref()
                .is_some_and(|range| range.len() != weight_bytes)
            || !range_in_region(&parts.offsets_range, parts.mmap.len())
            || !range_in_region(&parts.targets_range, parts.mmap.len())
            || !range_in_region(&parts.type_ids_range, parts.mmap.len())
            || !range_in_region(&parts.schema_reversed_range, parts.mmap.len())
            || parts
                .weights_range
                .as_ref()
                .is_some_and(|range| !range_in_region(range, parts.mmap.len()))
        {
            return None;
        }
        let base = parts.mmap.as_slice().as_ptr() as usize;
        if !base
            .checked_add(parts.offsets_range.start)?
            .is_multiple_of(std::mem::align_of::<u32>())
            || !base
                .checked_add(parts.targets_range.start)?
                .is_multiple_of(std::mem::align_of::<u32>())
            || parts.weights_range.as_ref().is_some_and(|range| {
                base.checked_add(range.start)
                    .is_none_or(|address| !address.is_multiple_of(std::mem::align_of::<u32>()))
            })
        {
            return None;
        }

        let offsets = u32_slice(&parts.mmap, &parts.offsets_range);
        let targets = u32_slice(&parts.mmap, &parts.targets_range);
        let schema_reversed = &parts.mmap.as_slice()[parts.schema_reversed_range.clone()];

        if offsets.first().copied() != Some(0)
            || offsets.last().copied() != Some(parts.edge_count)
            || offsets
                .windows(2)
                .any(|window| window[0] > window[1] || window[1] > parts.edge_count)
            || targets.iter().any(|&target| target >= parts.node_count)
            || schema_reversed.iter().any(|&flag| flag > 1)
        {
            return None;
        }

        Some(Self {
            mmap: parts.mmap,
            offsets_range: parts.offsets_range,
            targets_range: parts.targets_range,
            type_ids_range: parts.type_ids_range,
            schema_reversed_range: parts.schema_reversed_range,
            weights_range: parts.weights_range,
            node_count: parts.node_count,
            edge_count: parts.edge_count,
        })
    }

    fn offsets(&self) -> &[u32] {
        u32_slice(&self.mmap, &self.offsets_range)
    }

    fn targets(&self) -> &[u32] {
        u32_slice(&self.mmap, &self.targets_range)
    }

    fn type_ids(&self) -> &[u8] {
        &self.mmap.as_slice()[self.type_ids_range.clone()]
    }

    fn schema_reversed(&self) -> &[u8] {
        &self.mmap.as_slice()[self.schema_reversed_range.clone()]
    }

    fn weights(&self) -> &[u32] {
        self.weights_range
            .as_ref()
            .map_or(&[], |range| u32_slice(&self.mmap, range))
    }

    fn neighbor_range(&self, node_idx: u32) -> Option<Range<usize>> {
        if node_idx >= self.node_count {
            return None;
        }
        let offsets = self.offsets();
        let start = *offsets.get(node_idx as usize)? as usize;
        let end = *offsets.get(node_idx as usize + 1)? as usize;
        (start <= end && end <= self.edge_count as usize).then_some(start..end)
    }
}

fn range_in_region(range: &Range<usize>, region_len: usize) -> bool {
    range.start <= range.end && range.end <= region_len
}

fn u32_slice<'a>(mmap: &'a MappedBytes, range: &Range<usize>) -> &'a [u32] {
    // SAFETY: MmapEdgeArrays validation proves that this range is in the mapping,
    // aligned for u32, and has a byte length divisible by four. The mapping is
    // read-only and retained by Arc for the returned slice's lifetime.
    unsafe {
        std::slice::from_raw_parts(
            mmap.as_slice()[range.clone()].as_ptr().cast::<u32>(),
            range.len() / std::mem::size_of::<u32>(),
        )
    }
}

/// Backing store for edge data.
enum EdgeBacking {
    /// Build-time: owned Vecs.
    Owned {
        edge_offsets: Vec<u32>,
        targets: Vec<u32>,
        type_ids: Vec<u8>,
        schema_reversed: Vec<u8>,
        weights: Vec<u32>,
    },
    /// Load-time: validated ranges into an Arc-owned read-only mapping.
    Mmap { arrays: MmapEdgeArrays },
}

/// Raw edge triple before CSR construction.
#[derive(Debug, Clone)]
pub struct RawEdge {
    /// Source node index.
    pub source: u32,
    /// Target node index.
    pub target: u32,
    /// Registered edge type identifier.
    pub type_id: u8,
    /// Optional edge weight; unweighted stores ignore this value.
    pub weight: Option<u32>,
    /// Whether this adjacency is the generated reverse copy of a registered
    /// bidirectional edge row.
    pub schema_reversed: bool,
}

/// A raw adjacency paired with its durable relationship dictionary entry.
#[derive(Debug, Clone)]
pub(crate) struct IdentifiedRawEdge {
    pub(crate) edge: RawEdge,
    pub(crate) relationship_id: RelationshipId,
}

/// Incremental CSR builder for raw edges already sorted by
/// `(source, target, type_id, schema_reversed)`.
///
/// This lets the graph builder stream sorted rows from PostgreSQL temp storage
/// without retaining a full `Vec<RawEdge>` in Rust.
pub struct SortedEdgeStoreBuilder {
    node_count: u32,
    has_weights: bool,
    edge_offsets: Vec<u32>,
    targets: Vec<u32>,
    type_ids: Vec<u8>,
    schema_reversed: Vec<u8>,
    weights: Vec<u32>,
    relationship_ids: Vec<RelationshipId>,
    current_node: u32,
    edge_capacity: Option<usize>,
}

impl SortedEdgeStoreBuilder {
    /// Return the exact element-storage bytes requested by a bounded builder.
    ///
    /// This excludes the fixed-size builder and `Vec` headers themselves. A
    /// caller can add `size_of::<Self>()` when accounting for total private
    /// heap plus stack state.
    ///
    /// # Errors
    ///
    /// Returns an internal error when a count or byte total cannot be
    /// represented by the CSR format or the current platform.
    pub(crate) fn planned_storage_bytes(
        node_count: u32,
        edge_capacity: usize,
        has_weights: bool,
    ) -> GraphResult<usize> {
        if edge_capacity > u32::MAX as usize {
            return Err(GraphError::Internal(
                "CSR edge count exceeds the u32 offset format".to_string(),
            ));
        }
        let offsets = usize::try_from(node_count)
            .ok()
            .and_then(|count| count.checked_add(1))
            .and_then(|count| count.checked_mul(std::mem::size_of::<u32>()))
            .ok_or_else(|| GraphError::Internal("CSR offset bytes overflowed usize".to_string()))?;
        let per_edge = std::mem::size_of::<u32>()
            .checked_add(std::mem::size_of::<u8>())
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<u8>()))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<RelationshipId>()))
            .and_then(|bytes| {
                bytes.checked_add(if has_weights {
                    std::mem::size_of::<u32>()
                } else {
                    0
                })
            })
            .ok_or_else(|| GraphError::Internal("CSR edge width overflowed usize".to_string()))?;
        offsets
            .checked_add(edge_capacity.checked_mul(per_edge).ok_or_else(|| {
                GraphError::Internal("CSR edge bytes overflowed usize".to_string())
            })?)
            .ok_or_else(|| GraphError::Internal("CSR storage bytes overflowed usize".to_string()))
    }

    /// Create a streaming CSR builder for edges sorted by source node.
    #[cfg(test)]
    pub fn new(node_count: u32, has_weights: bool) -> Self {
        Self {
            node_count,
            has_weights,
            edge_offsets: vec![0],
            targets: Vec::new(),
            type_ids: Vec::new(),
            schema_reversed: Vec::new(),
            weights: Vec::new(),
            relationship_ids: Vec::new(),
            current_node: 0,
            edge_capacity: None,
        }
    }

    /// Create a streaming CSR builder with fallible initial allocation.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the offset count cannot fit the current
    /// platform, or `GraphError::Oom` when the initial allocation fails.
    pub(crate) fn try_new(node_count: u32, has_weights: bool) -> GraphResult<Self> {
        let mut edge_offsets = Vec::new();
        let offset_capacity = usize::try_from(node_count)
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| GraphError::Internal("CSR offset count overflowed usize".to_string()))?;
        edge_offsets
            .try_reserve(offset_capacity)
            .map_err(csr_allocation_error)?;
        edge_offsets.push(0);
        Ok(Self {
            node_count,
            has_weights,
            edge_offsets,
            targets: Vec::new(),
            type_ids: Vec::new(),
            schema_reversed: Vec::new(),
            weights: Vec::new(),
            relationship_ids: Vec::new(),
            current_node: 0,
            edge_capacity: None,
        })
    }

    /// Create a streaming CSR builder with exact storage for a known edge count.
    ///
    /// Callers should derive `edge_capacity` during their resource preflight and
    /// reserve the corresponding bytes with their operation governor before
    /// constructing the builder. The builder will not grow any edge array beyond
    /// this capacity.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the offset count cannot fit the current
    /// platform, or [`GraphError::Oom`] when an initial allocation fails.
    pub(crate) fn try_new_with_edge_capacity(
        node_count: u32,
        edge_capacity: usize,
        has_weights: bool,
    ) -> GraphResult<Self> {
        Self::planned_storage_bytes(node_count, edge_capacity, has_weights)?;
        let offset_capacity = usize::try_from(node_count)
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| GraphError::Internal("CSR offset count overflowed usize".to_string()))?;
        let mut edge_offsets = Vec::new();
        edge_offsets
            .try_reserve_exact(offset_capacity)
            .map_err(csr_allocation_error)?;
        edge_offsets.push(0);

        let mut targets = Vec::new();
        targets
            .try_reserve_exact(edge_capacity)
            .map_err(csr_allocation_error)?;
        let mut type_ids = Vec::new();
        type_ids
            .try_reserve_exact(edge_capacity)
            .map_err(csr_allocation_error)?;
        let mut schema_reversed = Vec::new();
        schema_reversed
            .try_reserve_exact(edge_capacity)
            .map_err(csr_allocation_error)?;
        let mut relationship_ids = Vec::new();
        relationship_ids
            .try_reserve_exact(edge_capacity)
            .map_err(csr_allocation_error)?;
        let mut weights = Vec::new();
        if has_weights {
            weights
                .try_reserve_exact(edge_capacity)
                .map_err(csr_allocation_error)?;
        }

        Ok(Self {
            node_count,
            has_weights,
            edge_offsets,
            targets,
            type_ids,
            schema_reversed,
            weights,
            relationship_ids,
            current_node: 0,
            edge_capacity: Some(edge_capacity),
        })
    }

    /// Add one sorted edge, rejecting endpoints outside the node range.
    ///
    /// Every source relationship row is retained, including rows with equal
    /// endpoints and type. PostgreSQL relationship tables have bag semantics;
    /// topology alone is not a relationship identity.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Internal`] when `edge.source` or `edge.target`
    /// is greater than or equal to the builder's node count.
    pub fn try_push(&mut self, edge: RawEdge) -> GraphResult<()> {
        self.try_push_identified(IdentifiedRawEdge {
            edge,
            relationship_id: NO_RELATIONSHIP_ID,
        })
    }

    /// Add one sorted edge with its source-row relationship identity.
    pub(crate) fn try_push_identified(&mut self, identified: IdentifiedRawEdge) -> GraphResult<()> {
        let edge = identified.edge;
        validate_raw_edge(self.node_count, &edge)?;
        if self
            .edge_capacity
            .is_some_and(|capacity| self.targets.len() >= capacity)
        {
            return Err(GraphError::Internal(
                "CSR edge count exceeded its preflight capacity".to_string(),
            ));
        }
        self.targets.try_reserve(1).map_err(csr_allocation_error)?;
        self.type_ids.try_reserve(1).map_err(csr_allocation_error)?;
        self.schema_reversed
            .try_reserve(1)
            .map_err(csr_allocation_error)?;
        self.relationship_ids
            .try_reserve(1)
            .map_err(csr_allocation_error)?;
        if self.has_weights {
            self.weights.try_reserve(1).map_err(csr_allocation_error)?;
        }
        while self.current_node < edge.source {
            self.current_node += 1;
            self.edge_offsets.push(self.targets.len() as u32);
        }
        self.targets.push(edge.target);
        self.type_ids.push(edge.type_id);
        self.schema_reversed.push(u8::from(edge.schema_reversed));
        self.relationship_ids.push(identified.relationship_id);
        if self.has_weights {
            self.weights.push(edge.weight.unwrap_or(1));
        }
        Ok(())
    }

    /// Finalize the streamed CSR arrays into an owned [`EdgeStore`].
    pub fn finish(mut self) -> EdgeStore {
        while self.edge_offsets.len() < self.node_count as usize + 1 {
            self.edge_offsets.push(self.targets.len() as u32);
        }

        EdgeStore {
            backing: EdgeBacking::Owned {
                edge_offsets: self.edge_offsets,
                targets: self.targets,
                type_ids: self.type_ids,
                schema_reversed: self.schema_reversed,
                weights: self.weights,
            },
            relationship_ids: self.relationship_ids,
        }
    }
}

fn csr_allocation_error(_error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
    }
}

fn try_filled_vec<T: Clone>(len: usize, value: T) -> GraphResult<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(csr_allocation_error)?;
    values.resize(len, value);
    Ok(values)
}

/// Compressed Sparse Row (CSR) edge storage.
pub struct EdgeStore {
    backing: EdgeBacking,
    relationship_ids: Vec<RelationshipId>,
}

impl EdgeStore {
    /// Create an empty EdgeStore.
    pub fn new() -> Self {
        Self {
            backing: EdgeBacking::Owned {
                edge_offsets: vec![0],
                targets: Vec::new(),
                type_ids: Vec::new(),
                schema_reversed: Vec::new(),
                weights: Vec::new(),
            },
            relationship_ids: Vec::new(),
        }
    }

    /// Create an mmap-backed EdgeStore from owning ranges and relationship IDs.
    pub(crate) fn from_mmap_with_relationship_ids(
        arrays: MmapEdgeArrays,
        relationship_ids: Vec<RelationshipId>,
    ) -> Self {
        debug_assert_eq!(relationship_ids.len(), arrays.edge_count as usize);
        Self {
            backing: EdgeBacking::Mmap { arrays },
            relationship_ids,
        }
    }

    /// Build a CSR EdgeStore from unsorted raw edges.
    #[cfg(test)]
    pub fn from_edges(node_count: u32, edges: Vec<RawEdge>, has_weights: bool) -> Self {
        Self::try_from_edges(node_count, edges, has_weights)
            .expect("trusted edge store input has valid endpoints")
    }

    /// Build a CSR EdgeStore from unsorted raw edges, rejecting invalid endpoints.
    pub fn try_from_edges(
        node_count: u32,
        edges: Vec<RawEdge>,
        has_weights: bool,
    ) -> GraphResult<Self> {
        for edge in &edges {
            validate_raw_edge(node_count, edge)?;
        }
        Ok(Self::from_valid_edges(node_count, edges, has_weights))
    }

    fn from_valid_edges(node_count: u32, mut edges: Vec<RawEdge>, has_weights: bool) -> Self {
        // Sort by source, then target, then type_id, then registered-direction
        // flag so real opposite rows do not collapse with synthetic reverse
        // copies of bidirectional rows.
        edges.sort_unstable_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then(a.target.cmp(&b.target))
                .then(a.type_id.cmp(&b.type_id))
                .then(a.schema_reversed.cmp(&b.schema_reversed))
        });

        let edge_count = edges.len();

        // Build CSR arrays
        let mut edge_offsets = Vec::with_capacity(node_count as usize + 1);
        let mut targets = Vec::with_capacity(edge_count);
        let mut type_ids = Vec::with_capacity(edge_count);
        let mut schema_reversed = Vec::with_capacity(edge_count);
        let mut weights = if has_weights {
            Vec::with_capacity(edge_count)
        } else {
            Vec::new()
        };

        let mut edge_idx = 0;
        for node in 0..node_count {
            edge_offsets.push(targets.len() as u32);
            while edge_idx < edges.len() && edges[edge_idx].source == node {
                targets.push(edges[edge_idx].target);
                type_ids.push(edges[edge_idx].type_id);
                schema_reversed.push(u8::from(edges[edge_idx].schema_reversed));
                if has_weights {
                    weights.push(edges[edge_idx].weight.unwrap_or(1));
                }
                edge_idx += 1;
            }
        }
        edge_offsets.push(targets.len() as u32);

        Self {
            backing: EdgeBacking::Owned {
                edge_offsets,
                targets,
                type_ids,
                schema_reversed,
                weights,
            },
            relationship_ids: vec![NO_RELATIONSHIP_ID; edge_count],
        }
    }

    /// Build a CSR EdgeStore from already sorted raw edges without retaining the
    /// raw edge list. Input must be ordered by
    /// `(source, target, type_id, schema_reversed)`.
    #[cfg(test)]
    pub fn from_sorted_edges<I>(node_count: u32, edges: I, has_weights: bool) -> Self
    where
        I: IntoIterator<Item = RawEdge>,
    {
        Self::try_from_sorted_edges(node_count, edges, has_weights)
            .expect("trusted sorted edge store input has valid endpoints")
    }

    /// Build a CSR EdgeStore from sorted raw edges, rejecting invalid endpoints.
    pub fn try_from_sorted_edges<I>(
        node_count: u32,
        edges: I,
        has_weights: bool,
    ) -> GraphResult<Self>
    where
        I: IntoIterator<Item = RawEdge>,
    {
        let mut builder = SortedEdgeStoreBuilder::try_new(node_count, has_weights)?;
        for edge in edges {
            builder.try_push(edge)?;
        }
        Ok(builder.finish())
    }

    /// Build a reverse CSR from this store's directed edge contents.
    ///
    /// The returned store owns its arrays. This keeps inbound traversal fast
    /// even when the forward graph was loaded from an mmap-backed file.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::Oom` when a reverse CSR allocation fails, or an
    /// internal error when a required array length overflows.
    pub(crate) fn try_reversed(&self) -> GraphResult<Self> {
        let has_weights = self.has_weights();
        let node_count = self.node_count();
        let edge_count = self.edge_count() as usize;
        let offset_count = (node_count as usize).checked_add(1).ok_or_else(|| {
            GraphError::Internal("reverse CSR offset count overflowed".to_string())
        })?;
        let mut edge_offsets = try_filled_vec(offset_count, 0u32)?;

        for source in 0..node_count {
            let (targets, _) = self.neighbors(source);
            for &target in targets {
                edge_offsets[target as usize + 1] += 1;
            }
        }

        for idx in 1..edge_offsets.len() {
            edge_offsets[idx] += edge_offsets[idx - 1];
        }

        let mut reversed_targets = try_filled_vec(edge_count, 0u32)?;
        let mut reversed_type_ids = try_filled_vec(edge_count, 0u8)?;
        let mut reversed_schema_reversed = try_filled_vec(edge_count, 0u8)?;
        let mut reversed_weights = if has_weights {
            try_filled_vec(edge_count, 0u32)?
        } else {
            Vec::new()
        };
        let mut reversed_relationship_ids = try_filled_vec(edge_count, NO_RELATIONSHIP_ID)?;
        let mut write_offsets = Vec::new();
        write_offsets
            .try_reserve_exact(edge_offsets.len())
            .map_err(csr_allocation_error)?;
        write_offsets.extend_from_slice(&edge_offsets);

        for source in 0..self.node_count() {
            let (targets, type_ids, schema_reversed, weights) =
                self.neighbors_weighted_with_schema(source);
            for (idx, (&target, &type_id)) in targets.iter().zip(type_ids.iter()).enumerate() {
                let write_idx = write_offsets[target as usize] as usize;
                write_offsets[target as usize] += 1;
                reversed_targets[write_idx] = source;
                reversed_type_ids[write_idx] = type_id;
                reversed_schema_reversed[write_idx] = schema_reversed[idx];
                reversed_relationship_ids[write_idx] = self
                    .relationship_ids
                    .get(self.offsets_slice()[source as usize] as usize + idx)
                    .copied()
                    .unwrap_or(NO_RELATIONSHIP_ID);
                if has_weights {
                    reversed_weights[write_idx] = weights.get(idx).copied().unwrap_or(1);
                }
            }
        }

        Ok(Self {
            backing: EdgeBacking::Owned {
                edge_offsets,
                targets: reversed_targets,
                type_ids: reversed_type_ids,
                schema_reversed: reversed_schema_reversed,
                weights: reversed_weights,
            },
            relationship_ids: reversed_relationship_ids,
        })
    }

    #[cfg(test)]
    /// Build a reverse CSR for trusted unit-test fixtures.
    pub fn reversed(&self) -> Self {
        self.try_reversed()
            .expect("test reverse CSR allocation should succeed")
    }

    /// Get the neighbor slice for a node. This is the BFS hot-loop access.
    ///
    /// Returns `(target_slice, type_id_slice)`.
    #[inline(always)]
    pub fn neighbors(&self, node_idx: u32) -> (&[u32], &[u8]) {
        match &self.backing {
            EdgeBacking::Owned {
                edge_offsets,
                targets,
                type_ids,
                ..
            } => {
                if node_idx as usize + 1 >= edge_offsets.len() {
                    return (&EMPTY_U32_SLICE, &EMPTY_U8_SLICE);
                }
                let start = edge_offsets[node_idx as usize] as usize;
                let end = edge_offsets[node_idx as usize + 1] as usize;
                (&targets[start..end], &type_ids[start..end])
            }
            EdgeBacking::Mmap { arrays } => {
                let Some(range) = arrays.neighbor_range(node_idx) else {
                    return (&EMPTY_U32_SLICE, &EMPTY_U8_SLICE);
                };
                (&arrays.targets()[range.clone()], &arrays.type_ids()[range])
            }
        }
    }

    /// Get the neighbor slice with registered-direction metadata.
    ///
    /// The third slice contains `0` for an adjacency in the registered source
    /// row direction and `1` for the generated reverse copy of a bidirectional
    /// row.
    #[inline(always)]
    pub fn neighbors_with_schema(&self, node_idx: u32) -> (&[u32], &[u8], &[u8]) {
        match &self.backing {
            EdgeBacking::Owned {
                edge_offsets,
                targets,
                type_ids,
                schema_reversed,
                ..
            } => {
                if node_idx as usize + 1 >= edge_offsets.len() {
                    return (&EMPTY_U32_SLICE, &EMPTY_U8_SLICE, &EMPTY_U8_SLICE);
                }
                let start = edge_offsets[node_idx as usize] as usize;
                let end = edge_offsets[node_idx as usize + 1] as usize;
                (
                    &targets[start..end],
                    &type_ids[start..end],
                    &schema_reversed[start..end],
                )
            }
            EdgeBacking::Mmap { arrays } => {
                let Some(range) = arrays.neighbor_range(node_idx) else {
                    return (&EMPTY_U32_SLICE, &EMPTY_U8_SLICE, &EMPTY_U8_SLICE);
                };
                (
                    &arrays.targets()[range.clone()],
                    &arrays.type_ids()[range.clone()],
                    &arrays.schema_reversed()[range],
                )
            }
        }
    }

    /// Get the neighbor slice with registered-direction metadata and
    /// relationship identity sidecars.
    ///
    /// The relationship ID slice is in the same adjacency order as the target,
    /// type, and registered-direction slices. A value of
    /// [`NO_RELATIONSHIP_ID`] means this adjacency does not have a durable
    /// source-row relationship identity.
    #[inline(always)]
    pub(crate) fn neighbors_with_schema_and_relationship_ids(
        &self,
        node_idx: u32,
    ) -> (&[u32], &[u8], &[u8], &[RelationshipId]) {
        let (targets, type_ids, schema_reversed) = self.neighbors_with_schema(node_idx);
        let start = self
            .offsets_slice()
            .get(node_idx as usize)
            .copied()
            .unwrap_or(0) as usize;
        let end = start.saturating_add(targets.len());
        let relationship_ids = self.relationship_ids.get(start..end).unwrap_or(&[]);
        (targets, type_ids, schema_reversed, relationship_ids)
    }

    /// Get the neighbor slice with weights for Dijkstra.
    #[inline]
    pub fn neighbors_weighted(&self, node_idx: u32) -> (&[u32], &[u8], &[u32]) {
        if node_idx >= self.node_count() {
            return (&EMPTY_U32_SLICE, &EMPTY_U8_SLICE, &EMPTY_U32_SLICE);
        }

        match &self.backing {
            EdgeBacking::Owned {
                edge_offsets,
                targets,
                type_ids,
                schema_reversed: _,
                weights,
            } => {
                let start = edge_offsets[node_idx as usize] as usize;
                let end = edge_offsets[node_idx as usize + 1] as usize;
                if weights.is_empty() {
                    return (&targets[start..end], &type_ids[start..end], &[]);
                }

                (
                    &targets[start..end],
                    &type_ids[start..end],
                    &weights[start..end],
                )
            }
            EdgeBacking::Mmap { arrays } => {
                let Some(range) = arrays.neighbor_range(node_idx) else {
                    return (&EMPTY_U32_SLICE, &EMPTY_U8_SLICE, &EMPTY_U32_SLICE);
                };
                let targets = &arrays.targets()[range.clone()];
                let type_ids = &arrays.type_ids()[range.clone()];
                if arrays.weights_range.is_none() {
                    return (targets, type_ids, &[]);
                }
                (targets, type_ids, &arrays.weights()[range])
            }
        }
    }

    /// Get the neighbor slice with weights and registered-direction metadata.
    #[inline]
    pub fn neighbors_weighted_with_schema(&self, node_idx: u32) -> (&[u32], &[u8], &[u8], &[u32]) {
        let (targets, type_ids, schema_reversed) = self.neighbors_with_schema(node_idx);
        let (_, _, weights) = self.neighbors_weighted(node_idx);
        (targets, type_ids, schema_reversed, weights)
    }

    /// Number of edges.
    pub fn edge_count(&self) -> u32 {
        match &self.backing {
            EdgeBacking::Owned { targets, .. } => targets.len() as u32,
            EdgeBacking::Mmap { arrays } => arrays.edge_count,
        }
    }

    /// Number of nodes the CSR is sized for.
    pub fn node_count(&self) -> u32 {
        match &self.backing {
            EdgeBacking::Owned { edge_offsets, .. } => {
                if edge_offsets.is_empty() {
                    0
                } else {
                    (edge_offsets.len() - 1) as u32
                }
            }
            EdgeBacking::Mmap { arrays } => arrays.node_count,
        }
    }

    /// Whether this EdgeStore has weight data.
    pub fn has_weights(&self) -> bool {
        match &self.backing {
            EdgeBacking::Owned { weights, .. } => !weights.is_empty(),
            EdgeBacking::Mmap { arrays } => arrays.weights_range.is_some(),
        }
    }

    /// Estimate heap bytes owned directly by this store.
    ///
    /// Mapped arrays are accounted once through the engine's private snapshot.
    pub fn estimated_heap_bytes(&self) -> usize {
        match &self.backing {
            EdgeBacking::Owned {
                edge_offsets,
                targets,
                type_ids,
                schema_reversed,
                weights,
            } => {
                edge_offsets.capacity() * std::mem::size_of::<u32>()
                    + targets.capacity() * std::mem::size_of::<u32>()
                    + type_ids.capacity() * std::mem::size_of::<u8>()
                    + schema_reversed.capacity() * std::mem::size_of::<u8>()
                    + weights.capacity() * std::mem::size_of::<u32>()
            }
            EdgeBacking::Mmap { .. } => 0,
        }
    }

    /// Estimate bytes borrowed from an mmap-backed graph artifact.
    pub fn estimated_mmap_bytes(&self) -> usize {
        match &self.backing {
            EdgeBacking::Owned { .. } => 0,
            EdgeBacking::Mmap { arrays } => {
                let weight_bytes = arrays.weights_range.as_ref().map_or(0, Range::len);
                (arrays.node_count as usize + 1)
                    .saturating_mul(std::mem::size_of::<u32>())
                    .saturating_add(arrays.edge_count as usize * std::mem::size_of::<u32>())
                    .saturating_add(arrays.edge_count as usize * std::mem::size_of::<u8>())
                    .saturating_add(arrays.edge_count as usize * std::mem::size_of::<u8>())
                    .saturating_add(weight_bytes)
            }
        }
    }

    /// Degree (number of outgoing edges) for a node.
    #[inline]
    pub fn degree(&self, node_idx: u32) -> u32 {
        match &self.backing {
            EdgeBacking::Owned { edge_offsets, .. } => {
                if node_idx as usize + 1 >= edge_offsets.len() {
                    return 0;
                }
                let start = edge_offsets[node_idx as usize];
                let end = edge_offsets[node_idx as usize + 1];
                end - start
            }
            EdgeBacking::Mmap { arrays } => {
                if node_idx >= arrays.node_count {
                    return 0;
                }
                let start = arrays.offsets()[node_idx as usize];
                let end = arrays.offsets()[node_idx as usize + 1];
                end.saturating_sub(start)
            }
        }
    }

    // ── Persistence helpers ──

    /// Get edge_offsets as a slice. Used by persistence.
    pub fn offsets_slice(&self) -> &[u32] {
        match &self.backing {
            EdgeBacking::Owned { edge_offsets, .. } => edge_offsets,
            EdgeBacking::Mmap { arrays } => arrays.offsets(),
        }
    }

    /// Get targets as a slice. Used by persistence.
    pub fn targets_slice(&self) -> &[u32] {
        match &self.backing {
            EdgeBacking::Owned { targets, .. } => targets,
            EdgeBacking::Mmap { arrays } => arrays.targets(),
        }
    }

    /// Get type_ids as a slice. Used by persistence.
    pub fn type_ids_slice(&self) -> &[u8] {
        match &self.backing {
            EdgeBacking::Owned { type_ids, .. } => type_ids,
            EdgeBacking::Mmap { arrays } => arrays.type_ids(),
        }
    }

    /// Get schema-reversed flags as a slice.
    pub fn schema_reversed_slice(&self) -> &[u8] {
        match &self.backing {
            EdgeBacking::Owned {
                schema_reversed, ..
            } => schema_reversed,
            EdgeBacking::Mmap { arrays } => arrays.schema_reversed(),
        }
    }

    /// Get weights as a slice. Empty means unweighted.
    pub fn weights_slice(&self) -> &[u32] {
        match &self.backing {
            EdgeBacking::Owned { weights, .. } => weights,
            EdgeBacking::Mmap { arrays } => arrays.weights(),
        }
    }

    /// Relationship IDs in CSR adjacency order.
    pub(crate) fn relationship_ids_slice(&self) -> &[RelationshipId] {
        &self.relationship_ids
    }
}

fn validate_raw_edge(node_count: u32, edge: &RawEdge) -> GraphResult<()> {
    if edge.source >= node_count {
        return Err(GraphError::Internal(format!(
            "edge source {} is outside node range 0..{}",
            edge.source, node_count
        )));
    }
    if edge.target >= node_count {
        return Err(GraphError::Internal(format!(
            "edge target {} is outside node range 0..{}",
            edge.target, node_count
        )));
    }
    Ok(())
}

impl Default for EdgeStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! Covers CSR edge construction, degree/neighborhood queries, reverse-edge
    //! handling, and mmap loading invariants for persisted edge data.

    use super::*;
    use proptest::prelude::*;

    #[derive(Clone)]
    struct MappedEdgeFixture {
        offsets: [u32; 3],
        targets: [u32; 1],
        type_ids: [u8; 1],
        schema_reversed: [u8; 1],
        weights: [u32; 1],
    }

    impl MappedEdgeFixture {
        fn valid() -> Self {
            Self {
                offsets: [0, 1, 1],
                targets: [1],
                type_ids: [7],
                schema_reversed: [0],
                weights: [9],
            }
        }

        fn parts(&self) -> MmapEdgeArrayParts {
            let offsets_range = 0..std::mem::size_of_val(&self.offsets);
            let targets_start = offsets_range
                .end
                .next_multiple_of(std::mem::align_of::<u32>());
            let targets_range = targets_start..targets_start + std::mem::size_of_val(&self.targets);
            let type_ids_range = targets_range.end..targets_range.end + self.type_ids.len();
            let schema_reversed_range =
                type_ids_range.end..type_ids_range.end + self.schema_reversed.len();
            let weights_start = schema_reversed_range
                .end
                .next_multiple_of(std::mem::align_of::<u32>());
            let weights_range = weights_start..weights_start + std::mem::size_of_val(&self.weights);
            let mut mapping = vec![0u8; weights_range.end];
            for (chunk, value) in mapping[offsets_range.clone()]
                .chunks_exact_mut(4)
                .zip(self.offsets)
            {
                chunk.copy_from_slice(&value.to_le_bytes());
            }
            for (chunk, value) in mapping[targets_range.clone()]
                .chunks_exact_mut(4)
                .zip(self.targets)
            {
                chunk.copy_from_slice(&value.to_le_bytes());
            }
            mapping[type_ids_range.clone()].copy_from_slice(&self.type_ids);
            mapping[schema_reversed_range.clone()].copy_from_slice(&self.schema_reversed);
            for (chunk, value) in mapping[weights_range.clone()]
                .chunks_exact_mut(4)
                .zip(self.weights)
            {
                chunk.copy_from_slice(&value.to_le_bytes());
            }
            MmapEdgeArrayParts {
                mmap: MappedBytes::from_test_bytes(mapping),
                offsets_range,
                targets_range,
                type_ids_range,
                schema_reversed_range,
                weights_range: Some(weights_range),
                node_count: 2,
                edge_count: 1,
            }
        }
    }

    #[test]
    fn exact_capacity_builder_rejects_edges_beyond_its_preflight() {
        assert_eq!(
            SortedEdgeStoreBuilder::planned_storage_bytes(3, 2, true)
                .expect("bounded storage bytes"),
            4 * std::mem::size_of::<u32>()
                + 2 * (std::mem::size_of::<u32>()
                    + 2 * std::mem::size_of::<u8>()
                    + std::mem::size_of::<RelationshipId>()
                    + std::mem::size_of::<u32>())
        );
        let mut builder = SortedEdgeStoreBuilder::try_new_with_edge_capacity(3, 2, true)
            .expect("bounded builder");
        let initial_capacities = (
            builder.targets.capacity(),
            builder.type_ids.capacity(),
            builder.schema_reversed.capacity(),
            builder.weights.capacity(),
            builder.relationship_ids.capacity(),
        );
        for (source, target) in [(0, 1), (1, 2)] {
            builder
                .try_push(RawEdge {
                    source,
                    target,
                    type_id: 1,
                    weight: Some(7),
                    schema_reversed: false,
                })
                .expect("edge inside preflight");
        }
        assert_eq!(
            initial_capacities,
            (
                builder.targets.capacity(),
                builder.type_ids.capacity(),
                builder.schema_reversed.capacity(),
                builder.weights.capacity(),
                builder.relationship_ids.capacity(),
            )
        );

        let error = builder
            .try_push(RawEdge {
                source: 2,
                target: 0,
                type_id: 1,
                weight: Some(7),
                schema_reversed: false,
            })
            .expect_err("third edge exceeds the preflight");
        assert!(
            matches!(error, GraphError::Internal(reason) if reason.contains("preflight capacity"))
        );
    }

    #[test]
    fn empty_graph() {
        let store = EdgeStore::from_edges(3, vec![], false);
        assert_eq!(store.node_count(), 3);
        assert_eq!(store.edge_count(), 0);
        assert_eq!(store.neighbors(0), (&[][..], &[][..]));
        assert_eq!(store.degree(0), 0);
    }

    #[test]
    fn simple_chain() {
        // 0 → 1 → 2
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
        ];
        let store = EdgeStore::from_edges(3, edges, false);
        assert_eq!(store.edge_count(), 2);
        assert_eq!(store.neighbors(0).0, &[1]);
        assert_eq!(store.neighbors(1).0, &[2]);
        assert_eq!(store.neighbors(2).0, &[] as &[u32]);
        assert_eq!(store.degree(0), 1);
        assert_eq!(store.degree(2), 0);
    }

    #[test]
    fn reversed_builds_inbound_csr_without_scanning_at_query_time() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 7,
                weight: Some(3),
                schema_reversed: false,
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: 9,
                weight: Some(5),
                schema_reversed: false,
            },
        ];
        let store = EdgeStore::from_edges(3, edges, true);
        let reverse = store.reversed();

        let (targets, type_ids, weights) = reverse.neighbors_weighted(1);
        assert_eq!(targets, &[0, 2]);
        assert_eq!(type_ids, &[7, 9]);
        assert_eq!(weights, &[3, 5]);
        assert_eq!(reverse.neighbors(0).0, &[] as &[u32]);
    }

    #[test]
    fn preserves_parallel_edges() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            }, // duplicate
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
        ];
        let store = EdgeStore::from_edges(3, edges, false);
        assert_eq!(store.edge_count(), 3);
        assert_eq!(store.neighbors(0).0, &[1, 1, 2]);
    }

    #[test]
    fn sorts_edges_by_source() {
        let edges = vec![
            RawEdge {
                source: 2,
                target: 0,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
        ];
        let store = EdgeStore::from_edges(3, edges, false);
        assert_eq!(store.neighbors(0).0, &[1]);
        assert_eq!(store.neighbors(2).0, &[0]);
    }

    #[test]
    fn weighted_edges() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: Some(10),
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: Some(20),
                schema_reversed: false,
            },
        ];
        let store = EdgeStore::from_edges(3, edges, true);
        assert!(store.has_weights());
        let (targets, _types, weights) = store.neighbors_weighted(0);
        assert_eq!(targets, &[1, 2]);
        assert_eq!(weights, &[10, 20]);
    }

    #[test]
    fn weighted_edges_default_missing_weight_to_one() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: Some(8),
                schema_reversed: false,
            },
        ];

        let store = EdgeStore::from_edges(3, edges, true);
        let (targets, _types, weights) = store.neighbors_weighted(0);

        assert_eq!(targets, &[1, 2]);
        assert_eq!(weights, &[1, 8]);
    }

    #[test]
    fn unweighted_neighbors_return_empty_weight_slice() {
        let edges = vec![RawEdge {
            source: 0,
            target: 1,
            type_id: 1,
            weight: Some(99),
            schema_reversed: false,
        }];

        let store = EdgeStore::from_edges(2, edges, false);
        let (targets, types, weights) = store.neighbors_weighted(0);

        assert_eq!(targets, &[1]);
        assert_eq!(types, &[1]);
        assert!(weights.is_empty());
        assert!(!store.has_weights());
    }

    #[test]
    fn mmap_edge_arrays_validate_region_bounds_and_alignment() {
        let valid = MappedEdgeFixture::valid().parts();
        assert!(MmapEdgeArrays::new(valid.clone()).is_some());
        assert!(MmapEdgeArrays::new(MmapEdgeArrayParts {
            targets_range: 1..5,
            ..valid.clone()
        })
        .is_none());
        assert!(MmapEdgeArrays::new(MmapEdgeArrayParts {
            offsets_range: 0..4,
            ..valid.clone()
        })
        .is_none());
        assert!(MmapEdgeArrays::new(MmapEdgeArrayParts {
            type_ids_range: valid.mmap.len()..valid.mmap.len() + 1,
            ..valid
        })
        .is_none());
    }

    #[test]
    fn mmap_edge_store_owns_mapping_after_constructor_inputs_drop() {
        let store = {
            let arrays = MmapEdgeArrays::new(MappedEdgeFixture::valid().parts())
                .expect("valid mapped edge fixture");
            EdgeStore::from_mmap_with_relationship_ids(arrays, vec![NO_RELATIONSHIP_ID])
        };
        assert_eq!(store.neighbors(0), (&[1][..], &[7][..]));
        assert_eq!(store.weights_slice(), &[9]);
    }

    #[test]
    fn mmap_edge_arrays_reject_malformed_csr_values() {
        let valid = MappedEdgeFixture::valid();
        assert!(MmapEdgeArrays::new(valid.parts()).is_some());

        let bad_first = MappedEdgeFixture {
            offsets: [1, 1, 1],
            ..valid.clone()
        };
        assert!(MmapEdgeArrays::new(bad_first.parts()).is_none());

        let nonmonotonic = MappedEdgeFixture {
            offsets: [0, 1, 0],
            ..valid.clone()
        };
        assert!(MmapEdgeArrays::new(nonmonotonic.parts()).is_none());

        let bad_final = MappedEdgeFixture {
            offsets: [0, 0, 0],
            ..valid.clone()
        };
        assert!(MmapEdgeArrays::new(bad_final.parts()).is_none());

        let bad_target = MappedEdgeFixture {
            targets: [2],
            ..valid.clone()
        };
        assert!(MmapEdgeArrays::new(bad_target.parts()).is_none());

        let bad_direction = MappedEdgeFixture {
            schema_reversed: [2],
            ..valid
        };
        assert!(MmapEdgeArrays::new(bad_direction.parts()).is_none());
    }

    #[test]
    fn mmap_edge_accessors_use_validated_in_memory_layout() {
        let fixture = MappedEdgeFixture::valid();
        let arrays = MmapEdgeArrays::new(fixture.parts()).expect("valid mapped edge fixture");
        let store = EdgeStore::from_mmap_with_relationship_ids(arrays, vec![NO_RELATIONSHIP_ID]);

        assert_eq!(store.node_count(), 2);
        assert_eq!(store.edge_count(), 1);
        assert!(store.has_weights());
        assert_eq!(store.degree(0), 1);
        assert_eq!(store.degree(2), 0);
        assert_eq!(store.neighbors(0), (&[1][..], &[7][..]));
        assert_eq!(store.neighbors(2), (&[][..], &[][..]));
        assert_eq!(
            store.neighbors_with_schema(0),
            (&[1][..], &[7][..], &[0][..])
        );
        assert_eq!(store.neighbors_weighted(0), (&[1][..], &[7][..], &[9][..]));
        assert_eq!(store.offsets_slice(), &[0, 1, 1]);
        assert_eq!(store.targets_slice(), &[1]);
        assert_eq!(store.type_ids_slice(), &[7]);
        assert_eq!(store.schema_reversed_slice(), &[0]);
        assert_eq!(store.weights_slice(), &[9]);
    }

    #[test]
    fn unsorted_edges_outside_node_range_are_rejected() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 99,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 99,
                target: 0,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
        ];

        let result = EdgeStore::try_from_edges(2, edges, false);

        assert!(
            matches!(result, Err(GraphError::Internal(reason)) if reason.contains("outside node range"))
        );
    }

    #[test]
    fn sorted_edges_outside_node_range_are_rejected() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
        ];

        let result = EdgeStore::try_from_sorted_edges(2, edges, false);

        assert!(
            matches!(result, Err(GraphError::Internal(reason)) if reason.contains("edge target 2"))
        );
    }

    #[test]
    fn incremental_sorted_builder_rejects_out_of_range_endpoint() {
        let mut builder = SortedEdgeStoreBuilder::new(2, false);

        let result = builder.try_push(RawEdge {
            source: 2,
            target: 0,
            type_id: 1,
            weight: None,
            schema_reversed: false,
        });

        assert!(
            matches!(result, Err(GraphError::Internal(reason)) if reason.contains("edge source 2"))
        );
    }

    #[test]
    fn default_new_has_zero_edges() {
        let store = EdgeStore::new();
        assert_eq!(store.edge_count(), 0);
        assert!(!store.has_weights());
    }

    #[test]
    fn zero_node_graph() {
        let store = EdgeStore::from_edges(0, vec![], false);
        assert_eq!(store.node_count(), 0);
        assert_eq!(store.edge_count(), 0);
    }

    #[test]
    fn multi_type_edges_same_source() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: 2,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
        ];
        let store = EdgeStore::from_edges(3, edges, false);
        // 0→1 type1, 0→1 type2, 0→2 type1 = 3 distinct edges
        assert_eq!(store.edge_count(), 3);
        let (targets, types) = store.neighbors(0);
        assert_eq!(targets.len(), 3);
        // Sorted: target=1/type1, target=1/type2, target=2/type1
        assert_eq!(types[0], 1);
        assert_eq!(types[1], 2);
        assert_eq!(types[2], 1);
    }

    #[test]
    fn self_loop_in_csr() {
        let edges = vec![RawEdge {
            source: 0,
            target: 0,
            type_id: 1,
            weight: None,
            schema_reversed: false,
        }];
        let store = EdgeStore::from_edges(1, edges, false);
        assert_eq!(store.edge_count(), 1);
        assert_eq!(store.neighbors(0).0, &[0]);
        assert_eq!(store.degree(0), 1);
    }

    #[test]
    fn out_of_range_neighbors_return_empty_slices() {
        let store = EdgeStore::new();
        let (targets, type_ids) = store.neighbors(0);
        assert!(targets.is_empty());
        assert!(type_ids.is_empty());
    }

    #[test]
    fn out_of_range_degree_is_zero() {
        let store = EdgeStore::new();
        assert_eq!(store.degree(0), 0);
        assert_eq!(store.degree(42), 0);
    }

    proptest! {
        /// Verifies the sorted temp-table CSR pipeline is behaviorally
        /// equivalent to the in-memory unsorted builder after invalid edges are
        /// rejected, while preserving relationship-row multiplicity.
        #[test]
        fn sorted_edge_pipeline_matches_unsorted_builder(
            node_count in 0u32..32,
            raw in proptest::collection::vec((0u32..40, 0u32..40, 0u8..8, proptest::option::of(1u32..1000)), 0..256),
            has_weights in any::<bool>(),
        ) {
            let edges = raw
                .into_iter()
                .map(|(source, target, type_id, weight)| RawEdge {
                    source,
                    target,
                    type_id,
                    weight,
                schema_reversed: false,
                })
                .collect::<Vec<_>>();
            let has_invalid_edge = edges
                .iter()
                .any(|edge| edge.source >= node_count || edge.target >= node_count);
            if has_invalid_edge {
                prop_assert!(EdgeStore::try_from_edges(node_count, edges.clone(), has_weights).is_err());

                let mut sorted = edges;
                sorted.sort_unstable_by(|a, b| {
                    a.source
                        .cmp(&b.source)
                        .then(a.target.cmp(&b.target))
                        .then(a.type_id.cmp(&b.type_id))
                });
                prop_assert!(EdgeStore::try_from_sorted_edges(node_count, sorted, has_weights).is_err());
                return Ok(());
            }

            let unsorted = EdgeStore::try_from_edges(node_count, edges.clone(), has_weights)
                .expect("valid generated edges should build unsorted CSR");

            let mut sorted = edges;
            sorted.sort_unstable_by(|a, b| {
                a.source
                    .cmp(&b.source)
                    .then(a.target.cmp(&b.target))
                    .then(a.type_id.cmp(&b.type_id))
            });
            let sorted = EdgeStore::try_from_sorted_edges(node_count, sorted, has_weights)
                .expect("valid generated edges should build sorted CSR");

            prop_assert_eq!(sorted.offsets_slice(), unsorted.offsets_slice());
            prop_assert_eq!(sorted.targets_slice(), unsorted.targets_slice());
            prop_assert_eq!(sorted.type_ids_slice(), unsorted.type_ids_slice());
            prop_assert_eq!(sorted.weights_slice(), unsorted.weights_slice());
        }
    }

    #[test]
    fn incremental_sorted_builder_matches_unsorted_builder() {
        let node_count = 4;
        let has_weights = true;
        let mut edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: Some(2),
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: Some(3),
                schema_reversed: false,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: Some(99),
                schema_reversed: false,
            },
            RawEdge {
                source: 3,
                target: 0,
                type_id: 2,
                weight: Some(4),
                schema_reversed: false,
            },
        ];
        edges.sort_unstable_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then(a.target.cmp(&b.target))
                .then(a.type_id.cmp(&b.type_id))
        });

        let unsorted = EdgeStore::from_edges(node_count, edges.clone(), has_weights);
        let mut builder = SortedEdgeStoreBuilder::new(node_count, has_weights);
        for edge in edges {
            builder
                .try_push(edge)
                .expect("test fixture edges are in range");
        }
        let sorted = builder.finish();

        assert_eq!(sorted.offsets_slice(), unsorted.offsets_slice());
        assert_eq!(sorted.targets_slice(), unsorted.targets_slice());
        assert_eq!(sorted.type_ids_slice(), unsorted.type_ids_slice());
        assert_eq!(sorted.weights_slice(), unsorted.weights_slice());
    }
}
