//! Durable projection delta segment binary format.
//!
//! Segment files store immutable mutation batches referenced by projection
//! manifests. The loader treats bytes as untrusted input and validates magic,
//! version, offsets, checksum, reserved bytes, and row bounds before returning
//! decoded sections.

use std::fs;
use std::path::Path;

use crc32fast::Hasher;

use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

const MAGIC: &[u8; 8] = b"PGGSEG01";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 160;
const CHECKSUM_OFFSET: usize = 124;
const RESERVED_OFFSET: usize = 128;
const RESERVED_LEN: usize = 32;
const SECTION_COUNT: usize = 7;
const RESERVED_HEADER_RANGES: [std::ops::Range<usize>; 3] = [15..16, 60..64, 120..124];

/// Segment file category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentKind {
    /// Edge topology, delete, and weight deltas.
    Edge = 1,
    /// Node, resolution, filter, and tenant deltas.
    Node = 2,
}

impl SegmentKind {
    fn from_u8(raw: u8) -> GraphResult<Self> {
        match raw {
            1 => Ok(Self::Edge),
            2 => Ok(Self::Node),
            other => Err(segment_corrupt(format!("unknown segment kind {other}"))),
        }
    }
}

/// Header metadata for one decoded segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentHeader {
    /// Segment file format version.
    pub(crate) version: u32,
    /// Segment category.
    pub(crate) kind: SegmentKind,
    /// Segment compaction level.
    pub(crate) level: u8,
    /// Direction covered by edge sections.
    pub(crate) direction: TraversalDirection,
    /// Inclusive source-node range start.
    pub(crate) source_start: u32,
    /// Exclusive source-node range end.
    pub(crate) source_end: u32,
    /// Highest sync-log row represented by this segment.
    pub(crate) sync_watermark: i64,
    /// CRC32 checksum over the segment bytes with the checksum field zeroed.
    pub(crate) checksum: u32,
}

/// Edge topology row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentEdge {
    /// Source node index.
    pub(crate) source: u32,
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: u8,
}

/// Weighted edge row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentEdgeWeight {
    /// Source node index.
    pub(crate) source: u32,
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: u8,
    /// Edge weight.
    pub(crate) weight: u32,
}

/// Node active-state row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentNodeState {
    /// Node index.
    pub(crate) node_idx: u32,
    /// Whether the node is active after this delta.
    pub(crate) active: bool,
}

/// Resolution-index row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentResolution {
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Hash of the source primary key.
    pub(crate) pk_hash: u64,
    /// Node index.
    pub(crate) node_idx: u32,
    /// Whether the resolution entry is removed.
    pub(crate) tombstone: bool,
}

/// Filter-index row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentFilterValue {
    /// Node index.
    pub(crate) node_idx: u32,
    /// Registered filter column identifier.
    pub(crate) column_id: u32,
    /// Encoded filter value.
    pub(crate) value: u32,
    /// Whether the filter value is removed.
    pub(crate) tombstone: bool,
}

/// Tenant-membership row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentTenant {
    /// Node index.
    pub(crate) node_idx: u32,
    /// Hash of the tenant identifier.
    pub(crate) tenant_hash: u64,
    /// Whether the tenant membership is removed.
    pub(crate) tombstone: bool,
}

/// Decoded durable delta segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeltaSegment {
    /// Segment header.
    pub(crate) header: SegmentHeader,
    /// Edge insert/topology rows.
    pub(crate) edge_inserts: Vec<SegmentEdge>,
    /// Edge delete rows.
    pub(crate) edge_deletes: Vec<SegmentEdge>,
    /// Edge weight rows.
    pub(crate) edge_weights: Vec<SegmentEdgeWeight>,
    /// Node active/tombstone rows.
    pub(crate) node_states: Vec<SegmentNodeState>,
    /// Resolution-index rows.
    pub(crate) resolutions: Vec<SegmentResolution>,
    /// Filter-index rows.
    pub(crate) filters: Vec<SegmentFilterValue>,
    /// Tenant-membership rows.
    pub(crate) tenants: Vec<SegmentTenant>,
}

