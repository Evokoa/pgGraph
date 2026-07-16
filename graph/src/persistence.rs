//! # Persistence — .pggraph file format, mmap, atomic writes
//!
//! The `.pggraph` file is the on-disk representation of the graph engine.
//! It is written atomically (write to `<path>.tmp` then rename) and
//! loaded into an immutable anonymous mapping for typed access to the base
//! graph arrays.
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
//!   section_offsets[12]  — 12 × u64 = 96 bytes
//!   crc32: u32           — 4 bytes
//!
//! [Section 0: NodeStore.is_active]            — ceil(node_count / 8) bytes
//! [Section 1: NodeStore.table_oids]           — node_count × 4 bytes
//! [Section 2: EdgeStore.edge_offsets]         — (node_count + 1) × 4 bytes
//! [Section 3: EdgeStore.targets]              — edge_count × 4 bytes
//! [Section 4: EdgeStore.type_ids]             — edge_count × 1 byte
//! [Section 5: EdgeStore.weights]              — edge_count × 4 bytes (optional)
//! [Section 6: EdgeStore.schema_reversed]      — edge_count × 1 byte
//! [Section 7: ResolutionIndex]                — 4 + entry_count × 16 bytes
//! [Section 8: NodeStore.primary_key_offsets]  — (node_count + 1) × 8 bytes
//! [Section 9: NodeStore.primary_key_bytes]    — variable length UTF-8
//! [Section 10: FilterIndex (Bincode)]         — variable length
//! [Section 11: edge metadata (Bincode)]       — variable length
//! ```
//!
//! ## Memory Model
//!
//! When loaded via `load_graph_file()`:
//! - **NodeStore** (`is_active`, `table_oids`, primary-key offsets/bytes):
//!   backed by a backend-local immutable mapping
//! - **Forward EdgeStore** (`edge_offsets`, `targets`, `type_ids`, optional
//!   `weights`): backed by the same backend-local immutable mapping
//! - **ResolutionIndex**: mapped, zero-copy within the backend, binary search
//! - **FilterIndex**, edge type registry, and relationship identity metadata:
//!   bincode sections deserialized into backend-local heap
//! - **Reverse EdgeStore**: derived into an owned CSR per backend
//!
//! See: `docs/contributor_guide/memory-model.mdx`

use std::fs;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::{Mmap, MmapMut};

use crate::config;
use crate::edge_store::{
    EdgeStore, MmapEdgeArrayParts, MmapEdgeArrays, RelationshipId, RelationshipIdentity,
};
use crate::engine::{Engine, MmapBackedGraph, MmapResolutionState};
use crate::filter_index::FilterIndex;
use crate::graph_policy::GraphId;
use crate::mapped_bytes::MappedBytes;
use crate::node_store::{MmapNodeArrayParts, MmapNodeArrays, NodeStore};
use crate::projection::manifest::{ProjectionManifest, ProjectionManifestStore};
use crate::resolution_index::{ResolutionIndex, ENTRY_SIZE as RESOLUTION_ENTRY_SIZE};
use crate::safety::{GraphError, GraphResult};

/// Magic bytes for .pggraph files.
const MAGIC: &[u8; 4] = b"PGGH";
/// Current file format version.
const VERSION: u32 = 4;
/// Header size in bytes.
const HEADER_SIZE: usize = 128;
/// Number of sections.
const NUM_SECTIONS: usize = 12;
const CRC_OFFSET: usize = 20 + NUM_SECTIONS * 8;
const INTERRUPT_CHECK_INTERVAL: u32 = 4096;

/// Fully validated fixed-width section layout for one graph artifact.
#[derive(Clone, Debug)]
struct ValidatedGraphLayout {
    ranges: [(usize, usize); NUM_SECTIONS],
    node_count: u32,
    edge_count: u32,
}

/// Capability proving the full artifact layout passed persistence validation.
/// Its private field prevents mapped stores from being constructed through a
/// production path that bypasses [`ValidatedGraphLayout`].
pub(crate) struct ValidatedMappedGraphToken {
    _private: (),
}

/// Owns the immutable mapping from which all mapped graph views are built.
struct MappedGraphArtifact {
    mmap: Arc<Mmap>,
    layout: ValidatedGraphLayout,
    token: ValidatedMappedGraphToken,
}

impl MappedGraphArtifact {
    fn node_arrays(&self) -> GraphResult<MmapNodeArrays> {
        let ranges = &self.layout.ranges;
        MmapNodeArrays::new_for_artifact(
            MmapNodeArrayParts {
                mmap: MappedBytes::from_mmap(Arc::clone(&self.mmap)),
                active_range: ranges[0].0
                    ..ranges[0].0 + (self.layout.node_count as usize).div_ceil(8),
                oid_range: ranges[1].0
                    ..ranges[1].0 + self.layout.node_count as usize * std::mem::size_of::<u32>(),
                pk_offsets_range: ranges[8].0
                    ..ranges[8].0
                        + (self.layout.node_count as usize + 1) * std::mem::size_of::<u64>(),
                pk_bytes_range: ranges[9].0..ranges[9].1,
                node_count: self.layout.node_count,
            },
            &self.token,
        )
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "invalid mmap node section metadata".to_string(),
        })
    }

    fn edge_arrays(&self) -> GraphResult<MmapEdgeArrays> {
        let ranges = &self.layout.ranges;
        let weights_range = (ranges[5].0 != ranges[5].1).then(|| {
            ranges[5].0..ranges[5].0 + self.layout.edge_count as usize * std::mem::size_of::<u32>()
        });
        MmapEdgeArrays::new_for_artifact(
            MmapEdgeArrayParts {
                mmap: MappedBytes::from_mmap(Arc::clone(&self.mmap)),
                offsets_range: ranges[2].0
                    ..ranges[2].0
                        + (self.layout.node_count as usize + 1) * std::mem::size_of::<u32>(),
                targets_range: ranges[3].0
                    ..ranges[3].0 + self.layout.edge_count as usize * std::mem::size_of::<u32>(),
                type_ids_range: ranges[4].0..ranges[4].0 + self.layout.edge_count as usize,
                schema_reversed_range: ranges[6].0..ranges[6].0 + self.layout.edge_count as usize,
                weights_range,
                node_count: self.layout.node_count,
                edge_count: self.layout.edge_count,
            },
            &self.token,
        )
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "invalid mmap edge section metadata".to_string(),
        })
    }
}

fn ensure_native_mapped_layout_supported(little_endian: bool) -> GraphResult<()> {
    if little_endian {
        Ok(())
    } else {
        Err(GraphError::IncompatibleVersion(
            "mmap graph loading requires a little-endian target; rebuild and load on a supported architecture"
                .to_string(),
        ))
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedEdgeMetadata {
    edge_type_registry: Vec<String>,
    relationship_ids: Vec<RelationshipId>,
    relationship_identities: Vec<Option<RelationshipIdentity>>,
}

fn check_for_interrupts() {
    crate::resource::check_postgres_interrupts();
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

fn decode_bincode_section<T, C>(
    data: &[u8],
    config: C,
    section_label: &str,
    error_label: &str,
) -> GraphResult<T>
where
    T: serde::de::DeserializeOwned,
    C: bincode::config::Config,
{
    let (value, bytes_read) = bincode::serde::decode_from_slice(data, config)
        .map_err(|e| GraphError::Internal(format!("{}: {}", error_label, e)))?;
    if bytes_read != data.len() {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "{} bincode payload has {} trailing byte(s)",
                section_label,
                data.len() - bytes_read
            ),
        });
    }
    Ok(value)
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

    let pk_offsets_start = ranges[8].0;
    let pk_bytes_len = ranges[9].1 - ranges[9].0;
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
        let current = persisted_pk_offset_to_usize(current, idx)?;
        if current > pk_bytes_len {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "primary_key_offsets[{}] exceeds primary key bytes: {} > {}",
                    idx, current, pk_bytes_len
                ),
            });
        }
        let start = persisted_pk_offset_to_usize(previous_pk, idx - 1)?;
        let end = current;
        std::str::from_utf8(&mmap[ranges[9].0 + start..ranges[9].0 + end]).map_err(|err| {
            GraphError::CorruptFile {
                reason: format!(
                    "primary key at node index {} is not valid UTF-8: {}",
                    idx - 1,
                    err
                ),
            }
        })?;
        previous_pk = current as u64;
    }

    Ok(())
}

