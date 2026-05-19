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
//!   primary-key bytes are read from the `.pggraph` file via mmap. Backends can
//!   share those physical pages through the OS page cache. The store is
//!   materialized into owned arrays on the first sync mutation.
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

use bitvec::prelude::*;

/// Validated pointer metadata for mmap-backed node arrays.
#[derive(Clone, Copy, Debug)]
pub struct MmapNodeArrays {
    active_ptr: *const u8,
    oid_ptr: *const u32,
    pk_offsets_ptr: *const u64,
    pk_bytes_ptr: *const u8,
    node_count: u32,
    active_byte_count: usize,
    pk_bytes_len: usize,
}

/// Raw mmap-backed node array pointers and lengths.
///
/// Values are validated by [`MmapNodeArrays::new`] before they are used by a
/// [`NodeStore`]. The struct groups the raw pointer contract so call sites do
/// not pass several same-typed pointer and length arguments positionally.
#[derive(Clone, Copy, Debug)]
pub struct MmapNodeArrayParts {
    /// Pointer to the start of the mmap region that owns every array section.
    pub region_ptr: *const u8,
    /// Length in bytes of the mmap region.
    pub region_len: usize,
    /// Pointer to `active_byte_count` initialized packed active-bit bytes.
    pub active_ptr: *const u8,
    /// Pointer to `node_count` initialized source table OIDs.
    pub oid_ptr: *const u32,
    /// Pointer to `node_count + 1` initialized primary-key byte offsets.
    pub pk_offsets_ptr: *const u64,
    /// Pointer to `pk_bytes_len` initialized UTF-8 primary-key bytes.
    pub pk_bytes_ptr: *const u8,
    /// Number of nodes represented by every per-node array.
    pub node_count: u32,
    /// Number of bytes in the packed active-bit array.
    pub active_byte_count: usize,
    /// Number of bytes in the primary-key byte section.
    pub pk_bytes_len: usize,
}

