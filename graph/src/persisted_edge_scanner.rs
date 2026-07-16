//! Bounded relationship staging for direct persisted builds.

use pgrx::prelude::*;

use crate::build_runs::{merge_runs, RunCollector, RunKind, RunRecord, RunWorkspace, StagedRun};
use crate::builder::{PrimaryKeySpec, RegisteredEdge, RegisteredTable};
use crate::catalog::sql_table_name_from_oid;
use crate::quote::quote_ident;
use crate::resource::{ByteCount, ResourceGovernor, ResourceLease, ResourcePhase};
use crate::safety::{GraphError, GraphResult};

const RAW_FIXED_BYTES: usize = 18;
const EDGE_VALUE_BYTES: usize = 14;

/// Final relationship runs and bounded registry metadata.
pub(crate) struct PersistedEdgeRuns<'governor> {
    pub(crate) forward: Option<StagedRun<'governor>>,
    pub(crate) inbound: Option<StagedRun<'governor>>,
    pub(crate) identities: Option<StagedRun<'governor>>,
    pub(crate) edge_type_registry: Vec<String>,
    pub(crate) forward_edge_count: u32,
    pub(crate) inbound_edge_count: u32,
    _registry_memory: ResourceLease<'governor>,
}

/// Resolve and stage all registered relationship rows without an owned graph.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_persisted_edges<'governor>(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    workspace: &RunWorkspace,
    governor: &'governor ResourceGovernor,
    batch_size: usize,
    run_target: ByteCount,
    max_record_bytes: usize,
    merge_fanout: usize,
) -> GraphResult<PersistedEdgeRuns<'governor>> {
    if batch_size == 0 || merge_fanout < 2 {
        return Err(GraphError::Internal(
            "direct edge scan limits are invalid".into(),
        ));
    }
    let mut raw = RunCollector::new(
        workspace,
        governor,
        RunKind::RelationshipsByIdentity,
        run_target,
        max_record_bytes,
    )?;
    let mut registry = Vec::new();
    registry.try_reserve(16).map_err(allocation_error)?;
    registry.push(String::new());
    let mut registry_memory = governor
        .reserve_memory(ResourcePhase::EdgeResolve, ByteCount::ZERO)
        .map_err(resource_error)?;

    for edge in edges {
        governor
            .check_elapsed(ResourcePhase::EdgeResolve)
            .map_err(resource_error)?;
        crate::resource::check_postgres_interrupts();
        let static_type = if edge.label_column.is_none() {
            Some(intern_edge_type(
                &mut registry,
                &mut registry_memory,
                &edge.label,
                max_record_bytes,
            )?)
        } else {
            None
        };
        let source = sql_table_name_from_oid(edge.from_table_oid)?;
        let fk_source = tables
            .iter()
            .find(|table| table.table_oid == edge.from_table_oid)
            .map(|table| aliased_pk_expr(&table.id_columns, "edge_row"))
            .transpose()?;
        let from_expr = fk_source
            .clone()
            .unwrap_or_else(|| aliased_column("edge_row", &edge.from_column));
        let to_expr = if fk_source.is_some() {
            aliased_column("edge_row", &edge.from_column)
        } else {
            aliased_column("edge_row", &edge.to_column)
        };
        let source_key = aliased_pk_expr(&edge.source_key_columns, "edge_row")?;
        let weight = edge
            .weight_column
            .as_ref()
            .map(|column| format!(", ({})::bigint", aliased_column("edge_row", column)))
            .unwrap_or_default();
        let label = edge
            .label_column
            .as_ref()
            .map(|column| format!(", ({})::text", aliased_column("edge_row", column)))
            .unwrap_or_default();
        let query = format!(
            "SELECT source_node.node_idx, target_node.node_idx, ({source_key})::text{weight}{label}
             FROM {} AS edge_row
             JOIN LATERAL (
               SELECT node_idx FROM pg_temp.graph_build_nodes
               WHERE primary_key = ({from_expr})::text
               ORDER BY (table_oid = {}) DESC, table_oid LIMIT 1
             ) source_node ON true
             JOIN LATERAL (
               SELECT node_idx FROM pg_temp.graph_build_nodes
               WHERE primary_key = ({to_expr})::text
               ORDER BY (table_oid = {}) DESC, table_oid LIMIT 1
             ) target_node ON true
             ORDER BY 3, 1, 2{}",
            source.as_sql(),
            edge.from_table_oid,
            edge.to_table_oid,
            stable_optional_order(edge.weight_column.is_some(), edge.label_column.is_some()),
        );
        let label_column = dynamic_label_column(edge.weight_column.is_some());
        Spi::connect(|client| {
            let mut cursor = client.open_cursor(&query, &[]);
            loop {
                let rows = cursor
                    .fetch(i64::try_from(batch_size).unwrap_or(i64::MAX))
                    .map_err(|error| GraphError::Internal(format!("edge fetch failed: {error}")))?;
                if rows.is_empty() {
                    break;
                }
                for row in rows {
                    let source_idx = required_u32(&row, 1, "source")?;
                    let target_idx = required_u32(&row, 2, "target")?;
                    let source_key = row
                        .get::<String>(3)
                        .map_err(|error| {
                            GraphError::Internal(format!("edge identity read failed: {error}"))
                        })?
                        .ok_or_else(|| GraphError::Internal("edge identity was NULL".into()))?;
                    let weight = if edge.weight_column.is_some() {
                        row.get::<i64>(4)
                            .ok()
                            .flatten()
                            .map(|value| value.clamp(1, u32::MAX as i64) as u32)
                    } else {
                        None
                    };
                    let type_id = if edge.label_column.is_some() {
                        let dynamic = row
                            .get::<String>(label_column)
                            .map_err(|error| {
                                GraphError::Internal(format!("edge label read failed: {error}"))
                            })?
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or_else(|| edge.label.clone());
                        intern_edge_type(
                            &mut registry,
                            &mut registry_memory,
                            &dynamic,
                            max_record_bytes,
                        )?
                    } else {
                        static_type.ok_or_else(|| {
                            GraphError::Internal("static edge type missing".into())
                        })?
                    };
                    raw.push(encode_raw_edge(
                        edge.mapping_id,
                        &source_key,
                        source_idx,
                        target_idx,
                        type_id,
                        weight,
                        edge.bidirectional,
                    )?)?;
                }
            }
            Ok::<(), GraphError>(())
        })?;
    }

    let raw = finish_single(raw, workspace, governor, merge_fanout, max_record_bytes)?;
    normalize_relationships(
        raw.as_ref(),
        workspace,
        governor,
        registry,
        registry_memory,
        run_target,
        max_record_bytes,
        merge_fanout,
    )
}

