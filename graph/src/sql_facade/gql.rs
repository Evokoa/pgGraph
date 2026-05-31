use super::admin::{check_enabled_result, with_panic_boundary};
use super::runtime::{current_query_freshness, ensure_current_graph_for_query};
use super::*;
use crate::catalog::{primary_key_expr, read_catalog, sql_table_name_from_catalog};
use crate::quote::quote_ident;

/// Explain how the supported GQL subset binds and lowers.
#[pg_extern(schema = "graph")]
fn gql_explain(query: &str) -> String {
    with_panic_boundary("gql_explain()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let statement =
            build_statement(query).unwrap_or_else(|err| gql_error_to_graph_error(err).report());
        match statement {
            crate::query::physical_plan::PhysicalStatement::Read(plan) => {
                check_plan_acl(&plan);
                crate::query::explain::explain(&plan)
            }
            crate::query::physical_plan::PhysicalStatement::NodeScan(plan) => {
                check_node_scan_acl(&plan);
                crate::query::explain::explain_node_scan(&plan)
            }
            crate::query::physical_plan::PhysicalStatement::CreateNode(plan) => {
                check_create_acl(&plan);
                format!(
                    "CreateNode(label={}, table_oid={}, returns={})",
                    plan.label,
                    plan.table_oid,
                    plan.returns.len()
                )
            }
            crate::query::physical_plan::PhysicalStatement::SetProperty(plan) => {
                check_set_acl(&plan);
                format!(
                    "SetProperty(label={}, table_oid={}, property={}, returns={})",
                    plan.label,
                    plan.table_oid,
                    plan.property,
                    plan.returns.len()
                )
            }
            crate::query::physical_plan::PhysicalStatement::DeleteEdge(plan) => {
                check_delete_acl(&plan);
                format!(
                    "DeleteEdge(type={}, edge_table_oid={}, returns={})",
                    plan.rel_type,
                    plan.edge_table_oid,
                    plan.returns.len()
                )
            }
        }
    })
}

/// Execute the supported GQL subset and return JSONB rows.
#[pg_extern(schema = "graph", cost = 1000, volatile)]
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
        let statement =
            build_statement(query).unwrap_or_else(|err| gql_error_to_graph_error(err).report());
        let params = gql_params(params).unwrap_or_else(|err| err.report());
        let rows: Vec<_> = execute_statement(statement, tenant_scope.as_deref(), &params, hydrate)
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .map(|row| (pgrx::JsonB(row),))
            .collect();
        TableIterator::new(rows)
    })
}

fn build_statement(
    query: &str,
) -> Result<crate::query::physical_plan::PhysicalStatement, crate::gql::errors::GqlError> {
    let ast = crate::gql::parse_statement(query)?;
    let span = match &ast {
        crate::gql::ast::Statement::Read(query) => query.span,
        crate::gql::ast::Statement::Create(query) => query.span,
        crate::gql::ast::Statement::Set(query) => query.span,
        crate::gql::ast::Statement::Delete(query) => query.span,
    };
    let catalog = crate::query::catalog_snapshot::CatalogSnapshotImpl::load()
        .map_err(|err| crate::gql::errors::GqlError::bind(span, err.to_string()))?;
    let logical = crate::query::semantics::bind_statement(&ast, &catalog)?;
    Ok(crate::query::lower::lower_statement(logical))
}

fn check_plan_acl(plan: &crate::query::physical_plan::PhysicalPlan) {
    for table_oid in plan.required_table_oids() {
        acl::check_table_acl(table_oid).unwrap_or_else(|err| err.report());
    }
}

fn check_create_acl(plan: &crate::query::physical_plan::PhysicalCreateNode) {
    acl::check_table_insert_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
}

fn check_node_scan_acl(plan: &crate::query::physical_plan::PhysicalNodeScan) {
    acl::check_table_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
}

fn check_set_acl(plan: &crate::query::physical_plan::PhysicalSetProperty) {
    acl::check_table_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
    acl::check_table_update_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
}

fn check_delete_acl(plan: &crate::query::physical_plan::PhysicalDeleteEdge) {
    for table_oid in plan.required_node_table_oids() {
        acl::check_table_acl(table_oid).unwrap_or_else(|err| err.report());
    }
    acl::check_table_acl(plan.required_edge_table_oid()).unwrap_or_else(|err| err.report());
    acl::check_table_delete_acl(plan.required_edge_table_oid()).unwrap_or_else(|err| err.report());
}

