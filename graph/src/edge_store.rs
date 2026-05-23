//! # EdgeStore — Compressed Sparse Row (CSR) edge storage
//!
//! Stores all edges in parallel flat arrays. For node `i`, its outgoing edges
//! are `targets[edge_offsets[i]..edge_offsets[i+1]]`.
//!
//! ## Modes
//!
//! - **Owned** (build time): Data in `Vec<T>`, supports construction from raw edges.
//! - **Mmap** (load time): Forward CSR arrays are read from the `.pggraph` file
//!   via mmap. Backends can share those physical pages through the OS page
//!   cache.
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

use crate::safety::{GraphError, GraphResult};

const EMPTY_U32_SLICE: [u32; 0] = [];
const EMPTY_U8_SLICE: [u8; 0] = [];

/// Validated pointer metadata for mmap-backed CSR edge arrays.
#[derive(Clone, Copy)]
pub struct MmapEdgeArrays {
    offsets_ptr: *const u32,
    targets_ptr: *const u32,
    type_ids_ptr: *const u8,
    weights_ptr: *const u32,
    node_count: u32,
    edge_count: u32,
    has_weights: bool,
}

/// Raw mmap-backed edge array pointers and lengths.
#[derive(Clone, Copy, Debug)]
pub struct MmapEdgeArrayParts {
    /// Pointer to the start of the mmap region that owns every array section.
    pub region_ptr: *const u8,
    /// Length in bytes of the mmap region.
    pub region_len: usize,
    /// Pointer to `node_count + 1` initialized CSR offsets.
    pub offsets_ptr: *const u32,
    /// Pointer to `edge_count` initialized target node indexes.
    pub targets_ptr: *const u32,
    /// Pointer to `edge_count` initialized edge type identifiers.
    pub type_ids_ptr: *const u8,
    /// Pointer to `edge_count` initialized weights when `has_weights` is true.
    pub weights_ptr: *const u32,
    /// Number of nodes represented by the CSR offset table.
    pub node_count: u32,
    /// Number of edges represented by the parallel edge arrays.
    pub edge_count: u32,
    /// Whether the weights section is present.
    pub has_weights: bool,
}

impl MmapEdgeArrays {
    /// Create validated mmap pointer metadata.
    ///
    /// # Safety
    ///
    /// The caller must ensure all pointers reference initialized sections in a
    /// mmap region that outlives every [`EdgeStore`] created from this metadata.
    pub unsafe fn new(parts: MmapEdgeArrayParts) -> Option<Self> {
        if parts.offsets_ptr.is_null()
            || parts.targets_ptr.is_null()
            || parts.type_ids_ptr.is_null()
        {
            return None;
        }
        if parts.has_weights && parts.weights_ptr.is_null() {
            return None;
        }
        let offset_bytes = (parts.node_count as usize)
            .checked_add(1)?
            .checked_mul(std::mem::size_of::<u32>())?;
        let target_bytes = (parts.edge_count as usize).checked_mul(std::mem::size_of::<u32>())?;
        let type_id_bytes = parts.edge_count as usize;
        let weight_bytes = (parts.edge_count as usize).checked_mul(std::mem::size_of::<u32>())?;
        if !ptr_range_in_region(
            parts.offsets_ptr.cast::<u8>(),
            offset_bytes,
            parts.region_ptr,
            parts.region_len,
        ) || !ptr_range_in_region(
            parts.targets_ptr.cast::<u8>(),
            target_bytes,
            parts.region_ptr,
            parts.region_len,
        ) || !ptr_range_in_region(
            parts.type_ids_ptr,
            type_id_bytes,
            parts.region_ptr,
            parts.region_len,
        ) || (parts.has_weights
            && !ptr_range_in_region(
                parts.weights_ptr.cast::<u8>(),
                weight_bytes,
                parts.region_ptr,
                parts.region_len,
            ))
        {
            return None;
        }
        if !(parts.offsets_ptr as usize).is_multiple_of(std::mem::align_of::<u32>())
            || !(parts.targets_ptr as usize).is_multiple_of(std::mem::align_of::<u32>())
            || (parts.has_weights
                && !(parts.weights_ptr as usize).is_multiple_of(std::mem::align_of::<u32>()))
        {
            return None;
        }

        Some(Self {
            offsets_ptr: parts.offsets_ptr,
            targets_ptr: parts.targets_ptr,
            type_ids_ptr: parts.type_ids_ptr,
            weights_ptr: parts.weights_ptr,
            node_count: parts.node_count,
            edge_count: parts.edge_count,
            has_weights: parts.has_weights,
        })
    }
}