#[allow(clippy::too_many_arguments)]
fn normalize_relationships<'governor>(
    raw: Option<&StagedRun<'_>>,
    workspace: &RunWorkspace,
    governor: &'governor ResourceGovernor,
    edge_type_registry: Vec<String>,
    registry_memory: ResourceLease<'governor>,
    run_target: ByteCount,
    max_record_bytes: usize,
    merge_fanout: usize,
) -> GraphResult<PersistedEdgeRuns<'governor>> {
    let mut forward = RunCollector::new(
        workspace,
        governor,
        RunKind::RelationshipsOutbound,
        run_target,
        max_record_bytes,
    )?;
    let mut inbound = RunCollector::new(
        workspace,
        governor,
        RunKind::RelationshipsInbound,
        run_target,
        max_record_bytes,
    )?;
    let mut identities = RunCollector::new(
        workspace,
        governor,
        RunKind::RelationshipIdentities,
        run_target,
        max_record_bytes,
    )?;
    let _identity_scratch = governor
        .reserve_memory(
            ResourcePhase::EdgeResolve,
            ByteCount::from_bytes(max_record_bytes as u64),
        )
        .map_err(resource_error)?;
    let mut previous_identity = Vec::new();
    let mut relationship_id = 0u32;
    let mut forward_count = 0u32;
    if let Some(raw) = raw {
        raw.replay(max_record_bytes, |record| {
            let decoded = decode_raw_edge(&record)?;
            let identity_key = &record.key[..decoded.identity_key_len];
            if previous_identity != identity_key {
                relationship_id = relationship_id.checked_add(1).ok_or_else(|| {
                    GraphError::Internal("relationship identity count exceeds u32".into())
                })?;
                previous_identity.clear();
                previous_identity
                    .try_reserve(identity_key.len())
                    .map_err(allocation_error)?;
                previous_identity.extend_from_slice(identity_key);
                identities.push(encode_identity(
                    relationship_id,
                    decoded.mapping_id,
                    decoded.source_key,
                )?)?;
            }
            push_oriented(
                &mut forward,
                decoded.source,
                decoded.target,
                decoded.type_id,
                false,
                decoded.weight,
                relationship_id,
            )?;
            push_oriented(
                &mut inbound,
                decoded.target,
                decoded.source,
                decoded.type_id,
                false,
                decoded.weight,
                relationship_id,
            )?;
            forward_count = forward_count
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("edge count exceeds u32".into()))?;
            if decoded.bidirectional {
                push_oriented(
                    &mut forward,
                    decoded.target,
                    decoded.source,
                    decoded.type_id,
                    true,
                    decoded.weight,
                    relationship_id,
                )?;
                push_oriented(
                    &mut inbound,
                    decoded.source,
                    decoded.target,
                    decoded.type_id,
                    true,
                    decoded.weight,
                    relationship_id,
                )?;
                forward_count = forward_count
                    .checked_add(1)
                    .ok_or_else(|| GraphError::Internal("edge count exceeds u32".into()))?;
            }
            Ok(())
        })?;
    }
    Ok(PersistedEdgeRuns {
        forward: finish_single(forward, workspace, governor, merge_fanout, max_record_bytes)?,
        inbound: finish_single(inbound, workspace, governor, merge_fanout, max_record_bytes)?,
        identities: finish_single(
            identities,
            workspace,
            governor,
            merge_fanout,
            max_record_bytes,
        )?,
        edge_type_registry,
        forward_edge_count: forward_count,
        inbound_edge_count: forward_count,
        _registry_memory: registry_memory,
    })
}

