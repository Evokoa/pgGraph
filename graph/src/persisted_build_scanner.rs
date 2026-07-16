//! Bounded source scanning for direct persisted builds.
//!
//! This module deliberately produces external-sort records rather than an
//! owned graph engine. Record keys use big-endian numeric fields so bytewise
//! run ordering matches numeric ordering; record values use explicit
//! little-endian codecs for later artifact assembly.

use pgrx::prelude::*;

use crate::build_runs::{merge_runs, RunCollector, RunKind, RunRecord, RunWorkspace, StagedRun};
use crate::build_spool::{create_node_lookup_spool, NodeLookupBatch, NodeLookupGuard};
use crate::builder::{RegisteredEdge, RegisteredFilterColumn, RegisteredTable};
use crate::catalog::sql_table_name_from_oid;
use crate::filter_index::FilterColumnType;
use crate::quote::quote_ident;
use crate::resolution_index::ResolutionIndexBuilder;
use crate::resource::{ByteCount, ResourceGovernor, ResourcePhase};
use crate::safety::{GraphError, GraphResult};

const NODE_RECORD_FIXED_BYTES: usize = 12;
const FILTER_KEY_FIXED_BYTES: usize = 8;
const FILTER_VALUE_HEADER_BYTES: usize = 2;

/// Governed run sets and bounded metadata produced by the node scan.
pub(crate) struct PersistedSourceRuns<'governor> {
    pub(crate) node_count: u32,
    pub(crate) pressure_events: u64,
    pub(crate) tenanted_table_oids: Vec<u32>,
    pub(crate) has_forward_weights: bool,
    pub(crate) has_unidirectional_edges: bool,
    pub(crate) nodes: Option<StagedRun<'governor>>,
    pub(crate) resolution: Option<StagedRun<'governor>>,
    pub(crate) filters: Option<StagedRun<'governor>>,
    pub(crate) filter_dictionary: Option<StagedRun<'governor>>,
    pub(crate) tenant_tokens: Option<StagedRun<'governor>>,
    pub(crate) tenant_dictionary: Option<StagedRun<'governor>>,
    pub(crate) node_lookup: NodeLookupGuard<'governor>,
    _metadata_memory: crate::resource::ResourceLease<'governor>,
}

