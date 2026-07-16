//! Direct persisted-build artifact assembly.
//!
//! Build scanners stage lexicographically ordered records in governed run
//! files. This module replays those validated runs directly into the canonical
//! persistence writer, avoiding an intermediate owned graph engine and any
//! graph-sized duplicate section buffers.

use std::path::Path;

use crate::build_runs::StagedRun;
use crate::builder::RegisteredFilterColumn;
use crate::config::ProjectionMode;
use crate::filter_index::{FilterColumnType, FILTER_CATALOG_HEADER_SIZE, FILTER_DESCRIPTOR_SIZE};
use crate::persistence::{write_direct_graph_file, DirectArtifactMetadata, DirectArtifactWriter};
use crate::resource::{ResourceGovernor, ResourcePhase};
use crate::safety::{GraphError, GraphResult};

const SECTION_COUNT: usize = 26;
const EDGE_RECORD_KEY_BYTES: usize = 14;
const EDGE_RECORD_VALUE_BYTES: usize = 14;
const IDENTITY_VALUE_HEADER_BYTES: usize = 12;

/// Bounded run inputs used to assemble every semantic v6 artifact section.
///
/// Edge records have a stable manual codec. Their key is
/// `source:u32be | target:u32be | type:u8 | schema_reversed:u8 |
/// relationship_id:u32be`; their value is `target:u32le | type:u8 |
/// schema_reversed:u8 | weight:u32le | relationship_id:u32le`. A zero weight
/// represents an absent source weight and is normalized to the runtime default
/// of one when the graph has a weighted CSR. Identity values are
/// `mapping_id:u64le | key_len:u32le | UTF-8 key` and are keyed by dense
/// `relationship_id:u32be`.
pub(crate) struct SemanticDirectBuild<'run, 'governor> {
    pub(crate) node_count: u32,
    pub(crate) forward_edge_count: u32,
    pub(crate) inbound_edge_count: u32,
    pub(crate) has_forward_weights: bool,
    pub(crate) has_inbound_weights: bool,
    pub(crate) has_unidirectional_edges: bool,
    pub(crate) applied_sync_id: i64,
    pub(crate) projection_mode: ProjectionMode,
    pub(crate) nodes: Option<&'run StagedRun<'governor>>,
    pub(crate) forward_edges: Option<&'run StagedRun<'governor>>,
    pub(crate) inbound_edges: Option<&'run StagedRun<'governor>>,
    pub(crate) resolution: Option<&'run StagedRun<'governor>>,
    pub(crate) filters: Option<&'run StagedRun<'governor>>,
    pub(crate) filter_dictionary: Option<&'run StagedRun<'governor>>,
    pub(crate) relationship_identities: Option<&'run StagedRun<'governor>>,
    pub(crate) tenant_tokens: Option<&'run StagedRun<'governor>>,
    pub(crate) tenant_dictionary: Option<&'run StagedRun<'governor>>,
    pub(crate) filter_columns: &'run [RegisteredFilterColumn],
    pub(crate) edge_type_registry: &'run [String],
    pub(crate) tenanted_table_oids: &'run [u32],
    pub(crate) max_record_bytes: usize,
    pub(crate) governor: &'run ResourceGovernor,
}

#[derive(Debug)]
struct FilterPlan<'a> {
    table_oid: u32,
    column_type: FilterColumnType,
    column_name: &'a str,
    row_count: u32,
    data_offset: u64,
    data_len: u64,
    dictionary_offset: u64,
    dictionary_len: u64,
    dictionary_count: u32,
}

/// One direct artifact section, either a small fixed-format prefix or a
/// validated external-sort run whose values form the section payload.
#[cfg(test)]
#[cfg(test)]
pub(crate) enum DirectSection<'run, 'governor> {
    /// Small bounded metadata encoded by the scanner.
    Bytes(&'run [u8]),
    /// Values from a checksummed, sorted run; keys are ordering metadata and
    /// are not copied into the artifact.
    Run {
        run: &'run StagedRun<'governor>,
        max_record_bytes: usize,
    },
    /// A zero-length section.
    Empty,
}

#[cfg(test)]
#[cfg(test)]
impl DirectSection<'_, '_> {
    fn emit(&self, writer: &mut DirectArtifactWriter) -> GraphResult<()> {
        match self {
            Self::Bytes(bytes) => writer.write_bytes(bytes),
            Self::Run {
                run,
                max_record_bytes,
            } => run.replay(*max_record_bytes, |record| {
                writer.write_bytes(&record.value)
            }),
            Self::Empty => Ok(()),
        }
    }

