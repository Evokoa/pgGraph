//! # Discover — Schema auto-discovery
//!
//! Discovers tables, primary keys, and foreign keys from the information schema
//! for SQL auto-discovery wrappers to register.
//!
//! Composite primary keys are supported:
//! - **Junction tables** (all PK columns are FKs) → registered as edges, not nodes.
//! - **Composite entities** (≥1 PK column is not a FK) → registered as nodes with
//!   a comma-separated `id_column` (e.g., `"order_id,line_number"`). The builder
//!   generates `jsonb_build_array(col1::text, col2::text)::text` as the PK string.
//!
//! See: `docs/user_guide/schema-registration.mdx`

use pgrx::prelude::*;
use std::collections::HashSet;

use crate::builder::{RegisteredEdge, RegisteredTable};
use crate::catalog::{regclass_text, validate_column_exists};
use crate::quote::{quote_ident, quote_literal};
use crate::safety::{GraphError, GraphResult};

/// Result of auto-discovery for SQL output.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    pub item_type: String, // "table", "edge", or "junction"
    pub item_name: String,
    pub details: String,
}

/// Info about a discovered table before classification.
struct DiscoveredTable {
    table_oid: u32,
    schema_name: String,
    table_name: String,
    pk_columns: Vec<String>,
    id_is_primary: bool,
    text_columns: Vec<String>,
}

/// Auto-discover tables and foreign keys from a schema.
pub fn discover_schema(
    schema_name: &str,
) -> GraphResult<(
    Vec<RegisteredTable>,
    Vec<RegisteredEdge>,
    Vec<DiscoveryResult>,
)> {
    let mut tables = Vec::new();
    let mut edges = Vec::new();
    let mut discoveries = Vec::new();

    // Step 1: Find all tables with their primary key columns (including composite PKs)
    let discovered_tables = discover_tables_with_pks(schema_name)?;

    // Step 2: Find all FK relationships in the schema (needed for both edge discovery
    // AND junction table classification)
    let schema_fks = discover_foreign_keys(schema_name)?;

    // Step 3: Classify each table
    for table in &discovered_tables {
        if table.pk_columns.is_empty() {
            // No PK at all — skip
            continue;
        }

        if table.pk_columns.len() == 1 {
            // Single-column PK tables are registered as node tables.
            let id_column = table.pk_columns[0].clone();
            discoveries.push(DiscoveryResult {
                item_type: "table".to_string(),
                item_name: table.table_name.clone(),
                details: format!(
                    "pk={}, columns=[{}]",
                    id_column,
                    table.text_columns.join(", ")
                ),
            });

            tables.push(RegisteredTable {
                table_name: format!(
                    "{}.{}",
                    quote_ident(schema_name),
                    quote_ident(&table.table_name)
                ),
                id_column,
                columns: table.text_columns.clone(),
                tenant_column: None,
            });
        } else {
            // Composite PK — classify as junction or entity
            let is_junction = classify_as_junction(
                table.table_oid,
                &table.table_name,
                &table.pk_columns,
                &schema_fks,
            );

            if is_junction {
                // Junction table: all PK cols are FKs — register FK edges from this table
                // (the FK edges are already picked up in Step 4 below, so just log a NOTICE)
                pgrx::notice!(
                    "graph: table '{}' has composite PK ({}) where all columns are foreign keys — treated as a junction table (edges only, not a node)",
                    table.table_name,
                    table.pk_columns.join(", ")
                );
                discoveries.push(DiscoveryResult {
                    item_type: "junction".to_string(),
                    item_name: table.table_name.clone(),
                    details: format!(
                        "composite pk=[{}], all columns are FKs — registered as edges",
                        table.pk_columns.join(", ")
                    ),
                });
            } else {
                // Composite entity: at least one PK col is NOT a FK — register as node
                let id_column = table.pk_columns.join(",");
                discoveries.push(DiscoveryResult {
                    item_type: "table".to_string(),
                    item_name: table.table_name.clone(),
                    details: format!(
                        "composite pk=[{}], columns=[{}]",
                        id_column,
                        table.text_columns.join(", ")
                    ),
                });

                tables.push(RegisteredTable {
                    table_name: format!(
                        "{}.{}",
                        quote_ident(schema_name),
                        quote_ident(&table.table_name)
                    ),
                    id_column,
                    columns: table.text_columns.clone(),
                    tenant_column: None,
                });
            }
        }
    }

    // Step 4: Register FK relationships as edges
    for fk in &schema_fks {
        let label = edge_label(&fk.from_column);

        discoveries.push(DiscoveryResult {
            item_type: "edge".to_string(),
            item_name: format!(
                "{}.{} → {}.{}",
                fk.from_table, fk.from_column, fk.to_table, fk.to_column
            ),
            details: format!("label={}, bidirectional=true", label),
        });

        edges.push(registered_edge(
            format!(
                "{}.{}",
                quote_ident(schema_name),
                quote_ident(&fk.from_table)
            ),
            &fk.from_column,
            format!("{}.{}", quote_ident(schema_name), quote_ident(&fk.to_table)),
            &fk.to_column,
            &fk.from_column,
        ));
    }

    Ok((tables, edges, discoveries))
}

