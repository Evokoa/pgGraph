//! # Builder — Graph construction from SQL tables
//!
//! Reads the graph catalog (registered tables, edges, filter columns),
//! queries Postgres via SPI, and constructs the NodeStore, EdgeStore,
//! ResolutionIndex, and FilterIndex.
//!
//! The build process:
//! 1. Read catalog tables to determine what to ingest
//! 2. OOM pre-check via `pg_class.reltuples` estimates
//! 3. Read registered tables through SPI cursor batches and populate stores
//! 4. Resolve registered edges through temporary spool tables
//! 5. Stream sorted edge spool rows into CSR
//! 6. Return an in-memory [`Engine`] for the SQL orchestration layer to install
//!    and optionally persist
//!
//! See: `docs/contributor_guide/build-pipeline.mdx`

use std::collections::HashMap;
use std::time::Instant;

use pgrx::prelude::*;

use crate::build_spool::{create_node_lookup_spool, NodeLookupBatch};
use crate::catalog::{estimated_table_rows, sql_table_name_from_oid};
use crate::config::BuildScanMode;
use crate::edge_store::{
    IdentifiedRawEdge, RawEdge, RelationshipId, RelationshipIdentity, SortedEdgeStoreBuilder,
};
use crate::engine::Engine;
use crate::filter_index::{EncodedFilterValue, FilterColumnType};
use crate::quote::quote_ident;
use crate::resource::{AdaptiveBatch, BatchAction, ByteCount, ResourcePhase};
use crate::safety::{GraphError, GraphResult};

/// Typed primary-key column set for a registered table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrimaryKeySpec {
    columns: Vec<String>,
}

impl PrimaryKeySpec {
    pub(crate) fn from_columns(columns: Vec<String>) -> Self {
        Self { columns }
    }

    pub(crate) fn from_catalog_text(raw: &str) -> Self {
        Self {
            columns: split_catalog_columns(raw),
        }
    }

    pub(crate) fn columns(&self) -> &[String] {
        &self.columns
    }

    pub(crate) fn as_catalog_text(&self) -> String {
        self.columns.join(",")
    }

    pub(crate) fn select_expr(&self) -> String {
        if self.columns.len() > 1 {
            let parts = self
                .columns
                .iter()
                .map(|col| format!("{}::text", quote_ident(col)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("jsonb_build_array({parts})::text")
        } else if let Some(column) = self.columns.first() {
            format!("{}::text", quote_ident(column))
        } else {
            "NULL::text".to_string()
        }
    }
}

impl From<&str> for PrimaryKeySpec {
    fn from(value: &str) -> Self {
        Self::from_catalog_text(value)
    }
}

impl From<&PrimaryKeySpec> for PrimaryKeySpec {
    fn from(value: &PrimaryKeySpec) -> Self {
        value.clone()
    }
}

impl From<Vec<String>> for PrimaryKeySpec {
    fn from(value: Vec<String>) -> Self {
        Self::from_columns(value)
    }
}

/// Typed property-column set for a registered table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyColumns {
    columns: Vec<String>,
}

impl PropertyColumns {
    pub(crate) fn from_columns(columns: Vec<String>) -> Self {
        Self { columns }
    }

    pub(crate) fn from_catalog_text(raw: &str) -> Self {
        Self {
            columns: split_catalog_columns(raw),
        }
    }