/// Scan registered node sources through bounded SPI cursor fetches.
///
/// The returned run files remain governed until dropped. Relationships are
/// accepted here so fixed build metadata is derived from the same validated
/// catalog snapshot, but relationship rows are staged by the edge scanner.
///
/// # Errors
///
/// Returns an error for invalid catalog filter types, SPI failures, node-index
/// overflow, oversized records, resource-limit exhaustion, or allocation
/// failure while staging bounded metadata.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_persisted_sources<'governor>(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    filters: &[RegisteredFilterColumn],
    workspace: &RunWorkspace,
    governor: &'governor ResourceGovernor,
    batch_size: usize,
    run_target: ByteCount,
    max_record_bytes: usize,
    merge_fanout: usize,
) -> GraphResult<PersistedSourceRuns<'governor>> {
    if batch_size == 0 || merge_fanout < 2 {
        return Err(GraphError::Internal(
            "persisted source scan batch size must be positive and merge fanout at least two"
                .into(),
        ));
    }

    let mut nodes = RunCollector::new(
        workspace,
        governor,
        RunKind::Nodes,
        run_target,
        max_record_bytes,
    )?;
    let mut resolution = RunCollector::new(
        workspace,
        governor,
        RunKind::Resolution,
        run_target,
        max_record_bytes,
    )?;
    let mut filter_values = RunCollector::new(
        workspace,
        governor,
        RunKind::Filters,
        run_target,
        max_record_bytes,
    )?;
    let mut filter_dictionary = RunCollector::new(
        workspace,
        governor,
        RunKind::FilterDictionary,
        run_target,
        max_record_bytes,
    )?;
    let mut tenant_dictionary = RunCollector::new(
        workspace,
        governor,
        RunKind::TenantDictionary,
        run_target,
        max_record_bytes,
    )?;
    create_node_lookup_spool()?;
    let mut node_lookup = NodeLookupBatch::with_limits(
        governor,
        i32::try_from(batch_size).unwrap_or(i32::MAX),
        run_target,
    )?;

    let mut node_count = 0_u32;
    let mut tenanted_table_oids = Vec::new();
    let metadata_bytes = tables
        .len()
        .checked_mul(std::mem::size_of::<u32>())
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("source metadata size overflowed".into()))?;
    let metadata_memory = governor
        .reserve_memory(ResourcePhase::Nodes, metadata_bytes)
        .map_err(resource_error)?;
    tenanted_table_oids
        .try_reserve(tables.len())
        .map_err(|error| allocation_error("tenanted table OIDs", error))?;

    for table in tables {
        governor
            .check_elapsed(ResourcePhase::Nodes)
            .map_err(resource_error)?;
        crate::resource::check_postgres_interrupts();

        let identifier_bytes = table
            .id_columns
            .columns()
            .iter()
            .map(String::len)
            .chain(filters.iter().map(|filter| filter.column_name.len()))
            .chain(table.tenant_column.iter().map(String::len))
            .try_fold(0_usize, |total, bytes| total.checked_add(bytes))
            .ok_or_else(|| GraphError::Internal("source query size overflowed".into()))?;
        let query_capacity = filters
            .len()
            .checked_mul(128)
            .and_then(|bytes| {
                identifier_bytes
                    .checked_mul(6)
                    .and_then(|identifiers| bytes.checked_add(identifiers))
            })
            .and_then(|bytes| bytes.checked_add(1024))
            .ok_or_else(|| GraphError::Internal("source query size overflowed".into()))?;
        let table_workspace_bytes = filters
            .len()
            .checked_mul(std::mem::size_of::<(usize, &RegisteredFilterColumn)>())
            .and_then(|bytes| bytes.checked_add(query_capacity))
            .and_then(|bytes| bytes.checked_add(512))
            .and_then(ByteCount::from_usize)
            .ok_or_else(|| GraphError::Internal("source query workspace overflowed".into()))?;
        let _table_workspace = governor
            .reserve_memory(ResourcePhase::Nodes, table_workspace_bytes)
            .map_err(resource_error)?;
        let mut table_filters = Vec::new();
        table_filters
            .try_reserve(filters.len())
            .map_err(|error| allocation_error("table filter catalog", error))?;
        table_filters.extend(
            filters
                .iter()
                .enumerate()
                .filter(|(_, filter)| filter.table_oid == table.table_oid),
        );
        for (_, filter) in &table_filters {
            FilterColumnType::parse(&filter.column_type)
                .map_err(|reason| GraphError::InvalidFilter { reason })?;
        }
        if table.tenant_column.is_some() {
            tenanted_table_oids.push(table.table_oid);
        }

        let pk_expression = table.id_columns.select_expr();
        let tenant_expression = table
            .tenant_column
            .as_ref()
            .map(|column| format!("{}::text", quote_ident(column)));
        let source = sql_table_name_from_oid(table.table_oid)?;
        let mut query = String::new();
        query
            .try_reserve_exact(query_capacity)
            .map_err(|error| allocation_error("source scan query", error))?;
        query.push_str("SELECT ");
        query.push_str(&pk_expression);
        for (_, filter) in &table_filters {
            query.push_str(", ");
            query.push_str(&filter_select_expr(filter));
        }
        if let Some(tenant_expression) = tenant_expression {
            query.push_str(", ");
            query.push_str(&tenant_expression);
        }
        query.push_str(" FROM ");
        query.push_str(source.as_sql());
        // Dense node indices are persisted and define the stable CSR neighbor
        // order used by capped traversals. PostgreSQL heap order can change
        // after VACUUM or a restore, so assign them by authoritative identity.
        query.push_str(" ORDER BY 1");
        let fetch_rows = i64::try_from(batch_size).unwrap_or(i64::MAX).max(1);

        Spi::connect(|client| {
            let mut cursor = client.open_cursor(&query, &[]);
            loop {
                governor
                    .check_elapsed(ResourcePhase::Nodes)
                    .map_err(resource_error)?;
                crate::resource::check_postgres_interrupts();
                let rows = cursor
                    .fetch(fetch_rows)
                    .map_err(|error| GraphError::Internal(format!("SPI fetch failed: {error}")))?;
                if rows.is_empty() {
                    break;
                }
                for row in rows {
                    let Some(primary_key) = row.get::<String>(1).map_err(|error| {
                        GraphError::Internal(format!("cannot read node primary key: {error}"))
                    })?
                    else {
                        continue;
                    };
                    let node_idx = node_count;
                    node_count = node_count.checked_add(1).ok_or_else(|| {
                        GraphError::Internal("persisted build exceeds u32 nodes".into())
                    })?;

                    nodes.push(encode_node_record(node_idx, table.table_oid, &primary_key)?)?;
                    node_lookup.push(table.table_oid, &primary_key, node_idx)?;
                    resolution.push(encode_resolution_record(
                        node_idx,
                        table.table_oid,
                        &primary_key,
                    )?)?;

                    for (table_filter_ordinal, (global_filter_ordinal, filter)) in
                        table_filters.iter().enumerate()
                    {
                        let column_idx = table_filter_ordinal.checked_add(2).ok_or_else(|| {
                            GraphError::Internal("filter column index overflowed".into())
                        })?;
                        if let Some(encoded) = read_filter_value(&row, column_idx, filter)? {
                            let ordinal = u32::try_from(*global_filter_ordinal).map_err(|_| {
                                GraphError::Internal("too many filter columns".into())
                            })?;
                            match encoded {
                                ScannedFilterValue::Text(text) => filter_dictionary
                                    .push(encode_dictionary_record(ordinal, &text, node_idx)?)?,
                                encoded => filter_values
                                    .push(encode_filter_record(ordinal, node_idx, &encoded)?)?,
                            }
                        }
                    }

                    if table.tenant_column.is_some() {
                        let tenant_column =
                            table_filters.len().checked_add(2).ok_or_else(|| {
                                GraphError::Internal("tenant column index overflowed".into())
                            })?;
                        if let Some(tenant) = row.get::<String>(tenant_column).map_err(|error| {
                            GraphError::Internal(format!("cannot read tenant value: {error}"))
                        })? {
                            tenant_dictionary
                                .push(encode_tenant_dictionary_record(&tenant, node_idx)?)?;
                        }
                    }
                }
            }
            Ok::<(), GraphError>(())
        })?;
    }

    tenanted_table_oids.sort_unstable();
    tenanted_table_oids.dedup();
    let pressure_events = node_lookup.pressure_events();
    let node_lookup = node_lookup.finish()?;
    let raw_filter_dictionary = finish_single(
        filter_dictionary,
        workspace,
        governor,
        merge_fanout,
        max_record_bytes,
    )?;
    let mut normalized_filter_dictionary = RunCollector::new(
        workspace,
        governor,
        RunKind::FilterDictionary,
        run_target,
        max_record_bytes,
    )?;
    normalize_text_filters(
        raw_filter_dictionary.as_ref(),
        &mut normalized_filter_dictionary,
        &mut filter_values,
        max_record_bytes,
        governor,
    )?;
    drop(raw_filter_dictionary);

    let raw_tenant_dictionary = finish_single(
        tenant_dictionary,
        workspace,
        governor,
        merge_fanout,
        max_record_bytes,
    )?;
    let mut tenant_tokens = RunCollector::new(
        workspace,
        governor,
        RunKind::TenantValues,
        run_target,
        max_record_bytes,
    )?;
    let mut normalized_tenant_dictionary = RunCollector::new(
        workspace,
        governor,
        RunKind::TenantDictionary,
        run_target,
        max_record_bytes,
    )?;
    normalize_tenant_tokens(
        raw_tenant_dictionary.as_ref(),
        &mut normalized_tenant_dictionary,
        &mut tenant_tokens,
        max_record_bytes,
        governor,
    )?;
    drop(raw_tenant_dictionary);

    Ok(PersistedSourceRuns {
        node_count,
        pressure_events,
        tenanted_table_oids,
        has_forward_weights: edges.iter().any(|edge| edge.weight_column.is_some()),
        has_unidirectional_edges: edges.iter().any(|edge| !edge.bidirectional),
        nodes: finish_single(nodes, workspace, governor, merge_fanout, max_record_bytes)?,
        resolution: finish_single(
            resolution,
            workspace,
            governor,
            merge_fanout,
            max_record_bytes,
        )?,
        filters: finish_single(
            filter_values,
            workspace,
            governor,
            merge_fanout,
            max_record_bytes,
        )?,
        filter_dictionary: finish_single(
            normalized_filter_dictionary,
            workspace,
            governor,
            merge_fanout,
            max_record_bytes,
        )?,
        tenant_tokens: finish_single(
            tenant_tokens,
            workspace,
            governor,
            merge_fanout,
            max_record_bytes,
        )?,
        tenant_dictionary: finish_single(
            normalized_tenant_dictionary,
            workspace,
            governor,
            merge_fanout,
            max_record_bytes,
        )?,
        node_lookup,
        _metadata_memory: metadata_memory,
    })
}

