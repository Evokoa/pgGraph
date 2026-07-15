//! # NodeStore — Struct-of-Arrays node storage
//!
//! Stores node metadata as parallel flat arrays (SoA layout) for maximum
//! cache efficiency during BFS traversal. Each node is identified by a
//! `node_idx: u32` that serves as the index into all arrays.
//!
//! ## Modes
//!
//! - **Owned** (build time): Data in `Vec<T>`, supports mutation (add/deactivate).
//! - **Mmap** (load time): Active bits, table OIDs, primary-key offsets, and
//!   primary-key bytes are read from a backend-local immutable artifact
//!   snapshot. The store is materialized into owned arrays on the first sync
//!   mutation.
//!
//! ## Arrays
//!
//! | Array | Type | Purpose |
//! |---|---|---|
//! | `is_active` | `[u8]` (packed bits) | Tombstone flag (false = deleted) |
//! | `table_oids` | `[u32]` | Source table OID per node |
//! | `primary_keys` | `Vec<String>` | Source PK value per node in owned mode |
//! | `primary_key_offsets` | `[u64]` | mmap offset table into `primary_key_bytes` |
//! | `primary_key_bytes` | `[u8]` | mmap UTF-8 primary-key byte section |
//!
//! See: `docs/contributor_guide/memory-model.mdx`

use crate::mapped_bytes::MappedBytes;
use crate::safety::{GraphError, GraphResult};
use bitvec::prelude::*;
use std::ops::Range;

/// Validated, owning metadata for mmap-backed node arrays.
#[derive(Clone, Debug)]
pub(crate) struct MmapNodeArrays {
    mmap: MappedBytes,
    active_range: Range<usize>,
    oid_range: Range<usize>,
    pk_offsets_range: Range<usize>,
    pk_bytes_range: Range<usize>,
    node_count: u32,
}

/// Byte ranges for mmap-backed node arrays.
///
/// Values are validated by [`MmapNodeArrays::new_for_artifact`] before they are
/// used by a [`NodeStore`]. The struct groups same-typed ranges so construction
/// cannot accidentally reorder sections.
#[derive(Clone, Debug)]
pub(crate) struct MmapNodeArrayParts {
    /// Read-only mapping retained by every store built from these ranges.
    pub(crate) mmap: MappedBytes,
    /// Packed active-bit bytes.
    pub(crate) active_range: Range<usize>,
    /// `node_count` source table OIDs.
    pub(crate) oid_range: Range<usize>,
    /// `node_count + 1` primary-key byte offsets.
    pub(crate) pk_offsets_range: Range<usize>,
    /// UTF-8 primary-key bytes.
    pub(crate) pk_bytes_range: Range<usize>,
    /// Number of nodes represented by every per-node array.
    pub(crate) node_count: u32,
}

impl MmapNodeArrays {
    /// Create validated, owning mmap metadata.
    ///
    /// Validation covers byte ranges, typed alignment, the complete
    /// primary-key offset table, terminal offsets, and UTF-8 boundaries.
    pub(crate) fn new_for_artifact(
        parts: MmapNodeArrayParts,
        _validated_artifact: &crate::persistence::ValidatedMappedGraphToken,
    ) -> Option<Self> {
        Self::validate(parts)
    }

    #[cfg(test)]
    pub(crate) fn new(parts: MmapNodeArrayParts) -> Option<Self> {
        Self::validate(parts)
    }