fn execute_statement(
    statement: crate::query::physical_plan::PhysicalStatement,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    match statement {
        crate::query::physical_plan::PhysicalStatement::Read(plan) => {
            check_plan_acl(&plan);
            let matches = ENGINE.with(|engine| {
                crate::query::execute::execute(&engine.borrow(), &plan, tenant_scope)
            })?;
            let hydrated = hydrate_gql_rows(
                &matches,
                crate::query::value::requires_hydration(&plan, hydrate),
            )?;
            crate::query::value::project_rows(matches, &plan, &hydrated, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::NodeScan(plan) => {
            check_node_scan_acl(&plan);
            let matches = ENGINE.with(|engine| {
                crate::query::execute::execute_node_scan(&engine.borrow(), &plan, tenant_scope)
            })?;
            let hydrated = hydrate_gql_node_rows(
                &matches,
                crate::query::value::node_scan_requires_hydration(&plan, hydrate),
            )?;
            crate::query::value::project_node_rows(matches, &plan, &hydrated, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::CreateNode(plan) => {
            check_create_acl(&plan);
            execute_create_node(&plan, tenant_scope, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::SetProperty(plan) => {
            check_set_acl(&plan);
            execute_set_property(&plan, tenant_scope, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::DeleteEdge(plan) => {
            check_delete_acl(&plan);
            execute_delete_edge(&plan, tenant_scope, params, hydrate)
        }
    }
}

fn execute_create_node(
    plan: &crate::query::physical_plan::PhysicalCreateNode,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    ensure_mutable_projection("GQL CREATE")?;
    crate::projection::tx_delta::ensure_write_capacity(1, 0, 0)?;
    let insert = insert_mapped_node(plan, tenant_scope, params)?;
    crate::projection::tx_delta::record_added_node(
        plan.table_oid,
        &insert.node_id,
        insert.tenant.as_deref(),
    )?;
    Ok(vec![project_created_node(plan, insert, hydrate)])
}

fn execute_set_property(
    plan: &crate::query::physical_plan::PhysicalSetProperty,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    ensure_mutable_projection("GQL SET")?;
    crate::projection::tx_delta::ensure_write_capacity(0, 0, 0)?;
    let scan = set_property_node_scan(plan);
    let matches = ENGINE.with(|engine| {
        crate::query::execute::execute_node_scan(&engine.borrow(), &scan, tenant_scope)
    })?;
    let hydrated = hydrate_gql_node_rows(&matches, scan.predicate.is_some())?;
    let matches = crate::query::value::filter_node_rows(matches, &scan, &hydrated, params)?;
    let [row] = matches.as_slice() else {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL SET requires exactly one matched `{}` node, found {}",
                plan.label,
                matches.len()
            ),
        });
    };
    let updated = update_mapped_property(plan, &row.node.node_id, params)?;
    update_filter_index_for_property(
        plan.table_oid,
        &updated.node_id,
        &plan.property,
        &updated.row,
    )?;
    Ok(vec![project_updated_node(plan, updated, hydrate)])
}

fn set_property_node_scan(
    plan: &crate::query::physical_plan::PhysicalSetProperty,
) -> crate::query::physical_plan::PhysicalNodeScan {
    crate::query::physical_plan::PhysicalNodeScan {
        var: plan.var.clone(),
        table_oid: plan.table_oid,
        label: plan.label.clone(),
        returns: Vec::new(),
        predicate: plan.predicate.clone(),
        order_by: Vec::new(),
        skip: None,
        limit: None,
    }
}

fn execute_delete_edge(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    ensure_mutable_projection("GQL DELETE")?;
    crate::projection::tx_delta::ensure_write_capacity(
        0,
        if plan.bidirectional { 2 } else { 1 },
        0,
    )?;
    let read_plan = delete_edge_read_plan(plan);
    let matches = ENGINE.with(|engine| {
        crate::query::execute::execute(&engine.borrow(), &read_plan, tenant_scope)
    })?;
    let hydrated = hydrate_gql_rows(
        &matches,
        crate::query::value::requires_hydration(&read_plan, hydrate),
    )?;
    let matches = crate::query::value::filter_rows(matches, &read_plan, &hydrated, params)?;
    let [row] = matches.as_slice() else {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL DELETE requires exactly one matched `{}` relationship, found {}",
                plan.rel_type,
                matches.len()
            ),
        });
    };
    let matched = matched_edge_ids(row);
    let deleted = delete_mapped_edge_row(plan, &matched.source_id, &matched.target_id)?;
    record_deleted_edge_delta(plan, &deleted.source_id, &deleted.target_id)?;
    Ok(crate::query::value::project_rows(
        matches, &read_plan, &hydrated, params, hydrate,
    )?)
}

fn delete_edge_read_plan(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
) -> crate::query::physical_plan::PhysicalPlan {
    crate::query::physical_plan::PhysicalPlan {
        source_var: plan.source_var.clone(),
        source_table_oid: plan.source_table_oid,
        source_label: plan.source_label.clone(),
        rel_type: plan.rel_type.clone(),
        rel_var: Some(plan.rel_var.clone()),
        direction: plan.direction,
        hops: crate::query::logical_plan::HopBounds { min: 1, max: 1 },
        target_var: plan.target_var.clone(),
        target_table_oid: plan.target_table_oid,
        target_label: plan.target_label.clone(),
        returns: plan.returns.clone(),
        predicate: plan.predicate.clone(),
        order_by: Vec::new(),
        skip: None,
        limit: None,
    }
}

struct MatchedEdgeIds {
    source_id: String,
    target_id: String,
}

fn matched_edge_ids(row: &crate::query::execute::GqlRow) -> MatchedEdgeIds {
    MatchedEdgeIds {
        source_id: row.rel_start.node_id.clone(),
        target_id: row.rel_end.node_id.clone(),
    }
}

struct CreatedNode {
    node_id: String,
    tenant: Option<String>,
    row: serde_json::Value,
}

struct UpdatedNode {
    node_id: String,
    row: serde_json::Value,
}

fn insert_mapped_node(
    plan: &crate::query::physical_plan::PhysicalCreateNode,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<CreatedNode> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find(|table| {
            crate::catalog::table_oid_from_name(&table.table_name)
                .ok()
                .is_some_and(|oid| oid == plan.table_oid)
        })
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot insert node into unregistered table OID {}",
                plan.table_oid
            ))
        })?;
    let table_name = sql_table_name_from_catalog(&table.table_name)?;
    let insert_shape = create_insert_shape(plan, table.tenant_column.as_deref(), tenant_scope);
    let values = create_values_json(plan, &insert_shape, tenant_scope, params)?;
    let pk_expr = primary_key_expr("inserted", &table.id_columns);
    let query = format!(
        "WITH inserted AS (
             INSERT INTO {} ({})
             SELECT {}
             FROM jsonb_populate_record(NULL::{}, $1::jsonb) AS rec
             RETURNING *
         )
         SELECT to_jsonb(inserted.*), {}
         FROM inserted",
        table_name.as_sql(),
        insert_shape.columns.join(", "),
        insert_shape.selectors.join(", "),
        table_name.as_sql(),
        pk_expr
    );
    pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[pgrx::JsonB(values).into()])
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL CREATE insert failed for {}: {}",
                    table_name.as_sql(),
                    err
                ))
            })?;
        let row = rows.first();
        let row_json = row
            .get::<pgrx::JsonB>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL CREATE row read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL CREATE returned no row JSON".to_string())
            })?;
        let node_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL CREATE primary key read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL CREATE returned no primary key".to_string())
            })?;
        let tenant = table.tenant_column.as_deref().and_then(|column| {
            row_json
                .0
                .get(column)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
        Ok(CreatedNode {
            node_id,
            tenant,
            row: row_json.0,
        })
    })
}

