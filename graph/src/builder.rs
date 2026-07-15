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

use crate::catalog::{estimated_table_rows, sql_table_name_from_oid};
use crate::config::BuildScanMode;
use crate::edge_store::{
    IdentifiedRawEdge, RawEdge, RelationshipId, RelationshipIdentity, SortedEdgeStoreBuilder,
};
use crate::engine::Engine;
use crate::filter_index::{EncodedFilterValue, FilterColumnType};
use crate::quote::quote_ident;
use crate::resource::ByteCount;
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
pub fn build_graph(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    filter_columns: &[RegisteredFilterColumn],
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
    create_node_lookup_spool()?;
    let mut node_lookup_batch =
        NodeLookupBatch::with_capacity(crate::config::BUILD_BATCH_SIZE.get());

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

                    let node_idx = engine.node_store.add_node(oid, pk.clone());
                    engine.insert_table_membership(oid, node_idx);
                    node_lookup_batch.push(oid, pk.clone(), node_idx);
                    node_lookup_batch.flush_if_full()?;

                    // Index in ResolutionIndex
                    engine.resolution_insert(oid, &pk, node_idx);

                    for (filter_idx, filter_col) in table_filter_columns.iter().enumerate() {
                        let value = read_encoded_filter_value(
                            &row,
                            filter_start_column + filter_idx,
                            filter_col,
                        )?;
                        if value.is_some() {
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
                            engine.insert_tenant_membership(&tenant, node_idx);
                        }
                    }
                }
            }

            Ok::<(), GraphError>(())
        })?;
    }
    node_lookup_batch.flush()?;
    index_node_lookup_spool()?;

    register_filter_columns(&mut engine, filter_columns, &filter_populated_counts);
    for ((table_oid, column_name), node_idx, value) in pending_filter_values {
        if let Some(global_filter_idx) = engine
            .filter_index
            .find_column_for_table(table_oid, &column_name)
        {
            let value = value.map(|value| match value {
                PendingFilterValue::Encoded(value) => value,
                PendingFilterValue::Text(value) => {
                    let token = engine
                        .filter_index
                        .intern_text_value(global_filter_idx, &value);
                    EncodedFilterValue::Text(token)
                }
            });
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
    let mut edge_batch = EdgeSpoolBatch::with_capacity(crate::config::BUILD_BATCH_SIZE.get());

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
            loop {
                let table_result = cursor
                    .fetch(batch_size)
                    .map_err(|e| GraphError::Internal(format!("SPI fetch failed: {}", e)))?;

                if table_result.is_empty() {
                    break;
                }

                let mut unresolved_edges = Vec::with_capacity(table_result.len());
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

                    unresolved_edges.push(UnresolvedEdge {
                        from_pk,
                        to_pk,
                        mapping_id: edge.mapping_id,
                        source_key,
                        type_id: edge_type_id,
                        weight,
                        bidirectional: edge.bidirectional,
                    });
                }
                resolve_edge_batch(from_oid, to_oid, &unresolved_edges, &mut edge_batch)?;
            }

            Ok::<(), GraphError>(())
        })?;

        if !edge.bidirectional {
            engine.mark_has_unidirectional_edges();
        }
    }
    edge_batch.flush()?;

    // Phase 3: Build CSR by streaming sorted temp-spooled edges.
    let node_count = engine.node_store.node_count();
    let (edge_store, relationship_identities) =
        load_edge_store_from_spool(node_count, has_weights)?;
    if edge_store.relationship_ids_slice().len() != edge_store.edge_count() as usize {
        return Err(GraphError::Internal(
            "relationship identity sidecar does not match CSR edge count".to_string(),
        ));
    }
    engine.replace_edge_stores(edge_store);
    engine.relationship_identities = relationship_identities;

    // Mark as built
    engine.finish_build(Some(pgrx::datetime::transaction_timestamp()));
    let elapsed = start.elapsed();
    pgrx::log!(
        "graph.build() completed: {} nodes, {} edges, {:.1}ms",
        engine.node_store.node_count(),
        engine.edge_store.edge_count(),
        elapsed.as_secs_f64() * 1000.0
    );

    Ok(engine)
}

struct NodeLookupBatch {
    table_oids: Vec<i64>,
    primary_keys: Vec<String>,
    node_indices: Vec<i64>,
    capacity: usize,
}

impl NodeLookupBatch {
    fn with_capacity(capacity: i32) -> Self {
        let capacity = capacity.max(1) as usize;
        Self {
            table_oids: Vec::with_capacity(capacity),
            primary_keys: Vec::with_capacity(capacity),
            node_indices: Vec::with_capacity(capacity),
            capacity,
        }
    }

    fn push(&mut self, table_oid: u32, primary_key: String, node_idx: u32) {
        self.table_oids.push(i64::from(table_oid));
        self.primary_keys.push(primary_key);
        self.node_indices.push(i64::from(node_idx));
    }

