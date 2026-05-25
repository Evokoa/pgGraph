//! # Persistence — .pggraph file format, mmap, atomic writes
//!
//! The `.pggraph` file is the on-disk representation of the graph engine.
//! It is written atomically (write to `<path>.tmp` then rename) and
//! read via `mmap` for zero-copy access to the base graph arrays.
//!
//! ## File Format
//!
//! ```text
//! [Header]              — 128 bytes
//!   magic: "PGGH"       — 4 bytes
//!   version: u32         — 4 bytes
//!   flags: u32           — 4 bytes
//!   node_count: u32      — 4 bytes
//!   edge_count: u32      — 4 bytes
//!   section_offsets[11]  — 11 × u64 = 88 bytes
//!   crc32: u32           — 4 bytes
//!
//! [Section 0: NodeStore.is_active]            — ceil(node_count / 8) bytes
//! [Section 1: NodeStore.table_oids]           — node_count × 4 bytes
//! [Section 2: EdgeStore.edge_offsets]         — (node_count + 1) × 4 bytes
//! [Section 3: EdgeStore.targets]              — edge_count × 4 bytes
//! [Section 4: EdgeStore.type_ids]             — edge_count × 1 byte
//! [Section 5: EdgeStore.weights]              — edge_count × 4 bytes (optional)
//! [Section 6: ResolutionIndex]                — 4 + entry_count × 16 bytes
//! [Section 7: NodeStore.primary_key_offsets]  — (node_count + 1) × 8 bytes
//! [Section 8: NodeStore.primary_key_bytes]    — variable length UTF-8
//! [Section 9: FilterIndex (Bincode)]          — variable length
//! [Section 10: edge_type_registry (Bincode)]  — variable length
//! ```
//!
//! ## Memory Model
//!
//! When loaded via `load_graph_file()`:
//! - **NodeStore** (`is_active`, `table_oids`, primary-key offsets/bytes):
//!   mmap-backed and shared through the OS page cache
//! - **Forward EdgeStore** (`edge_offsets`, `targets`, `type_ids`, optional
//!   `weights`): mmap-backed and shared through the OS page cache
//! - **ResolutionIndex**: mmap'd, zero-copy, binary search
//! - **FilterIndex** and **edge type registry**: bincode sections deserialized
//!   into backend-local heap
//! - **Reverse EdgeStore**: derived into an owned CSR per backend
//!
//! See: `docs/contributor_guide/memory-model.mdx`

use std::fs;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use crate::edge_store::{EdgeStore, MmapEdgeArrayParts, MmapEdgeArrays};
use crate::engine::{Engine, MmapResolutionState};
use crate::filter_index::FilterIndex;
use crate::node_store::{MmapNodeArrayParts, MmapNodeArrays, NodeStore};
use crate::resolution_index::{ResolutionIndex, ENTRY_SIZE as RESOLUTION_ENTRY_SIZE};
use crate::safety::{GraphError, GraphResult};

/// Magic bytes for .pggraph files.
const MAGIC: &[u8; 4] = b"PGGH";
/// Current file format version.
const VERSION: u32 = 2;
/// Header size in bytes.
const HEADER_SIZE: usize = 128;
/// Number of sections.
const NUM_SECTIONS: usize = 11;
const CRC_OFFSET: usize = 20 + NUM_SECTIONS * 8;
const INTERRUPT_CHECK_INTERVAL: u32 = 4096;

fn check_for_interrupts() {
    #[cfg(not(test))]
    {
        pgrx::check_for_interrupts!();
    }
}

struct GraphArtifactWriter {
    writer: BufWriter<fs::File>,
    section_offsets: [u64; NUM_SECTIONS],
    position: u64,
    hasher: crc32fast::Hasher,
    check_interrupts: bool,
}

impl GraphArtifactWriter {
    fn new(file: fs::File, check_interrupts: bool) -> GraphResult<Self> {
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&[0u8; HEADER_SIZE])
            .map_err(|e| GraphError::Internal(format!("Header reservation failed: {}", e)))?;
        Ok(Self {
            writer,
            section_offsets: [0u64; NUM_SECTIONS],
            position: HEADER_SIZE as u64,
            hasher: crc32fast::Hasher::new(),
            check_interrupts,
        })
    }

    fn begin_section(&mut self, section: usize, alignment: Option<usize>) -> GraphResult<()> {
        if let Some(alignment) = alignment {
            self.align(alignment)?;
        }
        self.section_offsets[section] = self.position;
        Ok(())
    }

    fn align(&mut self, alignment: usize) -> GraphResult<()> {
        let position = usize::try_from(self.position)
            .map_err(|_| GraphError::Internal("artifact too large".into()))?;
        let padding = (alignment - (position % alignment)) % alignment;
        if padding > 0 {
            self.write_body(&[0u8; 8][..padding])?;
        }
        Ok(())
    }

    fn write_body(&mut self, bytes: &[u8]) -> GraphResult<()> {
        self.hasher.update(bytes);
        self.writer
            .write_all(bytes)
            .map_err(|e| GraphError::Internal(format!("Write failed: {}", e)))?;
        let len = u64::try_from(bytes.len())
            .map_err(|_| GraphError::Internal("artifact too large".into()))?;
        self.position = self
            .position
            .checked_add(len)
            .ok_or_else(|| GraphError::Internal("artifact too large".into()))?;
        Ok(())
    }

    fn write_u32_values(&mut self, values: &[u32]) -> GraphResult<()> {
        for (idx, &value) in values.iter().enumerate() {
            if self.check_interrupts && (idx as u32).is_multiple_of(INTERRUPT_CHECK_INTERVAL) {
                check_for_interrupts();
            }
            self.write_body(&value.to_le_bytes())?;
        }
        Ok(())
    }

    fn write_u64_value(&mut self, value: u64) -> GraphResult<()> {
        self.write_body(&value.to_le_bytes())
    }

    fn write_length_prefixed_payload(&mut self, payload: &[u8], label: &str) -> GraphResult<()> {
        let payload_len = u32::try_from(payload.len()).map_err(|_| {
            GraphError::Internal(format!(
                "{} section payload exceeds u32 length prefix",
                label
            ))
        })?;
        self.write_body(&payload_len.to_le_bytes())?;
        self.write_body(payload)
    }

    fn finish(mut self, node_count: u32, edge_count: u32) -> GraphResult<fs::File> {
        let crc = std::mem::replace(&mut self.hasher, crc32fast::Hasher::new()).finalize();
        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(MAGIC);
        header[4..8].copy_from_slice(&VERSION.to_le_bytes());
        header[8..12].copy_from_slice(&0u32.to_le_bytes());
        header[12..16].copy_from_slice(&node_count.to_le_bytes());
        header[16..20].copy_from_slice(&edge_count.to_le_bytes());
        for (i, &offset) in self.section_offsets.iter().enumerate() {
            let start = 20 + i * 8;
            header[start..start + 8].copy_from_slice(&offset.to_le_bytes());
        }
        header[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());

        self.writer
            .seek(SeekFrom::Start(0))
            .map_err(|e| GraphError::Internal(format!("Header seek failed: {}", e)))?;
        self.writer
            .write_all(&header)
            .map_err(|e| GraphError::Internal(format!("Header write failed: {}", e)))?;
        self.writer
            .flush()
            .map_err(|e| GraphError::Internal(format!("Flush failed: {}", e)))?;
        self.writer
            .into_inner()
            .map_err(|e| GraphError::Internal(format!("Flush failed: {}", e)))
    }
}