fn persisted_pk_offset_to_usize(offset: u64, index: usize) -> GraphResult<usize> {
    usize::try_from(offset).map_err(|_| GraphError::CorruptFile {
        reason: format!(
            "primary_key_offsets[{}] cannot be represented on this platform",
            index
        ),
    })
}

fn validate_section_layout(
    mmap: &[u8],
    section_offsets: &[u64; NUM_SECTIONS],
    node_count: u32,
    edge_count: u32,
) -> GraphResult<ValidatedGraphLayout> {
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
    let edge_schema_reversed_bytes = checked_section_size(edge_count, 1, "schema_reversed")?;
    let pk_offsets_bytes = checked_section_size(node_plus_one, 8, "primary_key_offsets")?;

    validate_section_alignment(&ranges, 1, 4, "table_oids")?;
    validate_section_alignment(&ranges, 2, 4, "edge_offsets")?;
    validate_section_alignment(&ranges, 3, 4, "targets")?;
    validate_section_alignment(&ranges, 5, 4, "weights")?;
    validate_section_alignment(&ranges, 8, 8, "primary_key_offsets")?;

    validate_section_min_len(&ranges, 0, active_byte_count, "is_active")?;
    validate_section_min_len(&ranges, 1, node_u32_bytes, "table_oids")?;
    validate_section_min_len(&ranges, 2, edge_offsets_bytes, "edge_offsets")?;
    validate_section_min_len(&ranges, 3, edge_targets_bytes, "targets")?;
    validate_section_min_len(&ranges, 4, edge_type_bytes, "type_ids")?;
    validate_section_min_len(&ranges, 6, edge_schema_reversed_bytes, "schema_reversed")?;
    validate_section_min_len(&ranges, 8, pk_offsets_bytes, "primary_key_offsets")?;

    let weights_len = ranges[5].1 - ranges[5].0;
    if weights_len != 0 && weights_len != edge_weight_bytes {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "weights section must be empty or exactly {} bytes, found {}",
                edge_weight_bytes, weights_len
            ),
        });
    }

    for &flag in &mmap[ranges[6].0..ranges[6].0 + edge_schema_reversed_bytes] {
        if flag > 1 {
            return Err(GraphError::CorruptFile {
                reason: format!("schema_reversed flag must be 0 or 1, found {flag}"),
            });
        }
    }

    let resolution = &mmap[ranges[7].0..ranges[7].1];
    let resolution_index =
        ResolutionIndex::from_bytes(resolution).ok_or_else(|| GraphError::CorruptFile {
            reason: "invalid resolution index section".to_string(),
        })?;
    let resolution_min_len = 4 + resolution_index.len() as usize * RESOLUTION_ENTRY_SIZE;
    validate_section_min_len(&ranges, 7, resolution_min_len, "resolution_index")?;

    validate_length_prefixed_section(mmap, &ranges, 10, "filter index")?;
    validate_length_prefixed_section(mmap, &ranges, 11, "edge metadata")?;

    validate_persisted_contents(mmap, &ranges, node_count, edge_count)?;

    Ok(ValidatedGraphLayout {
        ranges,
        node_count,
        edge_count,
    })
}

/// Write the engine state to a .pggraph file.
///
/// Uses atomic rename: writes to `<path>.tmp`, then renames to `path`.
#[cfg(test)]
pub fn write_graph_file(engine: &Engine, path: &Path) -> GraphResult<()> {
    write_graph_file_internal(engine, path, false, None)
}

pub(crate) fn write_graph_file_with_interrupt_checks_and_resources(
    engine: &Engine,
    path: &Path,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<()> {
    write_graph_file_internal(engine, path, true, Some(governor))
}

fn write_graph_file_internal(
    engine: &Engine,
    path: &Path,
    check_interrupts: bool,
    governor: Option<&crate::resource::ResourceGovernor>,
) -> GraphResult<()> {
    let mut workspace = governor
        .map(|governor| {
            governor.reserve_memory(
                crate::resource::ResourcePhase::Persistence,
                crate::resource::ByteCount::ZERO,
            )
        })
        .transpose()
        .map_err(resource_error_to_oom)?;
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
    reserve_persistence_workspace(
        &mut workspace,
        packed_active_bytes(engine.node_store.node_count())?,
    )?;
    let is_active_bytes = engine.node_store.is_active_bytes();
    writer.write_body(&is_active_bytes)?;
    drop(is_active_bytes);
    release_persistence_workspace(&mut workspace);

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
    writer.write_body(engine.edge_store.schema_reversed_slice())?;

    writer.begin_section(7, None)?;
    reserve_persistence_workspace(
        &mut workspace,
        resolution_serialization_bytes(engine.node_store.node_count())?,
    )?;
    let ri_bytes = engine.resolution_to_bytes();
    writer.write_body(&ri_bytes)?;
    drop(ri_bytes);
    release_persistence_workspace(&mut workspace);

    writer.begin_section(8, Some(8))?;
    let mut pk_offset = 0u64;
    writer.write_u64_value(pk_offset)?;
    for node_idx in 0..engine.node_store.node_count() {
        if check_interrupts && node_idx.is_multiple_of(INTERRUPT_CHECK_INTERVAL) {
            check_for_interrupts();
        }
        let pk = engine.node_store.primary_key(node_idx).ok_or_else(|| {
            GraphError::Internal(format!(
                "node store is missing primary key metadata for index {node_idx}"
            ))
        })?;
        pk_offset =
            pk_offset
                .checked_add(u64::try_from(pk.len()).map_err(|_| {
                    GraphError::Internal("primary key payload too large".to_string())
                })?)
                .ok_or_else(|| GraphError::Internal("primary key payload too large".to_string()))?;
        writer.write_u64_value(pk_offset)?;
    }

    writer.begin_section(9, None)?;
    for node_idx in 0..engine.node_store.node_count() {
        if check_interrupts && node_idx.is_multiple_of(INTERRUPT_CHECK_INTERVAL) {
            check_for_interrupts();
        }
        let pk = engine.node_store.primary_key(node_idx).ok_or_else(|| {
            GraphError::Internal(format!(
                "node store is missing primary key metadata for index {node_idx}"
            ))
        })?;
        writer.write_body(pk.as_bytes())?;
    }

    writer.begin_section(10, None)?;
    let bincode_config = bincode::config::standard();
    reserve_persistence_workspace(
        &mut workspace,
        serialization_workspace_bytes(engine.filter_index.estimated_heap_bytes())?,
    )?;
    let filter_bytes = bincode::serde::encode_to_vec(&engine.filter_index, bincode_config)
        .map_err(|e| GraphError::Internal(format!("FilterIndex serialization failed: {}", e)))?;
    writer.write_length_prefixed_payload(&filter_bytes, "filter index")?;
    drop(filter_bytes);
    release_persistence_workspace(&mut workspace);

    writer.begin_section(11, None)?;
    reserve_persistence_workspace(
        &mut workspace,
        serialization_workspace_bytes(edge_metadata_heap_bytes(engine)?)?,
    )?;
    let edge_metadata = PersistedEdgeMetadata {
        edge_type_registry: engine.edge_type_registry.clone(),
        relationship_ids: engine.edge_store.relationship_ids_slice().to_vec(),
        relationship_identities: engine.relationship_identities.clone(),
    };
    let edge_metadata_bytes = bincode::serde::encode_to_vec(&edge_metadata, bincode_config)
        .map_err(|e| GraphError::Internal(format!("edge metadata serialization failed: {}", e)))?;
    writer.write_length_prefixed_payload(&edge_metadata_bytes, "edge metadata")?;
    drop(edge_metadata_bytes);
    drop(edge_metadata);
    release_persistence_workspace(&mut workspace);

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
    write_projection_mode(path, engine.projection_mode)?;

    Ok(())
}

fn reserve_persistence_workspace(
    workspace: &mut Option<crate::resource::ResourceLease<'_>>,
    bytes: crate::resource::ByteCount,
) -> GraphResult<()> {
    if let Some(workspace) = workspace {
        workspace
            .try_grow_in(crate::resource::ResourcePhase::Persistence, bytes)
            .map_err(resource_error_to_oom)?;
    }
    Ok(())
}

fn release_persistence_workspace(workspace: &mut Option<crate::resource::ResourceLease<'_>>) {
    if let Some(workspace) = workspace {
        workspace.release_all();
    }
}

fn packed_active_bytes(node_count: u32) -> GraphResult<crate::resource::ByteCount> {
    let bytes = u64::from(node_count)
        .checked_add(7)
        .map(|bits| bits / 8)
        .ok_or_else(|| GraphError::Internal("active bitmap size overflowed".to_string()))?;
    Ok(crate::resource::ByteCount::from_bytes(bytes))
}

fn resolution_serialization_bytes(node_count: u32) -> GraphResult<crate::resource::ByteCount> {
    let entry_size = u64::try_from(crate::resolution_index::ENTRY_SIZE)
        .map_err(|_| GraphError::Internal("resolution entry size does not fit u64".to_string()))?;
    u64::from(node_count)
        .checked_mul(entry_size)
        .and_then(|bytes| bytes.checked_add(4))
        .map(crate::resource::ByteCount::from_bytes)
        .ok_or_else(|| GraphError::Internal("resolution serialization size overflowed".to_string()))
}

fn serialization_workspace_bytes(source_bytes: usize) -> GraphResult<crate::resource::ByteCount> {
    source_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(64 * 1024))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("serialization workspace size overflowed".to_string()))
}