struct DecodedRaw<'a> {
    mapping_id: u64,
    source_key: &'a str,
    source: u32,
    target: u32,
    type_id: u8,
    weight: u32,
    bidirectional: bool,
    identity_key_len: usize,
}

fn encode_raw_edge(
    mapping_id: u64,
    source_key: &str,
    source: u32,
    target: u32,
    type_id: u8,
    weight: Option<u32>,
    bidirectional: bool,
) -> GraphResult<RunRecord> {
    let key_len = u32::try_from(source_key.len())
        .map_err(|_| GraphError::Internal("relationship key exceeds u32".into()))?;
    let mut key = Vec::new();
    key.try_reserve_exact(12 + source_key.len() + 9)
        .map_err(allocation_error)?;
    key.extend_from_slice(&mapping_id.to_be_bytes());
    key.extend_from_slice(&key_len.to_be_bytes());
    key.extend_from_slice(source_key.as_bytes());
    let identity_key_len = key.len();
    key.extend_from_slice(&source.to_be_bytes());
    key.extend_from_slice(&target.to_be_bytes());
    key.push(type_id);
    let mut value = Vec::new();
    value
        .try_reserve_exact(RAW_FIXED_BYTES)
        .map_err(allocation_error)?;
    value.extend_from_slice(&source.to_le_bytes());
    value.extend_from_slice(&target.to_le_bytes());
    value.push(type_id);
    value.extend_from_slice(&weight.unwrap_or(0).to_le_bytes());
    value.push(u8::from(bidirectional));
    value.extend_from_slice(&(identity_key_len as u32).to_le_bytes());
    Ok(RunRecord::new(key, value))
}