fn checked_section_size(count: u32, width: usize, label: &str) -> GraphResult<usize> {
    (count as usize)
        .checked_mul(width)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: format!("{} section size overflow", label),
        })
}

fn validate_section_min_len(
    ranges: &[(usize, usize); NUM_SECTIONS],
    section: usize,
    min_len: usize,
    label: &str,
) -> GraphResult<()> {
    let actual = ranges[section].1 - ranges[section].0;
    if actual < min_len {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "{} section too small: need at least {} bytes, found {}",
                label, min_len, actual
            ),
        });
    }
    Ok(())
}

fn validate_section_alignment(
    ranges: &[(usize, usize); NUM_SECTIONS],
    section: usize,
    alignment: usize,
    label: &str,
) -> GraphResult<()> {
    if !ranges[section].0.is_multiple_of(alignment) {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "{} section offset {} is not {}-byte aligned",
                label, ranges[section].0, alignment
            ),
        });
    }
    Ok(())
}

fn validate_length_prefixed_section(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    section: usize,
    label: &str,
) -> GraphResult<()> {
    length_prefixed_payload(mmap, ranges, section, label).map(|_| ())
}

fn length_prefixed_payload<'a>(
    mmap: &'a [u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    section: usize,
    label: &str,
) -> GraphResult<&'a [u8]> {
    if ranges[section].1 - ranges[section].0 < 4 {
        return Err(GraphError::CorruptFile {
            reason: format!("{} section too small for length prefix", label),
        });
    }
    let start = ranges[section].0;
    let size = read_u32_at(mmap, start) as usize;
    let payload_start = start + 4;
    let end = start
        .checked_add(4)
        .and_then(|payload_start| payload_start.checked_add(size))
        .ok_or_else(|| GraphError::CorruptFile {
            reason: format!("{} size overflow", label),
        })?;
    if end > ranges[section].1 {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "{} payload exceeds section: need end {}, section ends {}",
                label, end, ranges[section].1
            ),
        });
    }
    Ok(&mmap[payload_start..end])
}

fn read_le_array<const N: usize>(mmap: &[u8], offset: usize) -> [u8; N] {
    let end = offset + N;
    let mut bytes = [0u8; N];
    bytes.copy_from_slice(&mmap[offset..end]);
    bytes
}

fn read_u32_at(mmap: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(read_le_array(mmap, offset))
}

fn read_u64_at(mmap: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(read_le_array(mmap, offset))
}

fn validate_persisted_contents(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    node_count: u32,
    edge_count: u32,
) -> GraphResult<()> {
    let edge_offsets_start = ranges[2].0;
    let mut previous = read_u32_at(mmap, edge_offsets_start);
    if previous != 0 {
        return Err(GraphError::CorruptFile {
            reason: format!("edge_offsets[0] must be 0, found {}", previous),
        });
    }
    for idx in 1..=node_count as usize {
        let current = read_u32_at(mmap, edge_offsets_start + idx * 4);
        if current < previous {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "edge_offsets are not monotonic at index {}: {} < {}",
                    idx, current, previous
                ),
            });
        }
        if current > edge_count {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "edge_offsets[{}] exceeds edge_count: {} > {}",
                    idx, current, edge_count
                ),
            });
        }
        previous = current;
    }
    if previous != edge_count {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "final edge offset must equal edge_count: {} != {}",
                previous, edge_count
            ),
        });
    }

    let targets_start = ranges[3].0;
    for idx in 0..edge_count as usize {
        let target = read_u32_at(mmap, targets_start + idx * 4);
        if target >= node_count {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "target at index {} exceeds node_count: {} >= {}",
                    idx, target, node_count
                ),
            });
        }
    }

    let pk_offsets_start = ranges[7].0;
    let pk_bytes_len = ranges[8].1 - ranges[8].0;
    let mut previous_pk = read_u64_at(mmap, pk_offsets_start);
    if previous_pk != 0 {
        return Err(GraphError::CorruptFile {
            reason: format!("primary_key_offsets[0] must be 0, found {}", previous_pk),
        });
    }
    for idx in 1..=node_count as usize {
        let current = read_u64_at(mmap, pk_offsets_start + idx * 8);
        if current < previous_pk {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "primary_key_offsets are not monotonic at index {}: {} < {}",
                    idx, current, previous_pk
                ),
            });
        }
        if current as usize > pk_bytes_len {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "primary_key_offsets[{}] exceeds primary key bytes: {} > {}",
                    idx, current, pk_bytes_len
                ),
            });
        }
        let start = previous_pk as usize;
        let end = current as usize;
        std::str::from_utf8(&mmap[ranges[8].0 + start..ranges[8].0 + end]).map_err(|err| {
            GraphError::CorruptFile {
                reason: format!(
                    "primary key at node index {} is not valid UTF-8: {}",
                    idx - 1,
                    err
                ),
            }
        })?;
        previous_pk = current;
    }

    Ok(())
}