fn edge_metadata_heap_bytes(engine: &Engine) -> GraphResult<usize> {
    let labels = engine
        .edge_type_registry
        .iter()
        .try_fold(0usize, |bytes, label| {
            bytes
                .checked_add(std::mem::size_of::<String>())
                .and_then(|value| value.checked_add(label.len()))
                .ok_or(())
        })
        .map_err(|()| GraphError::Internal("edge label size overflowed".to_string()))?;
    let relationship_ids = engine
        .edge_store
        .relationship_ids_slice()
        .len()
        .checked_mul(std::mem::size_of::<RelationshipId>())
        .ok_or_else(|| GraphError::Internal("relationship ID size overflowed".to_string()))?;
    let identities = engine
        .relationship_identities
        .iter()
        .try_fold(0usize, |bytes, identity| {
            let payload = identity
                .as_ref()
                .map_or(0, |identity| identity.source_key.len());
            bytes
                .checked_add(std::mem::size_of::<Option<RelationshipIdentity>>())
                .and_then(|value| value.checked_add(payload))
                .ok_or(())
        })
        .map_err(|()| GraphError::Internal("relationship identity size overflowed".to_string()))?;
    labels
        .checked_add(relationship_ids)
        .and_then(|bytes| bytes.checked_add(identities))
        .ok_or_else(|| GraphError::Internal("edge metadata size overflowed".to_string()))
}

fn resource_error_to_oom(error: crate::resource::ResourceLimitError) -> GraphError {
    GraphError::Oom {
        used_mb: crate::resource::ByteCount::from_bytes(error.used()).ceil_mib(),
        need_mb: crate::resource::ByteCount::from_bytes(error.requested()).ceil_mib(),
        limit_mb: crate::resource::ByteCount::from_bytes(error.limit()).as_u64() / 1_048_576,
    }
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

pub fn projection_mode_path(path: &Path) -> PathBuf {
    append_path_suffix(path, ".projection_mode")
}

pub fn write_projection_mode(
    path: &Path,
    projection_mode: config::ProjectionMode,
) -> GraphResult<()> {
    let mode_path = projection_mode_path(path);
    let tmp_path = append_path_suffix(&mode_path, ".tmp");
    let mut file = fs::File::create(&tmp_path).map_err(|e| {
        GraphError::Internal(format!("Cannot create {}: {}", tmp_path.display(), e))
    })?;
    writeln!(file, "{}", projection_mode.as_str())
        .map_err(|e| GraphError::Internal(format!("Write failed: {}", e)))?;
    file.sync_all()
        .map_err(|e| GraphError::Internal(format!("Sync failed: {}", e)))?;
    fs::rename(&tmp_path, mode_path)
        .map_err(|e| GraphError::Internal(format!("Rename failed: {}", e)))?;
    Ok(())
}

pub fn read_projection_mode(path: &Path) -> GraphResult<Option<config::ProjectionMode>> {
    let mode_path = projection_mode_path(path);
    if !mode_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&mode_path).map_err(|e| {
        GraphError::Internal(format!(
            "Cannot read projection mode {}: {}",
            mode_path.display(),
            e
        ))
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    config::parse_projection_mode(trimmed)
        .map(Some)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: format!("invalid projection mode '{}'", trimmed),
        })
}

/// Load a graph from a .pggraph file.
///
/// The loader validates the header, section layout, CRC, CSR invariants,
/// relationship identity metadata, and primary-key offset table before
/// constructing an [`Engine`].
///
/// After load:
/// - NodeStore active bits, table OIDs, primary-key offsets, and primary-key
///   bytes are mmap-backed.
/// - The forward EdgeStore CSR arrays are mmap-backed.
/// - ResolutionIndex lookups read the mmap-backed resolution section.
/// - FilterIndex, the edge type registry, and relationship identity metadata
///   are bincode-deserialized into backend-local heap.
/// - The reverse EdgeStore CSR is rebuilt into backend-local heap for inbound
///   traversal.
///
/// Each backend copies the artifact into an anonymous read-only mapping before
/// creating typed views. This prevents same-inode writes or truncation by
/// another process from invalidating Rust references. Derived and
/// bincode-backed structures also remain per-backend allocations.
pub fn load_graph_file(path: &Path) -> GraphResult<Engine> {
    load_graph_file_internal(path, None, crate::resource::ByteCount::ZERO)
}

/// Load while accounting for private state retained until validation finishes.
pub(crate) fn load_graph_file_with_residency(
    path: &Path,
    resident: crate::resource::ByteCount,
) -> GraphResult<Engine> {
    load_graph_file_internal(path, None, resident)
}

/// Load a graph artifact against an unpublished projection candidate.
///
/// This is the semantic validation boundary used before an ingestion manifest
/// becomes current. The candidate's referenced immutable artifacts must
/// already exist, but the manifest itself need not have been published.
pub(crate) fn load_graph_file_with_projection_candidate_and_residency(
    path: &Path,
    candidate: &ProjectionManifest,
    resident: crate::resource::ByteCount,
) -> GraphResult<Engine> {
    load_graph_file_internal(path, Some(candidate), resident)
}