    fn planned_len(&self) -> GraphResult<u64> {
        match self {
            Self::Bytes(bytes) => Ok(bytes.len() as u64),
            Self::Run {
                run,
                max_record_bytes,
            } => {
                let mut len = 0_u64;
                run.replay(*max_record_bytes, |record| {
                    len = len.checked_add(record.value.len() as u64).ok_or_else(|| {
                        GraphError::Internal("direct section length overflowed".into())
                    })?;
                    Ok(())
                })?;
                Ok(len)
            }
            Self::Empty => Ok(0),
        }
    }
}

/// Validated counts and ordered section streams for one persisted build.
#[cfg(test)]
#[cfg(test)]
pub(crate) struct DirectBuildArtifact<'run, 'governor> {
    pub(crate) node_count: u32,
    pub(crate) forward_edge_count: u32,
    pub(crate) inbound_edge_count: u32,
    pub(crate) has_forward_weights: bool,
    pub(crate) has_inbound_weights: bool,
    pub(crate) has_unidirectional_edges: bool,
    pub(crate) applied_sync_id: i64,
    pub(crate) projection_mode: ProjectionMode,
    pub(crate) sections: [DirectSection<'run, 'governor>; SECTION_COUNT],
    pub(crate) governor: &'run ResourceGovernor,
}

#[cfg(test)]
#[cfg(test)]
impl DirectBuildArtifact<'_, '_> {
    fn validate_shape(&self) -> GraphResult<()> {
        let expected_active = usize::try_from(self.node_count)
            .ok()
            .and_then(|count| count.checked_add(7))
            .map(|bits| bits / 8)
            .ok_or_else(|| GraphError::Internal("active bitmap size overflowed".into()))?;
        if let DirectSection::Bytes(bytes) = &self.sections[0] {
            if bytes.len() != expected_active {
                return Err(GraphError::Internal(format!(
                    "direct active bitmap has {} bytes, expected {expected_active}",
                    bytes.len()
                )));
            }
        }
        if self.forward_edge_count != self.inbound_edge_count {
            return Err(GraphError::Internal(
                "forward and inbound direct edge counts differ".into(),
            ));
        }
        Ok(())
    }
}

/// Stream a prepared direct build into an atomically published v6 artifact.
///
/// Every run is checksum-validated before its values are admitted to the
/// artifact. The caller should load and validate the resulting candidate
/// before publishing a projection manifest that references it.
#[cfg(test)]
#[cfg(test)]
pub(crate) fn write_prepared_artifact(
    path: &Path,
    artifact: &DirectBuildArtifact<'_, '_>,
) -> GraphResult<()> {
    artifact.validate_shape()?;
    let metadata = DirectArtifactMetadata {
        node_count: artifact.node_count,
        forward_edge_count: artifact.forward_edge_count,
        inbound_edge_count: artifact.inbound_edge_count,
        has_forward_weights: artifact.has_forward_weights,
        has_inbound_weights: artifact.has_inbound_weights,
        has_unidirectional_edges: artifact.has_unidirectional_edges,
        applied_sync_id: artifact.applied_sync_id,
        projection_mode: artifact.projection_mode,
    };
    let mut section_lengths = [0_u64; SECTION_COUNT];
    for (index, section) in artifact.sections.iter().enumerate() {
        section_lengths[index] = section.planned_len()?;
    }
    write_direct_graph_file(
        path,
        &metadata,
        section_lengths,
        artifact.governor,
        |writer| {
            for (section_index, section) in artifact.sections.iter().enumerate() {
                writer.begin_section(section_index)?;
                section.emit(writer)?;
            }
            Ok(())
        },
    )
}

/// Assemble a canonical v6 artifact from validated, bounded scanner runs.
///
/// Graph-sized output is streamed directly to the candidate. Runs are replayed
/// when parallel persisted arrays require different projections of the same
/// record; every replay revalidates the private run before bytes are emitted.
///
/// # Errors
///
/// Returns an error when a run violates its documented codec or dense ordering,
/// when section arithmetic overflows, or when the candidate cannot be written.
pub(crate) fn write_semantic_artifact(
    path: &Path,
    build: &SemanticDirectBuild<'_, '_>,
) -> GraphResult<()> {
    validate_semantic_shape(build)?;
    let plan_bytes = build
        .filter_columns
        .len()
        .checked_mul(std::mem::size_of::<FilterPlan<'_>>())
        .and_then(|bytes| bytes.checked_add(build.max_record_bytes.saturating_mul(2)))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("direct build metadata size overflowed".into()))?;
    let _metadata_memory = build
        .governor
        .reserve_memory(ResourcePhase::Persistence, plan_bytes)
        .map_err(crate::safety::resource_limit_error)?;
    let filter_plan = plan_filters(build)?;
    let section_lengths = plan_section_lengths(build, &filter_plan)?;
    let metadata = DirectArtifactMetadata {
        node_count: build.node_count,
        forward_edge_count: build.forward_edge_count,
        inbound_edge_count: build.inbound_edge_count,
        has_forward_weights: build.has_forward_weights,
        has_inbound_weights: build.has_inbound_weights,
        has_unidirectional_edges: build.has_unidirectional_edges,
        applied_sync_id: build.applied_sync_id,
        projection_mode: build.projection_mode,
    };
    write_direct_graph_file(path, &metadata, section_lengths, build.governor, |writer| {
        writer.begin_section(0)?;
        emit_active_bitmap(writer, build.node_count)?;
        writer.begin_section(1)?;
        replay_nodes(build, |_, table_oid, _| write_u32(writer, table_oid))?;
        writer.begin_section(2)?;
        emit_primary_key_offsets(writer, build)?;
        writer.begin_section(3)?;
        replay_nodes(build, |_, _, primary_key| writer.write_bytes(primary_key))?;
        emit_csr(
            writer,
            4,
            build,
            build.forward_edges,
            build.forward_edge_count,
            build.has_forward_weights,
        )?;
        emit_csr(
            writer,
            10,
            build,
            build.inbound_edges,
            build.inbound_edge_count,
            build.has_inbound_weights,
        )?;
        writer.begin_section(16)?;
        emit_resolution(writer, build)?;
        writer.begin_section(17)?;
        emit_filter_catalog(writer, &filter_plan)?;
        writer.begin_section(18)?;
        emit_filter_data(writer, build, &filter_plan)?;
        writer.begin_section(19)?;
        emit_filter_dictionaries(writer, build, &filter_plan)?;
        writer.begin_section(20)?;
        emit_registry(writer, build.edge_type_registry)?;
        writer.begin_section(21)?;
        emit_identity_descriptors(writer, build)?;
        writer.begin_section(22)?;
        emit_identity_keys(writer, build)?;
        writer.begin_section(23)?;
        emit_tenanted_oids(writer, build.tenanted_table_oids)?;
        writer.begin_section(24)?;
        emit_tenant_dictionary(writer, build)?;
        writer.begin_section(25)?;
        emit_tenant_tokens(writer, build)?;
        Ok(())
    })
}

fn validate_semantic_shape(build: &SemanticDirectBuild<'_, '_>) -> GraphResult<()> {
    if build.forward_edge_count != build.inbound_edge_count
        || build.has_forward_weights != build.has_inbound_weights
        || build.edge_type_registry.len() > u8::MAX as usize
        || build
            .edge_type_registry
            .first()
            .is_none_or(|label| !label.is_empty())
        || build
            .edge_type_registry
            .iter()
            .skip(1)
            .any(String::is_empty)
    {
        return Err(GraphError::Internal(
            "invalid direct artifact counts, weight mode, or edge type registry".into(),
        ));
    }
    if build
        .edge_type_registry
        .iter()
        .enumerate()
        .any(|(index, label)| build.edge_type_registry[..index].contains(label))
    {
        return Err(GraphError::Internal(
            "direct edge type registry contains duplicate labels".into(),
        ));
    }
    for (run, count, label) in [
        (build.forward_edges, build.forward_edge_count, "forward"),
        (build.inbound_edges, build.inbound_edge_count, "inbound"),
    ] {
        let actual = run.map_or(0, StagedRun::record_count);
        if actual != u64::from(count) {
            return Err(GraphError::Internal(format!(
                "direct {label} run has {actual} records, expected {count}"
            )));
        }
    }
    let nodes = build.nodes.map_or(0, StagedRun::record_count);
    if nodes != u64::from(build.node_count) {
        return Err(GraphError::Internal(format!(
            "direct node run has {nodes} records, expected {}",
            build.node_count
        )));
    }
    Ok(())
}

fn plan_section_lengths(
    build: &SemanticDirectBuild<'_, '_>,
    filters: &[FilterPlan<'_>],
) -> GraphResult<[u64; SECTION_COUNT]> {
    let mut lengths = [0_u64; SECTION_COUNT];
    lengths[0] = u64::from(build.node_count).div_ceil(8);
    lengths[1] = u64::from(build.node_count)
        .checked_mul(4)
        .ok_or_else(|| GraphError::Internal("table OID section length overflowed".into()))?;
    lengths[2] = u64::from(build.node_count)
        .checked_add(1)
        .and_then(|count| count.checked_mul(8))
        .ok_or_else(|| GraphError::Internal("primary-key offsets overflowed".into()))?;
    replay_nodes(build, |_, _, primary_key| {
        lengths[3] = lengths[3]
            .checked_add(primary_key.len() as u64)
            .ok_or_else(|| GraphError::Internal("primary-key bytes overflowed".into()))?;
        Ok(())
    })?;
    for (base, count, weighted) in [
        (4, build.forward_edge_count, build.has_forward_weights),
        (10, build.inbound_edge_count, build.has_inbound_weights),
    ] {
        lengths[base] = u64::from(build.node_count)
            .checked_add(1)
            .and_then(|nodes| nodes.checked_mul(4))
            .ok_or_else(|| GraphError::Internal("CSR offsets length overflowed".into()))?;
        lengths[base + 1] = u64::from(count) * 4;
        lengths[base + 2] = u64::from(count);
        lengths[base + 3] = u64::from(count);
        lengths[base + 4] = if weighted { u64::from(count) * 4 } else { 0 };
        lengths[base + 5] = u64::from(count) * 4;
    }
    lengths[16] = u64::from(build.node_count)
        .checked_mul(16)
        .and_then(|bytes| bytes.checked_add(4))
        .ok_or_else(|| GraphError::Internal("resolution length overflowed".into()))?;
    let names_len = filters
        .iter()
        .try_fold(0_u64, |total, plan| {
            total.checked_add(plan.column_name.len() as u64)
        })
        .ok_or_else(|| GraphError::Internal("filter names length overflowed".into()))?;
    lengths[17] = (FILTER_CATALOG_HEADER_SIZE as u64)
        .checked_add(
            (filters.len() as u64)
                .checked_mul(FILTER_DESCRIPTOR_SIZE as u64)
                .ok_or_else(|| GraphError::Internal("filter catalog length overflowed".into()))?,
        )
        .and_then(|bytes| bytes.checked_add(names_len))
        .ok_or_else(|| GraphError::Internal("filter catalog length overflowed".into()))?;
    lengths[18] = filters
        .iter()
        .try_fold(0_u64, |total, plan| total.checked_add(plan.data_len))
        .ok_or_else(|| GraphError::Internal("filter data length overflowed".into()))?;
    lengths[19] = filters
        .iter()
        .try_fold(0_u64, |total, plan| total.checked_add(plan.dictionary_len))
        .ok_or_else(|| GraphError::Internal("filter dictionary length overflowed".into()))?;
    let registry_payload = build
        .edge_type_registry
        .iter()
        .try_fold(0_u64, |total, label| total.checked_add(label.len() as u64))
        .ok_or_else(|| GraphError::Internal("edge registry length overflowed".into()))?;
    lengths[20] = (build.edge_type_registry.len() as u64)
        .checked_add(1)
        .and_then(|count| count.checked_mul(8))
        .and_then(|offsets| offsets.checked_add(4))
        .and_then(|bytes| bytes.checked_add(registry_payload))
        .ok_or_else(|| GraphError::Internal("edge registry length overflowed".into()))?;
    let mut identity_count = 0_u64;
    replay_identities(build, |_, key| {
        identity_count = identity_count
            .checked_add(1)
            .ok_or_else(|| GraphError::Internal("identity count overflowed".into()))?;
        lengths[22] = lengths[22]
            .checked_add(key.len() as u64)
            .ok_or_else(|| GraphError::Internal("identity key length overflowed".into()))?;
        Ok(())
    })?;
    lengths[21] = identity_count
        .checked_add(1)
        .and_then(|count| count.checked_mul(16))
        .ok_or_else(|| GraphError::Internal("identity descriptor length overflowed".into()))?;
    lengths[23] = (build.tenanted_table_oids.len() as u64)
        .checked_mul(4)
        .ok_or_else(|| GraphError::Internal("tenant OID length overflowed".into()))?;
    let (tenant_count, tenant_payload) = inspect_tenant_dictionary(build, |_| Ok(()))?;
    lengths[24] = u64::from(tenant_count)
        .checked_add(1)
        .and_then(|count| count.checked_mul(8))
        .and_then(|offsets| offsets.checked_add(4))
        .and_then(|bytes| bytes.checked_add(tenant_payload))
        .ok_or_else(|| GraphError::Internal("tenant dictionary length overflowed".into()))?;
    lengths[25] = u64::from(build.node_count) * 4;
    Ok(lengths)
}

fn write_u32(writer: &mut DirectArtifactWriter, value: u32) -> GraphResult<()> {
    writer.write_bytes(&value.to_le_bytes())
}

fn write_u64(writer: &mut DirectArtifactWriter, value: u64) -> GraphResult<()> {
    writer.write_bytes(&value.to_le_bytes())
}

fn emit_active_bitmap(writer: &mut DirectArtifactWriter, node_count: u32) -> GraphResult<()> {
    let full_bytes = node_count / 8;
    for _ in 0..full_bytes {
        writer.write_bytes(&[u8::MAX])?;
    }
    let remainder = node_count % 8;
    if remainder != 0 {
        writer.write_bytes(&[((1_u16 << remainder) - 1) as u8])?;
    }
    Ok(())
}

fn replay_nodes(
    build: &SemanticDirectBuild<'_, '_>,
    mut visit: impl FnMut(u32, u32, &[u8]) -> GraphResult<()>,
) -> GraphResult<()> {
    let Some(run) = build.nodes else {
        return (build.node_count == 0)
            .then_some(())
            .ok_or_else(|| GraphError::Internal("direct node run is missing".into()));
    };
    let mut expected = 0_u32;
    replay_checked(build, run, |record| {
        if record.key.len() != 4 || record.value.len() < 12 {
            return Err(GraphError::Internal(
                "invalid direct node record shape".into(),
            ));
        }
        let key_idx = u32::from_be_bytes(read_array(&record.key, 0)?);
        let node_idx = u32::from_le_bytes(read_array(&record.value, 0)?);
        let table_oid = u32::from_le_bytes(read_array(&record.value, 4)?);
        let pk_len = usize::try_from(u32::from_le_bytes(read_array(&record.value, 8)?))
            .map_err(|_| GraphError::Internal("node primary key length exceeds usize".into()))?;
        if key_idx != expected || node_idx != expected || record.value.len() != 12 + pk_len {
            return Err(GraphError::Internal(
                "direct node records are not dense or have invalid lengths".into(),
            ));
        }
        std::str::from_utf8(&record.value[12..])
            .map_err(|_| GraphError::Internal("direct node primary key is not UTF-8".into()))?;
        visit(node_idx, table_oid, &record.value[12..])?;
        expected = expected
            .checked_add(1)
            .ok_or_else(|| GraphError::Internal("direct node index overflowed".into()))?;
        Ok(())
    })?;
    if expected != build.node_count {
        return Err(GraphError::Internal("direct node run ended early".into()));
    }
    Ok(())
}

fn emit_primary_key_offsets(
    writer: &mut DirectArtifactWriter,
    build: &SemanticDirectBuild<'_, '_>,
) -> GraphResult<()> {
    let mut offset = 0_u64;
    write_u64(writer, offset)?;
    replay_nodes(build, |_, _, primary_key| {
        offset = offset
            .checked_add(primary_key.len() as u64)
            .ok_or_else(|| GraphError::Internal("primary key section exceeds u64".into()))?;
        write_u64(writer, offset)
    })
}

#[derive(Clone, Copy)]
struct DecodedEdge {
    source: u32,
    target: u32,
    type_id: u8,
    schema_reversed: u8,
    weight: u32,
    relationship_id: u32,
}

fn decode_edge(record: &crate::build_runs::RunRecord) -> GraphResult<DecodedEdge> {
    if record.key.len() != EDGE_RECORD_KEY_BYTES || record.value.len() != EDGE_RECORD_VALUE_BYTES {
        return Err(GraphError::Internal(
            "invalid direct edge record shape".into(),
        ));
    }
    let edge = DecodedEdge {
        source: u32::from_be_bytes(read_array(&record.key, 0)?),
        target: u32::from_le_bytes(read_array(&record.value, 0)?),
        type_id: record.value[4],
        schema_reversed: record.value[5],
        weight: u32::from_le_bytes(read_array(&record.value, 6)?),
        relationship_id: u32::from_le_bytes(read_array(&record.value, 10)?),
    };
    if u32::from_be_bytes(read_array(&record.key, 4)?) != edge.target
        || record.key[8] != edge.type_id
        || record.key[9] != edge.schema_reversed
        || u32::from_be_bytes(read_array(&record.key, 10)?) != edge.relationship_id
        || edge.schema_reversed > 1
    {
        return Err(GraphError::Internal(
            "direct edge key/value mismatch".into(),
        ));
    }
    Ok(edge)
}

fn replay_edges(
    governor: &ResourceGovernor,
    run: Option<&StagedRun<'_>>,
    node_count: u32,
    edge_count: u32,
    max_record_bytes: usize,
    mut visit: impl FnMut(DecodedEdge) -> GraphResult<()>,
) -> GraphResult<()> {
    let Some(run) = run else {
        return (edge_count == 0)
            .then_some(())
            .ok_or_else(|| GraphError::Internal("direct edge run is missing".into()));
    };
    let mut count = 0_u32;
    replay_checked_raw(governor, run, max_record_bytes, |record| {
        let edge = decode_edge(&record)?;
        if edge.source >= node_count || edge.target >= node_count {
            return Err(GraphError::Internal(
                "direct edge endpoint is out of bounds".into(),
            ));
        }
        visit(edge)?;
        count = count
            .checked_add(1)
            .ok_or_else(|| GraphError::Internal("direct edge count overflowed".into()))?;
        Ok(())
    })?;
    if count != edge_count {
        return Err(GraphError::Internal("direct edge run count changed".into()));
    }
    Ok(())
}

fn emit_csr(
    writer: &mut DirectArtifactWriter,
    base: usize,
    build: &SemanticDirectBuild<'_, '_>,
    run: Option<&StagedRun<'_>>,
    edge_count: u32,
    has_weights: bool,
) -> GraphResult<()> {
    writer.begin_section(base)?;
    write_u32(writer, 0)?;
    let mut current_source = 0_u32;
    let mut seen_edges = 0_u32;
    replay_edges(
        build.governor,
        run,
        build.node_count,
        edge_count,
        build.max_record_bytes,
        |edge| {
            while current_source < edge.source {
                write_u32(writer, seen_edges)?;
                current_source += 1;
            }
            seen_edges += 1;
            Ok(())
        },
    )?;
    while current_source < build.node_count {
        write_u32(writer, seen_edges)?;
        current_source += 1;
    }
    writer.begin_section(base + 1)?;
    replay_edges(
        build.governor,
        run,
        build.node_count,
        edge_count,
        build.max_record_bytes,
        |edge| write_u32(writer, edge.target),
    )?;
    writer.begin_section(base + 2)?;
    replay_edges(
        build.governor,
        run,
        build.node_count,
        edge_count,
        build.max_record_bytes,
        |edge| writer.write_bytes(&[edge.type_id]),
    )?;
    writer.begin_section(base + 3)?;
    replay_edges(
        build.governor,
        run,
        build.node_count,
        edge_count,
        build.max_record_bytes,
        |edge| writer.write_bytes(&[edge.schema_reversed]),
    )?;
    writer.begin_section(base + 4)?;
    if has_weights {
        replay_edges(
            build.governor,
            run,
            build.node_count,
            edge_count,
            build.max_record_bytes,
            |edge| write_u32(writer, if edge.weight == 0 { 1 } else { edge.weight }),
        )?;
    }
    writer.begin_section(base + 5)?;
    replay_edges(
        build.governor,
        run,
        build.node_count,
        edge_count,
        build.max_record_bytes,
        |edge| write_u32(writer, edge.relationship_id),
    )
}

fn emit_resolution(
    writer: &mut DirectArtifactWriter,
    build: &SemanticDirectBuild<'_, '_>,
) -> GraphResult<()> {
    let count = build.resolution.map_or(0, StagedRun::record_count);
    let count = u32::try_from(count)
        .map_err(|_| GraphError::Internal("resolution count exceeds u32".into()))?;
    if count != build.node_count {
        return Err(GraphError::Internal(
            "resolution run must contain one record per node".into(),
        ));
    }
    write_u32(writer, count)?;
    if let Some(run) = build.resolution {
        replay_checked(build, run, |record| {
            if record.key.len() != 16 || record.value.len() != 16 {
                return Err(GraphError::Internal(
                    "invalid resolution record shape".into(),
                ));
            }
            writer.write_bytes(&record.value)
        })?;
    }
    Ok(())
}

fn plan_filters<'a>(build: &'a SemanticDirectBuild<'_, '_>) -> GraphResult<Vec<FilterPlan<'a>>> {
    let mut plans = Vec::new();
    plans
        .try_reserve_exact(build.filter_columns.len())
        .map_err(|error| allocation_error("filter plans", error))?;
    let present_len = u64::from(build.node_count).div_ceil(8);
    let mut data_offset = 0_u64;
    for column in build.filter_columns {
        let column_type = FilterColumnType::parse(&column.column_type)
            .map_err(|reason| GraphError::InvalidFilter { reason })?;
        let values_len = u64::from(build.node_count)
            .checked_mul(column_type.persisted_width() as u64)
            .ok_or_else(|| GraphError::Internal("filter data length overflowed".into()))?;
        let data_len = present_len
            .checked_add(values_len)
            .ok_or_else(|| GraphError::Internal("filter data length overflowed".into()))?;
        plans.push(FilterPlan {
            table_oid: column.table_oid,
            column_type,
            column_name: &column.column_name,
            row_count: 0,
            data_offset,
            data_len,
            dictionary_offset: 0,
            dictionary_len: 0,
            dictionary_count: 0,
        });
        data_offset = data_offset
            .checked_add(data_len)
            .ok_or_else(|| GraphError::Internal("filter data length overflowed".into()))?;
    }