    pub(crate) fn as_slice(&self) -> &[String] {
        &self.columns
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &String> {
        self.columns.iter()
    }

    pub(crate) fn to_vec(&self) -> Vec<String> {
        self.columns.clone()
    }

    pub(crate) fn as_catalog_text(&self) -> String {
        self.columns.join(",")
    }
}

impl From<&str> for PropertyColumns {
    fn from(value: &str) -> Self {
        Self::from_catalog_text(value)
    }
}

impl From<&PropertyColumns> for PropertyColumns {
    fn from(value: &PropertyColumns) -> Self {
        value.clone()
    }
}

impl From<Vec<String>> for PropertyColumns {
    fn from(value: Vec<String>) -> Self {
        Self::from_columns(value)
    }
}

pub(crate) fn split_catalog_columns(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|col| !col.is_empty())
        .map(ToString::to_string)
        .collect()
}

enum PendingFilterValue {
    Encoded(EncodedFilterValue),
    Text(String),
}

struct UnresolvedEdge {
    from_pk: String,
    to_pk: String,
    mapping_id: u64,
    source_key: String,
    type_id: u8,
    weight: Option<u32>,
    bidirectional: bool,
}

fn structural_text_value(value: Option<String>) -> Option<String> {
    value
}

/// Registered table in the graph catalog.
#[derive(Debug, Clone)]
pub struct RegisteredTable {
    /// PostgreSQL OID that remains stable across relation renames.
    pub table_oid: u32,
    pub table_name: String,
    pub id_columns: PrimaryKeySpec,
    pub columns: PropertyColumns,
    pub tenant_column: Option<String>,
}

/// Registered edge in the graph catalog.
#[derive(Debug, Clone)]
pub struct RegisteredEdge {
    /// Durable catalog identity for this relationship mapping.
    pub mapping_id: u64,
    /// PostgreSQL OID of the source edge relation.
    pub from_table_oid: u32,
    pub from_table: String,
    pub from_column: String,
    /// Declared PostgreSQL primary-key columns that identify one source
    /// relationship row independently of its endpoints.
    pub source_key_columns: PrimaryKeySpec,
    /// PostgreSQL OID of the target node relation.
    pub to_table_oid: u32,
    pub to_table: String,
    pub to_column: String,
    pub label: String,
    pub bidirectional: bool,
    pub weight_column: Option<String>,
    pub label_column: Option<String>,
}

/// Registered typed filter column in the graph catalog.
#[derive(Debug, Clone)]
pub struct RegisteredFilterColumn {
    /// PostgreSQL OID of the source relation.
    pub table_oid: u32,
    pub table_name: String,
    pub column_name: String,
    pub column_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildMemoryEstimate {
    pub bytes: ByteCount,
}

pub fn estimate_graph_memory(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
) -> GraphResult<BuildMemoryEstimate> {
    estimate_graph_memory_with_counts(tables, edges, estimated_table_rows)
}

fn estimate_graph_memory_with_counts(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    mut row_count: impl FnMut(&str) -> GraphResult<i64>,
) -> GraphResult<BuildMemoryEstimate> {
    let mut table_counts: HashMap<String, i64> = HashMap::new();
    let mut est_nodes: u64 = 0;
    let mut est_edges: u64 = 0;

    for table in tables {
        let count = cached_table_count(&mut table_counts, &mut row_count, &table.table_name)?;
        let count = u64::try_from(count).map_err(|_| {
            GraphError::Internal("negative table row estimate reached build planning".to_string())
        })?;
        est_nodes = est_nodes
            .checked_add(count)
            .ok_or_else(|| GraphError::Internal("node row estimate overflowed u64".to_string()))?;
    }

    for edge in edges {
        let count = cached_table_count(&mut table_counts, &mut row_count, &edge.from_table)?;
        let count = u64::try_from(count).map_err(|_| {
            GraphError::Internal(
                "negative relationship row estimate reached build planning".to_string(),
            )
        })?;
        let multiplier = if edge.bidirectional { 2u64 } else { 1u64 };
        let directed_edges = count.checked_mul(multiplier).ok_or_else(|| {
            GraphError::Internal("relationship row estimate overflowed u64".to_string())
        })?;
        est_edges = est_edges.checked_add(directed_edges).ok_or_else(|| {
            GraphError::Internal("relationship row estimate overflowed u64".to_string())
        })?;
    }

    let node_bytes = ByteCount::from_bytes(est_nodes)
        .checked_mul(140)
        .ok_or_else(|| GraphError::Internal("node memory estimate overflowed u64".to_string()))?;
    let edge_bytes = ByteCount::from_bytes(est_edges)
        .checked_mul(5)
        .ok_or_else(|| {
            GraphError::Internal("relationship memory estimate overflowed u64".to_string())
        })?;
    Ok(BuildMemoryEstimate {
        bytes: node_bytes.checked_add(edge_bytes).ok_or_else(|| {
            GraphError::Internal("total build memory estimate overflowed u64".to_string())
        })?,
    })
}

fn cached_table_count(
    table_counts: &mut HashMap<String, i64>,
    row_count: &mut impl FnMut(&str) -> GraphResult<i64>,
    table_name: &str,
) -> GraphResult<i64> {
    if let Some(count) = table_counts.get(table_name) {
        return Ok(*count);
    }
    let count = row_count(table_name)?;
    table_counts.insert(table_name.to_string(), count);
    Ok(count)
}

/// Build the graph engine from registered tables and edges.
///
/// This is called by `graph.build()`.
#[cfg(feature = "pg_test")]
pub fn build_graph(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    filter_columns: &[RegisteredFilterColumn],
) -> GraphResult<Engine> {
    let limit = ByteCount::from_mib(crate::config::MEMORY_LIMIT_MB.get().max(1) as u64)
        .ok_or_else(|| GraphError::Internal("configured memory limit overflowed".to_string()))?;
    let governor = crate::resource::ResourceGovernor::new(
        crate::resource::ResourceLimits::memory_only(crate::resource::MemoryBudget::new(limit)),
    );
    let batch_byte_limit = crate::resource::adaptive_build_batch_target(limit);
    build_graph_with_governor(tables, edges, filter_columns, &governor, batch_byte_limit)
}

pub(crate) fn build_graph_with_governor(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    filter_columns: &[RegisteredFilterColumn],
    governor: &crate::resource::ResourceGovernor,
    batch_byte_limit: ByteCount,
) -> GraphResult<Engine> {
    let start = Instant::now();
    match crate::config::build_scan_mode() {
        BuildScanMode::Select => {}
        BuildScanMode::Copy => {
            return Err(GraphError::Internal(
                "graph.build_scan_mode = 'copy' requires a safe server-side COPY reader; pgrx 0.18 exposes only low-level pg_sys COPY hooks, so use 'select' in this build"
                    .to_string(),
            ));
        }
    }

    let mut engine = Engine::new();
    let mut table_oid_map: HashMap<String, u32> = HashMap::new();
    let mut pending_filter_values: Vec<((u32, String), u32, Option<PendingFilterValue>)> =
        Vec::new();
    let mut filter_populated_counts: HashMap<(u32, String), usize> = HashMap::new();
    let mut persistent_memory = governor
        .reserve_memory(ResourcePhase::Replacement, ByteCount::ZERO)
        .map_err(resource_error_to_graph)?;
    create_node_lookup_spool()?;
    let mut node_lookup_batch = NodeLookupBatch::with_limits(
        governor,
        crate::config::BUILD_BATCH_SIZE.get(),
        batch_byte_limit,
    )?;

    // Phase 1: Load all nodes from registered tables
    for table in tables {
        let oid = table.table_oid;
        table_oid_map.insert(table.table_name.clone(), oid);

        let table_filter_columns: Vec<&RegisteredFilterColumn> = filter_columns
            .iter()
            .filter(|filter| filter.table_oid == table.table_oid)
            .collect();

        let pk_expression = table.id_columns.select_expr();

        let tenant_column = table
            .tenant_column
            .as_ref()
            .map(|column| format!("{}::text", quote_ident(column)));

        let column_list = if table_filter_columns.is_empty() && tenant_column.is_none() {
            pk_expression.clone()
        } else {
            let cols: Vec<String> = std::iter::once(pk_expression.clone())
                .chain(
                    table_filter_columns
                        .iter()
                        .map(|c| filter_column_select_expr(c)),
                )
                .chain(tenant_column.clone())
                .collect();
            cols.join(", ")
        };

        let table_name = sql_table_name_from_oid(oid)?;
        let query = format!("SELECT {} FROM {}", column_list, table_name.as_sql());
        let filter_start_column = 2;
        let tenant_column_idx = filter_start_column + table_filter_columns.len();
        if table.tenant_column.is_some() {
            engine.tenanted_table_oids.insert(oid);
        }

        Spi::connect(|client| {
            let mut cursor = client.open_cursor(&query, &[]);
            let batch_size = crate::config::BUILD_BATCH_SIZE.get().max(1) as i64;
            loop {
                let table_result = cursor
                    .fetch(batch_size)
                    .map_err(|e| GraphError::Internal(format!("SPI fetch failed: {}", e)))?;

                if table_result.is_empty() {
                    break;
                }

                for row in table_result {
                    let Some(pk) = structural_text_value(
                        row.get::<String>(1)
                            .map_err(|e| GraphError::Internal(format!("Cannot read PK: {}", e)))?,
                    ) else {
                        continue;
                    };

                    persistent_memory
                        .try_grow_in(ResourcePhase::Nodes, node_memory_bytes(&pk)?)
                        .map_err(resource_error_to_graph)?;

                    let node_idx = engine.node_store.try_add_node(oid, pk.clone())?;
                    engine.try_build_insert_table_membership(oid, node_idx)?;
                    node_lookup_batch.push(oid, &pk, node_idx)?;

                    // Index in ResolutionIndex
                    let resolution_bytes = ByteCount::from_usize(
                        crate::resolution_index::ENTRY_SIZE,
                    )
                    .ok_or_else(|| {
                        GraphError::Internal(
                            "resolution entry size does not fit byte accounting".to_string(),
                        )
                    })?;
                    persistent_memory
                        .try_grow_in(ResourcePhase::Resolution, resolution_bytes)
                        .map_err(resource_error_to_graph)?;
                    engine.try_build_resolution_insert(oid, &pk, node_idx)?;

                    for (filter_idx, filter_col) in table_filter_columns.iter().enumerate() {
                        let value = read_encoded_filter_value(
                            &row,
                            filter_start_column + filter_idx,
                            filter_col,
                        )?;
                        let filter_bytes =
                            pending_filter_memory_bytes(&filter_col.column_name, &value)?;
                        persistent_memory
                            .try_grow_in(ResourcePhase::Filters, filter_bytes)
                            .map_err(resource_error_to_graph)?;
                        pending_filter_values
                            .try_reserve(1)
                            .map_err(|error| allocation_error("pending filter values", error))?;
                        if value.is_some() {
                            filter_populated_counts.try_reserve(1).map_err(|error| {
                                allocation_error("filter population counters", error)
                            })?;
                            *filter_populated_counts
                                .entry((filter_col.table_oid, filter_col.column_name.clone()))
                                .or_insert(0) += 1;
                        }
                        pending_filter_values.push((
                            (filter_col.table_oid, filter_col.column_name.clone()),
                            node_idx,
                            value,
                        ));
                    }

                    if table.tenant_column.is_some() {
                        if let Ok(Some(tenant)) = row.get::<String>(tenant_column_idx) {
                            let tenant_bytes = std::mem::size_of::<String>()
                                .checked_add(tenant.len())
                                .and_then(ByteCount::from_usize)
                                .ok_or_else(|| {
                                    GraphError::Internal(
                                        "tenant memory accounting overflowed".to_string(),
                                    )
                                })?;
                            persistent_memory
                                .try_grow_in(ResourcePhase::Tenants, tenant_bytes)
                                .map_err(resource_error_to_graph)?;
                            engine.try_build_insert_tenant_membership(&tenant, node_idx)?;
                        }
                    }
                }
            }

            Ok::<(), GraphError>(())
        })?;
    }
    let node_batch_pressure_events = node_lookup_batch.pressure_events();
    let _node_lookup_guard = node_lookup_batch.finish()?;

    register_filter_columns(&mut engine, filter_columns, &filter_populated_counts);
    for ((table_oid, column_name), node_idx, value) in pending_filter_values {
        if let Some(global_filter_idx) = engine
            .filter_index
            .find_column_for_table(table_oid, &column_name)
        {
            let value = match value {
                None => None,
                Some(PendingFilterValue::Encoded(value)) => Some(value),
                Some(PendingFilterValue::Text(value)) => Some(EncodedFilterValue::Text(
                    engine
                        .filter_index
                        .intern_text_value(global_filter_idx, &value)?,
                )),
            };
            engine
                .filter_index
                .set_encoded_value(global_filter_idx, node_idx, value);
        }
    }

    // Finalize node resolution before edge linking. This drops the compact
    // build accumulator and makes edge resolution use binary search over the
    // same sorted array that is persisted into the .pggraph file.
    engine.finalize_resolution();

    // Phase 2: Resolve edges into a temp spool using bounded UNNEST batches.
    // This avoids millions of row-at-a-time SPI inserts without retaining all
    // raw edges in Rust.
    let has_weights = edges.iter().any(|e| e.weight_column.is_some());
    create_edge_spool()?;
    let mut edge_batch = EdgeSpoolBatch::with_limits(
        governor,
        crate::config::BUILD_BATCH_SIZE.get(),
        batch_byte_limit,
    )?;
    let mut edge_resolution_pressure_events = 0u64;

    for edge in edges {
        let static_edge_type_id = if edge.label_column.is_none() {
            Some(engine.register_edge_type(&edge.label)?)
        } else {
            None
        };
        let from_oid = Some(edge.from_table_oid);
        let to_oid = Some(edge.to_table_oid);
        let fk_style_source = from_oid.and_then(|_| {
            tables
                .iter()
                .find(|table| table.table_oid == edge.from_table_oid)
                .map(|table| primary_key_expr(&table.id_columns))
        });
        let from_expr = fk_style_source
            .clone()
            .unwrap_or_else(|| quote_ident(&edge.from_column));
        let to_expr = if fk_style_source.is_some() {
            quote_ident(&edge.from_column)
        } else {
            quote_ident(&edge.to_column)
        };

        let weight_select = edge
            .weight_column
            .as_ref()
            .map(|weight| format!(", ({})::bigint", quote_ident(weight)))
            .unwrap_or_default();
        let label_select = edge
            .label_column
            .as_ref()
            .map(|label| format!(", {}::text", quote_ident(label)))
            .unwrap_or_default();
        let source_key_expr = primary_key_expr(&edge.source_key_columns);
        let label_column_index = 4 + usize::from(edge.weight_column.is_some());

        let query = format!(
            "SELECT ({})::text, ({})::text, ({})::text{}{}
             FROM {}",
            from_expr, to_expr, source_key_expr, weight_select, label_select, edge.from_table
        );

        Spi::connect(|client| {
            let mut cursor = client.open_cursor(&query, &[]);
            let batch_size = crate::config::BUILD_BATCH_SIZE.get().max(1) as i64;
            let mut unresolved_edges = Vec::new();
            let mut unresolved_policy = AdaptiveBatch::new(
                ResourcePhase::EdgeResolve,
                crate::config::BUILD_BATCH_SIZE.get().max(1) as usize,
                batch_byte_limit,
            );
            let mut unresolved_memory = governor
                .reserve_memory(ResourcePhase::EdgeResolve, ByteCount::ZERO)
                .map_err(resource_error_to_graph)?;
            loop {
                let table_result = cursor
                    .fetch(batch_size)
                    .map_err(|e| GraphError::Internal(format!("SPI fetch failed: {}", e)))?;

                if table_result.is_empty() {
                    break;
                }

                for row in table_result {
                    let Some(from_pk) =
                        structural_text_value(row.get::<String>(1).map_err(|e| {
                            GraphError::Internal(format!("Cannot read source: {}", e))
                        })?)
                    else {
                        continue;
                    };
                    let Some(to_pk) =
                        structural_text_value(row.get::<String>(2).map_err(|e| {
                            GraphError::Internal(format!("Cannot read target: {}", e))
                        })?)
                    else {
                        continue;
                    };
                    let Some(source_key) =
                        structural_text_value(row.get::<String>(3).map_err(|e| {
                            GraphError::Internal(format!(
                                "Cannot read relationship source key: {e}"
                            ))
                        })?)
                    else {
                        continue;
                    };
                    let weight = if edge.weight_column.is_some() {
                        row.get::<i64>(4)
                            .ok()
                            .flatten()
                            .map(|value| value.clamp(1, u32::MAX as i64) as u32)
                    } else {
                        None
                    };
                    let edge_type_id = if edge.label_column.is_some() {
                        let dynamic_label = row
                            .get::<String>(label_column_index)
                            .map_err(|e| {
                                GraphError::Internal(format!("Cannot read label_column: {}", e))
                            })?
                            .filter(|label| !label.trim().is_empty())
                            .unwrap_or_else(|| edge.label.clone());
                        engine.register_edge_type(&dynamic_label)?
                    } else {
                        let Some(edge_type_id) = static_edge_type_id else {
                            return Err(GraphError::Internal(
                                "static edge type id missing for edge without label_column"
                                    .to_string(),
                            ));
                        };
                        edge_type_id
                    };

                    let unresolved = UnresolvedEdge {
                        from_pk,
                        to_pk,
                        mapping_id: edge.mapping_id,
                        source_key,
                        type_id: edge_type_id,
                        weight,
                        bidirectional: edge.bidirectional,
                    };
                    push_unresolved_edge(
                        from_oid,
                        to_oid,
                        unresolved,
                        &mut unresolved_edges,
                        &mut unresolved_policy,
                        &mut unresolved_memory,
                        &mut edge_batch,
                    )?;
                }
            }
            flush_unresolved_edges(
                from_oid,
                to_oid,
                &mut unresolved_edges,
                &mut unresolved_policy,
                &mut unresolved_memory,
                &mut edge_batch,
            )?;
            edge_resolution_pressure_events =
                edge_resolution_pressure_events.saturating_add(unresolved_policy.pressure_events());

            Ok::<(), GraphError>(())
        })?;

        if !edge.bidirectional {
            engine.mark_has_unidirectional_edges();
        }
    }
    edge_batch.flush()?;
    let edge_batch_pressure_events = edge_batch.pressure_events();

    // Phase 3: Build CSR by streaming sorted temp-spooled edges.
    let node_count = engine.node_store.node_count();
    let (edge_store, relationship_identities) =
        load_edge_store_from_spool(node_count, has_weights, &mut persistent_memory)?;
    if edge_store.relationship_ids_slice().len() != edge_store.edge_count() as usize {
        return Err(GraphError::Internal(
            "relationship identity sidecar does not match CSR edge count".to_string(),
        ));
    }
    engine.replace_edge_stores(edge_store)?;
    engine.relationship_identities =
        crate::relationship_identity_store::RelationshipIdentityStore::try_from_owned(
            relationship_identities,
        )?;

    // Mark as built
    engine.finish_build(Some(pgrx::datetime::transaction_timestamp()));
    let elapsed = start.elapsed();
    let pressure_events = node_batch_pressure_events
        .saturating_add(edge_batch_pressure_events)
        .saturating_add(edge_resolution_pressure_events);
    engine.record_build_resource_stats(
        governor.memory_limit(),
        governor.memory_peak(),
        governor.memory_peak_phase(),
        pressure_events,
    );
    pgrx::log!(
        "graph.build() completed: {} nodes, {} edges, {:.1}ms, {} byte-pressure flushes",
        engine.node_store.node_count(),
        engine.edge_store.edge_count(),
        elapsed.as_secs_f64() * 1000.0,
        pressure_events
    );

    Ok(engine)
}

fn batch_row_bytes<T>(text: &str) -> GraphResult<ByteCount> {
    let fixed = std::mem::size_of::<T>()
        .checked_add(std::mem::size_of::<String>())
        .and_then(|bytes| bytes.checked_add(text.len()))
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("build batch row size overflowed".to_string()))?;
    Ok(fixed)
}

fn node_memory_bytes(primary_key: &str) -> GraphResult<ByteCount> {
    const CONSERVATIVE_NODE_BYTES: usize = 140;
    CONSERVATIVE_NODE_BYTES
        .checked_add(primary_key.len())
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("node memory accounting overflowed".to_string()))
}