fn decode_raw_edge(record: &RunRecord) -> GraphResult<DecodedRaw<'_>> {
    if record.value.len() != RAW_FIXED_BYTES || record.key.len() < 12 {
        return Err(corrupt("raw relationship record has invalid width"));
    }
    let mapping_id = u64::from_be_bytes(
        record.key[0..8]
            .try_into()
            .map_err(|_| corrupt("mapping ID width"))?,
    );
    let key_len = u32::from_be_bytes(
        record.key[8..12]
            .try_into()
            .map_err(|_| corrupt("key length width"))?,
    ) as usize;
    let key_end = 12usize
        .checked_add(key_len)
        .ok_or_else(|| corrupt("identity key overflow"))?;
    if record.key.len() != key_end + 9 {
        return Err(corrupt("raw relationship key length mismatch"));
    }
    let source_key = std::str::from_utf8(&record.key[12..key_end])
        .map_err(|_| corrupt("relationship key is not UTF-8"))?;
    Ok(DecodedRaw {
        mapping_id,
        source_key,
        source: read_le_u32(&record.value, 0)?,
        target: read_le_u32(&record.value, 4)?,
        type_id: record.value[8],
        weight: read_le_u32(&record.value, 9)?,
        bidirectional: record.value[13] != 0,
        identity_key_len: key_end,
    })
}

fn encode_identity(id: u32, mapping_id: u64, source_key: &str) -> GraphResult<RunRecord> {
    let len = u32::try_from(source_key.len())
        .map_err(|_| GraphError::Internal("relationship key exceeds u32".into()))?;
    let mut value = Vec::new();
    value
        .try_reserve_exact(12usize.checked_add(source_key.len()).ok_or_else(|| {
            GraphError::Internal("relationship identity record length overflowed".into())
        })?)
        .map_err(allocation_error)?;
    value.extend_from_slice(&mapping_id.to_le_bytes());
    value.extend_from_slice(&len.to_le_bytes());
    value.extend_from_slice(source_key.as_bytes());
    Ok(RunRecord::new(id.to_be_bytes().to_vec(), value))
}

fn push_oriented(
    collector: &mut RunCollector<'_, '_>,
    source: u32,
    target: u32,
    type_id: u8,
    schema_reversed: bool,
    weight: u32,
    relationship_id: u32,
) -> GraphResult<()> {
    let mut key = Vec::new();
    key.try_reserve_exact(14).map_err(allocation_error)?;
    key.extend_from_slice(&source.to_be_bytes());
    key.extend_from_slice(&target.to_be_bytes());
    key.push(type_id);
    key.push(u8::from(schema_reversed));
    key.extend_from_slice(&relationship_id.to_be_bytes());
    let mut value = Vec::new();
    value
        .try_reserve_exact(EDGE_VALUE_BYTES)
        .map_err(allocation_error)?;
    value.extend_from_slice(&target.to_le_bytes());
    value.push(type_id);
    value.push(u8::from(schema_reversed));
    value.extend_from_slice(&weight.to_le_bytes());
    value.extend_from_slice(&relationship_id.to_le_bytes());
    collector.push(RunRecord::new(key, value))
}

fn intern_edge_type(
    registry: &mut Vec<String>,
    memory: &mut ResourceLease<'_>,
    label: &str,
    max_record_bytes: usize,
) -> GraphResult<u8> {
    if let Some(index) = registry.iter().position(|value| value == label) {
        return u8::try_from(index)
            .map_err(|_| GraphError::Internal("edge type registry exceeds 255 entries".into()));
    }
    if registry.len() >= u8::MAX as usize || label.len() > max_record_bytes {
        return Err(GraphError::Internal(
            "edge type registry exceeds bounded artifact limits".into(),
        ));
    }
    let id = registry.len() as u8;
    let bytes = std::mem::size_of::<String>()
        .checked_add(label.len())
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("edge type registry memory overflowed".into()))?;
    memory
        .try_grow_in(ResourcePhase::EdgeResolve, bytes)
        .map_err(resource_error)?;
    registry.try_reserve(1).map_err(allocation_error)?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(label.len())
        .map_err(allocation_error)?;
    owned.push_str(label);
    registry.push(owned);
    Ok(id)
}

fn aliased_column(alias: &str, column: &str) -> String {
    format!("{alias}.{}", quote_ident(column))
}

fn stable_optional_order(has_weight: bool, has_label: bool) -> &'static str {
    match (has_weight, has_label) {
        (false, false) => "",
        (true, false) => ", 4",
        (false, true) => ", 4",
        (true, true) => ", 4, 5",
    }
}