fn load_graph_file_internal(
    path: &Path,
    projection_candidate: Option<&ProjectionManifest>,
    resident: crate::resource::ByteCount,
) -> GraphResult<Engine> {
    ensure_native_mapped_layout_supported(cfg!(target_endian = "little"))?;
    let manifest_root = projection_manifest_root(path);
    let _generation_reader_lock = projection_candidate
        .is_none()
        .then(|| ProjectionManifestStore::new(&manifest_root).acquire_reader_lock())
        .transpose()?;

    let mut file = fs::File::open(path)
        .map_err(|e| GraphError::Internal(format!("Cannot open {}: {}", path.display(), e)))?;

    let file_len = usize::try_from(
        file.metadata()
            .map_err(|e| GraphError::Internal(format!("Cannot stat {}: {}", path.display(), e)))?
            .len(),
    )
    .map_err(|_| GraphError::Internal("graph artifact is too large for this platform".into()))?;
    if file_len < HEADER_SIZE {
        return Err(GraphError::CorruptFile {
            reason: "file too small for header".to_string(),
        });
    }
    let load_governor = crate::resource::load_governor(resident);
    let file_bytes = crate::resource::ByteCount::from_usize(file_len)
        .ok_or_else(|| GraphError::Internal("graph artifact size does not fit u64".to_string()))?;
    let _snapshot_memory = load_governor
        .reserve_memory(crate::resource::ResourcePhase::LoadMetadata, file_bytes)
        .map_err(crate::safety::resource_limit_error)?;
    let mut snapshot = MmapMut::map_anon(file_len)
        .map_err(|e| GraphError::Internal(format!("anonymous mmap failed: {}", e)))?;
    file.read_exact(&mut snapshot)
        .map_err(|e| GraphError::Internal(format!("Cannot snapshot {}: {}", path.display(), e)))?;
    let mmap = Arc::new(
        snapshot
            .make_read_only()
            .map_err(|e| GraphError::Internal(format!("read-only mmap failed: {}", e)))?,
    );

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

    let layout = validate_section_layout(&mmap, &section_offsets, node_count, edge_count)?;
    let reverse_bytes = reverse_csr_workspace_bytes(node_count, edge_count)?;
    let _reverse_memory = load_governor
        .reserve_memory(crate::resource::ResourcePhase::LoadInbound, reverse_bytes)
        .map_err(crate::safety::resource_limit_error)?;
    let variable_metadata_bytes = layout.ranges[10]
        .1
        .checked_sub(layout.ranges[10].0)
        .and_then(|filter| {
            layout.ranges[11]
                .1
                .checked_sub(layout.ranges[11].0)
                .and_then(|metadata| filter.checked_add(metadata))
        })
        .and_then(|bytes| bytes.checked_mul(2))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("load metadata estimate overflowed".to_string()))?;
    let _variable_metadata = load_governor
        .reserve_memory(
            crate::resource::ResourcePhase::LoadMetadata,
            variable_metadata_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
    let artifact = MappedGraphArtifact {
        mmap,
        layout,
        token: ValidatedMappedGraphToken { _private: () },
    };
    let section_ranges = &artifact.layout.ranges;

    // The artifact is the only production construction boundary for mapped
    // stores. Each store retains its own Arc to the immutable mapping.
    let node_store = NodeStore::from_mmap(artifact.node_arrays()?);
    let edge_arrays = artifact.edge_arrays()?;
    // ── ResolutionIndex: mmap'd, zero-copy (handled by Engine) ──
    let ri_start = section_ranges[7].0;
    let ri_end = section_ranges[7].1;
    let ri_len = ri_end - ri_start;

    // FilterIndex and edge metadata are variable-size bincode sections. They
    // are deserialized into backend-local heap rather than kept as mmap-backed
    // stores.
    let bincode_config = bincode::config::standard();
    let filter_data = length_prefixed_payload(&artifact.mmap, section_ranges, 10, "filter index")?;
    let filter_index: FilterIndex = decode_bincode_section(
        filter_data,
        bincode_config,
        "filter index",
        "FilterIndex deserialization failed",
    )?;
    filter_index.validate_persisted_layout(node_count)?;

    let registry_data =
        length_prefixed_payload(&artifact.mmap, section_ranges, 11, "edge metadata")?;
    let edge_metadata: PersistedEdgeMetadata = decode_bincode_section(
        registry_data,
        bincode_config,
        "edge metadata",
        "edge metadata deserialization failed",
    )?;
    if edge_metadata
        .edge_type_registry
        .first()
        .is_none_or(|label| !label.is_empty())
    {
        return Err(GraphError::CorruptFile {
            reason: "edge type registry must reserve empty label at index 0".to_string(),
        });
    }
    if edge_metadata.relationship_ids.len() != edge_count as usize {
        return Err(GraphError::CorruptFile {
            reason: "relationship identity sidecar length does not match edge count".to_string(),
        });
    }
    if edge_metadata
        .relationship_identities
        .first()
        .is_none_or(Option::is_some)
    {
        return Err(GraphError::CorruptFile {
            reason: "relationship identity dictionary must reserve ID 0".to_string(),
        });
    }
    for (idx, identity) in edge_metadata
        .relationship_identities
        .iter()
        .enumerate()
        .skip(1)
    {
        if let Some(identity) = identity {
            if identity.mapping_id == 0 {
                return Err(GraphError::CorruptFile {
                    reason: format!("relationship identity dictionary slot {idx} is invalid"),
                });
            }
        }
    }
    for &id in &edge_metadata.relationship_ids {
        if id == 0 {
            continue;
        }
        match edge_metadata.relationship_identities.get(id as usize) {
            Some(Some(identity)) => {
                debug_assert!(identity.mapping_id != 0);
            }
            Some(None) => {
                return Err(GraphError::CorruptFile {
                    reason: format!("relationship ID {id} points to an empty dictionary slot"),
                });
            }
            None => {
                return Err(GraphError::CorruptFile {
                    reason: format!("relationship ID {id} is outside dictionary"),
                });
            }
        }
    }

    let edge_store =
        EdgeStore::from_mmap_with_relationship_ids(edge_arrays, edge_metadata.relationship_ids);

    let mut engine = Engine::new();
    engine.install_mmap_backed_graph(MmapBackedGraph {
        node_store,
        edge_store,
        filter_index,
        edge_type_registry: edge_metadata.edge_type_registry,
        relationship_identities: edge_metadata.relationship_identities,
        mmap: artifact.mmap,
        resolution_state: MmapResolutionState::new(ri_start, ri_len),
    })?;
    if let Some(applied_sync_id) = read_sync_checkpoint(path)? {
        engine.record_applied_sync_id(applied_sync_id);
    }
    if let Some(projection_mode) = read_projection_mode(path)? {
        engine.set_projection_mode(projection_mode);
    }
    let manifest = match projection_candidate {
        Some(candidate) => {
            validate_projection_manifest_base(path, computed_crc, candidate)?;
            Some(candidate.clone())
        }
        None => load_projection_manifest(path, computed_crc, &manifest_root)?,
    };
    let projection_workspace = manifest
        .as_ref()
        .map(|manifest| projection_workspace_bytes(&manifest_root, manifest))
        .transpose()?;
    let _projection_memory = projection_workspace
        .map(|bytes| {
            load_governor
                .reserve_memory(crate::resource::ResourcePhase::LoadMetadata, bytes)
                .map_err(crate::safety::resource_limit_error)
        })
        .transpose()?;
    if let Some(manifest) = manifest {
        if let Some(reference) = &manifest.relationship_identities {
            let identity_path = manifest_root.join(&reference.path);
            let actual_bytes = std::fs::metadata(&identity_path)
                .map_err(|err| GraphError::CorruptFile {
                    reason: format!("relationship identity artifact metadata read failed: {err}"),
                })?
                .len();
            if actual_bytes != reference.bytes {
                return Err(GraphError::CorruptFile {
                    reason: "relationship identity manifest byte count mismatch".to_string(),
                });
            }
            let dictionary = crate::projection::identity::read_manifest_identity_artifact(
                &identity_path,
                &reference.checksum,
                reference.bytes,
                reference.entry_count,
            )?;
            if dictionary.identities().len() != reference.entry_count as usize {
                return Err(GraphError::CorruptFile {
                    reason: "relationship identity manifest entry count mismatch".to_string(),
                });
            }
            if !dictionary
                .identities()
                .starts_with(&engine.relationship_identities)
            {
                return Err(GraphError::CorruptFile {
                    reason: "relationship identity artifact does not extend the base dictionary"
                        .to_string(),
                });
            }
            engine.relationship_identities = dictionary.identities().to_vec();
        }
        if !manifest.segments.is_empty() {
            engine.set_projection_mode(crate::config::ProjectionMode::MutableOverlay);
        }
        if projection_candidate.is_some() {
            engine.install_projection_candidate(&manifest, manifest_root)?;
        } else {
            engine.install_projection_manifest(&manifest, manifest_root)?;
        }
    }

    Ok(engine)
}

fn projection_workspace_bytes(
    root: &Path,
    manifest: &ProjectionManifest,
) -> GraphResult<crate::resource::ByteCount> {
    let segment_bytes = manifest
        .segments
        .iter()
        .map(|segment| root.join(&segment.path))
        .chain(
            manifest
                .base_chunks
                .iter()
                .map(|chunk| root.join(&chunk.path)),
        )
        .try_fold(0u64, |total, path| {
            let bytes = std::fs::metadata(&path)
                .map_err(|err| GraphError::CorruptFile {
                    reason: format!(
                        "projection artifact metadata read failed for {}: {err}",
                        path.display()
                    ),
                })?
                .len();
            total
                .checked_add(bytes)
                .ok_or_else(|| GraphError::CorruptFile {
                    reason: "projection artifact byte count overflowed".to_string(),
                })
        })?;
    let identity_bytes = manifest
        .relationship_identities
        .as_ref()
        .map_or(0, |identity| identity.bytes);
    // Decoding retains row vectors and derives forward/reverse hash maps. The
    // multiplier includes both representations plus allocator/hash overhead.
    let identity_workspace =
        identity_bytes
            .checked_mul(2)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: "projection identity workspace estimate overflowed".to_string(),
            })?;
    segment_bytes
        .checked_mul(8)
        .and_then(|bytes| bytes.checked_add(identity_workspace))
        .map(crate::resource::ByteCount::from_bytes)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "projection load workspace estimate overflowed".to_string(),
        })
}