fn validate_section_layout(
    mmap: &[u8],
    section_offsets: &[u64; NUM_SECTIONS],
    node_count: u32,
    edge_count: u32,
) -> GraphResult<[(usize, usize); NUM_SECTIONS]> {
    let mut starts = [0usize; NUM_SECTIONS];
    let mut prev_offset = HEADER_SIZE;
    for (i, &offset) in section_offsets.iter().enumerate() {
        let offset = usize::try_from(offset).map_err(|_| GraphError::CorruptFile {
            reason: format!("section offset {} does not fit in usize", i),
        })?;
        if offset < prev_offset || offset > mmap.len() {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "invalid section offset at index {}: {} (prev: {}, mmap_len: {})",
                    i,
                    offset,
                    prev_offset,
                    mmap.len()
                ),
            });
        }
        starts[i] = offset;
        prev_offset = offset;
    }

    let ranges = std::array::from_fn(|i| {
        let end = if i + 1 < NUM_SECTIONS {
            starts[i + 1]
        } else {
            mmap.len()
        };
        (starts[i], end)
    });

    let active_byte_count = (node_count as usize).div_ceil(8);
    let node_plus_one = node_count
        .checked_add(1)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "node_count overflow in edge offset section".to_string(),
        })?;
    let node_u32_bytes = checked_section_size(node_count, 4, "table_oids")?;
    let edge_offsets_bytes = checked_section_size(node_plus_one, 4, "edge_offsets")?;
    let edge_targets_bytes = checked_section_size(edge_count, 4, "targets")?;
    let edge_type_bytes = checked_section_size(edge_count, 1, "type_ids")?;
    let edge_weight_bytes = checked_section_size(edge_count, 4, "weights")?;
    let pk_offsets_bytes = checked_section_size(node_plus_one, 8, "primary_key_offsets")?;

    validate_section_alignment(&ranges, 1, 4, "table_oids")?;
    validate_section_alignment(&ranges, 2, 4, "edge_offsets")?;
    validate_section_alignment(&ranges, 3, 4, "targets")?;
    validate_section_alignment(&ranges, 5, 4, "weights")?;
    validate_section_alignment(&ranges, 7, 8, "primary_key_offsets")?;

    validate_section_min_len(&ranges, 0, active_byte_count, "is_active")?;
    validate_section_min_len(&ranges, 1, node_u32_bytes, "table_oids")?;
    validate_section_min_len(&ranges, 2, edge_offsets_bytes, "edge_offsets")?;
    validate_section_min_len(&ranges, 3, edge_targets_bytes, "targets")?;
    validate_section_min_len(&ranges, 4, edge_type_bytes, "type_ids")?;
    validate_section_min_len(&ranges, 7, pk_offsets_bytes, "primary_key_offsets")?;

    let weights_len = ranges[5].1 - ranges[5].0;
    if weights_len != 0 && weights_len != edge_weight_bytes {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "weights section must be empty or exactly {} bytes, found {}",
                edge_weight_bytes, weights_len
            ),
        });
    }

    let resolution = &mmap[ranges[6].0..ranges[6].1];
    let resolution_index =
        ResolutionIndex::from_bytes(resolution).ok_or_else(|| GraphError::CorruptFile {
            reason: "invalid resolution index section".to_string(),
        })?;
    let resolution_min_len = 4 + resolution_index.len() as usize * RESOLUTION_ENTRY_SIZE;
    validate_section_min_len(&ranges, 6, resolution_min_len, "resolution_index")?;

    validate_length_prefixed_section(mmap, &ranges, 9, "filter index")?;
    validate_length_prefixed_section(mmap, &ranges, 10, "edge type registry")?;

    validate_persisted_contents(mmap, &ranges, node_count, edge_count)?;

    Ok(ranges)
}

/// Write the engine state to a .pggraph file.
///
/// Uses atomic rename: writes to `<path>.tmp`, then renames to `path`.
#[cfg(test)]
pub fn write_graph_file(engine: &Engine, path: &Path) -> GraphResult<()> {
    write_graph_file_internal(engine, path, false)
}

pub(crate) fn write_graph_file_with_interrupt_checks(
    engine: &Engine,
    path: &Path,
) -> GraphResult<()> {
    write_graph_file_internal(engine, path, true)
}

fn write_graph_file_internal(
    engine: &Engine,
    path: &Path,
    check_interrupts: bool,
) -> GraphResult<()> {
    let tmp_path = append_path_suffix(path, ".tmp");

    // Ensure parent directory exists (handles first-run where $PGDATA/graph/ doesn't exist)
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            GraphError::Internal(format!(
                "Cannot create directory {}: {}",
                parent.display(),
                e
            ))
        })?;
    }

    let file = fs::File::create(&tmp_path).map_err(|e| {
        GraphError::Internal(format!("Cannot create {}: {}", tmp_path.display(), e))
    })?;
    let mut writer = GraphArtifactWriter::new(file, check_interrupts)?;

    writer.begin_section(0, None)?;
    let is_active_bytes = engine.node_store.is_active_bytes();
    writer.write_body(&is_active_bytes)?;

    writer.begin_section(1, Some(4))?;
    writer.write_u32_values(engine.node_store.table_oids_slice())?;

    writer.begin_section(2, Some(4))?;
    writer.write_u32_values(engine.edge_store.offsets_slice())?;

    writer.begin_section(3, Some(4))?;
    writer.write_u32_values(engine.edge_store.targets_slice())?;

    writer.begin_section(4, None)?;
    writer.write_body(engine.edge_store.type_ids_slice())?;

    writer.begin_section(5, Some(4))?;
    writer.write_u32_values(engine.edge_store.weights_slice())?;

    writer.begin_section(6, None)?;
    let ri_bytes = engine.resolution_to_bytes();
    writer.write_body(&ri_bytes)?;

    writer.begin_section(7, Some(8))?;
    let mut pk_offset = 0u64;
    writer.write_u64_value(pk_offset)?;
    for node_idx in 0..engine.node_store.node_count() {
        if check_interrupts && node_idx.is_multiple_of(INTERRUPT_CHECK_INTERVAL) {
            check_for_interrupts();
        }
        let pk = engine.node_store.primary_key(node_idx);
        pk_offset =
            pk_offset
                .checked_add(u64::try_from(pk.len()).map_err(|_| {
                    GraphError::Internal("primary key payload too large".to_string())
                })?)
                .ok_or_else(|| GraphError::Internal("primary key payload too large".to_string()))?;
        writer.write_u64_value(pk_offset)?;
    }

    writer.begin_section(8, None)?;
    for node_idx in 0..engine.node_store.node_count() {
        if check_interrupts && node_idx.is_multiple_of(INTERRUPT_CHECK_INTERVAL) {
            check_for_interrupts();
        }
        let pk = engine.node_store.primary_key(node_idx);
        writer.write_body(pk.as_bytes())?;
    }

    writer.begin_section(9, None)?;
    let filter_bytes = bincode::serialize(&engine.filter_index)
        .map_err(|e| GraphError::Internal(format!("FilterIndex serialization failed: {}", e)))?;
    writer.write_length_prefixed_payload(&filter_bytes, "filter index")?;

    writer.begin_section(10, None)?;
    let edge_type_bytes = bincode::serialize(&engine.edge_type_registry).map_err(|e| {
        GraphError::Internal(format!("edge_type_registry serialization failed: {}", e))
    })?;
    writer.write_length_prefixed_payload(&edge_type_bytes, "edge type registry")?;

    let file = writer.finish(
        engine.node_store.node_count(),
        engine.edge_store.edge_count(),
    )?;
    file.sync_all()
        .map_err(|e| GraphError::Internal(format!("Sync failed: {}", e)))?;

    // Atomic rename
    fs::rename(&tmp_path, path)
        .map_err(|e| GraphError::Internal(format!("Rename failed: {}", e)))?;
    write_sync_checkpoint(path, engine.applied_sync_id)?;

    Ok(())
}