fn aliased_pk_expr(spec: &PrimaryKeySpec, alias: &str) -> GraphResult<String> {
    if spec.columns().len() > 1 {
        let capacity = spec
            .columns()
            .iter()
            .try_fold(32usize, |total, column| {
                total.checked_add(alias.len() + column.len() + 12)
            })
            .ok_or_else(|| {
                GraphError::Internal("composite primary-key SQL length overflowed".into())
            })?;
        let mut expression = String::new();
        expression
            .try_reserve_exact(capacity)
            .map_err(allocation_error)?;
        expression.push_str("jsonb_build_array(");
        for (index, column) in spec.columns().iter().enumerate() {
            if index != 0 {
                expression.push_str(", ");
            }
            expression.push_str(&aliased_column(alias, column));
            expression.push_str("::text");
        }
        expression.push_str(")::text");
        Ok(expression)
    } else {
        Ok(spec
            .columns()
            .first()
            .map(|column| aliased_column(alias, column))
            .unwrap_or_else(|| "NULL".into()))
    }
}
fn required_u32(
    row: &pgrx::spi::SpiHeapTupleData<'_>,
    column: usize,
    label: &str,
) -> GraphResult<u32> {
    let value = row
        .get::<i64>(column)
        .map_err(|error| GraphError::Internal(format!("edge {label} read failed: {error}")))?
        .ok_or_else(|| GraphError::Internal(format!("edge {label} was NULL")))?;
    u32::try_from(value).map_err(|_| GraphError::Internal(format!("edge {label} is out of range")))
}
fn read_le_u32(bytes: &[u8], offset: usize) -> GraphResult<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(|| corrupt("integer truncated"))?
            .try_into()
            .map_err(|_| corrupt("integer width"))?,
    ))
}
fn corrupt(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: reason.into(),
    }
}
fn resource_error(error: crate::resource::ResourceLimitError) -> GraphError {
    crate::safety::resource_limit_error(error)
}
fn allocation_error(_error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
    }
}
const fn dynamic_label_column(has_weight: bool) -> usize {
    4 + has_weight as usize
}
fn finish_single<'w, 'g>(
    collector: RunCollector<'w, 'g>,
    workspace: &'w RunWorkspace,
    governor: &'g ResourceGovernor,
    fanout: usize,
    max: usize,
) -> GraphResult<Option<StagedRun<'g>>> {
    let mut runs = collector.finish()?;
    match runs.len() {
        0 => Ok(None),
        1 => Ok(runs.pop()),
        _ => merge_runs(workspace, governor, runs, fanout, max).map(|(run, _)| Some(run)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_label_column_follows_optional_weight() {
        assert_eq!(dynamic_label_column(false), 4);
        assert_eq!(dynamic_label_column(true), 5);
    }

    #[test]
    fn stable_edge_order_includes_every_optional_projection() {
        assert_eq!(stable_optional_order(false, false), "");
        assert_eq!(stable_optional_order(true, false), ", 4");
        assert_eq!(stable_optional_order(false, true), ", 4");
        assert_eq!(stable_optional_order(true, true), ", 4, 5");
    }

    #[test]
    fn edge_codec_round_trips() {
        let record = encode_raw_edge(7, "key", 1, 2, 3, Some(4), true).unwrap();
        let decoded = decode_raw_edge(&record).unwrap();
        assert_eq!(
            (
                decoded.mapping_id,
                decoded.source_key,
                decoded.source,
                decoded.target,
                decoded.type_id,
                decoded.weight,
                decoded.bidirectional
            ),
            (7, "key", 1, 2, 3, 4, true)
        );
    }

    #[test]
    fn edge_type_registry_reserves_slot_zero_and_rejects_entry_256() {
        let governor = ResourceGovernor::new(crate::resource::ResourceLimits::memory_only(
            crate::resource::MemoryBudget::new(ByteCount::from_bytes(64 * 1024)),
        ));
        let mut memory = governor
            .reserve_memory(ResourcePhase::EdgeResolve, ByteCount::ZERO)
            .unwrap();
        let mut registry = vec![String::new()];
        for index in 0..254 {
            let label = format!("type-{index}");
            assert!(intern_edge_type(&mut registry, &mut memory, &label, 128).is_ok());
        }
        assert_eq!(registry.len(), 255);
        assert!(intern_edge_type(&mut registry, &mut memory, "overflow", 128).is_err());
    }
}
