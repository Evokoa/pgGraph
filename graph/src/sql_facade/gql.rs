use super::admin::{check_enabled_result, with_panic_boundary};
use super::runtime::{current_query_freshness, ensure_current_graph_for_query};
use super::*;

/// Development-only GQL plan inspection.
#[pg_extern(schema = "graph")]
fn gql_explain(query: &str) -> String {
    with_panic_boundary("gql_explain()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let plan = build_plan(query).unwrap_or_else(|err| gql_error_to_graph_error(err).report());
        check_plan_acl(&plan);
        crate::query::explain::explain(&plan)
    })
}

/// Development-only coordinate-returning GQL execution.
#[pg_extern(schema = "graph", cost = 1000)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes tuple row columns"
)]
fn gql(
    query: &str,
    params: default!(Option<pgrx::JsonB>, "NULL"),
    hydrate: default!(bool, "true"),
) -> TableIterator<'static, (name!(row, pgrx::JsonB),)> {
    with_panic_boundary("gql()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let plan = build_plan(query).unwrap_or_else(|err| gql_error_to_graph_error(err).report());
        check_plan_acl(&plan);
        let matches = ENGINE
            .with(|engine| crate::query::execute::execute(&engine.borrow(), &plan))
            .unwrap_or_else(|err| err.report());
        let hydrated = hydrate_gql_rows(
            &matches,
            crate::query::value::requires_hydration(&plan, hydrate),
        )
        .unwrap_or_else(|err| err.report());
        let params = gql_params(params).unwrap_or_else(|err| err.report());
        let rows = crate::query::value::project_rows(matches, &plan, &hydrated, &params, hydrate)
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .map(|row| (pgrx::JsonB(row),))
            .collect();
        TableIterator::new(rows)
    })
}

fn build_plan(
    query: &str,
) -> Result<crate::query::physical_plan::PhysicalPlan, crate::gql::errors::GqlError> {
    let ast = crate::gql::parse(query)?;
    let catalog = crate::query::catalog_snapshot::CatalogSnapshotImpl::load()
        .map_err(|err| crate::gql::errors::GqlError::bind(ast.span, err.to_string()))?;
    let logical = crate::query::semantics::bind(&ast, &catalog)?;
    Ok(crate::query::lower::lower(logical))
}

fn check_plan_acl(plan: &crate::query::physical_plan::PhysicalPlan) {
    for table_oid in plan.required_table_oids() {
        acl::check_table_acl(table_oid).unwrap_or_else(|err| err.report());
    }
}

fn gql_error_to_graph_error(err: crate::gql::errors::GqlError) -> safety::GraphError {
    match &err.kind {
        crate::gql::errors::GqlErrorKind::Syntax { .. } => safety::GraphError::GqlSyntax {
            reason: err.to_string(),
        },
        crate::gql::errors::GqlErrorKind::Unsupported { .. } => {
            safety::GraphError::GqlUnsupported {
                reason: err.to_string(),
            }
        }
        crate::gql::errors::GqlErrorKind::Bind { .. } => safety::GraphError::GqlSemantic {
            reason: err.to_string(),
        },
    }
}

fn gql_params(
    params: Option<pgrx::JsonB>,
) -> safety::GraphResult<crate::query::value::QueryParams> {
    match params.map(|json| json.0) {
        Some(serde_json::Value::Object(map)) => Ok(map),
        Some(_) => Err(safety::GraphError::GqlParameter {
            reason: "GQL params must be a JSON object".to_string(),
        }),
        None => Ok(serde_json::Map::new()),
    }
}

fn hydrate_gql_rows(
    rows: &[crate::query::execute::GqlRow],
    needed: bool,
) -> safety::GraphResult<crate::query::value::HydratedRows> {
    let mut hydrated = crate::query::value::HydratedRows::new();
    if !needed {
        return Ok(hydrated);
    }
    for row in rows {
        for coordinate in [&row.source, &row.target] {
            let key = (coordinate.table_oid, coordinate.node_id.clone());
            if hydrated.contains_key(&key) {
                continue;
            }
            let node = hydrate_node(coordinate.table_oid, &coordinate.node_id)?
                .map(|json| json.0)
                .unwrap_or(serde_json::Value::Null);
            hydrated.insert(key, node);
        }
    }
    Ok(hydrated)
}