    fn validate(parts: MmapNodeArrayParts) -> Option<Self> {
        if !cfg!(target_endian = "little") {
            return None;
        }
        let active_byte_count = (parts.node_count as usize).div_ceil(8);
        if parts.active_range.len() != active_byte_count {
            return None;
        }
        let oid_bytes = (parts.node_count as usize).checked_mul(std::mem::size_of::<u32>())?;
        let pk_offsets_bytes = (parts.node_count as usize)
            .checked_add(1)?
            .checked_mul(std::mem::size_of::<u64>())?;
        if parts.oid_range.len() != oid_bytes
            || parts.pk_offsets_range.len() != pk_offsets_bytes
            || !range_in_region(&parts.active_range, parts.mmap.len())
            || !range_in_region(&parts.oid_range, parts.mmap.len())
            || !range_in_region(&parts.pk_offsets_range, parts.mmap.len())
            || !range_in_region(&parts.pk_bytes_range, parts.mmap.len())
        {
            return None;
        }
        let base = parts.mmap.as_slice().as_ptr() as usize;
        if !base
            .checked_add(parts.oid_range.start)?
            .is_multiple_of(std::mem::align_of::<u32>())
            || !base
                .checked_add(parts.pk_offsets_range.start)?
                .is_multiple_of(std::mem::align_of::<u64>())
        {
            return None;
        }

        let pk_offsets = u64_slice(&parts.mmap, &parts.pk_offsets_range);
        let pk_bytes = &parts.mmap.as_slice()[parts.pk_bytes_range.clone()];
        if pk_offsets.first().copied() != Some(0)
            || pk_offsets.last().copied() != Some(pk_bytes.len() as u64)
        {
            return None;
        }
        for offsets in pk_offsets.windows(2) {
            let Ok(start) = usize::try_from(offsets[0]) else {
                return None;
            };
            let Ok(end) = usize::try_from(offsets[1]) else {
                return None;
            };
            if start > end
                || end > pk_bytes.len()
                || std::str::from_utf8(&pk_bytes[start..end]).is_err()
            {
                return None;
            }
        }

        Some(Self {
            mmap: parts.mmap,
            active_range: parts.active_range,
            oid_range: parts.oid_range,
            pk_offsets_range: parts.pk_offsets_range,
            pk_bytes_range: parts.pk_bytes_range,
            node_count: parts.node_count,
        })
    }

    fn active_bytes(&self) -> &[u8] {
        &self.mmap.as_slice()[self.active_range.clone()]
    }

    fn table_oids(&self) -> &[u32] {
        u32_slice(&self.mmap, &self.oid_range)
    }

    fn primary_key_offsets(&self) -> &[u64] {
        u64_slice(&self.mmap, &self.pk_offsets_range)
    }

    fn primary_key_bytes(&self) -> &[u8] {
        &self.mmap.as_slice()[self.pk_bytes_range.clone()]
    }
}

fn range_in_region(range: &Range<usize>, region_len: usize) -> bool {
    range.start <= range.end && range.end <= region_len
}

fn u32_slice<'a>(mmap: &'a MappedBytes, range: &Range<usize>) -> &'a [u32] {
    // SAFETY: MmapNodeArrays validation proves that this range is in the mapping,
    // aligned for u32, and has a byte length divisible by four. The mapping is
    // read-only and retained by Arc for the returned slice's lifetime.
    unsafe {
        std::slice::from_raw_parts(
            mmap.as_slice()[range.clone()].as_ptr().cast::<u32>(),
            range.len() / std::mem::size_of::<u32>(),
        )
    }
}

fn u64_slice<'a>(mmap: &'a MappedBytes, range: &Range<usize>) -> &'a [u64] {
    // SAFETY: MmapNodeArrays validation proves that this range is in the mapping,
    // aligned for u64, and has a byte length divisible by eight. The mapping is
    // read-only and retained by Arc for the returned slice's lifetime.
    unsafe {
        std::slice::from_raw_parts(
            mmap.as_slice()[range.clone()].as_ptr().cast::<u64>(),
            range.len() / std::mem::size_of::<u64>(),
        )
    }
}

/// Backing store for array data: either owned Vecs or mmap-backed sections.
enum ArrayBacking {
    /// Build-time: owned Vecs, mutable.
    Owned {
        is_active: BitVec,
        table_oids: Vec<u32>,
        primary_keys: Vec<String>,
    },
    /// Load-time: validated ranges into an Arc-owned read-only mapping.
    Mmap { arrays: MmapNodeArrays },
}

/// Struct-of-Arrays node storage.
///
/// Hot path active bits are read on every BFS iteration. Table OIDs may also be
/// read during tenant-scoped traversal; primary keys are result-construction
/// metadata.
pub struct NodeStore {
    backing: ArrayBacking,
}

impl NodeStore {
    /// Create an empty NodeStore (owned mode).
    pub fn new() -> Self {
        Self {
            backing: ArrayBacking::Owned {
                is_active: BitVec::new(),
                table_oids: Vec::new(),
                primary_keys: Vec::new(),
            },
        }
    }