/// Auto-discover graph metadata for an explicit set of tables.
pub fn discover_table_set(
    table_oids: &[u32],
    tenant_column: Option<&str>,
) -> GraphResult<(
    Vec<RegisteredTable>,
    Vec<RegisteredEdge>,
    Vec<DiscoveryResult>,
)> {
    validate_target_table_list(table_oids)?;

    let mut tables = Vec::new();
    let mut edges = Vec::new();
    let mut discoveries = Vec::new();
    let selected_oids = table_oids.iter().copied().collect::<HashSet<_>>();
    let discovered_tables = discover_tables_by_oid(table_oids)?;
    let selected_fks = discover_foreign_keys_for_tables(table_oids)?;
    let mut junction_oids = HashSet::new();

    for table in &discovered_tables {
        if let Some(column) = tenant_column {
            validate_column_exists(table.table_oid, column)?;
        }

        if table.pk_columns.len() == 1 {
            let id_column = table.pk_columns[0].clone();
            discoveries.push(DiscoveryResult {
                item_type: "table".to_string(),
                item_name: table.table_name.clone(),
                details: discovery_details("pk", &id_column, &table.text_columns, tenant_column),
            });
            tables.push(registered_table(table, id_column, tenant_column));
        } else if table.id_is_primary
            && classify_as_junction(
                table.table_oid,
                &table.table_name,
                &table.pk_columns,
                &selected_fks,
            )
        {
            junction_oids.insert(table.table_oid);
            pgrx::notice!(
                "graph: table '{}' has composite PK ({}) where all columns are foreign keys — treated as a junction table (edges only, not a node)",
                table.table_name,
                table.pk_columns.join(", ")
            );
            discoveries.push(DiscoveryResult {
                item_type: "junction".to_string(),
                item_name: table.table_name.clone(),
                details: format!(
                    "composite pk=[{}], all columns are FKs — registered as edges",
                    table.pk_columns.join(", ")
                ),
            });
        } else {
            let id_column = table.pk_columns.join(",");
            discoveries.push(DiscoveryResult {
                item_type: "table".to_string(),
                item_name: table.table_name.clone(),
                details: discovery_details(
                    if table.id_is_primary {
                        "composite pk"
                    } else {
                        "unique not-null key"
                    },
                    &id_column,
                    &table.text_columns,
                    tenant_column,
                ),
            });
            tables.push(registered_table(table, id_column, tenant_column));
        }
    }

    for junction_oid in &junction_oids {
        let junction_fks = selected_fks
            .iter()
            .filter(|fk| fk.from_oid == *junction_oid)
            .collect::<Vec<_>>();
        let Some(first_fk) = junction_fks.first() else {
            continue;
        };
        for fk in junction_fks.iter().skip(1) {
            let label = edge_label(&fk.from_column);
            discoveries.push(DiscoveryResult {
                item_type: "edge".to_string(),
                item_name: format!(
                    "{}.{} → {}.{}",
                    first_fk.from_table, first_fk.from_column, fk.to_table, fk.from_column
                ),
                details: format!("label={}, bidirectional=true", label),
            });
            edges.push(registered_edge(
                regclass_text(*junction_oid)?,
                &first_fk.from_column,
                regclass_text(fk.to_oid)?,
                &fk.from_column,
                &fk.from_column,
            ));
        }
    }

    for fk in selected_fks
        .iter()
        .filter(|fk| selected_oids.contains(&fk.from_oid) && selected_oids.contains(&fk.to_oid))
        .filter(|fk| !junction_oids.contains(&fk.from_oid))
    {
        let label = edge_label(&fk.from_column);

        discoveries.push(DiscoveryResult {
            item_type: "edge".to_string(),
            item_name: format!(
                "{}.{} → {}.{}",
                fk.from_table, fk.from_column, fk.to_table, fk.to_column
            ),
            details: format!("label={}, bidirectional=true", label),
        });
        edges.push(registered_edge(
            regclass_text(fk.from_oid)?,
            &fk.from_column,
            regclass_text(fk.to_oid)?,
            &fk.to_column,
            &fk.from_column,
        ));
    }

    Ok((tables, edges, discoveries))
}