impl DeltaSegment {
    /// Construct an empty segment with validated header metadata.
    pub(crate) fn new(
        kind: SegmentKind,
        level: u8,
        direction: TraversalDirection,
        source_start: u32,
        source_end: u32,
        sync_watermark: i64,
    ) -> GraphResult<Self> {
        let header = SegmentHeader {
            version: VERSION,
            kind,
            level,
            direction,
            source_start,
            source_end,
            sync_watermark,
            checksum: 0,
        };
        validate_header_shape(&header)?;
        Ok(Self {
            header,
            edge_inserts: Vec::new(),
            edge_deletes: Vec::new(),
            edge_weights: Vec::new(),
            node_states: Vec::new(),
            resolutions: Vec::new(),
            filters: Vec::new(),
            tenants: Vec::new(),
        })
    }

    /// Encode this segment to bytes.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] if row bounds or section ownership
    /// are invalid.
    pub(crate) fn to_bytes(&self) -> GraphResult<Vec<u8>> {
        validate_segment(self)?;
        let mut sections = EncodedSections::default();
        encode_edges(&mut sections.edge_inserts, &self.edge_inserts);
        encode_edges(&mut sections.edge_deletes, &self.edge_deletes);
        encode_edge_weights(&mut sections.edge_weights, &self.edge_weights);
        encode_node_states(&mut sections.node_states, &self.node_states);
        encode_resolutions(&mut sections.resolutions, &self.resolutions);
        encode_filters(&mut sections.filters, &self.filters);
        encode_tenants(&mut sections.tenants, &self.tenants);

        let section_bytes = sections.as_slices();
        let counts = self.section_counts()?;
        let mut offsets = [0_u64; SECTION_COUNT];
        let mut cursor = HEADER_SIZE;
        let mut bytes = vec![0; HEADER_SIZE];
        for (idx, section) in section_bytes.iter().enumerate() {
            offsets[idx] = cursor as u64;
            bytes.extend_from_slice(section);
            cursor += section.len();
        }
        write_header(&mut bytes, &self.header, &counts, &offsets, 0);
        let checksum = checksum_segment_bytes(&bytes);
        write_u32_at(&mut bytes, CHECKSUM_OFFSET, checksum);
        Ok(bytes)
    }

    /// Write this segment to a file path.
    ///
    /// # Errors
    ///
    /// Returns encoding errors or filesystem errors.
    #[cfg_attr(all(feature = "fuzzing", not(test)), allow(dead_code))]
    pub(crate) fn write_to_path(&self, path: &Path) -> GraphResult<()> {
        let bytes = self.to_bytes()?;
        fs::write(path, bytes)
            .map_err(|err| GraphError::Internal(format!("segment write failed: {err}")))
    }

    /// Read and validate a segment file from disk.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] when the segment is malformed.
    #[cfg_attr(all(feature = "fuzzing", not(test)), allow(dead_code))]
    pub(crate) fn read_from_path(path: &Path) -> GraphResult<Self> {
        let bytes = fs::read(path)
            .map_err(|err| GraphError::Internal(format!("segment read failed: {err}")))?;
        Self::from_bytes(&bytes)
    }

    /// Decode and validate a segment from bytes.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] when the segment is malformed.
    pub(crate) fn from_bytes(bytes: &[u8]) -> GraphResult<Self> {
        if bytes.len() < HEADER_SIZE {
            return Err(segment_corrupt("segment is shorter than header"));
        }
        if &bytes[0..8] != MAGIC {
            return Err(segment_corrupt("invalid segment magic"));
        }
        validate_reserved_header_bytes(bytes)?;
        let version = read_u32(bytes, 8)?;
        if version != VERSION {
            return Err(GraphError::IncompatibleVersion(format!(
                "projection segment version {version} is unsupported; expected {VERSION}"
            )));
        }
        let stored_checksum = read_u32(bytes, CHECKSUM_OFFSET)?;
        if stored_checksum != checksum_segment_bytes(bytes) {
            return Err(segment_corrupt("segment checksum mismatch"));
        }

        let header = SegmentHeader {
            version,
            kind: SegmentKind::from_u8(read_u8(bytes, 12)?)?,
            level: read_u8(bytes, 14)?,
            direction: decode_direction(read_u8(bytes, 13)?)?,
            source_start: read_u32(bytes, 16)?,
            source_end: read_u32(bytes, 20)?,
            sync_watermark: read_i64(bytes, 24)?,
            checksum: stored_checksum,
        };
        validate_header_shape(&header)?;
        let counts = read_counts(bytes)?;
        let offsets = read_offsets(bytes)?;
        let ranges = validate_section_ranges(bytes.len(), &counts, &offsets)?;
        let segment = Self {
            edge_inserts: decode_edges(section(bytes, ranges[0].clone())?, counts[0])?,
            edge_deletes: decode_edges(section(bytes, ranges[1].clone())?, counts[1])?,
            edge_weights: decode_edge_weights(section(bytes, ranges[2].clone())?, counts[2])?,
            node_states: decode_node_states(section(bytes, ranges[3].clone())?, counts[3])?,
            resolutions: decode_resolutions(section(bytes, ranges[4].clone())?, counts[4])?,
            filters: decode_filters(section(bytes, ranges[5].clone())?, counts[5])?,
            tenants: decode_tenants(section(bytes, ranges[6].clone())?, counts[6])?,
            header,
        };
        validate_segment(&segment)?;
        Ok(segment)
    }