pub fn sync_checkpoint_path(path: &Path) -> PathBuf {
    append_path_suffix(path, ".sync")
}

pub fn write_sync_checkpoint(path: &Path, applied_sync_id: i64) -> GraphResult<()> {
    let checkpoint_path = sync_checkpoint_path(path);
    let tmp_path = append_path_suffix(&checkpoint_path, ".tmp");
    let mut file = fs::File::create(&tmp_path).map_err(|e| {
        GraphError::Internal(format!("Cannot create {}: {}", tmp_path.display(), e))
    })?;
    writeln!(file, "{}", applied_sync_id)
        .map_err(|e| GraphError::Internal(format!("Write failed: {}", e)))?;
    file.sync_all()
        .map_err(|e| GraphError::Internal(format!("Sync failed: {}", e)))?;
    fs::rename(&tmp_path, checkpoint_path)
        .map_err(|e| GraphError::Internal(format!("Rename failed: {}", e)))?;
    Ok(())
}

pub fn read_sync_checkpoint(path: &Path) -> GraphResult<Option<i64>> {
    let checkpoint_path = sync_checkpoint_path(path);
    if !checkpoint_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&checkpoint_path).map_err(|e| {
        GraphError::Internal(format!(
            "Cannot read sync checkpoint {}: {}",
            checkpoint_path.display(),
            e
        ))
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<i64>()
        .map(Some)
        .map_err(|e| GraphError::CorruptFile {
            reason: format!("invalid sync checkpoint '{}': {}", trimmed, e),
        })
}

/// Load a graph from a .pggraph file.
///
/// The loader validates the header, section layout, CRC, CSR invariants, and
/// primary-key offset table before constructing an [`Engine`].
///
/// After load:
/// - NodeStore active bits, table OIDs, primary-key offsets, and primary-key
///   bytes are mmap-backed.
/// - The forward EdgeStore CSR arrays are mmap-backed.
/// - ResolutionIndex lookups read the mmap-backed resolution section.
/// - FilterIndex and the edge type registry are bincode-deserialized into
///   backend-local heap.
/// - The reverse EdgeStore CSR is rebuilt into backend-local heap for inbound
///   traversal.
///
/// Multiple backends can share mmap-backed pages via the OS page cache.
/// Derived and bincode-backed structures remain per-backend heap allocations.
pub fn load_graph_file(path: &Path) -> GraphResult<Engine> {
    let file = fs::File::open(path)
        .map_err(|e| GraphError::Internal(format!("Cannot open {}: {}", path.display(), e)))?;

    // SAFETY: The file remains open until the read-only mapping is created,
    // and Engine owns the mapping for the lifetime of all mmap-backed stores.
    let mmap = unsafe { Mmap::map(&file) }
        .map_err(|e| GraphError::Internal(format!("mmap failed: {}", e)))?;

    if mmap.len() < HEADER_SIZE {
        return Err(GraphError::CorruptFile {
            reason: "file too small for header".to_string(),
        });
    }

    // Validate header
    if &mmap[0..4] != MAGIC {
        return Err(GraphError::CorruptFile {
            reason: "invalid magic bytes".to_string(),
        });
    }
    let version = read_u32_at(&mmap, 4);
    if version != VERSION {
        return Err(GraphError::IncompatibleVersion(
            "Graph file format is outdated. Please run SELECT graph.build() to regenerate it."
                .to_string(),
        ));
    }

    let node_count = read_u32_at(&mmap, 12);
    let edge_count = read_u32_at(&mmap, 16);

    // Read section offsets
    let mut section_offsets = [0u64; NUM_SECTIONS];
    for (i, offset) in section_offsets.iter_mut().enumerate().take(NUM_SECTIONS) {
        let start = 20 + i * 8;
        *offset = read_u64_at(&mmap, start);
    }

    // Validate CRC32
    let stored_crc = read_u32_at(&mmap, CRC_OFFSET);
    let computed_crc = crc32fast::hash(&mmap[HEADER_SIZE..]);
    if stored_crc != computed_crc {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "CRC32 mismatch: stored={:#x}, computed={:#x}",
                stored_crc, computed_crc
            ),
        });
    }

    let section_ranges = validate_section_layout(&mmap, &section_offsets, node_count, edge_count)?;

    // ── Construct mmap-backed NodeStore ──
    // SAFETY: The mmap handle outlives all pointer dereferences.
    // The .pggraph file layout guarantees correct alignment for u32/u64 arrays.
    let active_ptr = mmap[section_ranges[0].0..].as_ptr();
    let oid_ptr = mmap[section_ranges[1].0..].as_ptr() as *const u32;
    let pk_offsets_ptr = mmap[section_ranges[7].0..].as_ptr() as *const u64;
    let pk_bytes_ptr = mmap[section_ranges[8].0..].as_ptr();
    let active_byte_count = (node_count as usize).div_ceil(8);
    let pk_bytes_len = section_ranges[8].1 - section_ranges[8].0;

    // SAFETY: validate_section_layout has already checked section bounds and
    // cross-section invariants. MmapNodeArrays validates alignment and active
    // byte count before NodeStore receives pointer metadata.
    let node_arrays = unsafe {
        MmapNodeArrays::new(MmapNodeArrayParts {
            region_ptr: mmap.as_ptr(),
            region_len: mmap.len(),
            active_ptr,
            oid_ptr,
            pk_offsets_ptr,
            pk_bytes_ptr,
            node_count,
            active_byte_count,
            pk_bytes_len,
        })
    }
    .ok_or_else(|| GraphError::CorruptFile {
        reason: "invalid mmap node section metadata".to_string(),
    })?;
    // SAFETY: node_arrays points into mmap, and engine._mmap owns the mapping
    // for at least as long as this NodeStore is reachable.
    let node_store = unsafe { NodeStore::from_mmap(node_arrays) };

    // ── Construct mmap-backed EdgeStore ──
    let offsets_ptr = mmap[section_ranges[2].0..].as_ptr() as *const u32;
    let targets_ptr = mmap[section_ranges[3].0..].as_ptr() as *const u32;
    let type_ids_ptr = mmap[section_ranges[4].0..].as_ptr();
    let weights_ptr = mmap[section_ranges[5].0..].as_ptr() as *const u32;
    let has_weights = section_ranges[5].1 > section_ranges[5].0;

    // SAFETY: validate_section_layout has checked CSR bounds and monotonicity.
    // MmapEdgeArrays validates pointer presence and alignment.
    let edge_arrays = unsafe {
        MmapEdgeArrays::new(MmapEdgeArrayParts {
            region_ptr: mmap.as_ptr(),
            region_len: mmap.len(),
            offsets_ptr,
            targets_ptr,
            type_ids_ptr,
            weights_ptr,
            node_count,
            edge_count,
            has_weights,
        })
    }
    .ok_or_else(|| GraphError::CorruptFile {
        reason: "invalid mmap edge section metadata".to_string(),
    })?;
    // SAFETY: edge_arrays points into mmap, and engine._mmap owns the mapping
    // for at least as long as this EdgeStore is reachable.
    let edge_store = unsafe { EdgeStore::from_mmap(edge_arrays) };

    // ── ResolutionIndex: mmap'd, zero-copy (handled by Engine) ──
    let ri_start = section_ranges[6].0;
    let ri_end = section_ranges[6].1;
    let ri_len = ri_end - ri_start;

    // FilterIndex and edge_type_registry are variable-size bincode sections.
    // They are deserialized into backend-local heap rather than kept as
    // mmap-backed stores.
    let filter_data = length_prefixed_payload(&mmap, &section_ranges, 9, "filter index")?;
    let filter_index: FilterIndex = bincode::deserialize(filter_data)
        .map_err(|e| GraphError::Internal(format!("FilterIndex deserialization failed: {}", e)))?;

    let registry_data = length_prefixed_payload(&mmap, &section_ranges, 10, "edge type registry")?;
    let edge_type_registry: Vec<String> = bincode::deserialize(registry_data).map_err(|e| {
        GraphError::Internal(format!("edge_type_registry deserialization failed: {}", e))
    })?;
    if edge_type_registry
        .first()
        .is_none_or(|label| !label.is_empty())
    {
        return Err(GraphError::CorruptFile {
            reason: "edge type registry must reserve empty label at index 0".to_string(),
        });
    }

    let mut engine = Engine::new();
    engine.install_mmap_backed_graph(
        node_store,
        edge_store,
        filter_index,
        edge_type_registry,
        mmap,
        MmapResolutionState::new(ri_start, ri_len),
    );
    if let Some(applied_sync_id) = read_sync_checkpoint(path)? {
        engine.record_applied_sync_id(applied_sync_id);
    }

    Ok(engine)
}

