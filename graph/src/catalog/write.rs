use crate::{builder, safety};
use pgrx::prelude::*;

use super::selected_or_default_graph_metadata;

pub(crate) fn insert_registered_table(
    table_name: &str,
    id_columns: impl Into<builder::PrimaryKeySpec>,
    columns: impl Into<builder::PropertyColumns>,
    tenant_column: Option<&str>,
) -> safety::GraphResult<()> {
    let graph = selected_or_default_graph_metadata()?;
    insert_registered_table_for_graph(
        &graph.graph_id,
        table_name,
        id_columns,
        columns,
        tenant_column,
    )
}

pub(crate) fn insert_registered_table_for_graph(
    graph_id: &str,
    table_name: &str,
    id_columns: impl Into<builder::PrimaryKeySpec>,
    columns: impl Into<builder::PropertyColumns>,
    tenant_column: Option<&str>,
) -> safety::GraphResult<()> {
    let id_columns = id_columns.into();
    let columns = columns.into();
    let id_column = id_columns.as_catalog_text();
    let columns = columns.as_catalog_text();
    Spi::run_with_args(
        "WITH relation AS (
             SELECT pg_catalog.to_regclass($2)::oid AS oid
         ), updated AS (
             UPDATE graph._registered_tables AS registered
                SET table_name = $2,
                    id_column = $3,
                    columns = $4,
                    tenant_column = $5
               FROM relation
              WHERE registered.graph_id = $1::uuid
                AND registered.table_oid = relation.oid
          RETURNING 1
         )
         INSERT INTO graph._registered_tables (graph_id, table_name, table_oid, id_column, columns, tenant_column)
         SELECT $1::uuid, $2, relation.oid, $3, $4, $5
           FROM relation
          WHERE relation.oid IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM updated)
         ON CONFLICT (graph_id, table_name) DO UPDATE SET
           table_oid = EXCLUDED.table_oid,
           id_column = EXCLUDED.id_column,
           columns = EXCLUDED.columns,
           tenant_column = EXCLUDED.tenant_column",
        &[
            graph_id.into(),
            table_name.into(),
            id_column.into(),
            columns.into(),
            tenant_column.map(|value| value.to_string()).into(),
        ],
    )
    .map_err(|e| safety::GraphError::Internal(format!("registered table write failed: {}", e)))
}

pub(crate) struct RegisteredEdgeInsert<'a> {
    pub(crate) from_table: &'a str,
    pub(crate) from_column: &'a str,
    pub(crate) to_table: &'a str,
    pub(crate) to_column: &'a str,
    pub(crate) label: &'a str,
    pub(crate) bidirectional: bool,
    pub(crate) weight_column: Option<&'a str>,
    pub(crate) label_column: Option<&'a str>,
}

pub(crate) fn insert_registered_edge(edge: RegisteredEdgeInsert<'_>) -> safety::GraphResult<()> {
    let graph = selected_or_default_graph_metadata()?;
    insert_registered_edge_for_graph(&graph.graph_id, edge)
}

pub(crate) fn insert_registered_edge_for_graph(
    graph_id: &str,
    edge: RegisteredEdgeInsert<'_>,
) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "INSERT INTO graph._registered_edges
           (graph_id, from_table, from_table_oid, from_column, to_table, to_table_oid, to_column, label, bidirectional, weight_column, label_column)
         VALUES ($1::uuid, $2, pg_catalog.to_regclass($2)::oid, $3, $4, pg_catalog.to_regclass($4)::oid, $5, $6, $7, $8, $9)
         ON CONFLICT (graph_id, from_table, from_column, to_table, to_column, label)
         DO UPDATE SET
            bidirectional = EXCLUDED.bidirectional,
            weight_column = EXCLUDED.weight_column,
            label_column = EXCLUDED.label_column",
        &[
            graph_id.into(),
            edge.from_table.into(),
            edge.from_column.into(),
            edge.to_table.into(),
            edge.to_column.into(),
            edge.label.into(),
            edge.bidirectional.into(),
            edge.weight_column.map(|value| value.to_string()).into(),
            edge.label_column.map(|value| value.to_string()).into(),
        ],
    )
    .map_err(|e| safety::GraphError::Internal(format!("registered edge write failed: {}", e)))
}