    fn section_counts(&self) -> GraphResult<[u32; SECTION_COUNT]> {
        Ok([
            count_len(self.edge_inserts.len())?,
            count_len(self.edge_deletes.len())?,
            count_len(self.edge_weights.len())?,
            count_len(self.node_states.len())?,
            count_len(self.resolutions.len())?,
            count_len(self.filters.len())?,
            count_len(self.tenants.len())?,
        ])
    }
}

/// Return valid seed bytes for projection segment fuzz targets.
#[cfg(any(test, feature = "fuzzing"))]
pub(crate) fn fuzz_seed_bytes(name: &str) -> Option<Vec<u8>> {
    match name.trim() {
        "edge" => {
            let mut segment =
                DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 4, 1).ok()?;
            segment.edge_inserts.push(SegmentEdge {
                source: 0,
                target: 1,
                type_id: 1,
            });
            segment.edge_deletes.push(SegmentEdge {
                source: 1,
                target: 2,
                type_id: 1,
            });
            segment.edge_weights.push(SegmentEdgeWeight {
                source: 0,
                target: 1,
                type_id: 1,
                weight: 5,
            });
            segment.to_bytes().ok()
        }
        "node" => {
            let mut segment =
                DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 4, 1).ok()?;
            segment.node_states.push(SegmentNodeState {
                node_idx: 0,
                active: true,
            });
            segment.resolutions.push(SegmentResolution {
                table_oid: 100,
                pk_hash: 1_001,
                node_idx: 0,
                tombstone: false,
            });
            segment.filters.push(SegmentFilterValue {
                node_idx: 0,
                column_id: 7,
                value: 9,
                tombstone: false,
            });
            segment.tenants.push(SegmentTenant {
                node_idx: 0,
                tenant_hash: 2_002,
                tombstone: false,
            });
            segment.to_bytes().ok()
        }
        _ => None,
    }
}

#[derive(Default)]
struct EncodedSections {
    edge_inserts: Vec<u8>,
    edge_deletes: Vec<u8>,
    edge_weights: Vec<u8>,
    node_states: Vec<u8>,
    resolutions: Vec<u8>,
    filters: Vec<u8>,
    tenants: Vec<u8>,
}

impl EncodedSections {
    fn as_slices(&self) -> [&[u8]; SECTION_COUNT] {
        [
            &self.edge_inserts,
            &self.edge_deletes,
            &self.edge_weights,
            &self.node_states,
            &self.resolutions,
            &self.filters,
            &self.tenants,
        ]
    }
}

fn validate_segment(segment: &DeltaSegment) -> GraphResult<()> {
    validate_header_shape(&segment.header)?;
    match segment.header.kind {
        SegmentKind::Edge => {
            if !segment.node_states.is_empty()
                || !segment.resolutions.is_empty()
                || !segment.filters.is_empty()
                || !segment.tenants.is_empty()
            {
                return Err(segment_corrupt("edge segment contains node sections"));
            }
        }
        SegmentKind::Node => {
            if !segment.edge_inserts.is_empty()
                || !segment.edge_deletes.is_empty()
                || !segment.edge_weights.is_empty()
            {
                return Err(segment_corrupt("node segment contains edge sections"));
            }
        }
    }
    for edge in segment
        .edge_inserts
        .iter()
        .chain(segment.edge_deletes.iter())
    {
        validate_edge_bounds(&segment.header, edge)?;
    }
    for weight in &segment.edge_weights {
        validate_edge_bounds(
            &segment.header,
            &SegmentEdge {
                source: weight.source,
                target: weight.target,
                type_id: weight.type_id,
            },
        )?;
    }
    for node in &segment.node_states {
        validate_node_bounds(&segment.header, node.node_idx)?;
    }
    for resolution in &segment.resolutions {
        validate_node_bounds(&segment.header, resolution.node_idx)?;
    }
    for filter in &segment.filters {
        validate_node_bounds(&segment.header, filter.node_idx)?;
    }
    for tenant in &segment.tenants {
        validate_node_bounds(&segment.header, tenant.node_idx)?;
    }
    Ok(())
}