fn update_mapped_property(
    plan: &crate::query::physical_plan::PhysicalSetProperty,
    node_id: &str,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<UpdatedNode> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find(|table| {
            crate::catalog::table_oid_from_name(&table.table_name)
                .ok()
                .is_some_and(|oid| oid == plan.table_oid)
        })
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot update node in unregistered table OID {}",
                plan.table_oid
            ))
        })?;
    let table_name = sql_table_name_from_catalog(&table.table_name)?;
    let value = write_value_json(&plan.value, params)?;
    let values = serde_json::json!({ &plan.property: value });
    let pk_expr = primary_key_expr("src", &table.id_columns);
    let query = format!(
        "WITH updated AS (
             UPDATE {} AS src
             SET {} = rec.{}
             FROM jsonb_populate_record(NULL::{}, $2::jsonb) AS rec
             WHERE {} = $1
             RETURNING to_jsonb(src.*), {}
         )
         SELECT * FROM updated",
        table_name.as_sql(),
        quote_ident(&plan.property),
        quote_ident(&plan.property),
        table_name.as_sql(),
        pk_expr,
        pk_expr
    );
    pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[node_id.into(), pgrx::JsonB(values).into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL SET update failed for {}.{}: {}",
                    table_name.as_sql(),
                    quote_ident(&plan.property),
                    err
                ),
            })?;
        if rows.is_empty() {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL SET matched node `{}` but PostgreSQL updated no row",
                    node_id
                ),
            });
        }
        let row = rows.first();
        let row_json = row
            .get::<pgrx::JsonB>(1)
            .map_err(|err| safety::GraphError::Internal(format!("GQL SET row read failed: {err}")))?
            .ok_or_else(|| safety::GraphError::Internal("GQL SET returned no row JSON".into()))?;
        let node_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL SET primary key read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL SET returned no primary key".to_string())
            })?;
        Ok(UpdatedNode {
            node_id,
            row: row_json.0,
        })
    })
}