fn pending_filter_memory_bytes(
    column_name: &str,
    value: &Option<PendingFilterValue>,
) -> GraphResult<ByteCount> {
    let text_bytes = match value {
        Some(PendingFilterValue::Text(text)) => text.len(),
        Some(PendingFilterValue::Encoded(_)) | None => 0,
    };
    std::mem::size_of::<((u32, String), u32, Option<PendingFilterValue>)>()
        .checked_add(column_name.len())
        .and_then(|bytes| bytes.checked_add(text_bytes))
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("filter memory accounting overflowed".to_string()))
}

fn allocation_error(_area: &str, _error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
    }
}

fn batch_accounting_error(error: crate::resource::ResourceLimitError) -> GraphError {
    GraphError::Internal(format!("build batch accounting failed: {error}"))
}

fn resource_error_to_graph(error: crate::resource::ResourceLimitError) -> GraphError {
    GraphError::Oom {
        used_mb: ByteCount::from_bytes(error.used()).ceil_mib(),
        need_mb: ByteCount::from_bytes(error.requested()).ceil_mib(),
        limit_mb: ByteCount::from_bytes(error.limit()).as_u64() / 1_048_576,
    }
}

fn push_unresolved_edge(
    from_oid: Option<u32>,
    to_oid: Option<u32>,
    edge: UnresolvedEdge,
    pending: &mut Vec<UnresolvedEdge>,
    policy: &mut AdaptiveBatch,
    memory: &mut crate::resource::ResourceLease<'_>,
    edge_batch: &mut EdgeSpoolBatch<'_>,
) -> GraphResult<()> {
    let row_bytes = unresolved_edge_memory_bytes(&edge)?;
    if policy.before_push(row_bytes) == BatchAction::Flush {
        flush_unresolved_edges(from_oid, to_oid, pending, policy, memory, edge_batch)?;
    }
    memory
        .try_grow_in(ResourcePhase::EdgeResolve, row_bytes)
        .map_err(resource_error_to_graph)?;
    pending
        .try_reserve(1)
        .map_err(|error| allocation_error("unresolved edge batch", error))?;
    pending.push(edge);
    policy.record(row_bytes).map_err(batch_accounting_error)?;
    if policy.is_full() {
        flush_unresolved_edges(from_oid, to_oid, pending, policy, memory, edge_batch)?;
    }
    Ok(())
}

