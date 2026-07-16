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
//! [Header]              — 512 bytes, encoded explicitly as little endian
//!   magic/version/header size/flags/counts/checksums
//!   section_descriptors[26] — 26 × { offset: u64, length: u64 }
//!
//! [Section 0: NodeStore.is_active]            — ceil(node_count / 8) bytes
//! [Section 1: NodeStore.table_oids]           — node_count × 4 bytes
//! [Sections 2..4]  primary-key offsets/bytes and forward CSR offsets
//! [Sections 5..9]  forward targets/types/direction/weights/relationship IDs
//! [Sections 10..15] inbound CSR with the same parallel arrays
//! [Section 16] resolution index
//! [Sections 17..19] filter catalog/data/text dictionaries
//! [Section 20] edge-type registry
//! [Sections 21..22] relationship identity descriptors/key bytes
//! [Section 23] sorted tenanted table OIDs
//! [Section 24] lexical tenant dictionary
//! [Section 25] dense per-node tenant tokens
//! ```
//!
//! ## Memory Model
//!
//! When loaded via `load_graph_file()`:
//! - **NodeStore** (`is_active`, `table_oids`, primary-key offsets/bytes):
//!   backed by a backend-local immutable mapping
//! - **Forward and inbound EdgeStore** arrays are backed by the same
//!   backend-local immutable mapping
//! - **ResolutionIndex**: mapped, zero-copy within the backend, binary search
//! - the bounded edge type registry is decoded into backend-local metadata
//! - relationship identity descriptors and key bytes remain mapped
//!
//! See: `docs/contributor_guide/memory-model.mdx`

use std::fs;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::FileExt;

use memmap2::{Mmap, MmapMut};

use crate::config;
use crate::edge_store::{EdgeStore, MmapEdgeArrayParts, MmapEdgeArrays};
use crate::engine::{Engine, MmapBackedGraph, MmapResolutionState};
use crate::filter_index::FilterIndex;
use crate::graph_policy::GraphId;
use crate::mapped_bytes::MappedBytes;
use crate::node_store::{MmapNodeArrayParts, MmapNodeArrays, NodeStore};
use crate::projection::manifest::{ProjectionManifest, ProjectionManifestStore};
use crate::relationship_identity_store::{
    MappedRelationshipIdentities, MappedRelationshipIdentityParts, RelationshipIdentityStore,
};
use crate::resolution_index::{ResolutionIndexBuilder, ENTRY_SIZE as RESOLUTION_ENTRY_SIZE};
use crate::safety::{GraphError, GraphResult};
use crate::tenant_store::{MappedTenantIndex, MappedTenantIndexParts};

/// Magic bytes for .pggraph files.
const MAGIC: &[u8; 4] = b"PGGH";
/// Current file format version.
const VERSION: u32 = 6;
/// Header size in bytes.
const HEADER_SIZE: usize = 512;
/// Number of sections.
const NUM_SECTIONS: usize = 26;
const SECTION_DESCRIPTORS_OFFSET: usize = 64;
const SECTION_DESCRIPTOR_SIZE: usize = 16;
const BODY_CRC_OFFSET: usize = 40;
const HEADER_CRC_OFFSET: usize = 44;
const SECTION_ALIGNMENT: usize = 64;
const FLAG_FORWARD_WEIGHTS: u32 = 1 << 0;
const FLAG_INBOUND_WEIGHTS: u32 = 1 << 1;
const FLAG_HAS_UNIDIRECTIONAL: u32 = 1 << 2;
const KNOWN_FLAGS: u32 = FLAG_FORWARD_WEIGHTS | FLAG_INBOUND_WEIGHTS | FLAG_HAS_UNIDIRECTIONAL;
#[cfg(test)]
const INTERRUPT_CHECK_INTERVAL: u32 = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SectionDescriptor {
    offset: u64,
    len: u64,
}

/// Fully validated fixed-width section layout for one graph artifact.
#[derive(Clone, Debug)]
struct ValidatedGraphLayout {
    ranges: [(usize, usize); NUM_SECTIONS],
    node_count: u32,
    forward_edge_count: u32,
    inbound_edge_count: u32,
    has_forward_weights: bool,
    has_inbound_weights: bool,
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
                pk_offsets_range: ranges[2].0
                    ..ranges[2].0
                        + (self.layout.node_count as usize + 1) * std::mem::size_of::<u64>(),
                pk_bytes_range: ranges[3].0..ranges[3].1,
                node_count: self.layout.node_count,
            },
            &self.token,
        )
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "invalid mmap node section metadata".to_string(),
        })
    }

    fn edge_arrays(&self, inbound: bool) -> GraphResult<MmapEdgeArrays> {
        let ranges = &self.layout.ranges;
        let (base, edge_count, has_weights) = if inbound {
            (
                10,
                self.layout.inbound_edge_count,
                self.layout.has_inbound_weights,
            )
        } else {
            (
                4,
                self.layout.forward_edge_count,
                self.layout.has_forward_weights,
            )
        };
        let weights_range = has_weights.then(|| ranges[base + 4].0..ranges[base + 4].1);
        MmapEdgeArrays::new_for_artifact(
            MmapEdgeArrayParts {
                mmap: MappedBytes::from_mmap(Arc::clone(&self.mmap)),
                offsets_range: ranges[base].0
                    ..ranges[base].0
                        + (self.layout.node_count as usize + 1) * std::mem::size_of::<u32>(),
                targets_range: ranges[base + 1].0..ranges[base + 1].1,
                type_ids_range: ranges[base + 2].0..ranges[base + 2].1,
                schema_reversed_range: ranges[base + 3].0..ranges[base + 3].1,
                weights_range,
                relationship_ids_range: ranges[base + 5].0..ranges[base + 5].1,
                node_count: self.layout.node_count,
                edge_count,
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

#[cfg(test)]
fn check_for_interrupts() {
    crate::resource::check_postgres_interrupts();
}

struct GraphArtifactWriter {
    writer: BufWriter<fs::File>,
    sections: [SectionDescriptor; NUM_SECTIONS],
    active_section: Option<usize>,
    position: u64,
    hasher: crc32fast::Hasher,
    #[cfg(test)]
    check_interrupts: bool,
}

/// Header metadata for an artifact assembled from bounded build streams.
///
/// The direct build path supplies only graph counts and feature flags. The
/// persistence module remains the single owner of the v6 header, alignment,
/// checksum, and atomic-publication format.
pub(crate) struct DirectArtifactMetadata {
    pub(crate) node_count: u32,
    pub(crate) forward_edge_count: u32,
    pub(crate) inbound_edge_count: u32,
    pub(crate) has_forward_weights: bool,
    pub(crate) has_inbound_weights: bool,
    pub(crate) has_unidirectional_edges: bool,
    pub(crate) applied_sync_id: i64,
    pub(crate) projection_mode: config::ProjectionMode,
}

/// Section-oriented sink used by the bounded persisted builder.
///
/// Section payloads are written incrementally; this type deliberately does
/// not expose the underlying file or header representation.
pub(crate) struct DirectArtifactWriter {
    inner: GraphArtifactWriter,
    next_section: usize,
    expected_lengths: [u64; NUM_SECTIONS],
    active_written: u64,
    positioned_section: bool,
}

struct DirectCandidateGuard<'a> {
    path: &'a Path,
    keep: bool,
}

impl Drop for DirectCandidateGuard<'_> {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = fs::remove_file(self.path);
        let sync = sync_checkpoint_path(self.path);
        let mode = projection_mode_path(self.path);
        let _ = fs::remove_file(&sync);
        let _ = fs::remove_file(append_path_suffix(&sync, ".tmp"));
        let _ = fs::remove_file(&mode);
        let _ = fs::remove_file(append_path_suffix(&mode, ".tmp"));
    }
}

impl DirectArtifactWriter {
    /// Begin the next v6 section in canonical numeric order.
    pub(crate) fn begin_section(&mut self, section: usize) -> GraphResult<()> {
        self.validate_active_length()?;
        if section != self.next_section || section >= NUM_SECTIONS {
            return Err(GraphError::Internal(format!(
                "direct artifact sections must be emitted in order: expected {}, found {section}",
                self.next_section
            )));
        }
        self.inner.begin_section(section)?;
        self.next_section += 1;
        self.active_written = 0;
        self.positioned_section = false;
        Ok(())
    }

    /// Append raw bytes to the active section.
    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) -> GraphResult<()> {
        if self.next_section == 0 {
            return Err(GraphError::Internal(
                "direct artifact write has no active section".into(),
            ));
        }
        if self.positioned_section {
            return Err(GraphError::Internal(
                "cannot append sequential bytes after positioned section writes".into(),
            ));
        }
        let written = self
            .active_written
            .checked_add(u64::try_from(bytes.len()).map_err(|_| {
                GraphError::Internal("direct artifact section length exceeds u64".into())
            })?)
            .ok_or_else(|| {
                GraphError::Internal("direct artifact section length overflowed".into())
            })?;
        let active = self.next_section - 1;
        if written > self.expected_lengths[active] {
            return Err(GraphError::Internal(format!(
                "direct artifact section {active} exceeded planned length {}",
                self.expected_lengths[active]
            )));
        }
        self.inner.write_body(bytes)?;
        self.active_written = written;
        Ok(())
    }

    /// Write bytes at a checked relative offset in the active preplanned
    /// section. The candidate is already fully zero-filled, so omitted ranges
    /// retain their canonical zero/default representation.
    #[cfg(unix)]
    pub(crate) fn write_at(&mut self, relative_offset: u64, bytes: &[u8]) -> GraphResult<()> {
        let active = self
            .next_section
            .checked_sub(1)
            .ok_or_else(|| GraphError::Internal("positioned write has no active section".into()))?;
        let len = u64::try_from(bytes.len())
            .map_err(|_| GraphError::Internal("positioned write length exceeds u64".into()))?;
        let end = relative_offset
            .checked_add(len)
            .ok_or_else(|| GraphError::Internal("positioned write range overflowed".into()))?;
        if end > self.expected_lengths[active] {
            return Err(GraphError::Internal(format!(
                "positioned write exceeds section {active} planned length {}",
                self.expected_lengths[active]
            )));
        }
        if !self.positioned_section {
            self.inner.writer.flush().map_err(|error| {
                GraphError::Internal(format!("Positioned section flush failed: {error}"))
            })?;
            let section_end = self.inner.sections[active]
                .offset
                .checked_add(self.expected_lengths[active])
                .ok_or_else(|| GraphError::Internal("positioned section end overflowed".into()))?;
            self.inner
                .writer
                .seek(SeekFrom::Start(section_end))
                .map_err(|error| {
                    GraphError::Internal(format!("Positioned section seek failed: {error}"))
                })?;
            self.inner.position = section_end;
            self.active_written = self.expected_lengths[active];
            self.positioned_section = true;
        }
        let absolute = self.inner.sections[active]
            .offset
            .checked_add(relative_offset)
            .ok_or_else(|| GraphError::Internal("positioned write offset overflowed".into()))?;
        self.inner
            .writer
            .get_ref()
            .write_all_at(bytes, absolute)
            .map_err(|error| GraphError::Internal(format!("Positioned write failed: {error}")))
    }

    fn validate_active_length(&self) -> GraphResult<()> {
        let Some(active) = self.next_section.checked_sub(1) else {
            return Ok(());
        };
        if self.active_written != self.expected_lengths[active] {
            return Err(GraphError::Internal(format!(
                "direct artifact section {active} wrote {} bytes, expected {}",
                self.active_written, self.expected_lengths[active]
            )));
        }
        Ok(())
    }
}