/// A foreign key relationship discovered from the schema.
struct DiscoveredFk {
    from_oid: u32,
    from_table: String,
    from_column: String,
    to_oid: u32,
    to_table: String,
    to_column: String,
}

/// Discover all tables and their primary key columns.
fn discover_tables_with_pks(schema_name: &str) -> GraphResult<Vec<DiscoveredTable>> {
    // Query all tables with their PK columns (supports composite PKs)
    let table_pk_query = format!(
        "SELECT t.table_name::text AS table_name,
                kcu.column_name::text AS pk_column,
                kcu.ordinal_position
         FROM information_schema.tables t
         JOIN information_schema.table_constraints tc
           ON tc.table_schema = t.table_schema AND tc.table_name = t.table_name
           AND tc.constraint_type = 'PRIMARY KEY'
         JOIN information_schema.key_column_usage kcu
           ON kcu.constraint_name = tc.constraint_name AND kcu.table_schema = tc.table_schema
         WHERE t.table_schema = {}
           AND t.table_type = 'BASE TABLE'
         ORDER BY t.table_name, kcu.ordinal_position",
        quote_literal(schema_name)
    );

    let mut table_map: Vec<(String, Vec<String>)> = Vec::new();

    Spi::connect(|client| {
        let result = client
            .select(&table_pk_query, None, &[])
            .map_err(|e| GraphError::Internal(format!("Schema discovery failed: {}", e)))?;

        for row in result {
            let table_name: String = row
                .get::<String>(1)
                .map_err(|e| GraphError::Internal(format!("Cannot read table_name: {}", e)))?
                .unwrap_or_default();
            let pk_column: String = row
                .get::<String>(2)
                .map_err(|e| GraphError::Internal(format!("Cannot read pk_column: {}", e)))?
                .unwrap_or_default();

            // Group PK columns by table name
            if let Some(last) = table_map.last_mut() {
                if last.0 == table_name {
                    last.1.push(pk_column);
                    continue;
                }
            }
            table_map.push((table_name, vec![pk_column]));
        }
        Ok::<(), GraphError>(())
    })?;

    // For each table, discover text/varchar columns
    let mut discovered = Vec::new();
    for (table_name, pk_columns) in table_map {
        let text_columns = discover_text_columns(schema_name, &table_name, &pk_columns)?;
        discovered.push(DiscoveredTable {
            table_oid: 0,
            schema_name: schema_name.to_string(),
            table_name,
            pk_columns,
            id_is_primary: true,
            text_columns,
        });
    }

    Ok(discovered)
}