impl MmapNodeArrays {
    /// Create validated mmap pointer metadata.
    ///
    /// # Safety
    ///
    /// The caller must ensure all pointers point into the same mmap region and
    /// that the mmap outlives every [`NodeStore`] created from this metadata.
    /// Typed pointers must be aligned and initialized for the documented
    /// element counts.
    pub unsafe fn new(parts: MmapNodeArrayParts) -> Option<Self> {
        let required_ptrs_present = !parts.active_ptr.is_null()
            && !parts.oid_ptr.is_null()
            && !parts.pk_offsets_ptr.is_null()
            && !parts.pk_bytes_ptr.is_null();
        if !required_ptrs_present {
            return None;
        }
        if parts.active_byte_count != (parts.node_count as usize).div_ceil(8) {
            return None;
        }
        let oid_bytes = (parts.node_count as usize).checked_mul(std::mem::size_of::<u32>())?;
        let pk_offsets_bytes = (parts.node_count as usize)
            .checked_add(1)?
            .checked_mul(std::mem::size_of::<u64>())?;
        if !ptr_range_in_region(
            parts.active_ptr,
            parts.active_byte_count,
            parts.region_ptr,
            parts.region_len,
        ) || !ptr_range_in_region(
            parts.oid_ptr.cast::<u8>(),
            oid_bytes,
            parts.region_ptr,
            parts.region_len,
        ) || !ptr_range_in_region(
            parts.pk_offsets_ptr.cast::<u8>(),
            pk_offsets_bytes,
            parts.region_ptr,
            parts.region_len,
        ) || !ptr_range_in_region(
            parts.pk_bytes_ptr,
            parts.pk_bytes_len,
            parts.region_ptr,
            parts.region_len,
        ) {
            return None;
        }
        if !(parts.oid_ptr as usize).is_multiple_of(std::mem::align_of::<u32>())
            || !(parts.pk_offsets_ptr as usize).is_multiple_of(std::mem::align_of::<u64>())
        {
            return None;
        }

        Some(Self {
            active_ptr: parts.active_ptr,
            oid_ptr: parts.oid_ptr,
            pk_offsets_ptr: parts.pk_offsets_ptr,
            pk_bytes_ptr: parts.pk_bytes_ptr,
            node_count: parts.node_count,
            active_byte_count: parts.active_byte_count,
            pk_bytes_len: parts.pk_bytes_len,
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

/// Backing store for array data: either owned Vecs or mmap-backed sections.
pub enum ArrayBacking {
    /// Build-time: owned Vecs, mutable.
    Owned {
        is_active: BitVec,
        table_oids: Vec<u32>,
        primary_keys: Vec<String>,
    },
    /// Load-time: read-only pointers into Engine-owned mmap memory.
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

    /// Create an mmap-backed NodeStore from raw pointers.
    ///
    /// # Safety
    ///
    /// The caller must ensure all pointers point into a valid mmap'd region
    /// that outlives this NodeStore. `oid_ptr` and `pk_offsets_ptr` must be
    /// correctly aligned for their types. `oid_ptr` must contain `node_count`
    /// initialized values, `active_ptr` must contain `active_byte_count` initialized bytes,
    /// `pk_offsets_ptr` must contain `node_count + 1` initialized offsets, and
    /// `pk_bytes_ptr` must contain `pk_bytes_len` initialized bytes.
    pub unsafe fn from_mmap(arrays: MmapNodeArrays) -> Self {
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
                // SAFETY: MmapNodeArrays::new validates the active byte count,
                // and the node_idx guard above keeps byte_idx in bounds.
                let byte = unsafe { *arrays.active_ptr.add(byte_idx) };
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
                // SAFETY: MmapNodeArrays::new validates active_ptr and
                // active_byte_count for the mmap-backed active-bit section.
                let bytes = unsafe {
                    std::slice::from_raw_parts(arrays.active_ptr, arrays.active_byte_count)
                };
                let mut count = 0u32;
                for &byte in bytes {
                    count += byte.count_ones();
                }
                // Don't count bits beyond node_count
                let total_bits = arrays.active_byte_count * 8;
                let excess = total_bits as u32 - arrays.node_count;
                if excess > 0 {
                    let last_byte = bytes[arrays.active_byte_count - 1];
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

    /// Get the source table OID for a node.
    pub fn table_oid(&self, node_idx: u32) -> u32 {
        match &self.backing {
            ArrayBacking::Owned { table_oids, .. } => table_oids[node_idx as usize],
            ArrayBacking::Mmap { arrays } => {
                // SAFETY: MmapNodeArrays::new validates oid_ptr points to
                // node_count initialized u32 values.
                unsafe { *arrays.oid_ptr.add(node_idx as usize) }
            }
        }
    }

    /// Get the primary key for a node. Cold path.
    ///
    /// In mmap-backed mode this reads UTF-8 bytes through the persisted
    /// primary-key offset and byte sections. Returns an empty string if the
    /// node index is out of range, offsets are invalid, or bytes are not valid
    /// UTF-8.
    pub fn primary_key(&self, node_idx: u32) -> &str {
        match &self.backing {
            ArrayBacking::Owned { primary_keys, .. } => &primary_keys[node_idx as usize],
            ArrayBacking::Mmap { arrays } => {
                if node_idx >= arrays.node_count {
                    return "";
                }

                // SAFETY: MmapNodeArrays::new validates pk_offsets_ptr points
                // to node_count + 1 initialized offsets.
                let start = unsafe { *arrays.pk_offsets_ptr.add(node_idx as usize) as usize };
                // SAFETY: The node_idx guard keeps node_idx + 1 within the
                // validated offset table.
                let end = unsafe { *arrays.pk_offsets_ptr.add(node_idx as usize + 1) as usize };
                if start > end || end > arrays.pk_bytes_len {
                    return "";
                }

                // SAFETY: start/end are validated against pk_bytes_len above.
                let bytes = unsafe {
                    std::slice::from_raw_parts(arrays.pk_bytes_ptr.add(start), end - start)
                };
                std::str::from_utf8(bytes).unwrap_or("")
            }
        }
    }

    /// True when this store is backed by mmap'd read-only memory.
    pub fn is_mmap_backed(&self) -> bool {
        matches!(self.backing, ArrayBacking::Mmap { .. })
    }

    /// Estimate heap bytes owned directly by this store.
    ///
    /// Mmap-backed arrays are accounted as shared file mappings, not Rust heap.
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

    /// Return an owned, mutable copy of this store.
    ///
    /// Sync overlays use this to accept node tombstones and inserts after a
    /// persisted graph has been auto-loaded from mmap.
    pub fn to_owned_store(&self) -> Self {
        let node_count = self.node_count();
        let mut owned = Self::with_capacity(node_count as usize);
        for node_idx in 0..node_count {
            let idx = owned.add_node(
                self.table_oid(node_idx),
                self.primary_key(node_idx).to_string(),
            );
            if !self.is_active(node_idx) {
                owned.deactivate(idx);
            }
        }
        owned
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
            ArrayBacking::Mmap { arrays } => {
                // SAFETY: MmapNodeArrays::new validates active_ptr and
                // active_byte_count.
                unsafe {
                    std::slice::from_raw_parts(arrays.active_ptr, arrays.active_byte_count).to_vec()
                }
            }
        }
    }

    /// Get table_oids as a slice. Used by persistence.
    pub fn table_oids_slice(&self) -> &[u32] {
        match &self.backing {
            ArrayBacking::Owned { table_oids, .. } => table_oids,
            ArrayBacking::Mmap { arrays } => {
                // SAFETY: MmapNodeArrays::new validates oid_ptr and node_count.
                unsafe { std::slice::from_raw_parts(arrays.oid_ptr, arrays.node_count as usize) }
            }
        }
    }
}

impl Default for NodeStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! Covers node lifecycle, tombstone handling, storage statistics, and mmap
    //! loading invariants for persisted node data.

    use super::*;

    fn test_region_for_slices(slices: &[(*const u8, usize)]) -> (*const u8, usize) {
        let start = slices
            .iter()
            .map(|(ptr, _)| *ptr as usize)
            .min()
            .expect("test region requires at least one slice");
        let end = slices
            .iter()
            .map(|(ptr, len)| (*ptr as usize) + len)
            .max()
            .expect("test region requires at least one slice");
        (start as *const u8, end - start)
    }

    #[test]
    fn add_and_retrieve_node() {
        let mut store = NodeStore::new();
        let idx = store.add_node(12345, "PK-001".to_string());
        assert_eq!(idx, 0);
        assert_eq!(store.node_count(), 1);
        assert!(store.is_active(0));
        assert_eq!(store.table_oid(0), 12345);
        assert_eq!(store.primary_key(0), "PK-001");
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

        assert_eq!(store.table_oid(0), 100);
        assert_eq!(store.table_oid(1), 200);
        assert_eq!(store.table_oid(2), 300);
    }

    #[test]
    fn primary_keys_support_special_characters() {
        let mut store = NodeStore::new();
        store.add_node(1, "pk with spaces".to_string());
        store.add_node(1, "pk-with-dashes".to_string());
        store.add_node(1, "pk_with_🦀_emoji".to_string());
        store.add_node(1, "".to_string()); // Empty PK

        assert_eq!(store.primary_key(0), "pk with spaces");
        assert_eq!(store.primary_key(1), "pk-with-dashes");
        assert_eq!(store.primary_key(2), "pk_with_🦀_emoji");
        assert_eq!(store.primary_key(3), "");
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
        let pk_bytes = [0u8];
        let (region_ptr, region_len) = test_region_for_slices(&[
            (active.as_ptr(), active.len()),
            (
                oids.as_ptr().cast::<u8>(),
                oids.len() * std::mem::size_of::<u32>(),
            ),
            (
                pk_offsets.as_ptr().cast::<u8>(),
                pk_offsets.len() * std::mem::size_of::<u64>(),
            ),
            (pk_bytes.as_ptr(), 0),
        ]);
        // SAFETY: Pointers reference local arrays that outlive the store within
        // this test, and no mmap data is dereferenced before the expected panic.
        let arrays = unsafe {
            MmapNodeArrays::new(MmapNodeArrayParts {
                region_ptr,
                region_len,
                active_ptr: active.as_ptr(),
                oid_ptr: oids.as_ptr(),
                pk_offsets_ptr: pk_offsets.as_ptr(),
                pk_bytes_ptr: pk_bytes.as_ptr(),
                node_count: 1,
                active_byte_count: 1,
                pk_bytes_len: 0,
            })
            .expect("valid mmap node test metadata")
        };
        // SAFETY: The validated metadata above outlives this test store.
        let mut store = unsafe { NodeStore::from_mmap(arrays) };
        store.add_node(1, "crash".to_string());
    }

    #[test]
    fn mmap_node_arrays_validate_region_bounds_and_alignment() {
        let region = [0u64; 8];
        let base = region.as_ptr().cast::<u8>();
        let valid = MmapNodeArrayParts {
            region_ptr: base,
            region_len: std::mem::size_of_val(&region),
            active_ptr: base,
            // SAFETY: Offsets are within the local byte region used only for
            // constructor validation in this test.
            oid_ptr: unsafe { base.add(8).cast::<u32>() },
            // SAFETY: See above.
            pk_offsets_ptr: unsafe { base.add(16).cast::<u64>() },
            // SAFETY: See above.
            pk_bytes_ptr: unsafe { base.add(32) },
            node_count: 1,
            active_byte_count: 1,
            pk_bytes_len: 4,
        };

        // SAFETY: Pointers are into the local region and are not dereferenced.
        assert!(unsafe { MmapNodeArrays::new(valid) }.is_some());
        assert!(unsafe {
            MmapNodeArrays::new(MmapNodeArrayParts {
                region_len: 16,
                ..valid
            })
        }
        .is_none());
        assert!(unsafe {
            MmapNodeArrays::new(MmapNodeArrayParts {
                oid_ptr: base.wrapping_add(1).cast::<u32>(),
                ..valid
            })
        }
        .is_none());
        assert!(unsafe {
            MmapNodeArrays::new(MmapNodeArrayParts {
                active_ptr: std::ptr::null(),
                ..valid
            })
        }
        .is_none());
    }

    #[test]
    fn mmap_node_arrays_reject_pointer_overflow() {
        let region = [0u64; 1];
        let base = region.as_ptr().cast::<u8>();
        let near_usize_end = usize::MAX as *const u8;

        assert!(unsafe {
            MmapNodeArrays::new(MmapNodeArrayParts {
                region_ptr: base,
                region_len: std::mem::size_of_val(&region),
                active_ptr: near_usize_end,
                oid_ptr: base.cast::<u32>(),
                pk_offsets_ptr: base.cast::<u64>(),
                pk_bytes_ptr: base,
                node_count: 1,
                active_byte_count: 1,
                pk_bytes_len: 0,
            })
        }
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
        assert_eq!(store.primary_key(999), "pk-999");
    }
}