impl GraphArtifactWriter {
    fn new(file: fs::File, _check_interrupts: bool) -> GraphResult<Self> {
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&[0u8; HEADER_SIZE])
            .map_err(|e| GraphError::Internal(format!("Header reservation failed: {}", e)))?;
        Ok(Self {
            writer,
            sections: [SectionDescriptor::default(); NUM_SECTIONS],
            active_section: None,
            position: HEADER_SIZE as u64,
            hasher: crc32fast::Hasher::new(),
            #[cfg(test)]
            check_interrupts: _check_interrupts,
        })
    }

    fn begin_section(&mut self, section: usize) -> GraphResult<()> {
        self.finish_active_section()?;
        self.align(SECTION_ALIGNMENT)?;
        self.sections[section].offset = self.position;
        self.active_section = Some(section);
        Ok(())
    }

    fn finish_active_section(&mut self) -> GraphResult<()> {
        let Some(section) = self.active_section.take() else {
            return Ok(());
        };
        self.sections[section].len = self
            .position
            .checked_sub(self.sections[section].offset)
            .ok_or_else(|| GraphError::Internal("artifact section length underflow".into()))?;
        Ok(())
    }

    fn align(&mut self, alignment: usize) -> GraphResult<()> {
        let position = usize::try_from(self.position)
            .map_err(|_| GraphError::Internal("artifact too large".into()))?;
        let padding = (alignment - (position % alignment)) % alignment;
        if padding > 0 {
            self.write_body(&[0u8; SECTION_ALIGNMENT][..padding])?;
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

    #[cfg(test)]
    fn write_u32_values(&mut self, values: &[u32]) -> GraphResult<()> {
        for (idx, &value) in values.iter().enumerate() {
            if self.check_interrupts && (idx as u32).is_multiple_of(INTERRUPT_CHECK_INTERVAL) {
                check_for_interrupts();
            }
            self.write_body(&value.to_le_bytes())?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn write_u64_value(&mut self, value: u64) -> GraphResult<()> {
        self.write_body(&value.to_le_bytes())
    }

    #[cfg(test)]
    fn write_u32_value(&mut self, value: u32) -> GraphResult<()> {
        self.write_body(&value.to_le_bytes())
    }

    #[cfg(test)]
    fn finish(
        self,
        node_count: u32,
        forward_edge_count: u32,
        inbound_edge_count: u32,
        flags: u32,
    ) -> GraphResult<fs::File> {
        self.finish_internal(
            node_count,
            forward_edge_count,
            inbound_edge_count,
            flags,
            None,
        )
    }

    fn finish_recomputed(
        self,
        node_count: u32,
        forward_edge_count: u32,
        inbound_edge_count: u32,
        flags: u32,
        governor: &crate::resource::ResourceGovernor,
    ) -> GraphResult<fs::File> {
        self.finish_internal(
            node_count,
            forward_edge_count,
            inbound_edge_count,
            flags,
            Some(governor),
        )
    }

    fn finish_internal(
        mut self,
        node_count: u32,
        forward_edge_count: u32,
        inbound_edge_count: u32,
        flags: u32,
        recompute: Option<&crate::resource::ResourceGovernor>,
    ) -> GraphResult<fs::File> {
        self.finish_active_section()?;
        let body_len = self
            .position
            .checked_sub(HEADER_SIZE as u64)
            .ok_or_else(|| GraphError::Internal("artifact body length underflow".into()))?;
        let body_crc = if let Some(governor) = recompute {
            self.writer.flush().map_err(|error| {
                GraphError::Internal(format!("Artifact flush before checksum failed: {error}"))
            })?;
            let file = self.writer.get_mut();
            file.seek(SeekFrom::Start(HEADER_SIZE as u64))
                .map_err(|error| {
                    GraphError::Internal(format!("Artifact checksum seek failed: {error}"))
                })?;
            let mut remaining = body_len;
            let mut buffer = [0_u8; 64 * 1024];
            let mut hasher = crc32fast::Hasher::new();
            while remaining > 0 {
                let requested =
                    usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
                        GraphError::Internal("Artifact checksum size overflowed".into())
                    })?;
                file.read_exact(&mut buffer[..requested]).map_err(|error| {
                    GraphError::Internal(format!("Artifact checksum read failed: {error}"))
                })?;
                hasher.update(&buffer[..requested]);
                remaining -= requested as u64;
                governor
                    .check_elapsed(crate::resource::ResourcePhase::Persistence)
                    .map_err(crate::safety::resource_limit_error)?;
                crate::resource::check_postgres_interrupts();
            }
            hasher.finalize()
        } else {
            std::mem::replace(&mut self.hasher, crc32fast::Hasher::new()).finalize()
        };
        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(MAGIC);
        header[4..8].copy_from_slice(&VERSION.to_le_bytes());
        header[8..12].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        header[12..16].copy_from_slice(&flags.to_le_bytes());
        header[16..20].copy_from_slice(&node_count.to_le_bytes());
        header[20..24].copy_from_slice(&forward_edge_count.to_le_bytes());
        header[24..28].copy_from_slice(&inbound_edge_count.to_le_bytes());
        header[28..32].copy_from_slice(&(NUM_SECTIONS as u32).to_le_bytes());
        header[32..40].copy_from_slice(&body_len.to_le_bytes());
        header[BODY_CRC_OFFSET..BODY_CRC_OFFSET + 4].copy_from_slice(&body_crc.to_le_bytes());
        for (i, descriptor) in self.sections.iter().enumerate() {
            let start = SECTION_DESCRIPTORS_OFFSET + i * SECTION_DESCRIPTOR_SIZE;
            header[start..start + 8].copy_from_slice(&descriptor.offset.to_le_bytes());
            header[start + 8..start + 16].copy_from_slice(&descriptor.len.to_le_bytes());
        }
        let header_crc = crc32fast::hash(&header);
        header[HEADER_CRC_OFFSET..HEADER_CRC_OFFSET + 4].copy_from_slice(&header_crc.to_le_bytes());

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
    forward_edge_count: u32,
    inbound_edge_count: u32,
) -> GraphResult<()> {
    validate_csr_contents(mmap, ranges, 4, node_count, forward_edge_count, "forward")?;
    validate_csr_contents(mmap, ranges, 10, node_count, inbound_edge_count, "inbound")?;

    let pk_offsets_start = ranges[2].0;
    let pk_bytes_len = ranges[3].1 - ranges[3].0;
    let mut previous_pk = read_u64_at(mmap, pk_offsets_start);
    if previous_pk != 0 {
        return Err(GraphError::CorruptFile {
            reason: format!("primary_key_offsets[0] must be 0, found {previous_pk}"),
        });
    }
    for idx in 1..=node_count as usize {
        let current_raw = read_u64_at(mmap, pk_offsets_start + idx * 8);
        if current_raw < previous_pk {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "primary_key_offsets are not monotonic at index {idx}: {current_raw} < {previous_pk}"
                ),
            });
        }
        let current = persisted_pk_offset_to_usize(current_raw, idx)?;
        if current > pk_bytes_len {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "primary_key_offsets[{idx}] exceeds primary key bytes: {current} > {pk_bytes_len}"
                ),
            });
        }
        let start = persisted_pk_offset_to_usize(previous_pk, idx - 1)?;
        std::str::from_utf8(&mmap[ranges[3].0 + start..ranges[3].0 + current]).map_err(|err| {
            GraphError::CorruptFile {
                reason: format!(
                    "primary key at node index {} is not valid UTF-8: {err}",
                    idx - 1
                ),
            }
        })?;
        previous_pk = current_raw;
    }
    if persisted_pk_offset_to_usize(previous_pk, node_count as usize)? != pk_bytes_len {
        return Err(GraphError::CorruptFile {
            reason: "final primary-key offset does not equal the primary-key byte length".into(),
        });
    }

    validate_resolution_contents(mmap, ranges, node_count)?;

    Ok(())
}

fn validate_csr_contents(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    base: usize,
    node_count: u32,
    edge_count: u32,
    label: &str,
) -> GraphResult<()> {
    let edge_offsets_start = ranges[base].0;
    let mut previous = read_u32_at(mmap, edge_offsets_start);
    if previous != 0 {
        return Err(GraphError::CorruptFile {
            reason: format!("{label} edge_offsets[0] must be 0, found {previous}"),
        });
    }
    for idx in 1..=node_count as usize {
        let current = read_u32_at(mmap, edge_offsets_start + idx * 4);
        if current < previous {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "{label} edge_offsets are not monotonic at index {idx}: {current} < {previous}"
                ),
            });
        }
        if current > edge_count {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "{label} edge_offsets[{idx}] exceeds edge_count: {current} > {edge_count}"
                ),
            });
        }
        previous = current;
    }
    if previous != edge_count {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "final {label} edge offset must equal edge_count: {previous} != {edge_count}"
            ),
        });
    }

    let targets_start = ranges[base + 1].0;
    for idx in 0..edge_count as usize {
        let target = read_u32_at(mmap, targets_start + idx * 4);
        if target >= node_count {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "{label} target at index {idx} exceeds node_count: {target} >= {node_count}"
                ),
            });
        }
    }

    for source in 0..node_count as usize {
        let start = read_u32_at(mmap, edge_offsets_start + source * 4) as usize;
        let end = read_u32_at(mmap, edge_offsets_start + (source + 1) * 4) as usize;
        if (start + 1..end).any(|idx| {
            edge_topology(mmap, ranges, base, idx - 1) > edge_topology(mmap, ranges, base, idx)
        }) {
            return Err(GraphError::CorruptFile {
                reason: format!("{label} adjacency is not in canonical CSR order"),
            });
        }
    }

    for &flag in &mmap[ranges[base + 3].0..ranges[base + 3].1] {
        if flag > 1 {
            return Err(GraphError::CorruptFile {
                reason: format!("{label} schema_reversed flag must be 0 or 1, found {flag}"),
            });
        }
    }
    Ok(())
}

fn edge_topology(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    base: usize,
    index: usize,
) -> (u32, u8, u8) {
    (
        read_u32_at(mmap, ranges[base + 1].0 + index * 4),
        mmap[ranges[base + 2].0 + index],
        mmap[ranges[base + 3].0 + index],
    )
}

fn validate_csr_reverse_equivalence(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    node_count: u32,
    edge_count: u32,
    has_weights: bool,
) -> GraphResult<()> {
    let forward_offsets = ranges[4].0;
    let inbound_offsets = ranges[10].0;
    for inbound_source in 0..node_count as usize {
        let inbound_start = read_u32_at(mmap, inbound_offsets + inbound_source * 4) as usize;
        let inbound_end = read_u32_at(mmap, inbound_offsets + (inbound_source + 1) * 4) as usize;
        let mut inbound_index = inbound_start;
        while inbound_index < inbound_end {
            let (forward_source, type_id, schema_reversed) =
                edge_topology(mmap, ranges, 10, inbound_index);
            let topology = (inbound_source as u32, type_id, schema_reversed);
            let mut inbound_group_end = inbound_index + 1;
            while inbound_group_end < inbound_end
                && edge_topology(mmap, ranges, 10, inbound_group_end)
                    == (forward_source, type_id, schema_reversed)
            {
                inbound_group_end += 1;
            }

            let forward_start =
                read_u32_at(mmap, forward_offsets + forward_source as usize * 4) as usize;
            let forward_end =
                read_u32_at(mmap, forward_offsets + (forward_source as usize + 1) * 4) as usize;
            let forward_group_start =
                lower_bound_topology(mmap, ranges, 4, forward_start, forward_end, topology);
            let forward_group_end =
                upper_bound_topology(mmap, ranges, 4, forward_group_start, forward_end, topology);
            if forward_group_end - forward_group_start != inbound_group_end - inbound_index {
                return Err(GraphError::CorruptFile {
                    reason: "forward and inbound CSR topology counts differ".into(),
                });
            }
            for offset in 0..inbound_group_end - inbound_index {
                let forward_idx = forward_group_start + offset;
                let inbound_idx = inbound_index + offset;
                let forward_weight =
                    has_weights.then(|| read_u32_at(mmap, ranges[8].0 + forward_idx * 4));
                let inbound_weight =
                    has_weights.then(|| read_u32_at(mmap, ranges[14].0 + inbound_idx * 4));
                let forward_id = read_u32_at(mmap, ranges[9].0 + forward_idx * 4);
                let inbound_id = read_u32_at(mmap, ranges[15].0 + inbound_idx * 4);
                if forward_weight != inbound_weight || forward_id != inbound_id {
                    return Err(GraphError::CorruptFile {
                        reason: "forward and inbound CSR sidecars differ".into(),
                    });
                }
            }
            inbound_index = inbound_group_end;
        }
    }
    if read_u32_at(mmap, forward_offsets + node_count as usize * 4) != edge_count {
        return Err(GraphError::CorruptFile {
            reason: "forward and inbound CSR counts differ".into(),
        });
    }
    Ok(())
}

