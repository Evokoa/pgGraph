//! # ResolutionIndex — mmap'd sorted array for (table_oid, pk) → node_idx
//!
//! During build time, entries are accumulated as compact 16-byte records. Before
//! edge linking, entries are sorted and written as the same flat array used in
//! the `.pggraph` file. At query time, the mmap'd array is searched via binary
//! search.
//!
//! This eliminates per-process duplication for the persisted resolution
//! section: backends can share the same physical pages via the OS page cache.
//!
//! See: `docs/contributor_guide/memory-model.mdx`

use xxhash_rust::xxh3::xxh3_64;

/// Entry size in the persisted sorted array: 4 + 8 + 4 = 16 bytes.
pub const ENTRY_SIZE: usize = 16;

/// A single entry in the resolution index.
/// Sorted by (table_oid, pk_hash) for binary search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C, packed)]
pub struct ResolutionEntry {
    pub table_oid: u32,
    pub pk_hash: u64,
    pub node_idx: u32,
}

/// Build-time resolution index. Uses compact append-only entries and converts
/// them to a sorted array for persistence/query.
pub struct ResolutionIndexBuilder {
    entries: Vec<ResolutionEntry>,
}

impl ResolutionIndexBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    #[cfg(any(test, feature = "development"))]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
        }
    }

    /// Hash a primary key string to a u64.
    #[inline]
    pub fn hash_pk(pk: &str) -> u64 {
        xxh3_64(pk.as_bytes())
    }

    /// Insert a (table_oid, pk) → node_idx mapping.
    pub fn insert(&mut self, table_oid: u32, pk: &str, node_idx: u32) {
        let pk_hash = Self::hash_pk(pk);
        self.insert_hashed(table_oid, pk_hash, node_idx);
    }

    fn insert_hashed(&mut self, table_oid: u32, pk_hash: u64, node_idx: u32) {
        self.entries.push(ResolutionEntry {
            table_oid,
            pk_hash,
            node_idx,
        });
    }

    /// Resolve a verified (table_oid, pk) → node_idx from recent delta entries.
    pub fn resolve_verified(
        &self,
        table_oid: u32,
        pk: &str,
        mut verify: impl FnMut(u32) -> bool,
    ) -> Option<u32> {
        let pk_hash = Self::hash_pk(pk);
        self.entries
            .iter()
            .rev()
            .find(|entry| {
                entry.table_oid == table_oid && entry.pk_hash == pk_hash && verify(entry.node_idx)
            })
            .map(|entry| entry.node_idx)
    }

    /// Resolve a (table_oid, pk) → node_idx from recent delta entries.
    ///
    /// This hash-only helper is kept for direct index tests. Engine lookups use
    /// [`Self::resolve_verified`] so hash collisions cannot resolve the wrong
    /// primary key.
    #[cfg(any(test, feature = "development"))]
    pub fn resolve(&self, table_oid: u32, pk: &str) -> Option<u32> {
        let pk_hash = Self::hash_pk(pk);
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.table_oid == table_oid && entry.pk_hash == pk_hash)
            .map(|entry| entry.node_idx)
    }

    /// Convert to a sorted array of entries for persistence.
    pub fn to_sorted_entries(&self) -> Vec<ResolutionEntry> {
        let mut entries = self.entries.clone();
        entries.sort_by(|left, right| {
            (left.table_oid, left.pk_hash).cmp(&(right.table_oid, right.pk_hash))
        });
        entries
    }

    /// Serialize sorted entries to bytes for writing to the .pggraph file.
    pub fn to_bytes(&self) -> Vec<u8> {
        let entries = self.to_sorted_entries();
        let mut bytes = Vec::with_capacity(4 + entries.len() * ENTRY_SIZE);
        // Write entry count
        bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        // Write each entry
        for entry in &entries {
            bytes.extend_from_slice(&entry.table_oid.to_le_bytes());
            bytes.extend_from_slice(&entry.pk_hash.to_le_bytes());
            bytes.extend_from_slice(&entry.node_idx.to_le_bytes());
        }
        bytes
    }

    pub fn len(&self) -> usize {
        self.to_sorted_entries().len()
    }

    /// Estimate heap bytes owned by build-time resolution entries.
    pub fn estimated_heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<ResolutionEntry>()
    }

    #[cfg(any(test, feature = "development"))]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    fn insert_hashed_for_test(&mut self, table_oid: u32, pk_hash: u64, node_idx: u32) {
        self.insert_hashed(table_oid, pk_hash, node_idx);
    }
}

impl Default for ResolutionIndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Query-time resolution index. Operates on a byte slice (mmap'd or owned).
/// Uses binary search for O(log n) lookups.
pub struct ResolutionIndex<'a> {
    data: &'a [u8],
    entry_count: u32,
}