/// Get the default .pggraph file path under $PGDATA/{data_dir}/.
///
/// Uses the `graph.data_dir` GUC (default: "graph").
pub fn graph_file_path() -> GraphResult<PathBuf> {
    let pgdata = std::env::var("PGDATA")
        .ok()
        .or_else(postgres_data_directory)
        .ok_or_else(|| {
            GraphError::Internal(
                "PGDATA is not set; cannot determine durable graph artifact path".to_string(),
            )
        })?;
    if pgdata.trim().is_empty() {
        return Err(GraphError::Internal(
            "PGDATA is empty; cannot determine durable graph artifact path".to_string(),
        ));
    }
    let subdir = graph_data_dir();
    let dir = PathBuf::from(&pgdata).join(&subdir);
    fs::create_dir_all(&dir).map_err(|e| {
        GraphError::Internal(format!(
            "Cannot create graph data directory {}: {}",
            dir.display(),
            e
        ))
    })?;
    Ok(dir.join("main.pggraph"))
}

#[cfg(any(not(test), feature = "pg_test"))]
fn postgres_data_directory() -> Option<String> {
    // SAFETY: `DataDir` is initialized by PostgreSQL before extension code runs
    // in a backend. It is a NUL-terminated server-owned string and is only read
    // here to derive the durable artifact directory.
    let data_dir = unsafe {
        let ptr = pgrx::pg_sys::DataDir;
        if ptr.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(ptr)
    };
    data_dir
        .to_str()
        .ok()
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}

#[cfg(all(test, not(feature = "pg_test")))]
fn postgres_data_directory() -> Option<String> {
    None
}