fn lower_bound_topology(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    base: usize,
    mut low: usize,
    mut high: usize,
    expected: (u32, u8, u8),
) -> usize {
    while low < high {
        let middle = low + (high - low) / 2;
        if edge_topology(mmap, ranges, base, middle) < expected {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn upper_bound_topology(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    base: usize,
    mut low: usize,
    mut high: usize,
    expected: (u32, u8, u8),
) -> usize {
    while low < high {
        let middle = low + (high - low) / 2;
        if edge_topology(mmap, ranges, base, middle) <= expected {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn validate_resolution_contents(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    node_count: u32,
) -> GraphResult<()> {
    let resolution = &mmap[ranges[16].0..ranges[16].1];
    let entry_count = read_u32_at(resolution, 0) as usize;
    if entry_count != node_count as usize {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "resolution entry count must equal node count: {entry_count} != {node_count}"
            ),
        });
    }
    let mut seen_nodes = Vec::new();
    seen_nodes
        .try_reserve_exact(node_count as usize)
        .map_err(|_| GraphError::Oom {
            used_mb: 0,
            need_mb: 1,
            limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
        })?;
    seen_nodes.resize(node_count as usize, false);
    let mut previous = None;
    for index in 0..entry_count {
        let offset = 4 + index * RESOLUTION_ENTRY_SIZE;
        let table_oid = read_u32_at(resolution, offset);
        let pk_hash = read_u64_at(resolution, offset + 4);
        let node_idx = read_u32_at(resolution, offset + 12);
        if node_idx >= node_count {
            return Err(GraphError::CorruptFile {
                reason: format!("resolution entry {index} has an out-of-range node index"),
            });
        }
        if std::mem::replace(&mut seen_nodes[node_idx as usize], true) {
            return Err(GraphError::CorruptFile {
                reason: format!("resolution entry {index} duplicates node index {node_idx}"),
            });
        }
        let key = (table_oid, pk_hash);
        if previous.is_some_and(|previous| previous > key) {
            return Err(GraphError::CorruptFile {
                reason: "resolution entries are not sorted".into(),
            });
        }
        let persisted_oid = read_u32_at(mmap, ranges[1].0 + node_idx as usize * 4);
        let pk_start = persisted_pk_offset_to_usize(
            read_u64_at(mmap, ranges[2].0 + node_idx as usize * 8),
            node_idx as usize,
        )?;
        let pk_end = persisted_pk_offset_to_usize(
            read_u64_at(mmap, ranges[2].0 + (node_idx as usize + 1) * 8),
            node_idx as usize + 1,
        )?;
        let primary_key = std::str::from_utf8(&mmap[ranges[3].0 + pk_start..ranges[3].0 + pk_end])
            .map_err(|_| GraphError::CorruptFile {
                reason: format!("resolution entry {index} references invalid UTF-8"),
            })?;
        if table_oid != persisted_oid || pk_hash != ResolutionIndexBuilder::hash_pk(primary_key) {
            return Err(GraphError::CorruptFile {
                reason: format!("resolution entry {index} does not match its node identity"),
            });
        }
        previous = Some(key);
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
    sections: &[SectionDescriptor; NUM_SECTIONS],
    node_count: u32,
    forward_edge_count: u32,
    inbound_edge_count: u32,
    flags: u32,
) -> GraphResult<ValidatedGraphLayout> {
    if flags & !KNOWN_FLAGS != 0 {
        return Err(GraphError::CorruptFile {
            reason: format!("unsupported graph artifact flags {flags:#x}"),
        });
    }
    if forward_edge_count != inbound_edge_count
        || (flags & FLAG_FORWARD_WEIGHTS != 0) != (flags & FLAG_INBOUND_WEIGHTS != 0)
    {
        return Err(GraphError::CorruptFile {
            reason: "forward and inbound CSR counts or weight modes differ".into(),
        });
    }
    let mut ranges = [(0usize, 0usize); NUM_SECTIONS];
    let mut previous_end = HEADER_SIZE;
    for (i, descriptor) in sections.iter().enumerate() {
        let start = usize::try_from(descriptor.offset).map_err(|_| GraphError::CorruptFile {
            reason: format!("section offset {} does not fit in usize", i),
        })?;
        let len = usize::try_from(descriptor.len).map_err(|_| GraphError::CorruptFile {
            reason: format!("section length {i} does not fit in usize"),
        })?;
        let end = start
            .checked_add(len)
            .ok_or_else(|| GraphError::CorruptFile {
                reason: format!("section {i} end overflows usize"),
            })?;
        if !start.is_multiple_of(SECTION_ALIGNMENT) || start < previous_end || end > mmap.len() {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "invalid section {i}: offset={start}, len={len}, previous_end={previous_end}, file_len={}",
                    mmap.len()
                ),
            });
        }
        if mmap[previous_end..start].iter().any(|&byte| byte != 0) {
            return Err(GraphError::CorruptFile {
                reason: format!("nonzero alignment padding before section {i}"),
            });
        }
        ranges[i] = (start, end);
        previous_end = end;
    }
    if previous_end != mmap.len() {
        return Err(GraphError::CorruptFile {
            reason: "artifact contains trailing bytes after the final section".into(),
        });
    }

    let active_byte_count = (node_count as usize).div_ceil(8);
    let node_plus_one = node_count
        .checked_add(1)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "node_count overflow in edge offset section".to_string(),
        })?;
    let node_u32_bytes = checked_section_size(node_count, 4, "table_oids")?;
    let pk_offsets_bytes = checked_section_size(node_plus_one, 8, "primary_key_offsets")?;
    require_exact_section_len(&ranges, 0, active_byte_count, "is_active")?;
    require_exact_section_len(&ranges, 1, node_u32_bytes, "table_oids")?;
    require_exact_section_len(&ranges, 2, pk_offsets_bytes, "primary_key_offsets")?;
    validate_csr_section_lengths(
        &ranges,
        4,
        node_plus_one,
        forward_edge_count,
        flags & FLAG_FORWARD_WEIGHTS != 0,
        "forward",
    )?;
    validate_csr_section_lengths(
        &ranges,
        10,
        node_plus_one,
        inbound_edge_count,
        flags & FLAG_INBOUND_WEIGHTS != 0,
        "inbound",
    )?;

    let resolution = &mmap[ranges[16].0..ranges[16].1];
    if resolution.len() < 4 {
        return Err(GraphError::CorruptFile {
            reason: "invalid resolution index section".to_string(),
        });
    }
    let resolution_min_len = (read_u32_at(resolution, 0) as usize)
        .checked_mul(RESOLUTION_ENTRY_SIZE)
        .and_then(|bytes| bytes.checked_add(4))
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "resolution index length overflowed".into(),
        })?;
    require_exact_section_len(&ranges, 16, resolution_min_len, "resolution_index")?;

    validate_filter_sections(mmap, &ranges)?;
    validate_tenant_sections(mmap, &ranges, node_count)?;
    validate_persisted_contents(
        mmap,
        &ranges,
        node_count,
        forward_edge_count,
        inbound_edge_count,
    )?;
    validate_csr_reverse_equivalence(
        mmap,
        &ranges,
        node_count,
        forward_edge_count,
        flags & FLAG_FORWARD_WEIGHTS != 0,
    )?;

    Ok(ValidatedGraphLayout {
        ranges,
        node_count,
        forward_edge_count,
        inbound_edge_count,
        has_forward_weights: flags & FLAG_FORWARD_WEIGHTS != 0,
        has_inbound_weights: flags & FLAG_INBOUND_WEIGHTS != 0,
    })
}

fn validate_tenant_sections(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
    node_count: u32,
) -> GraphResult<()> {
    let oid_bytes = &mmap[ranges[23].0..ranges[23].1];
    if !oid_bytes.len().is_multiple_of(std::mem::size_of::<u32>()) {
        return Err(GraphError::CorruptFile {
            reason: "tenanted table OID section length is not a multiple of four".into(),
        });
    }
    let mut previous_oid = None;
    for index in 0..oid_bytes.len() / std::mem::size_of::<u32>() {
        let oid = read_u32_at(oid_bytes, index * std::mem::size_of::<u32>());
        if previous_oid.is_some_and(|previous| previous >= oid) {
            return Err(GraphError::CorruptFile {
                reason: "tenanted table OIDs are not strictly sorted".into(),
            });
        }
        previous_oid = Some(oid);
    }

    let dictionary = &mmap[ranges[24].0..ranges[24].1];
    if dictionary.len() < 12 {
        return Err(GraphError::CorruptFile {
            reason: "tenant dictionary header is truncated".into(),
        });
    }
    let tenant_count = read_u32_at(dictionary, 0);
    let offset_count = tenant_count
        .checked_add(1)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "tenant dictionary offset count overflowed".into(),
        })?;
    let payload_start = checked_section_size(offset_count, 8, "tenant dictionary offsets")?
        .checked_add(4)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "tenant dictionary header length overflowed".into(),
        })?;
    if payload_start > dictionary.len() || read_u64_at(dictionary, 4) != 0 {
        return Err(GraphError::CorruptFile {
            reason: "tenant dictionary has an invalid initial offset".into(),
        });
    }
    let payload = &dictionary[payload_start..];
    let mut previous_value: Option<&str> = None;
    let mut previous_end = 0usize;
    for index in 0..tenant_count as usize {
        let end = usize::try_from(read_u64_at(dictionary, 4 + (index + 1) * 8)).map_err(|_| {
            GraphError::CorruptFile {
                reason: "tenant dictionary offset exceeds usize".into(),
            }
        })?;
        if end < previous_end || end > payload.len() {
            return Err(GraphError::CorruptFile {
                reason: "tenant dictionary offsets are not monotonic and in bounds".into(),
            });
        }
        let value = std::str::from_utf8(&payload[previous_end..end]).map_err(|_| {
            GraphError::CorruptFile {
                reason: "tenant dictionary value is not valid UTF-8".into(),
            }
        })?;
        if previous_value.is_some_and(|previous| previous >= value) {
            return Err(GraphError::CorruptFile {
                reason: "tenant dictionary is not strictly lexical".into(),
            });
        }
        previous_value = Some(value);
        previous_end = end;
    }
    if previous_end != payload.len() {
        return Err(GraphError::CorruptFile {
            reason: "tenant dictionary contains unreferenced trailing bytes".into(),
        });
    }

    require_exact_section_len(
        ranges,
        25,
        checked_section_size(node_count, 4, "node tenant tokens")?,
        "node_tenant_tokens",
    )?;
    let table_oids = &mmap[ranges[1].0..ranges[1].1];
    let tokens = &mmap[ranges[25].0..ranges[25].1];
    for node_idx in 0..node_count as usize {
        let token = read_u32_at(tokens, node_idx * 4);
        if token > tenant_count {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "tenant token {token} at node {node_idx} exceeds dictionary count {tenant_count}"
                ),
            });
        }
        if token != 0 {
            let table_oid = read_u32_at(table_oids, node_idx * 4);
            if !sorted_u32_bytes_contains(oid_bytes, table_oid) {
                return Err(GraphError::CorruptFile {
                    reason: format!(
                        "tenant-scoped node {node_idx} belongs to unregistered table OID {table_oid}"
                    ),
                });
            }
        }
    }
    Ok(())
}