    if let Some(run) = build.filters {
        replay_checked(build, run, |record| {
            let (ordinal, node_idx) = decode_filter_key(&record.key)?;
            let plan = plans.get_mut(ordinal as usize).ok_or_else(|| {
                GraphError::Internal("filter run references unknown column".into())
            })?;
            if node_idx >= build.node_count {
                return Err(GraphError::Internal(
                    "filter node index is out of bounds".into(),
                ));
            }
            validate_filter_value(plan.column_type, &record.value)?;
            plan.row_count = plan
                .row_count
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("filter row count overflowed".into()))?;
            Ok(())
        })?;
    }

    inspect_filter_dictionaries(build, &mut plans)?;
    let mut dictionary_offset = 0_u64;
    for plan in &mut plans {
        plan.dictionary_offset = dictionary_offset;
        if plan.column_type == FilterColumnType::Text {
            plan.dictionary_len = u64::from(plan.dictionary_count)
                .checked_add(1)
                .and_then(|offsets| offsets.checked_mul(8))
                .and_then(|offsets| offsets.checked_add(plan.dictionary_len))
                .ok_or_else(|| {
                    GraphError::Internal("filter dictionary length overflowed".into())
                })?;
        }
        dictionary_offset = dictionary_offset
            .checked_add(plan.dictionary_len)
            .ok_or_else(|| GraphError::Internal("filter dictionary length overflowed".into()))?;
    }
    Ok(plans)
}