    fn flush_if_full(&mut self) -> GraphResult<()> {
        if self.table_oids.len() >= self.capacity {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> GraphResult<()> {
        if self.table_oids.is_empty() {
            return Ok(());
        }

        let table_oids = std::mem::take(&mut self.table_oids);
        let primary_keys = std::mem::take(&mut self.primary_keys);
        let node_indices = std::mem::take(&mut self.node_indices);
        self.table_oids = Vec::with_capacity(self.capacity);
        self.primary_keys = Vec::with_capacity(self.capacity);
        self.node_indices = Vec::with_capacity(self.capacity);

        Spi::run_with_args(
            "INSERT INTO pg_temp.graph_build_nodes (table_oid, primary_key, node_idx)
             SELECT table_oid, primary_key, node_idx
             FROM unnest($1::int8[], $2::text[], $3::int8[])
               AS node(table_oid, primary_key, node_idx)",
            &[table_oids.into(), primary_keys.into(), node_indices.into()],
        )
        .map_err(|err| GraphError::Internal(format!("node lookup batch insert failed: {}", err)))
    }
}

fn create_node_lookup_spool() -> GraphResult<()> {
    Spi::run(
        "DROP TABLE IF EXISTS pg_temp.graph_build_nodes;
         CREATE TEMP TABLE graph_build_nodes (
            table_oid bigint NOT NULL,
            primary_key text NOT NULL,
            node_idx bigint NOT NULL
         ) ON COMMIT DROP",
    )
    .map_err(|err| GraphError::Internal(format!("node lookup spool setup failed: {}", err)))
}

fn index_node_lookup_spool() -> GraphResult<()> {
    Spi::run(
        "CREATE INDEX graph_build_nodes_table_pk_idx
           ON pg_temp.graph_build_nodes (table_oid, primary_key);
         CREATE INDEX graph_build_nodes_pk_idx
           ON pg_temp.graph_build_nodes (primary_key, table_oid);
         ANALYZE pg_temp.graph_build_nodes",
    )
    .map_err(|err| GraphError::Internal(format!("node lookup spool index failed: {}", err)))
}

fn resolve_edge_batch(
    from_oid: Option<u32>,
    to_oid: Option<u32>,
    inputs: &[UnresolvedEdge],
    edge_batch: &mut EdgeSpoolBatch,
) -> GraphResult<()> {
    if inputs.is_empty() {
        return Ok(());
    }

    let from_keys = inputs
        .iter()
        .map(|edge| edge.from_pk.clone())
        .collect::<Vec<_>>();
    let to_keys = inputs
        .iter()
        .map(|edge| edge.to_pk.clone())
        .collect::<Vec<_>>();
    let preferred_from = from_oid.map(i64::from).unwrap_or(-1);
    let preferred_to = to_oid.map(i64::from).unwrap_or(-1);
    let mut resolved = vec![(None, None); inputs.len()];

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

struct EdgeSpoolBatch {
    sources: Vec<i64>,
    targets: Vec<i64>,
    type_ids: Vec<i64>,
    weights: Vec<i64>,
    schema_reversed: Vec<bool>,
    mapping_ids: Vec<i64>,
    source_keys: Vec<String>,
    capacity: usize,
}

impl EdgeSpoolBatch {
    fn with_capacity(capacity: i32) -> Self {
        let capacity = capacity.max(1) as usize;
        Self {
            sources: Vec::with_capacity(capacity),
            targets: Vec::with_capacity(capacity),
            type_ids: Vec::with_capacity(capacity),
            weights: Vec::with_capacity(capacity),
            schema_reversed: Vec::with_capacity(capacity),
            mapping_ids: Vec::with_capacity(capacity),
            source_keys: Vec::with_capacity(capacity),
            capacity,
        }
    }

    fn push(&mut self, edge: RawEdge, mapping_id: u64, source_key: &str) -> GraphResult<()> {
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
        Ok(())
    }

    fn flush_if_full(&mut self) -> GraphResult<()> {
        if self.sources.len() >= self.capacity {
            self.flush()?;
        }
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
        self.sources = Vec::with_capacity(self.capacity);
        self.targets = Vec::with_capacity(self.capacity);
        self.type_ids = Vec::with_capacity(self.capacity);
        self.weights = Vec::with_capacity(self.capacity);
        self.schema_reversed = Vec::with_capacity(self.capacity);
        self.mapping_ids = Vec::with_capacity(self.capacity);
        self.source_keys = Vec::with_capacity(self.capacity);

        Spi::run_with_args(
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
        .map_err(|err| GraphError::Internal(format!("edge spool batch insert failed: {}", err)))
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
        let mut builder = SortedEdgeStoreBuilder::new(node_count, has_weights);
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
                let identity = RelationshipIdentity {
                    mapping_id,
                    source_key,
                };
                let relationship_id = if let Some(&id) = relationship_ids.get(&identity) {
                    id
                } else {
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