fn validate_header_shape(header: &SegmentHeader) -> GraphResult<()> {
    if header.source_start > header.source_end {
        return Err(segment_corrupt("source_start must not exceed source_end"));
    }
    if header.sync_watermark < 0 {
        return Err(segment_corrupt("sync_watermark must be nonnegative"));
    }
    Ok(())
}

fn validate_edge_bounds(header: &SegmentHeader, edge: &SegmentEdge) -> GraphResult<()> {
    validate_node_bounds(header, edge.source)?;
    if edge.type_id == 255 {
        return Err(segment_corrupt("edge type 255 is reserved"));
    }
    Ok(())
}

fn validate_node_bounds(header: &SegmentHeader, node_idx: u32) -> GraphResult<()> {
    if node_idx < header.source_start || node_idx >= header.source_end {
        return Err(segment_corrupt(format!(
            "node {node_idx} is outside segment range {}..{}",
            header.source_start, header.source_end
        )));
    }
    Ok(())
}

fn validate_reserved_header_bytes(bytes: &[u8]) -> GraphResult<()> {
    for range in RESERVED_HEADER_RANGES {
        if bytes[range].iter().any(|byte| *byte != 0) {
            return Err(segment_corrupt("reserved header bytes must be zero"));
        }
    }
    if bytes[RESERVED_OFFSET..RESERVED_OFFSET + RESERVED_LEN]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(segment_corrupt("reserved header bytes must be zero"));
    }
    Ok(())
}

fn write_header(
    bytes: &mut [u8],
    header: &SegmentHeader,
    counts: &[u32; SECTION_COUNT],
    offsets: &[u64; SECTION_COUNT],
    checksum: u32,
) {
    bytes[0..8].copy_from_slice(MAGIC);
    write_u32_at(bytes, 8, VERSION);
    bytes[12] = header.kind as u8;
    bytes[13] = encode_direction(header.direction);
    bytes[14] = header.level;
    bytes[15] = 0;
    write_u32_at(bytes, 16, header.source_start);
    write_u32_at(bytes, 20, header.source_end);
    write_i64_at(bytes, 24, header.sync_watermark);
    for (idx, count) in counts.iter().enumerate() {
        write_u32_at(bytes, 32 + idx * 4, *count);
    }
    for (idx, offset) in offsets.iter().enumerate() {
        write_u64_at(bytes, 64 + idx * 8, *offset);
    }
    write_u32_at(bytes, CHECKSUM_OFFSET, checksum);
}

fn count_len(len: usize) -> GraphResult<u32> {
    u32::try_from(len).map_err(|_| segment_corrupt("section row count exceeds u32"))
}

fn read_counts(bytes: &[u8]) -> GraphResult<[u32; SECTION_COUNT]> {
    let mut counts = [0_u32; SECTION_COUNT];
    for (idx, count) in counts.iter_mut().enumerate() {
        *count = read_u32(bytes, 32 + idx * 4)?;
    }
    Ok(counts)
}

fn read_offsets(bytes: &[u8]) -> GraphResult<[u64; SECTION_COUNT]> {
    let mut offsets = [0_u64; SECTION_COUNT];
    for (idx, offset) in offsets.iter_mut().enumerate() {
        *offset = read_u64(bytes, 64 + idx * 8)?;
    }
    Ok(offsets)
}