fn finish_single<'workspace, 'governor>(
    collector: RunCollector<'workspace, 'governor>,
    workspace: &'workspace RunWorkspace,
    governor: &'governor ResourceGovernor,
    merge_fanout: usize,
    max_record_bytes: usize,
) -> GraphResult<Option<StagedRun<'governor>>> {
    let mut runs = collector.finish()?;
    match runs.len() {
        0 => Ok(None),
        1 => Ok(runs.pop()),
        _ => merge_runs(workspace, governor, runs, merge_fanout, max_record_bytes)
            .map(|(run, _)| Some(run)),
    }
}

fn normalize_text_filters(
    raw_dictionary: Option<&StagedRun<'_>>,
    normalized_dictionary: &mut RunCollector<'_, '_>,
    normalized_values: &mut RunCollector<'_, '_>,
    max_record_bytes: usize,
    governor: &ResourceGovernor,
) -> GraphResult<()> {
    let Some(raw_dictionary) = raw_dictionary else {
        return Ok(());
    };
    let _retained_scratch = governor
        .reserve_memory(
            ResourcePhase::Filters,
            ByteCount::from_bytes(max_record_bytes as u64),
        )
        .map_err(resource_error)?;
    let mut previous_ordinal = None;
    let mut previous_text = Vec::new();
    let mut token = 0_u32;
    raw_dictionary.replay(max_record_bytes, |record| {
        if record.key.len() < 9 {
            return Err(corrupt_scanner_run("text dictionary key is truncated"));
        }
        let ordinal = read_be_u32(&record.key, 0)?;
        let node_offset = record.key.len() - 4;
        let node_idx = read_be_u32(&record.key, node_offset)?;
        let is_new = previous_ordinal != Some(ordinal) || previous_text != record.value;
        if is_new {
            token = if previous_ordinal == Some(ordinal) {
                token.checked_add(1).ok_or_else(|| {
                    GraphError::Internal("text filter dictionary exceeds u32 tokens".into())
                })?
            } else {
                0
            };
            previous_ordinal = Some(ordinal);
            previous_text.clone_from(&record.value);
            let mut key = Vec::new();
            key.try_reserve_exact(8)
                .map_err(|error| allocation_error("filter dictionary token key", error))?;
            key.extend_from_slice(&ordinal.to_be_bytes());
            key.extend_from_slice(&token.to_be_bytes());
            normalized_dictionary.push(RunRecord::new(key, record.value.clone()))?;
        }
        normalized_values.push(encode_filter_record(
            ordinal,
            node_idx,
            &ScannedFilterValue::TextToken(token),
        )?)
    })
}