fn reverse_csr_workspace_bytes(
    node_count: u32,
    edge_count: u32,
) -> GraphResult<crate::resource::ByteCount> {
    let offsets = u64::from(node_count)
        .checked_add(1)
        .and_then(|count| count.checked_mul(8));
    let edges = u64::from(edge_count).checked_mul(24);
    offsets
        .and_then(|offsets| edges.and_then(|edges| offsets.checked_add(edges)))
        .map(crate::resource::ByteCount::from_bytes)
        .ok_or_else(|| GraphError::Internal("reverse CSR load estimate overflowed".to_string()))
}

fn load_projection_manifest(
    path: &Path,
    artifact_crc: u32,
    manifest_root: &Path,
) -> GraphResult<Option<ProjectionManifest>> {
    let store = ProjectionManifestStore::new(manifest_root);
    let Some(manifest) = store.load_latest_current()? else {
        return Ok(None);
    };
    validate_projection_manifest_base(path, artifact_crc, &manifest)?;
    Ok(Some(manifest))
}

fn validate_projection_manifest_base(
    path: &Path,
    artifact_crc: u32,
    manifest: &ProjectionManifest,
) -> GraphResult<()> {
    if manifest.base_artifact_version != VERSION {
        return Err(GraphError::IncompatibleVersion(format!(
            "projection manifest references base artifact version {}; expected {}",
            manifest.base_artifact_version, VERSION
        )));
    }
    let expected_base = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| GraphError::Internal("graph artifact path has no file name".into()))?;
    if manifest.base_artifact_path != expected_base {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection manifest: base artifact '{}' does not match loaded artifact '{}'",
                manifest.base_artifact_path, expected_base
            ),
        });
    }
    let expected_checksum = graph_artifact_checksum(artifact_crc);
    if manifest.base_artifact_checksum != expected_checksum {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "projection manifest: base artifact checksum '{}' does not match loaded artifact '{}'",
                manifest.base_artifact_checksum, expected_checksum
            ),
        });
    }
    Ok(())
}

fn graph_artifact_checksum(crc: u32) -> String {
    format!("crc32:{crc:08x}")
}

pub(crate) fn graph_artifact_version() -> u32 {
    VERSION
}

pub(crate) fn graph_artifact_checksum_for_path(path: &Path) -> GraphResult<String> {
    let bytes = fs::read(path)
        .map_err(|err| GraphError::Internal(format!("read graph artifact checksum: {err}")))?;
    if bytes.len() < HEADER_SIZE {
        return Err(GraphError::CorruptFile {
            reason: "file too small for header".to_string(),
        });
    }
    if &bytes[0..4] != MAGIC {
        return Err(GraphError::CorruptFile {
            reason: "invalid magic bytes".to_string(),
        });
    }
    let stored_crc = read_u32_at(&bytes, CRC_OFFSET);
    let computed_crc = crc32fast::hash(&bytes[HEADER_SIZE..]);
    if stored_crc != computed_crc {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "CRC32 mismatch: stored={:#x}, computed={:#x}",
                stored_crc, computed_crc
            ),
        });
    }
    Ok(graph_artifact_checksum(computed_crc))
}

/// Resolve the graph root directory under `$PGDATA/{data_dir}/{graph_id}`.
///
/// The graph id must be canonical UUID text. Graph names are intentionally not
/// accepted here because filesystem paths must be derived from stable catalog
/// identity, not user-controlled display names.
fn graph_root_path_for_uncreated(graph_id: &str) -> GraphResult<PathBuf> {
    let graph_id = GraphId::parse(graph_id)
        .map_err(|err| GraphError::Internal(format!("invalid graph artifact id: {err}")))?;
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
    Ok(PathBuf::from(&pgdata).join(&subdir).join(graph_id.as_str()))
}

/// Get the graph root directory under `$PGDATA/{data_dir}/{graph_id}`.
///
/// The directory is created for callers that intend to write artifacts.
pub fn graph_root_path_for(graph_id: &str) -> GraphResult<PathBuf> {
    let dir = graph_root_path_for_uncreated(graph_id)?;
    fs::create_dir_all(&dir).map_err(|e| {
        GraphError::Internal(format!(
            "Cannot create graph data directory {}: {}",
            dir.display(),
            e
        ))
    })?;
    Ok(dir)
}

/// Get the `.pggraph` artifact path for a graph id.
pub fn graph_file_path_for(graph_id: &str) -> GraphResult<PathBuf> {
    Ok(graph_root_path_for(graph_id)?.join("main.pggraph"))
}

/// Resolve the `.pggraph` artifact path for a graph id without creating dirs.
pub(crate) fn graph_file_path_for_uncreated(graph_id: &str) -> GraphResult<PathBuf> {
    Ok(graph_root_path_for_uncreated(graph_id)?.join("main.pggraph"))
}

/// Get the selected graph's `.pggraph` file path.
///
/// Uses the `graph.data_dir` GUC (default: `graph`) and the selected graph id.
/// In pure Rust unit tests, where SPI graph selection is unavailable, this
/// resolves to the compatibility default graph.
pub fn graph_file_path() -> GraphResult<PathBuf> {
    graph_file_path_for(&selected_graph_id_for_paths()?)
}

/// Resolve the selected graph's `.pggraph` artifact path without creating dirs.
pub(crate) fn graph_file_path_uncreated() -> GraphResult<PathBuf> {
    graph_file_path_for_uncreated(&selected_graph_id_for_paths()?)
}

/// Get the sync checkpoint sidecar path for a graph id.
#[allow(
    dead_code,
    reason = "Phase 5 exposes explicit graph-id path helpers before all checkpoint call sites are graph-id explicit"
)]
pub fn sync_checkpoint_path_for(graph_id: &str) -> GraphResult<PathBuf> {
    Ok(sync_checkpoint_path(&graph_file_path_for(graph_id)?))
}

/// Get the projection manifest root for a graph id.
#[allow(
    dead_code,
    reason = "Phase 5 exposes explicit graph-id path helpers before all manifest call sites are graph-id explicit"
)]
pub fn projection_manifest_root_for(graph_id: &str) -> GraphResult<PathBuf> {
    Ok(projection_manifest_root(&graph_file_path_for(graph_id)?))
}

/// Remove all derived artifact files for one graph id.
///
/// The target root is derived from a validated graph UUID and the configured
/// data directory. No caller-provided path is accepted.
pub fn remove_graph_artifacts_for(graph_id: &str) -> GraphResult<()> {
    let graph_id = GraphId::parse(graph_id)
        .map_err(|err| GraphError::Internal(format!("invalid graph artifact id: {err}")))?;
    let root = graph_root_path_for_uncreated(graph_id.as_str())?;
    if root.file_name().and_then(|name| name.to_str()) != Some(graph_id.as_str()) {
        return Err(GraphError::Internal(format!(
            "refusing to remove graph artifact root outside graph id directory: {}",
            root.display()
        )));
    }
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|err| {
            GraphError::Internal(format!(
                "remove graph artifact directory {}: {err}",
                root.display()
            ))
        })?;
    }
    Ok(())
}

pub fn projection_manifest_root(path: &Path) -> PathBuf {
    path.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(any(not(test), feature = "pg_test"))]
fn selected_graph_id_for_paths() -> GraphResult<String> {
    crate::catalog::selected_or_default_graph_id_via_definer()
}