fn validate_section_ranges(
    len: usize,
    counts: &[u32; SECTION_COUNT],
    offsets: &[u64; SECTION_COUNT],
) -> GraphResult<[std::ops::Range<usize>; SECTION_COUNT]> {
    let widths = [9_usize, 9, 13, 5, 17, 13, 13];
    let mut ranges: [std::ops::Range<usize>; SECTION_COUNT] =
        std::array::from_fn(|_| 0_usize..0_usize);
    let mut previous_end = HEADER_SIZE;
    for idx in 0..SECTION_COUNT {
        let start = usize::try_from(offsets[idx])
            .map_err(|_| segment_corrupt("section offset exceeds usize"))?;
        let byte_len = usize::try_from(counts[idx])
            .ok()
            .and_then(|count| count.checked_mul(widths[idx]))
            .ok_or_else(|| segment_corrupt("section byte length overflowed"))?;
        let end = start
            .checked_add(byte_len)
            .ok_or_else(|| segment_corrupt("section end overflowed"))?;
        if start != previous_end {
            return Err(segment_corrupt("section offsets are not contiguous"));
        }
        if end > len {
            return Err(segment_corrupt("section extends past file end"));
        }
        ranges[idx] = start..end;
        previous_end = end;
    }
    if previous_end != len {
        return Err(segment_corrupt("trailing bytes after final section"));
    }
    Ok(ranges)
}

fn section(bytes: &[u8], range: std::ops::Range<usize>) -> GraphResult<&[u8]> {
    bytes
        .get(range)
        .ok_or_else(|| segment_corrupt("section range is out of bounds"))
}

fn encode_edges(out: &mut Vec<u8>, rows: &[SegmentEdge]) {
    for row in rows {
        push_u32(out, row.source);
        push_u32(out, row.target);
        out.push(row.type_id);
    }
}

fn encode_edge_weights(out: &mut Vec<u8>, rows: &[SegmentEdgeWeight]) {
    for row in rows {
        push_u32(out, row.source);
        push_u32(out, row.target);
        out.push(row.type_id);
        push_u32(out, row.weight);
    }
}

fn encode_node_states(out: &mut Vec<u8>, rows: &[SegmentNodeState]) {
    for row in rows {
        push_u32(out, row.node_idx);
        out.push(u8::from(row.active));
    }
}

fn encode_resolutions(out: &mut Vec<u8>, rows: &[SegmentResolution]) {
    for row in rows {
        push_u32(out, row.table_oid);
        push_u64(out, row.pk_hash);
        push_u32(out, row.node_idx);
        out.push(u8::from(row.tombstone));
    }
}

fn encode_filters(out: &mut Vec<u8>, rows: &[SegmentFilterValue]) {
    for row in rows {
        push_u32(out, row.node_idx);
        push_u32(out, row.column_id);
        push_u32(out, row.value);
        out.push(u8::from(row.tombstone));
    }
}

fn encode_tenants(out: &mut Vec<u8>, rows: &[SegmentTenant]) {
    for row in rows {
        push_u32(out, row.node_idx);
        push_u64(out, row.tenant_hash);
        out.push(u8::from(row.tombstone));
    }
}

fn decode_edges(bytes: &[u8], count: u32) -> GraphResult<Vec<SegmentEdge>> {
    let mut rows = Vec::with_capacity(count as usize);
    for idx in 0..count as usize {
        let offset = idx * 9;
        rows.push(SegmentEdge {
            source: read_u32(bytes, offset)?,
            target: read_u32(bytes, offset + 4)?,
            type_id: read_u8(bytes, offset + 8)?,
        });
    }
    Ok(rows)
}

fn decode_edge_weights(bytes: &[u8], count: u32) -> GraphResult<Vec<SegmentEdgeWeight>> {
    let mut rows = Vec::with_capacity(count as usize);
    for idx in 0..count as usize {
        let offset = idx * 13;
        rows.push(SegmentEdgeWeight {
            source: read_u32(bytes, offset)?,
            target: read_u32(bytes, offset + 4)?,
            type_id: read_u8(bytes, offset + 8)?,
            weight: read_u32(bytes, offset + 9)?,
        });
    }
    Ok(rows)
}

fn decode_node_states(bytes: &[u8], count: u32) -> GraphResult<Vec<SegmentNodeState>> {
    let mut rows = Vec::with_capacity(count as usize);
    for idx in 0..count as usize {
        let offset = idx * 5;
        rows.push(SegmentNodeState {
            node_idx: read_u32(bytes, offset)?,
            active: decode_bool(read_u8(bytes, offset + 4)?)?,
        });
    }
    Ok(rows)
}

fn decode_resolutions(bytes: &[u8], count: u32) -> GraphResult<Vec<SegmentResolution>> {
    let mut rows = Vec::with_capacity(count as usize);
    for idx in 0..count as usize {
        let offset = idx * 17;
        rows.push(SegmentResolution {
            table_oid: read_u32(bytes, offset)?,
            pk_hash: read_u64(bytes, offset + 4)?,
            node_idx: read_u32(bytes, offset + 12)?,
            tombstone: decode_bool(read_u8(bytes, offset + 16)?)?,
        });
    }
    Ok(rows)
}

