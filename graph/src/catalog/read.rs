use crate::{builder, safety};
use pgrx::prelude::*;

use super::selected_or_default_graph_id_via_definer;
use super::validate::registered_schema_drift_reason;

/// Read registered tables and edges for the selected or default graph.
pub(crate) fn read_catalog() -> safety::GraphResult<(
    Vec<builder::RegisteredTable>,
    Vec<builder::RegisteredEdge>,
    Vec<builder::RegisteredFilterColumn>,
)> {
    let graph_id = selected_or_default_graph_id_via_definer()?;
    read_catalog_for_graph(&graph_id)
}

/// Read registered tables and edges from one graph catalog via SPI.
pub(crate) fn read_catalog_for_graph(
    graph_id: &str,
) -> safety::GraphResult<(
    Vec<builder::RegisteredTable>,
    Vec<builder::RegisteredEdge>,
    Vec<builder::RegisteredFilterColumn>,
)> {
    let mut tables = Vec::new();
    let mut edges = Vec::new();
    let mut filter_columns = Vec::new();

    Spi::connect(|client| {
        let result = client
            .select(
                "SELECT registered.table_oid::integer,
                        pg_catalog.quote_ident(namespace.nspname) || '.' || pg_catalog.quote_ident(relation.relname),
                        registered.id_column,
                        registered.columns,
                        registered.tenant_column
                   FROM graph._registered_tables AS registered
                   LEFT JOIN pg_catalog.pg_class AS relation
                     ON relation.oid = registered.table_oid
                   LEFT JOIN pg_catalog.pg_namespace AS namespace
                     ON namespace.oid = relation.relnamespace
                  WHERE registered.graph_id = $1::uuid
                  ORDER BY registered.table_name",
                None,
                &[graph_id.into()],
            )
            .map_err(|e| {
                safety::GraphError::Internal(format!(
                    "catalog read failed for graph._registered_tables: {}",
                    e
                ))
            })?;
        for row in result {
            let table_oid = row
                .get::<i32>(1)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (table_oid): {e}"))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered table has no relation OID; re-register it".to_string(),
                    )
                })
                .and_then(|oid| {
                    u32::try_from(oid).map_err(|_| {
                        safety::GraphError::Internal(format!(
                            "registered table has invalid relation OID {oid}"
                        ))
                    })
                })?;
            let table_name = row
                .get::<String>(2)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (table_name): {}", e))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered table relation no longer exists; re-register it".to_string(),
                    )
                })?;
            let id_column = row
                .get::<String>(3)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (id_column): {}", e))
                })?
                .unwrap_or_default();
            let columns_str = row
                .get::<String>(4)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (columns): {}", e))
                })?
                .unwrap_or_default();
            let id_columns = builder::PrimaryKeySpec::from_catalog_text(&id_column);
            let columns = builder::PropertyColumns::from_catalog_text(&columns_str);
            let tenant_column = row
                .get::<String>(5)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (tenant_column): {}",
                        e
                    ))
                })?
                .filter(|s| !s.is_empty());

            tables.push(builder::RegisteredTable {
                table_oid,
                table_name,
                id_columns,
                columns,
                tenant_column,
            });
        }
        Ok::<(), safety::GraphError>(())
    })?;

    Spi::connect(|client| {
        let result = client
            .select(
                "SELECT registered.from_table_oid::integer,
                        pg_catalog.quote_ident(source_namespace.nspname) || '.' || pg_catalog.quote_ident(source_relation.relname),
                        registered.from_column,
                        registered.source_key_columns,
                        registered.to_table_oid::integer,
                        pg_catalog.quote_ident(target_namespace.nspname) || '.' || pg_catalog.quote_ident(target_relation.relname),
                        registered.to_column,
                        registered.label,
                        registered.bidirectional,
                        registered.weight_column,
                        registered.label_column
                   FROM graph._registered_edges AS registered
                   LEFT JOIN pg_catalog.pg_class AS source_relation
                     ON source_relation.oid = registered.from_table_oid
                   LEFT JOIN pg_catalog.pg_namespace AS source_namespace
                     ON source_namespace.oid = source_relation.relnamespace
                   LEFT JOIN pg_catalog.pg_class AS target_relation
                     ON target_relation.oid = registered.to_table_oid
                   LEFT JOIN pg_catalog.pg_namespace AS target_namespace
                     ON target_namespace.oid = target_relation.relnamespace
                  WHERE registered.graph_id = $1::uuid
                  ORDER BY registered.from_table, registered.from_column, registered.to_table, registered.to_column, registered.label",
                None,
                &[graph_id.into()],
            )
            .map_err(|e| {
                safety::GraphError::Internal(format!(
                    "catalog read failed for graph._registered_edges: {}",
                    e
                ))
            })?;
        for row in result {
            let from_table_oid = row
                .get::<i32>(1)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (from_table_oid): {e}"
                    ))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered edge has no source relation OID; re-register it".to_string(),
                    )
                })
                .and_then(|oid| {
                    u32::try_from(oid).map_err(|_| {
                        safety::GraphError::Internal(format!(
                            "registered edge has invalid source relation OID {oid}"
                        ))
                    })
                })?;
            let from_table = row
                .get::<String>(2)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (from_table): {e}"))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered edge source relation no longer exists; re-register it"
                            .to_string(),
                    )
                })?;
            let from_column = row
                .get::<String>(3)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (from_column): {}", e))
                })?
                .unwrap_or_default();
            let source_key_columns = row
                .get::<String>(4)
                .map_err(|e| safety::GraphError::Internal(format!("catalog read error (source_key_columns): {e}")))?
                .filter(|columns| !columns.trim().is_empty())
                .map(|columns| builder::PrimaryKeySpec::from_catalog_text(&columns))
                .ok_or_else(|| safety::GraphError::InvalidFilter {
                    reason: format!("registered edge source relation OID {from_table_oid} has no stable primary-key mapping; re-register it"),
                })?;
            let to_table_oid = row
                .get::<i32>(5)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (to_table_oid): {e}"))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered edge has no target relation OID; re-register it".to_string(),
                    )
                })
                .and_then(|oid| {
                    u32::try_from(oid).map_err(|_| {
                        safety::GraphError::Internal(format!(
                            "registered edge has invalid target relation OID {oid}"
                        ))
                    })
                })?;
            let to_table = row
                .get::<String>(6)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (to_table): {}", e))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered edge target relation no longer exists; re-register it"
                            .to_string(),
                    )
                })?;
            let to_column = row
                .get::<String>(7)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (to_column): {}", e))
                })?
                .unwrap_or_default();
            let label = row
                .get::<String>(8)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (label): {}", e))
                })?
                .unwrap_or_default();
            let bidirectional = row
                .get::<bool>(9)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (bidirectional): {}",
                        e
                    ))
                })?
                .unwrap_or(true);
            let weight_column = row
                .get::<String>(10)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (weight_column): {}",
                        e
                    ))
                })?
                .filter(|s| !s.is_empty());
            let label_column = row
                .get::<String>(11)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (label_column): {}",
                        e
                    ))
                })?
                .filter(|s| !s.is_empty());

            edges.push(builder::RegisteredEdge {
                from_table_oid,
                from_table,
                from_column,
                source_key_columns,
                to_table_oid,
                to_table,
                to_column,
                label,
                bidirectional,
                weight_column,
                label_column,
            });
        }
        Ok::<(), safety::GraphError>(())
    })?;

    Spi::connect(|client| {
        let result = client
            .select(
                "SELECT registered.table_oid::integer,
                        pg_catalog.quote_ident(namespace.nspname) || '.' || pg_catalog.quote_ident(relation.relname),
                        registered.column_name,
                        registered.column_type
                   FROM graph._registered_filter_columns AS registered
                   LEFT JOIN pg_catalog.pg_class AS relation
                     ON relation.oid = registered.table_oid
                   LEFT JOIN pg_catalog.pg_namespace AS namespace
                     ON namespace.oid = relation.relnamespace
                  WHERE registered.graph_id = $1::uuid
                  ORDER BY registered.table_name, registered.column_name",
                None,
                &[graph_id.into()],
            )
            .map_err(|e| {
                safety::GraphError::Internal(format!(
                    "catalog read failed for graph._registered_filter_columns: {}",
                    e
                ))
            })?;
        for row in result {
            let table_oid = row
                .get::<i32>(1)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (filter table_oid): {e}"
                    ))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered filter has no relation OID; re-register it".to_string(),
                    )
                })
                .and_then(|oid| {
                    u32::try_from(oid).map_err(|_| {
                        safety::GraphError::Internal(format!(
                            "registered filter has invalid relation OID {oid}"
                        ))
                    })
                })?;
            let table_name = row
                .get::<String>(2)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (table_name): {}", e))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered filter relation no longer exists; re-register it".to_string(),
                    )
                })?;
            let column_name = row
                .get::<String>(3)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (column_name): {}", e))
                })?
                .unwrap_or_default();
            let column_type = row
                .get::<String>(4)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (column_type): {}", e))
                })?
                .unwrap_or_else(|| "numeric".to_string());
            filter_columns.push(builder::RegisteredFilterColumn {
                table_oid,
                table_name,
                column_name,
                column_type,
            });
        }
        Ok::<(), safety::GraphError>(())
    })?;

    Ok((tables, edges, filter_columns))
}