fn decode_filter_key(key: &[u8]) -> GraphResult<(u32, u32)> {
    if key.len() != 8 {
        return Err(GraphError::Internal("invalid filter record key".into()));
    }
    Ok((
        u32::from_be_bytes(read_array(key, 0)?),
        u32::from_be_bytes(read_array(key, 4)?),
    ))
}

fn validate_filter_value(column_type: FilterColumnType, value: &[u8]) -> GraphResult<&[u8]> {
    let (tag, width) = match column_type {
        FilterColumnType::Numeric => (1, 8),
        FilterColumnType::Boolean => (2, 1),
        FilterColumnType::Text => (3, 4),
        FilterColumnType::Date => (4, 8),
        FilterColumnType::Timestamptz => (5, 8),
        FilterColumnType::Uuid => (6, 16),
    };
    if value.len() != width + 1 || value[0] != tag {
        return Err(GraphError::Internal(
            "filter value does not match its registered type".into(),
        ));
    }
    if column_type == FilterColumnType::Boolean && value[1] > 1 {
        return Err(GraphError::Internal(
            "invalid persisted boolean filter".into(),
        ));
    }
    Ok(&value[1..])
}

fn decode_dictionary_key(key: &[u8]) -> GraphResult<(u32, u32)> {
    if key.len() != 8 {
        return Err(GraphError::Internal(
            "invalid normalized dictionary record key".into(),
        ));
    }
    Ok((
        u32::from_be_bytes(read_array(key, 0)?),
        u32::from_be_bytes(read_array(key, 4)?),
    ))
}