fn sorted_u32_bytes_contains(bytes: &[u8], expected: u32) -> bool {
    let mut left = 0usize;
    let mut right = bytes.len() / 4;
    while left < right {
        let middle = left + (right - left) / 2;
        match read_u32_at(bytes, middle * 4).cmp(&expected) {
            std::cmp::Ordering::Less => left = middle + 1,
            std::cmp::Ordering::Equal => return true,
            std::cmp::Ordering::Greater => right = middle,
        }
    }
    false
}

fn require_exact_section_len(
    ranges: &[(usize, usize); NUM_SECTIONS],
    section: usize,
    expected: usize,
    label: &str,
) -> GraphResult<()> {
    let actual = ranges[section].1 - ranges[section].0;
    if actual != expected {
        return Err(GraphError::CorruptFile {
            reason: format!("{label} section length must be {expected}, found {actual}"),
        });
    }
    Ok(())
}

fn validate_csr_section_lengths(
    ranges: &[(usize, usize); NUM_SECTIONS],
    base: usize,
    node_plus_one: u32,
    edge_count: u32,
    has_weights: bool,
    label: &str,
) -> GraphResult<()> {
    require_exact_section_len(
        ranges,
        base,
        checked_section_size(node_plus_one, 4, label)?,
        label,
    )?;
    require_exact_section_len(
        ranges,
        base + 1,
        checked_section_size(edge_count, 4, label)?,
        label,
    )?;
    require_exact_section_len(ranges, base + 2, edge_count as usize, label)?;
    require_exact_section_len(ranges, base + 3, edge_count as usize, label)?;
    let weight_len = if has_weights {
        checked_section_size(edge_count, 4, label)?
    } else {
        0
    };
    require_exact_section_len(ranges, base + 4, weight_len, label)?;
    require_exact_section_len(
        ranges,
        base + 5,
        checked_section_size(edge_count, 4, label)?,
        label,
    )
}

fn validate_filter_sections(
    mmap: &[u8],
    ranges: &[(usize, usize); NUM_SECTIONS],
) -> GraphResult<()> {
    if ranges[17].1 - ranges[17].0 < 16 {
        return Err(GraphError::CorruptFile {
            reason: "filter catalog header is truncated".into(),
        });
    }
    let _ = mmap;
    Ok(())
}

/// Write the engine state to a .pggraph file.
///
/// Uses atomic rename: writes to `<path>.tmp`, then renames to `path`.
#[cfg(test)]
pub fn write_graph_file(engine: &Engine, path: &Path) -> GraphResult<()> {
    write_graph_file_internal(engine, path, false, None)
}

/// Assemble one unpublished v6 candidate from bounded section streams without
/// constructing an owned [`Engine`].
///
/// The destination is created exclusively and is never overwritten. The
/// callback must emit all 26 sections in numeric order. A failed callback
/// removes only the candidate created by this invocation.
pub(crate) fn write_direct_graph_file(
    path: &Path,
    metadata: &DirectArtifactMetadata,
    section_lengths: [u64; NUM_SECTIONS],
    governor: &crate::resource::ResourceGovernor,
    emit: impl FnOnce(&mut DirectArtifactWriter) -> GraphResult<()>,
) -> GraphResult<()> {
    let planned_len = planned_direct_artifact_len(&section_lengths)?;
    let _candidate_disk = governor
        .reserve_disk(
            crate::resource::ResourcePhase::Persistence,
            crate::resource::ByteCount::from_bytes(planned_len),
        )
        .map_err(crate::safety::resource_limit_error)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            GraphError::Internal(format!(
                "Cannot create directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            GraphError::Internal(format!(
                "Cannot exclusively create {}: {error}",
                path.display()
            ))
        })?;
    let mut candidate = DirectCandidateGuard { path, keep: false };
    zero_fill_candidate(&mut file, planned_len, governor)?;
    file.sync_all().map_err(|error| {
        GraphError::Internal(format!("Candidate preallocation sync failed: {error}"))
    })?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| GraphError::Internal(format!("Candidate rewind failed: {error}")))?;
    let mut writer = DirectArtifactWriter {
        inner: GraphArtifactWriter::new(file, true)?,
        next_section: 0,
        expected_lengths: section_lengths,
        active_written: 0,
        positioned_section: false,
    };
    if let Err(error) = emit(&mut writer) {
        drop(writer);
        return Err(error);
    }
    if writer.next_section != NUM_SECTIONS {
        let emitted = writer.next_section;
        drop(writer);
        return Err(GraphError::Internal(format!(
            "direct artifact emitted {emitted} of {NUM_SECTIONS} sections"
        )));
    }
    writer.validate_active_length()?;
    let mut flags = 0;
    if metadata.has_forward_weights {
        flags |= FLAG_FORWARD_WEIGHTS;
    }
    if metadata.has_inbound_weights {
        flags |= FLAG_INBOUND_WEIGHTS;
    }
    if metadata.has_unidirectional_edges {
        flags |= FLAG_HAS_UNIDIRECTIONAL;
    }
    let file = writer.inner.finish_recomputed(
        metadata.node_count,
        metadata.forward_edge_count,
        metadata.inbound_edge_count,
        flags,
        governor,
    )?;
    file.sync_all()
        .map_err(|error| GraphError::Internal(format!("Sync failed: {error}")))?;
    write_sync_checkpoint(path, metadata.applied_sync_id)?;
    write_projection_mode(path, metadata.projection_mode)?;
    candidate.keep = true;
    Ok(())
}

fn planned_direct_artifact_len(lengths: &[u64; NUM_SECTIONS]) -> GraphResult<u64> {
    lengths
        .iter()
        .try_fold(HEADER_SIZE as u64, |position, length| {
            let remainder = position % SECTION_ALIGNMENT as u64;
            let padding = if remainder == 0 {
                0
            } else {
                SECTION_ALIGNMENT as u64 - remainder
            };
            position
                .checked_add(padding)
                .and_then(|position| position.checked_add(*length))
                .ok_or_else(|| GraphError::Internal("direct artifact layout exceeds u64".into()))
        })
}