fn delete_mapped_edge_row(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    source_id: &str,
    target_id: &str,
) -> safety::GraphResult<MatchedEdgeIds> {
    let (tables, edges, _filter_columns) = read_catalog()?;
    let edge = edges
        .iter()
        .find(|edge| {
            crate::catalog::table_oid_from_name(&edge.from_table)
                .ok()
                .is_some_and(|oid| oid == plan.edge_table_oid)
                && edge.from_column == plan.source_column
                && edge.to_column == plan.target_column
                && edge.label == plan.rel_type
        })
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot delete unregistered edge row for table OID {}",
                plan.edge_table_oid
            ))
        })?;
    if tables
        .iter()
        .any(|table| table.table_name == edge.from_table)
    {
        return Err(safety::GraphError::UnsupportedOperation {
            operation: "GQL DELETE".to_string(),
            reason: "DELETE requires a relationship backed by a registered edge row table"
                .to_string(),
        });
    }
    let table_name = sql_table_name_from_catalog(&edge.from_table)?;
    if plan.bidirectional
        && plan.edge_source_table_oid == plan.edge_target_table_oid
        && source_id != target_id
    {
        return delete_bidirectional_self_edge_row(plan, table_name.as_sql(), source_id, target_id);
    }
    match try_delete_mapped_edge_row(plan, table_name.as_sql(), source_id, target_id)? {
        DeleteEdgeRowResult::Deleted => {
            return Ok(MatchedEdgeIds {
                source_id: source_id.to_string(),
                target_id: target_id.to_string(),
            });
        }
        DeleteEdgeRowResult::NoRow if plan.bidirectional => {}
        DeleteEdgeRowResult::NoRow => {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL DELETE matched `{}` relationship but PostgreSQL found no edge row",
                    plan.rel_type
                ),
            });
        }
    }
    match try_delete_mapped_edge_row(plan, table_name.as_sql(), target_id, source_id)? {
        DeleteEdgeRowResult::Deleted => Ok(MatchedEdgeIds {
            source_id: target_id.to_string(),
            target_id: source_id.to_string(),
        }),
        DeleteEdgeRowResult::NoRow => Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL DELETE matched `{}` relationship but PostgreSQL found no edge row",
                plan.rel_type
            ),
        }),
    }
}

fn delete_bidirectional_self_edge_row(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    table_sql: &str,
    source_id: &str,
    target_id: &str,
) -> safety::GraphResult<MatchedEdgeIds> {
    let forward = count_mapped_edge_rows(plan, table_sql, source_id, target_id)?;
    let reverse = count_mapped_edge_rows(plan, table_sql, target_id, source_id)?;
    match (forward, reverse) {
        (1, 0) => {
            let deleted = try_delete_mapped_edge_row(plan, table_sql, source_id, target_id)?;
            if matches!(deleted, DeleteEdgeRowResult::Deleted) {
                Ok(MatchedEdgeIds {
                    source_id: source_id.to_string(),
                    target_id: target_id.to_string(),
                })
            } else {
                Err(edge_row_missing(plan))
            }
        }
        (0, 1) => {
            let deleted = try_delete_mapped_edge_row(plan, table_sql, target_id, source_id)?;
            if matches!(deleted, DeleteEdgeRowResult::Deleted) {
                Ok(MatchedEdgeIds {
                    source_id: target_id.to_string(),
                    target_id: source_id.to_string(),
                })
            } else {
                Err(edge_row_missing(plan))
            }
        }
        (0, 0) => Err(edge_row_missing(plan)),
        _ => Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL DELETE requires exactly one mapped `{}` edge row across bidirectional self-edge orientations, found {}",
                plan.rel_type,
                forward + reverse
            ),
        }),
    }
}