fn decode_filters(bytes: &[u8], count: u32) -> GraphResult<Vec<SegmentFilterValue>> {
    let mut rows = Vec::with_capacity(count as usize);
    for idx in 0..count as usize {
        let offset = idx * 13;
        rows.push(SegmentFilterValue {
            node_idx: read_u32(bytes, offset)?,
            column_id: read_u32(bytes, offset + 4)?,
            value: read_u32(bytes, offset + 8)?,
            tombstone: decode_bool(read_u8(bytes, offset + 12)?)?,
        });
    }
    Ok(rows)
}

fn decode_tenants(bytes: &[u8], count: u32) -> GraphResult<Vec<SegmentTenant>> {
    let mut rows = Vec::with_capacity(count as usize);
    for idx in 0..count as usize {
        let offset = idx * 13;
        rows.push(SegmentTenant {
            node_idx: read_u32(bytes, offset)?,
            tenant_hash: read_u64(bytes, offset + 4)?,
            tombstone: decode_bool(read_u8(bytes, offset + 12)?)?,
        });
    }
    Ok(rows)
}

fn decode_bool(raw: u8) -> GraphResult<bool> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(segment_corrupt("boolean section value must be 0 or 1")),
    }
}

fn encode_direction(direction: TraversalDirection) -> u8 {
    match direction {
        TraversalDirection::Any => 0,
        TraversalDirection::Out => 1,
        TraversalDirection::In => 2,
    }
}

fn decode_direction(raw: u8) -> GraphResult<TraversalDirection> {
    match raw {
        0 => Ok(TraversalDirection::Any),
        1 => Ok(TraversalDirection::Out),
        2 => Ok(TraversalDirection::In),
        _ => Err(segment_corrupt("unknown segment direction")),
    }
}

fn checksum_segment_bytes(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(&bytes[..CHECKSUM_OFFSET]);
    hasher.update(&[0; 4]);
    hasher.update(&bytes[CHECKSUM_OFFSET + 4..]);
    hasher.finalize()
}

fn read_u8(bytes: &[u8], offset: usize) -> GraphResult<u8> {
    bytes
        .get(offset)
        .copied()
        .ok_or_else(|| segment_corrupt("unexpected end of segment"))
}

fn read_u32(bytes: &[u8], offset: usize) -> GraphResult<u32> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| segment_corrupt("unexpected end of segment"))?;
    Ok(u32::from_le_bytes(
        raw.try_into().expect("slice length is 4"),
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> GraphResult<u64> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| segment_corrupt("unexpected end of segment"))?;
    Ok(u64::from_le_bytes(
        raw.try_into().expect("slice length is 8"),
    ))
}

fn read_i64(bytes: &[u8], offset: usize) -> GraphResult<i64> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| segment_corrupt("unexpected end of segment"))?;
    Ok(i64::from_le_bytes(
        raw.try_into().expect("slice length is 8"),
    ))
}

