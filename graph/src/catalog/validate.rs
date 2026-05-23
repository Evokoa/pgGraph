use crate::quote::quote_ident;
use crate::{builder, safety};
use pgrx::prelude::*;

pub(crate) fn registered_schema_drift_reason(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filter_columns: &[builder::RegisteredFilterColumn],
) -> Option<String> {
    for table in tables {
        let oid = match table_oid_from_name(&table.table_name) {
            Ok(oid) => oid,
            Err(err) => {
                return Some(format!(
                    "registered table '{}' is unavailable: {}",
                    table.table_name, err
                ));
            }
        };
        if let Err(err) = validate_registered_table(
            oid,
            &table.id_column,
            Some(&table.columns),
            table.tenant_column.as_deref(),
        ) {
            return Some(format!(
                "registered table '{}' no longer matches graph catalog: {}",
                table.table_name, err
            ));
        }
    }

    for edge in edges {
        let from_oid = match table_oid_from_name(&edge.from_table) {
            Ok(oid) => oid,
            Err(err) => {
                return Some(format!(
                    "registered edge '{}' source table '{}' is unavailable: {}",
                    edge.label, edge.from_table, err
                ));
            }
        };
        let to_oid = match table_oid_from_name(&edge.to_table) {
            Ok(oid) => oid,
            Err(err) => {
                return Some(format!(
                    "registered edge '{}' target table '{}' is unavailable: {}",
                    edge.label, edge.to_table, err
                ));
            }
        };
        if let Err(err) = validate_column_exists(from_oid, &edge.from_column) {
            return Some(format!(
                "registered edge '{}' source column '{}.{}' is invalid: {}",
                edge.label, edge.from_table, edge.from_column, err
            ));
        }
        if validate_column_exists(to_oid, &edge.to_column).is_err()
            && validate_column_exists(from_oid, &edge.to_column).is_err()
        {
            return Some(format!(
                "registered edge '{}' target column '{}' no longer exists on target table '{}' or source edge table '{}'",
                edge.label, edge.to_column, edge.to_table, edge.from_table
            ));
        }
        if let Some(weight_column) = edge.weight_column.as_deref() {
            if let Err(err) = validate_numeric_column(from_oid, weight_column) {
                return Some(format!(
                    "registered edge '{}' weight column '{}.{}' is invalid: {}",
                    edge.label, edge.from_table, weight_column, err
                ));
            }
        }
        if let Some(label_column) = edge.label_column.as_deref() {
            if let Err(err) = validate_column_exists(from_oid, label_column) {
                return Some(format!(
                    "registered edge '{}' label column '{}.{}' is invalid: {}",
                    edge.label, edge.from_table, label_column, err
                ));
            }
        }
    }

    for filter in filter_columns {
        let table_oid = match table_oid_from_name(&filter.table_name) {
            Ok(oid) => oid,
            Err(err) => {
                return Some(format!(
                    "registered filter table '{}' is unavailable: {}",
                    filter.table_name, err
                ));
            }
        };
        if let Err(err) =
            validate_filter_column_type(table_oid, &filter.column_name, &filter.column_type)
        {
            return Some(format!(
                "registered filter column '{}.{}' is invalid: {}",
                filter.table_name, filter.column_name, err
            ));
        }
    }

    None
}

pub(crate) fn split_catalog_columns(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|col| !col.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::split_catalog_columns;

    /// Covers catalog column-list parsing used by discovery functions and the
    /// `id_columns` compatibility layer.
    #[test]
    fn split_catalog_columns_ignores_empty_segments_and_whitespace() {
        assert_eq!(
            split_catalog_columns(" id, , tenant_id "),
            vec!["id".to_string(), "tenant_id".to_string()]
        );
        assert!(split_catalog_columns("").is_empty());
    }
}

pub(crate) fn regclass_text(table_oid: u32) -> safety::GraphResult<String> {
    Spi::connect(|client| {
        let table_oid = pgrx::pg_sys::Oid::from_u32(table_oid);
        let result = client
            .select("SELECT $1::oid::regclass::text", None, &[table_oid.into()])
            .map_err(|e| safety::GraphError::Internal(format!("regclass lookup failed: {}", e)))?;
        let row = result.first();
        row.get::<String>(1)
            .map_err(|e| safety::GraphError::Internal(format!("regclass read failed: {}", e)))?
            .ok_or_else(|| {
                safety::GraphError::Internal(format!("NULL regclass for OID {}", table_oid))
            })
    })
}

