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
    #[cfg(feature = "development")]
    crate::sql_facade::record_query_start_catalog_read();
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
                "SELECT registered.mapping_id,
                        registered.from_table_oid::integer,
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
            let mapping_id = row
                .get::<i64>(1)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (mapping_id): {e}"))
                })?
                .and_then(|id| u64::try_from(id).ok())
                .filter(|&id| id != 0)
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "registered edge has no valid mapping identity; re-register it".to_string(),
                    )
                })?;
            let from_table_oid = row
                .get::<i32>(2)
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
                .get::<String>(3)
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
                .get::<String>(4)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (from_column): {}", e))
                })?
                .unwrap_or_default();
            let source_key_columns = row
                .get::<String>(5)
                .map_err(|e| safety::GraphError::Internal(format!("catalog read error (source_key_columns): {e}")))?
                .filter(|columns| !columns.trim().is_empty())
                .map(|columns| builder::PrimaryKeySpec::from_catalog_text(&columns))
                .ok_or_else(|| safety::GraphError::InvalidFilter {
                    reason: format!("registered edge source relation OID {from_table_oid} has no stable primary-key mapping; re-register it"),
                })?;
            let to_table_oid = row
                .get::<i32>(6)
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
                .get::<String>(7)
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
                .get::<String>(8)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (to_column): {}", e))
                })?
                .unwrap_or_default();
            let label = row
                .get::<String>(9)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("catalog read error (label): {}", e))
                })?
                .unwrap_or_default();
            let bidirectional = row
                .get::<bool>(10)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (bidirectional): {}",
                        e
                    ))
                })?
                .unwrap_or(true);
            let weight_column = row
                .get::<String>(11)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (weight_column): {}",
                        e
                    ))
                })?
                .filter(|s| !s.is_empty());
            let label_column = row
                .get::<String>(12)
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "catalog read error (label_column): {}",
                        e
                    ))
                })?
                .filter(|s| !s.is_empty());

            edges.push(builder::RegisteredEdge {
                mapping_id,
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

/// Version 1 catalog digest: XXH3-64 over tagged, length-prefixed fields.
/// Integers and lengths use little-endian u64; text uses UTF-8 bytes.
fn registration_fingerprint_v1(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filter_columns: &[builder::RegisteredFilterColumn],
) -> u64 {
    let mut hash = xxhash_rust::xxh3::Xxh3::new();
    hash.update(b"pggraph-catalog-v1");
    let mut table_rows = tables.iter().collect::<Vec<_>>();
    table_rows.sort_by_key(|table| table.table_oid);
    digest_number(&mut hash, table_rows.len() as u64);
    for table in table_rows {
        digest_number(&mut hash, u64::from(table.table_oid));
        digest_text(&mut hash, &table.table_name);
        digest_columns(&mut hash, table.id_columns.columns());
        digest_columns(&mut hash, table.columns.as_slice());
        digest_optional_text(&mut hash, table.tenant_column.as_deref());
    }
    let mut edge_rows = edges.iter().collect::<Vec<_>>();
    edge_rows.sort_by_key(|edge| edge.mapping_id);
    digest_number(&mut hash, edge_rows.len() as u64);
    for edge in edge_rows {
        digest_number(&mut hash, edge.mapping_id);
        digest_number(&mut hash, u64::from(edge.from_table_oid));
        digest_text(&mut hash, &edge.from_table);
        digest_text(&mut hash, &edge.from_column);
        digest_columns(&mut hash, edge.source_key_columns.columns());
        digest_number(&mut hash, u64::from(edge.to_table_oid));
        digest_text(&mut hash, &edge.to_table);
        digest_text(&mut hash, &edge.to_column);
        digest_text(&mut hash, &edge.label);
        hash.update(&[u8::from(edge.bidirectional)]);
        digest_optional_text(&mut hash, edge.weight_column.as_deref());
        digest_optional_text(&mut hash, edge.label_column.as_deref());
    }
    let mut filter_rows = filter_columns.iter().collect::<Vec<_>>();
    filter_rows.sort_by(|a, b| (a.table_oid, &a.column_name).cmp(&(b.table_oid, &b.column_name)));
    digest_number(&mut hash, filter_rows.len() as u64);
    for filter in filter_rows {
        digest_number(&mut hash, u64::from(filter.table_oid));
        digest_text(&mut hash, &filter.table_name);
        digest_text(&mut hash, &filter.column_name);
        digest_text(&mut hash, &filter.column_type);
    }
    hash.digest()
}

/// Include inferred endpoint bindings so foreign-key replacement invalidates a build.
pub(crate) fn catalog_fingerprint(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filters: &[builder::RegisteredFilterColumn],
) -> safety::GraphResult<u64> {
    let mut hash = xxhash_rust::xxh3::Xxh3::new();
    hash.update(b"pggraph-bound-catalog-v1");
    digest_number(
        &mut hash,
        registration_fingerprint_v1(tables, edges, filters),
    );
    let mut bindings = edges.iter().collect::<Vec<_>>();
    bindings.sort_by_key(|edge| edge.mapping_id);
    digest_number(&mut hash, bindings.len() as u64);
    for edge in bindings {
        digest_number(&mut hash, edge.mapping_id);
        let source = builder::edge_source_node_oid(edge, tables)?;
        hash.update(&[u8::from(source.is_some())]);
        if let Some(oid) = source {
            digest_number(&mut hash, u64::from(oid));
        }
    }
    Ok(hash.digest())
}

fn digest_number(hash: &mut xxhash_rust::xxh3::Xxh3, value: u64) {
    hash.update(&value.to_le_bytes());
}

fn digest_text(hash: &mut xxhash_rust::xxh3::Xxh3, value: &str) {
    digest_number(hash, value.len() as u64);
    hash.update(value.as_bytes());
}

fn digest_columns(hash: &mut xxhash_rust::xxh3::Xxh3, columns: &[String]) {
    digest_number(hash, columns.len() as u64);
    for column in columns {
        digest_text(hash, column);
    }
}

fn digest_optional_text(hash: &mut xxhash_rust::xxh3::Xxh3, value: Option<&str>) {
    hash.update(&[u8::from(value.is_some())]);
    if let Some(value) = value {
        digest_text(hash, value);
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    #[test]
    fn catalog_fingerprint_encoding_v1_is_stable_and_unambiguous() {
        let mut table = builder::RegisteredTable {
            table_oid: 42,
            table_name: "public.nodes".into(),
            id_columns: vec!["id".into()].into(),
            columns: vec!["ab".into(), "c".into()].into(),
            tenant_column: None,
        };
        let original = registration_fingerprint_v1(&[table.clone()], &[], &[]);
        assert_eq!(original, 10_817_585_336_816_200_419);
        table.columns = vec!["a".into(), "bc".into()].into();
        assert_ne!(
            original,
            registration_fingerprint_v1(&[table.clone()], &[], &[])
        );
        let mut other = table.clone();
        other.table_oid = 43;
        other.table_name = "public.other".into();
        assert_eq!(
            registration_fingerprint_v1(&[table.clone(), other.clone()], &[], &[]),
            registration_fingerprint_v1(&[other, table], &[], &[])
        );
    }
}

pub(crate) fn current_catalog_state_from_rows(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filter_columns: &[builder::RegisteredFilterColumn],
) -> safety::GraphResult<(u64, Option<String>)> {
    let fingerprint = catalog_fingerprint(tables, edges, filter_columns)?;
    let drift_reason = registered_schema_drift_reason(tables, edges, filter_columns);
    Ok((fingerprint, drift_reason))
}