    /// Create a NodeStore pre-allocated for `capacity` nodes.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            backing: ArrayBacking::Owned {
                is_active: BitVec::with_capacity(capacity),
                table_oids: Vec::with_capacity(capacity),
                primary_keys: Vec::with_capacity(capacity),
            },
        }
    }

    /// Create an mmap-backed NodeStore from validated owning metadata.
    pub(crate) fn from_mmap(arrays: MmapNodeArrays) -> Self {
        Self {
            backing: ArrayBacking::Mmap { arrays },
        }
    }

    /// Add a node and return its index. Only valid in Owned mode.
    ///
    /// # Panics
    /// Panics if called on an mmap-backed NodeStore.
    #[allow(
        clippy::panic,
        reason = "mmap stores are immutable views; callers must materialize before mutation"
    )]
    pub fn add_node(&mut self, table_oid: u32, primary_key: String) -> u32 {
        match &mut self.backing {
            ArrayBacking::Owned {
                is_active,
                table_oids,
                primary_keys,
            } => {
                let idx = table_oids.len() as u32;
                is_active.push(true);
                table_oids.push(table_oid);
                primary_keys.push(primary_key);
                idx
            }
            ArrayBacking::Mmap { .. } => {
                panic!("Cannot add nodes to an mmap-backed NodeStore");
            }
        }
    }

    /// Fallibly add a node to owned storage for resource-governed builds.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::Oom` when an owned array cannot grow, or an
    /// internal error for index overflow or mmap-backed storage.
    pub(crate) fn try_add_node(&mut self, table_oid: u32, primary_key: String) -> GraphResult<u32> {
        match &mut self.backing {
            ArrayBacking::Owned {
                is_active,
                table_oids,
                primary_keys,
            } => {
                table_oids.try_reserve(1).map_err(node_allocation_error)?;
                primary_keys.try_reserve(1).map_err(node_allocation_error)?;
                let idx = u32::try_from(table_oids.len()).map_err(|_| {
                    GraphError::Internal("node count exceeds u32 index space".to_string())
                })?;
                is_active.push(true);
                table_oids.push(table_oid);
                primary_keys.push(primary_key);
                Ok(idx)
            }
            ArrayBacking::Mmap { .. } => Err(GraphError::Internal(
                "cannot add nodes to mmap-backed storage during build".to_string(),
            )),
        }
    }

    /// Mark a node as deleted (tombstone). Only valid in Owned mode.
    pub fn deactivate(&mut self, node_idx: u32) {
        match &mut self.backing {
            ArrayBacking::Owned { is_active, .. } => {
                if let Some(mut bit) = is_active.get_mut(node_idx as usize) {
                    *bit = false;
                }
            }
            ArrayBacking::Mmap { .. } => {
                // Cannot mutate mmap — would need rebuild
            }
        }
    }

    /// Check if a node is active (not tombstoned). HOT PATH.
    #[inline(always)]
    pub fn is_active(&self, node_idx: u32) -> bool {
        match &self.backing {
            ArrayBacking::Owned { is_active, .. } => is_active
                .get(node_idx as usize)
                .map(|b| *b)
                .unwrap_or(false),
            ArrayBacking::Mmap { arrays } => {
                if node_idx >= arrays.node_count {
                    return false;
                }
                let byte_idx = node_idx as usize / 8;
                let bit_idx = node_idx as usize % 8;
                let byte = arrays.active_bytes()[byte_idx];
                (byte >> bit_idx) & 1 == 1
            }
        }
    }

    /// Number of nodes (including tombstoned).
    pub fn node_count(&self) -> u32 {
        match &self.backing {
            ArrayBacking::Owned { table_oids, .. } => table_oids.len() as u32,
            ArrayBacking::Mmap { arrays } => arrays.node_count,
        }
    }

    /// Number of active (non-tombstoned) nodes.
    pub fn active_count(&self) -> u32 {
        match &self.backing {
            ArrayBacking::Owned { is_active, .. } => is_active.count_ones() as u32,
            ArrayBacking::Mmap { arrays } => {
                let bytes = arrays.active_bytes();
                let mut count = 0u32;
                for &byte in bytes {
                    count += byte.count_ones();
                }
                // Don't count bits beyond node_count
                let total_bits = bytes.len() * 8;
                let excess = total_bits as u32 - arrays.node_count;
                if excess > 0 {
                    let last_byte = bytes[bytes.len() - 1];
                    for bit in (8 - excess as usize)..8 {
                        if (last_byte >> bit) & 1 == 1 {
                            count -= 1;
                        }
                    }
                }
                count
            }
        }
    }

    /// Return the source table OID for a node.
    ///
    /// Returns `None` when `node_idx` is outside this store.
    pub fn table_oid(&self, node_idx: u32) -> Option<u32> {
        match &self.backing {
            ArrayBacking::Owned { table_oids, .. } => table_oids.get(node_idx as usize).copied(),
            ArrayBacking::Mmap { arrays } => {
                if node_idx >= arrays.node_count {
                    return None;
                }
                arrays.table_oids().get(node_idx as usize).copied()
            }
        }
    }

    /// Get the primary key for a node. Cold path.
    ///
    /// In mmap-backed mode this reads UTF-8 bytes through the persisted
    /// primary-key offset and byte sections. Returns `None` when `node_idx` is
    /// outside this store.
    pub fn primary_key(&self, node_idx: u32) -> Option<&str> {
        match &self.backing {
            ArrayBacking::Owned { primary_keys, .. } => {
                primary_keys.get(node_idx as usize).map(String::as_str)
            }
            ArrayBacking::Mmap { arrays } => {
                if node_idx >= arrays.node_count {
                    return None;
                }

                let offsets = arrays.primary_key_offsets();
                let start = usize::try_from(offsets[node_idx as usize]).ok()?;
                let end = usize::try_from(offsets[node_idx as usize + 1]).ok()?;
                let bytes = arrays.primary_key_bytes();
                if start > end || end > bytes.len() {
                    return None;
                }
                std::str::from_utf8(&bytes[start..end]).ok()
            }
        }
    }

    /// True when this store is backed by mmap'd read-only memory.
    pub fn is_mmap_backed(&self) -> bool {
        matches!(self.backing, ArrayBacking::Mmap { .. })
    }

    /// Estimate heap bytes owned directly by this store.
    ///
    /// Mapped arrays are accounted once through the engine's private snapshot.
    pub fn estimated_heap_bytes(&self) -> usize {
        match &self.backing {
            ArrayBacking::Owned {
                is_active,
                table_oids,
                primary_keys,
            } => {
                is_active.capacity().div_ceil(8)
                    + table_oids.capacity() * std::mem::size_of::<u32>()
                    + primary_keys.capacity() * std::mem::size_of::<String>()
                    + primary_keys.iter().map(String::capacity).sum::<usize>()
            }
            ArrayBacking::Mmap { .. } => 0,
        }
    }

    /// Estimate bytes borrowed from an mmap-backed graph artifact.
    pub fn estimated_mmap_bytes(&self) -> usize {
        match &self.backing {
            ArrayBacking::Owned { .. } => 0,
            ArrayBacking::Mmap { arrays } => arrays
                .active_range
                .len()
                .saturating_add(arrays.oid_range.len())
                .saturating_add(arrays.pk_offsets_range.len())
                .saturating_add(arrays.pk_bytes_range.len()),
        }
    }

    /// Return an owned, mutable copy of this store.
    ///
    /// Sync overlays use this to accept node tombstones and inserts after a
    /// persisted graph has been auto-loaded from mmap.
    pub fn to_owned_store(&self) -> Self {
        match &self.backing {
            ArrayBacking::Owned {
                is_active,
                table_oids,
                primary_keys,
            } => Self {
                backing: ArrayBacking::Owned {
                    is_active: is_active.clone(),
                    table_oids: table_oids.clone(),
                    primary_keys: primary_keys.clone(),
                },
            },
            ArrayBacking::Mmap { arrays } => {
                let mut is_active = BitVec::with_capacity(arrays.node_count as usize);
                for node_idx in 0..arrays.node_count {
                    is_active.push(self.is_active(node_idx));
                }
                let table_oids = arrays.table_oids().to_vec();
                let offsets = arrays.primary_key_offsets();
                let bytes = arrays.primary_key_bytes();
                let primary_keys = offsets
                    .windows(2)
                    .map(|window| {
                        let range = usize::try_from(window[0])
                            .ok()
                            .zip(usize::try_from(window[1]).ok())
                            .and_then(|(start, end)| bytes.get(start..end));
                        range
                            .and_then(|value| std::str::from_utf8(value).ok())
                            .map(str::to_owned)
                            .unwrap_or_default()
                    })
                    .collect();
                Self {
                    backing: ArrayBacking::Owned {
                        is_active,
                        table_oids,
                        primary_keys,
                    },
                }
            }
        }
    }

    // ── Persistence helpers (for write_graph_file) ──

    /// Get is_active raw bytes. Used by persistence.
    pub fn is_active_bytes(&self) -> Vec<u8> {
        match &self.backing {
            ArrayBacking::Owned { is_active, .. } => {
                let raw = is_active.as_raw_slice();
                let mut bytes = Vec::new();
                for word in raw {
                    bytes.extend_from_slice(&word.to_le_bytes());
                }
                bytes
            }
            ArrayBacking::Mmap { arrays } => arrays.active_bytes().to_vec(),
        }
    }

    /// Get table_oids as a slice. Used by persistence.
    pub fn table_oids_slice(&self) -> &[u32] {
        match &self.backing {
            ArrayBacking::Owned { table_oids, .. } => table_oids,
            ArrayBacking::Mmap { arrays } => arrays.table_oids(),
        }
    }
}