#[derive(Debug, Clone)]
pub(crate) struct SqlTableName {
    oid: u32,
    sql: String,
}

impl SqlTableName {
    pub(crate) fn oid(&self) -> u32 {
        self.oid
    }

    pub(crate) fn as_sql(&self) -> &str {
        &self.sql
    }
}

pub(crate) fn sql_table_name_from_catalog(table_name: &str) -> safety::GraphResult<SqlTableName> {
    let oid = table_oid_from_name(table_name)?;
    let sql = regclass_text(oid)?;
    Ok(SqlTableName { oid, sql })
}

pub(crate) fn table_oid_from_name(table_name: &str) -> safety::GraphResult<u32> {
    Spi::connect(|client| {
        let result = client
            .select(
                "SELECT to_regclass($1)::oid::integer",
                None,
                &[table_name.into()],
            )
            .map_err(|e| {
                safety::GraphError::Internal(format!(
                    "table lookup failed for {}: {}",
                    table_name, e
                ))
            })?;
        let row = result.first();
        row.get::<i32>(1)
            .map_err(|e| safety::GraphError::Internal(format!("table OID read failed: {}", e)))?
            .map(|oid| oid as u32)
            .ok_or_else(|| {
                safety::GraphError::Internal(format!("relation not found: {}", table_name))
            })
    })
}

pub(crate) fn primary_key_expr(alias: &str, id_column: &str) -> String {
    if id_column.contains(',') {
        let parts = id_column
            .split(',')
            .map(str::trim)
            .filter(|col| !col.is_empty())
            .map(|col| format!("{}.{}::text", alias, quote_ident(col)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("jsonb_build_array({})::text", parts)
    } else {
        format!("{}.{}::text", alias, quote_ident(id_column))
    }
}

pub(crate) fn validate_column_exists(table_oid: u32, column: &str) -> safety::GraphResult<()> {
    let exists = Spi::connect(|client| {
        let table_oid = pgrx::pg_sys::Oid::from_u32(table_oid);
        let result = client
            .select(
                "SELECT EXISTS (
                SELECT 1
                FROM pg_attribute
                WHERE attrelid = $1::oid
                  AND attname = $2
                  AND attnum > 0
                  AND NOT attisdropped
            )",
                None,
                &[table_oid.into(), column.into()],
            )
            .map_err(|e| {
                safety::GraphError::Internal(format!("column validation failed: {}", e))
            })?;
        let row = result.first();
        Ok::<_, safety::GraphError>(
            row.get::<bool>(1)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("column validation read failed: {}", e))
                })?
                .unwrap_or(false),
        )
    })?;
    if exists {
        Ok(())
    } else {
        Err(safety::GraphError::Internal(format!(
            "column '{}' does not exist on table OID {}",
            column, table_oid
        )))
    }
}

pub(crate) fn validate_numeric_column(table_oid: u32, column: &str) -> safety::GraphResult<()> {
    let is_numeric = Spi::connect(|client| {
        let table_oid = pgrx::pg_sys::Oid::from_u32(table_oid);
        let result = client
            .select(
                "SELECT t.typcategory = 'N'
             FROM pg_attribute a
             JOIN pg_type t ON t.oid = a.atttypid
             WHERE a.attrelid = $1::oid
               AND a.attname = $2
               AND a.attnum > 0
               AND NOT a.attisdropped",
                None,
                &[table_oid.into(), column.into()],
            )
            .map_err(|e| {
                safety::GraphError::Internal(format!("numeric validation failed: {}", e))
            })?;
        let row = result.first();
        Ok::<_, safety::GraphError>(
            row.get::<bool>(1)
                .map_err(|e| {
                    safety::GraphError::Internal(format!("numeric validation read failed: {}", e))
                })?
                .unwrap_or(false),
        )
    })?;

    if is_numeric {
        Ok(())
    } else {
        Err(safety::GraphError::Internal(format!(
            "column '{}' on table OID {} is not numeric",
            column, table_oid
        )))
    }
}