fn zero_fill_candidate(
    file: &mut fs::File,
    len: u64,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<()> {
    static ZERO_CHUNK: [u8; 64 * 1024] = [0; 64 * 1024];
    let mut remaining = len;
    while remaining > 0 {
        let chunk = usize::try_from(remaining.min(ZERO_CHUNK.len() as u64))
            .map_err(|_| GraphError::Internal("candidate preallocation chunk overflowed".into()))?;
        file.write_all(&ZERO_CHUNK[..chunk]).map_err(|error| {
            GraphError::Internal(format!("Candidate preallocation failed: {error}"))
        })?;
        remaining -= chunk as u64;
        governor
            .check_elapsed(crate::resource::ResourcePhase::Persistence)
            .map_err(crate::safety::resource_limit_error)?;
        crate::resource::check_postgres_interrupts();
    }
    Ok(())
}

#[cfg(test)]
#[cfg(test)]
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

    writer.begin_section(0)?;
    reserve_persistence_workspace(
        &mut workspace,
        packed_active_bytes(engine.node_store.node_count())?,
    )?;
    let is_active_bytes = engine.node_store.is_active_bytes();
    let packed_active_len = (engine.node_store.node_count() as usize).div_ceil(8);
    let packed_active = is_active_bytes.get(..packed_active_len).ok_or_else(|| {
        GraphError::Internal("node active bitmap is shorter than the projected node count".into())
    })?;
    writer.write_body(packed_active)?;
    drop(is_active_bytes);
    release_persistence_workspace(&mut workspace);

    writer.begin_section(1)?;
    writer.write_u32_values(engine.node_store.table_oids_slice())?;

    writer.begin_section(2)?;
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

    writer.begin_section(3)?;
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

    write_edge_sections(&mut writer, 4, &engine.edge_store)?;
    let derived_inbound;
    let inbound_store = if engine.reverse_edge_store.node_count() == engine.node_store.node_count()
        && engine.reverse_edge_store.edge_count() == engine.edge_store.edge_count()
    {
        &engine.reverse_edge_store
    } else {
        derived_inbound = engine.edge_store.try_reversed()?;
        &derived_inbound
    };
    write_edge_sections(&mut writer, 10, inbound_store)?;

    writer.begin_section(16)?;
    reserve_persistence_workspace(
        &mut workspace,
        resolution_serialization_bytes(engine.node_store.node_count())?,
    )?;
    let ri_bytes = engine.resolution_to_bytes();
    writer.write_body(&ri_bytes)?;
    drop(ri_bytes);
    release_persistence_workspace(&mut workspace);

    let filter_workspace = engine
        .filter_index
        .v5_encoding_workspace_upper_bound(engine.node_store.node_count())?;
    reserve_persistence_workspace(
        &mut workspace,
        crate::resource::ByteCount::from_usize(filter_workspace).ok_or_else(|| {
            GraphError::Internal("filter encoding workspace exceeds u64 bytes".into())
        })?,
    )?;
    let (filter_catalog, filter_data, filter_dictionaries) = engine
        .filter_index
        .encode_v5_sections(engine.node_store.node_count())?;
    writer.begin_section(17)?;
    writer.write_body(&filter_catalog)?;
    writer.begin_section(18)?;
    writer.write_body(&filter_data)?;
    writer.begin_section(19)?;
    writer.write_body(&filter_dictionaries)?;
    drop((filter_catalog, filter_data, filter_dictionaries));
    release_persistence_workspace(&mut workspace);

    writer.begin_section(20)?;
    write_string_registry(&mut writer, &engine.edge_type_registry)?;

    writer.begin_section(21)?;
    write_relationship_identity_descriptors(&mut writer, &engine.relationship_identities)?;
    writer.begin_section(22)?;
    write_relationship_identity_keys(&mut writer, &engine.relationship_identities)?;

    reserve_persistence_workspace(&mut workspace, tenant_encoding_workspace(engine)?)?;
    write_tenant_sections(&mut writer, engine)?;
    release_persistence_workspace(&mut workspace);

    let mut flags = 0u32;
    if engine.edge_store.has_weights() {
        flags |= FLAG_FORWARD_WEIGHTS;
    }
    if inbound_store.has_weights() {
        flags |= FLAG_INBOUND_WEIGHTS;
    }
    if engine.has_unidirectional_edges {
        flags |= FLAG_HAS_UNIDIRECTIONAL;
    }

    let file = writer.finish(
        engine.node_store.node_count(),
        engine.edge_store.edge_count(),
        inbound_store.edge_count(),
        flags,
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

#[cfg(test)]
#[cfg(test)]
fn write_edge_sections(
    writer: &mut GraphArtifactWriter,
    base: usize,
    store: &EdgeStore,
) -> GraphResult<()> {
    writer.begin_section(base)?;
    writer.write_u32_values(store.offsets_slice())?;
    writer.begin_section(base + 1)?;
    writer.write_u32_values(store.targets_slice())?;
    writer.begin_section(base + 2)?;
    writer.write_body(store.type_ids_slice())?;
    writer.begin_section(base + 3)?;
    writer.write_body(store.schema_reversed_slice())?;
    writer.begin_section(base + 4)?;
    writer.write_u32_values(store.weights_slice())?;
    writer.begin_section(base + 5)?;
    writer.write_u32_values(store.relationship_ids_slice())
}

#[cfg(test)]
#[cfg(test)]
fn write_string_registry(writer: &mut GraphArtifactWriter, values: &[String]) -> GraphResult<()> {
    let count = u32::try_from(values.len())
        .map_err(|_| GraphError::Internal("edge type registry exceeds u32 entries".into()))?;
    writer.write_u32_value(count)?;
    let mut offset = 0u64;
    writer.write_u64_value(offset)?;
    for value in values {
        offset = offset
            .checked_add(u64::try_from(value.len()).map_err(|_| {
                GraphError::Internal("edge type registry payload exceeds u64".into())
            })?)
            .ok_or_else(|| GraphError::Internal("edge type registry payload exceeds u64".into()))?;
        writer.write_u64_value(offset)?;
    }
    for value in values {
        writer.write_body(value.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
#[cfg(test)]
fn write_relationship_identity_descriptors(
    writer: &mut GraphArtifactWriter,
    identities: &RelationshipIdentityStore,
) -> GraphResult<()> {
    let mut key_end = 0u64;
    for (idx, identity) in identities.iter().enumerate() {
        let (mapping_id, key_len) = match identity {
            None if idx == 0 => (0, 0u32),
            None => {
                return Err(GraphError::Internal(format!(
                    "relationship identity dictionary slot {idx} is empty"
                )));
            }
            Some(identity) if identity.mapping_id != 0 => {
                let len = u32::try_from(identity.source_key.len()).map_err(|_| {
                    GraphError::Internal("relationship identity key exceeds u32".into())
                })?;
                (identity.mapping_id, len)
            }
            Some(_) => {
                return Err(GraphError::Internal(format!(
                    "relationship identity dictionary slot {idx} has mapping ID 0"
                )));
            }
        };
        writer.write_body(&mapping_id.to_le_bytes())?;
        key_end = key_end
            .checked_add(u64::from(key_len))
            .ok_or_else(|| GraphError::Internal("relationship key bytes exceed u64".into()))?;
        writer.write_u64_value(key_end)?;
    }
    Ok(())
}

#[cfg(test)]
#[cfg(test)]
fn write_relationship_identity_keys(
    writer: &mut GraphArtifactWriter,
    identities: &RelationshipIdentityStore,
) -> GraphResult<()> {
    for identity in identities.iter().flatten() {
        writer.write_body(identity.source_key.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
#[cfg(test)]
fn tenant_encoding_workspace(engine: &Engine) -> GraphResult<crate::resource::ByteCount> {
    let node_count = engine.node_store.node_count() as usize;
    let bytes = engine
        .tenanted_table_oids
        .len()
        .checked_mul(std::mem::size_of::<u32>())
        .and_then(|bytes| {
            node_count
                .checked_mul(std::mem::size_of::<Option<&str>>())
                .and_then(|membership_bytes| bytes.checked_add(membership_bytes))
        })
        .and_then(|bytes| {
            node_count
                .checked_mul(std::mem::size_of::<&str>())
                .and_then(|dictionary_bytes| bytes.checked_add(dictionary_bytes))
        })
        .and_then(|bytes| {
            node_count
                .checked_mul(std::mem::size_of::<u32>())
                .and_then(|token_bytes| bytes.checked_add(token_bytes))
        })
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("tenant encoding workspace overflowed".into()))?;
    Ok(bytes)
}

#[cfg(test)]
#[cfg(test)]
fn write_tenant_sections(writer: &mut GraphArtifactWriter, engine: &Engine) -> GraphResult<()> {
    let mut tenanted_oids = Vec::new();
    tenanted_oids
        .try_reserve_exact(engine.tenanted_table_oids.len())
        .map_err(|error| {
            GraphError::Internal(format!("tenanted table OID allocation failed: {error}"))
        })?;
    tenanted_oids.extend(engine.tenanted_table_oids.iter().copied());
    tenanted_oids.sort_unstable();

    let node_count = engine.node_store.node_count() as usize;
    let mut tenant_by_node = Vec::new();
    tenant_by_node
        .try_reserve_exact(node_count)
        .map_err(|error| {
            GraphError::Internal(format!("node tenant assignment allocation failed: {error}"))
        })?;
    tenant_by_node.resize(node_count, None::<&str>);
    for (tenant, membership) in &engine.tenant_membership {
        for node_idx in membership.iter() {
            let slot = tenant_by_node.get_mut(node_idx as usize).ok_or_else(|| {
                GraphError::Internal(format!(
                    "tenant membership references out-of-range node {node_idx}"
                ))
            })?;
            if slot.replace(tenant.as_str()).is_some() {
                return Err(GraphError::Internal(format!(
                    "node {node_idx} belongs to multiple tenant memberships"
                )));
            }
        }
    }
    for (node_idx, slot) in tenant_by_node.iter_mut().enumerate() {
        if slot.is_some() {
            continue;
        }
        let node_idx = node_idx as u32;
        let Some(mapped) = engine.filter_index.mapped_tenant_for_node(node_idx) else {
            continue;
        };
        if !engine
            .tenant_membership_removals
            .get(mapped)
            .is_some_and(|removed| removed.contains(node_idx))
        {
            *slot = Some(mapped);
        }
    }

    let mut tenants = Vec::new();
    tenants.try_reserve_exact(node_count).map_err(|error| {
        GraphError::Internal(format!("tenant dictionary allocation failed: {error}"))
    })?;
    tenants.extend(tenant_by_node.iter().flatten().copied());
    tenants.sort_unstable();
    tenants.dedup();
    let tenant_count = u32::try_from(tenants.len())
        .map_err(|_| GraphError::Internal("tenant dictionary exceeds u32 entries".into()))?;

    let mut tokens = Vec::new();
    tokens.try_reserve_exact(node_count).map_err(|error| {
        GraphError::Internal(format!("node tenant token allocation failed: {error}"))
    })?;
    tokens.resize(node_count, 0u32);
    for (node_idx, tenant) in tenant_by_node.into_iter().enumerate() {
        let Some(tenant) = tenant else {
            continue;
        };
        let table_oid = engine
            .node_store
            .table_oid(node_idx as u32)
            .ok_or_else(|| {
                GraphError::Internal(format!(
                    "tenant membership node {node_idx} has no table OID"
                ))
            })?;
        if tenanted_oids.binary_search(&table_oid).is_err() {
            return Err(GraphError::Internal(format!(
                "tenant membership node {node_idx} belongs to unregistered table OID {table_oid}"
            )));
        }
        let dictionary_index = tenants
            .binary_search(&tenant)
            .map_err(|_| GraphError::Internal("tenant dictionary lost a node assignment".into()))?;
        tokens[node_idx] = u32::try_from(dictionary_index + 1)
            .map_err(|_| GraphError::Internal("tenant token exceeds u32".into()))?;
    }

    writer.begin_section(23)?;
    writer.write_u32_values(&tenanted_oids)?;

    writer.begin_section(24)?;
    writer.write_u32_value(tenant_count)?;
    let mut offset = 0u64;
    writer.write_u64_value(offset)?;
    for tenant in &tenants {
        offset =
            offset
                .checked_add(u64::try_from(tenant.len()).map_err(|_| {
                    GraphError::Internal("tenant dictionary value exceeds u64".into())
                })?)
                .ok_or_else(|| GraphError::Internal("tenant dictionary bytes exceed u64".into()))?;
        writer.write_u64_value(offset)?;
    }
    for tenant in tenants {
        writer.write_body(tenant.as_bytes())?;
    }

    writer.begin_section(25)?;
    writer.write_u32_values(&tokens)
}

fn decode_string_registry(mmap: &[u8], range: (usize, usize)) -> GraphResult<Vec<String>> {
    let bytes = &mmap[range.0..range.1];
    if bytes.len() < 12 {
        return Err(GraphError::CorruptFile {
            reason: "edge type registry is too short".into(),
        });
    }
    let count = read_u32_at(bytes, 0) as usize;
    let offsets_len = count
        .checked_add(1)
        .and_then(|count| count.checked_mul(8))
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "edge type registry offset table overflows".into(),
        })?;
    let payload_start = 4usize
        .checked_add(offsets_len)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "edge type registry header overflows".into(),
        })?;
    if payload_start > bytes.len() || read_u64_at(bytes, 4) != 0 {
        return Err(GraphError::CorruptFile {
            reason: "edge type registry has an invalid offset table".into(),
        });
    }
    let payload = &bytes[payload_start..];
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| GraphError::Oom {
            used_mb: 0,
            need_mb: (bytes.len() as u64).div_ceil(1_048_576),
            limit_mb: 0,
        })?;
    let mut previous = 0usize;
    for idx in 0..count {
        let end = usize::try_from(read_u64_at(bytes, 4 + (idx + 1) * 8)).map_err(|_| {
            GraphError::CorruptFile {
                reason: "edge type registry offset exceeds usize".into(),
            }
        })?;
        if end < previous || end > payload.len() {
            return Err(GraphError::CorruptFile {
                reason: format!("edge type registry offset {idx} is invalid"),
            });
        }
        let value = std::str::from_utf8(&payload[previous..end]).map_err(|err| {
            GraphError::CorruptFile {
                reason: format!("edge type registry entry {idx} is not UTF-8: {err}"),
            }
        })?;
        values.push(value.to_owned());
        previous = end;
    }
    if previous != payload.len() {
        return Err(GraphError::CorruptFile {
            reason: "edge type registry has trailing bytes".into(),
        });
    }
    Ok(values)
}

fn validate_relationship_ids(
    store: &EdgeStore,
    identities: &RelationshipIdentityStore,
    label: &str,
) -> GraphResult<()> {
    for &id in store.relationship_ids_slice() {
        if id == 0 {
            continue;
        }
        if identities.get(id).is_none() {
            return Err(GraphError::CorruptFile {
                reason: format!("{label} relationship ID {id} is outside the identity dictionary"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg(test)]
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

#[cfg(test)]
#[cfg(test)]
fn release_persistence_workspace(workspace: &mut Option<crate::resource::ResourceLease<'_>>) {
    if let Some(workspace) = workspace {
        workspace.release_all();
    }
}

#[cfg(test)]
#[cfg(test)]
fn packed_active_bytes(node_count: u32) -> GraphResult<crate::resource::ByteCount> {
    let bytes = u64::from(node_count)
        .checked_add(7)
        .map(|bits| bits / 8)
        .ok_or_else(|| GraphError::Internal("active bitmap size overflowed".to_string()))?;
    Ok(crate::resource::ByteCount::from_bytes(bytes))
}

#[cfg(test)]
#[cfg(test)]
fn resolution_serialization_bytes(node_count: u32) -> GraphResult<crate::resource::ByteCount> {
    let entry_size = u64::try_from(crate::resolution_index::ENTRY_SIZE)
        .map_err(|_| GraphError::Internal("resolution entry size does not fit u64".to_string()))?;
    u64::from(node_count)
        .checked_mul(entry_size)
        .and_then(|bytes| bytes.checked_add(4))
        .map(crate::resource::ByteCount::from_bytes)
        .ok_or_else(|| GraphError::Internal("resolution serialization size overflowed".to_string()))
}

#[cfg(test)]
#[cfg(test)]
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
/// - Both forward and inbound EdgeStore CSR arrays are mmap-backed.
/// - ResolutionIndex lookups read the mmap-backed resolution section.
/// - Immutable filter values, text dictionaries, and relationship identities
///   stay mapped; only bounded filter and edge-label metadata is decoded.
///
/// Each backend copies the artifact into an anonymous read-only mapping before
/// creating typed views. This prevents same-inode writes or truncation by
/// another process from invalidating Rust references. Derived metadata and
/// mutable overlays remain per-backend allocations.
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
    let manifest_store = ProjectionManifestStore::new(&manifest_root);
    let _generation_reader_lock = projection_candidate
        .is_none()
        .then(|| manifest_store.acquire_reader_lock())
        .transpose()?;
    let pinned_manifest = match projection_candidate {
        Some(candidate) => Some(candidate.clone()),
        None => manifest_store.load_latest_current()?,
    };
    let artifact_path = match (projection_candidate, pinned_manifest.as_ref()) {
        (None, Some(manifest)) => manifest_root.join(&manifest.base_artifact_path),
        _ => path.to_path_buf(),
    };
    let path = artifact_path.as_path();

    let mut file = fs::File::open(path)
        .map_err(|e| GraphError::Internal(format!("Cannot open {}: {}", path.display(), e)))?;

    let file_len = usize::try_from(
        file.metadata()
            .map_err(|e| GraphError::Internal(format!("Cannot stat {}: {}", path.display(), e)))?
            .len(),
    )
    .map_err(|_| GraphError::Internal("graph artifact is too large for this platform".into()))?;
    if file_len < 8 {
        return Err(GraphError::CorruptFile {
            reason: "file too small for graph artifact preamble".to_string(),
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
        return Err(GraphError::IncompatibleVersion(format!(
            "graph artifact version {version} is unsupported; run SELECT graph.build() to create version {VERSION}"
        )));
    }
    if file_len < HEADER_SIZE {
        return Err(GraphError::CorruptFile {
            reason: "file too small for v6 graph artifact header".to_string(),
        });
    }
    if read_u32_at(&mmap, 8) as usize != HEADER_SIZE
        || read_u32_at(&mmap, 28) as usize != NUM_SECTIONS
    {
        return Err(GraphError::CorruptFile {
            reason: "invalid v6 header or section count".into(),
        });
    }
    if mmap[48..64].iter().any(|&byte| byte != 0)
        || mmap[480..HEADER_SIZE].iter().any(|&byte| byte != 0)
    {
        return Err(GraphError::CorruptFile {
            reason: "v6 header reserved bytes must be zero".into(),
        });
    }
    let stored_header_crc = read_u32_at(&mmap, HEADER_CRC_OFFSET);
    let mut header = [0u8; HEADER_SIZE];
    header.copy_from_slice(&mmap[..HEADER_SIZE]);
    header[HEADER_CRC_OFFSET..HEADER_CRC_OFFSET + 4].fill(0);
    let computed_header_crc = crc32fast::hash(&header);
    if stored_header_crc != computed_header_crc {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "header CRC32 mismatch: stored={stored_header_crc:#x}, computed={computed_header_crc:#x}"
            ),
        });
    }
    let body_len =
        usize::try_from(read_u64_at(&mmap, 32)).map_err(|_| GraphError::CorruptFile {
            reason: "v6 body length exceeds usize".into(),
        })?;
    if body_len != file_len - HEADER_SIZE {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "v6 body length mismatch: header={body_len}, actual={}",
                file_len - HEADER_SIZE
            ),
        });
    }
    let stored_crc = read_u32_at(&mmap, BODY_CRC_OFFSET);
    let computed_crc = crc32fast::hash(&mmap[HEADER_SIZE..]);
    if stored_crc != computed_crc {
        return Err(GraphError::CorruptFile {
            reason: format!(
                "body CRC32 mismatch: stored={stored_crc:#x}, computed={computed_crc:#x}"
            ),
        });
    }

    let flags = read_u32_at(&mmap, 12);
    let node_count = read_u32_at(&mmap, 16);
    let forward_edge_count = read_u32_at(&mmap, 20);
    let inbound_edge_count = read_u32_at(&mmap, 24);
    let mut sections = [SectionDescriptor::default(); NUM_SECTIONS];
    for (idx, descriptor) in sections.iter_mut().enumerate() {
        let start = SECTION_DESCRIPTORS_OFFSET + idx * SECTION_DESCRIPTOR_SIZE;
        descriptor.offset = read_u64_at(&mmap, start);
        descriptor.len = read_u64_at(&mmap, start + 8);
    }

    let resolution_validation_bytes =
        crate::resource::ByteCount::from_usize(node_count as usize)
            .ok_or_else(|| GraphError::Internal("resolution validation size overflowed".into()))?;
    let resolution_validation = load_governor
        .reserve_memory(
            crate::resource::ResourcePhase::LoadMetadata,
            resolution_validation_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
    let layout = validate_section_layout(
        &mmap,
        &sections,
        node_count,
        forward_edge_count,
        inbound_edge_count,
        flags,
    )?;
    drop(resolution_validation);
    // Mapped value/key bytes are already covered by the full anonymous
    // snapshot reservation. This additional lease covers bounded descriptors,
    // copied names/labels, empty per-column delta containers, and temporary
    // uniqueness validation indexes.
    let filter_metadata_bytes = FilterIndex::mapped_load_metadata_upper_bound(
        &mmap[layout.ranges[17].0..layout.ranges[17].1],
    )?;
    let registry_metadata_bytes = (layout.ranges[20].1 - layout.ranges[20].0)
        .checked_mul(4)
        .ok_or_else(|| GraphError::Internal("registry metadata estimate overflowed".into()))?;
    let identity_validation_bytes = (layout.ranges[21].1 - layout.ranges[21].0)
        .checked_mul(4)
        .ok_or_else(|| GraphError::Internal("identity metadata estimate overflowed".into()))?;
    let tenant_oid_metadata_bytes = (layout.ranges[23].1 - layout.ranges[23].0)
        .checked_mul(4)
        .ok_or_else(|| GraphError::Internal("tenant metadata estimate overflowed".into()))?;
    let tenant_dictionary_count = read_u32_at(&mmap, layout.ranges[24].0) as usize;
    let tenant_dictionary_metadata_bytes = tenant_dictionary_count
        .checked_mul(std::mem::size_of::<std::ops::Range<usize>>())
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or_else(|| GraphError::Internal("tenant dictionary metadata overflowed".into()))?;
    let tenant_metadata_bytes = tenant_oid_metadata_bytes
        .checked_add(tenant_dictionary_metadata_bytes)
        .ok_or_else(|| GraphError::Internal("tenant metadata estimate overflowed".into()))?;
    let variable_metadata_bytes = filter_metadata_bytes
        .checked_add(registry_metadata_bytes)
        .and_then(|bytes| bytes.checked_add(identity_validation_bytes))
        .and_then(|bytes| bytes.checked_add(tenant_metadata_bytes))
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
    let edge_store = EdgeStore::from_mmap(artifact.edge_arrays(false)?);
    let reverse_edge_store = EdgeStore::from_mmap(artifact.edge_arrays(true)?);
    // ── ResolutionIndex: mmap'd, zero-copy (handled by Engine) ──
    let ri_start = section_ranges[16].0;
    let ri_end = section_ranges[16].1;
    let ri_len = ri_end - ri_start;

    let mut filter_index = FilterIndex::from_mapped_sections(
        MappedBytes::from_mmap(Arc::clone(&artifact.mmap)),
        section_ranges[17].0..section_ranges[17].1,
        section_ranges[18].0..section_ranges[18].1,
        section_ranges[19].0..section_ranges[19].1,
        node_count,
    )?;
    filter_index.install_mapped_tenant_base(MappedTenantIndex::new(
        MappedTenantIndexParts {
            bytes: MappedBytes::from_mmap(Arc::clone(&artifact.mmap)),
            dictionary_range: section_ranges[24].0..section_ranges[24].1,
            tokens_range: section_ranges[25].0..section_ranges[25].1,
        },
        node_count,
    )?);

    let tenanted_oid_bytes = &artifact.mmap[section_ranges[23].0..section_ranges[23].1];
    let mut tenanted_table_oids = std::collections::HashSet::new();
    tenanted_table_oids
        .try_reserve(tenanted_oid_bytes.len() / 4)
        .map_err(|error| {
            GraphError::Internal(format!(
                "tenanted table metadata allocation failed: {error}"
            ))
        })?;
    for index in 0..tenanted_oid_bytes.len() / 4 {
        tenanted_table_oids.insert(read_u32_at(tenanted_oid_bytes, index * 4));
    }

    let edge_type_registry = decode_string_registry(&artifact.mmap, section_ranges[20])?;
    if edge_type_registry
        .first()
        .is_none_or(|label| !label.is_empty())
    {
        return Err(GraphError::CorruptFile {
            reason: "edge type registry must reserve empty label at index 0".to_string(),
        });
    }
    if edge_type_registry.len() > 255
        || edge_type_registry.iter().skip(1).any(String::is_empty)
        || edge_type_registry
            .iter()
            .enumerate()
            .any(|(index, label)| edge_type_registry[..index].contains(label))
    {
        return Err(GraphError::CorruptFile {
            reason: "edge type registry contains invalid or duplicate labels".into(),
        });
    }
    let registry_len = edge_type_registry.len();
    if edge_store
        .type_ids_slice()
        .iter()
        .chain(reverse_edge_store.type_ids_slice())
        .any(|type_id| *type_id as usize >= registry_len)
    {
        return Err(GraphError::CorruptFile {
            reason: "CSR edge type ID is outside the edge type registry".into(),
        });
    }
    let relationship_identities = RelationshipIdentityStore::Mapped(
        MappedRelationshipIdentities::new(MappedRelationshipIdentityParts {
            mmap: MappedBytes::from_mmap(Arc::clone(&artifact.mmap)),
            descriptors_range: section_ranges[21].0..section_ranges[21].1,
            key_bytes_range: section_ranges[22].0..section_ranges[22].1,
        })?,
    );
    validate_relationship_ids(&edge_store, &relationship_identities, "forward")?;
    validate_relationship_ids(&reverse_edge_store, &relationship_identities, "inbound")?;

    let mut engine = Engine::new();
    engine.install_mmap_backed_graph(MmapBackedGraph {
        node_store,
        edge_store,
        reverse_edge_store,
        filter_index,
        edge_type_registry,
        relationship_identities,
        mmap: artifact.mmap,
        resolution_state: MmapResolutionState::new(ri_start, ri_len),
    })?;
    engine.tenanted_table_oids = tenanted_table_oids;
    engine.has_unidirectional_edges = flags & FLAG_HAS_UNIDIRECTIONAL != 0;
    if let Some(applied_sync_id) = read_sync_checkpoint(path)? {
        engine.record_applied_sync_id(applied_sync_id);
    }
    if let Some(projection_mode) = read_projection_mode(path)? {
        engine.set_projection_mode(projection_mode);
    }
    if let Some(manifest) = pinned_manifest.as_ref() {
        validate_projection_manifest_base(path, computed_crc, manifest)?;
    }
    let manifest = pinned_manifest;
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
            engine
                .relationship_identities
                .install_cumulative_owned(dictionary.identities())?;
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
    let mut file = fs::File::open(path)
        .map_err(|err| GraphError::Internal(format!("open graph artifact checksum: {err}")))?;
    let file_len = file
        .metadata()
        .map_err(|err| GraphError::Internal(format!("stat graph artifact checksum: {err}")))?
        .len();
    if file_len < HEADER_SIZE as u64 {
        return Err(GraphError::CorruptFile {
            reason: "file too small for header".to_string(),
        });
    }
    let mut header = [0_u8; HEADER_SIZE];
    file.read_exact(&mut header)
        .map_err(|err| GraphError::Internal(format!("read graph artifact header: {err}")))?;
    if &header[0..4] != MAGIC {
        return Err(GraphError::CorruptFile {
            reason: "invalid magic bytes".to_string(),
        });
    }
    let stored_crc = read_u32_at(&header, BODY_CRC_OFFSET);
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| GraphError::Internal(format!("read graph artifact body: {err}")))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let computed_crc = hasher.finalize();
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

/// Resolve the immutable base artifact currently serving a logical graph path.
///
/// A generation manifest takes precedence over the compatibility base path.
/// The returned path is not opened or validated by this helper.
pub(crate) fn current_base_artifact_path(path: &Path) -> GraphResult<Option<PathBuf>> {
    let root = projection_manifest_root(path);
    if let Some(manifest) = ProjectionManifestStore::new(&root).load_latest_current()? {
        return Ok(Some(root.join(manifest.base_artifact_path)));
    }
    Ok(path.is_file().then(|| path.to_path_buf()))
}

/// Return whether a complete persisted base is available for loading.
pub(crate) fn persisted_graph_exists(path: &Path) -> GraphResult<bool> {
    Ok(current_base_artifact_path(path)?.is_some_and(|base| base.is_file()))
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
    use crate::edge_store::{
        IdentifiedRawEdge, RawEdge, RelationshipIdentity, SortedEdgeStoreBuilder,
    };
    use crate::filter_index::{
        FilterColumnType, PersistedFilterValue, FILTER_CATALOG_HEADER_SIZE, FILTER_DESCRIPTOR_SIZE,
    };
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
    fn persisted_mmap_load_preserves_tenant_tokens_and_direction_flag() {
        let mut engine = tenant_fixture();
        engine.mark_has_unidirectional_edges();
        let path = temp_graph_path("tenant-roundtrip");

        write_graph_file(&engine, &path).expect("tenant fixture writes");
        let mut loaded = load_graph_file(&path).expect("tenant fixture reloads");

        assert!(loaded.has_unidirectional_edges);
        assert_eq!(loaded.tenanted_table_oids.len(), 2);
        assert!(loaded.tenanted_table_oids.contains(&10));
        assert!(loaded.tenanted_table_oids.contains(&20));
        assert!(loaded.filter_index.mapped_tenant_contains("alpha", 1));
        assert!(loaded.filter_index.mapped_tenant_contains("tenant-ζ", 0));
        assert!(!loaded.filter_index.mapped_tenant_contains("alpha", 0));
        assert_eq!(loaded.filter_index.mapped_tenant_for_node(2), None);
        loaded.remove_tenant_membership("alpha", 1);
        assert!(!loaded.tenant_contains("alpha", 1));
        loaded.insert_tenant_membership("alpha", 1);
        assert!(loaded.tenant_contains("alpha", 1));
        loaded.remove_all_tenant_memberships(0);
        assert!(!loaded.tenant_contains("tenant-ζ", 0));
        loaded.insert_tenant_membership("tenant-ζ", 0);

        write_graph_file(&loaded, &path).expect("mapped tenant fixture rewrites");
        let reloaded = load_graph_file(&path).expect("rewritten tenant fixture reloads");
        assert!(reloaded.filter_index.mapped_tenant_contains("alpha", 1));
        assert!(reloaded.filter_index.mapped_tenant_contains("tenant-ζ", 0));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_graph_file_rejects_nonlexical_tenant_dictionary() {
        let engine = tenant_fixture();
        let path = temp_graph_path("tenant-dictionary-order");
        write_graph_file(&engine, &path).expect("tenant fixture writes");
        let dictionary = read_section_offset(&path, 24);
        let count = read_u32_from_file(&path, dictionary);
        let payload = dictionary + 4 + u64::from(count + 1) * 8;
        write_u8_at(&path, payload, b'z');
        rewrite_crc(&path);

        let error = match load_graph_file(&path) {
            Ok(_) => panic!("nonlexical tenant dictionary was accepted"),
            Err(error) => error,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(matches!(
            error,
            GraphError::CorruptFile { reason } if reason.contains("strictly lexical")
        ));
    }

    #[test]
    fn load_graph_file_rejects_unsorted_tenanted_table_oids() {
        let engine = tenant_fixture();
        let path = temp_graph_path("tenant-oid-order");
        write_graph_file(&engine, &path).expect("tenant fixture writes");
        let oids = read_section_offset(&path, 23);
        write_u32_at(&path, oids, 20);
        write_u32_at(&path, oids + 4, 10);
        rewrite_crc(&path);

        let error = match load_graph_file(&path) {
            Ok(_) => panic!("unsorted tenanted table OIDs were accepted"),
            Err(error) => error,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(matches!(
            error,
            GraphError::CorruptFile { reason } if reason.contains("strictly sorted")
        ));
    }

    #[test]
    fn load_graph_file_rejects_out_of_range_tenant_token() {
        let engine = tenant_fixture();
        let path = temp_graph_path("tenant-token-range");
        write_graph_file(&engine, &path).expect("tenant fixture writes");
        let dictionary = read_section_offset(&path, 24);
        let tenant_count = read_u32_from_file(&path, dictionary);
        let tokens = read_section_offset(&path, 25);
        write_u32_at(&path, tokens, tenant_count + 1);
        rewrite_crc(&path);

        let error = match load_graph_file(&path) {
            Ok(_) => panic!("out-of-range tenant token was accepted"),
            Err(error) => error,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(matches!(
            error,
            GraphError::CorruptFile { reason } if reason.contains("exceeds dictionary count")
        ));
    }

    #[test]
    fn load_graph_file_rejects_unknown_v6_flag() {
        let engine = tenant_fixture();
        let path = temp_graph_path("unknown-v6-flag");
        write_graph_file(&engine, &path).expect("tenant fixture writes");
        write_u32_at(&path, 12, 1 << 31);
        rewrite_crc(&path);

        let error = match load_graph_file(&path) {
            Ok(_) => panic!("unknown v6 flag was accepted"),
            Err(error) => error,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(matches!(
            error,
            GraphError::CorruptFile { reason } if reason.contains("unsupported graph artifact flags")
        ));
    }

    #[test]
    fn load_graph_file_rejects_tenant_token_for_unregistered_table() {
        let mut engine = tenant_fixture();
        engine.tenanted_table_oids.remove(&20);
        let path = temp_graph_path("tenant-token-table");
        write_graph_file(&engine, &path).expect("tenant fixture writes");
        let tokens = read_section_offset(&path, 25);
        write_u32_at(&path, tokens + 2 * 4, 1);
        rewrite_crc(&path);

        let error = match load_graph_file(&path) {
            Ok(_) => panic!("tenant token for an unregistered table was accepted"),
            Err(error) => error,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(matches!(
            error,
            GraphError::CorruptFile { reason } if reason.contains("unregistered table OID")
        ));
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
    fn mapped_filter_metadata_is_charged_before_many_empty_columns_load() {
        let mut engine = Engine::new();
        engine.node_store.add_node(10, "A".to_string());
        engine.resolution_insert(10, "A", 0);
        engine.edge_store = EdgeStore::from_edges(1, vec![], false);
        for column in 0..512 {
            engine
                .filter_index
                .register_column(10, format!("empty_{column}"), 1);
        }
        engine.built = true;
        let path = temp_graph_path("many-empty-filter-columns");
        write_graph_file(&engine, &path).expect("fixture writes");

        let artifact = std::fs::read(&path).expect("artifact reads");
        let catalog_start = read_section_offset(&path, 17) as usize;
        let catalog_end = catalog_start + read_section_len(&path, 17) as usize;
        let filter_metadata =
            FilterIndex::mapped_load_metadata_upper_bound(&artifact[catalog_start..catalog_end])
                .expect("metadata bound computes");
        let registry_metadata = (read_section_len(&path, 20) as usize) * 4;
        let identity_metadata = (read_section_len(&path, 21) as usize) * 4;
        let metadata = filter_metadata + registry_metadata + identity_metadata;
        let hard = crate::resource::configured_memory_limit_mb().max(1) as u64 * 1_048_576;
        let admitted = artifact.len() as u64 + metadata as u64;
        let resident = crate::resource::ByteCount::from_bytes(hard - admitted + 1);

        let result = load_graph_file_with_residency(&path, resident);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::ResourceLimit { .. })));
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
        assert!(loaded.relationship_identities.get(0).is_none());
        let loaded_identity = loaded.relationship_identities.get(1).unwrap();
        assert_eq!(loaded_identity.mapping_id, 42);
        assert_eq!(loaded_identity.source_key, "edge:100");
        assert_eq!(reloaded.edge_store.relationship_ids_slice(), &[1]);
        let reloaded_identity = reloaded.relationship_identities.get(1).unwrap();
        assert_eq!(reloaded_identity.mapping_id, 42);
        assert_eq!(reloaded_identity.source_key, "edge:100");
    }

    #[test]
    fn persisted_relationship_identity_allows_empty_source_key() {
        let mut engine = graph_with_identified_relationship();
        engine.relationship_identities = RelationshipIdentityStore::try_from_owned(vec![
            None,
            Some(RelationshipIdentity {
                mapping_id: 42,
                source_key: String::new(),
            }),
        ])
        .unwrap();
        let path = temp_graph_path("relationship-identity-empty-source-key");
        write_graph_file(&engine, &path).unwrap();

        let loaded = load_graph_file(&path).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let identity = loaded.relationship_identities.get(1).unwrap();
        assert_eq!(identity.mapping_id, 42);
        assert_eq!(identity.source_key, "");
    }

    #[test]
    fn load_graph_file_rejects_empty_relationship_identity_slot() {
        let engine = graph_with_identified_relationship();
        let path = temp_graph_path("relationship-identity-empty-slot");
        write_graph_file(&engine, &path).unwrap();
        let descriptors = read_section_offset(&path, 21);
        write_u64_at(&path, descriptors + 16, 0);
        rewrite_crc(&path);

        let err = match load_graph_file(&path) {
            Ok(_) => panic!("malformed relationship dictionary must fail closed"),
            Err(err) => err,
        };
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(
            matches!(err, GraphError::CorruptFile { reason } if reason.contains("mapping ID 0"))
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
        let open = engine
            .filter_index
            .intern_text_value(status, "open")
            .unwrap();
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
        let open = engine
            .filter_index
            .intern_text_value(column, "open")
            .unwrap();
        engine.filter_index.set_encoded_value(
            column,
            0,
            Some(crate::filter_index::EncodedFilterValue::Text(open)),
        );

        let path = temp_graph_path("malformed-filter-dictionary");
        write_graph_file(&engine, &path).expect("filter fixture writes");
        let dictionary = read_section_offset(&path, 19);
        write_u64_at(&path, dictionary, 1);
        rewrite_crc(&path);
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
    fn engine_resolves_current_generation_base_artifact() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("base-manifest-wrong-base");
        write_graph_file(&engine, &path).unwrap();
        let other_path = path.with_file_name("other.pggraph");
        let mut generation = Engine::new();
        let node = generation.node_store.add_node(20, "generation".to_string());
        generation.resolution_insert(20, "generation", node);
        generation.edge_store = EdgeStore::from_edges(1, vec![], false);
        generation.reverse_edge_store = generation.edge_store.reversed();
        generation.built = true;
        write_graph_file(&generation, &other_path).unwrap();
        publish_base_manifest(&other_path, 9, 0);

        let loaded = load_graph_file(&path).expect("current generation base loads");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(loaded.node_store.node_count(), 1);
        assert_eq!(loaded.node_store.primary_key(0), Some("generation"));
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
        let filter_offset = read_section_offset(&path, 17);
        let registry_offset = read_section_offset(&path, 20);
        let file_len = std::fs::metadata(&path).unwrap().len();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(NUM_SECTIONS, 26);
        assert_eq!(active_offset, HEADER_SIZE as u64);
        assert_eq!(table_oids_offset, HEADER_SIZE as u64);
        assert!(filter_offset >= HEADER_SIZE as u64);
        assert!(registry_offset > filter_offset);
        assert!(file_len >= registry_offset);
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
        let open = engine
            .filter_index
            .intern_text_value(status, "open")
            .unwrap();
        engine.filter_index.set_encoded_value(
            status,
            node_idx,
            Some(crate::filter_index::EncodedFilterValue::Text(open)),
        );
        engine.built = true;

        let path = temp_graph_path("launch-artifact-section-sizes");
        write_graph_file(&engine, &path).unwrap();
        let header_version = read_u32_from_file(&path, 4);
        let filter_offset = read_section_offset(&path, 17);
        let filter_len = read_section_len(&path, 17);
        let filter_data_len = read_section_len(&path, 18);
        let filter_dictionary_len = read_section_len(&path, 19);
        let registry_offset = read_section_offset(&path, 20);
        let file_len = std::fs::metadata(&path).unwrap().len();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(header_version, VERSION);
        assert_eq!(NUM_SECTIONS, 26);
        assert!(filter_offset >= HEADER_SIZE as u64);
        assert!(registry_offset > filter_offset);
        assert_eq!(filter_len, 16 + 64 + "status".len() as u64);
        assert_eq!(filter_data_len, 1 + 4);
        assert_eq!(filter_dictionary_len, 16 + "open".len() as u64);
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
        // Write a v6 preamble smaller than the 512-byte header.
        let mut preamble = Vec::from(*MAGIC);
        preamble.extend_from_slice(&VERSION.to_le_bytes());
        std::fs::write(&path, preamble).unwrap();

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

        let edge_type = engine.register_edge_type("connected_to").unwrap();
        // No weights
        engine.edge_store = EdgeStore::from_edges(
            2,
            vec![RawEdge {
                source: 0,
                target: 1,
                type_id: edge_type,
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
            Err(GraphError::IncompatibleVersion(message)) => {
                assert!(message.contains("unsupported"));
                assert!(message.contains("graph.build()"));
            }
            Err(other) => panic!("expected IncompatibleVersion, got {:?}", other),
            Ok(_) => panic!("expected version mismatch to fail"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tenant_fixture() -> Engine {
        let mut engine = Engine::new();
        let first = engine.node_store.add_node(10, "A-1".to_string());
        let second = engine.node_store.add_node(10, "A-2".to_string());
        let unscoped = engine.node_store.add_node(20, "B-1".to_string());
        engine.resolution_insert(10, "A-1", first);
        engine.resolution_insert(10, "A-2", second);
        engine.resolution_insert(20, "B-1", unscoped);
        engine.tenanted_table_oids.insert(20);
        engine.tenanted_table_oids.insert(10);
        engine.insert_tenant_membership("tenant-ζ", first);
        engine.insert_tenant_membership("alpha", second);
        engine.edge_store = EdgeStore::from_edges(3, Vec::new(), false);
        engine.built = true;
        engine
    }

    fn read_section_offset(path: &Path, section: usize) -> u64 {
        use std::io::{Read, Seek};

        let mut file = std::fs::OpenOptions::new().read(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(
            (SECTION_DESCRIPTORS_OFFSET + section * SECTION_DESCRIPTOR_SIZE) as u64,
        ))
        .unwrap();
        let mut bytes = [0u8; 8];
        file.read_exact(&mut bytes).unwrap();
        u64::from_le_bytes(bytes)
    }

    fn read_section_len(path: &Path, section: usize) -> u64 {
        use std::io::{Read, Seek};

        let mut file = std::fs::OpenOptions::new().read(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(
            (SECTION_DESCRIPTORS_OFFSET + section * SECTION_DESCRIPTOR_SIZE + 8) as u64,
        ))
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
        file.seek(std::io::SeekFrom::Start(
            (SECTION_DESCRIPTORS_OFFSET + section * SECTION_DESCRIPTOR_SIZE) as u64,
        ))
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

    fn write_u8_at(path: &Path, offset: u64, value: u8) {
        use std::io::{Seek, Write};

        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.write_all(&[value]).unwrap();
        file.flush().unwrap();
    }

    fn write_u64_at(path: &Path, offset: u64, value: u64) {
        use std::io::{Seek, Write};

        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.write_all(&value.to_le_bytes()).unwrap();
        file.flush().unwrap();
    }

    fn copy_file_bytes(path: &Path, source: u64, target: u64, len: usize) {
        use std::io::{Read, Seek, Write};

        let mut bytes = vec![0u8; len];
        let mut file = std::fs::OpenOptions::new().read(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(source)).unwrap();
        file.read_exact(&mut bytes).unwrap();
        drop(file);
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(target)).unwrap();
        file.write_all(&bytes).unwrap();
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
        file.seek(std::io::SeekFrom::Start(BODY_CRC_OFFSET as u64))
            .unwrap();
        file.write_all(&crc.to_le_bytes()).unwrap();
        file.flush().unwrap();
        drop(file);
        let mut header = [0u8; HEADER_SIZE];
        std::fs::File::open(path)
            .unwrap()
            .read_exact(&mut header)
            .unwrap();
        header[HEADER_CRC_OFFSET..HEADER_CRC_OFFSET + 4].fill(0);
        let header_crc = crc32fast::hash(&header);
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(std::io::SeekFrom::Start(HEADER_CRC_OFFSET as u64))
            .unwrap();
        file.write_all(&header_crc.to_le_bytes()).unwrap();
        file.flush().unwrap();
    }

    fn graph_with_relationship() -> Engine {
        let mut engine = Engine::new();
        let a = engine.node_store.add_node(10, "A".to_string());
        let b = engine.node_store.add_node(10, "B".to_string());
        engine.resolution_insert(10, "A", a);
        engine.resolution_insert(10, "B", b);
        let edge_type = engine.register_edge_type("related_to").unwrap();
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
        engine.relationship_identities = RelationshipIdentityStore::try_from_owned(vec![
            None,
            Some(RelationshipIdentity {
                mapping_id: 42,
                source_key: "edge:100".to_string(),
            }),
        ])
        .unwrap();
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

        let filter_offset = read_section_offset(&path, 17);
        write_section_offset(&path, 18, filter_offset);

        let result = load_graph_file(&path);
        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_nonmonotonic_edge_offsets() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-edge-offsets");
        write_graph_file(&engine, &path).unwrap();

        let edge_offsets = read_section_offset(&path, 4);
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

        let edge_offsets = read_section_offset(&path, 4);
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

        let targets = read_section_offset(&path, 5);
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

        let weights = read_section_offset(&path, 8);
        write_section_offset(&path, 9, weights + 1);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(result, Err(GraphError::CorruptFile { .. })));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_inbound_sidecar_mismatch() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-inbound-sidecar");
        write_graph_file(&engine, &path).unwrap();

        let inbound_relationship_ids = read_section_offset(&path, 15);
        write_u32_at(&path, inbound_relationship_ids, 99);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("sidecars differ")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_inbound_topology_mismatch() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-inbound-topology");
        write_graph_file(&engine, &path).unwrap();

        write_u32_at(&path, read_section_offset(&path, 11), 1);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("topology counts differ")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_forward_inbound_count_mismatch() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-csr-count-mode");
        write_graph_file(&engine, &path).unwrap();

        write_u32_at(&path, 20, 0);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("counts or weight modes differ")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_forward_inbound_weight_mode_mismatch() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-csr-weight-mode");
        write_graph_file(&engine, &path).unwrap();

        write_u32_at(&path, 12, FLAG_FORWARD_WEIGHTS);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("counts or weight modes differ")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_inbound_weight_mismatch() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-inbound-weight");
        write_graph_file(&engine, &path).unwrap();

        write_u32_at(&path, read_section_offset(&path, 14), 8);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("sidecars differ")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_resolution_identity_mismatch() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-resolution-identity");
        write_graph_file(&engine, &path).unwrap();

        let resolution = read_section_offset(&path, 16);
        write_u64_at(&path, resolution + 8, u64::MAX);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("does not match")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_duplicate_resolution_node() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("duplicate-resolution-node");
        write_graph_file(&engine, &path).unwrap();

        let resolution = read_section_offset(&path, 16);
        copy_file_bytes(
            &path,
            resolution + 4,
            resolution + 4 + RESOLUTION_ENTRY_SIZE as u64,
            RESOLUTION_ENTRY_SIZE,
        );
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("duplicates node index")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_unknown_edge_type() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-edge-type");
        write_graph_file(&engine, &path).unwrap();

        write_u8_at(&path, read_section_offset(&path, 6), 254);
        write_u8_at(&path, read_section_offset(&path, 12), 254);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("edge type registry")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_overlapping_filter_ranges() {
        let mut engine = graph_with_relationship();
        engine
            .filter_index
            .register_column(10, "first".to_string(), 2);
        engine
            .filter_index
            .register_column(10, "second".to_string(), 2);
        let path = temp_graph_path("overlapping-filter-ranges");
        write_graph_file(&engine, &path).unwrap();

        let catalog = read_section_offset(&path, 17);
        let second_descriptor_data_offset =
            catalog + FILTER_CATALOG_HEADER_SIZE as u64 + FILTER_DESCRIPTOR_SIZE as u64 + 24;
        write_u64_at(&path, second_descriptor_data_offset, 0);
        rewrite_crc(&path);

        let result = load_graph_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(
            result,
            Err(GraphError::CorruptFile { reason }) if reason.contains("canonical")
        ));
    }

    #[test]
    fn load_graph_file_rejects_crc_valid_nonmonotonic_pk_offsets() {
        let engine = graph_with_relationship();
        let path = temp_graph_path("bad-pk-offsets");
        write_graph_file(&engine, &path).unwrap();

        let pk_offsets = read_section_offset(&path, 2);
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

        let pk_offsets = read_section_offset(&path, 2);
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

        let pk_offsets = read_section_offset(&path, 2);
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

        let pk_bytes = read_section_offset(&path, 3);
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

    fn direct_test_metadata() -> DirectArtifactMetadata {
        DirectArtifactMetadata {
            node_count: 0,
            forward_edge_count: 0,
            inbound_edge_count: 0,
            has_forward_weights: false,
            has_inbound_weights: false,
            has_unidirectional_edges: false,
            applied_sync_id: 0,
            projection_mode: config::ProjectionMode::CsrReadonly,
        }
    }

    fn direct_test_governor(disk_bytes: u64) -> crate::resource::ResourceGovernor {
        crate::resource::ResourceGovernor::new(crate::resource::ResourceLimits::bounded(
            crate::resource::MemoryBudget::new(crate::resource::ByteCount::from_bytes(4096)),
            crate::resource::DiskBudget::new(crate::resource::ByteCount::from_bytes(disk_bytes)),
            crate::resource::RowCount::UNLIMITED,
            crate::resource::WorkUnits::UNLIMITED,
            crate::resource::ElapsedBudget::new(std::time::Duration::MAX),
        ))
    }

    #[test]
    fn direct_candidate_failure_removes_only_owned_staging_files() {
        let path = temp_graph_path("direct-candidate-cleanup");
        let governor = direct_test_governor(4096);
        let error = write_direct_graph_file(
            &path,
            &direct_test_metadata(),
            [0; NUM_SECTIONS],
            &governor,
            |_writer| Err(GraphError::Internal("injected direct write failure".into())),
        )
        .expect_err("injected failure must escape");
        assert!(error.to_string().contains("injected direct write failure"));
        assert!(!path.exists());
        assert!(!sync_checkpoint_path(&path).exists());
        assert!(!projection_mode_path(&path).exists());
        let _ = std::fs::remove_dir_all(path.parent().expect("candidate parent"));
    }

    #[test]
    fn direct_candidate_disk_limit_fails_before_file_creation() {
        let path = temp_graph_path("direct-candidate-disk-limit");
        let governor = direct_test_governor(100);
        assert!(write_direct_graph_file(
            &path,
            &direct_test_metadata(),
            [0; NUM_SECTIONS],
            &governor,
            |_writer| Ok(()),
        )
        .is_err());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().expect("candidate parent"));
    }

    #[test]
    fn direct_candidate_never_overwrites_an_existing_path() {
        let path = temp_graph_path("direct-candidate-create-new");
        std::fs::write(&path, b"previous-generation").expect("write sentinel candidate");
        let governor = direct_test_governor(4096);
        assert!(write_direct_graph_file(
            &path,
            &direct_test_metadata(),
            [0; NUM_SECTIONS],
            &governor,
            |_writer| Ok(()),
        )
        .is_err());
        assert_eq!(
            std::fs::read(&path).expect("read sentinel candidate"),
            b"previous-generation"
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("candidate parent"));
    }
}
