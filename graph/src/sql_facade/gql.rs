use super::admin::{check_enabled_result, with_panic_boundary};
use super::runtime::{current_query_freshness, ensure_current_graph_for_query};
use super::*;

/// Explain how the supported read-only GQL subset binds and lowers.
#[pg_extern(schema = "graph")]
fn gql_explain(query: &str) -> String {
    with_panic_boundary("gql_explain()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let plan = build_plan(query).unwrap_or_else(|err| gql_error_to_graph_error(err).report());
        check_plan_acl(&plan);
        crate::query::explain::explain(&plan)
    })
}

/// Execute the supported read-only GQL subset and return JSONB rows.
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
        let tenant_scope = resolve_tenant_scope(None).unwrap_or_else(|err| err.report());
        let plan = build_plan(query).unwrap_or_else(|err| gql_error_to_graph_error(err).report());
        check_plan_acl(&plan);
        let matches = ENGINE
            .with(|engine| {
                crate::query::execute::execute(&engine.borrow(), &plan, tenant_scope.as_deref())
            })
            .unwrap_or_else(|err| err.report());
        let hydrated = hydrate_gql_rows(
            &matches,
            crate::query::value::requires_hydration(&plan, hydrate),
        )
        .unwrap_or_else(|err| err.report());
        let params = gql_params(params).unwrap_or_else(|err| err.report());
        let rows: Vec<_> =
            crate::query::value::project_rows(matches, &plan, &hydrated, &params, hydrate)
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

#[cfg(feature = "pg_test")]
#[pg_extern(schema = "graph", name = "_test_record_tx_edge")]
fn test_record_tx_edge(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
    edge_label: &str,
    mutation: &str,
) {
    with_panic_boundary("_test_record_tx_edge()", || {
        super::admin::require_graph_admin_result().unwrap_or_else(|err| err.report());
        let (source_idx, target_idx, type_id) = ENGINE
            .with(|engine| {
                let engine = engine.borrow();
                let source_idx = engine
                    .resolve(source_table.to_u32(), source_id)
                    .ok_or_else(|| safety::GraphError::NodeNotFound {
                        table: source_table.to_u32().to_string(),
                        pk: source_id.to_string(),
                    })?;
                let target_idx = engine
                    .resolve(target_table.to_u32(), target_id)
                    .ok_or_else(|| safety::GraphError::NodeNotFound {
                        table: target_table.to_u32().to_string(),
                        pk: target_id.to_string(),
                    })?;
                let type_id = engine
                    .edge_type_registry
                    .iter()
                    .position(|label| label == edge_label)
                    .map(|idx| idx as u8)
                    .ok_or_else(|| safety::GraphError::InvalidFilter {
                        reason: format!("unknown edge type '{edge_label}'"),
                    })?;
                Ok::<_, safety::GraphError>((source_idx, target_idx, type_id))
            })
            .unwrap_or_else(|err| err.report());

        match mutation {
            "insert" => crate::projection::tx_delta::record_added_edge(
                source_idx,
                crate::projection::tx_delta::DeltaEdge {
                    target: target_idx,
                    type_id,
                    weight: None,
                },
            ),
            "delete" => {
                crate::projection::tx_delta::record_deleted_edge(source_idx, target_idx, type_id)
            }
            other => Err(safety::GraphError::InvalidFilter {
                reason: format!(
                    "unsupported tx edge mutation '{other}'; expected insert or delete"
                ),
            }),
        }
        .unwrap_or_else(|err| err.report());
    });
}