/// Discover text/varchar columns for a table (excluding PK columns).
fn discover_text_columns(
    schema_name: &str,
    table_name: &str,
    pk_columns: &[String],
) -> GraphResult<Vec<String>> {
    let pk_exclusions = pk_columns
        .iter()
        .map(|c| format!("column_name != {}", quote_literal(c)))
        .collect::<Vec<_>>()
        .join(" AND ");

    let pk_filter = if pk_exclusions.is_empty() {
        String::new()
    } else {
        format!(" AND {}", pk_exclusions)
    };

    let col_query = format!(
        "SELECT column_name::text FROM information_schema.columns
         WHERE table_schema = {} AND table_name = {}
           AND data_type IN ('text', 'character varying')
           AND (character_maximum_length IS NULL OR character_maximum_length <= 128)
           {}
         ORDER BY ordinal_position",
        quote_literal(schema_name),
        quote_literal(table_name),
        pk_filter
    );

    let mut columns = Vec::new();
    Spi::connect(|client| {
        let col_result = client
            .select(&col_query, None, &[])
            .map_err(|e| GraphError::Internal(format!("Column discovery failed: {}", e)))?;

        for col_row in col_result {
            if let Ok(Some(col_name)) = col_row.get::<String>(1) {
                columns.push(col_name);
            }
        }
        Ok::<(), GraphError>(())
    })?;

    Ok(columns)
}

/// Discover all foreign key relationships in a schema.
fn discover_foreign_keys(schema_name: &str) -> GraphResult<Vec<DiscoveredFk>> {
    let fk_query = format!(
        "SELECT
            tc.table_name::text AS from_table,
            kcu.column_name::text AS from_column,
            ccu.table_name::text AS to_table,
            ccu.column_name::text AS to_column
         FROM information_schema.table_constraints tc
         JOIN information_schema.key_column_usage kcu
           ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema
         JOIN information_schema.constraint_column_usage ccu
           ON ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema
         WHERE tc.constraint_type = 'FOREIGN KEY'
           AND tc.table_schema = {}",
        quote_literal(schema_name)
    );

    let mut fks = Vec::new();
    Spi::connect(|client| {
        let result = client
            .select(&fk_query, None, &[])
            .map_err(|e| GraphError::Internal(format!("FK discovery failed: {}", e)))?;

        for row in result {
            let from_table: String = row
                .get::<String>(1)
                .map_err(|e| GraphError::Internal(format!("Cannot read from_table: {}", e)))?
                .unwrap_or_default();
            let from_column: String = row
                .get::<String>(2)
                .map_err(|e| GraphError::Internal(format!("Cannot read from_column: {}", e)))?
                .unwrap_or_default();
            let to_table: String = row
                .get::<String>(3)
                .map_err(|e| GraphError::Internal(format!("Cannot read to_table: {}", e)))?
                .unwrap_or_default();
            let to_column: String = row
                .get::<String>(4)
                .map_err(|e| GraphError::Internal(format!("Cannot read to_column: {}", e)))?
                .unwrap_or_default();

            fks.push(DiscoveredFk {
                from_oid: 0,
                from_table,
                from_column,
                to_oid: 0,
                to_table,
                to_column,
            });
        }
        Ok::<(), GraphError>(())
    })?;

    Ok(fks)
}

fn validate_target_table_list(table_oids: &[u32]) -> GraphResult<()> {
    if table_oids.is_empty() {
        return Err(GraphError::InvalidFilter {
            reason: "auto_discover_tables() requires at least one table".to_string(),
        });
    }

    let mut seen = HashSet::with_capacity(table_oids.len());
    for oid in table_oids {
        if !seen.insert(*oid) {
            return Err(GraphError::InvalidFilter {
                reason: format!(
                    "auto_discover_tables() table list contains duplicate table {}",
                    regclass_text(*oid).unwrap_or_else(|_| format!("OID {}", oid))
                ),
            });
        }
        validate_supported_relation(*oid)?;
    }
    Ok(())
}