impl<'a> ResolutionIndex<'a> {
    /// Create a ResolutionIndex from a byte slice (typically mmap'd).
    ///
    /// The first 4 bytes are the entry count (u32 LE).
    /// Followed by `entry_count` entries of 16 bytes each.
    pub fn from_bytes(data: &'a [u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        let entry_count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let expected_len = 4 + (entry_count as usize) * ENTRY_SIZE;
        if data.len() < expected_len {
            return None;
        }
        Some(Self { data, entry_count })
    }

    /// Read entry at index `i`.
    #[inline]
    fn entry_at(&self, i: usize) -> (u32, u64, u32) {
        let offset = 4 + i * ENTRY_SIZE;
        let table_oid = u32::from_le_bytes([
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ]);
        let pk_hash = u64::from_le_bytes([
            self.data[offset + 4],
            self.data[offset + 5],
            self.data[offset + 6],
            self.data[offset + 7],
            self.data[offset + 8],
            self.data[offset + 9],
            self.data[offset + 10],
            self.data[offset + 11],
        ]);
        let node_idx = u32::from_le_bytes([
            self.data[offset + 12],
            self.data[offset + 13],
            self.data[offset + 14],
            self.data[offset + 15],
        ]);
        (table_oid, pk_hash, node_idx)
    }

    /// Resolve a (table_oid, pk) → node_idx via binary search.
    ///
    /// O(log n) — ~23 comparisons for 10M entries, ~115ns.
    #[cfg(any(test, feature = "development"))]
    pub fn resolve(&self, table_oid: u32, pk: &str) -> Option<u32> {
        self.resolve_verified(table_oid, pk, |_| true)
    }