fn inspect_filter_dictionaries(
    build: &SemanticDirectBuild<'_, '_>,
    plans: &mut [FilterPlan<'_>],
) -> GraphResult<()> {
    let mut current_ordinal = None;
    let mut expected_token = 0_u32;
    let mut previous: Option<Vec<u8>> = None;
    if let Some(run) = build.filter_dictionary {
        replay_checked(build, run, |record| {
            let (ordinal, token) = decode_dictionary_key(&record.key)?;
            let plan = plans.get_mut(ordinal as usize).ok_or_else(|| {
                GraphError::Internal("dictionary references unknown filter column".into())
            })?;
            if plan.column_type != FilterColumnType::Text {
                return Err(GraphError::Internal(
                    "dictionary references a non-text filter column".into(),
                ));
            }
            if current_ordinal != Some(ordinal) {
                current_ordinal = Some(ordinal);
                expected_token = 0;
                previous = None;
            }
            if token != expected_token
                || previous
                    .as_deref()
                    .is_some_and(|value| value >= record.value.as_slice())
            {
                return Err(GraphError::Internal(
                    "filter dictionary is not dense and strictly lexical".into(),
                ));
            }
            std::str::from_utf8(&record.value)
                .map_err(|_| GraphError::Internal("filter dictionary is not UTF-8".into()))?;
            expected_token = expected_token
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("filter dictionary count overflowed".into()))?;
            plan.dictionary_count = plan
                .dictionary_count
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("filter dictionary count overflowed".into()))?;
            plan.dictionary_len = plan
                .dictionary_len
                .checked_add(record.value.len() as u64)
                .ok_or_else(|| GraphError::Internal("filter dictionary bytes overflowed".into()))?;
            previous = Some(record.value);
            Ok(())
        })?;
    }
    Ok(())
}