fn ptr_range_in_region(
    ptr: *const u8,
    byte_len: usize,
    region_ptr: *const u8,
    region_len: usize,
) -> bool {
    if ptr.is_null() || region_ptr.is_null() {
        return false;
    }
    let start = ptr as usize;
    let region_start = region_ptr as usize;
    let Some(end) = start.checked_add(byte_len) else {
        return false;
    };
    let Some(region_end) = region_start.checked_add(region_len) else {
        return false;
    };
    start >= region_start && end <= region_end
}

/// Backing store for edge data.
pub enum EdgeBacking {
    /// Build-time: owned Vecs.
    Owned {
        edge_offsets: Vec<u32>,
        targets: Vec<u32>,
        type_ids: Vec<u8>,
        weights: Vec<u32>,
    },
    /// Load-time: read-only pointers into Engine-owned mmap memory.
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
}

/// Incremental CSR builder for raw edges already sorted by
/// `(source, target, type_id)`.
///
/// This lets the graph builder stream sorted rows from PostgreSQL temp storage
/// without retaining a full `Vec<RawEdge>` in Rust.
pub struct SortedEdgeStoreBuilder {
    node_count: u32,
    has_weights: bool,
    edge_offsets: Vec<u32>,
    targets: Vec<u32>,
    type_ids: Vec<u8>,
    weights: Vec<u32>,
    current_node: u32,
    previous: Option<(u32, u32, u8)>,
}

impl SortedEdgeStoreBuilder {
    /// Create a streaming CSR builder for edges sorted by source node.
    pub fn new(node_count: u32, has_weights: bool) -> Self {
        Self {
            node_count,
            has_weights,
            edge_offsets: vec![0],
            targets: Vec::new(),
            type_ids: Vec::new(),
            weights: Vec::new(),
            current_node: 0,
            previous: None,
        }
    }

    /// Add one sorted edge, rejecting endpoints outside the node range.
    ///
    /// Consecutive duplicate `(source, target, type_id)` rows are ignored so
    /// SQL build spools can deduplicate without retaining all edges in memory.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Internal`] when `edge.source` or `edge.target`
    /// is greater than or equal to the builder's node count.
    pub fn try_push(&mut self, edge: RawEdge) -> GraphResult<()> {
        validate_raw_edge(self.node_count, &edge)?;
        let key = (edge.source, edge.target, edge.type_id);
        if self.previous == Some(key) {
            return Ok(());
        }
        while self.current_node < edge.source {
            self.current_node += 1;
            self.edge_offsets.push(self.targets.len() as u32);
        }
        self.targets.push(edge.target);
        self.type_ids.push(edge.type_id);
        if self.has_weights {
            self.weights.push(edge.weight.unwrap_or(1));
        }
        self.previous = Some(key);
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
                weights: self.weights,
            },
        }
    }
}

/// Compressed Sparse Row (CSR) edge storage.
pub struct EdgeStore {
    backing: EdgeBacking,
}

impl EdgeStore {
    /// Create an empty EdgeStore.
    pub fn new() -> Self {
        Self {
            backing: EdgeBacking::Owned {
                edge_offsets: vec![0],
                targets: Vec::new(),
                type_ids: Vec::new(),
                weights: Vec::new(),
            },
        }
    }

    /// Create an mmap-backed EdgeStore from raw pointers.
    ///
    /// # Safety
    ///
    /// The caller must ensure all pointers point into a valid mmap'd region
    /// that outlives this EdgeStore. `offsets_ptr` must contain
    /// `node_count + 1` initialized `u32` values, `targets_ptr` must contain
    /// `edge_count` initialized `u32` values, `type_ids_ptr` must contain
    /// `edge_count` initialized bytes, and `weights_ptr` must contain
    /// `edge_count` initialized `u32` values when `has_weights` is true.
    pub unsafe fn from_mmap(arrays: MmapEdgeArrays) -> Self {
        Self {
            backing: EdgeBacking::Mmap { arrays },
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
        // Sort by source, then target, then type_id
        edges.sort_unstable_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then(a.target.cmp(&b.target))
                .then(a.type_id.cmp(&b.type_id))
        });

        // Deduplicate
        edges.dedup_by(|a, b| {
            a.source == b.source && a.target == b.target && a.type_id == b.type_id
        });

        let edge_count = edges.len();