enum DeleteEdgeRowResult {
    Deleted,
    NoRow,
}

fn count_mapped_edge_rows(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    table_sql: &str,
    source_id: &str,
    target_id: &str,
) -> safety::GraphResult<i64> {
    let query = format!(
        "SELECT count(*)::bigint
         FROM {} AS e
         WHERE e.{}::text = $1
           AND e.{}::text = $2",
        table_sql,
        quote_ident(&plan.source_column),
        quote_ident(&plan.target_column),
    );
    pgrx::Spi::connect(|client| {
        client
            .select(&query, None, &[source_id.into(), target_id.into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL DELETE edge row lookup failed for {}: {}",
                    table_sql, err
                ),
            })?
            .first()
            .get::<i64>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL DELETE edge row lookup count read failed: {err}"
                ))
            })
            .map(|count| count.unwrap_or_default())
    })
}

fn edge_row_missing(plan: &crate::query::physical_plan::PhysicalDeleteEdge) -> safety::GraphError {
    safety::GraphError::GqlExecution {
        reason: format!(
            "GQL DELETE matched `{}` relationship but PostgreSQL found no edge row",
            plan.rel_type
        ),
    }
}

fn try_delete_mapped_edge_row(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    table_sql: &str,
    source_id: &str,
    target_id: &str,
) -> safety::GraphResult<DeleteEdgeRowResult> {
    let query = format!(
        "WITH candidates AS (
             SELECT e.ctid
             FROM {} AS e
             WHERE e.{}::text = $1
               AND e.{}::text = $2
             LIMIT 2
         ),
         deleted AS (
             DELETE FROM {} AS e
             USING candidates c
             WHERE e.ctid = c.ctid
               AND (SELECT count(*) FROM candidates) = 1
             RETURNING 1
         )
         SELECT
             (SELECT count(*) FROM candidates)::bigint,
             (SELECT count(*) FROM deleted)::bigint",
        table_sql,
        quote_ident(&plan.source_column),
        quote_ident(&plan.target_column),
        table_sql
    );
    pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[source_id.into(), target_id.into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!("GQL DELETE edge row failed for {}: {}", table_sql, err),
            })?;
        let row = rows.first();
        let candidates = row
            .get::<i64>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL DELETE candidate count read failed: {err}"
                ))
            })?
            .unwrap_or_default();
        let deleted = row
            .get::<i64>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL DELETE row count read failed: {err}"))
            })?
            .unwrap_or_default();
        match (candidates, deleted) {
            (1, 1) => Ok(DeleteEdgeRowResult::Deleted),
            (0, _) => Ok(DeleteEdgeRowResult::NoRow),
            (count, _) => Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL DELETE requires exactly one mapped `{}` edge row, found {}",
                    plan.rel_type, count
                ),
            }),
        }
    })
}

fn record_deleted_edge_delta(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    source_id: &str,
    target_id: &str,
) -> safety::GraphResult<()> {
    let (source, target, type_id) = ENGINE.with(|engine| {
        let engine = engine.borrow();
        let source = engine
            .resolve(plan.edge_source_table_oid, source_id)
            .ok_or_else(|| safety::GraphError::GqlExecution {
                reason: format!("GQL DELETE source node `{source_id}` is not in the built graph"),
            })?;
        let target = engine
            .resolve(plan.edge_target_table_oid, target_id)
            .ok_or_else(|| safety::GraphError::GqlExecution {
                reason: format!("GQL DELETE target node `{target_id}` is not in the built graph"),
            })?;
        let type_id = engine
            .edge_type_registry
            .iter()
            .position(|label| label == &plan.rel_type)
            .map(|idx| idx as u8)
            .ok_or_else(|| safety::GraphError::GqlExecution {
                reason: format!(
                    "relationship type `{}` is not present in the built graph",
                    plan.rel_type
                ),
            })?;
        Ok::<_, safety::GraphError>((source, target, type_id))
    })?;
    crate::projection::tx_delta::record_deleted_edge(source, target, type_id)?;
    if plan.bidirectional {
        crate::projection::tx_delta::record_deleted_edge(target, source, type_id)?;
    }
    Ok(())
}