pub(crate) fn validate_filter_column_type(
    table_oid: u32,
    column: &str,
    column_type: &str,
) -> safety::GraphResult<()> {
    match column_type.trim().to_ascii_lowercase().as_str() {
        "numeric" => validate_numeric_column(table_oid, column),
        "text" | "boolean" | "date" | "timestamptz" | "uuid" => {
            validate_column_exists(table_oid, column)
        }
        other => Err(safety::GraphError::InvalidFilter {
            reason: format!(
                "unsupported filter column_type '{}'; expected text, numeric, boolean, date, timestamptz, or uuid",
                other
            ),
        }),
    }
}

pub(crate) fn validate_registered_table(
    table_oid: u32,
    id_column: &str,
    columns: Option<&[String]>,
    tenant_column: Option<&str>,
) -> safety::GraphResult<()> {
    let candidate_keys = Spi::connect(|client| {
        let query = format!(
            "SELECT
                i.indisprimary,
                array_agg(a.attname::text ORDER BY ord.n)::text[] AS columns,
                bool_and(a.attnotnull) AS all_not_null
             FROM pg_index i
             JOIN unnest(i.indkey) WITH ORDINALITY AS ord(attnum, n) ON true
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ord.attnum
             WHERE i.indrelid = {}::oid
               AND (i.indisprimary OR i.indisunique)
               AND i.indpred IS NULL
             GROUP BY i.indexrelid, i.indisprimary
             ORDER BY i.indisprimary DESC, i.indexrelid",
            table_oid
        );
        let result = client.select(&query, None, &[]).map_err(|e| {
            safety::GraphError::Internal(format!("identifier validation failed: {}", e))
        })?;
        let mut keys = Vec::new();
        for row in result {
            let is_primary = row
                .get::<bool>(1)
                .map_err(|err| safety::GraphError::Internal(err.to_string()))?
                .unwrap_or(false);
            let columns = row
                .get::<Vec<String>>(2)
                .map_err(|err| safety::GraphError::Internal(err.to_string()))?
                .unwrap_or_default();
            let all_not_null = row
                .get::<bool>(3)
                .map_err(|err| safety::GraphError::Internal(err.to_string()))?
                .unwrap_or(false);
            keys.push((is_primary, columns, all_not_null));
        }
        Ok::<_, safety::GraphError>(keys)
    })?;

    let provided_cols = id_column
        .split(',')
        .map(str::trim)
        .filter(|col| !col.is_empty())
        .collect::<Vec<_>>();

    if provided_cols.is_empty() {
        return Err(safety::GraphError::InvalidFilter {
            reason: "id_columns must contain at least one column".to_string(),
        });
    }

    let matches_identifier =
        candidate_keys
            .iter()
            .any(|(_is_primary, key_columns, all_not_null)| {
                *all_not_null
                    && key_columns.len() == provided_cols.len()
                    && key_columns
                        .iter()
                        .zip(provided_cols.iter())
                        .all(|(actual, provided)| actual == provided)
            });

    if !matches_identifier {
        let available = candidate_keys
            .iter()
            .filter(|(_is_primary, _columns, all_not_null)| *all_not_null)
            .map(|(is_primary, columns, _all_not_null)| {
                let kind = if *is_primary {
                    "primary key"
                } else {
                    "unique NOT NULL index"
                };
                format!("{} ({})", kind, columns.join(","))
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(safety::GraphError::InvalidFilter {
            reason: if available.is_empty() {
                format!(
                    "id_columns '{}' must match a primary key or unique NOT NULL index on table OID {}",
                    id_column, table_oid
                )
            } else {
                format!(
                    "id_columns '{}' must match a primary key or unique NOT NULL index on table OID {}; available identifiers: {}",
                    id_column, table_oid, available
                )
            },
        });
    }

    for col in &provided_cols {
        validate_column_exists(table_oid, col)?;
    }
    if let Some(cols) = columns {
        for column in cols {
            validate_column_exists(table_oid, column)?;
        }
    }
    if let Some(column) = tenant_column {
        validate_column_exists(table_oid, column)?;
    }
    Ok(())
}