#[cfg(all(test, not(feature = "pg_test")))]
fn selected_graph_id_for_paths() -> GraphResult<String> {
    Ok(crate::graph_policy::DEFAULT_GRAPH_ID_TEXT.to_string())
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
    use crate::edge_store::{IdentifiedRawEdge, RawEdge, SortedEdgeStoreBuilder};
    use crate::filter_index::{FilterColumnType, PersistedFilterValue};
    use crate::projection::manifest::{
        ManifestSegmentRef, ProjectionManifest, ProjectionManifestStore,
    };
    use crate::projection::segment::{DeltaSegment, SegmentFilterValue, SegmentKind};
    use crate::types::{FilterCondition, FilterOp, TraversalDirection, TraversalStrategy};

    #[cfg(not(feature = "pg_test"))]
    use std::sync::Mutex;

    #[cfg(not(feature = "pg_test"))]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn mapped_layout_rejects_non_little_endian_targets() {
        assert!(ensure_native_mapped_layout_supported(true).is_ok());
        assert!(matches!(
            ensure_native_mapped_layout_supported(false),
            Err(GraphError::IncompatibleVersion(message))
                if message.contains("little-endian")
        ));
    }

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
        assert_eq!(projection_manifest_root(&path), PathBuf::from("/tmp/graph"));
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

        assert_eq!(
            path,
            pgdata
                .join("graph")
                .join(crate::graph_policy::DEFAULT_GRAPH_ID_TEXT)
                .join("main.pggraph")
        );
        assert!(path.parent().unwrap().exists());
        let _ = std::fs::remove_dir_all(&pgdata);
    }

    #[cfg(not(feature = "pg_test"))]
    #[test]
    fn graph_file_path_for_separates_graph_roots() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _restore = EnvRestore::capture("PGDATA");
        let pgdata = std::env::temp_dir().join(format!(
            "graph-pgdata-path-for-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::remove_dir_all(&pgdata);
        std::env::set_var("PGDATA", &pgdata);

        let graph_a = "00000000-0000-0000-0000-0000000000aa";
        let graph_b = "00000000-0000-0000-0000-0000000000bb";
        let path_a = graph_file_path_for(graph_a).unwrap();
        let path_b = graph_file_path_for(graph_b).unwrap();
        let checkpoint_a = sync_checkpoint_path_for(graph_a).unwrap();
        let manifest_root_a = projection_manifest_root_for(graph_a).unwrap();

        assert_eq!(
            path_a,
            pgdata.join("graph").join(graph_a).join("main.pggraph")
        );
        assert_eq!(
            path_b,
            pgdata.join("graph").join(graph_b).join("main.pggraph")
        );
        assert_eq!(
            checkpoint_a,
            pgdata.join("graph").join(graph_a).join("main.pggraph.sync")
        );
        assert_eq!(manifest_root_a, pgdata.join("graph").join(graph_a));
        assert_ne!(path_a.parent(), path_b.parent());
        let _ = std::fs::remove_dir_all(&pgdata);
    }

    #[cfg(not(feature = "pg_test"))]
    #[test]
    fn graph_file_path_for_rejects_non_uuid_path_input() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _restore = EnvRestore::capture("PGDATA");
        std::env::set_var("PGDATA", std::env::temp_dir());

        let result = graph_file_path_for("../not-a-uuid");

        assert!(
            matches!(result, Err(GraphError::Internal(message)) if message.contains("graph id"))
        );
    }

    #[cfg(not(feature = "pg_test"))]
    #[test]
    fn remove_graph_artifacts_for_missing_graph_does_not_create_root() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _restore = EnvRestore::capture("PGDATA");
        let pgdata = std::env::temp_dir().join(format!(
            "graph-pgdata-remove-missing-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::remove_dir_all(&pgdata);
        std::env::set_var("PGDATA", &pgdata);

        let graph_id = "00000000-0000-0000-0000-0000000000cc";

        remove_graph_artifacts_for(graph_id).unwrap();

        assert!(!pgdata.join("graph").join(graph_id).exists());
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
                schema_reversed: false,
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

        assert_eq!(loaded.node_store.primary_key(a), Some("A-1"));
        assert_eq!(loaded.node_store.primary_key(b), Some("B-2"));
        assert_eq!(loaded.resolve(10, "A-1"), Some(a));
        assert_eq!(loaded.resolve(10, "B-2"), Some(b));
        assert_eq!(loaded.edge_type_registry, vec!["", "officer_of"]);
        assert!(loaded.edge_store.has_weights());
        assert_eq!(loaded.edge_store.neighbors_weighted(a).2, &[7]);
        assert_eq!(reloaded.node_store.primary_key(a), Some("A-1"));
        assert_eq!(reloaded.node_store.primary_key(b), Some("B-2"));
        assert_eq!(reloaded.edge_type_registry, vec!["", "officer_of"]);
        assert_eq!(reloaded.edge_store.neighbors_weighted(a).2, &[7]);
    }

    #[test]
    fn loaded_snapshot_is_counted_and_survives_source_inode_truncation() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("snapshot-inode-isolation");
        write_graph_file(&engine, &path).unwrap();
        let artifact_mb = std::fs::metadata(&path).unwrap().len() as f64 / 1_048_576.0;

        let loaded = load_graph_file(&path).unwrap();
        assert!(loaded.estimated_memory_used_mb() >= artifact_mb);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(0)
            .unwrap();

        assert_eq!(loaded.node_store.primary_key(0), Some("A"));
        assert_eq!(loaded.resolve(10, "B"), Some(1));
        assert_eq!(loaded.edge_store.neighbors_weighted(0).0, &[1]);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn replacement_load_rejects_before_allocation_when_residency_exhausts_budget() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("load-residency-budget");
        write_graph_file(&engine, &path).expect("fixture writes");
        let resident =
            crate::resource::ByteCount::from_mib(2_048).expect("configured test memory fits");

        let error = match load_graph_file_with_residency(&path, resident) {
            Ok(_) => panic!("no private-memory headroom must reject the replacement"),
            Err(error) => error,
        };

        assert!(matches!(error, GraphError::ResourceLimit { .. }));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn persisted_mmap_load_preserves_schema_reversed_edges() {
        let mut engine = Engine::new();
        let a = engine.node_store.add_node(10, "A-1".to_string());
        let b = engine.node_store.add_node(10, "B-2".to_string());
        engine.resolution_insert(10, "A-1", a);
        engine.resolution_insert(10, "B-2", b);
        let edge_type = engine.register_edge_type("friend").unwrap();
        engine.edge_store = EdgeStore::from_edges(
            2,
            vec![
                RawEdge {
                    source: a,
                    target: b,
                    type_id: edge_type,
                    weight: Some(7),
                    schema_reversed: false,
                },
                RawEdge {
                    source: b,
                    target: a,
                    type_id: edge_type,
                    weight: Some(11),
                    schema_reversed: true,
                },
            ],
            true,
        );
        engine.built = true;

        let path = std::env::temp_dir().join(format!(
            "graph-persistence-schema-reversed-test-{}.pggraph",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        write_graph_file(&loaded, &path).unwrap();
        let reloaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let (_, _, loaded_schema, loaded_weights) =
            loaded.edge_store.neighbors_weighted_with_schema(b);
        assert_eq!(loaded_schema, &[1]);
        assert_eq!(loaded_weights, &[11]);
        assert_eq!(loaded.edge_store.schema_reversed_slice(), &[0, 1]);

        let (_, _, reloaded_schema, reloaded_weights) =
            reloaded.edge_store.neighbors_weighted_with_schema(b);
        assert_eq!(reloaded_schema, &[1]);
        assert_eq!(reloaded_weights, &[11]);
        assert_eq!(reloaded.edge_store.schema_reversed_slice(), &[0, 1]);
    }

    #[test]
    fn persisted_mmap_load_preserves_relationship_identity_metadata() {
        let engine = graph_with_identified_relationship();
        let path = temp_graph_path("relationship-identity-roundtrip");
        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        write_graph_file(&loaded, &path).unwrap();
        let reloaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(loaded.edge_store.relationship_ids_slice(), &[1]);
        assert_eq!(loaded.reverse_edge_store.relationship_ids_slice(), &[1]);
        assert_eq!(loaded.relationship_identities[0], None);
        assert_eq!(
            loaded.relationship_identities[1],
            Some(RelationshipIdentity {
                mapping_id: 42,
                source_key: "edge:100".to_string(),
            })
        );
        assert_eq!(reloaded.edge_store.relationship_ids_slice(), &[1]);
        assert_eq!(
            reloaded.relationship_identities[1],
            Some(RelationshipIdentity {
                mapping_id: 42,
                source_key: "edge:100".to_string(),
            })
        );
    }

    #[test]
    fn persisted_relationship_identity_allows_empty_source_key() {
        let mut engine = graph_with_identified_relationship();
        engine.relationship_identities[1] = Some(RelationshipIdentity {
            mapping_id: 42,
            source_key: String::new(),
        });
        let path = temp_graph_path("relationship-identity-empty-source-key");
        write_graph_file(&engine, &path).unwrap();

        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(
            loaded.relationship_identities[1],
            Some(RelationshipIdentity {
                mapping_id: 42,
                source_key: String::new(),
            })
        );
    }

    #[test]
    fn load_graph_file_rejects_empty_relationship_identity_slot() {
        let engine = graph_with_identified_relationship();
        let path = temp_graph_path("relationship-identity-empty-slot");
        write_graph_file(&engine, &path).unwrap();
        rewrite_edge_metadata_section(
            &path,
            &PersistedEdgeMetadata {
                edge_type_registry: engine.edge_type_registry.clone(),
                relationship_ids: vec![1],
                relationship_identities: vec![None, None],
            },
        );

        let err = match load_graph_file(&path) {
            Ok(_) => panic!("malformed relationship dictionary must fail closed"),
            Err(err) => err,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(
            matches!(err, GraphError::CorruptFile { reason } if reason.contains("empty dictionary slot"))
        );
    }

    #[test]
    fn persisted_load_preserves_projection_mode_sidecar() {
        let mut engine = Engine::new();
        engine.built = true;
        engine.set_projection_mode(crate::config::ProjectionMode::MutableOverlay);

        let path = std::env::temp_dir().join(format!(
            "graph-projection-mode-test-{}.pggraph",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(sync_checkpoint_path(&path));
        let _ = std::fs::remove_file(projection_mode_path(&path));

        write_graph_file(&engine, &path).unwrap();
        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(sync_checkpoint_path(&path));
        let _ = std::fs::remove_file(projection_mode_path(&path));

        assert_eq!(
            loaded.projection_mode,
            crate::config::ProjectionMode::MutableOverlay
        );
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

        let loaded_status = loaded
            .filter_index
            .find_first_column_by_name("status")
            .unwrap();
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
    fn persisted_load_rejects_malformed_filter_dictionary_layout() {
        let mut engine = graph_with_relationship();
        let column = engine.filter_index.register_typed_column(
            10,
            "status".to_string(),
            FilterColumnType::Text,
            engine.node_store.node_count() as usize,
        );
        engine.filter_index.intern_text_value(column, "open");
        engine
            .filter_index
            .corrupt_reverse_text_dictionary_for_test();

        let path = temp_graph_path("malformed-filter-dictionary");
        write_graph_file(&engine, &path).expect("malformed fixture writes with a valid checksum");
        let err = match load_graph_file(&path) {
            Ok(_) => panic!("malformed filter dictionary must fail closed"),
            Err(err) => err,
        };
        let _ = std::fs::remove_dir_all(path.parent().expect("artifact parent"));

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn persisted_load_applies_exact_typed_filter_segment_values() {
        let mut engine = Engine::new();
        let node = engine.node_store.add_node(10, "A-1".to_string());
        engine.resolution_insert(10, "A-1", node);
        engine.edge_store = EdgeStore::from_edges(1, vec![], false);
        let columns = [
            ("numeric", FilterColumnType::Numeric),
            ("boolean", FilterColumnType::Boolean),
            ("text", FilterColumnType::Text),
            ("date", FilterColumnType::Date),
            ("timestamptz", FilterColumnType::Timestamptz),
            ("uuid", FilterColumnType::Uuid),
            ("nullable", FilterColumnType::Numeric),
            ("deleted", FilterColumnType::Numeric),
        ];
        for (name, column_type) in columns {
            engine
                .filter_index
                .register_typed_column(10, name.to_string(), column_type, 1);
        }
        engine.filter_index.set_encoded_value(
            7,
            node,
            Some(crate::filter_index::EncodedFilterValue::Numeric(99)),
        );
        engine.built = true;

        let path = temp_graph_path("typed-filter-segment-reload");
        write_graph_file(&engine, &path).expect("base graph writes");
        let root = projection_manifest_root(&path);
        let segment_path = root.join("typed-filter.pggraph-delta");
        let mut segment = DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 1, 1)
            .expect("node segment constructs");
        let values = [
            PersistedFilterValue::Numeric(i64::MIN + 1),
            PersistedFilterValue::Boolean(true),
            PersistedFilterValue::Text("durable 🧪 filter".to_string()),
            PersistedFilterValue::Date(-20_000),
            PersistedFilterValue::Timestamptz(4_102_444_800_123_456),
            PersistedFilterValue::Uuid(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef),
            PersistedFilterValue::Null,
            PersistedFilterValue::Numeric(99),
        ];
        for (column_id, value) in values.into_iter().enumerate() {
            segment.filters.push(SegmentFilterValue {
                node_idx: node,
                column_id: column_id as u32,
                value,
                tombstone: column_id == 7,
            });
        }
        let segment_bytes = segment.to_bytes().expect("typed filter segment encodes");
        std::fs::write(&segment_path, &segment_bytes).expect("typed filter segment writes");

        let mut manifest = ProjectionManifest::base_only(
            2,
            path.file_name().expect("base name").to_string_lossy(),
            checksum_graph_artifact(&path),
            VERSION,
            1,
            1,
        );
        manifest.segments.push(ManifestSegmentRef {
            path: "typed-filter.pggraph-delta".to_string(),
            checksum: format!("crc32:{:08x}", crc32fast::hash(&segment_bytes)),
            level: 0,
            source_start: 0,
            source_end: 1,
            sync_watermark: 1,
        });
        publish_manifest(root, manifest);

        let loaded = load_graph_file(&path).expect("typed filter projection reloads");
        let text_column = loaded
            .filter_index
            .find_first_column_by_name("text")
            .expect("text column");
        let text_token = loaded
            .filter_index
            .lookup_text_value(text_column, "durable 🧪 filter")
            .expect("restored text is interned");
        let checks = [
            ("numeric", FilterCondition::EqI64(i64::MIN + 1)),
            ("boolean", FilterCondition::EqBool(true)),
            ("text", FilterCondition::EqToken(text_token)),
            ("date", FilterCondition::EqI64(-20_000)),
            ("timestamptz", FilterCondition::EqI64(4_102_444_800_123_456)),
            (
                "uuid",
                FilterCondition::EqUuid(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef),
            ),
            ("nullable", FilterCondition::IsNull),
            ("deleted", FilterCondition::IsNull),
        ];
        for (name, condition) in checks {
            let column = loaded
                .filter_index
                .find_first_column_by_name(name)
                .expect("restored column exists");
            assert!(loaded
                .filter_index
                .check_filter(node, &FilterOp::new(column, condition)));
        }
        let _ = std::fs::remove_dir_all(path.parent().expect("artifact parent"));
    }

    #[test]
    fn engine_loads_base_only_projection_manifest() {
        let mut engine = graph_with_relationship();
        engine.record_applied_sync_id(41);
        let path = temp_graph_path("base-manifest-load");
        write_graph_file(&engine, &path).unwrap();
        publish_base_manifest(&path, 7, 41);

        let loaded = load_graph_file(&path).unwrap();
        let status = loaded.base_projection_manifest_status();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(status, (Some(7), Some(41)));
    }

    #[test]
    fn engine_base_only_manifest_keeps_csr_neighbors_unchanged() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("base-manifest-csr");
        write_graph_file(&engine, &path).unwrap();
        publish_base_manifest(&path, 8, 0);

        let loaded = load_graph_file(&path).unwrap();
        let results = loaded
            .traverse(
                10,
                "A",
                1,
                100,
                100,
                None,
                None,
                None,
                TraversalStrategy::Bfs,
                TraversalDirection::Out,
            )
            .unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let ids = results
            .iter()
            .map(|result| result.node_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["A", "B"]);
    }

    #[test]
    fn engine_rejects_base_manifest_for_different_graph_artifact() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("base-manifest-wrong-base");
        write_graph_file(&engine, &path).unwrap();
        let other_path = path.with_file_name("other.pggraph");
        write_graph_file(&engine, &other_path).unwrap();
        publish_base_manifest(&other_path, 9, 0);

        let err = match load_graph_file(&path) {
            Ok(_) => panic!("wrong base manifest was accepted"),
            Err(err) => err,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(
            matches!(err, GraphError::CorruptFile { reason } if reason.contains("does not match loaded artifact"))
        );
    }

    #[test]
    fn engine_rejects_stale_base_manifest_checksum() {
        let mut engine = graph_with_relationship();
        let path = temp_graph_path("base-manifest-stale-checksum");
        write_graph_file(&engine, &path).unwrap();
        publish_base_manifest(&path, 10, 0);
        let c = engine.node_store.add_node(10, "C".to_string());
        engine.resolution_insert(10, "C", c);
        engine.edge_store = EdgeStore::from_edges(
            3,
            vec![RawEdge {
                source: 0,
                target: 1,
                type_id: 1,
                weight: Some(7),
                schema_reversed: false,
            }],
            true,
        );
        engine.reverse_edge_store = engine.edge_store.reversed();
        write_graph_file(&engine, &path).unwrap();

        let err = match load_graph_file(&path) {
            Ok(_) => panic!("stale base manifest checksum was accepted"),
            Err(err) => err,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(err, GraphError::CorruptFile { reason } if reason.contains("checksum")));
    }

    #[test]
    fn engine_rejects_base_manifest_with_wrong_artifact_version() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("base-manifest-wrong-version");
        write_graph_file(&engine, &path).unwrap();
        let checksum = checksum_graph_artifact(&path);
        publish_manifest(
            projection_manifest_root(&path),
            ProjectionManifest::base_only(
                11,
                path.file_name().unwrap().to_string_lossy(),
                checksum,
                VERSION + 1,
                0,
                1,
            ),
        );

        let err = match load_graph_file(&path) {
            Ok(_) => panic!("wrong-version base manifest was accepted"),
            Err(err) => err,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(err, GraphError::IncompatibleVersion(_)));
    }

    #[test]
    fn engine_loads_segment_backed_projection_manifest() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("base-manifest-segmented");
        write_graph_file(&engine, &path).unwrap();
        let root = projection_manifest_root(&path);
        let segment_path = root.join("segment.pggraph-delta");
        let segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 2, 0)
            .expect("segment constructs");
        let segment_bytes = segment.to_bytes().expect("segment encodes");
        std::fs::write(&segment_path, &segment_bytes).unwrap();
        let checksum = checksum_graph_artifact(&path);
        let mut manifest = ProjectionManifest::base_only(
            12,
            path.file_name().unwrap().to_string_lossy(),
            checksum,
            VERSION,
            0,
            1,
        );
        manifest.segments.push(ManifestSegmentRef {
            path: "segment.pggraph-delta".to_string(),
            checksum: format!("crc32:{:08x}", crc32fast::hash(&segment_bytes)),
            level: 0,
            source_start: 0,
            source_end: 2,
            sync_watermark: 0,
        });
        publish_manifest(root, manifest);

        let loaded = load_graph_file(&path).expect("segment-backed manifest loads");
        let status = loaded.base_projection_manifest_status();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(status, (Some(12), Some(0)));
    }

    #[test]
    fn graph_status_reports_base_manifest_generation() {
        let mut engine = Engine::new();
        let manifest =
            ProjectionManifest::base_only(11, "main.pggraph", "checksum", VERSION, 99, 1);

        engine
            .install_projection_manifest(&manifest, PathBuf::from("."))
            .expect("projection manifest installs");

        assert_eq!(
            engine.base_projection_manifest_status(),
            (Some(11), Some(99))
        );
    }

    #[test]
    fn graph_file_uses_launch_section_layout() {
        let mut engine = Engine::new();
        engine.built = true;

        let path = temp_graph_path("launch-section-layout");
        write_graph_file(&engine, &path).unwrap();
        let active_offset = read_section_offset(&path, 0);
        let table_oids_offset = read_section_offset(&path, 1);
        let filter_offset = read_section_offset(&path, 10);
        let registry_offset = read_section_offset(&path, 11);
        let file_len = std::fs::metadata(&path).unwrap().len();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(NUM_SECTIONS, 12);
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
        let filter_offset = read_section_offset(&path, 10);
        let registry_offset = read_section_offset(&path, 11);
        let filter_payload_len = read_u32_from_file(&path, filter_offset) as u64;
        let file_len = std::fs::metadata(&path).unwrap().len();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(header_version, VERSION);
        assert_eq!(NUM_SECTIONS, 12);
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
                schema_reversed: false,
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
        assert_eq!(loaded.node_store.primary_key(0), Some("X"));
        assert_eq!(loaded.node_store.primary_key(1), Some("Y"));
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
        assert_eq!(loaded.node_store.primary_key(0), Some("node-0"));
        assert_eq!(loaded.node_store.primary_key(999), Some("node-999"));
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

    fn rewrite_edge_metadata_section(path: &Path, edge_metadata: &PersistedEdgeMetadata) {
        use std::io::{Read, Seek, Write};

        let section_start = read_section_offset(path, 11);
        let mut data = Vec::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        let section_end = data.len();
        let payload = bincode::serde::encode_to_vec(edge_metadata, bincode::config::standard())
            .expect("test metadata serializes");
        let replacement_len = 4 + payload.len();
        assert!(
            section_start as usize + replacement_len <= section_end,
            "replacement metadata must fit in the existing section"
        );

        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(section_start)).unwrap();
        file.write_all(&(payload.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&payload).unwrap();
        file.flush().unwrap();
        rewrite_crc(path);
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
                schema_reversed: false,
            }],
            true,
        );
        engine.built = true;
        engine
    }

    fn graph_with_identified_relationship() -> Engine {
        let mut engine = Engine::new();
        let a = engine.node_store.add_node(10, "A-1".to_string());
        let b = engine.node_store.add_node(10, "B-2".to_string());
        engine.resolution_insert(10, "A-1", a);
        engine.resolution_insert(10, "B-2", b);
        let edge_type = engine.register_edge_type("officer_of").unwrap();
        let mut builder = SortedEdgeStoreBuilder::new(2, true);
        builder
            .try_push_identified(IdentifiedRawEdge {
                edge: RawEdge {
                    source: a,
                    target: b,
                    type_id: edge_type,
                    weight: Some(7),
                    schema_reversed: false,
                },
                relationship_id: 1,
            })
            .unwrap();
        engine.edge_store = builder.finish();
        engine.relationship_identities = vec![
            None,
            Some(RelationshipIdentity {
                mapping_id: 42,
                source_key: "edge:100".to_string(),
            }),
        ];
        engine.built = true;
        engine
    }

    fn publish_base_manifest(path: &Path, generation_id: u64, sync_watermark: i64) {
        let base_name = path.file_name().unwrap().to_string_lossy();
        let checksum = checksum_graph_artifact(path);
        let manifest = ProjectionManifest::base_only(
            generation_id,
            base_name,
            checksum,
            VERSION,
            sync_watermark,
            1,
        );
        publish_manifest(projection_manifest_root(path), manifest);
    }

    fn publish_manifest(root: PathBuf, manifest: ProjectionManifest) {
        ProjectionManifestStore::new(root)
            .publish(&manifest)
            .expect("base manifest publishes");
    }

    fn checksum_graph_artifact(path: &Path) -> String {
        let data = std::fs::read(path).unwrap();
        graph_artifact_checksum(crc32fast::hash(&data[HEADER_SIZE..]))
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

        let filter_offset = read_section_offset(&path, 10);
        write_section_offset(&path, 11, filter_offset);

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

        let pk_offsets = read_section_offset(&path, 8);
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

        let pk_offsets = read_section_offset(&path, 8);
        write_u64_at(&path, pk_offsets + 16, 999);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_pk_offset_width_overflow() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-pk-offset-width");
        write_graph_file(&engine, &path).unwrap();

        let pk_offsets = read_section_offset(&path, 8);
        write_u64_at(&path, pk_offsets + 8, u64::MAX);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn persisted_pk_offset_rejects_values_wider_than_usize() {
        let too_wide = u64::from(u32::MAX) + 1;

        assert!(matches!(
            persisted_pk_offset_to_usize(too_wide, 1),
            Err(GraphError::CorruptFile { .. })
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_invalid_primary_key_utf8() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-pk-utf8");
        write_graph_file(&engine, &path).unwrap();

        let pk_bytes = read_section_offset(&path, 9);
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