fn flush_unresolved_edges(
    from_oid: Option<u32>,
    to_oid: Option<u32>,
    pending: &mut Vec<UnresolvedEdge>,
    policy: &mut AdaptiveBatch,
    memory: &mut crate::resource::ResourceLease<'_>,
    edge_batch: &mut EdgeSpoolBatch<'_>,
) -> GraphResult<()> {
    if pending.is_empty() {
        return Ok(());
    }
    memory
        .try_grow_in(
            ResourcePhase::EdgeResolve,
            edge_resolution_workspace_bytes(pending)?,
        )
        .map_err(resource_error_to_graph)?;
    let result = resolve_edge_batch(from_oid, to_oid, pending, edge_batch);
    pending.clear();
    pending.shrink_to(0);
    policy.clear();
    memory.release_all();
    result
}

fn edge_resolution_workspace_bytes(edges: &[UnresolvedEdge]) -> GraphResult<ByteCount> {
    edges
        .iter()
        .try_fold(0usize, |bytes, edge| {
            bytes
                .checked_add(edge.from_pk.len())
                .and_then(|value| value.checked_add(edge.to_pk.len()))
                .and_then(|value| value.checked_add(std::mem::size_of::<String>() * 2))
                .and_then(|value| {
                    value.checked_add(std::mem::size_of::<(Option<u32>, Option<u32>)>())
                })
                .ok_or(())
        })
        .ok()
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| {
            GraphError::Internal("edge resolution workspace accounting overflowed".to_string())
        })
}