fn validate_supported_relation(table_oid: u32) -> GraphResult<()> {
    let relkind = Spi::connect(|client| {
        let table_oid = pgrx::pg_sys::Oid::from_u32(table_oid);
        let result = client
            .select(
                "SELECT c.relkind::text
                 FROM pg_class c
                 WHERE c.oid = $1::oid",
                None,
                &[table_oid.into()],
            )
            .map_err(|err| GraphError::Internal(format!("relation validation failed: {}", err)))?;
        let row = result.first();
        row.get::<String>(1)
            .map_err(|err| GraphError::Internal(format!("relation kind read failed: {}", err)))?
            .ok_or_else(|| {
                GraphError::Internal(format!("relation OID {} does not exist", table_oid))
            })
    })?;

    match relkind.as_str() {
        "r" | "p" => Ok(()),
        "v" => Err(GraphError::InvalidFilter {
            reason: format!(
                "auto_discover_tables() does not support views: {}",
                regclass_text(table_oid).unwrap_or_else(|_| format!("OID {}", table_oid))
            ),
        }),
        "m" => Err(GraphError::InvalidFilter {
            reason: format!(
                "auto_discover_tables() does not support materialized views: {}",
                regclass_text(table_oid).unwrap_or_else(|_| format!("OID {}", table_oid))
            ),
        }),
        _ => Err(GraphError::InvalidFilter {
            reason: format!(
                "auto_discover_tables() only supports base tables: {}",
                regclass_text(table_oid).unwrap_or_else(|_| format!("OID {}", table_oid))
            ),
        }),
    }
}

fn discover_tables_by_oid(table_oids: &[u32]) -> GraphResult<Vec<DiscoveredTable>> {
    let mut tables = Vec::with_capacity(table_oids.len());
    for oid in table_oids {
        let (schema_name, table_name, id_is_primary, pk_columns) = discover_identifier(*oid)?;
        let text_columns = discover_text_columns(&schema_name, &table_name, &pk_columns)?;
        tables.push(DiscoveredTable {
            table_oid: *oid,
            schema_name,
            table_name,
            pk_columns,
            id_is_primary,
            text_columns,
        });
    }
    Ok(tables)
}

fn discover_identifier(table_oid: u32) -> GraphResult<(String, String, bool, Vec<String>)> {
    Spi::connect(|client| {
        let query = format!(
            "SELECT
                n.nspname::text,
                c.relname::text,
                i.indisprimary,
                array_agg(a.attname::text ORDER BY ord.n)::text[] AS columns,
                bool_and(a.attnotnull) AS all_not_null
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             JOIN pg_index i ON i.indrelid = c.oid
             JOIN unnest(i.indkey) WITH ORDINALITY AS ord(attnum, n) ON true
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ord.attnum
             WHERE c.oid = {}::oid
               AND (i.indisprimary OR i.indisunique)
               AND i.indpred IS NULL
             GROUP BY n.nspname, c.relname, i.indexrelid, i.indisprimary
             HAVING bool_and(a.attnotnull)
             ORDER BY i.indisprimary DESC, i.indexrelid
             LIMIT 1",
            table_oid
        );
        let result = client.select(&query, None, &[]).map_err(|err| {
            GraphError::Internal(format!("targeted table discovery failed: {}", err))
        })?;
        let row = result.first();
        let Some(schema_name) = row
            .get::<String>(1)
            .map_err(|err| GraphError::Internal(format!("schema name read failed: {}", err)))?
        else {
            return Err(GraphError::InvalidFilter {
                reason: format!(
                    "table {} must have a primary key or unique NOT NULL key",
                    regclass_text(table_oid).unwrap_or_else(|_| format!("OID {}", table_oid))
                ),
            });
        };
        let table_name = row
            .get::<String>(2)
            .map_err(|err| GraphError::Internal(format!("table name read failed: {}", err)))?
            .unwrap_or_default();
        let id_is_primary = row
            .get::<bool>(3)
            .map_err(|err| GraphError::Internal(format!("identifier kind read failed: {}", err)))?
            .unwrap_or(false);
        let columns = row
            .get::<Vec<String>>(4)
            .map_err(|err| {
                GraphError::Internal(format!("identifier columns read failed: {}", err))
            })?
            .unwrap_or_default();
        Ok((schema_name, table_name, id_is_primary, columns))
    })
}

