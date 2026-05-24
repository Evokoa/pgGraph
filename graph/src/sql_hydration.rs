//! SQL hydration helpers for source rows returned by graph operations.

use crate::catalog::{
    primary_key_expr, read_catalog, sql_table_name_from_catalog, table_oid_from_name,
};
use crate::{acl, safety, types};
use pgrx::prelude::*;
use std::collections::{HashMap, HashSet};

pub(crate) fn hydrate_node(
    table_oid: u32,
    node_id: &str,
) -> safety::GraphResult<Option<pgrx::JsonB>> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find_map(|table| {
            table_oid_from_name(&table.table_name)
                .ok()
                .filter(|oid| *oid == table_oid)
                .map(|_| table)
        })
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot hydrate node from unregistered table OID {}",
                table_oid
            ))
        })?;

    acl::check_table_acl(table_oid)?;
    let table_name = sql_table_name_from_catalog(&table.table_name)?;
    let pk_expr = primary_key_expr("src", &table.id_columns);
    Spi::connect(|client| {
        let query = format!(
            "SELECT to_jsonb(src.*) FROM {} src WHERE {} = $1 LIMIT 1",
            table_name.as_sql(),
            pk_expr
        );
        let result = client
            .select(&query, None, &[node_id.into()])
            .map_err(|e| {
                safety::GraphError::Internal(format!(
                    "hydration failed for {}: {}",
                    table_name.as_sql(),
                    e
                ))
            })?;
        if result.is_empty() {
            return Ok(None);
        }
        let row = result.first();
        row.get::<pgrx::JsonB>(1)
            .map_err(|e| safety::GraphError::Internal(format!("hydration read failed: {}", e)))
    })
}

pub(crate) fn hydrate_nodes(
    rows: &[types::TraversalResult],
) -> safety::GraphResult<HashMap<(u32, String), pgrx::JsonB>> {
    let mut ids_by_table: HashMap<u32, Vec<String>> = HashMap::new();
    for row in rows {
        ids_by_table
            .entry(row.node_table.0)
            .or_default()
            .push(row.node_id.clone());
    }
    if ids_by_table.is_empty() {
        return Ok(HashMap::new());
    }

    let needed_table_oids = ids_by_table.keys().copied().collect::<HashSet<_>>();
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let mut tables_by_oid = HashMap::with_capacity(needed_table_oids.len());
    for table in &tables {
        let oid = table_oid_from_name(&table.table_name)?;
        if needed_table_oids.contains(&oid) {
            tables_by_oid.insert(oid, table);
            if tables_by_oid.len() == needed_table_oids.len() {
                break;
            }
        }
    }

    let mut hydrated = HashMap::new();
    for (table_oid, mut node_ids) in ids_by_table {
        node_ids.sort();
        node_ids.dedup();
        let table = tables_by_oid.get(&table_oid).ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot hydrate nodes from unregistered table OID {}",
                table_oid
            ))
        })?;
        acl::check_table_acl(table_oid)?;
        let table_name = sql_table_name_from_catalog(&table.table_name)?;
        let pk_expr = primary_key_expr("src", &table.id_columns);
        let query = format!(
            "SELECT {} AS graph_node_id, to_jsonb(src.*) FROM {} src WHERE {} = ANY($1::text[])",
            pk_expr,
            table_name.as_sql(),
            pk_expr
        );
        Spi::connect(|client| {
            let result = client
                .select(&query, None, &[node_ids.clone().into()])
                .map_err(|e| {
                    safety::GraphError::Internal(format!(
                        "batch hydration failed for {}: {}",
                        table_name.as_sql(),
                        e
                    ))
                })?;
            for row in result {
                let node_id = row
                    .get::<String>(1)
                    .map_err(|e| {
                        safety::GraphError::Internal(format!("hydration PK read failed: {}", e))
                    })?
                    .ok_or_else(|| {
                        safety::GraphError::Internal("hydration returned NULL PK".to_string())
                    })?;
                if let Some(node) = row.get::<pgrx::JsonB>(2).map_err(|e| {
                    safety::GraphError::Internal(format!("hydration row read failed: {}", e))
                })? {
                    hydrated.insert((table_oid, node_id), node);
                }
            }
            Ok::<(), safety::GraphError>(())
        })?;
    }

    Ok(hydrated)
}