fn unresolved_edge_memory_bytes(edge: &UnresolvedEdge) -> GraphResult<ByteCount> {
    std::mem::size_of::<UnresolvedEdge>()
        .checked_add(edge.from_pk.len())
        .and_then(|bytes| bytes.checked_add(edge.to_pk.len()))
        .and_then(|bytes| bytes.checked_add(edge.source_key.len()))
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| {
            GraphError::Internal("unresolved edge memory accounting overflowed".to_string())
        })
}

fn resolve_edge_batch(
    from_oid: Option<u32>,
    to_oid: Option<u32>,
    inputs: &[UnresolvedEdge],
    edge_batch: &mut EdgeSpoolBatch<'_>,
) -> GraphResult<()> {
    if inputs.is_empty() {
        return Ok(());
    }

    let mut from_keys = Vec::new();
    from_keys
        .try_reserve(inputs.len())
        .map_err(|error| allocation_error("edge source lookup keys", error))?;
    let mut to_keys = Vec::new();
    to_keys
        .try_reserve(inputs.len())
        .map_err(|error| allocation_error("edge target lookup keys", error))?;
    for edge in inputs {
        from_keys.push(edge.from_pk.clone());
        to_keys.push(edge.to_pk.clone());
    }
    let preferred_from = from_oid.map(i64::from).unwrap_or(-1);
    let preferred_to = to_oid.map(i64::from).unwrap_or(-1);
    let mut resolved = Vec::new();
    resolved
        .try_reserve(inputs.len())
        .map_err(|error| allocation_error("resolved edge coordinates", error))?;
    resolved.resize(inputs.len(), (None, None));

    Spi::connect(|client| {
        let rows = client
            .select(
                "WITH input AS (
                    SELECT ord::bigint, from_pk, to_pk
                    FROM unnest($1::text[], $2::text[]) WITH ORDINALITY
                      AS edge(from_pk, to_pk, ord)
                 )
                 SELECT input.ord,
                        COALESCE(preferred_from.node_idx, fallback_from.node_idx),
                        COALESCE(preferred_to.node_idx, fallback_to.node_idx)
                 FROM input
                 LEFT JOIN pg_temp.graph_build_nodes preferred_from
                   ON $3::int8 >= 0
                  AND preferred_from.table_oid = $3::int8
                  AND preferred_from.primary_key = input.from_pk
                 LEFT JOIN LATERAL (
                    SELECT node_idx
                    FROM pg_temp.graph_build_nodes fallback
                    WHERE fallback.primary_key = input.from_pk
                    ORDER BY fallback.table_oid
                    LIMIT 1
                 ) fallback_from ON preferred_from.node_idx IS NULL
                 LEFT JOIN pg_temp.graph_build_nodes preferred_to
                   ON $4::int8 >= 0
                  AND preferred_to.table_oid = $4::int8
                  AND preferred_to.primary_key = input.to_pk
                 LEFT JOIN LATERAL (
                    SELECT node_idx
                    FROM pg_temp.graph_build_nodes fallback
                    WHERE fallback.primary_key = input.to_pk
                    ORDER BY fallback.table_oid
                    LIMIT 1
                 ) fallback_to ON preferred_to.node_idx IS NULL
                 ORDER BY input.ord",
                None,
                &[
                    from_keys.into(),
                    to_keys.into(),
                    preferred_from.into(),
                    preferred_to.into(),
                ],
            )
            .map_err(|err| GraphError::Internal(format!("edge endpoint lookup failed: {}", err)))?;

        for row in rows {
            let ord = row
                .get::<i64>(1)
                .map_err(|err| GraphError::Internal(format!("edge ord read failed: {}", err)))?
                .ok_or_else(|| GraphError::Internal("edge ord was NULL".to_string()))?;
            let source = row
                .get::<i64>(2)
                .map_err(|err| GraphError::Internal(format!("edge source lookup failed: {}", err)))?
                .map(|value| {
                    u32::try_from(value).map_err(|_| {
                        GraphError::Internal(format!("edge source out of range: {}", value))
                    })
                })
                .transpose()?;
            let target = row
                .get::<i64>(3)
                .map_err(|err| GraphError::Internal(format!("edge target lookup failed: {}", err)))?
                .map(|value| {
                    u32::try_from(value).map_err(|_| {
                        GraphError::Internal(format!("edge target out of range: {}", value))
                    })
                })
                .transpose()?;
            let idx = usize::try_from(ord - 1)
                .map_err(|_| GraphError::Internal(format!("edge ord out of range: {}", ord)))?;
            if let Some(slot) = resolved.get_mut(idx) {
                *slot = (source, target);
            }
        }

        Ok::<(), GraphError>(())
    })?;

    for (edge, (source, target)) in inputs.iter().zip(resolved) {
        if let (Some(source), Some(target)) = (source, target) {
            edge_batch.push(
                RawEdge {
                    source,
                    target,
                    type_id: edge.type_id,
                    weight: edge.weight,
                    schema_reversed: false,
                },
                edge.mapping_id,
                &edge.source_key,
            )?;
            edge_batch.flush_if_full()?;

            if edge.bidirectional {
                edge_batch.push(
                    RawEdge {
                        source: target,
                        target: source,
                        type_id: edge.type_id,
                        weight: edge.weight,
                        schema_reversed: true,
                    },
                    edge.mapping_id,
                    &edge.source_key,
                )?;
                edge_batch.flush_if_full()?;
            }
        }
    }

    Ok(())
}