        // Build CSR arrays
        let mut edge_offsets = Vec::with_capacity(node_count as usize + 1);
        let mut targets = Vec::with_capacity(edge_count);
        let mut type_ids = Vec::with_capacity(edge_count);
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
                weights,
            },
        }
    }

    /// Build a CSR EdgeStore from already sorted raw edges without retaining the
    /// raw edge list. Input must be ordered by `(source, target, type_id)`.
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
        let mut builder = SortedEdgeStoreBuilder::new(node_count, has_weights);
        for edge in edges {
            builder.try_push(edge)?;
        }
        Ok(builder.finish())
    }

    /// Build a reverse CSR from this store's directed edge contents.
    ///
    /// The returned store owns its arrays. This keeps inbound traversal fast
    /// even when the forward graph was loaded from an mmap-backed file.
    pub fn reversed(&self) -> Self {
        let has_weights = self.has_weights();
        let mut edges = Vec::with_capacity(self.edge_count() as usize);
        for source in 0..self.node_count() {
            let (targets, type_ids, weights) = self.neighbors_weighted(source);
            for (idx, (&target, &type_id)) in targets.iter().zip(type_ids.iter()).enumerate() {
                edges.push(RawEdge {
                    source: target,
                    target: source,
                    type_id,
                    weight: has_weights.then(|| weights.get(idx).copied().unwrap_or(1)),
                });
            }
        }
        Self::from_valid_edges(self.node_count(), edges, has_weights)
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
                if node_idx >= arrays.node_count {
                    return (&EMPTY_U32_SLICE, &EMPTY_U8_SLICE);
                }
                // SAFETY: MmapEdgeArrays::new validates offsets_ptr points to
                // node_count + 1 initialized offsets.
                let start = unsafe { *arrays.offsets_ptr.add(node_idx as usize) as usize };
                // SAFETY: The node_idx guard keeps node_idx + 1 in the offset table.
                let end = unsafe { *arrays.offsets_ptr.add(node_idx as usize + 1) as usize };
                if start > end || end > arrays.edge_count as usize {
                    return (&EMPTY_U32_SLICE, &EMPTY_U8_SLICE);
                }
                let len = end - start;
                // SAFETY: start/end were checked against edge_count above, and
                // targets_ptr/type_ids_ptr point to edge_count initialized values.
                unsafe {
                    (
                        std::slice::from_raw_parts(arrays.targets_ptr.add(start), len),
                        std::slice::from_raw_parts(arrays.type_ids_ptr.add(start), len),
                    )
                }
            }
        }
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
                let (t, ti) = self.neighbors(node_idx);
                if !arrays.has_weights {
                    return (t, ti, &[]);
                }
                let start = self.offsets_slice()[node_idx as usize] as usize;
                let len = t.len();
                // SAFETY: neighbors() already validated start + len against
                // edge_count, and weighted metadata validates weights_ptr.
                let weights =
                    unsafe { std::slice::from_raw_parts(arrays.weights_ptr.add(start), len) };
                (t, ti, weights)
            }
        }
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
            EdgeBacking::Mmap { arrays } => arrays.has_weights,
        }
    }

    /// Estimate heap bytes owned directly by this store.
    ///
    /// Mmap-backed arrays are accounted as shared file mappings, not Rust heap.
    pub fn estimated_heap_bytes(&self) -> usize {
        match &self.backing {
            EdgeBacking::Owned {
                edge_offsets,
                targets,
                type_ids,
                weights,
            } => {
                edge_offsets.capacity() * std::mem::size_of::<u32>()
                    + targets.capacity() * std::mem::size_of::<u32>()
                    + type_ids.capacity() * std::mem::size_of::<u8>()
                    + weights.capacity() * std::mem::size_of::<u32>()
            }
            EdgeBacking::Mmap { .. } => 0,
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
                // SAFETY: MmapEdgeArrays::new validates offsets_ptr points to
                // node_count + 1 initialized offsets.
                let start = unsafe { *arrays.offsets_ptr.add(node_idx as usize) };
                // SAFETY: The node_idx guard keeps node_idx + 1 in the offset table.
                let end = unsafe { *arrays.offsets_ptr.add(node_idx as usize + 1) };
                end.saturating_sub(start)
            }
        }
    }

    // ── Persistence helpers ──

    /// Get edge_offsets as a slice. Used by persistence.
    pub fn offsets_slice(&self) -> &[u32] {
        match &self.backing {
            EdgeBacking::Owned { edge_offsets, .. } => edge_offsets,
            EdgeBacking::Mmap { arrays } => {
                // SAFETY: MmapEdgeArrays::new validates offsets_ptr and node_count.
                unsafe {
                    std::slice::from_raw_parts(arrays.offsets_ptr, arrays.node_count as usize + 1)
                }
            }
        }
    }

    /// Get targets as a slice. Used by persistence.
    pub fn targets_slice(&self) -> &[u32] {
        match &self.backing {
            EdgeBacking::Owned { targets, .. } => targets,
            EdgeBacking::Mmap { arrays } => {
                // SAFETY: MmapEdgeArrays::new validates targets_ptr and edge_count.
                unsafe {
                    std::slice::from_raw_parts(arrays.targets_ptr, arrays.edge_count as usize)
                }
            }
        }
    }

    /// Get type_ids as a slice. Used by persistence.
    pub fn type_ids_slice(&self) -> &[u8] {
        match &self.backing {
            EdgeBacking::Owned { type_ids, .. } => type_ids,
            EdgeBacking::Mmap { arrays } => {
                // SAFETY: MmapEdgeArrays::new validates type_ids_ptr and edge_count.
                unsafe {
                    std::slice::from_raw_parts(arrays.type_ids_ptr, arrays.edge_count as usize)
                }
            }
        }
    }

    /// Get weights as a slice. Empty means unweighted.
    pub fn weights_slice(&self) -> &[u32] {
        match &self.backing {
            EdgeBacking::Owned { weights, .. } => weights,
            EdgeBacking::Mmap { arrays } => {
                if !arrays.has_weights {
                    &[]
                } else {
                    // SAFETY: MmapEdgeArrays::new validates weights_ptr and edge_count.
                    unsafe {
                        std::slice::from_raw_parts(arrays.weights_ptr, arrays.edge_count as usize)
                    }
                }
            }
        }
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
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
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
            },
            RawEdge {
                source: 2,
                target: 1,
                type_id: 9,
                weight: Some(5),
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
    fn deduplicates_edges() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
            }, // duplicate
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: None,
            },
        ];
        let store = EdgeStore::from_edges(3, edges, false);
        assert_eq!(store.edge_count(), 2); // deduped
        assert_eq!(store.neighbors(0).0, &[1, 2]);
    }

    #[test]
    fn sorts_edges_by_source() {
        let edges = vec![
            RawEdge {
                source: 2,
                target: 0,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
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
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: Some(20),
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
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: Some(8),
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
        let region = [0u64; 8];
        let base = region.as_ptr().cast::<u8>();
        let valid = MmapEdgeArrayParts {
            region_ptr: base,
            region_len: std::mem::size_of_val(&region),
            offsets_ptr: base.cast::<u32>(),
            // SAFETY: Offsets are within the local byte region used only for
            // constructor validation in this test.
            targets_ptr: unsafe { base.add(16).cast::<u32>() },
            // SAFETY: See above.
            type_ids_ptr: unsafe { base.add(24) },
            // SAFETY: See above.
            weights_ptr: unsafe { base.add(32).cast::<u32>() },
            node_count: 1,
            edge_count: 1,
            has_weights: true,
        };

        // SAFETY: Pointers are into the local region and are not dereferenced.
        assert!(unsafe { MmapEdgeArrays::new(valid) }.is_some());
        // SAFETY: Pointers are into the local region and are not dereferenced.
        assert!(unsafe {
            MmapEdgeArrays::new(MmapEdgeArrayParts {
                region_len: 24,
                ..valid
            })
        }
        .is_none());
        // SAFETY: Pointers are into the local region and are not dereferenced.
        assert!(unsafe {
            MmapEdgeArrays::new(MmapEdgeArrayParts {
                targets_ptr: base.wrapping_add(1).cast::<u32>(),
                ..valid
            })
        }
        .is_none());
        // SAFETY: Pointers are into the local region and are not dereferenced.
        assert!(unsafe {
            MmapEdgeArrays::new(MmapEdgeArrayParts {
                offsets_ptr: std::ptr::null(),
                ..valid
            })
        }
        .is_none());
    }

    #[test]
    fn mmap_edge_arrays_reject_pointer_overflow() {
        let region = [0u64; 1];
        let base = region.as_ptr().cast::<u8>();
        let near_usize_end = usize::MAX as *const u32;

        // SAFETY: The constructor validates the deliberately overflowing
        // pointer metadata and returns `None` before dereferencing.
        assert!(unsafe {
            MmapEdgeArrays::new(MmapEdgeArrayParts {
                region_ptr: base,
                region_len: std::mem::size_of_val(&region),
                offsets_ptr: near_usize_end,
                targets_ptr: base.cast::<u32>(),
                type_ids_ptr: base,
                weights_ptr: base.cast::<u32>(),
                node_count: 1,
                edge_count: 0,
                has_weights: false,
            })
        }
        .is_none());
    }

    #[test]
    fn unsorted_edges_outside_node_range_are_rejected() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 99,
                type_id: 1,
                weight: None,
            },
            RawEdge {
                source: 99,
                target: 0,
                type_id: 1,
                weight: None,
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
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: None,
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
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: 2,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 1,
                weight: None,
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
        /// rejected and duplicate `(source, target, type_id)` entries collapse.
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
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: Some(3),
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: 1,
                weight: Some(99),
            },
            RawEdge {
                source: 3,
                target: 0,
                type_id: 2,
                weight: Some(4),
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