pub(crate) fn catalog_fingerprint(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filter_columns: &[builder::RegisteredFilterColumn],
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut table_rows = tables.to_vec();
    table_rows.sort_by(|a, b| a.table_name.cmp(&b.table_name));
    for table in table_rows {
        table.table_name.hash(&mut hasher);
        table.id_columns.hash(&mut hasher);
        table.columns.hash(&mut hasher);
        table.tenant_column.hash(&mut hasher);
    }
    let mut edge_rows = edges.to_vec();
    edge_rows.sort_by(|a, b| {
        a.from_table
            .cmp(&b.from_table)
            .then(a.from_column.cmp(&b.from_column))
            .then(a.to_table.cmp(&b.to_table))
            .then(a.to_column.cmp(&b.to_column))
            .then(a.label.cmp(&b.label))
    });
    for edge in edge_rows {
        edge.from_table.hash(&mut hasher);
        edge.from_column.hash(&mut hasher);
        edge.source_key_columns.hash(&mut hasher);
        edge.to_table.hash(&mut hasher);
        edge.to_column.hash(&mut hasher);
        edge.label.hash(&mut hasher);
        edge.bidirectional.hash(&mut hasher);
        edge.weight_column.hash(&mut hasher);
        edge.label_column.hash(&mut hasher);
    }
    let mut filter_rows = filter_columns.to_vec();
    filter_rows.sort_by(|a, b| {
        a.table_name
            .cmp(&b.table_name)
            .then(a.column_name.cmp(&b.column_name))
    });
    for filter in filter_rows {
        filter.table_name.hash(&mut hasher);
        filter.column_name.hash(&mut hasher);
        filter.column_type.hash(&mut hasher);
    }
    hasher.finish()
}

pub(crate) fn current_catalog_state() -> safety::GraphResult<(u64, Option<String>)> {
    let (tables, edges, filter_columns) = read_catalog()?;
    current_catalog_state_from_rows(&tables, &edges, &filter_columns)
}

pub(crate) fn current_catalog_state_from_rows(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filter_columns: &[builder::RegisteredFilterColumn],
) -> safety::GraphResult<(u64, Option<String>)> {
    let fingerprint = catalog_fingerprint(tables, edges, filter_columns);
    let drift_reason = registered_schema_drift_reason(tables, edges, filter_columns);
    Ok((fingerprint, drift_reason))
}