impl Default for NodeStore {
    fn default() -> Self {
        Self::new()
    }
}

fn node_allocation_error(_error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
    }
}

#[cfg(test)]
mod tests {
    //! Covers node lifecycle, tombstone handling, storage statistics, and mmap
    //! loading invariants for persisted node data.

    use super::*;

    fn align_up(value: usize, alignment: usize) -> usize {
        value.next_multiple_of(alignment)
    }

    fn mapped_node_parts(
        active: &[u8],
        oids: &[u32],
        offsets: &[u64],
        primary_key_bytes: &[u8],
    ) -> MmapNodeArrayParts {
        let active_range = 0..active.len();
        let oid_start = align_up(active_range.end, std::mem::align_of::<u32>());
        let oid_range = oid_start..oid_start + std::mem::size_of_val(oids);
        let offsets_start = align_up(oid_range.end, std::mem::align_of::<u64>());
        let pk_offsets_range = offsets_start..offsets_start + std::mem::size_of_val(offsets);
        let pk_bytes_range = pk_offsets_range.end..pk_offsets_range.end + primary_key_bytes.len();
        let mut mapping = vec![0u8; pk_bytes_range.end.max(1)];
        mapping[active_range.clone()].copy_from_slice(active);
        for (chunk, value) in mapping[oid_range.clone()].chunks_exact_mut(4).zip(oids) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        for (chunk, value) in mapping[pk_offsets_range.clone()]
            .chunks_exact_mut(8)
            .zip(offsets)
        {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        mapping[pk_bytes_range.clone()].copy_from_slice(primary_key_bytes);
        MmapNodeArrayParts {
            mmap: MappedBytes::from_test_bytes(mapping),
            active_range,
            oid_range,
            pk_offsets_range,
            pk_bytes_range,
            node_count: oids.len() as u32,
        }
    }

    #[test]
    fn add_and_retrieve_node() {
        let mut store = NodeStore::new();
        let idx = store.add_node(12345, "PK-001".to_string());
        assert_eq!(idx, 0);
        assert_eq!(store.node_count(), 1);
        assert!(store.is_active(0));
        assert_eq!(store.table_oid(0), Some(12345));
        assert_eq!(store.primary_key(0), Some("PK-001"));
    }

    #[test]
    fn deactivate_tombstones_node() {
        let mut store = NodeStore::new();
        store.add_node(1, "A".to_string());
        assert!(store.is_active(0));
        store.deactivate(0);
        assert!(!store.is_active(0));
    }

    #[test]
    fn multiple_nodes_sequential_indices() {
        let mut store = NodeStore::new();
        let a = store.add_node(1, "A".to_string());
        let b = store.add_node(2, "B".to_string());
        let c = store.add_node(3, "C".to_string());
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(c, 2);
        assert_eq!(store.node_count(), 3);
    }

    #[test]
    fn active_count_excludes_tombstones() {
        let mut store = NodeStore::new();
        store.add_node(1, "A".to_string());
        store.add_node(1, "B".to_string());
        store.add_node(1, "C".to_string());
        assert_eq!(store.active_count(), 3);

        store.deactivate(1); // Tombstone B
        assert_eq!(store.node_count(), 3); // Still 3 slots
        assert_eq!(store.active_count(), 2); // Only 2 active
    }

    #[test]
    fn empty_store_has_zero_counts() {
        let store = NodeStore::new();
        assert_eq!(store.node_count(), 0);
        assert_eq!(store.active_count(), 0);
    }

    #[test]
    fn nodes_from_different_tables_coexist() {
        let mut store = NodeStore::new();
        store.add_node(100, "user-1".to_string());
        store.add_node(200, "order-1".to_string());
        store.add_node(300, "product-1".to_string());

        assert_eq!(store.table_oid(0), Some(100));
        assert_eq!(store.table_oid(1), Some(200));
        assert_eq!(store.table_oid(2), Some(300));
    }

    #[test]
    fn primary_keys_support_special_characters() {
        let mut store = NodeStore::new();
        store.add_node(1, "pk with spaces".to_string());
        store.add_node(1, "pk-with-dashes".to_string());
        store.add_node(1, "pk_with_🦀_emoji".to_string());
        store.add_node(1, "".to_string()); // Empty PK

        assert_eq!(store.primary_key(0), Some("pk with spaces"));
        assert_eq!(store.primary_key(1), Some("pk-with-dashes"));
        assert_eq!(store.primary_key(2), Some("pk_with_🦀_emoji"));
        assert_eq!(store.primary_key(3), Some(""));
    }

    #[test]
    fn deactivate_already_inactive_is_safe() {
        let mut store = NodeStore::new();
        store.add_node(1, "X".to_string());
        store.deactivate(0);
        assert!(!store.is_active(0));

        // Deactivate again — should not panic
        store.deactivate(0);
        assert!(!store.is_active(0));
    }

    #[test]
    fn out_of_range_active_checks_are_safe() {
        let mut store = NodeStore::new();
        store.add_node(1, "X".to_string());

        assert!(!store.is_active(1));
        assert!(!store.is_active(u32::MAX));
    }

    #[test]
    fn mmap_metadata_lookups_reject_out_of_range_nodes() {
        let active = [1u8];
        let oids = [42u32];
        let pk_offsets = [0u64, 1u64];
        let pk_bytes = *b"X";
        let arrays = MmapNodeArrays::new(mapped_node_parts(&active, &oids, &pk_offsets, &pk_bytes))
            .expect("valid mapped node fixture");
        let store = NodeStore::from_mmap(arrays);

        assert_eq!(store.table_oid(0), Some(42));
        assert_eq!(store.primary_key(0), Some("X"));
        assert_eq!(store.table_oid(1), None);
        assert_eq!(store.primary_key(1), None);
        assert_eq!(store.table_oid(u32::MAX), None);
        assert_eq!(store.primary_key(u32::MAX), None);
        assert!(store.is_active(0));
        assert!(!store.is_active(1));
        assert_eq!(store.active_count(), 1);
        assert_eq!(store.is_active_bytes(), vec![1]);
        assert_eq!(store.table_oids_slice(), &[42]);

        let owned = store.to_owned_store();
        assert!(!owned.is_mmap_backed());
        assert_eq!(owned.table_oid(0), Some(42));
        assert_eq!(owned.primary_key(0), Some("X"));
        assert!(owned.is_active(0));
    }

    #[test]
    fn deactivate_out_of_range_is_noop() {
        let mut store = NodeStore::new();
        store.add_node(1, "X".to_string());

        store.deactivate(99);

        assert_eq!(store.node_count(), 1);
        assert_eq!(store.active_count(), 1);
        assert!(store.is_active(0));
    }

    #[test]
    fn with_capacity_creates_empty_store() {
        let store = NodeStore::with_capacity(100);
        assert_eq!(store.node_count(), 0);
        assert_eq!(store.active_count(), 0);
    }

    #[test]
    fn table_oids_slice_returns_correct_data() {
        let mut store = NodeStore::new();
        store.add_node(10, "A".to_string());
        store.add_node(20, "B".to_string());
        let oids = store.table_oids_slice();
        assert_eq!(oids, &[10, 20]);
    }

    #[test]
    fn is_active_bytes_roundtrips_state() {
        let mut store = NodeStore::new();
        for i in 0..10u32 {
            store.add_node(1, format!("N{}", i));
        }
        store.deactivate(3);
        store.deactivate(7);
        let bytes = store.is_active_bytes();
        assert!(!bytes.is_empty());
        // Verify deactivated nodes have their bits unset
        assert!(!store.is_active(3));
        assert!(!store.is_active(7));
        assert!(store.is_active(0));
    }

    #[test]
    #[should_panic(expected = "Cannot add nodes to an mmap-backed NodeStore")]
    fn add_node_on_mmap_panics() {
        let active = [0u8];
        let oids = [0u32];
        let pk_offsets = [0u64, 0u64];
        let arrays = MmapNodeArrays::new(mapped_node_parts(&active, &oids, &pk_offsets, &[]))
            .expect("valid mmap node test metadata");
        let mut store = NodeStore::from_mmap(arrays);
        store.add_node(1, "crash".to_string());
    }

    #[test]
    fn mmap_node_arrays_validate_region_bounds_and_alignment() {
        let valid = mapped_node_parts(&[1], &[42], &[0, 0], &[]);
        assert!(MmapNodeArrays::new(valid.clone()).is_some());
        assert!(MmapNodeArrays::new(MmapNodeArrayParts {
            oid_range: 1..5,
            ..valid.clone()
        })
        .is_none());
        assert!(MmapNodeArrays::new(MmapNodeArrayParts {
            active_range: 0..2,
            ..valid.clone()
        })
        .is_none());
        assert!(MmapNodeArrays::new(MmapNodeArrayParts {
            pk_bytes_range: valid.mmap.len()..valid.mmap.len() + 1,
            ..valid
        })
        .is_none());
    }

    #[test]
    fn mmap_node_store_owns_mapping_after_constructor_inputs_drop() {
        let store = {
            let arrays = MmapNodeArrays::new(mapped_node_parts(&[1], &[42], &[0, 1], b"X"))
                .expect("valid mapped node fixture");
            NodeStore::from_mmap(arrays)
        };
        assert_eq!(store.table_oid(0), Some(42));
        assert_eq!(store.primary_key(0), Some("X"));
    }

    #[test]
    fn mmap_node_arrays_reject_invalid_primary_key_layout() {
        let active = [1u8];
        let oids = [42u32];
        let valid_offsets = [0u64, 1];
        let valid_bytes = *b"X";
        assert!(MmapNodeArrays::new(mapped_node_parts(
            &active,
            &oids,
            &valid_offsets,
            &valid_bytes
        ))
        .is_some());

        let nonzero_start = [1u64, 1];
        assert!(MmapNodeArrays::new(mapped_node_parts(
            &active,
            &oids,
            &nonzero_start,
            &valid_bytes
        ))
        .is_none());

        let bad_terminal = [0u64, 0];
        assert!(MmapNodeArrays::new(mapped_node_parts(
            &active,
            &oids,
            &bad_terminal,
            &valid_bytes
        ))
        .is_none());

        let invalid_utf8 = [0xffu8];
        assert!(MmapNodeArrays::new(mapped_node_parts(
            &active,
            &oids,
            &valid_offsets,
            &invalid_utf8
        ))
        .is_none());
    }

    #[test]
    fn bulk_add_1000_nodes() {
        let mut store = NodeStore::with_capacity(1000);
        for i in 0..1000u32 {
            let idx = store.add_node(i % 10, format!("pk-{}", i));
            assert_eq!(idx, i);
        }
        assert_eq!(store.node_count(), 1000);
        assert_eq!(store.active_count(), 1000);
        assert_eq!(store.primary_key(999), Some("pk-999"));
    }
}