    /// Resolve a verified (table_oid, pk) → node_idx via binary search.
    ///
    /// The persisted index is keyed by a 64-bit primary-key hash, so callers
    /// must verify each same-hash candidate against the authoritative
    /// [`NodeStore`](crate::node_store::NodeStore) primary-key bytes.
    pub fn resolve_verified(
        &self,
        table_oid: u32,
        pk: &str,
        mut verify: impl FnMut(u32) -> bool,
    ) -> Option<u32> {
        let pk_hash = ResolutionIndexBuilder::hash_pk(pk);
        let mut lo = 0usize;
        let mut hi = self.entry_count as usize;

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (t, h, _) = self.entry_at(mid);
            match (t, h).cmp(&(table_oid, pk_hash)) {
                std::cmp::Ordering::Equal => {
                    let mut first = mid;
                    while first > 0 {
                        let (prev_table, prev_hash, _) = self.entry_at(first - 1);
                        if (prev_table, prev_hash) != (table_oid, pk_hash) {
                            break;
                        }
                        first -= 1;
                    }

                    let mut last = mid + 1;
                    while last < self.entry_count as usize {
                        let (next_table, next_hash, _) = self.entry_at(last);
                        if (next_table, next_hash) != (table_oid, pk_hash) {
                            break;
                        }
                        last += 1;
                    }

                    return (first..last).rev().find_map(|idx| {
                        let (_, _, node_idx) = self.entry_at(idx);
                        verify(node_idx).then_some(node_idx)
                    });
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// Number of entries.
    pub fn len(&self) -> u32 {
        self.entry_count
    }

    #[cfg(any(test, feature = "development"))]
    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }
}

#[cfg(test)]
mod tests {
    //! Covers primary-key resolution index construction and lookup invariants
    //! across table OIDs, duplicate keys, and serialized index shape.

    use super::*;

    #[test]
    fn roundtrip_build_and_query() {
        let mut builder = ResolutionIndexBuilder::new();
        builder.insert(100, "PK-001", 0);
        builder.insert(100, "PK-002", 1);
        builder.insert(200, "PK-001", 2);

        let bytes = builder.to_bytes();
        let index = ResolutionIndex::from_bytes(&bytes).unwrap();

        assert_eq!(index.len(), 3);
        assert_eq!(index.resolve(100, "PK-001"), Some(0));
        assert_eq!(index.resolve(100, "PK-002"), Some(1));
        assert_eq!(index.resolve(200, "PK-001"), Some(2));
        assert_eq!(index.resolve(200, "PK-999"), None);
        assert_eq!(index.resolve(999, "PK-001"), None);
    }

    #[test]
    fn empty_index() {
        let builder = ResolutionIndexBuilder::new();
        let bytes = builder.to_bytes();
        let index = ResolutionIndex::from_bytes(&bytes).unwrap();
        assert!(index.is_empty());
        assert_eq!(index.resolve(100, "anything"), None);
    }

    #[test]
    fn large_index_binary_search() {
        let mut builder = ResolutionIndexBuilder::with_capacity(10000);
        for i in 0..10000u32 {
            builder.insert(1, &format!("PK-{:05}", i), i);
        }
        let bytes = builder.to_bytes();
        let index = ResolutionIndex::from_bytes(&bytes).unwrap();

        assert_eq!(index.len(), 10000);
        // Spot check
        assert_eq!(index.resolve(1, "PK-00000"), Some(0));
        assert_eq!(index.resolve(1, "PK-05000"), Some(5000));
        assert_eq!(index.resolve(1, "PK-09999"), Some(9999));
        assert_eq!(index.resolve(1, "PK-10000"), None);
    }

    #[test]
    fn serialized_length_matches_entry_count() {
        let mut builder = ResolutionIndexBuilder::new();
        builder.insert(10, "A", 0);
        builder.insert(20, "B", 1);
        builder.insert(30, "C", 2);

        let bytes = builder.to_bytes();

        assert_eq!(builder.len(), 3);
        assert_eq!(bytes.len(), 4 + 3 * ENTRY_SIZE);
    }

    #[test]
    fn duplicate_table_and_pk_replaces_existing_node() {
        let mut builder = ResolutionIndexBuilder::new();
        builder.insert(10, "A", 0);
        builder.insert(10, "A", 42);

        let bytes = builder.to_bytes();
        let index = ResolutionIndex::from_bytes(&bytes).unwrap();

        assert_eq!(builder.len(), 2);
        assert_eq!(index.resolve(10, "A"), Some(42));
    }

    #[test]
    fn same_hash_candidates_are_not_collapsed_and_are_verified() {
        let mut builder = ResolutionIndexBuilder::new();
        let hash = ResolutionIndexBuilder::hash_pk("left");
        builder.insert_hashed_for_test(10, hash, 0);
        builder.insert_hashed_for_test(10, hash, 1);

        let bytes = builder.to_bytes();
        let index = ResolutionIndex::from_bytes(&bytes).unwrap();

        assert_eq!(index.len(), 2);
        assert_eq!(index.resolve_verified(10, "left", |idx| idx == 0), Some(0));
        assert_eq!(index.resolve_verified(10, "left", |idx| idx == 1), Some(1));
        assert_eq!(index.resolve_verified(10, "left", |_| false), None);
        assert_eq!(
            builder.resolve_verified(10, "left", |idx| idx == 0),
            Some(0)
        );
    }

    #[test]
    fn truncated_bytes_are_rejected() {
        let mut builder = ResolutionIndexBuilder::new();
        builder.insert(10, "A", 0);
        let mut bytes = builder.to_bytes();
        bytes.pop();

        assert!(ResolutionIndex::from_bytes(&bytes).is_none());
        assert!(ResolutionIndex::from_bytes(&[1, 2, 3]).is_none());
    }

    #[test]
    fn empty_bytes_returns_none() {
        assert!(ResolutionIndex::from_bytes(&[]).is_none());
    }

    #[test]
    fn zero_entry_index_roundtrips() {
        let builder = ResolutionIndexBuilder::new();
        assert!(builder.is_empty());
        let bytes = builder.to_bytes();
        // 4 bytes header (entry_count=0)
        assert_eq!(bytes.len(), 4);
        let index = ResolutionIndex::from_bytes(&bytes).unwrap();
        assert_eq!(index.resolve(1, "anything"), None);
    }

    #[test]
    fn hash_pk_is_deterministic() {
        let h1 = ResolutionIndexBuilder::hash_pk("test-key");
        let h2 = ResolutionIndexBuilder::hash_pk("test-key");
        assert_eq!(h1, h2);
        // Different inputs produce different hashes
        let h3 = ResolutionIndexBuilder::hash_pk("other-key");
        assert_ne!(h1, h3);
    }

    #[test]
    fn builder_is_empty_reflects_state() {
        let mut builder = ResolutionIndexBuilder::new();
        assert!(builder.is_empty());
        builder.insert(1, "x", 0);
        assert!(!builder.is_empty());
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn resolution_index_serialization_invariants(
            entries in proptest::collection::vec(
                (any::<u32>(), "[a-zA-Z0-9_-]{1,20}", any::<u32>()),
                0..1000
            )
        ) {
            let mut builder = ResolutionIndexBuilder::new();
            let mut expected = std::collections::HashMap::new();

            for (table_oid, pk, node_idx) in &entries {
                builder.insert(*table_oid, pk, *node_idx);
                expected.insert((*table_oid, pk.clone()), *node_idx);
            }

            let bytes = builder.to_bytes();
            let index = ResolutionIndex::from_bytes(&bytes).unwrap();

            assert_eq!(index.len() as usize, entries.len());

            for ((table_oid, pk), expected_idx) in expected {
                assert_eq!(index.resolve(table_oid, &pk), Some(expected_idx));
            }

            // Check non-existent entry
            assert_eq!(index.resolve(999999, "nonexistent"), None);
        }
    }
}