fn write_value_json(
    value: &crate::query::physical_plan::CreateValueSlot,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<serde_json::Value> {
    match value {
        crate::query::physical_plan::CreateValueSlot::Literal(value) => Ok(value.clone()),
        crate::query::physical_plan::CreateValueSlot::Param(name) => params
            .get(name)
            .cloned()
            .ok_or_else(|| safety::GraphError::GqlParameter {
                reason: format!("missing GQL parameter `{name}`"),
            }),
    }
}

struct CreateInsertShape {
    columns: Vec<String>,
    selectors: Vec<String>,
    tenant_column: Option<String>,
}

fn create_insert_shape(
    plan: &crate::query::physical_plan::PhysicalCreateNode,
    tenant_column: Option<&str>,
    tenant_scope: Option<&str>,
) -> CreateInsertShape {
    let mut columns =
        Vec::with_capacity(plan.properties.len() + usize::from(tenant_scope.is_some()));
    let mut selectors = Vec::with_capacity(columns.capacity());
    for property in &plan.properties {
        columns.push(quote_ident(&property.property));
        selectors.push(format!("rec.{}", quote_ident(&property.property)));
    }
    let tenant_column = match (tenant_column, tenant_scope) {
        (Some(column), Some(_))
            if plan
                .properties
                .iter()
                .any(|property| property.property == column) =>
        {
            Some(column.to_string())
        }
        (Some(column), Some(_)) => {
            columns.push(quote_ident(column));
            selectors.push(format!("rec.{}", quote_ident(column)));
            Some(column.to_string())
        }
        _ => None,
    };
    CreateInsertShape {
        columns,
        selectors,
        tenant_column,
    }
}

fn ensure_mutable_projection(operation: &str) -> safety::GraphResult<()> {
    ENGINE.with(|engine| {
        let engine = engine.borrow();
        if engine.projection_mode == config::ProjectionMode::MutableOverlay {
            Ok(())
        } else {
            Err(safety::GraphError::UnsupportedOperation {
                operation: operation.to_string(),
                reason: "mapped writes require a mutable_overlay projection".to_string(),
            })
        }
    })
}

fn create_values_json(
    plan: &crate::query::physical_plan::PhysicalCreateNode,
    insert_shape: &CreateInsertShape,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<serde_json::Value> {
    let mut values = serde_json::Map::with_capacity(plan.properties.len());
    for property in &plan.properties {
        let value = match &property.value {
            crate::query::physical_plan::CreateValueSlot::Literal(value) => value.clone(),
            crate::query::physical_plan::CreateValueSlot::Param(name) => params
                .get(name)
                .cloned()
                .ok_or_else(|| safety::GraphError::GqlParameter {
                    reason: format!("missing GQL parameter `{name}`"),
                })?,
        };
        values.insert(property.property.clone(), value);
    }
    if let Some(tenant_column) = &insert_shape.tenant_column {
        let tenant_scope = tenant_scope.unwrap_or_default();
        match values.get(tenant_column) {
            Some(serde_json::Value::String(value)) if value == &tenant_scope => {}
            Some(_) => {
                return Err(safety::GraphError::InvalidFilter {
                    reason: format!(
                        "GQL CREATE tenant property `{tenant_column}` must match the active tenant scope"
                    ),
                });
            }
            None => {
                values.insert(
                    tenant_column.clone(),
                    serde_json::Value::String(tenant_scope.to_string()),
                );
            }
        }
    }
    Ok(serde_json::Value::Object(values))
}

fn project_created_node(
    plan: &crate::query::physical_plan::PhysicalCreateNode,
    created: CreatedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            crate::query::physical_plan::CreateReturnSlot::Node { name } => {
                output.insert(name.clone(), created_node_value(plan, &created, hydrate));
            }
            crate::query::physical_plan::CreateReturnSlot::Property { property, name } => {
                output.insert(
                    name.clone(),
                    created
                        .row
                        .get(property)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
            }
        }
    }
    serde_json::Value::Object(output)
}

fn project_updated_node(
    plan: &crate::query::physical_plan::PhysicalSetProperty,
    updated: UpdatedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            crate::query::physical_plan::CreateReturnSlot::Node { name } => {
                output.insert(name.clone(), updated_node_value(plan, &updated, hydrate));
            }
            crate::query::physical_plan::CreateReturnSlot::Property { property, name } => {
                output.insert(
                    name.clone(),
                    updated
                        .row
                        .get(property)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
            }
        }
    }
    serde_json::Value::Object(output)
}