fn normalize_tenant_tokens(
    raw_dictionary: Option<&StagedRun<'_>>,
    normalized_dictionary: &mut RunCollector<'_, '_>,
    tokens_by_node: &mut RunCollector<'_, '_>,
    max_record_bytes: usize,
    governor: &ResourceGovernor,
) -> GraphResult<()> {
    let Some(raw_dictionary) = raw_dictionary else {
        return Ok(());
    };
    let _retained_scratch = governor
        .reserve_memory(
            ResourcePhase::Filters,
            ByteCount::from_bytes(max_record_bytes as u64),
        )
        .map_err(resource_error)?;
    let mut previous_tenant: Option<Vec<u8>> = None;
    let mut token = 0_u32;
    raw_dictionary.replay(max_record_bytes, |record| {
        if record.key.len() < 5 {
            return Err(corrupt_scanner_run("tenant dictionary key is truncated"));
        }
        if previous_tenant.as_deref() != Some(record.value.as_slice()) {
            token = token.checked_add(1).ok_or_else(|| {
                GraphError::Internal("tenant dictionary exceeds u32 tokens".into())
            })?;
            previous_tenant = Some(record.value.clone());
            normalized_dictionary
                .push(RunRecord::new(record.value.clone(), record.value.clone()))?;
        }
        let node_idx = read_be_u32(&record.key, record.key.len() - 4)?;
        tokens_by_node.push(RunRecord::new(
            node_idx.to_be_bytes().to_vec(),
            token.to_le_bytes().to_vec(),
        ))
    })
}