fn graph_data_dir() -> String {
    #[cfg(all(test, not(feature = "pg_test")))]
    {
        "graph".to_string()
    }
    #[cfg(any(not(test), feature = "pg_test"))]
    {
        crate::config::data_dir()
    }
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    //! Covers `.pggraph` file persistence and loader hardening so corrupted
    //! section metadata cannot reach mmap-backed stores unchecked.

    use super::*;
    use crate::edge_store::RawEdge;
    use crate::types::{FilterCondition, FilterOp};

    #[cfg(not(feature = "pg_test"))]
    use std::sync::Mutex;

    #[cfg(not(feature = "pg_test"))]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(not(feature = "pg_test"))]
    struct EnvRestore {
        key: &'static str,
        value: Option<String>,
    }

    #[cfg(not(feature = "pg_test"))]
    impl EnvRestore {
        fn capture(key: &'static str) -> Self {
            Self {
                key,
                value: std::env::var(key).ok(),
            }
        }
    }

    #[cfg(not(feature = "pg_test"))]
    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(value) = &self.value {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn artifact_sidecar_paths_append_to_pggraph_filename() {
        let path = PathBuf::from("/tmp/graph/main.pggraph");

        assert_eq!(
            append_path_suffix(&path, ".tmp"),
            PathBuf::from("/tmp/graph/main.pggraph.tmp")
        );
        assert_eq!(
            sync_checkpoint_path(&path),
            PathBuf::from("/tmp/graph/main.pggraph.sync")
        );
        assert_eq!(
            append_path_suffix(&sync_checkpoint_path(&path), ".tmp"),
            PathBuf::from("/tmp/graph/main.pggraph.sync.tmp")
        );
    }

    #[cfg(not(feature = "pg_test"))]
    #[test]
    fn graph_file_path_requires_pgdata() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _restore = EnvRestore::capture("PGDATA");
        std::env::remove_var("PGDATA");

        let result = graph_file_path();

        assert!(matches!(result, Err(GraphError::Internal(message)) if message.contains("PGDATA")));
    }

    #[cfg(not(feature = "pg_test"))]
    #[test]
    fn graph_file_path_creates_pgdata_subdir() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _restore = EnvRestore::capture("PGDATA");
        let pgdata = std::env::temp_dir().join(format!(
            "graph-pgdata-path-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::remove_dir_all(&pgdata);
        std::env::set_var("PGDATA", &pgdata);

        let path = graph_file_path().unwrap();

        assert_eq!(path, pgdata.join("graph").join("main.pggraph"));
        assert!(path.parent().unwrap().exists());
        let _ = std::fs::remove_dir_all(&pgdata);
    }

    #[test]
    fn persisted_mmap_load_preserves_primary_keys_and_weights() {
        let mut engine = Engine::new();
        let a = engine.node_store.add_node(10, "A-1".to_string());
        let b = engine.node_store.add_node(10, "B-2".to_string());
        engine.resolution_insert(10, "A-1", a);
        engine.resolution_insert(10, "B-2", b);
        let edge_type = engine.register_edge_type("officer_of").unwrap();
        engine.edge_store = EdgeStore::from_edges(
            2,
            vec![RawEdge {
                source: a,
                target: b,
                type_id: edge_type,
                weight: Some(7),
            }],
            true,
        );
        engine.built = true;

        let path = std::env::temp_dir().join(format!(
            "graph-persistence-test-{}.pggraph",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        write_graph_file(&loaded, &path).unwrap();
        let reloaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.node_store.primary_key(a), "A-1");
        assert_eq!(loaded.node_store.primary_key(b), "B-2");
        assert_eq!(loaded.resolve(10, "A-1"), Some(a));
        assert_eq!(loaded.resolve(10, "B-2"), Some(b));
        assert_eq!(loaded.edge_type_registry, vec!["", "officer_of"]);
        assert!(loaded.edge_store.has_weights());
        assert_eq!(loaded.edge_store.neighbors_weighted(a).2, &[7]);
        assert_eq!(reloaded.node_store.primary_key(a), "A-1");
        assert_eq!(reloaded.node_store.primary_key(b), "B-2");
        assert_eq!(reloaded.edge_type_registry, vec!["", "officer_of"]);
        assert_eq!(reloaded.edge_store.neighbors_weighted(a).2, &[7]);
    }

    #[test]
    fn persisted_graph_roundtrips_filter_index_section() {
        let mut engine = Engine::new();
        let a = engine.node_store.add_node(10, "A-1".to_string());
        let b = engine.node_store.add_node(10, "B-2".to_string());
        engine.resolution_insert(10, "A-1", a);
        engine.resolution_insert(10, "B-2", b);
        engine.edge_store = EdgeStore::from_edges(2, vec![], false);
        let status = engine
            .filter_index
            .register_typed_column_with_populated_count(
                10,
                "status".to_string(),
                crate::filter_index::FilterColumnType::Text,
                100,
                2,
            );
        let open = engine.filter_index.intern_text_value(status, "open");
        engine.filter_index.set_encoded_value(
            status,
            a,
            Some(crate::filter_index::EncodedFilterValue::Text(open)),
        );
        engine.built = true;

        let path = temp_graph_path("filter-index-roundtrip");
        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let loaded_status = loaded.filter_index.find_column("status").unwrap();
        assert!(loaded.filter_index.check_filter(
            a,
            &FilterOp::new(loaded_status, FilterCondition::EqToken(open))
        ));
        assert!(!loaded.filter_index.check_filter(
            b,
            &FilterOp::new(loaded_status, FilterCondition::NeqToken(open))
        ));
        assert!(loaded
            .filter_index
            .check_filter(b, &FilterOp::new(loaded_status, FilterCondition::IsNull)));
    }

    #[test]
    fn graph_file_uses_launch_section_layout() {
        let mut engine = Engine::new();
        engine.built = true;

        let path = temp_graph_path("launch-section-layout");
        write_graph_file(&engine, &path).unwrap();
        let active_offset = read_section_offset(&path, 0);
        let table_oids_offset = read_section_offset(&path, 1);
        let filter_offset = read_section_offset(&path, 9);
        let registry_offset = read_section_offset(&path, 10);
        let file_len = std::fs::metadata(&path).unwrap().len();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(NUM_SECTIONS, 11);
        assert_eq!(active_offset, HEADER_SIZE as u64);
        assert_eq!(table_oids_offset, HEADER_SIZE as u64);
        assert!(filter_offset >= HEADER_SIZE as u64);
        assert!(registry_offset > filter_offset);
        assert!(file_len > registry_offset);
    }

    #[test]
    fn graph_file_section_sizes_match_launch_artifact_contract() {
        let mut engine = Engine::new();
        let node_idx = engine.node_store.add_node(10, "A-1".to_string());
        engine.resolution_insert(10, "A-1", node_idx);
        engine.edge_store = EdgeStore::from_edges(1, vec![], false);
        let status = engine
            .filter_index
            .register_typed_column_with_populated_count(
                10,
                "status".to_string(),
                crate::filter_index::FilterColumnType::Text,
                1,
                1,
            );
        let open = engine.filter_index.intern_text_value(status, "open");
        engine.filter_index.set_encoded_value(
            status,
            node_idx,
            Some(crate::filter_index::EncodedFilterValue::Text(open)),
        );
        engine.built = true;

        let path = temp_graph_path("launch-artifact-section-sizes");
        write_graph_file(&engine, &path).unwrap();
        let header_version = read_u32_from_file(&path, 4);
        let filter_offset = read_section_offset(&path, 9);
        let registry_offset = read_section_offset(&path, 10);
        let filter_payload_len = read_u32_from_file(&path, filter_offset) as u64;
        let file_len = std::fs::metadata(&path).unwrap().len();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(header_version, VERSION);
        assert_eq!(NUM_SECTIONS, 11);
        assert!(filter_offset >= HEADER_SIZE as u64);
        assert!(registry_offset > filter_offset);
        assert_eq!(registry_offset - filter_offset, 4 + filter_payload_len);
        assert!(filter_payload_len > 0);
        assert!(file_len > registry_offset);
    }

    #[test]
    fn corrupt_magic_bytes_returns_error() {
        let path = std::env::temp_dir().join(format!(
            "graph-corrupt-magic-{}.pggraph",
            std::process::id()
        ));
        // Write garbage that starts with wrong magic
        std::fs::write(&path, b"NOPE_THIS_IS_NOT_A_GRAPH_FILE_AND_HAS_ENOUGH_BYTES_FOR_HEADER_VALIDATION_128_BYTES_PADDED_OUT_WITH_JUNK_0000000000000000000000000000000000000").unwrap();

        let result = load_graph_file(&path);
        let _ = std::fs::remove_file(&path);

        assert!(result.is_err());
        match result {
            Err(GraphError::CorruptFile { reason }) => {
                assert!(
                    reason.contains("magic"),
                    "expected magic error, got: {}",
                    reason
                );
            }
            Err(other) => panic!("expected CorruptFile, got {:?}", other),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    #[test]
    fn truncated_file_returns_error() {
        let path =
            std::env::temp_dir().join(format!("graph-truncated-{}.pggraph", std::process::id()));
        // Write file smaller than HEADER_SIZE (128 bytes)
        std::fs::write(&path, b"PGGH_tiny").unwrap();

        let result = load_graph_file(&path);
        let _ = std::fs::remove_file(&path);

        assert!(result.is_err());
        match result {
            Err(GraphError::CorruptFile { reason }) => {
                assert!(
                    reason.contains("too small"),
                    "expected size error, got: {}",
                    reason
                );
            }
            Err(other) => panic!("expected CorruptFile, got {:?}", other),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    #[test]
    fn nonexistent_file_returns_error() {
        let path = std::env::temp_dir().join("graph-does-not-exist.pggraph");
        let _ = std::fs::remove_file(&path); // Ensure it doesn't exist

        let result = load_graph_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn empty_graph_roundtrips() {
        let engine = Engine::new();

        let path = std::env::temp_dir().join(format!("graph-empty-{}.pggraph", std::process::id()));
        let _ = std::fs::remove_file(&path);

        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.node_store.node_count(), 0);
        assert_eq!(loaded.edge_store.edge_count(), 0);
    }

    #[test]
    fn unweighted_graph_roundtrips_without_weights() {
        let mut engine = Engine::new();
        engine.node_store.add_node(10, "X".to_string());
        engine.node_store.add_node(10, "Y".to_string());
        engine.resolution_insert(10, "X", 0);
        engine.resolution_insert(10, "Y", 1);

        // No weights
        engine.edge_store = EdgeStore::from_edges(
            2,
            vec![RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: None,
            }],
            false,
        );
        engine.built = true;

        let path =
            std::env::temp_dir().join(format!("graph-unweighted-{}.pggraph", std::process::id()));
        let _ = std::fs::remove_file(&path);

        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.node_store.node_count(), 2);
        assert_eq!(loaded.edge_store.edge_count(), 1);
        assert!(!loaded.edge_store.has_weights());
        assert_eq!(loaded.node_store.primary_key(0), "X");
        assert_eq!(loaded.node_store.primary_key(1), "Y");
    }

    #[test]
    fn large_graph_roundtrip_preserves_all_nodes() {
        let mut engine = Engine::new();
        let n = 1000;
        for i in 0..n {
            engine.node_store.add_node(1, format!("node-{}", i));
            engine.resolution_insert(1, &format!("node-{}", i), i);
        }
        engine.built = true;
        engine.edge_store = EdgeStore::from_edges(n, vec![], false);

        let path = std::env::temp_dir().join(format!("graph-large-{}.pggraph", std::process::id()));
        let _ = std::fs::remove_file(&path);

        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.node_store.node_count(), n);
        assert_eq!(loaded.node_store.primary_key(0), "node-0");
        assert_eq!(loaded.node_store.primary_key(999), "node-999");
        assert_eq!(loaded.resolve(1, "node-500"), Some(500));
    }

    #[test]
    fn corrupted_crc_is_detected() {
        let mut engine = Engine::new();
        engine.node_store.add_node(1, "A".to_string());
        engine.resolution_insert(1, "A", 0);
        engine.edge_store = EdgeStore::from_edges(1, vec![], false);
        engine.built = true;

        let path = std::env::temp_dir().join(format!("graph-crc-{}.pggraph", std::process::id()));
        let _ = std::fs::remove_file(&path);
        write_graph_file(&engine, &path).unwrap();

        // Corrupt the file by flipping a byte near the end (CRC region)
        let mut data = std::fs::read(&path).unwrap();
        let last_idx = data.len() - 1;
        data[last_idx] ^= 0xFF;
        std::fs::write(&path, &data).unwrap();

        let result = load_graph_file(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err(), "corrupted CRC should be rejected");
    }

    #[test]
    fn tombstoned_nodes_persist_through_roundtrip() {
        let mut engine = Engine::new();
        engine.node_store.add_node(1, "alive".to_string());
        engine.node_store.add_node(1, "dead".to_string());
        engine.resolution_insert(1, "alive", 0);
        engine.resolution_insert(1, "dead", 1);
        engine.node_store.deactivate(1); // tombstone "dead"
        engine.edge_store = EdgeStore::from_edges(2, vec![], false);
        engine.built = true;

        let path = std::env::temp_dir().join(format!("graph-tomb-{}.pggraph", std::process::id()));
        let _ = std::fs::remove_file(&path);
        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.node_store.node_count(), 2);
        assert!(loaded.node_store.is_active(0));
        assert!(!loaded.node_store.is_active(1));
    }

    #[test]
    fn empty_graph_roundtrips_cleanly() {
        let mut engine = Engine::new();
        engine.edge_store = EdgeStore::from_edges(0, vec![], false);
        engine.built = true;

        let dir = std::env::temp_dir().join(format!(
            "graph-test-empty-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.pggraph");
        let _ = std::fs::remove_file(&path);
        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(loaded.node_store.node_count(), 0);
        assert_eq!(loaded.edge_store.edge_count(), 0);
    }

    #[test]
    fn load_graph_file_rejects_out_of_bounds_section_offsets() {
        let mut engine = Engine::new();
        engine.built = true;

        let dir = std::env::temp_dir().join(format!(
            "graph-test-corrupt-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_corrupt.pggraph");
        write_graph_file(&engine, &path).unwrap();

        // Corrupt the section offsets: Make the first offset extremely large
        use std::io::{Seek, Write};
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(std::io::SeekFrom::Start(20)).unwrap();
        let bad_offset: u64 = u64::MAX;
        file.write_all(&bad_offset.to_le_bytes()).unwrap();
        file.flush().unwrap();

        // Must NOT panic. Must return CorruptFile error.
        let result = load_graph_file(&path);
        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_graph_file_rejects_version_before_section_parsing() {
        let mut engine = Engine::new();
        engine.built = true;

        let dir = std::env::temp_dir().join(format!(
            "graph-test-version-mismatch-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_version_mismatch.pggraph");
        write_graph_file(&engine, &path).unwrap();

        use std::io::{Seek, Write};
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(std::io::SeekFrom::Start(4)).unwrap();
        file.write_all(&(VERSION + 1).to_le_bytes()).unwrap();
        file.seek(std::io::SeekFrom::Start(20)).unwrap();
        file.write_all(&u64::MAX.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_graph_file(&path);
        match result {
            Err(GraphError::IncompatibleVersion(message)) => assert_eq!(
                message,
                "Graph file format is outdated. Please run SELECT graph.build() to regenerate it."
            ),
            Err(other) => panic!("expected IncompatibleVersion, got {:?}", other),
            Ok(_) => panic!("expected version mismatch to fail"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn read_section_offset(path: &Path, section: usize) -> u64 {
        use std::io::{Read, Seek};

        let mut file = std::fs::OpenOptions::new().read(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start((20 + section * 8) as u64))
            .unwrap();
        let mut bytes = [0u8; 8];
        file.read_exact(&mut bytes).unwrap();
        u64::from_le_bytes(bytes)
    }

    fn read_u32_from_file(path: &Path, offset: u64) -> u32 {
        use std::io::{Read, Seek};

        let mut file = std::fs::OpenOptions::new().read(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        let mut bytes = [0u8; 4];
        file.read_exact(&mut bytes).unwrap();
        u32::from_le_bytes(bytes)
    }

    fn write_section_offset(path: &Path, section: usize, offset: u64) {
        use std::io::{Seek, Write};

        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start((20 + section * 8) as u64))
            .unwrap();
        file.write_all(&offset.to_le_bytes()).unwrap();
        file.flush().unwrap();
    }

    fn write_u32_at(path: &Path, offset: u64, value: u32) {
        use std::io::{Seek, Write};

        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.write_all(&value.to_le_bytes()).unwrap();
        file.flush().unwrap();
    }

    fn write_u64_at(path: &Path, offset: u64, value: u64) {
        use std::io::{Seek, Write};

        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.write_all(&value.to_le_bytes()).unwrap();
        file.flush().unwrap();
    }

    fn rewrite_crc(path: &Path) {
        use std::io::{Read, Seek, Write};

        let mut data = Vec::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        let crc = crc32fast::hash(&data[HEADER_SIZE..]);
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(CRC_OFFSET as u64))
            .unwrap();
        file.write_all(&crc.to_le_bytes()).unwrap();
        file.flush().unwrap();
    }

    fn graph_with_relationship() -> Engine {
        let mut engine = Engine::new();
        let a = engine.node_store.add_node(10, "A".to_string());
        let b = engine.node_store.add_node(10, "B".to_string());
        engine.resolution_insert(10, "A", a);
        engine.resolution_insert(10, "B", b);
        engine.edge_store = EdgeStore::from_edges(
            2,
            vec![RawEdge {
                source: a,
                target: b,
                type_id: 1,
                weight: Some(7),
            }],
            true,
        );
        engine.built = true;
        engine
    }

    fn temp_graph_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "graph-test-{}-{}-{}",
            name,
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("test.pggraph")
    }

    #[test]
    fn load_graph_file_rejects_in_bounds_undersized_fixed_section() {
        let mut engine = Engine::new();
        engine.node_store.add_node(10, "A".to_string());
        engine.resolution_insert(10, "A", 0);
        engine.edge_store = EdgeStore::from_edges(1, vec![], false);
        engine.built = true;

        let dir = std::env::temp_dir().join(format!(
            "graph-test-short-fixed-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_short_fixed.pggraph");
        write_graph_file(&engine, &path).unwrap();

        let first_section_offset = read_section_offset(&path, 0);
        write_section_offset(&path, 1, first_section_offset);

        let result = load_graph_file(&path);
        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_graph_file_rejects_in_bounds_empty_filter_section() {
        let mut engine = Engine::new();
        engine.built = true;

        let dir = std::env::temp_dir().join(format!(
            "graph-test-short-filter-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_short_filter.pggraph");
        write_graph_file(&engine, &path).unwrap();

        let filter_offset = read_section_offset(&path, 9);
        write_section_offset(&path, 10, filter_offset);

        let result = load_graph_file(&path);
        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_nonmonotonic_edge_offsets() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-edge-offsets");
        write_graph_file(&engine, &path).unwrap();

        let edge_offsets = read_section_offset(&path, 2);
        write_u32_at(&path, edge_offsets + 4, 2);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_bad_final_edge_offset() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-final-edge-offset");
        write_graph_file(&engine, &path).unwrap();

        let edge_offsets = read_section_offset(&path, 2);
        write_u32_at(&path, edge_offsets + 8, 0);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_target_out_of_range() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-target");
        write_graph_file(&engine, &path).unwrap();

        let targets = read_section_offset(&path, 3);
        write_u32_at(&path, targets, 2);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_partial_weights_section() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-weights");
        write_graph_file(&engine, &path).unwrap();

        let weights = read_section_offset(&path, 5);
        write_section_offset(&path, 6, weights + 1);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_nonmonotonic_pk_offsets() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-pk-offsets");
        write_graph_file(&engine, &path).unwrap();

        let pk_offsets = read_section_offset(&path, 7);
        write_u64_at(&path, pk_offsets + 16, 0);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_pk_offset_out_of_bounds() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-pk-offset-bounds");
        write_graph_file(&engine, &path).unwrap();

        let pk_offsets = read_section_offset(&path, 7);
        write_u64_at(&path, pk_offsets + 16, 999);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_invalid_primary_key_utf8() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-pk-utf8");
        write_graph_file(&engine, &path).unwrap();

        let pk_bytes = read_section_offset(&path, 8);
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        use std::io::{Seek, Write};
        file.seek(std::io::SeekFrom::Start(pk_bytes)).unwrap();
        file.write_all(&[0xFF]).unwrap();
        file.flush().unwrap();
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("valid UTF-8")
        ));
    }
}