fn discover_foreign_keys_for_tables(table_oids: &[u32]) -> GraphResult<Vec<DiscoveredFk>> {
    let oid_list = table_oids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT
            c.conrelid::oid::integer AS from_oid,
            from_class.relname::text AS from_table,
            from_attr.attname::text AS from_column,
            c.confrelid::oid::integer AS to_oid,
            to_class.relname::text AS to_table,
            to_attr.attname::text AS to_column
         FROM pg_constraint c
         JOIN pg_class from_class ON from_class.oid = c.conrelid
         JOIN pg_class to_class ON to_class.oid = c.confrelid
         JOIN unnest(c.conkey) WITH ORDINALITY AS fk_from(attnum, n) ON true
         JOIN unnest(c.confkey) WITH ORDINALITY AS fk_to(attnum, n) ON fk_to.n = fk_from.n
         JOIN pg_attribute from_attr ON from_attr.attrelid = c.conrelid AND from_attr.attnum = fk_from.attnum
         JOIN pg_attribute to_attr ON to_attr.attrelid = c.confrelid AND to_attr.attnum = fk_to.attnum
         WHERE c.contype = 'f'
           AND c.conrelid IN ({0})
           AND c.confrelid IN ({0})
         ORDER BY c.conrelid, c.oid, fk_from.n",
        oid_list
    );

    let mut fks = Vec::new();
    Spi::connect(|client| {
        let result = client.select(&query, None, &[]).map_err(|err| {
            GraphError::Internal(format!("targeted FK discovery failed: {}", err))
        })?;
        for row in result {
            fks.push(DiscoveredFk {
                from_oid: row
                    .get::<i32>(1)
                    .map_err(|err| GraphError::Internal(format!("from_oid read failed: {}", err)))?
                    .unwrap_or_default() as u32,
                from_table: row
                    .get::<String>(2)
                    .map_err(|err| {
                        GraphError::Internal(format!("from_table read failed: {}", err))
                    })?
                    .unwrap_or_default(),
                from_column: row
                    .get::<String>(3)
                    .map_err(|err| {
                        GraphError::Internal(format!("from_column read failed: {}", err))
                    })?
                    .unwrap_or_default(),
                to_oid: row
                    .get::<i32>(4)
                    .map_err(|err| GraphError::Internal(format!("to_oid read failed: {}", err)))?
                    .unwrap_or_default() as u32,
                to_table: row
                    .get::<String>(5)
                    .map_err(|err| GraphError::Internal(format!("to_table read failed: {}", err)))?
                    .unwrap_or_default(),
                to_column: row
                    .get::<String>(6)
                    .map_err(|err| GraphError::Internal(format!("to_column read failed: {}", err)))?
                    .unwrap_or_default(),
            });
        }
        Ok::<(), GraphError>(())
    })?;
    Ok(fks)
}

fn registered_table(
    table: &DiscoveredTable,
    id_column: String,
    tenant_column: Option<&str>,
) -> RegisteredTable {
    RegisteredTable {
        table_name: format!(
            "{}.{}",
            quote_ident(&table.schema_name),
            quote_ident(&table.table_name)
        ),
        id_column,
        columns: table.text_columns.clone(),
        tenant_column: tenant_column.map(ToString::to_string),
    }
}

fn discovery_details(
    key_label: &str,
    id_column: &str,
    text_columns: &[String],
    tenant_column: Option<&str>,
) -> String {
    let tenant = tenant_column
        .map(|column| format!(", tenant_column={}", column))
        .unwrap_or_default();
    format!(
        "{}={}, columns=[{}]{}",
        key_label,
        id_column,
        text_columns.join(", "),
        tenant
    )
}

fn edge_label(label_source_column: &str) -> String {
    label_source_column
        .trim_end_matches("_id")
        .trim_end_matches("_fk")
        .to_string()
}