fn read_be_u32(bytes: &[u8], offset: usize) -> GraphResult<u32> {
    let value = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| corrupt_scanner_run("scanner run integer is truncated"))?;
    Ok(u32::from_be_bytes(value.try_into().map_err(|_| {
        corrupt_scanner_run("scanner run integer has invalid width")
    })?))
}

fn corrupt_scanner_run(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: reason.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScannedFilterValue {
    Numeric(i64),
    Boolean(bool),
    Text(String),
    TextToken(u32),
    Date(i64),
    Timestamptz(i64),
    Uuid(u128),
}

fn encode_node_record(node_idx: u32, table_oid: u32, primary_key: &str) -> GraphResult<RunRecord> {
    let pk_len = u32::try_from(primary_key.len())
        .map_err(|_| GraphError::Internal("node primary key exceeds u32 bytes".into()))?;
    let mut value = Vec::new();
    value
        .try_reserve_exact(NODE_RECORD_FIXED_BYTES + primary_key.len())
        .map_err(|error| allocation_error("node run record", error))?;
    value.extend_from_slice(&node_idx.to_le_bytes());
    value.extend_from_slice(&table_oid.to_le_bytes());
    value.extend_from_slice(&pk_len.to_le_bytes());
    value.extend_from_slice(primary_key.as_bytes());
    Ok(RunRecord::new(node_idx.to_be_bytes().to_vec(), value))
}

fn encode_resolution_record(
    node_idx: u32,
    table_oid: u32,
    primary_key: &str,
) -> GraphResult<RunRecord> {
    let hash = ResolutionIndexBuilder::hash_pk(primary_key);
    let mut key = Vec::new();
    key.try_reserve_exact(16)
        .map_err(|error| allocation_error("resolution key", error))?;
    key.extend_from_slice(&table_oid.to_be_bytes());
    key.extend_from_slice(&hash.to_be_bytes());
    key.extend_from_slice(&node_idx.to_be_bytes());
    let mut value = Vec::new();
    value
        .try_reserve_exact(16)
        .map_err(|error| allocation_error("resolution value", error))?;
    value.extend_from_slice(&table_oid.to_le_bytes());
    value.extend_from_slice(&hash.to_le_bytes());
    value.extend_from_slice(&node_idx.to_le_bytes());
    Ok(RunRecord::new(key, value))
}

fn encode_filter_record(
    ordinal: u32,
    node_idx: u32,
    value: &ScannedFilterValue,
) -> GraphResult<RunRecord> {
    let mut key = Vec::new();
    key.try_reserve_exact(FILTER_KEY_FIXED_BYTES)
        .map_err(|error| allocation_error("filter key", error))?;
    key.extend_from_slice(&ordinal.to_be_bytes());
    key.extend_from_slice(&node_idx.to_be_bytes());
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(FILTER_VALUE_HEADER_BYTES + filter_payload_len(value))
        .map_err(|error| allocation_error("filter run record", error))?;
    match value {
        ScannedFilterValue::Numeric(number) => {
            encoded.push(1);
            encoded.extend_from_slice(&number.to_le_bytes());
        }
        ScannedFilterValue::Boolean(boolean) => {
            encoded.push(2);
            encoded.push(u8::from(*boolean));
        }
        ScannedFilterValue::Text(text) => {
            encoded.push(3);
            let len = u32::try_from(text.len())
                .map_err(|_| GraphError::Internal("text filter exceeds u32 bytes".into()))?;
            encoded.extend_from_slice(&len.to_le_bytes());
            encoded.extend_from_slice(text.as_bytes());
        }
        ScannedFilterValue::TextToken(token) => {
            encoded.push(3);
            encoded.extend_from_slice(&token.to_le_bytes());
        }
        ScannedFilterValue::Date(days) => {
            encoded.push(4);
            encoded.extend_from_slice(&days.to_le_bytes());
        }
        ScannedFilterValue::Timestamptz(micros) => {
            encoded.push(5);
            encoded.extend_from_slice(&micros.to_le_bytes());
        }
        ScannedFilterValue::Uuid(uuid) => {
            encoded.push(6);
            encoded.extend_from_slice(&uuid.to_le_bytes());
        }
    }
    Ok(RunRecord::new(key, encoded))
}

fn filter_payload_len(value: &ScannedFilterValue) -> usize {
    match value {
        ScannedFilterValue::Boolean(_) => 1,
        ScannedFilterValue::Text(text) => 4 + text.len(),
        ScannedFilterValue::TextToken(_) => 4,
        ScannedFilterValue::Uuid(_) => 16,
        ScannedFilterValue::Numeric(_)
        | ScannedFilterValue::Date(_)
        | ScannedFilterValue::Timestamptz(_) => 8,
    }
}

fn encode_dictionary_record(ordinal: u32, text: &str, node_idx: u32) -> GraphResult<RunRecord> {
    let mut key = Vec::new();
    key.try_reserve_exact(4 + text.len() + 5)
        .map_err(|error| allocation_error("filter dictionary key", error))?;
    key.extend_from_slice(&ordinal.to_be_bytes());
    key.extend_from_slice(text.as_bytes());
    key.push(0);
    key.extend_from_slice(&node_idx.to_be_bytes());
    Ok(RunRecord::new(key, text.as_bytes().to_vec()))
}

fn encode_tenant_dictionary_record(tenant: &str, node_idx: u32) -> GraphResult<RunRecord> {
    let mut key = Vec::new();
    key.try_reserve_exact(tenant.len() + 5)
        .map_err(|error| allocation_error("tenant dictionary key", error))?;
    key.extend_from_slice(tenant.as_bytes());
    key.push(0);
    key.extend_from_slice(&node_idx.to_be_bytes());
    Ok(RunRecord::new(key, tenant.as_bytes().to_vec()))
}

fn filter_select_expr(filter: &RegisteredFilterColumn) -> String {
    let column = quote_ident(&filter.column_name);
    match filter.column_type.to_ascii_lowercase().as_str() {
        "numeric" => format!("({column})::bigint"),
        "boolean" => format!("({column})::boolean"),
        "text" => format!("({column})::text"),
        "date" => format!("(({column})::date - DATE '2000-01-01')::bigint"),
        "timestamptz" => {
            format!("(EXTRACT(EPOCH FROM ({column})::timestamptz) * 1000000)::bigint")
        }
        "uuid" => format!("({column})::text"),
        _ => format!("({column})::bigint"),
    }
}

fn read_filter_value(
    row: &pgrx::spi::SpiHeapTupleData<'_>,
    column_idx: usize,
    filter: &RegisteredFilterColumn,
) -> GraphResult<Option<ScannedFilterValue>> {
    let column_type = FilterColumnType::parse(&filter.column_type)
        .map_err(|reason| GraphError::InvalidFilter { reason })?;
    match column_type {
        FilterColumnType::Numeric => {
            read_i64(row, column_idx).map(|value| value.map(ScannedFilterValue::Numeric))
        }
        FilterColumnType::Boolean => row
            .get::<bool>(column_idx)
            .map(|value| value.map(ScannedFilterValue::Boolean))
            .map_err(filter_read_error),
        FilterColumnType::Text => row
            .get::<String>(column_idx)
            .map(|value| value.map(ScannedFilterValue::Text))
            .map_err(filter_read_error),
        FilterColumnType::Date => {
            read_i64(row, column_idx).map(|value| value.map(ScannedFilterValue::Date))
        }
        FilterColumnType::Timestamptz => {
            read_i64(row, column_idx).map(|value| value.map(ScannedFilterValue::Timestamptz))
        }
        FilterColumnType::Uuid => row
            .get::<String>(column_idx)
            .map_err(filter_read_error)?
            .map(|value| parse_uuid(&value).map(ScannedFilterValue::Uuid))
            .transpose(),
    }
}

fn read_i64(row: &pgrx::spi::SpiHeapTupleData<'_>, column_idx: usize) -> GraphResult<Option<i64>> {
    row.get::<i64>(column_idx).map_err(filter_read_error)
}

fn parse_uuid(value: &str) -> GraphResult<u128> {
    let mut compact = String::new();
    compact
        .try_reserve_exact(32)
        .map_err(|error| allocation_error("UUID filter", error))?;
    compact.extend(value.chars().filter(|character| *character != '-'));
    if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GraphError::InvalidFilter {
            reason: format!("invalid UUID filter value '{value}'"),
        });
    }
    u128::from_str_radix(&compact, 16).map_err(|error| GraphError::InvalidFilter {
        reason: format!("invalid UUID filter value '{value}': {error}"),
    })
}