struct EdgeSpoolBatch<'a> {
    sources: Vec<i64>,
    targets: Vec<i64>,
    type_ids: Vec<i64>,
    weights: Vec<i64>,
    schema_reversed: Vec<bool>,
    mapping_ids: Vec<i64>,
    source_keys: Vec<String>,
    policy: AdaptiveBatch,
    memory: crate::resource::ResourceLease<'a>,
}

impl<'a> EdgeSpoolBatch<'a> {
    fn with_limits(
        governor: &'a crate::resource::ResourceGovernor,
        row_limit: i32,
        byte_limit: ByteCount,
    ) -> GraphResult<Self> {
        Ok(Self {
            sources: Vec::new(),
            targets: Vec::new(),
            type_ids: Vec::new(),
            weights: Vec::new(),
            schema_reversed: Vec::new(),
            mapping_ids: Vec::new(),
            source_keys: Vec::new(),
            policy: AdaptiveBatch::new(
                ResourcePhase::EdgeBatch,
                row_limit.max(1) as usize,
                byte_limit,
            ),
            memory: governor
                .reserve_memory(ResourcePhase::EdgeBatch, ByteCount::ZERO)
                .map_err(resource_error_to_graph)?,
        })
    }

    fn push(&mut self, edge: RawEdge, mapping_id: u64, source_key: &str) -> GraphResult<()> {
        let row_bytes = batch_row_bytes::<(i64, i64, i64, i64, bool, i64)>(source_key)?;
        if self.policy.before_push(row_bytes) == BatchAction::Flush {
            self.flush()?;
        }
        self.memory
            .try_grow_in(ResourcePhase::EdgeBatch, row_bytes)
            .map_err(resource_error_to_graph)?;
        self.try_reserve_row()?;
        self.sources.push(i64::from(edge.source));
        self.targets.push(i64::from(edge.target));
        self.type_ids.push(i64::from(edge.type_id));
        self.weights.push(i64::from(edge.weight.unwrap_or(0)));
        self.schema_reversed.push(edge.schema_reversed);
        self.mapping_ids
            .push(i64::try_from(mapping_id).map_err(|_| {
                GraphError::Internal(format!(
                    "relationship mapping ID {mapping_id} is out of range"
                ))
            })?);
        self.source_keys.push(source_key.to_string());
        self.policy
            .record(row_bytes)
            .map_err(batch_accounting_error)?;
        Ok(())
    }

    fn flush_if_full(&mut self) -> GraphResult<()> {
        if self.policy.is_full() {
            self.flush()?;
        }
        Ok(())
    }

    fn try_reserve_row(&mut self) -> GraphResult<()> {
        self.sources
            .try_reserve(1)
            .map_err(|error| allocation_error("edge spool sources", error))?;
        self.targets
            .try_reserve(1)
            .map_err(|error| allocation_error("edge spool targets", error))?;
        self.type_ids
            .try_reserve(1)
            .map_err(|error| allocation_error("edge spool type IDs", error))?;
        self.weights
            .try_reserve(1)
            .map_err(|error| allocation_error("edge spool weights", error))?;
        self.schema_reversed
            .try_reserve(1)
            .map_err(|error| allocation_error("edge spool directions", error))?;
        self.mapping_ids
            .try_reserve(1)
            .map_err(|error| allocation_error("edge spool mapping IDs", error))?;
        self.source_keys
            .try_reserve(1)
            .map_err(|error| allocation_error("edge spool source keys", error))?;
        Ok(())
    }

    fn flush(&mut self) -> GraphResult<()> {
        if self.sources.is_empty() {
            return Ok(());
        }

        let sources = std::mem::take(&mut self.sources);
        let targets = std::mem::take(&mut self.targets);
        let type_ids = std::mem::take(&mut self.type_ids);
        let weights = std::mem::take(&mut self.weights);
        let schema_reversed = std::mem::take(&mut self.schema_reversed);
        let mapping_ids = std::mem::take(&mut self.mapping_ids);
        let source_keys = std::mem::take(&mut self.source_keys);
        self.policy.clear();

        let result = Spi::run_with_args(
            "INSERT INTO pg_temp.graph_build_edges (source, target, type_id, weight, schema_reversed, mapping_id, source_key)
             SELECT source, target, type_id, NULLIF(weight, 0), schema_reversed, mapping_id, source_key
             FROM unnest($1::int8[], $2::int8[], $3::int8[], $4::int8[], $5::bool[], $6::int8[], $7::text[])
               AS edge(source, target, type_id, weight, schema_reversed, mapping_id, source_key)",
            &[
                sources.into(),
                targets.into(),
                type_ids.into(),
                weights.into(),
                schema_reversed.into(),
                mapping_ids.into(),
                source_keys.into(),
            ],
        )
        .map_err(|err| GraphError::Internal(format!("edge spool batch insert failed: {}", err)));
        self.memory.release_all();
        result
    }

    fn pressure_events(&self) -> u64 {
        self.policy.pressure_events()
    }
}

fn create_edge_spool() -> GraphResult<()> {
    Spi::run(
        "DROP TABLE IF EXISTS pg_temp.graph_build_edges;
         CREATE TEMP TABLE graph_build_edges (
            source bigint NOT NULL,
            target bigint NOT NULL,
            type_id bigint NOT NULL,
            weight bigint,
            schema_reversed boolean NOT NULL,
            mapping_id bigint NOT NULL,
            source_key text NOT NULL
         ) ON COMMIT DROP",
    )
    .map_err(|err| GraphError::Internal(format!("edge spool setup failed: {}", err)))
}

