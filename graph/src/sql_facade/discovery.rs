use super::admin::{build, with_panic_boundary};
use super::*;

/// Auto-discover tables and foreign keys from a schema.
///
/// See: `docs/user_guide/schema-registration.mdx`
#[pg_extern(schema = "graph")]
fn auto_discover(
    schema_name: default!(&str, "'public'"),
    graph_name: default!(Option<&str>, "NULL"),
    build: default!(bool, "true"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(item_type, String),
        name!(item_name, String),
        name!(details, String),
    ),
> {
    with_panic_boundary("auto_discover()", || {
        let graph = target_graph_metadata(graph_name, graph_tenant, graph_namespace);
        let mut result = match discover::discover_schema(schema_name) {
            Ok((tables, edges, discoveries)) => {
                register_discovery_for_graph(&graph.graph_id, tables, edges, discoveries)
                    .unwrap_or_else(|err| err.report())
            }
            Err(err) => err.report(),
        };

        if build {
            append_auto_build_summary(&mut result, &graph);
        }

        TableIterator::new(result)
    })
}

/// Auto-discover selected tables and FK edges between only those tables.
#[pg_extern(schema = "graph")]
fn auto_discover_tables(
    tables: Vec<pgrx::pg_sys::Oid>,
    tenant_column: default!(Option<String>, "NULL"),
    graph_name: default!(Option<&str>, "NULL"),
    build: default!(bool, "true"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(item_type, String),
        name!(item_name, String),
        name!(details, String),
    ),
> {
    with_panic_boundary("auto_discover_tables()", || {
        let graph = target_graph_metadata(graph_name, graph_tenant, graph_namespace);
        let table_oids = tables.iter().map(|oid| oid.to_u32()).collect::<Vec<_>>();
        let mut result = match discover::discover_table_set(&table_oids, tenant_column.as_deref()) {
            Ok((tables, edges, discoveries)) => {
                register_discovery_for_graph(&graph.graph_id, tables, edges, discoveries)
                    .unwrap_or_else(|err| err.report())
            }
            Err(err) => err.report(),
        };

        if build {
            append_auto_build_summary(&mut result, &graph);
        }
        TableIterator::new(result)
    })
}

/// Preview schema discovery without writing graph registration rows.
#[pg_extern(schema = "graph")]
fn preview_discover(
    schema_name: default!(&str, "'public'"),
    graph_name: default!(Option<&str>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(item_type, String),
        name!(item_name, String),
        name!(details, String),
    ),
> {
    with_panic_boundary("preview_discover()", || {
        if graph_name.is_some() {
            let _ = target_graph_metadata(graph_name, graph_tenant, graph_namespace);
        }
        let rows = match discover::discover_schema(schema_name) {
            Ok((_tables, _edges, discoveries)) => discovery_rows(discoveries),
            Err(err) => err.report(),
        };
        TableIterator::new(rows)
    })
}

/// Preview selected-table discovery without writing graph registration rows.
#[pg_extern(schema = "graph")]
fn preview_discover_tables(
    tables: Vec<pgrx::pg_sys::Oid>,
    tenant_column: default!(Option<String>, "NULL"),
    graph_name: default!(Option<&str>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(item_type, String),
        name!(item_name, String),
        name!(details, String),
    ),
> {
    with_panic_boundary("preview_discover_tables()", || {
        if graph_name.is_some() {
            let _ = target_graph_metadata(graph_name, graph_tenant, graph_namespace);
        }
        let table_oids = tables.iter().map(|oid| oid.to_u32()).collect::<Vec<_>>();
        let rows = match discover::discover_table_set(&table_oids, tenant_column.as_deref()) {
            Ok((_tables, _edges, discoveries)) => discovery_rows(discoveries),
            Err(err) => err.report(),
        };
        TableIterator::new(rows)
    })
}

/// Reject arbitrary row-predicate subgraphs until PostgreSQL-first semantics are complete.
#[pg_extern(schema = "graph")]
fn create_row_predicate_subgraph(
    graph_name: &str,
    row_predicate: pgrx::JsonB,
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) {
    with_panic_boundary("create_row_predicate_subgraph()", || {
        let _ = (graph_name, row_predicate, graph_tenant, graph_namespace);
        safety::GraphError::UnsupportedOperation {
            operation: "create_row_predicate_subgraph".to_string(),
            reason: "row-predicate subgraphs are not supported; use graph.auto_discover_tables() with an explicit table set and optional tenant_column"
                .to_string(),
        }
        .report();
    });
}

fn target_graph_metadata(
    graph_name: Option<&str>,
    graph_tenant: Option<&str>,
    graph_namespace: Option<&str>,
) -> catalog::GraphMetadata {
    if let Some(graph_name) = graph_name {
        catalog::resolve_visible_graph_metadata(graph_name, graph_tenant, graph_namespace)
            .unwrap_or_else(|err| err.report())
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("graph '{}' does not exist", graph_name),
                }
                .report()
            })
    } else {
        catalog::selected_or_default_graph_metadata().unwrap_or_else(|err| err.report())
    }
}

fn register_discovery_for_graph(
    graph_id: &str,
    tables: Vec<builder::RegisteredTable>,
    edges: Vec<builder::RegisteredEdge>,
    discoveries: Vec<discover::DiscoveryResult>,
) -> safety::GraphResult<Vec<(String, String, String)>> {
    for table in &tables {
        insert_registered_table_for_graph(
            graph_id,
            &table.table_name,
            &table.id_columns,
            &table.columns,
            table.tenant_column.as_deref(),
        )?;
    }

    for edge in &edges {
        insert_registered_edge_for_graph(
            graph_id,
            RegisteredEdgeInsert {
                from_table: &edge.from_table,
                from_column: &edge.from_column,
                to_table: &edge.to_table,
                to_column: &edge.to_column,
                label: &edge.label,
                bidirectional: edge.bidirectional,
                weight_column: edge.weight_column.as_deref(),
                label_column: edge.label_column.as_deref(),
            },
        )?;
    }

    Ok(discovery_rows(discoveries))
}

fn discovery_rows(discoveries: Vec<discover::DiscoveryResult>) -> Vec<(String, String, String)> {
    discoveries
        .into_iter()
        .map(|d| (d.item_type, d.item_name, d.details))
        .collect::<Vec<_>>()
}

fn append_auto_build_summary(
    result: &mut Vec<(String, String, String)>,
    graph: &catalog::GraphMetadata,
) {
    catalog::set_selected_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report());
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
