use super::admin::{check_enabled_result, with_panic_boundary};
use super::runtime::{current_query_freshness, ensure_current_graph_for_query};
use super::*;

/// Explain how the openCypher compatibility subset binds and lowers.
#[pg_extern(schema = "graph")]
fn cypher_explain(query: &str) -> String {
    with_panic_boundary("cypher_explain()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let statement = build_statement(query)
            .unwrap_or_else(|err| gql::gql_error_to_graph_error(err).report());
        gql::explain_statement(statement)
    })
}

/// Execute the openCypher compatibility subset and return JSONB rows.
#[pg_extern(schema = "graph", cost = 1000, volatile)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes tuple row columns"
)]
fn cypher(
    query: &str,
    params: default!(Option<pgrx::JsonB>, "NULL"),
    hydrate: default!(bool, "true"),
) -> TableIterator<'static, (name!(row, pgrx::JsonB),)> {
    with_panic_boundary("cypher()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let tenant_scope = crate::sql_sync::resolve_tenant_scope_for_query(
            None,
            &query_start.graph,
            &query_start.tables,
        )
        .unwrap_or_else(|err| err.report());
        let statement = build_statement_from_query_start(query, &query_start)
            .unwrap_or_else(|err| gql::gql_error_to_graph_error(err).report());
        let params = gql::gql_params(params).unwrap_or_else(|err| err.report());
        let rows: Vec<_> = gql::execute_statement(
            statement,
            tenant_scope.as_deref(),
            &params,
            hydrate,
            &query_start.tables,
            &query_start.edges,
        )
        .unwrap_or_else(|err| err.report())
        .into_iter()
        .map(|row| (pgrx::JsonB(row),))
        .collect();
        TableIterator::new(rows)
    })
}

/// Return the openCypher compatibility matrix.
#[pg_extern(schema = "graph")]
fn cypher_compatibility() -> TableIterator<
    'static,
    (
        name!(feature, &'static str),
        name!(status, &'static str),
        name!(notes, &'static str),
    ),
> {
    let rows = crate::cypher::ast::COMPATIBILITY_MATRIX
        .iter()
        .map(|row| (row.feature, row.status, row.notes))
        .collect::<Vec<_>>();
    TableIterator::new(rows)
}

fn build_statement(
    query: &str,
) -> Result<crate::query::physical_plan::PhysicalStatement, crate::gql::errors::GqlError> {
    let ast = crate::cypher::parse_statement(query)?;
    let span = ast.span();
    let catalog = crate::query::catalog_snapshot::CatalogSnapshotImpl::load()
        .map_err(|err| crate::gql::errors::GqlError::bind(span, err.to_string()))?;
    crate::cypher::lower::lower_statement(&ast, &catalog)
}

fn build_statement_from_query_start(
    query: &str,
    query_start: &super::runtime::QueryStartState,
) -> Result<crate::query::physical_plan::PhysicalStatement, crate::gql::errors::GqlError> {
    let ast = crate::cypher::parse_statement(query)?;
    let catalog = crate::query::catalog_snapshot::CatalogSnapshotImpl::from_rows(
        &query_start.tables,
        &query_start.edges,
    )
    .map_err(|err| crate::gql::errors::GqlError::bind(ast.span(), err.to_string()))?;
    crate::cypher::lower::lower_statement(&ast, &catalog)
}