fn write_u32_at(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i64_at(bytes: &mut [u8], offset: usize, value: i64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_u64_at(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn segment_corrupt(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: format!("projection segment: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_segment_roundtrips_edge_topology_weight_and_delete_sections() {
        let mut segment =
            DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 8, 42)
                .expect("segment constructs");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 2,
        });
        segment.edge_deletes.push(SegmentEdge {
            source: 2,
            target: 3,
            type_id: 4,
        });
        segment.edge_weights.push(SegmentEdgeWeight {
            source: 0,
            target: 1,
            type_id: 2,
            weight: 7,
        });

        let decoded = DeltaSegment::from_bytes(&segment.to_bytes().expect("segment encodes"))
            .expect("segment decodes");

        assert_eq!(decoded.header.kind, SegmentKind::Edge);
        assert_eq!(decoded.header.direction, TraversalDirection::Out);
        assert_eq!(decoded.edge_inserts, segment.edge_inserts);
        assert_eq!(decoded.edge_deletes, segment.edge_deletes);
        assert_eq!(decoded.edge_weights, segment.edge_weights);
    }

    #[test]
    fn delta_segment_roundtrips_node_resolution_filter_tenant_sections() {
        let mut segment =
            DeltaSegment::new(SegmentKind::Node, 1, TraversalDirection::Any, 0, 8, 43)
                .expect("segment constructs");
        segment.node_states.push(SegmentNodeState {
            node_idx: 1,
            active: false,
        });
        segment.resolutions.push(SegmentResolution {
            table_oid: 100,
            pk_hash: 10_001,
            node_idx: 1,
            tombstone: false,
        });
        segment.filters.push(SegmentFilterValue {
            node_idx: 1,
            column_id: 7,
            value: 99,
            tombstone: true,
        });
        segment.tenants.push(SegmentTenant {
            node_idx: 1,
            tenant_hash: 22_002,
            tombstone: false,
        });

        let decoded = DeltaSegment::from_bytes(&segment.to_bytes().expect("segment encodes"))
            .expect("segment decodes");

        assert_eq!(decoded.header.kind, SegmentKind::Node);
        assert_eq!(decoded.header.level, 1);
        assert_eq!(decoded.node_states, segment.node_states);
        assert_eq!(decoded.resolutions, segment.resolutions);
        assert_eq!(decoded.filters, segment.filters);
        assert_eq!(decoded.tenants, segment.tenants);
    }

    #[test]
    fn delta_segment_rejects_corrupt_offsets_checksum_and_reserved_flags() {
        let mut segment =
            DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 8, 42)
                .expect("segment constructs");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 1,
            type_id: 2,
        });
        let bytes = segment.to_bytes().expect("segment encodes");

        let mut bad_offset = bytes.clone();
        write_u64_at(&mut bad_offset, 64, (HEADER_SIZE + 1) as u64);
        rewrite_checksum(&mut bad_offset);
        let offset_err = DeltaSegment::from_bytes(&bad_offset).expect_err("bad offset rejects");

        let mut bad_checksum = bytes.clone();
        bad_checksum[CHECKSUM_OFFSET] ^= 0xff;
        let checksum_err =
            DeltaSegment::from_bytes(&bad_checksum).expect_err("bad checksum rejects");

        let mut bad_reserved = bytes;
        bad_reserved[RESERVED_OFFSET] = 1;
        rewrite_checksum(&mut bad_reserved);
        let reserved_err =
            DeltaSegment::from_bytes(&bad_reserved).expect_err("bad reserved flags reject");

        assert!(matches!(offset_err, GraphError::CorruptFile { .. }));
        assert!(matches!(checksum_err, GraphError::CorruptFile { .. }));
        assert!(matches!(reserved_err, GraphError::CorruptFile { .. }));

        for offset in [15, 60, 120] {
            let mut bad_padding = segment.to_bytes().expect("segment encodes");
            bad_padding[offset] = 1;
            rewrite_checksum(&mut bad_padding);
            let padding_err =
                DeltaSegment::from_bytes(&bad_padding).expect_err("bad padding rejects");
            assert!(matches!(padding_err, GraphError::CorruptFile { .. }));
        }
    }

    #[test]
    fn delta_segment_allows_targets_outside_source_range() {
        let mut segment =
            DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 2, 42)
                .expect("segment constructs");
        segment.edge_inserts.push(SegmentEdge {
            source: 0,
            target: 99,
            type_id: 2,
        });

        let decoded = DeltaSegment::from_bytes(&segment.to_bytes().expect("segment encodes"))
            .expect("segment decodes");

        assert_eq!(decoded.edge_inserts[0].target, 99);
    }

    #[test]
    fn delta_segment_rejects_source_out_of_range() {
        let mut segment =
            DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 2, 42)
                .expect("segment constructs");
        segment.edge_inserts.push(SegmentEdge {
            source: 2,
            target: 0,
            type_id: 2,
        });

        let err = segment
            .to_bytes()
            .expect_err("source outside source range rejects");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn delta_segment_loader_never_panics_on_arbitrary_bytes() {
        let cases: &[&[u8]] = &[
            b"",
            b"PGGSEG01",
            &[0xff; HEADER_SIZE],
            b"not a segment but definitely bytes",
        ];

        for case in cases {
            assert!(DeltaSegment::from_bytes(case).is_err());
        }
    }

    fn rewrite_checksum(bytes: &mut [u8]) {
        write_u32_at(bytes, CHECKSUM_OFFSET, 0);
        let checksum = checksum_segment_bytes(bytes);
        write_u32_at(bytes, CHECKSUM_OFFSET, checksum);
    }
}