fn registered_edge(
    from_table: String,
    from_column: &str,
    to_table: String,
    to_column: &str,
    label_source_column: &str,
) -> RegisteredEdge {
    RegisteredEdge {
        from_table,
        from_column: from_column.to_string(),
        to_table,
        to_column: to_column.to_string(),
        label: edge_label(label_source_column),
        bidirectional: true,
        weight_column: None,
        label_column: None,
    }
}

/// Classify a composite-PK table as a junction table or a composite entity.
///
/// A junction table has ALL of its PK columns participating as FK source columns.
/// A composite entity has at least one PK column that is NOT a FK.
fn classify_as_junction(
    table_oid: u32,
    table_name: &str,
    pk_columns: &[String],
    schema_fks: &[DiscoveredFk],
) -> bool {
    // Collect all FK source columns for this specific table
    let fk_source_columns: Vec<&str> = schema_fks
        .iter()
        .filter(|fk| {
            if table_oid == 0 {
                fk.from_table == table_name
            } else {
                fk.from_oid == table_oid
            }
        })
        .map(|fk| fk.from_column.as_str())
        .collect();

    // Check if every PK column is also a FK source column
    pk_columns
        .iter()
        .all(|pk_col| fk_source_columns.contains(&pk_col.as_str()))
}

#[cfg(test)]
mod tests {
    //! Covers schema discovery classification, especially junction-table
    //! detection from primary-key and foreign-key relationships.

    use super::{classify_as_junction, edge_label, registered_edge, DiscoveredFk};

    fn fk(from_table: &str, from_column: &str, to_table: &str, to_column: &str) -> DiscoveredFk {
        DiscoveredFk {
            from_oid: 0,
            from_table: from_table.to_string(),
            from_column: from_column.to_string(),
            to_oid: 0,
            to_table: to_table.to_string(),
            to_column: to_column.to_string(),
        }
    }

    #[test]
    fn classify_as_junction_true_when_all_pk_columns_are_fk_sources() {
        let pk_columns = vec!["user_id".to_string(), "group_id".to_string()];
        let schema_fks = vec![
            fk("user_groups", "user_id", "users", "id"),
            fk("user_groups", "group_id", "groups", "id"),
        ];

        assert!(classify_as_junction(
            0,
            "user_groups",
            &pk_columns,
            &schema_fks
        ));
    }

    #[test]
    fn classify_as_junction_false_when_any_pk_column_is_not_fk_source() {
        let pk_columns = vec!["order_id".to_string(), "line_number".to_string()];
        let schema_fks = vec![fk("order_lines", "order_id", "orders", "id")];

        assert!(!classify_as_junction(
            0,
            "order_lines",
            &pk_columns,
            &schema_fks
        ));
    }

    #[test]
    fn classify_as_junction_ignores_foreign_keys_from_other_tables() {
        let pk_columns = vec!["a_id".to_string(), "b_id".to_string()];
        let schema_fks = vec![
            fk("other_table", "a_id", "a", "id"),
            fk("other_table", "b_id", "b", "id"),
        ];

        assert!(!classify_as_junction(
            0,
            "junction_table",
            &pk_columns,
            &schema_fks
        ));
    }

    #[test]
    fn registered_edge_uses_shared_defaults_and_label_source_column() {
        let edge = registered_edge(
            "public.user_groups".to_string(),
            "user_id",
            "public.groups".to_string(),
            "group_id",
            "group_id",
        );

        assert_eq!(edge.from_table, "public.user_groups");
        assert_eq!(edge.from_column, "user_id");
        assert_eq!(edge.to_table, "public.groups");
        assert_eq!(edge.to_column, "group_id");
        assert_eq!(edge.label, "group");
        assert!(edge.bidirectional);
        assert_eq!(edge.weight_column, None);
        assert_eq!(edge.label_column, None);
    }

    #[test]
    fn edge_label_strips_supported_fk_suffixes() {
        assert_eq!(edge_label("user_id"), "user");
        assert_eq!(edge_label("account_fk"), "account");
        assert_eq!(edge_label("owner"), "owner");
    }
}