fn load_edge_store_from_spool(
    node_count: u32,
    has_weights: bool,
    persistent_memory: &mut crate::resource::ResourceLease<'_>,
) -> GraphResult<(
    crate::edge_store::EdgeStore,
    Vec<Option<RelationshipIdentity>>,
)> {
    Spi::connect(|client| {
        let mut cursor = client.open_cursor(
            "SELECT source, target, type_id, weight, schema_reversed, mapping_id, source_key
             FROM pg_temp.graph_build_edges
             ORDER BY source, target, type_id, schema_reversed, mapping_id, source_key",
            &[],
        );
        let batch_size = crate::config::BUILD_BATCH_SIZE.get().max(1) as i64;
        let offset_count = u64::from(node_count)
            .checked_add(1)
            .and_then(|count| count.checked_mul(8))
            .map(ByteCount::from_bytes)
            .ok_or_else(|| GraphError::Internal("CSR offset accounting overflowed".to_string()))?;
        persistent_memory
            .try_grow_in(ResourcePhase::Csr, offset_count)
            .map_err(resource_error_to_graph)?;
        let mut builder = SortedEdgeStoreBuilder::try_new(node_count, has_weights)?;
        let mut relationship_ids =
            std::collections::HashMap::<RelationshipIdentity, RelationshipId>::new();
        let mut relationship_identities = vec![None];

        loop {
            let rows = cursor
                .fetch(batch_size)
                .map_err(|err| GraphError::Internal(format!("edge spool fetch failed: {}", err)))?;
            if rows.is_empty() {
                break;
            }

            for row in rows {
                let source = row
                    .get::<i64>(1)
                    .map_err(|err| {
                        GraphError::Internal(format!("edge source read failed: {}", err))
                    })?
                    .ok_or_else(|| GraphError::Internal("edge source was NULL".to_string()))?;
                let target = row
                    .get::<i64>(2)
                    .map_err(|err| {
                        GraphError::Internal(format!("edge target read failed: {}", err))
                    })?
                    .ok_or_else(|| GraphError::Internal("edge target was NULL".to_string()))?;
                let type_id = row
                    .get::<i64>(3)
                    .map_err(|err| GraphError::Internal(format!("edge type read failed: {}", err)))?
                    .ok_or_else(|| GraphError::Internal("edge type was NULL".to_string()))?;
                let weight = row.get::<i64>(4).map_err(|err| {
                    GraphError::Internal(format!("edge weight read failed: {}", err))
                })?;
                let schema_reversed = row
                    .get::<bool>(5)
                    .map_err(|err| {
                        GraphError::Internal(format!("edge schema direction read failed: {}", err))
                    })?
                    .ok_or_else(|| {
                        GraphError::Internal("edge schema direction was NULL".to_string())
                    })?;

                let mapping_id = row
                    .get::<i64>(6)
                    .map_err(|err| {
                        GraphError::Internal(format!("edge mapping ID read failed: {err}"))
                    })?
                    .ok_or_else(|| GraphError::Internal("edge mapping ID was NULL".to_string()))?;
                let mapping_id = u64::try_from(mapping_id).map_err(|_| {
                    GraphError::Internal("edge mapping ID was negative".to_string())
                })?;
                let source_key = row
                    .get::<String>(7)
                    .map_err(|err| {
                        GraphError::Internal(format!("edge source key read failed: {err}"))
                    })?
                    .ok_or_else(|| GraphError::Internal("edge source key was NULL".to_string()))?;
                persistent_memory
                    .try_grow_in(ResourcePhase::Csr, csr_edge_memory_bytes(has_weights)?)
                    .map_err(resource_error_to_graph)?;
                let identity = RelationshipIdentity {
                    mapping_id,
                    source_key,
                };
                let relationship_id = if let Some(&id) = relationship_ids.get(&identity) {
                    id
                } else {
                    let identity_bytes = relationship_identity_memory_bytes(&identity.source_key)?;
                    persistent_memory
                        .try_grow_in(ResourcePhase::Csr, identity_bytes)
                        .map_err(resource_error_to_graph)?;
                    relationship_ids
                        .try_reserve(1)
                        .map_err(|error| allocation_error("relationship identity map", error))?;
                    relationship_identities
                        .try_reserve(1)
                        .map_err(|error| allocation_error("relationship identity list", error))?;
                    let id =
                        RelationshipId::try_from(relationship_identities.len()).map_err(|_| {
                            GraphError::Internal("too many relationship identities".to_string())
                        })?;
                    relationship_ids.insert(identity.clone(), id);
                    relationship_identities.push(Some(identity));
                    id
                };

                builder.try_push_identified(IdentifiedRawEdge {
                    edge: RawEdge {
                        source: u32::try_from(source).map_err(|_| {
                            GraphError::Internal(format!("edge source out of range: {}", source))
                        })?,
                        target: u32::try_from(target).map_err(|_| {
                            GraphError::Internal(format!("edge target out of range: {}", target))
                        })?,
                        type_id: u8::try_from(type_id).map_err(|_| {
                            GraphError::Internal(format!("edge type out of range: {}", type_id))
                        })?,
                        weight: weight
                            .map(|value| {
                                u32::try_from(value).map_err(|_| {
                                    GraphError::Internal(format!(
                                        "edge weight out of range: {}",
                                        value
                                    ))
                                })
                            })
                            .transpose()?,
                        schema_reversed,
                    },
                    relationship_id,
                })?;
            }
        }

        Ok((builder.finish(), relationship_identities))
    })
}

fn csr_edge_memory_bytes(has_weights: bool) -> GraphResult<ByteCount> {
    let one_direction = std::mem::size_of::<u32>()
        .checked_add(std::mem::size_of::<u8>())
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<u8>()))
        .and_then(|bytes| {
            bytes.checked_add(if has_weights {
                std::mem::size_of::<u32>()
            } else {
                0
            })
        })
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<RelationshipId>()))
        .and_then(|bytes| bytes.checked_mul(2))
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| GraphError::Internal("CSR edge accounting overflowed".to_string()))?;
    Ok(one_direction)
}

fn relationship_identity_memory_bytes(source_key: &str) -> GraphResult<ByteCount> {
    std::mem::size_of::<RelationshipIdentity>()
        .checked_add(std::mem::size_of::<RelationshipId>())
        .and_then(|bytes| bytes.checked_add(source_key.len()))
        .and_then(|bytes| bytes.checked_add(64))
        .and_then(ByteCount::from_usize)
        .ok_or_else(|| {
            GraphError::Internal("relationship identity accounting overflowed".to_string())
        })
}

fn register_filter_columns(
    engine: &mut Engine,
    filter_columns: &[RegisteredFilterColumn],
    populated_counts: &HashMap<(u32, String), usize>,
) {
    let node_count = engine.node_store.node_count() as usize;
    for filter in filter_columns {
        let table_oid = filter.table_oid;
        if engine
            .filter_index
            .find_column_for_table(table_oid, &filter.column_name)
            .is_none()
        {
            let column_type =
                FilterColumnType::parse(&filter.column_type).unwrap_or(FilterColumnType::Numeric);
            let populated_count = populated_counts
                .get(&(table_oid, filter.column_name.clone()))
                .copied()
                .unwrap_or(0);
            engine
                .filter_index
                .register_typed_column_with_populated_count(
                    table_oid,
                    filter.column_name.clone(),
                    column_type,
                    node_count,
                    populated_count,
                );
        }
    }
}