fn created_node_value(
    plan: &crate::query::physical_plan::PhysicalCreateNode,
    created: &CreatedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut node = if hydrate {
        created.row.as_object().cloned().unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    node.insert(
        "_id".to_string(),
        serde_json::json!({
            "table": &plan.label,
            "id": &created.node_id,
        }),
    );
    node.insert(
        "_labels".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(plan.label.clone())]),
    );
    serde_json::Value::Object(node)
}

fn updated_node_value(
    plan: &crate::query::physical_plan::PhysicalSetProperty,
    updated: &UpdatedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut node = if hydrate {
        updated.row.as_object().cloned().unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    node.insert(
        "_id".to_string(),
        serde_json::json!({
            "table": &plan.label,
            "id": &updated.node_id,
        }),
    );
    node.insert(
        "_labels".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(plan.label.clone())]),
    );
    serde_json::Value::Object(node)
}

fn update_filter_index_for_property(
    table_oid: u32,
    node_id: &str,
    property: &str,
    row: &serde_json::Value,
) -> safety::GraphResult<()> {
    ENGINE.with(|engine| {
        let mut engine = engine.borrow_mut();
        let Some(node_idx) = engine.resolve(table_oid, node_id) else {
            return Ok(());
        };
        let Some(column_idx) = engine
            .filter_index
            .columns
            .iter()
            .position(|column| column.table_oid == table_oid && column.column_name == property)
        else {
            return Ok(());
        };
        let value =
            encode_filter_value_from_json(row.get(property), &mut engine.filter_index, column_idx)?;
        crate::projection::tx_delta::record_filter_value_update(column_idx, node_idx, value)
    })
}

fn encode_filter_value_from_json(
    raw: Option<&serde_json::Value>,
    filter_index: &mut crate::filter_index::FilterIndex,
    column_idx: usize,
) -> safety::GraphResult<Option<crate::filter_index::EncodedFilterValue>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(column_type) = filter_index.column_type(column_idx) else {
        return Ok(None);
    };
    match column_type {
        crate::filter_index::FilterColumnType::Numeric => {
            Ok(Some(crate::filter_index::EncodedFilterValue::Numeric(
                crate::sql_filters::jsonb_filter_i64(raw)?,
            )))
        }
        crate::filter_index::FilterColumnType::Boolean => {
            Ok(Some(crate::filter_index::EncodedFilterValue::Boolean(
                crate::sql_filters::jsonb_filter_bool(raw)?,
            )))
        }
        crate::filter_index::FilterColumnType::Text => {
            let value = crate::sql_filters::jsonb_filter_text(raw)?;
            let token = filter_index.intern_text_value(column_idx, &value);
            Ok(Some(crate::filter_index::EncodedFilterValue::Text(token)))
        }
        crate::filter_index::FilterColumnType::Date => {
            Ok(Some(crate::filter_index::EncodedFilterValue::Date(
                crate::sql_filters::encode_date_filter_value(raw)?,
            )))
        }
        crate::filter_index::FilterColumnType::Timestamptz => {
            Ok(Some(crate::filter_index::EncodedFilterValue::Timestamptz(
                crate::sql_filters::encode_timestamptz_filter_value(raw)?,
            )))
        }
        crate::filter_index::FilterColumnType::Uuid => {
            Ok(Some(crate::filter_index::EncodedFilterValue::Uuid(
                crate::sql_filters::jsonb_filter_uuid(raw)?,
            )))
        }
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

fn hydrate_gql_node_rows(
    rows: &[crate::query::execute::GqlNodeRow],
    needed: bool,
) -> safety::GraphResult<crate::query::value::HydratedRows> {
    let mut hydrated = crate::query::value::HydratedRows::new();
    if !needed {
        return Ok(hydrated);
    }
    for row in rows {
        let key = (row.node.table_oid, row.node.node_id.clone());
        if hydrated.contains_key(&key) {
            continue;
        }
        let node = hydrate_node(row.node.table_oid, &row.node.node_id)?
            .map(|json| json.0)
            .unwrap_or(serde_json::Value::Null);
        hydrated.insert(key, node);
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
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
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