fn filter_read_error(error: pgrx::spi::SpiError) -> GraphError {
    GraphError::Internal(format!("filter value read failed: {error}"))
}

fn resource_error(error: crate::resource::ResourceLimitError) -> GraphError {
    crate::safety::resource_limit_error(error)
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
    use super::*;

    #[test]
    fn node_codec_carries_index_oid_and_primary_key() {
        let record = encode_node_record(7, 42, "tenant:node").expect("record encodes");
        assert_eq!(record.key, 7_u32.to_be_bytes());
        assert_eq!(&record.value[0..4], &7_u32.to_le_bytes());
        assert_eq!(&record.value[4..8], &42_u32.to_le_bytes());
        assert_eq!(&record.value[8..12], &11_u32.to_le_bytes());
        assert_eq!(&record.value[12..], b"tenant:node");
    }

    #[test]
    fn resolution_keys_sort_by_oid_hash_then_node() {
        let first = encode_resolution_record(9, 5, "A").unwrap();
        let later_node = encode_resolution_record(10, 5, "A").unwrap();
        let later_table = encode_resolution_record(0, 6, "A").unwrap();
        assert!(first.key < later_node.key);
        assert!(later_node.key < later_table.key);
        assert_eq!(first.value.len(), crate::resolution_index::ENTRY_SIZE);
    }

    #[test]
    fn filter_keys_sort_numerically_not_by_native_endianness() {
        let low =
            encode_filter_record(2, 255, &ScannedFilterValue::Boolean(true)).expect("low record");
        let high =
            encode_filter_record(2, 256, &ScannedFilterValue::Boolean(false)).expect("high record");
        assert!(low.key < high.key);
        assert_eq!(low.value, vec![2, 1]);
        assert_eq!(high.value, vec![2, 0]);
    }

    #[test]
    fn text_and_tenant_dictionary_keys_are_lexical_with_stable_ties() {
        let alpha = encode_tenant_dictionary_record("alpha", 9).expect("alpha");
        let beta = encode_tenant_dictionary_record("beta", 0).expect("beta");
        let alpha_later = encode_tenant_dictionary_record("alpha", 10).expect("alpha later");
        assert!(alpha.key < alpha_later.key);
        assert!(alpha_later.key < beta.key);

        let text = encode_dictionary_record(3, "zeta", 8).expect("text dictionary");
        assert_eq!(&text.key[..4], &[0, 0, 0, 3]);
        assert_eq!(text.value, b"zeta");
    }

    #[test]
    fn uuid_codec_accepts_canonical_form_and_rejects_invalid_text() {
        assert_eq!(
            parse_uuid("550e8400-e29b-41d4-a716-446655440000").expect("UUID"),
            0x550e8400e29b41d4a716446655440000
        );
        assert!(parse_uuid("not-a-uuid").is_err());
    }
}