fn filter_column_select_expr(filter: &RegisteredFilterColumn) -> String {
    let column = quote_ident(&filter.column_name);
    match filter.column_type.to_ascii_lowercase().as_str() {
        "numeric" => format!("({})::bigint", column),
        "boolean" => format!("({})::boolean", column),
        "text" => format!("({})::text", column),
        "date" => format!("(({})::date - DATE '2000-01-01')::bigint", column),
        "timestamptz" => format!(
            "(EXTRACT(EPOCH FROM ({})::timestamptz) * 1000000)::bigint",
            column
        ),
        "uuid" => format!("({})::text", column),
        _ => format!("({})::bigint", column),
    }
}

fn read_encoded_filter_value(
    row: &pgrx::spi::SpiHeapTupleData<'_>,
    column_idx: usize,
    filter: &RegisteredFilterColumn,
) -> GraphResult<Option<PendingFilterValue>> {
    let column_type = FilterColumnType::parse(&filter.column_type)
        .map_err(|reason| GraphError::InvalidFilter { reason })?;
    match column_type {
        FilterColumnType::Numeric => Ok(row
            .get::<i64>(column_idx)
            .map_err(|err| GraphError::Internal(format!("filter value read failed: {}", err)))?
            .map(|value| PendingFilterValue::Encoded(EncodedFilterValue::Numeric(value)))),
        FilterColumnType::Boolean => Ok(row
            .get::<bool>(column_idx)
            .map_err(|err| GraphError::Internal(format!("filter value read failed: {}", err)))?
            .map(|value| PendingFilterValue::Encoded(EncodedFilterValue::Boolean(value)))),
        FilterColumnType::Text => Ok(row
            .get::<String>(column_idx)
            .map_err(|err| GraphError::Internal(format!("filter value read failed: {}", err)))?
            .map(PendingFilterValue::Text)),
        FilterColumnType::Date => Ok(row
            .get::<i64>(column_idx)
            .map_err(|err| GraphError::Internal(format!("filter value read failed: {}", err)))?
            .map(|value| PendingFilterValue::Encoded(EncodedFilterValue::Date(value)))),
        FilterColumnType::Timestamptz => Ok(row
            .get::<i64>(column_idx)
            .map_err(|err| GraphError::Internal(format!("filter value read failed: {}", err)))?
            .map(|value| PendingFilterValue::Encoded(EncodedFilterValue::Timestamptz(value)))),
        FilterColumnType::Uuid => Ok(row
            .get::<String>(column_idx)
            .map_err(|err| GraphError::Internal(format!("filter value read failed: {}", err)))?
            .map(|value| parse_uuid_u128(&value).map(EncodedFilterValue::Uuid))
            .transpose()?
            .map(PendingFilterValue::Encoded)),
    }
}

fn parse_uuid_u128(value: &str) -> GraphResult<u128> {
    let compact = value.chars().filter(|ch| *ch != '-').collect::<String>();
    if compact.len() != 32 || !compact.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(GraphError::InvalidFilter {
            reason: format!("invalid uuid filter value '{}'", value),
        });
    }
    u128::from_str_radix(&compact, 16).map_err(|err| GraphError::InvalidFilter {
        reason: format!("invalid uuid filter value '{}': {}", value, err),
    })
}

fn primary_key_expr(primary_key: &PrimaryKeySpec) -> String {
    if primary_key.columns().len() > 1 {
        primary_key.select_expr()
    } else if let Some(column) = primary_key.columns().first() {
        quote_ident(column)
    } else {
        "NULL".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        estimate_graph_memory_with_counts, structural_text_value, PrimaryKeySpec, PropertyColumns,
        RegisteredEdge, RegisteredTable,
    };
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[test]
    fn structural_text_value_preserves_empty_string_but_skips_null() {
        assert_eq!(
            structural_text_value(Some(String::new())),
            Some(String::new())
        );
        assert_eq!(
            structural_text_value(Some("node-1".to_string())),
            Some("node-1".to_string())
        );
        assert_eq!(structural_text_value(None), None);
    }

    #[test]
    fn memory_estimate_reuses_table_counts_across_nodes_and_edges() {
        let tables = vec![RegisteredTable {
            table_oid: 42,
            table_name: "public.accounts".to_string(),
            id_columns: PrimaryKeySpec::from_columns(vec!["id".to_string()]),
            columns: PropertyColumns::from_columns(Vec::new()),
            tenant_column: None,
        }];
        let edges = vec![
            RegisteredEdge {
                mapping_id: 1,
                from_table_oid: 42,
                from_table: "public.accounts".to_string(),
                from_column: "parent_id".to_string(),
                source_key_columns: PrimaryKeySpec::from_columns(vec!["id".to_string()]),
                to_table_oid: 42,
                to_table: "public.accounts".to_string(),
                to_column: "id".to_string(),
                label: "parent".to_string(),
                bidirectional: false,
                weight_column: None,
                label_column: None,
            },
            RegisteredEdge {
                mapping_id: 2,
                from_table_oid: 42,
                from_table: "public.accounts".to_string(),
                from_column: "owner_id".to_string(),
                source_key_columns: PrimaryKeySpec::from_columns(vec!["id".to_string()]),
                to_table_oid: 42,
                to_table: "public.accounts".to_string(),
                to_column: "id".to_string(),
                label: "owner".to_string(),
                bidirectional: true,
                weight_column: None,
                label_column: None,
            },
        ];
        let calls = RefCell::new(HashMap::new());

        let estimate = estimate_graph_memory_with_counts(&tables, &edges, |table| {
            *calls
                .borrow_mut()
                .entry(table.to_string())
                .or_insert(0usize) += 1;
            Ok(10)
        })
        .expect("estimate should succeed");

        assert_eq!(calls.borrow().get("public.accounts"), Some(&1));
        let expected_bytes = 10 * 140 + 30 * 5;
        assert_eq!(estimate.bytes.as_u64(), expected_bytes);
    }

    #[test]
    fn memory_estimate_rejects_negative_and_overflowing_counts() {
        let tables = vec![RegisteredTable {
            table_oid: 42,
            table_name: "public.accounts".to_string(),
            id_columns: PrimaryKeySpec::from_columns(vec!["id".to_string()]),
            columns: PropertyColumns::from_columns(Vec::new()),
            tenant_column: None,
        }];

        let negative = estimate_graph_memory_with_counts(&tables, &[], |_| Ok(-1))
            .expect_err("negative estimates must fail closed");
        assert!(negative.to_string().contains("negative table row estimate"));

        let overflowing = estimate_graph_memory_with_counts(&tables, &[], |_| Ok(i64::MAX))
            .expect_err("byte multiplication overflow must fail closed");
        assert!(overflowing
            .to_string()
            .contains("memory estimate overflowed"));
    }
}
