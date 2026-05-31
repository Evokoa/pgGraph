use super::admin::{build, with_panic_boundary};
use super::*;

/// Auto-discover tables and foreign keys from a schema.
///
/// See: `docs/user_guide/schema-registration.mdx`
#[pg_extern(schema = "graph")]
fn auto_discover(
    schema_name: default!(&str, "'public'"),
) -> TableIterator<
    'static,
    (
        name!(item_type, String),
        name!(item_name, String),
        name!(details, String),
    ),
> {
    with_panic_boundary("auto_discover()", || {
        let mut result = match discover::discover_schema(schema_name) {
            Ok((tables, edges, discoveries)) => {
                register_discovery(tables, edges, discoveries).unwrap_or_else(|err| err.report())
            }
            Err(err) => err.report(),
        };

        append_auto_build_summary(&mut result);

        TableIterator::new(result)
    })
}

/// Auto-discover selected tables and FK edges between only those tables.
#[pg_extern(schema = "graph")]
fn auto_discover_tables(
    tables: Vec<pgrx::pg_sys::Oid>,
    tenant_column: default!(Option<String>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(item_type, String),
        name!(item_name, String),
        name!(details, String),
    ),
> {
    with_panic_boundary("auto_discover_tables()", || {
        let table_oids = tables.iter().map(|oid| oid.to_u32()).collect::<Vec<_>>();
        let mut result = match discover::discover_table_set(&table_oids, tenant_column.as_deref()) {
            Ok((tables, edges, discoveries)) => {
                register_discovery(tables, edges, discoveries).unwrap_or_else(|err| err.report())
            }
            Err(err) => err.report(),
        };

        append_auto_build_summary(&mut result);
        TableIterator::new(result)
    })
}

fn register_discovery(
    tables: Vec<builder::RegisteredTable>,
    edges: Vec<builder::RegisteredEdge>,
    discoveries: Vec<discover::DiscoveryResult>,
) -> safety::GraphResult<Vec<(String, String, String)>> {
    for table in &tables {
        insert_registered_table(
            &table.table_name,
            &table.id_columns,
            &table.columns,
            table.tenant_column.as_deref(),
        )?;
    }

    for edge in &edges {
        insert_registered_edge(RegisteredEdgeInsert {
            from_table: &edge.from_table,
            from_column: &edge.from_column,
            to_table: &edge.to_table,
            to_column: &edge.to_column,
            label: &edge.label,
            bidirectional: edge.bidirectional,
            weight_column: edge.weight_column.as_deref(),
            label_column: edge.label_column.as_deref(),
        })?;
    }

    Ok(discoveries
        .into_iter()
        .map(|d| (d.item_type, d.item_name, d.details))
        .collect::<Vec<_>>())
}

fn append_auto_build_summary(result: &mut Vec<(String, String, String)>) {
    // Build automatically so discovered schemas are immediately queryable.
    let build_rows: Vec<_> = build().collect();
    if let Some((nodes, edges, _ms, mem_mb, sync_mode, projection_mode)) = build_rows.first() {
        result.push((
            "build".to_string(),
            "graph".to_string(),
            format!(
                "{} nodes, {} edges, {:.1} MB, sync_mode={}, projection_mode={}",
                nodes, edges, mem_mb, sync_mode, projection_mode
            ),
        ));
    }
}