fn emit_filter_catalog(
    writer: &mut DirectArtifactWriter,
    plans: &[FilterPlan<'_>],
) -> GraphResult<()> {
    let count = u32::try_from(plans.len())
        .map_err(|_| GraphError::Internal("filter catalog exceeds u32".into()))?;
    let names_len = plans
        .iter()
        .try_fold(0_u64, |total, plan| {
            total.checked_add(plan.column_name.len() as u64)
        })
        .ok_or_else(|| GraphError::Internal("filter names exceed u64".into()))?;
    write_u32(writer, count)?;
    write_u32(writer, FILTER_DESCRIPTOR_SIZE as u32)?;
    write_u64(writer, names_len)?;
    let mut name_offset = 0_u64;
    for plan in plans {
        let name_len = u32::try_from(plan.column_name.len())
            .map_err(|_| GraphError::Internal("filter name exceeds u32".into()))?;
        let mut descriptor = [0_u8; FILTER_DESCRIPTOR_SIZE];
        descriptor[0..4].copy_from_slice(&plan.table_oid.to_le_bytes());
        descriptor[4] = plan.column_type.persisted_tag();
        descriptor[5] = 0; // canonical dense storage
        descriptor[6] = plan.column_type.persisted_width() as u8;
        descriptor[8..16].copy_from_slice(&name_offset.to_le_bytes());
        descriptor[16..20].copy_from_slice(&name_len.to_le_bytes());
        descriptor[20..24].copy_from_slice(&plan.row_count.to_le_bytes());
        descriptor[24..32].copy_from_slice(&plan.data_offset.to_le_bytes());
        descriptor[32..40].copy_from_slice(&plan.data_len.to_le_bytes());
        descriptor[40..48].copy_from_slice(&plan.dictionary_offset.to_le_bytes());
        descriptor[48..56].copy_from_slice(&plan.dictionary_len.to_le_bytes());
        descriptor[56..60].copy_from_slice(&plan.dictionary_count.to_le_bytes());
        writer.write_bytes(&descriptor)?;
        name_offset += u64::from(name_len);
    }
    for plan in plans {
        writer.write_bytes(plan.column_name.as_bytes())?;
    }
    debug_assert_eq!(FILTER_CATALOG_HEADER_SIZE, 16);
    Ok(())
}

fn emit_filter_data(
    writer: &mut DirectArtifactWriter,
    build: &SemanticDirectBuild<'_, '_>,
    plans: &[FilterPlan<'_>],
) -> GraphResult<()> {
    if plans.is_empty() {
        return Ok(());
    }
    // Mark the pre-zeroed section complete even when every registered value is
    // SQL NULL. Subsequent checked writes replace only present slots.
    writer.write_at(0, &[])?;
    let presence_len = u64::from(build.node_count).div_ceil(8);
    let mut pending_presence: Option<(u32, u32, u8)> = None;
    let mut previous_key = None;
    if let Some(run) = build.filters {
        replay_checked(build, run, |record| {
            let (ordinal, node_idx) = decode_filter_key(&record.key)?;
            if previous_key.is_some_and(|previous| previous >= (ordinal, node_idx)) {
                return Err(GraphError::Internal(
                    "filter records are not strictly ordered by column and node".into(),
                ));
            }
            let plan = plans.get(ordinal as usize).ok_or_else(|| {
                GraphError::Internal("filter run references unknown column".into())
            })?;
            let value = validate_filter_value(plan.column_type, &record.value)?;
            if plan.column_type == FilterColumnType::Text
                && u32::from_le_bytes(read_array(value, 0)?) >= plan.dictionary_count
            {
                return Err(GraphError::Internal(
                    "text filter token is outside its dictionary".into(),
                ));
            }
            if node_idx >= build.node_count {
                return Err(GraphError::Internal(
                    "filter node index is out of bounds".into(),
                ));
            }
            let byte_idx = node_idx / 8;
            if pending_presence.is_some_and(|(pending_ordinal, pending_byte, _)| {
                pending_ordinal != ordinal || pending_byte != byte_idx
            }) {
                let Some((pending_ordinal, pending_byte, bits)) = pending_presence.take() else {
                    return Err(GraphError::Internal(
                        "pending filter presence state disappeared".into(),
                    ));
                };
                let pending_plan = &plans[pending_ordinal as usize];
                writer.write_at(pending_plan.data_offset + u64::from(pending_byte), &[bits])?;
            }
            let bits = pending_presence.map_or(0, |(_, _, bits)| bits) | (1 << (node_idx % 8));
            pending_presence = Some((ordinal, byte_idx, bits));
            let value_offset = plan
                .data_offset
                .checked_add(presence_len)
                .and_then(|offset| {
                    u64::from(node_idx)
                        .checked_mul(plan.column_type.persisted_width() as u64)
                        .and_then(|value_offset| offset.checked_add(value_offset))
                })
                .ok_or_else(|| GraphError::Internal("filter value offset overflowed".into()))?;
            writer.write_at(value_offset, value)?;
            previous_key = Some((ordinal, node_idx));
            Ok(())
        })?;
    }
    if let Some((ordinal, byte_idx, bits)) = pending_presence {
        writer.write_at(
            plans[ordinal as usize].data_offset + u64::from(byte_idx),
            &[bits],
        )?;
    }
    Ok(())
}

fn emit_filter_dictionaries(
    writer: &mut DirectArtifactWriter,
    build: &SemanticDirectBuild<'_, '_>,
    plans: &[FilterPlan<'_>],
) -> GraphResult<()> {
    let Some(first_text) = plans
        .iter()
        .find(|plan| plan.column_type == FilterColumnType::Text)
    else {
        return Ok(());
    };
    writer.write_at(first_text.dictionary_offset, &[])?;
    for plan in plans
        .iter()
        .filter(|plan| plan.column_type == FilterColumnType::Text)
    {
        writer.write_at(plan.dictionary_offset, &0_u64.to_le_bytes())?;
    }
    let mut payload_offset = 0_u64;
    let mut current_ordinal = None;
    if let Some(run) = build.filter_dictionary {
        replay_checked(build, run, |record| {
            let (ordinal, token) = decode_dictionary_key(&record.key)?;
            let plan = plans.get(ordinal as usize).ok_or_else(|| {
                GraphError::Internal("dictionary references unknown filter column".into())
            })?;
            if current_ordinal != Some(ordinal) {
                current_ordinal = Some(ordinal);
                payload_offset = 0;
            }
            let payload_start = plan
                .dictionary_offset
                .checked_add((u64::from(plan.dictionary_count) + 1) * 8)
                .and_then(|offset| offset.checked_add(payload_offset))
                .ok_or_else(|| {
                    GraphError::Internal("filter dictionary payload offset overflowed".into())
                })?;
            writer.write_at(payload_start, &record.value)?;
            payload_offset = payload_offset
                .checked_add(record.value.len() as u64)
                .ok_or_else(|| {
                    GraphError::Internal("filter dictionary payload overflowed".into())
                })?;
            let offset_position = plan
                .dictionary_offset
                .checked_add((u64::from(token) + 1) * 8)
                .ok_or_else(|| {
                    GraphError::Internal("filter dictionary offset table overflowed".into())
                })?;
            writer.write_at(offset_position, &payload_offset.to_le_bytes())
        })?;
    }
    Ok(())
}

fn emit_registry(writer: &mut DirectArtifactWriter, registry: &[String]) -> GraphResult<()> {
    write_u32(
        writer,
        u32::try_from(registry.len())
            .map_err(|_| GraphError::Internal("edge registry exceeds u32".into()))?,
    )?;
    let mut offset = 0_u64;
    write_u64(writer, offset)?;
    for label in registry {
        offset = offset
            .checked_add(label.len() as u64)
            .ok_or_else(|| GraphError::Internal("edge registry bytes exceed u64".into()))?;
        write_u64(writer, offset)?;
    }
    for label in registry {
        writer.write_bytes(label.as_bytes())?;
    }
    Ok(())
}

fn decode_identity(record: &crate::build_runs::RunRecord) -> GraphResult<(u32, u64, &[u8])> {
    if record.key.len() != 4 || record.value.len() < IDENTITY_VALUE_HEADER_BYTES {
        return Err(GraphError::Internal("invalid identity record shape".into()));
    }
    let relationship_id = u32::from_be_bytes(read_array(&record.key, 0)?);
    let mapping_id = u64::from_le_bytes(read_array(&record.value, 0)?);
    let key_len = usize::try_from(u32::from_le_bytes(read_array(&record.value, 8)?))
        .map_err(|_| GraphError::Internal("identity key length exceeds usize".into()))?;
    if relationship_id == 0
        || mapping_id == 0
        || record.value.len() != IDENTITY_VALUE_HEADER_BYTES + key_len
    {
        return Err(GraphError::Internal("invalid relationship identity".into()));
    }
    let key = &record.value[IDENTITY_VALUE_HEADER_BYTES..];
    std::str::from_utf8(key)
        .map_err(|_| GraphError::Internal("relationship identity key is not UTF-8".into()))?;
    Ok((relationship_id, mapping_id, key))
}

fn replay_identities(
    build: &SemanticDirectBuild<'_, '_>,
    mut visit: impl FnMut(u64, &[u8]) -> GraphResult<()>,
) -> GraphResult<()> {
    let mut expected_id = 1_u32;
    if let Some(run) = build.relationship_identities {
        replay_checked(build, run, |record| {
            let (relationship_id, mapping_id, key) = decode_identity(&record)?;
            if relationship_id != expected_id {
                return Err(GraphError::Internal(
                    "relationship identities are not densely ordered".into(),
                ));
            }
            visit(mapping_id, key)?;
            expected_id = expected_id.checked_add(1).ok_or_else(|| {
                GraphError::Internal("relationship identity count overflowed".into())
            })?;
            Ok(())
        })?;
    }
    Ok(())
}

fn emit_identity_descriptors(
    writer: &mut DirectArtifactWriter,
    build: &SemanticDirectBuild<'_, '_>,
) -> GraphResult<()> {
    writer.write_bytes(&[0_u8; 16])?;
    let mut key_end = 0_u64;
    replay_identities(build, |mapping_id, key| {
        writer.write_bytes(&mapping_id.to_le_bytes())?;
        key_end = key_end
            .checked_add(key.len() as u64)
            .ok_or_else(|| GraphError::Internal("identity key bytes exceed u64".into()))?;
        write_u64(writer, key_end)
    })
}

fn emit_identity_keys(
    writer: &mut DirectArtifactWriter,
    build: &SemanticDirectBuild<'_, '_>,
) -> GraphResult<()> {
    replay_identities(build, |_, key| writer.write_bytes(key))
}

fn emit_tenanted_oids(writer: &mut DirectArtifactWriter, oids: &[u32]) -> GraphResult<()> {
    let mut previous = None;
    for &oid in oids {
        if previous.is_some_and(|value| value >= oid) {
            return Err(GraphError::Internal(
                "tenanted table OIDs are not strictly sorted".into(),
            ));
        }
        write_u32(writer, oid)?;
        previous = Some(oid);
    }
    Ok(())
}

fn inspect_tenant_dictionary(
    build: &SemanticDirectBuild<'_, '_>,
    mut visit_unique: impl FnMut(&[u8]) -> GraphResult<()>,
) -> GraphResult<(u32, u64)> {
    let mut previous: Option<Vec<u8>> = None;
    let mut count = 0_u32;
    let mut payload_len = 0_u64;
    if let Some(run) = build.tenant_dictionary {
        replay_checked(build, run, |record| {
            std::str::from_utf8(&record.value)
                .map_err(|_| GraphError::Internal("tenant dictionary is not UTF-8".into()))?;
            if previous.as_deref() == Some(record.value.as_slice()) {
                return Ok(());
            }
            if previous
                .as_deref()
                .is_some_and(|value| value >= record.value.as_slice())
            {
                return Err(GraphError::Internal(
                    "tenant dictionary is not lexically sorted".into(),
                ));
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("tenant count overflowed".into()))?;
            payload_len = payload_len
                .checked_add(record.value.len() as u64)
                .ok_or_else(|| GraphError::Internal("tenant bytes overflowed".into()))?;
            visit_unique(&record.value)?;
            previous = Some(record.value);
            Ok(())
        })?;
    }
    Ok((count, payload_len))
}

fn emit_tenant_dictionary(
    writer: &mut DirectArtifactWriter,
    build: &SemanticDirectBuild<'_, '_>,
) -> GraphResult<()> {
    let (count, _) = inspect_tenant_dictionary(build, |_| Ok(()))?;
    write_u32(writer, count)?;
    let mut offset = 0_u64;
    write_u64(writer, offset)?;
    inspect_tenant_dictionary(build, |tenant| {
        offset = offset
            .checked_add(tenant.len() as u64)
            .ok_or_else(|| GraphError::Internal("tenant offset overflowed".into()))?;
        write_u64(writer, offset)
    })?;
    inspect_tenant_dictionary(build, |tenant| writer.write_bytes(tenant))?;
    Ok(())
}

fn emit_tenant_tokens(
    writer: &mut DirectArtifactWriter,
    build: &SemanticDirectBuild<'_, '_>,
) -> GraphResult<()> {
    let mut next_node = 0_u32;
    if let Some(run) = build.tenant_tokens {
        replay_checked(build, run, |record| {
            if record.key.len() != 4 || record.value.len() != 4 {
                return Err(GraphError::Internal("invalid tenant token record".into()));
            }
            let node_idx = u32::from_be_bytes(read_array(&record.key, 0)?);
            let token = u32::from_le_bytes(read_array(&record.value, 0)?);
            if node_idx < next_node || node_idx >= build.node_count || token == 0 {
                return Err(GraphError::Internal(
                    "tenant token records are not sparse-dense ordered".into(),
                ));
            }
            while next_node < node_idx {
                write_u32(writer, 0)?;
                next_node += 1;
            }
            write_u32(writer, token)?;
            next_node += 1;
            Ok(())
        })?;
    }
    while next_node < build.node_count {
        write_u32(writer, 0)?;
        next_node += 1;
    }
    Ok(())
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> GraphResult<[u8; N]> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| GraphError::Internal("direct record offset overflowed".into()))?;
    let source = bytes
        .get(offset..end)
        .ok_or_else(|| GraphError::Internal("direct record is truncated".into()))?;
    let mut result = [0_u8; N];
    result.copy_from_slice(source);
    Ok(result)
}

fn replay_checked(
    build: &SemanticDirectBuild<'_, '_>,
    run: &StagedRun<'_>,
    visit: impl FnMut(crate::build_runs::RunRecord) -> GraphResult<()>,
) -> GraphResult<()> {
    replay_checked_raw(build.governor, run, build.max_record_bytes, visit)
}

fn replay_checked_raw(
    governor: &ResourceGovernor,
    run: &StagedRun<'_>,
    max_record_bytes: usize,
    mut visit: impl FnMut(crate::build_runs::RunRecord) -> GraphResult<()>,
) -> GraphResult<()> {
    governor
        .check_elapsed(ResourcePhase::Persistence)
        .map_err(crate::safety::resource_limit_error)?;
    crate::resource::check_postgres_interrupts();
    let mut visited = 0_u32;
    run.replay(max_record_bytes, |record| {
        if visited.is_multiple_of(1024) {
            governor
                .check_elapsed(ResourcePhase::Persistence)
                .map_err(crate::safety::resource_limit_error)?;
            crate::resource::check_postgres_interrupts();
        }
        visited = visited.wrapping_add(1);
        visit(record)
    })
}

fn allocation_error(_label: &str, _error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        write_prepared_artifact, write_semantic_artifact, DirectBuildArtifact, DirectSection,
        SemanticDirectBuild,
    };
    use crate::build_runs::{RunCollector, RunKind, RunRecord, RunWorkspace};
    use crate::builder::RegisteredFilterColumn;
    use crate::config::ProjectionMode;
    use crate::resource::{
        ByteCount, DiskBudget, ElapsedBudget, MemoryBudget, ResourceGovernor, ResourceLimits,
        RowCount, WorkUnits,
    };
    use std::time::Duration;

    #[test]
    fn writes_minimal_valid_v6_without_owned_engine() {
        let temp = std::env::temp_dir().join(format!(
            "pggraph-direct-build-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("tempdir");
        let path = temp.join("direct.pggraph");
        let governor = ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(64 * 1024)),
            DiskBudget::new(ByteCount::from_bytes(1024 * 1024)),
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::MAX),
        ));
        let workspace =
            RunWorkspace::create(&temp, [1; 16], [2; 16], 3, 8).expect("run workspace creates");
        let zero_u32 = 0u32.to_le_bytes();
        let zero_u64 = 0u64.to_le_bytes();
        let filter_catalog = [
            zero_u32.as_slice(),
            64u32.to_le_bytes().as_slice(),
            zero_u64.as_slice(),
        ]
        .concat();
        let one_u32 = 1u32.to_le_bytes();
        let registry = [one_u32.as_slice(), zero_u64.as_slice(), zero_u64.as_slice()].concat();
        let identity_zero = [0u8; 16];
        let tenant_dictionary = [zero_u32.as_slice(), zero_u64.as_slice()].concat();
        let mut resolution = RunCollector::new(
            &workspace,
            &governor,
            RunKind::Resolution,
            ByteCount::from_bytes(1024),
            64,
        )
        .expect("collector creates");
        resolution
            .push(RunRecord::new(Vec::new(), zero_u32.to_vec()))
            .expect("resolution record stages");
        let mut resolution_runs = resolution.finish().expect("resolution run finishes");
        let resolution_run = resolution_runs.pop().expect("resolution run exists");
        let sections = [
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Bytes(&zero_u64),
            DirectSection::Empty,
            DirectSection::Run {
                run: &resolution_run,
                max_record_bytes: 64,
            },
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Bytes(&zero_u32),
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Bytes(&zero_u32),
            DirectSection::Bytes(&filter_catalog),
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Bytes(&registry),
            DirectSection::Bytes(&identity_zero),
            DirectSection::Empty,
            DirectSection::Empty,
            DirectSection::Bytes(&tenant_dictionary),
            DirectSection::Empty,
        ];
        let artifact = DirectBuildArtifact {
            node_count: 0,
            forward_edge_count: 0,
            inbound_edge_count: 0,
            has_forward_weights: false,
            has_inbound_weights: false,
            has_unidirectional_edges: false,
            applied_sync_id: 0,
            projection_mode: ProjectionMode::CsrReadonly,
            sections,
            governor: &governor,
        };

        write_prepared_artifact(&path, &artifact).expect("direct artifact writes");
        let original = std::fs::read(&path).expect("artifact reads");
        assert!(write_prepared_artifact(&path, &artifact).is_err());
        assert_eq!(
            std::fs::read(&path).expect("artifact still reads"),
            original
        );
        let loaded = crate::persistence::load_graph_file(&path).expect("artifact loads");
        assert_eq!(loaded.node_store.node_count(), 0);
        assert_eq!(loaded.edge_store.edge_count(), 0);
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn semantic_assembly_streams_nodes_csr_registry_and_identities() {
        let temp = std::env::temp_dir().join(format!(
            "pggraph-semantic-direct-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("tempdir");
        let path = temp.join("semantic.pggraph");
        let governor = ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(256 * 1024)),
            DiskBudget::new(ByteCount::from_bytes(2 * 1024 * 1024)),
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::MAX),
        ));
        let workspace =
            RunWorkspace::create(&temp, [3; 16], [4; 16], 9, 16).expect("run workspace creates");
        let stage = |kind, records: Vec<RunRecord>| {
            let mut collector = RunCollector::new(
                &workspace,
                &governor,
                kind,
                ByteCount::from_bytes(4096),
                256,
            )
            .expect("collector creates");
            for record in records {
                collector.push(record).expect("record stages");
            }
            collector
                .finish()
                .expect("collector finishes")
                .pop()
                .expect("run exists")
        };
        let node = |index: u32, key: &str| {
            let mut value = Vec::new();
            value.extend_from_slice(&index.to_le_bytes());
            value.extend_from_slice(&42_u32.to_le_bytes());
            value.extend_from_slice(&(key.len() as u32).to_le_bytes());
            value.extend_from_slice(key.as_bytes());
            RunRecord::new(index.to_be_bytes().to_vec(), value)
        };
        let nodes = stage(RunKind::Nodes, vec![node(0, "a"), node(1, "b")]);
        let mut resolution_records = [(0_u32, "a"), (1_u32, "b")]
            .into_iter()
            .map(|(node_idx, key)| {
                let hash = crate::resolution_index::ResolutionIndexBuilder::hash_pk(key);
                let mut order = Vec::new();
                order.extend_from_slice(&42_u32.to_be_bytes());
                order.extend_from_slice(&hash.to_be_bytes());
                order.extend_from_slice(&node_idx.to_be_bytes());
                let mut value = Vec::new();
                value.extend_from_slice(&42_u32.to_le_bytes());
                value.extend_from_slice(&hash.to_le_bytes());
                value.extend_from_slice(&node_idx.to_le_bytes());
                RunRecord::new(order, value)
            })
            .collect::<Vec<_>>();
        resolution_records.sort_unstable();
        let resolution = stage(RunKind::Resolution, resolution_records);
        let edge = |source: u32, target: u32| {
            let mut key = Vec::new();
            key.extend_from_slice(&source.to_be_bytes());
            key.extend_from_slice(&target.to_be_bytes());
            key.extend_from_slice(&[1, 0]);
            key.extend_from_slice(&1_u32.to_be_bytes());
            let mut value = Vec::new();
            value.extend_from_slice(&target.to_le_bytes());
            value.extend_from_slice(&[1, 0]);
            value.extend_from_slice(&5_u32.to_le_bytes());
            value.extend_from_slice(&1_u32.to_le_bytes());
            RunRecord::new(key, value)
        };
        let forward = stage(RunKind::RelationshipsOutbound, vec![edge(0, 1)]);
        let inbound = stage(RunKind::RelationshipsInbound, vec![edge(1, 0)]);
        let mut identity_value = Vec::new();
        identity_value.extend_from_slice(&7_u64.to_le_bytes());
        identity_value.extend_from_slice(&4_u32.to_le_bytes());
        identity_value.extend_from_slice(b"edge");
        let identities = stage(
            RunKind::RelationshipIdentities,
            vec![RunRecord::new(1_u32.to_be_bytes().to_vec(), identity_value)],
        );
        let filter_record = |ordinal: u32, node_idx: u32, tag: u8, payload: &[u8]| {
            let mut key = Vec::new();
            key.extend_from_slice(&ordinal.to_be_bytes());
            key.extend_from_slice(&node_idx.to_be_bytes());
            let mut value = vec![tag];
            value.extend_from_slice(payload);
            RunRecord::new(key, value)
        };
        let uuid = 0x550e8400e29b41d4a716446655440000_u128;
        let filters = stage(
            RunKind::Filters,
            vec![
                filter_record(0, 0, 1, &(-7_i64).to_le_bytes()),
                filter_record(1, 0, 2, &[1]),
                filter_record(2, 0, 3, &1_u32.to_le_bytes()),
                filter_record(2, 1, 3, &0_u32.to_le_bytes()),
                filter_record(3, 0, 4, &12_i64.to_le_bytes()),
                filter_record(4, 0, 5, &99_i64.to_le_bytes()),
                filter_record(5, 0, 6, &uuid.to_le_bytes()),
            ],
        );
        let dictionary_record = |token: u32, value: &str| {
            let mut key = Vec::new();
            key.extend_from_slice(&2_u32.to_be_bytes());
            key.extend_from_slice(&token.to_be_bytes());
            RunRecord::new(key, value.as_bytes().to_vec())
        };
        let filter_dictionary = stage(
            RunKind::FilterDictionary,
            vec![dictionary_record(0, "blue"), dictionary_record(1, "red")],
        );
        let mut tenant_key = b"acme\0".to_vec();
        tenant_key.extend_from_slice(&0_u32.to_be_bytes());
        let tenant_dictionary = stage(
            RunKind::TenantDictionary,
            vec![RunRecord::new(tenant_key, b"acme".to_vec())],
        );
        let tenant_tokens = stage(
            RunKind::TenantValues,
            vec![RunRecord::new(
                0_u32.to_be_bytes().to_vec(),
                1_u32.to_le_bytes().to_vec(),
            )],
        );
        let filter_columns = [
            ("score", "numeric"),
            ("active", "boolean"),
            ("color", "text"),
            ("day", "date"),
            ("seen", "timestamptz"),
            ("owner", "uuid"),
        ]
        .into_iter()
        .map(|(column_name, column_type)| RegisteredFilterColumn {
            table_oid: 42,
            table_name: "nodes".to_owned(),
            column_name: column_name.to_owned(),
            column_type: column_type.to_owned(),
        })
        .collect::<Vec<_>>();
        let registry = vec![String::new(), "KNOWS".to_owned()];
        let build = SemanticDirectBuild {
            node_count: 2,
            forward_edge_count: 1,
            inbound_edge_count: 1,
            has_forward_weights: true,
            has_inbound_weights: true,
            has_unidirectional_edges: true,
            applied_sync_id: 13,
            projection_mode: ProjectionMode::CsrReadonly,
            nodes: Some(&nodes),
            forward_edges: Some(&forward),
            inbound_edges: Some(&inbound),
            resolution: Some(&resolution),
            filters: Some(&filters),
            filter_dictionary: Some(&filter_dictionary),
            relationship_identities: Some(&identities),
            tenant_tokens: Some(&tenant_tokens),
            tenant_dictionary: Some(&tenant_dictionary),
            filter_columns: &filter_columns,
            edge_type_registry: &registry,
            tenanted_table_oids: &[42],
            max_record_bytes: 256,
            governor: &governor,
        };

        write_semantic_artifact(&path, &build).expect("semantic artifact writes");
        let loaded = crate::persistence::load_graph_file(&path).expect("artifact loads");
        assert_eq!(loaded.node_store.primary_key(0), Some("a"));
        assert_eq!(loaded.node_store.primary_key(1), Some("b"));
        assert_eq!(loaded.edge_store.neighbors(0), (&[1][..], &[1][..]));
        assert_eq!(loaded.reverse_edge_store.neighbors(1), (&[0][..], &[1][..]));
        assert_eq!(loaded.edge_type_registry, registry);
        assert_eq!(loaded.edge_store.weights_slice(), &[5]);
        assert_eq!(
            loaded
                .relationship_identities
                .get(1)
                .map(|identity| (identity.mapping_id, identity.source_key.to_owned())),
            Some((7, "edge".to_owned()))
        );
        assert!(loaded.has_unidirectional_edges);
        assert_eq!(loaded.applied_sync_id, 13);
        use crate::types::{FilterCondition, FilterOp};
        assert!(loaded
            .filter_index
            .check_filter(0, &FilterOp::new(0, FilterCondition::EqI64(-7))));
        assert!(loaded
            .filter_index
            .check_filter(0, &FilterOp::new(1, FilterCondition::EqBool(true))));
        let blue = loaded
            .filter_index
            .lookup_text_value(2, "blue")
            .expect("text dictionary maps blue");
        assert!(loaded
            .filter_index
            .check_filter(1, &FilterOp::new(2, FilterCondition::EqToken(blue))));
        assert!(loaded
            .filter_index
            .check_filter(0, &FilterOp::new(3, FilterCondition::EqI64(12))));
        assert!(loaded
            .filter_index
            .check_filter(0, &FilterOp::new(4, FilterCondition::EqI64(99))));
        assert!(loaded
            .filter_index
            .check_filter(0, &FilterOp::new(5, FilterCondition::EqUuid(uuid))));
        assert!(loaded
            .filter_index
            .check_filter(1, &FilterOp::new(0, FilterCondition::IsNull)));
        assert!(loaded.tenant_contains("acme", 0));
        assert!(!loaded.tenant_contains("acme", 1));
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn semantic_assembly_rejects_non_dense_node_codec_before_publication() {
        let temp = std::env::temp_dir().join(format!(
            "pggraph-semantic-invalid-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("tempdir");
        let governor = ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(64 * 1024)),
            DiskBudget::new(ByteCount::from_bytes(1024 * 1024)),
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::MAX),
        ));
        let workspace = RunWorkspace::create(&temp, [5; 16], [6; 16], 1, 8).expect("workspace");
        let mut nodes = RunCollector::new(
            &workspace,
            &governor,
            RunKind::Nodes,
            ByteCount::from_bytes(1024),
            64,
        )
        .expect("collector");
        let mut invalid_value = Vec::new();
        invalid_value.extend_from_slice(&1_u32.to_le_bytes());
        invalid_value.extend_from_slice(&42_u32.to_le_bytes());
        invalid_value.extend_from_slice(&1_u32.to_le_bytes());
        invalid_value.push(b'x');
        nodes
            .push(RunRecord::new(0_u32.to_be_bytes().to_vec(), invalid_value))
            .expect("record");
        let node_run = nodes.finish().expect("finish").pop().expect("run");
        let registry = vec![String::new()];
        let build = SemanticDirectBuild {
            node_count: 1,
            forward_edge_count: 0,
            inbound_edge_count: 0,
            has_forward_weights: false,
            has_inbound_weights: false,
            has_unidirectional_edges: false,
            applied_sync_id: 0,
            projection_mode: ProjectionMode::CsrReadonly,
            nodes: Some(&node_run),
            forward_edges: None,
            inbound_edges: None,
            resolution: None,
            filters: None,
            filter_dictionary: None,
            relationship_identities: None,
            tenant_tokens: None,
            tenant_dictionary: None,
            filter_columns: &[],
            edge_type_registry: &registry,
            tenanted_table_oids: &[],
            max_record_bytes: 64,
            governor: &governor,
        };
        let path = temp.join("invalid.pggraph");
        assert!(write_semantic_artifact(&path, &build).is_err());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(temp);
    }
}
