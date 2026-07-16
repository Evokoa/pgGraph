use super::admin::{check_enabled_result, with_panic_boundary};
use super::runtime::{current_query_freshness, ensure_current_graph_for_query};
use super::*;
use crate::catalog::{
    primary_key_expr, read_catalog, sql_table_name_from_oid, validate_column_exists,
};
use crate::quote::{quote_ident, quote_literal};

/// Explain how the supported GQL subset binds and lowers.
#[pg_extern(schema = "graph")]
fn gql_explain(query: &str) -> String {
    with_panic_boundary("gql_explain()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let statement =
            build_statement(query).unwrap_or_else(|err| gql_error_to_graph_error(err).report());
        explain_statement(statement)
    })
}

pub(super) fn explain_statement(
    statement: crate::query::physical_plan::PhysicalStatement,
) -> String {
    match statement {
        crate::query::physical_plan::PhysicalStatement::Read(plan) => {
            check_plan_acl(&plan);
            crate::query::explain::explain(&plan)
        }
        crate::query::physical_plan::PhysicalStatement::NodeScan(plan) => {
            check_node_scan_acl(&plan);
            crate::query::explain::explain_node_scan(&plan)
        }
        crate::query::physical_plan::PhysicalStatement::JoinRead(plan) => {
            check_join_acl(&plan);
            format!(
                "JoinExpand(patterns={}, nodes={}, return={})",
                plan.patterns.len(),
                plan.node_slots.len(),
                plan.returns.len()
            )
        }
        crate::query::physical_plan::PhysicalStatement::WildcardPathRead(plan) => {
            check_wildcard_path_acl(&plan);
            format!(
                "WildcardPathExpand(path={}, direction={:?}, rel=*, hops=1..1, return={})",
                plan.path_var,
                plan.direction,
                plan.returns.len()
            )
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
        crate::query::physical_plan::PhysicalStatement::CreateRelationship(plan) => {
            check_create_relationship_acl(&plan);
            format!(
                "CreateRelationship(type={}, edge_table_oid={}, properties={}, returns={})",
                plan.rel_type,
                plan.edge_table_oid,
                plan.properties.len(),
                plan.returns.len()
            )
        }
        crate::query::physical_plan::PhysicalStatement::MergeNode(plan) => {
            check_merge_acl(&plan);
            format!(
                "MergeNode(label={}, table_oid={}, properties={}, on_create={}, on_match={}, returns={})",
                plan.label,
                plan.table_oid,
                plan.properties.len(),
                plan.on_create.is_some(),
                plan.on_match.is_some(),
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
        crate::query::physical_plan::PhysicalStatement::RemoveProperty(plan) => {
            check_remove_acl(&plan);
            format!(
                "RemoveProperty(label={}, table_oid={}, property={}, returns={})",
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
        crate::query::physical_plan::PhysicalStatement::DetachDeleteNode(plan) => {
            check_detach_delete_acl(&plan);
            format!(
                "DetachDeleteNode(label={}, table_oid={}, incident_edge_tables={}, returns={})",
                plan.label,
                plan.table_oid,
                plan.required_edge_table_oids().len(),
                plan.returns.len()
            )
        }
    }
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
        let governor = gql_query_governor().unwrap_or_else(|err| err.report());
        let rows: Vec<_> = execute_statement_governed(
            statement,
            tenant_scope.as_deref(),
            &params,
            hydrate,
            &governor,
        )
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
        crate::gql::ast::Statement::CreateRelationship(query) => query.span,
        crate::gql::ast::Statement::Merge(query) => query.span,
        crate::gql::ast::Statement::Set(query) => query.span,
        crate::gql::ast::Statement::Remove(query) => query.span,
        crate::gql::ast::Statement::Delete(query) => query.span,
        crate::gql::ast::Statement::DetachDelete(query) => query.span,
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
    if let Some(edge_mapping) = &plan.edge_mapping {
        acl::check_table_acl(edge_mapping.edge_table_oid).unwrap_or_else(|err| err.report());
    }
}

fn check_create_acl(plan: &crate::query::physical_plan::PhysicalCreateNode) {
    acl::check_table_insert_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
}

fn check_create_relationship_acl(plan: &crate::query::physical_plan::PhysicalCreateRelationship) {
    for table_oid in plan.required_node_table_oids() {
        acl::check_table_acl(table_oid).unwrap_or_else(|err| err.report());
    }
    acl::check_table_acl(plan.required_edge_table_oid()).unwrap_or_else(|err| err.report());
    acl::check_table_insert_acl(plan.required_edge_table_oid()).unwrap_or_else(|err| err.report());
}

fn check_merge_acl(plan: &crate::query::physical_plan::PhysicalMergeNode) {
    acl::check_table_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
    acl::check_table_insert_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
    if plan.on_match.is_some() {
        acl::check_table_update_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
    }
}

fn check_node_scan_acl(plan: &crate::query::physical_plan::PhysicalNodeScan) {
    acl::check_table_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
}

fn check_join_acl(plan: &crate::query::physical_plan::PhysicalJoinPlan) {
    for table_oid in plan.required_table_oids() {
        acl::check_table_acl(table_oid).unwrap_or_else(|err| err.report());
    }
    let edge_table_oids = plan
        .patterns
        .iter()
        .filter_map(|pattern| pattern.edge_mapping.as_ref())
        .map(|mapping| mapping.edge_table_oid)
        .collect::<std::collections::BTreeSet<_>>();
    for table_oid in edge_table_oids {
        acl::check_table_acl(table_oid).unwrap_or_else(|err| err.report());
    }
}

fn check_wildcard_path_acl(plan: &crate::query::physical_plan::PhysicalWildcardPathPlan) {
    for table_oid in &plan.required_node_table_oids {
        acl::check_table_acl(*table_oid).unwrap_or_else(|err| err.report());
    }
    let edge_table_oids = plan
        .edge_mappings_by_id
        .values()
        .map(|mapping| mapping.edge_table_oid)
        .collect::<std::collections::BTreeSet<_>>();
    for table_oid in edge_table_oids {
        acl::check_table_acl(table_oid).unwrap_or_else(|err| err.report());
    }
}

fn check_set_acl(plan: &crate::query::physical_plan::PhysicalSetProperty) {
    acl::check_table_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
    acl::check_table_update_acl(plan.required_table_oid()).unwrap_or_else(|err| err.report());
}

fn check_remove_acl(plan: &crate::query::physical_plan::PhysicalRemoveProperty) {
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

fn check_detach_delete_acl(plan: &crate::query::physical_plan::PhysicalDetachDeleteNode) {
    acl::check_table_acl(plan.required_node_table_oid()).unwrap_or_else(|err| err.report());
    acl::check_table_delete_acl(plan.required_node_table_oid()).unwrap_or_else(|err| err.report());
    for table_oid in plan.required_edge_table_oids() {
        acl::check_table_acl(table_oid).unwrap_or_else(|err| err.report());
        acl::check_table_delete_acl(table_oid).unwrap_or_else(|err| err.report());
    }
}

pub(super) fn execute_statement(
    statement: crate::query::physical_plan::PhysicalStatement,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    let governor = gql_query_governor()?;
    execute_statement_governed(statement, tenant_scope, params, hydrate, &governor)
}

fn gql_query_governor() -> safety::GraphResult<crate::resource::ResourceGovernor> {
    let resident = ENGINE
        .with(|engine| {
            crate::resource::ByteCount::from_usize(engine.borrow().estimated_memory_used_bytes())
        })
        .ok_or_else(|| {
            safety::GraphError::Internal("engine residency does not fit u64".to_string())
        })?;
    Ok(crate::resource::query_governor(resident))
}

fn execute_statement_governed(
    statement: crate::query::physical_plan::PhysicalStatement,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    match statement {
        crate::query::physical_plan::PhysicalStatement::Read(plan) => {
            check_plan_acl(&plan);
            let matches = ENGINE.with(|engine| {
                crate::query::execute::execute_governed(
                    &engine.borrow(),
                    &plan,
                    tenant_scope,
                    governor,
                )
            })?;
            ensure_gql_rows_visible(&matches, governor)?;
            ensure_gql_relationship_rows_visible(&matches, &plan, governor)?;
            let hydrated = hydrate_gql_rows_governed(
                &matches,
                crate::query::value::requires_hydration(&plan, hydrate),
                governor,
            )?;
            let hydrated_relationships =
                hydrate_gql_relationship_rows_governed(&matches, &plan, hydrate, governor)?;
            crate::query::value::project_rows_with_relationships_governed(
                matches,
                &plan,
                &hydrated,
                &hydrated_relationships,
                params,
                hydrate,
                governor,
            )
        }
        crate::query::physical_plan::PhysicalStatement::NodeScan(plan) => {
            check_node_scan_acl(&plan);
            let matches = ENGINE.with(|engine| {
                crate::query::execute::execute_node_scan_governed(
                    &engine.borrow(),
                    &plan,
                    tenant_scope,
                    params,
                    governor,
                )
            })?;
            ensure_gql_node_rows_visible(&matches, governor)?;
            let hydrated = hydrate_gql_node_rows_governed(
                &matches,
                crate::query::value::node_scan_requires_hydration(&plan, hydrate),
                governor,
            )?;
            crate::query::value::project_node_rows_governed(
                matches, &plan, &hydrated, params, hydrate, governor,
            )
        }
        crate::query::physical_plan::PhysicalStatement::JoinRead(plan) => {
            check_join_acl(&plan);
            let matches = ENGINE.with(|engine| {
                crate::query::execute::execute_join_governed(
                    &engine.borrow(),
                    &plan,
                    tenant_scope,
                    governor,
                )
            })?;
            ensure_gql_rows_visible(&matches, governor)?;
            ensure_gql_join_relationship_rows_visible(&matches, &plan, governor)?;
            let hydrated = hydrate_gql_rows_governed(
                &matches,
                crate::query::value::join_requires_hydration(&plan, hydrate),
                governor,
            )?;
            crate::query::value::project_join_rows_governed(
                matches, &plan, &hydrated, params, hydrate, governor,
            )
        }
        crate::query::physical_plan::PhysicalStatement::WildcardPathRead(plan) => {
            check_wildcard_path_acl(&plan);
            let matches = ENGINE.with(|engine| {
                crate::query::execute::execute_wildcard_path_governed(
                    &engine.borrow(),
                    &plan,
                    tenant_scope,
                    governor,
                )
            })?;
            ensure_gql_rows_visible(&matches, governor)?;
            ensure_gql_wildcard_relationship_rows_visible(&matches, &plan, governor)?;
            let hydrated = hydrate_gql_rows_governed(
                &matches,
                crate::query::value::wildcard_path_requires_hydration(&plan, hydrate),
                governor,
            )?;
            crate::query::value::project_wildcard_path_rows_governed(
                matches, &plan, &hydrated, params, hydrate, governor,
            )
        }
        crate::query::physical_plan::PhysicalStatement::CreateNode(plan) => {
            check_create_acl(&plan);
            execute_create_node(&plan, tenant_scope, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::CreateRelationship(plan) => {
            check_create_relationship_acl(&plan);
            execute_create_relationship(&plan, tenant_scope, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::MergeNode(plan) => {
            check_merge_acl(&plan);
            execute_merge_node(&plan, tenant_scope, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::SetProperty(plan) => {
            check_set_acl(&plan);
            execute_set_property(&plan, tenant_scope, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::RemoveProperty(plan) => {
            check_remove_acl(&plan);
            execute_remove_property(&plan, tenant_scope, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::DeleteEdge(plan) => {
            check_delete_acl(&plan);
            execute_delete_edge(&plan, tenant_scope, params, hydrate)
        }
        crate::query::physical_plan::PhysicalStatement::DetachDeleteNode(plan) => {
            check_detach_delete_acl(&plan);
            execute_detach_delete_node(&plan, tenant_scope, params, hydrate)
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
    let base_node_count = ENGINE.with(|engine| engine.borrow().node_store.node_count());
    crate::projection::tx_delta::record_added_node_indexed(
        plan.table_oid,
        &insert.node_id,
        insert.tenant.as_deref(),
        base_node_count,
    )?;
    Ok(vec![project_created_node(plan, insert, hydrate)])
}

fn execute_create_relationship(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    ensure_mutable_projection("GQL relationship CREATE")?;
    crate::projection::tx_delta::ensure_write_capacity(
        0,
        if plan.bidirectional { 2 } else { 1 },
        0,
    )?;
    let source_scan = create_relationship_node_scan(plan, true);
    let target_scan = create_relationship_node_scan(plan, false);
    let source = matched_create_relationship_endpoint(&source_scan, tenant_scope, params)?;
    let target = matched_create_relationship_endpoint(&target_scan, tenant_scope, params)?;
    lock_and_recheck_node_write(
        plan.source_table_oid,
        &plan.source_label,
        &plan.source_predicate,
        &source.node.node_id,
        params,
        tenant_scope,
        "GQL relationship CREATE",
    )?;
    if plan.source_var != plan.target_var || source.node.node_id != target.node.node_id {
        lock_and_recheck_node_write(
            plan.target_table_oid,
            &plan.target_label,
            &plan.target_predicate,
            &target.node.node_id,
            params,
            tenant_scope,
            "GQL relationship CREATE",
        )?;
    }
    let created =
        insert_mapped_relationship(plan, &source.node.node_id, &target.node.node_id, params)?;
    record_added_relationship_delta(
        plan,
        &source.node.node_id,
        &target.node.node_id,
        &created.source_key,
        tenant_scope,
    )?;
    project_created_relationship(
        plan,
        &source.node.node_id,
        &target.node.node_id,
        created,
        hydrate,
    )
}

fn execute_merge_node(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    ensure_mutable_projection("GQL MERGE")?;
    let merged = merge_mapped_node(plan, tenant_scope, params)?;
    if merged.created {
        let base_node_count = ENGINE.with(|engine| engine.borrow().node_store.node_count());
        crate::projection::tx_delta::record_added_node_indexed(
            plan.table_oid,
            &merged.node_id,
            merged.tenant.as_deref(),
            base_node_count,
        )?;
    } else if let Some(on_match) = &plan.on_match {
        update_filter_index_for_property(
            plan.table_oid,
            &merged.node_id,
            &on_match.property,
            &merged.row,
        )?;
    }
    Ok(vec![project_merged_node(plan, merged, hydrate)])
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
        crate::query::execute::execute_node_scan(&engine.borrow(), &scan, tenant_scope, params)
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
    let updated = update_mapped_property(plan, &row.node.node_id, params, tenant_scope)?;
    update_filter_index_for_property(
        plan.table_oid,
        &updated.node_id,
        &plan.property,
        &updated.row,
    )?;
    Ok(vec![project_updated_node(plan, updated, hydrate)])
}

fn execute_remove_property(
    plan: &crate::query::physical_plan::PhysicalRemoveProperty,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    ensure_mutable_projection("GQL REMOVE")?;
    crate::projection::tx_delta::ensure_write_capacity(0, 0, 0)?;
    let scan = remove_property_node_scan(plan);
    let matches = ENGINE.with(|engine| {
        crate::query::execute::execute_node_scan(&engine.borrow(), &scan, tenant_scope, params)
    })?;
    let hydrated = hydrate_gql_node_rows(&matches, scan.predicate.is_some())?;
    let matches = crate::query::value::filter_node_rows(matches, &scan, &hydrated, params)?;
    let [row] = matches.as_slice() else {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL REMOVE requires exactly one matched `{}` node, found {}",
                plan.label,
                matches.len()
            ),
        });
    };
    let updated = remove_mapped_property(plan, &row.node.node_id, params, tenant_scope)?;
    update_filter_index_for_property(
        plan.table_oid,
        &updated.node_id,
        &plan.property,
        &updated.row,
    )?;
    Ok(vec![project_removed_node(plan, updated, hydrate)])
}

fn set_property_node_scan(
    plan: &crate::query::physical_plan::PhysicalSetProperty,
) -> crate::query::physical_plan::PhysicalNodeScan {
    crate::query::physical_plan::PhysicalNodeScan {
        optional: false,
        var: plan.var.clone(),
        table_oid: plan.table_oid,
        label: plan.label.clone(),
        returns: Vec::new(),
        distinct_stages: Vec::new(),
        distinct: false,
        predicate: plan.predicate.clone(),
        identity_lookup: None,
        order_by: Vec::new(),
        skip: None,
        limit: None,
    }
}

fn remove_property_node_scan(
    plan: &crate::query::physical_plan::PhysicalRemoveProperty,
) -> crate::query::physical_plan::PhysicalNodeScan {
    crate::query::physical_plan::PhysicalNodeScan {
        optional: false,
        var: plan.var.clone(),
        table_oid: plan.table_oid,
        label: plan.label.clone(),
        returns: Vec::new(),
        distinct_stages: Vec::new(),
        distinct: false,
        predicate: plan.predicate.clone(),
        identity_lookup: None,
        order_by: Vec::new(),
        skip: None,
        limit: None,
    }
}

fn detach_delete_node_scan(
    plan: &crate::query::physical_plan::PhysicalDetachDeleteNode,
) -> crate::query::physical_plan::PhysicalNodeScan {
    crate::query::physical_plan::PhysicalNodeScan {
        optional: false,
        var: plan.var.clone(),
        table_oid: plan.table_oid,
        label: plan.label.clone(),
        returns: Vec::new(),
        distinct_stages: Vec::new(),
        distinct: false,
        predicate: plan.predicate.clone(),
        identity_lookup: None,
        order_by: Vec::new(),
        skip: None,
        limit: None,
    }
}

fn execute_detach_delete_node(
    plan: &crate::query::physical_plan::PhysicalDetachDeleteNode,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    ensure_mutable_projection("GQL DETACH DELETE")?;
    let scan = detach_delete_node_scan(plan);
    let matches = ENGINE.with(|engine| {
        crate::query::execute::execute_node_scan(&engine.borrow(), &scan, tenant_scope, params)
    })?;
    let hydrated = hydrate_gql_node_rows(&matches, scan.predicate.is_some())?;
    let matches = crate::query::value::filter_node_rows(matches, &scan, &hydrated, params)?;
    let [row] = matches.as_slice() else {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL DETACH DELETE requires exactly one matched `{}` node, found {}",
                plan.label,
                matches.len()
            ),
        });
    };
    let node_idx = ENGINE.with(|engine| {
        engine
            .borrow()
            .resolve(plan.table_oid, &row.node.node_id)
            .ok_or_else(|| safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL DETACH DELETE node `{}` is not in the built graph",
                    row.node.node_id
                ),
            })
    })?;
    lock_and_recheck_node_write(
        plan.table_oid,
        &plan.label,
        &plan.predicate,
        &row.node.node_id,
        params,
        tenant_scope,
        "GQL DETACH DELETE",
    )?;
    let deleted_edges = delete_incident_edge_rows(plan, &row.node.node_id)?;
    crate::projection::tx_delta::ensure_write_capacity(1, deleted_edges.delta_count(), 0)?;
    let deleted = delete_mapped_node_row(plan, &row.node.node_id)?;
    for edge in deleted_edges.edges {
        record_detach_deleted_edge_delta(&edge)?;
    }
    crate::projection::tx_delta::record_deleted_node(node_idx)?;
    Ok(vec![project_detach_deleted_node(plan, deleted, hydrate)])
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
    let matched = matched_edge_ids(row, plan)?;
    lock_and_recheck_edge_write(&read_plan, row, params, tenant_scope)?;
    delete_mapped_edge_row(
        plan,
        &matched.source_id,
        &matched.target_id,
        &matched.source_key,
    )?;
    record_deleted_edge_delta(
        plan,
        &matched.source_id,
        &matched.target_id,
        matched.relationship_id,
    )?;
    crate::query::value::project_rows(matches, &read_plan, &hydrated, params, hydrate)
}

fn delete_edge_read_plan(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
) -> crate::query::physical_plan::PhysicalPlan {
    crate::query::physical_plan::PhysicalPlan {
        optional: false,
        source_var: plan.source_var.clone(),
        source_table_oid: plan.source_table_oid,
        source_label: plan.source_label.clone(),
        rel_type: plan.rel_type.clone(),
        rel_var: Some(plan.rel_var.clone()),
        direction: plan.direction,
        hops: crate::query::logical_plan::HopBounds {
            variable: false,
            min: 1,
            max: 1,
        },
        edge_mapping: Some(crate::query::catalog_snapshot::EdgeMappingInfo {
            mapping_id: plan.edge_mapping_id,
            edge_table_oid: plan.edge_table_oid,
            source_table_oid: plan.edge_source_table_oid,
            target_table_oid: plan.edge_target_table_oid,
            source_column: plan.source_column.clone(),
            target_column: plan.target_column.clone(),
            source_key_columns: plan.edge_source_key_columns.clone(),
            bidirectional: plan.bidirectional,
            label_column: None,
        }),
        target_var: plan.target_var.clone(),
        target_table_oid: plan.target_table_oid,
        target_label: plan.target_label.clone(),
        returns: plan.returns.clone(),
        distinct_stages: Vec::new(),
        distinct: false,
        predicate: plan.predicate.clone(),
        order_by: Vec::new(),
        skip: None,
        limit: None,
    }
}

fn create_relationship_node_scan(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    source: bool,
) -> crate::query::physical_plan::PhysicalNodeScan {
    if source {
        crate::query::physical_plan::PhysicalNodeScan {
            optional: false,
            var: plan.source_var.clone(),
            table_oid: plan.source_table_oid,
            label: plan.source_label.clone(),
            returns: Vec::new(),
            distinct_stages: Vec::new(),
            distinct: false,
            predicate: plan.source_predicate.clone(),
            identity_lookup: None,
            order_by: Vec::new(),
            skip: None,
            limit: None,
        }
    } else {
        crate::query::physical_plan::PhysicalNodeScan {
            optional: false,
            var: plan.target_var.clone(),
            table_oid: plan.target_table_oid,
            label: plan.target_label.clone(),
            returns: Vec::new(),
            distinct_stages: Vec::new(),
            distinct: false,
            predicate: plan.target_predicate.clone(),
            identity_lookup: None,
            order_by: Vec::new(),
            skip: None,
            limit: None,
        }
    }
}

fn matched_create_relationship_endpoint(
    scan: &crate::query::physical_plan::PhysicalNodeScan,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<crate::query::execute::GqlNodeRow> {
    let matches = ENGINE.with(|engine| {
        crate::query::execute::execute_node_scan(&engine.borrow(), scan, tenant_scope, params)
    })?;
    let hydrated = hydrate_gql_node_rows(&matches, scan.predicate.is_some())?;
    let matches = crate::query::value::filter_node_rows(matches, scan, &hydrated, params)?;
    let [row] = matches.as_slice() else {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL relationship CREATE requires exactly one matched `{}` endpoint, found {}",
                scan.label,
                matches.len()
            ),
        });
    };
    Ok(row.clone())
}

struct MatchedEdgeIds {
    source_id: String,
    target_id: String,
    relationship_id: crate::edge_store::RelationshipId,
    source_key: String,
}

fn matched_edge_ids(
    row: &crate::query::execute::GqlRow,
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
) -> safety::GraphResult<MatchedEdgeIds> {
    let rel_start = row
        .rel_start
        .as_ref()
        .ok_or_else(|| safety::GraphError::GqlExecution {
            reason: "GQL DELETE matched a row without relationship endpoints".to_string(),
        })?;
    let rel_end = row
        .rel_end
        .as_ref()
        .ok_or_else(|| safety::GraphError::GqlExecution {
            reason: "GQL DELETE matched a row without relationship endpoints".to_string(),
        })?;
    let relationship_id = row
        .relationship_id
        .ok_or_else(|| safety::GraphError::GqlExecution {
            reason: "GQL DELETE requires a stable source-row relationship identity".to_string(),
        })?;
    let identities = relationship_identity_snapshot();
    let identity = identities
        .get(relationship_id as usize)
        .and_then(Option::as_ref)
        .filter(|identity| identity.mapping_id == plan.edge_mapping_id)
        .ok_or_else(|| safety::GraphError::GqlExecution {
            reason: format!(
                "GQL DELETE relationship identity `{relationship_id}` does not belong to mapping {}",
                plan.edge_mapping_id
            ),
        })?;
    Ok(MatchedEdgeIds {
        source_id: rel_start.node_id.clone(),
        target_id: rel_end.node_id.clone(),
        relationship_id,
        source_key: identity.source_key.clone(),
    })
}

struct CreatedNode {
    node_id: String,
    tenant: Option<String>,
    row: serde_json::Value,
}

struct CreatedRelationship {
    row: serde_json::Value,
    source_key: String,
}

struct MergedNode {
    node_id: String,
    tenant: Option<String>,
    row: serde_json::Value,
    created: bool,
}

struct UpdatedNode {
    node_id: String,
    row: serde_json::Value,
}

struct DeletedNode {
    node_id: String,
    row: serde_json::Value,
}

struct DeletedIncidentEdges {
    edges: Vec<DeletedIncidentEdge>,
}

impl DeletedIncidentEdges {
    fn delta_count(&self) -> usize {
        self.edges
            .iter()
            .map(|edge| if edge.bidirectional { 2 } else { 1 })
            .sum()
    }
}

struct DeletedIncidentEdge {
    rel_type: String,
    source_table_oid: u32,
    target_table_oid: u32,
    source_id: String,
    target_id: String,
    bidirectional: bool,
}

fn insert_mapped_node(
    plan: &crate::query::physical_plan::PhysicalCreateNode,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<CreatedNode> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find(|table| table.table_oid == plan.table_oid)
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot insert node into unregistered table OID {}",
                plan.table_oid
            ))
        })?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
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
         SELECT {}
         FROM inserted",
        table_name.as_sql(),
        insert_shape.columns.join(", "),
        insert_shape.selectors.join(", "),
        table_name.as_sql(),
        pk_expr
    );
    let returned_node_id = pgrx::Spi::connect_mut(|client| {
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
        row.get::<String>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL CREATE primary key read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL CREATE returned no primary key".to_string())
            })
    })?;
    let final_row = read_node_after_statement_triggers(
        table,
        table_name.as_sql(),
        &returned_node_id,
        "GQL CREATE",
    )?;
    recheck_locked_row_tenant(
        &final_row.row,
        table.tenant_column.as_deref(),
        tenant_scope,
        "GQL CREATE",
        &final_row.node_id,
    )?;
    Ok(CreatedNode {
        tenant: row_tenant(&final_row.row, table.tenant_column.as_deref()),
        node_id: final_row.node_id,
        row: final_row.row,
    })
}

fn insert_mapped_relationship(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    source_id: &str,
    target_id: &str,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<CreatedRelationship> {
    let table_name = regclass_text(plan.edge_table_oid)?;
    let insert_shape = create_relationship_insert_shape(plan)?;
    let values =
        create_relationship_values_json(plan, &insert_shape, source_id, target_id, params)?;
    let source_key_expr = create_relationship_source_key_predicate(plan, "inserted");
    let query = format!(
        "WITH inserted AS (
             INSERT INTO {table_name} ({})
             SELECT {}
             FROM jsonb_populate_record(NULL::{table_name}, $1::jsonb) AS rec
             RETURNING *
         )
         SELECT {source_key_expr}
         FROM inserted",
        insert_shape.columns.join(", "),
        insert_shape.selectors.join(", "),
    );
    let source_key = pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[pgrx::JsonB(values).into()])
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL relationship CREATE insert failed for {table_name}: {err}"
                ))
            })?;
        let row = rows.first();
        row.get::<String>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL relationship CREATE source-key read failed: {err}"
                ))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal(
                    "GQL relationship CREATE returned a NULL source key".to_string(),
                )
            })
    })?;

    let row_source_key_expr = create_relationship_source_key_predicate(plan, "edge_row");
    let row_source_id_expr = format!("edge_row.{}::text", quote_ident(&plan.edge_source_column));
    let row_target_id_expr = format!("edge_row.{}::text", quote_ident(&plan.edge_target_column));
    let row_label_expr = plan
        .label_column
        .as_ref()
        .map(|column| format!("edge_row.{}::text", quote_ident(column)))
        .unwrap_or_else(|| "NULL::text".to_string());
    let verify_query = format!(
        "SELECT to_jsonb(edge_row.*), {row_source_id_expr},
                {row_target_id_expr}, {row_label_expr}
           FROM {table_name} AS edge_row
          WHERE {row_source_key_expr} = $1"
    );
    pgrx::Spi::connect(|client| {
        let rows = client
            .select(&verify_query, None, &[source_key.as_str().into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL relationship CREATE post-trigger source-row verification failed for {table_name}: {err}"
                ),
            })?;
        if rows.len() != 1 {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL relationship CREATE was rejected because PostgreSQL statement triggers removed or changed registered relationship source identity `{source_key}`; the source write was rolled back"
                ),
            });
        }
        let row = rows.first();
        let row_json = row
            .get::<pgrx::JsonB>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL relationship CREATE post-trigger row read failed: {err}"
                ))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal(
                    "GQL relationship CREATE post-trigger verification returned no row JSON"
                        .to_string(),
                )
            })?;
        let returned_source_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL relationship CREATE source endpoint read failed: {err}"
                ))
            })?
            .ok_or_else(|| safety::GraphError::GqlExecution {
                reason: "GQL relationship CREATE returned a NULL registered source endpoint"
                    .to_string(),
            })?;
        let returned_target_id = row
            .get::<String>(3)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL relationship CREATE target endpoint read failed: {err}"
                ))
            })?
            .ok_or_else(|| safety::GraphError::GqlExecution {
                reason: "GQL relationship CREATE returned a NULL registered target endpoint"
                    .to_string(),
            })?;
        ensure_unchanged_relationship_identity(
            plan,
            source_id,
            target_id,
            &returned_source_id,
            &returned_target_id,
            row.get::<String>(4).map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL relationship CREATE dynamic label read failed: {err}"
                ))
            })?,
        )?;
        Ok(CreatedRelationship {
            row: row_json.0,
            source_key,
        })
    })
}

fn ensure_unchanged_relationship_identity(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    expected_source_id: &str,
    expected_target_id: &str,
    returned_source_id: &str,
    returned_target_id: &str,
    returned_label: Option<String>,
) -> safety::GraphResult<()> {
    let endpoints_match =
        returned_source_id == expected_source_id && returned_target_id == expected_target_id;
    let label_matches =
        plan.label_column.is_none() || returned_label.as_deref() == Some(&plan.rel_type);
    if endpoints_match && label_matches {
        return Ok(());
    }
    Err(safety::GraphError::GqlExecution {
        reason: format!(
            "GQL relationship CREATE was rejected because a PostgreSQL trigger changed registered graph identity (expected {}-[{}]->{}, returned {}-[{}]->{}); the source write was rolled back",
            expected_source_id,
            plan.rel_type,
            expected_target_id,
            returned_source_id,
            returned_label.as_deref().unwrap_or(&plan.rel_type),
            returned_target_id
        ),
    })
}

fn create_relationship_source_key_predicate(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    alias: &str,
) -> String {
    let columns = plan.edge_source_key_columns.columns();
    if columns.len() > 1 {
        let parts = columns
            .iter()
            .map(|column| format!("{alias}.{}::text", quote_ident(column)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("jsonb_build_array({parts})::text")
    } else if let Some(column) = columns.first() {
        format!("{alias}.{}::text", quote_ident(column))
    } else {
        "NULL::text".to_string()
    }
}

fn merge_mapped_node(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<MergedNode> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find(|table| table.table_oid == plan.table_oid)
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot merge node into unregistered table OID {}",
                plan.table_oid
            ))
        })?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
    let identity_values =
        merge_identity_values_json(plan, table.tenant_column.as_deref(), tenant_scope, params)?;
    validate_merge_identity(&identity_values, &table.id_columns)?;
    if let Some(locked) = try_lock_merge_node(
        plan,
        table,
        table_name.as_sql(),
        identity_values.clone(),
        tenant_scope,
    )? {
        return apply_merge_match_branch(plan, locked, params, tenant_scope);
    }

    let insert_values =
        merge_insert_values_json(plan, table.tenant_column.as_deref(), tenant_scope, params)?;
    if let Some(inserted) = try_insert_merge_node(
        plan,
        table,
        table_name.as_sql(),
        tenant_scope,
        insert_values,
    )? {
        return Ok(inserted);
    }

    let locked = try_lock_merge_node(
        plan,
        table,
        table_name.as_sql(),
        identity_values,
        tenant_scope,
    )?
    .ok_or_else(|| safety::GraphError::GqlExecution {
        reason: format!("GQL MERGE could not find or insert `{}` node", plan.label),
    })?;
    apply_merge_match_branch(plan, locked, params, tenant_scope)
}

fn apply_merge_match_branch(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    locked: MergedNode,
    params: &crate::query::value::QueryParams,
    tenant_scope: Option<&str>,
) -> safety::GraphResult<MergedNode> {
    if let Some(on_match) = &plan.on_match {
        let set_plan = crate::query::physical_plan::PhysicalSetProperty {
            var: plan.var.clone(),
            table_oid: plan.table_oid,
            label: plan.label.clone(),
            predicate: None,
            property: on_match.property.clone(),
            value: on_match.value.clone(),
            returns: Vec::new(),
        };
        let updated = update_mapped_property(&set_plan, &locked.node_id, params, tenant_scope)?;
        return Ok(MergedNode {
            node_id: updated.node_id,
            tenant: locked.tenant,
            row: updated.row,
            created: false,
        });
    }
    Ok(locked)
}

fn try_insert_merge_node(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    table: &crate::builder::RegisteredTable,
    table_sql: &str,
    tenant_scope: Option<&str>,
    values: serde_json::Value,
) -> safety::GraphResult<Option<MergedNode>> {
    let insert_shape = merge_insert_shape(plan, table.tenant_column.as_deref(), tenant_scope);
    let pk_expr = primary_key_expr("src", &table.id_columns);
    let conflict_columns = table
        .id_columns
        .columns()
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "WITH inserted AS (
             INSERT INTO {} AS src ({})
             SELECT {}
             FROM jsonb_populate_record(NULL::{}, $1::jsonb) AS rec
             ON CONFLICT ({}) DO NOTHING
             RETURNING {}
         )
         SELECT * FROM inserted",
        table_sql,
        insert_shape.columns.join(", "),
        insert_shape.selectors.join(", "),
        table_sql,
        conflict_columns,
        pk_expr
    );
    let returned_node_id = pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[pgrx::JsonB(values).into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!("GQL MERGE insert failed for {}: {}", table_sql, err),
            })?;
        if rows.is_empty() {
            return Ok(None);
        }
        let row = rows.first();
        let node_id = row
            .get::<String>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL MERGE inserted primary key read failed: {err}"
                ))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL MERGE insert returned no primary key".to_string())
            })?;
        Ok(Some(node_id))
    })?;
    let Some(returned_node_id) = returned_node_id else {
        return Ok(None);
    };
    let final_row =
        read_node_after_statement_triggers(table, table_sql, &returned_node_id, "GQL MERGE")?;
    recheck_locked_row_tenant(
        &final_row.row,
        table.tenant_column.as_deref(),
        tenant_scope,
        "GQL MERGE",
        &final_row.node_id,
    )?;
    Ok(Some(MergedNode {
        tenant: row_tenant(&final_row.row, table.tenant_column.as_deref()),
        node_id: final_row.node_id,
        row: final_row.row,
        created: true,
    }))
}

fn try_lock_merge_node(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    table: &crate::builder::RegisteredTable,
    table_sql: &str,
    values: serde_json::Value,
    tenant_scope: Option<&str>,
) -> safety::GraphResult<Option<MergedNode>> {
    let pk_expr = primary_key_expr("src", &table.id_columns);
    let rec_pk_expr = primary_key_expr("rec", &table.id_columns);
    let tenant_predicate = match (table.tenant_column.as_deref(), tenant_scope) {
        (Some(column), Some(_)) => {
            format!(
                " AND src.{}::text = rec.{}::text",
                quote_ident(column),
                quote_ident(column)
            )
        }
        _ => String::new(),
    };
    let lock_clause = if plan.on_match.is_some() {
        " FOR UPDATE OF src"
    } else {
        ""
    };
    let query = format!(
        "SELECT to_jsonb(src.*), {}
         FROM {} AS src, jsonb_populate_record(NULL::{}, $1::jsonb) AS rec
         WHERE {} = {}{}{}",
        pk_expr, table_sql, table_sql, pk_expr, rec_pk_expr, tenant_predicate, lock_clause
    );
    pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[pgrx::JsonB(values).into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!("GQL MERGE lookup failed for {}: {}", table_sql, err),
            })?;
        if rows.is_empty() {
            return Ok(None);
        }
        if rows.len() > 1 {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL MERGE requires exactly one matched `{}` node, found {}",
                    plan.label,
                    rows.len()
                ),
            });
        }
        let row = rows.first();
        let row_json = row
            .get::<pgrx::JsonB>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL MERGE matched row read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL MERGE match returned no row JSON".to_string())
            })?;
        let node_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL MERGE matched primary key read failed: {err}"
                ))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL MERGE match returned no primary key".to_string())
            })?;
        let tenant = row_tenant(&row_json.0, table.tenant_column.as_deref());
        Ok(Some(MergedNode {
            node_id,
            tenant,
            row: row_json.0,
            created: false,
        }))
    })
}

fn row_tenant(row: &serde_json::Value, tenant_column: Option<&str>) -> Option<String> {
    tenant_column.and_then(|column| {
        row.get(column)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    })
}

fn read_node_after_statement_triggers(
    table: &crate::builder::RegisteredTable,
    table_sql: &str,
    returned_node_id: &str,
    operation: &str,
) -> safety::GraphResult<LockedNodeRow> {
    let pk_expr = primary_key_expr("src", &table.id_columns);
    let query = format!(
        "SELECT to_jsonb(src.*), {pk_expr}
           FROM {table_sql} AS src
          WHERE {pk_expr} = $1"
    );
    pgrx::Spi::connect(|client| {
        let rows = client
            .select(&query, None, &[returned_node_id.into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!(
                    "{operation} post-trigger source-row verification failed for {table_sql}: {err}"
                ),
            })?;
        if rows.len() != 1 {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "{operation} was rejected because PostgreSQL statement triggers removed or changed registered node identity `{returned_node_id}`; the source write was rolled back"
                ),
            });
        }
        let row = rows.first();
        let row_json = row
            .get::<pgrx::JsonB>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "{operation} post-trigger row read failed: {err}"
                ))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal(format!(
                    "{operation} post-trigger verification returned no row JSON"
                ))
            })?;
        let node_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "{operation} post-trigger primary key read failed: {err}"
                ))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal(format!(
                    "{operation} post-trigger verification returned no primary key"
                ))
            })?;
        Ok(LockedNodeRow {
            node_id,
            row: row_json.0,
        })
    })
}

fn ensure_unchanged_tenant_identity(
    operation: &str,
    tenant_column: Option<&str>,
    before: &serde_json::Value,
    after: &serde_json::Value,
) -> safety::GraphResult<()> {
    let Some(tenant_column) = tenant_column else {
        return Ok(());
    };
    let before_tenant = row_tenant(before, Some(tenant_column));
    let after_tenant = row_tenant(after, Some(tenant_column));
    if before_tenant == after_tenant {
        return Ok(());
    }
    Err(safety::GraphError::GqlExecution {
        reason: format!(
            "{operation} was rejected because a PostgreSQL trigger changed registered tenant identity; the source write was rolled back"
        ),
    })
}

fn validate_merge_identity(
    values: &serde_json::Value,
    id_columns: &crate::builder::PrimaryKeySpec,
) -> safety::GraphResult<()> {
    let Some(values) = values.as_object() else {
        return Err(safety::GraphError::GqlExecution {
            reason: "GQL MERGE identity values must be a JSON object".to_string(),
        });
    };
    for column in id_columns.columns() {
        if !values.contains_key(column)
            || values.get(column).is_some_and(serde_json::Value::is_null)
        {
            return Err(safety::GraphError::GqlExecution {
                reason: format!("GQL MERGE requires non-null identity property `{column}`"),
            });
        }
    }
    Ok(())
}

fn lock_and_recheck_node_write(
    table_oid: u32,
    label: &str,
    predicate: &Option<crate::query::logical_plan::Predicate>,
    node_id: &str,
    params: &crate::query::value::QueryParams,
    tenant_scope: Option<&str>,
    operation: &str,
) -> safety::GraphResult<LockedNodeRow> {
    let locked = lock_node_coordinate(table_oid, node_id, tenant_scope, operation)?;
    let scan = crate::query::physical_plan::PhysicalNodeScan {
        optional: false,
        var: String::new(),
        table_oid,
        label: label.to_string(),
        returns: Vec::new(),
        distinct_stages: Vec::new(),
        distinct: false,
        predicate: predicate.clone(),
        identity_lookup: None,
        order_by: Vec::new(),
        skip: None,
        limit: None,
    };
    let row = crate::query::execute::GqlNodeRow {
        node: crate::query::execute::GqlNodeCoordinate {
            table_oid,
            node_id: locked.node_id.clone(),
        },
        optional_null: false,
    };
    let hydrated = [((table_oid, locked.node_id.clone()), locked.row.clone())]
        .into_iter()
        .collect();
    let matches = crate::query::value::filter_node_rows(vec![row], &scan, &hydrated, params)?;
    if matches.len() == 1 {
        return Ok(locked);
    }
    Err(safety::GraphError::GqlExecution {
        reason: format!(
            "{operation} matched `{label}` node `{node_id}` but the PostgreSQL row no longer satisfies the matched predicate"
        ),
    })
}

fn lock_and_recheck_edge_write(
    plan: &crate::query::physical_plan::PhysicalPlan,
    row: &crate::query::execute::GqlRow,
    params: &crate::query::value::QueryParams,
    tenant_scope: Option<&str>,
) -> safety::GraphResult<()> {
    let Some(target) = row.target.as_ref() else {
        return Err(safety::GraphError::GqlExecution {
            reason: "GQL DELETE matched a row without a target node".to_string(),
        });
    };
    let source_key = (plan.source_table_oid, row.source.node_id.as_str());
    let target_key = (plan.target_table_oid, target.node_id.as_str());
    let (source, target) = if source_key <= target_key {
        let source = lock_node_coordinate(source_key.0, source_key.1, tenant_scope, "GQL DELETE")?;
        let target = lock_node_coordinate(target_key.0, target_key.1, tenant_scope, "GQL DELETE")?;
        (source, target)
    } else {
        let target = lock_node_coordinate(target_key.0, target_key.1, tenant_scope, "GQL DELETE")?;
        let source = lock_node_coordinate(source_key.0, source_key.1, tenant_scope, "GQL DELETE")?;
        (source, target)
    };
    let hydrated = [
        ((plan.source_table_oid, source.node_id), source.row),
        ((plan.target_table_oid, target.node_id), target.row),
    ]
    .into_iter()
    .collect();
    let matches = crate::query::value::filter_rows(vec![row.clone()], plan, &hydrated, params)?;
    if matches.len() == 1 {
        return Ok(());
    }
    Err(safety::GraphError::GqlExecution {
        reason: format!(
            "GQL DELETE matched `{}` relationship but the PostgreSQL endpoint rows no longer satisfy the matched predicate",
            plan.rel_type
        ),
    })
}

struct LockedNodeRow {
    node_id: String,
    row: serde_json::Value,
}

fn lock_node_coordinate(
    table_oid: u32,
    node_id: &str,
    tenant_scope: Option<&str>,
    operation: &str,
) -> safety::GraphResult<LockedNodeRow> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find(|table| table.table_oid == table_oid)
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "{operation} cannot lock unregistered table OID {table_oid}"
            ))
        })?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
    let pk_expr = primary_key_expr("src", &table.id_columns);
    let query = format!(
        "SELECT to_jsonb(src.*), {}
         FROM {} AS src
         WHERE {} = $1
         FOR UPDATE OF src",
        pk_expr,
        table_name.as_sql(),
        pk_expr
    );
    pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[node_id.into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!(
                    "{operation} row lock failed for {}: {}",
                    table_name.as_sql(),
                    err
                ),
            })?;
        if rows.is_empty() {
            return Err(safety::GraphError::GqlExecution {
                reason: format!("{operation} matched node `{node_id}` but PostgreSQL found no row"),
            });
        }
        let row = rows.first();
        let row_json = row
            .get::<pgrx::JsonB>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!("{operation} locked row read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal(format!("{operation} locked row returned no JSON"))
            })?;
        recheck_locked_row_tenant(
            &row_json.0,
            table.tenant_column.as_deref(),
            tenant_scope,
            operation,
            node_id,
        )?;
        let node_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "{operation} locked primary key read failed: {err}"
                ))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal(format!(
                    "{operation} locked row returned no primary key"
                ))
            })?;
        Ok(LockedNodeRow {
            node_id,
            row: row_json.0,
        })
    })
}

fn recheck_locked_row_tenant(
    row: &serde_json::Value,
    tenant_column: Option<&str>,
    tenant_scope: Option<&str>,
    operation: &str,
    node_id: &str,
) -> safety::GraphResult<()> {
    let (Some(tenant_column), Some(tenant_scope)) = (tenant_column, tenant_scope) else {
        return Ok(());
    };
    let current_tenant = row.get(tenant_column).and_then(serde_json::Value::as_str);
    if current_tenant == Some(tenant_scope) {
        return Ok(());
    }
    Err(safety::GraphError::GqlExecution {
        reason: format!(
            "{operation} matched node `{node_id}` but the PostgreSQL row no longer satisfies the active tenant scope"
        ),
    })
}

fn ensure_unchanged_node_identity(
    operation: &str,
    expected_node_id: &str,
    returned_node_id: &str,
) -> safety::GraphResult<()> {
    if expected_node_id == returned_node_id {
        return Ok(());
    }
    Err(safety::GraphError::GqlExecution {
        reason: format!(
            "{operation} was rejected because a PostgreSQL trigger changed registered node identity from `{expected_node_id}` to `{returned_node_id}`; the source write was rolled back"
        ),
    })
}

fn update_mapped_property(
    plan: &crate::query::physical_plan::PhysicalSetProperty,
    node_id: &str,
    params: &crate::query::value::QueryParams,
    tenant_scope: Option<&str>,
) -> safety::GraphResult<UpdatedNode> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find(|table| table.table_oid == plan.table_oid)
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot update node in unregistered table OID {}",
                plan.table_oid
            ))
        })?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
    let locked = lock_and_recheck_node_write(
        plan.table_oid,
        &plan.label,
        &plan.predicate,
        node_id,
        params,
        tenant_scope,
        "GQL SET",
    )?;
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
    let returned = pgrx::Spi::connect_mut(|client| {
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
        let returned_node_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL SET primary key read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL SET returned no primary key".to_string())
            })?;
        ensure_unchanged_node_identity("GQL SET", node_id, &returned_node_id)?;
        recheck_locked_row_tenant(
            &row_json.0,
            table.tenant_column.as_deref(),
            tenant_scope,
            "GQL SET",
            node_id,
        )?;
        Ok(UpdatedNode {
            node_id: returned_node_id,
            row: row_json.0,
        })
    })?;
    let final_row = read_node_after_statement_triggers(
        table,
        table_name.as_sql(),
        &returned.node_id,
        "GQL SET",
    )?;
    ensure_unchanged_node_identity("GQL SET", node_id, &final_row.node_id)?;
    ensure_unchanged_tenant_identity(
        "GQL SET",
        table.tenant_column.as_deref(),
        &locked.row,
        &final_row.row,
    )?;
    recheck_locked_row_tenant(
        &final_row.row,
        table.tenant_column.as_deref(),
        tenant_scope,
        "GQL SET",
        node_id,
    )?;
    Ok(UpdatedNode {
        node_id: final_row.node_id,
        row: final_row.row,
    })
}

fn remove_mapped_property(
    plan: &crate::query::physical_plan::PhysicalRemoveProperty,
    node_id: &str,
    params: &crate::query::value::QueryParams,
    tenant_scope: Option<&str>,
) -> safety::GraphResult<UpdatedNode> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find(|table| table.table_oid == plan.table_oid)
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot remove property from unregistered table OID {}",
                plan.table_oid
            ))
        })?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
    let locked = lock_and_recheck_node_write(
        plan.table_oid,
        &plan.label,
        &plan.predicate,
        node_id,
        params,
        tenant_scope,
        "GQL REMOVE",
    )?;
    let pk_expr = primary_key_expr("src", &table.id_columns);
    let assignment = remove_property_assignment(&plan.property);
    let query = format!(
        "WITH updated AS (
             UPDATE {} AS src
             SET {}
             WHERE {} = $1
             RETURNING to_jsonb(src.*), {}
         )
         SELECT * FROM updated",
        table_name.as_sql(),
        assignment,
        pk_expr,
        pk_expr
    );
    let returned = pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[node_id.into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL REMOVE update failed for {}.{}: {}",
                    table_name.as_sql(),
                    quote_ident(&plan.property),
                    err
                ),
            })?;
        if rows.is_empty() {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL REMOVE matched node `{}` but PostgreSQL updated no row",
                    node_id
                ),
            });
        }
        let row = rows.first();
        let row_json = row
            .get::<pgrx::JsonB>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL REMOVE row read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL REMOVE returned no row JSON".into())
            })?;
        let returned_node_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL REMOVE primary key read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL REMOVE returned no primary key".to_string())
            })?;
        ensure_unchanged_node_identity("GQL REMOVE", node_id, &returned_node_id)?;
        recheck_locked_row_tenant(
            &row_json.0,
            table.tenant_column.as_deref(),
            tenant_scope,
            "GQL REMOVE",
            node_id,
        )?;
        Ok(UpdatedNode {
            node_id: returned_node_id,
            row: row_json.0,
        })
    })?;
    let final_row = read_node_after_statement_triggers(
        table,
        table_name.as_sql(),
        &returned.node_id,
        "GQL REMOVE",
    )?;
    ensure_unchanged_node_identity("GQL REMOVE", node_id, &final_row.node_id)?;
    ensure_unchanged_tenant_identity(
        "GQL REMOVE",
        table.tenant_column.as_deref(),
        &locked.row,
        &final_row.row,
    )?;
    recheck_locked_row_tenant(
        &final_row.row,
        table.tenant_column.as_deref(),
        tenant_scope,
        "GQL REMOVE",
        node_id,
    )?;
    Ok(UpdatedNode {
        node_id: final_row.node_id,
        row: final_row.row,
    })
}

fn delete_mapped_node_row(
    plan: &crate::query::physical_plan::PhysicalDetachDeleteNode,
    node_id: &str,
) -> safety::GraphResult<DeletedNode> {
    let (tables, _edges, _filter_columns) = read_catalog()?;
    let table = tables
        .iter()
        .find(|table| table.table_oid == plan.table_oid)
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "cannot delete node from unregistered table OID {}",
                plan.table_oid
            ))
        })?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
    let pk_expr = primary_key_expr("src", &table.id_columns);
    let query = format!(
        "WITH deleted AS (
             DELETE FROM {} AS src
             WHERE {} = $1
             RETURNING to_jsonb(src.*), {}
         )
         SELECT * FROM deleted",
        table_name.as_sql(),
        pk_expr,
        pk_expr
    );
    pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[node_id.into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL DETACH DELETE node row failed for {}: {}",
                    table_name.as_sql(),
                    err
                ),
            })?;
        if rows.is_empty() {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL DETACH DELETE matched node `{node_id}` but PostgreSQL deleted no row"
                ),
            });
        }
        let row = rows.first();
        let row_json = row
            .get::<pgrx::JsonB>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!("GQL DETACH DELETE row read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("GQL DETACH DELETE returned no row JSON".into())
            })?;
        let node_id = row
            .get::<String>(2)
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "GQL DETACH DELETE primary key read failed: {err}"
                ))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal(
                    "GQL DETACH DELETE returned no primary key".to_string(),
                )
            })?;
        Ok(DeletedNode {
            node_id,
            row: row_json.0,
        })
    })
}

fn delete_incident_edge_rows(
    plan: &crate::query::physical_plan::PhysicalDetachDeleteNode,
    node_id: &str,
) -> safety::GraphResult<DeletedIncidentEdges> {
    let (_tables, edges, _filter_columns) = read_catalog()?;
    let mut deleted = Vec::new();
    for incident in &plan.incident_edges {
        let edge = edges
            .iter()
            .find(|edge| {
                edge.from_table_oid == incident.edge_table_oid
                    && edge.from_column == incident.source_column
                    && edge.to_column == incident.target_column
                    && edge.label == incident.rel_type
            })
            .ok_or_else(|| {
                safety::GraphError::Internal(format!(
                    "cannot detach-delete unregistered edge row table OID {}",
                    incident.edge_table_oid
                ))
            })?;
        let table_name = sql_table_name_from_oid(edge.from_table_oid)?;
        deleted.extend(delete_incident_edge_rows_for_mapping(
            plan,
            incident,
            table_name.as_sql(),
            node_id,
        )?);
    }
    Ok(DeletedIncidentEdges { edges: deleted })
}

fn delete_incident_edge_rows_for_mapping(
    plan: &crate::query::physical_plan::PhysicalDetachDeleteNode,
    incident: &crate::query::physical_plan::PhysicalIncidentEdge,
    table_sql: &str,
    node_id: &str,
) -> safety::GraphResult<Vec<DeletedIncidentEdge>> {
    let source_incident = incident.edge_source_table_oid == plan.table_oid;
    let target_incident = incident.edge_target_table_oid == plan.table_oid;
    let predicate = match (source_incident, target_incident) {
        (true, true) => format!(
            "e.{}::text = $1 OR e.{}::text = $1",
            quote_ident(&incident.source_column),
            quote_ident(&incident.target_column)
        ),
        (true, false) => format!("e.{}::text = $1", quote_ident(&incident.source_column)),
        (false, true) => format!("e.{}::text = $1", quote_ident(&incident.target_column)),
        (false, false) => return Ok(Vec::new()),
    };
    let query = format!(
        "WITH deleted AS (
             DELETE FROM {} AS e
             WHERE {}
             RETURNING e.{}::text, e.{}::text
         )
         SELECT * FROM deleted",
        table_sql,
        predicate,
        quote_ident(&incident.source_column),
        quote_ident(&incident.target_column)
    );
    pgrx::Spi::connect_mut(|client| {
        let rows = client
            .update(&query, None, &[node_id.into()])
            .map_err(|err| safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL DETACH DELETE incident edge row failed for {}: {}",
                    table_sql, err
                ),
            })?;
        let mut deleted = Vec::with_capacity(rows.len());
        for row in rows {
            let source_id = row
                .get::<String>(1)
                .map_err(|err| {
                    safety::GraphError::Internal(format!(
                        "GQL DETACH DELETE source edge key read failed: {err}"
                    ))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "GQL DETACH DELETE returned no source edge key".to_string(),
                    )
                })?;
            let target_id = row
                .get::<String>(2)
                .map_err(|err| {
                    safety::GraphError::Internal(format!(
                        "GQL DETACH DELETE target edge key read failed: {err}"
                    ))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "GQL DETACH DELETE returned no target edge key".to_string(),
                    )
                })?;
            deleted.push(DeletedIncidentEdge {
                rel_type: incident.rel_type.clone(),
                source_table_oid: incident.edge_source_table_oid,
                target_table_oid: incident.edge_target_table_oid,
                source_id,
                target_id,
                bidirectional: incident.bidirectional,
            });
        }
        Ok(deleted)
    })
}

fn remove_property_assignment(property: &str) -> String {
    let Some((root, path)) = property.split_once('.') else {
        return format!("{} = NULL", quote_ident(property));
    };
    let path = path
        .split('.')
        .map(quote_literal)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} = {} #- ARRAY[{}]::text[]",
        quote_ident(root),
        quote_ident(root),
        path
    )
}

fn delete_mapped_edge_row(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    source_id: &str,
    target_id: &str,
    source_key: &str,
) -> safety::GraphResult<()> {
    let (tables, edges, _filter_columns) = read_catalog()?;
    let edge = edges
        .iter()
        .find(|edge| {
            edge.from_table_oid == plan.edge_table_oid
                && edge.from_column == plan.source_column
                && edge.to_column == plan.target_column
                && (edge.label == plan.rel_type || edge.label_column.is_some())
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
    let table_name = sql_table_name_from_oid(edge.from_table_oid)?;
    delete_mapped_edge_row_by_identity(
        plan,
        table_name.as_sql(),
        edge.label_column.as_deref(),
        &edge.label,
        source_id,
        target_id,
        source_key,
    )
}

fn delete_mapped_edge_row_by_identity(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    table_sql: &str,
    label_column: Option<&str>,
    fallback_label: &str,
    source_id: &str,
    target_id: &str,
    source_key: &str,
) -> safety::GraphResult<()> {
    let source_key_predicate =
        relationship_source_key_predicate_for_alias("e", &plan.edge_source_key_columns);
    let label_predicate = label_column
        .map(|column| {
            format!(
                "\n                AND COALESCE(NULLIF(BTRIM(e.{}::text), ''), $5) = $4",
                quote_ident(column)
            )
        })
        .unwrap_or_default();
    let query = format!(
        "WITH deleted AS (
             DELETE FROM {table_sql} AS e
              WHERE {source_key_predicate} = $1
                AND e.{}::text = $2
                AND e.{}::text = $3{label_predicate}
              RETURNING 1
         )
         SELECT count(*)::bigint FROM deleted",
        quote_ident(&plan.source_column),
        quote_ident(&plan.target_column),
    );
    let mut params = vec![source_key.into(), source_id.into(), target_id.into()];
    if label_column.is_some() {
        params.push(plan.rel_type.as_str().into());
        params.push(fallback_label.into());
    }
    let deleted = Spi::get_one_with_args::<i64>(&query, &params)
        .map_err(|err| safety::GraphError::GqlExecution {
            reason: format!("GQL DELETE edge row deletion failed for {table_sql}: {err}"),
        })?
        .unwrap_or_default();
    if deleted != 1 {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL DELETE relationship source row `{source_key}` was not visible in {table_sql}"
            ),
        });
    }
    Ok(())
}

fn record_deleted_edge_delta(
    plan: &crate::query::physical_plan::PhysicalDeleteEdge,
    source_id: &str,
    target_id: &str,
    relationship_id: crate::edge_store::RelationshipId,
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
    crate::projection::tx_delta::record_deleted_edge_with_identity(
        source,
        target,
        type_id,
        false,
        Some(relationship_id),
    )?;
    if plan.bidirectional {
        crate::projection::tx_delta::record_deleted_edge_with_identity(
            target,
            source,
            type_id,
            true,
            Some(relationship_id),
        )?;
    }
    Ok(())
}

fn record_detach_deleted_edge_delta(edge: &DeletedIncidentEdge) -> safety::GraphResult<()> {
    let Some((source, target, type_id)) = ENGINE.with(|engine| {
        let engine = engine.borrow();
        let source = engine.resolve(edge.source_table_oid, &edge.source_id)?;
        let target = engine.resolve(edge.target_table_oid, &edge.target_id)?;
        let type_id = engine
            .edge_type_registry
            .iter()
            .position(|label| label == &edge.rel_type)
            .map(|idx| idx as u8)?;
        Some((source, target, type_id))
    }) else {
        return Ok(());
    };
    crate::projection::tx_delta::record_deleted_edge(source, target, type_id)?;
    if edge.bidirectional {
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

struct RelationshipInsertShape {
    columns: Vec<String>,
    selectors: Vec<String>,
    source_column: String,
    target_column: String,
    label_column: Option<String>,
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

fn create_relationship_insert_shape(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
) -> safety::GraphResult<RelationshipInsertShape> {
    let mut columns = Vec::with_capacity(plan.properties.len() + 3);
    let mut selectors = Vec::with_capacity(columns.capacity());
    columns.push(quote_ident(&plan.edge_source_column));
    selectors.push(format!("rec.{}", quote_ident(&plan.edge_source_column)));
    columns.push(quote_ident(&plan.edge_target_column));
    selectors.push(format!("rec.{}", quote_ident(&plan.edge_target_column)));
    if let Some(label_column) = &plan.label_column {
        columns.push(quote_ident(label_column));
        selectors.push(format!("rec.{}", quote_ident(label_column)));
    }
    for property in &plan.properties {
        validate_column_exists(plan.edge_table_oid, &property.property)?;
        columns.push(quote_ident(&property.property));
        selectors.push(format!("rec.{}", quote_ident(&property.property)));
    }
    Ok(RelationshipInsertShape {
        columns,
        selectors,
        source_column: plan.edge_source_column.clone(),
        target_column: plan.edge_target_column.clone(),
        label_column: plan.label_column.clone(),
    })
}

fn merge_insert_shape(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    tenant_column: Option<&str>,
    tenant_scope: Option<&str>,
) -> CreateInsertShape {
    let mut columns = Vec::with_capacity(
        plan.properties.len()
            + usize::from(plan.on_create.is_some())
            + usize::from(tenant_column.is_some() && tenant_scope.is_some()),
    );
    let mut selectors = Vec::with_capacity(columns.capacity());
    for property in &plan.properties {
        columns.push(quote_ident(&property.property));
        selectors.push(format!("rec.{}", quote_ident(&property.property)));
    }
    if let Some(property) = &plan.on_create {
        columns.push(quote_ident(&property.property));
        selectors.push(format!("rec.{}", quote_ident(&property.property)));
    }
    let tenant_column = match tenant_column {
        Some(column)
            if plan
                .properties
                .iter()
                .any(|property| property.property == column) =>
        {
            Some(column.to_string())
        }
        Some(column) if tenant_scope.is_some() => {
            columns.push(quote_ident(column));
            selectors.push(format!("rec.{}", quote_ident(column)));
            Some(column.to_string())
        }
        Some(_) | None => None,
    };
    CreateInsertShape {
        columns,
        selectors,
        tenant_column,
    }
}

fn create_relationship_values_json(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    insert_shape: &RelationshipInsertShape,
    source_id: &str,
    target_id: &str,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<serde_json::Value> {
    let mut values = serde_json::Map::with_capacity(plan.properties.len() + 3);
    values.insert(
        insert_shape.source_column.clone(),
        serde_json::Value::String(source_id.to_string()),
    );
    values.insert(
        insert_shape.target_column.clone(),
        serde_json::Value::String(target_id.to_string()),
    );
    if let Some(label_column) = &insert_shape.label_column {
        values.insert(
            label_column.clone(),
            serde_json::Value::String(plan.rel_type.clone()),
        );
    }
    for property in &plan.properties {
        values.insert(
            property.property.clone(),
            write_value_json(&property.value, params)?,
        );
    }
    Ok(serde_json::Value::Object(values))
}

fn record_added_relationship_delta(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    source_id: &str,
    target_id: &str,
    source_key: &str,
    tenant_scope: Option<&str>,
) -> safety::GraphResult<()> {
    let (source, target, type_id, base_identity_count) = ENGINE.with(|engine| {
        let engine = engine.borrow();
        let source = resolve_built_or_added_node(
            &engine,
            plan.edge_source_table_oid,
            source_id,
            tenant_scope,
            "source",
        )?;
        let target = resolve_built_or_added_node(
            &engine,
            plan.edge_target_table_oid,
            target_id,
            tenant_scope,
            "target",
        )?;
        let type_id = engine
            .edge_type_registry
            .iter()
            .position(|label| label == &plan.rel_type)
            .map(|index| index as u8)
            .ok_or_else(|| safety::GraphError::GqlExecution {
                reason: format!(
                    "relationship type `{}` is not present in the built graph",
                    plan.rel_type
                ),
            })?;
        Ok::<_, safety::GraphError>((
            source,
            target,
            type_id,
            engine.relationship_identities.len(),
        ))
    })?;
    let relationship_id = crate::projection::tx_delta::record_relationship_identity(
        base_identity_count,
        crate::edge_store::RelationshipIdentity {
            mapping_id: plan.edge_mapping_id,
            source_key: source_key.to_string(),
        },
    )?;
    crate::projection::tx_delta::record_added_edge(
        source,
        crate::projection::tx_delta::DeltaEdge {
            target,
            type_id,
            schema_reversed: false,
            weight: None,
            relationship_id: Some(relationship_id),
        },
    )?;
    if plan.bidirectional {
        crate::projection::tx_delta::record_added_edge(
            target,
            crate::projection::tx_delta::DeltaEdge {
                target: source,
                type_id,
                schema_reversed: true,
                weight: None,
                relationship_id: Some(relationship_id),
            },
        )?;
    }
    Ok(())
}

fn resolve_built_or_added_node(
    engine: &crate::engine::Engine,
    table_oid: u32,
    node_id: &str,
    tenant_scope: Option<&str>,
    role: &str,
) -> safety::GraphResult<u32> {
    if let Some(node_idx) = engine.resolve(table_oid, node_id) {
        return Ok(node_idx);
    }
    let table_is_tenanted = engine.tenanted_table_oids.contains(&table_oid);
    crate::projection::tx_delta::resolve_added_node(
        table_oid,
        node_id,
        tenant_scope,
        table_is_tenanted,
    )
    .ok_or_else(|| safety::GraphError::GqlExecution {
        reason: format!(
            "GQL relationship CREATE {role} node `{node_id}` is not in the built graph or transaction delta"
        ),
    })
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
            Some(serde_json::Value::String(value)) if value == tenant_scope => {}
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

fn merge_identity_values_json(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    tenant_column: Option<&str>,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<serde_json::Value> {
    let mut values = serde_json::Map::with_capacity(plan.properties.len());
    for property in &plan.properties {
        values.insert(
            property.property.clone(),
            write_value_json(&property.value, params)?,
        );
    }
    apply_tenant_scope(&mut values, tenant_column, tenant_scope, "GQL MERGE")?;
    Ok(serde_json::Value::Object(values))
}

fn merge_insert_values_json(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    tenant_column: Option<&str>,
    tenant_scope: Option<&str>,
    params: &crate::query::value::QueryParams,
) -> safety::GraphResult<serde_json::Value> {
    let mut values = serde_json::Map::with_capacity(plan.properties.len() + 1);
    for property in &plan.properties {
        values.insert(
            property.property.clone(),
            write_value_json(&property.value, params)?,
        );
    }
    if let Some(property) = &plan.on_create {
        values.insert(
            property.property.clone(),
            write_value_json(&property.value, params)?,
        );
    }
    apply_tenant_scope(&mut values, tenant_column, tenant_scope, "GQL MERGE")?;
    Ok(serde_json::Value::Object(values))
}

fn apply_tenant_scope(
    values: &mut serde_json::Map<String, serde_json::Value>,
    tenant_column: Option<&str>,
    tenant_scope: Option<&str>,
    operation: &str,
) -> safety::GraphResult<()> {
    let (Some(tenant_column), Some(tenant_scope)) = (tenant_column, tenant_scope) else {
        return Ok(());
    };
    match values.get(tenant_column) {
        Some(serde_json::Value::String(value)) if value == tenant_scope => {}
        Some(_) => {
            return Err(safety::GraphError::InvalidFilter {
                reason: format!(
                    "{operation} tenant property `{tenant_column}` must match the active tenant scope"
                ),
            });
        }
        None => {
            values.insert(
                tenant_column.to_string(),
                serde_json::Value::String(tenant_scope.to_string()),
            );
        }
    }
    Ok(())
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

fn project_created_relationship(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    source_id: &str,
    target_id: &str,
    created: CreatedRelationship,
    hydrate: bool,
) -> safety::GraphResult<Vec<serde_json::Value>> {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            crate::query::physical_plan::ReturnSlot::Node { side, name } => {
                let value =
                    created_relationship_node_value(plan, *side, source_id, target_id, hydrate)?;
                output.insert(name.clone(), value);
            }
            crate::query::physical_plan::ReturnSlot::Relationship { name } => {
                output.insert(
                    name.clone(),
                    created_relationship_value(plan, source_id, target_id, &created, hydrate),
                );
            }
            crate::query::physical_plan::ReturnSlot::Property {
                side,
                property,
                name,
            } => {
                let value = created_relationship_node_property(
                    plan, *side, source_id, target_id, property,
                )?;
                output.insert(name.clone(), value);
            }
            crate::query::physical_plan::ReturnSlot::Path { .. }
            | crate::query::physical_plan::ReturnSlot::PathFunction { .. }
            | crate::query::physical_plan::ReturnSlot::Projected { .. }
            | crate::query::physical_plan::ReturnSlot::Aggregate { .. } => {
                return Err(safety::GraphError::GqlExecution {
                    reason: format!(
                        "relationship CREATE cannot project return slot `{}`",
                        slot.name()
                    ),
                });
            }
        }
    }
    Ok(vec![serde_json::Value::Object(output)])
}

fn project_merged_node(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    merged: MergedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            crate::query::physical_plan::CreateReturnSlot::Node { name } => {
                output.insert(name.clone(), merged_node_value(plan, &merged, hydrate));
            }
            crate::query::physical_plan::CreateReturnSlot::Property { property, name } => {
                output.insert(name.clone(), row_property_value(&merged.row, property));
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

fn project_removed_node(
    plan: &crate::query::physical_plan::PhysicalRemoveProperty,
    updated: UpdatedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            crate::query::physical_plan::CreateReturnSlot::Node { name } => {
                output.insert(name.clone(), removed_node_value(plan, &updated, hydrate));
            }
            crate::query::physical_plan::CreateReturnSlot::Property { property, name } => {
                output.insert(name.clone(), row_property_value(&updated.row, property));
            }
        }
    }
    serde_json::Value::Object(output)
}

fn project_detach_deleted_node(
    plan: &crate::query::physical_plan::PhysicalDetachDeleteNode,
    deleted: DeletedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            crate::query::physical_plan::CreateReturnSlot::Node { name } => {
                output.insert(
                    name.clone(),
                    detach_deleted_node_value(plan, &deleted, hydrate),
                );
            }
            crate::query::physical_plan::CreateReturnSlot::Property { property, name } => {
                output.insert(name.clone(), row_property_value(&deleted.row, property));
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

fn merged_node_value(
    plan: &crate::query::physical_plan::PhysicalMergeNode,
    merged: &MergedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut node = if hydrate {
        merged.row.as_object().cloned().unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    node.insert(
        "_id".to_string(),
        serde_json::json!({
            "table": &plan.label,
            "id": &merged.node_id,
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

fn removed_node_value(
    plan: &crate::query::physical_plan::PhysicalRemoveProperty,
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

fn detach_deleted_node_value(
    plan: &crate::query::physical_plan::PhysicalDetachDeleteNode,
    deleted: &DeletedNode,
    hydrate: bool,
) -> serde_json::Value {
    let mut node = if hydrate {
        deleted.row.as_object().cloned().unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    node.insert(
        "_id".to_string(),
        serde_json::json!({
            "table": &plan.label,
            "id": &deleted.node_id,
        }),
    );
    node.insert(
        "_labels".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(plan.label.clone())]),
    );
    serde_json::Value::Object(node)
}

fn created_relationship_node_value(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    side: crate::query::logical_plan::BindingSide,
    source_id: &str,
    target_id: &str,
    hydrate: bool,
) -> safety::GraphResult<serde_json::Value> {
    let (table_oid, label, node_id) =
        created_relationship_node_parts(plan, side, source_id, target_id)?;
    let mut node = if hydrate {
        hydrate_required_node(&crate::query::execute::GqlNodeCoordinate {
            table_oid,
            node_id: node_id.to_string(),
        })?
        .as_object()
        .cloned()
        .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    node.insert(
        "_id".to_string(),
        serde_json::json!({
            "table": label,
            "id": node_id,
        }),
    );
    node.insert(
        "_labels".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(label.to_string())]),
    );
    Ok(serde_json::Value::Object(node))
}

fn created_relationship_node_property(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    side: crate::query::logical_plan::BindingSide,
    source_id: &str,
    target_id: &str,
    property: &str,
) -> safety::GraphResult<serde_json::Value> {
    let (table_oid, _, node_id) =
        created_relationship_node_parts(plan, side, source_id, target_id)?;
    let row = hydrate_required_node(&crate::query::execute::GqlNodeCoordinate {
        table_oid,
        node_id: node_id.to_string(),
    })?;
    Ok(row_property_value(&row, property))
}

fn created_relationship_node_parts<'a>(
    plan: &'a crate::query::physical_plan::PhysicalCreateRelationship,
    side: crate::query::logical_plan::BindingSide,
    source_id: &'a str,
    target_id: &'a str,
) -> safety::GraphResult<(u32, &'a str, &'a str)> {
    match side {
        crate::query::logical_plan::BindingSide::Source => {
            Ok((plan.source_table_oid, &plan.source_label, source_id))
        }
        crate::query::logical_plan::BindingSide::Target => {
            Ok((plan.target_table_oid, &plan.target_label, target_id))
        }
        crate::query::logical_plan::BindingSide::PathNode(_) => {
            Err(safety::GraphError::GqlExecution {
                reason: "relationship CREATE cannot project path-node return slots".to_string(),
            })
        }
    }
}

fn created_relationship_value(
    plan: &crate::query::physical_plan::PhysicalCreateRelationship,
    source_id: &str,
    target_id: &str,
    created: &CreatedRelationship,
    hydrate: bool,
) -> serde_json::Value {
    let mut relationship = serde_json::json!({
        "_type": &plan.rel_type,
        "_start": created_relationship_endpoint(&plan.source_label, source_id),
        "_end": created_relationship_endpoint(&plan.target_label, target_id),
    });
    if hydrate {
        merge_created_relationship_properties(&mut relationship, &created.row);
    }
    relationship
}

fn created_relationship_endpoint(label: &str, node_id: &str) -> serde_json::Value {
    serde_json::json!({
        "table": label,
        "id": node_id,
    })
}

fn merge_created_relationship_properties(
    relationship: &mut serde_json::Value,
    row: &serde_json::Value,
) {
    let (Some(relationship), Some(row)) = (relationship.as_object_mut(), row.as_object()) else {
        return;
    };
    for (key, value) in row {
        if key.starts_with('_') {
            continue;
        }
        relationship.insert(key.clone(), value.clone());
    }
}

fn row_property_value(row: &serde_json::Value, property: &str) -> serde_json::Value {
    let mut current = row;
    for part in property.split('.') {
        match current {
            serde_json::Value::Object(map) => {
                let Some(next) = map.get(part) else {
                    return serde_json::Value::Null;
                };
                current = next;
            }
            _ => return serde_json::Value::Null,
        }
    }
    current.clone()
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

pub(super) fn gql_error_to_graph_error(err: crate::gql::errors::GqlError) -> safety::GraphError {
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

pub(super) fn gql_params(
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
    let governor = gql_query_governor()?;
    hydrate_gql_rows_governed(rows, needed, &governor)
}

fn hydrate_gql_rows_governed(
    rows: &[crate::query::execute::GqlRow],
    needed: bool,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<crate::query::value::HydratedRows> {
    let mut hydrated = crate::query::value::HydratedRows::new();
    if !needed {
        return Ok(hydrated);
    }
    for row in rows {
        for coordinate in std::iter::once(Some(&row.source))
            .chain(std::iter::once(row.target.as_ref()))
            .chain(row.path_nodes.iter().map(Some))
            .flatten()
        {
            let key = (coordinate.table_oid, coordinate.node_id.clone());
            if hydrated.contains_key(&key) {
                continue;
            }
            let node = hydrate_required_node_governed(coordinate, governor)?;
            hydrated.insert(key, node);
        }
    }
    Ok(hydrated)
}

/// Check source-row visibility before exposing graph coordinates or topology.
///
/// GQL's projection may be served from the in-memory graph even when callers
/// request `hydrate := false`. Reusing the PostgreSQL hydration boundary here
/// makes RLS authoritative for every coordinate-bearing result shape.
fn ensure_gql_rows_visible(
    rows: &[crate::query::execute::GqlRow],
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<()> {
    let (coordinate_count, key_bytes) =
        rows.iter()
            .try_fold((0usize, 0usize), |(count, bytes), row| {
                std::iter::once(&row.source)
                    .chain(row.target.iter())
                    .chain(row.path_nodes.iter())
                    .chain(row.join_node_slots.iter().flatten().flatten())
                    .chain(row.join_path_nodes.iter().flatten().flatten().flatten())
                    .try_fold((count, bytes), |(count, bytes), coordinate| {
                        Ok::<_, safety::GraphError>((
                            count.checked_add(1).ok_or_else(|| {
                                safety::GraphError::Internal(
                                    "visibility coordinate count overflowed".to_string(),
                                )
                            })?,
                            bytes.checked_add(coordinate.node_id.len()).ok_or_else(|| {
                                safety::GraphError::Internal(
                                    "visibility coordinate bytes overflowed".to_string(),
                                )
                            })?,
                        ))
                    })
            })?;
    let _workspace =
        crate::sql_hydration::reserve_hydration_workspace(governor, coordinate_count, key_bytes)?;
    let mut ids_by_table = std::collections::HashMap::<u32, Vec<String>>::new();
    for row in rows {
        for coordinate in std::iter::once(&row.source)
            .chain(row.target.iter())
            .chain(row.path_nodes.iter())
            .chain(row.join_node_slots.iter().flatten().flatten())
            .chain(row.join_path_nodes.iter().flatten().flatten().flatten())
        {
            ids_by_table
                .entry(coordinate.table_oid)
                .or_default()
                .push(coordinate.node_id.clone());
        }
    }
    let visible = crate::sql_hydration::visible_node_keys_governed(&ids_by_table, governor)?;
    let hidden = ids_by_table
        .into_iter()
        .flat_map(|(table_oid, ids)| ids.into_iter().map(move |node_id| (table_oid, node_id)))
        .any(|key| !visible.contains(&key));
    if hidden {
        return Err(safety::GraphError::GqlExecution {
            reason: "GQL source row is not visible to the current role".to_string(),
        });
    }
    Ok(())
}

/// Check source-row visibility for node-only GQL scans.
fn ensure_gql_node_rows_visible(
    rows: &[crate::query::execute::GqlNodeRow],
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<()> {
    let mut visible_rows = rows.iter().filter(|row| !row.optional_null);
    let row_count = visible_rows.clone().count();
    let key_bytes = visible_rows.try_fold(0usize, |bytes, row| {
        bytes.checked_add(row.node.node_id.len()).ok_or_else(|| {
            safety::GraphError::Internal("visibility key bytes overflowed".to_string())
        })
    })?;
    let _workspace =
        crate::sql_hydration::reserve_hydration_workspace(governor, row_count, key_bytes)?;
    let mut ids_by_table = std::collections::HashMap::<u32, Vec<String>>::new();
    for row in rows.iter().filter(|row| !row.optional_null) {
        ids_by_table
            .entry(row.node.table_oid)
            .or_default()
            .push(row.node.node_id.clone());
    }
    let visible = crate::sql_hydration::visible_node_keys_governed(&ids_by_table, governor)?;
    if ids_by_table
        .into_iter()
        .flat_map(|(table_oid, ids)| ids.into_iter().map(move |node_id| (table_oid, node_id)))
        .any(|key| !visible.contains(&key))
    {
        return Err(safety::GraphError::GqlExecution {
            reason: "GQL source row is not visible to the current role".to_string(),
        });
    }
    Ok(())
}

/// Check mapped relationship source-row visibility before exposing graph
/// topology.
fn ensure_gql_relationship_rows_visible(
    rows: &[crate::query::execute::GqlRow],
    plan: &crate::query::physical_plan::PhysicalPlan,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<()> {
    let Some(edge_mapping) = &plan.edge_mapping else {
        return Ok(());
    };
    let _relationship_ids_memory = reserve_relationship_id_workspace(governor, rows.len())?;
    let mut relationship_ids = Vec::with_capacity(rows.len());
    for row in rows {
        if row.rel_start.is_none() || row.rel_end.is_none() {
            continue;
        }
        relationship_ids.push(row.relationship_id);
    }
    ensure_relationship_source_keys_visible(edge_mapping, relationship_ids, governor)
}

fn relationship_identity_snapshot() -> Vec<Option<crate::edge_store::RelationshipIdentity>> {
    let mut identities = ENGINE.with(|engine| engine.borrow().relationship_identities.clone());
    identities.extend(
        crate::projection::tx_delta::relationship_identities()
            .into_iter()
            .map(Some),
    );
    identities
}

fn reserve_relationship_id_workspace(
    governor: &crate::resource::ResourceGovernor,
    count: usize,
) -> safety::GraphResult<crate::resource::ResourceLease<'_>> {
    let bytes = count
        .checked_mul(std::mem::size_of::<Option<crate::edge_store::RelationshipId>>())
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal(
                "relationship identity list workspace overflowed".to_string(),
            )
        })?;
    governor
        .reserve_memory(crate::resource::ResourcePhase::QueryHydrate, bytes)
        .map_err(crate::safety::resource_limit_error)
}

/// Check mapped relationship source-row visibility for multi-pattern joins.
fn ensure_gql_join_relationship_rows_visible(
    rows: &[crate::query::execute::GqlRow],
    plan: &crate::query::physical_plan::PhysicalJoinPlan,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<()> {
    if plan
        .patterns
        .iter()
        .all(|pattern| pattern.edge_mapping.is_none())
    {
        return Ok(());
    }
    for (pattern_idx, pattern) in plan.patterns.iter().enumerate() {
        let Some(edge_mapping) = &pattern.edge_mapping else {
            continue;
        };
        let relationship_count = rows.iter().try_fold(0usize, |count, row| {
            let Some(paths) = &row.join_path_relationships else {
                return Ok(count);
            };
            let path = paths.get(pattern_idx).ok_or_else(|| {
                safety::GraphError::Internal(format!(
                    "join row is missing relationship path slot {pattern_idx}"
                ))
            })?;
            count
                .checked_add(path.as_ref().map_or(0, Vec::len))
                .ok_or_else(|| {
                    safety::GraphError::Internal("join relationship count overflowed".to_string())
                })
        })?;
        let _relationship_ids_memory =
            reserve_relationship_id_workspace(governor, relationship_count)?;
        let mut relationship_ids = Vec::with_capacity(relationship_count);
        for row in rows {
            let Some(paths) = &row.join_path_relationships else {
                continue;
            };
            let Some(path) = paths.get(pattern_idx) else {
                return Err(safety::GraphError::Internal(format!(
                    "join row is missing relationship path slot {pattern_idx}"
                )));
            };
            let Some(relationships) = path else {
                continue;
            };
            relationship_ids.extend(
                relationships
                    .iter()
                    .map(|relationship| relationship.relationship_id),
            );
        }
        ensure_relationship_source_keys_visible(edge_mapping, relationship_ids, governor)?;
    }
    Ok(())
}

/// Check mapped relationship source-row visibility for wildcard path outputs.
fn ensure_gql_wildcard_relationship_rows_visible(
    rows: &[crate::query::execute::GqlRow],
    plan: &crate::query::physical_plan::PhysicalWildcardPathPlan,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<()> {
    if plan.edge_mappings_by_id.is_empty() {
        return Ok(());
    }
    let relationship_count = rows.iter().try_fold(0usize, |count, row| {
        count
            .checked_add(row.path_relationships.len())
            .ok_or_else(|| {
                safety::GraphError::Internal("wildcard relationship count overflowed".to_string())
            })
    })?;
    let mut identity_workspace =
        crate::sql_hydration::reserve_hydration_workspace(governor, relationship_count, 0)?;
    let mut ids_by_mapping =
        std::collections::BTreeMap::<u64, Vec<Option<crate::edge_store::RelationshipId>>>::new();

    let mut relationship_index = 0usize;
    for row in rows {
        for relationship in &row.path_relationships {
            check_query_hydrate_progress(governor, relationship_index)?;
            relationship_index = relationship_index.saturating_add(1);
            let Some((relationship_id, mapping_id)) = wildcard_relationship_mapping(
                relationship,
                &plan.edge_mappings_by_id,
                &plan.mapped_rel_types,
                &mut identity_workspace,
            )?
            else {
                continue;
            };
            ids_by_mapping
                .entry(mapping_id)
                .or_default()
                .push(Some(relationship_id));
        }
    }

    for (mapping_id, relationship_ids) in ids_by_mapping {
        let Some(edge_mapping) = plan.edge_mappings_by_id.get(&mapping_id) else {
            return Err(safety::GraphError::Internal(format!(
                "wildcard path relationship mapping {mapping_id} is missing from the physical plan"
            )));
        };
        ensure_relationship_source_keys_visible(edge_mapping, relationship_ids, governor)?;
    }
    Ok(())
}

fn wildcard_relationship_mapping(
    relationship: &crate::query::execute::GqlPathRelationship,
    edge_mappings_by_id: &std::collections::BTreeMap<
        u64,
        crate::query::catalog_snapshot::EdgeMappingInfo,
    >,
    mapped_rel_types: &std::collections::BTreeSet<String>,
    workspace: &mut crate::resource::ResourceLease<'_>,
) -> safety::GraphResult<Option<(crate::edge_store::RelationshipId, u64)>> {
    let Some(relationship_id) = relationship.relationship_id else {
        if mapped_rel_types.contains(&relationship.rel_type) {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL mapped relationship type `{}` has no stable source identity",
                    relationship.rel_type
                ),
            });
        }
        return Ok(None);
    };
    let identity = relationship_identity_owned(relationship_id, workspace)?;
    if !edge_mappings_by_id.contains_key(&identity.mapping_id) {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL relationship identity `{relationship_id}` references mapping {} outside the wildcard plan",
                identity.mapping_id
            ),
        });
    }
    Ok(Some((relationship_id, identity.mapping_id)))
}

fn ensure_relationship_source_keys_visible<I>(
    edge_mapping: &crate::query::catalog_snapshot::EdgeMappingInfo,
    relationship_ids: I,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<()>
where
    I: IntoIterator<Item = Option<crate::edge_store::RelationshipId>>,
{
    let relationship_ids = relationship_ids.into_iter().collect::<Vec<_>>();
    let mut workspace =
        crate::sql_hydration::reserve_hydration_workspace(governor, relationship_ids.len(), 0)?;
    let mut source_keys = Vec::with_capacity(relationship_ids.len());
    for (index, relationship_id) in relationship_ids.into_iter().enumerate() {
        if index.is_multiple_of(1_024) {
            crate::resource::check_postgres_interrupts();
            governor
                .check_elapsed(crate::resource::ResourcePhase::QueryHydrate)
                .map_err(crate::safety::resource_limit_error)?;
        }
        let identity =
            relationship_source_identity_owned(edge_mapping, relationship_id, &mut workspace)?;
        source_keys.push(identity.source_key);
    }
    if source_keys.is_empty() {
        return Ok(());
    }
    source_keys.sort();
    source_keys.dedup();
    let source_key_bytes = source_keys.iter().try_fold(0usize, |bytes, key| {
        bytes.checked_add(key.len()).ok_or_else(|| {
            safety::GraphError::Internal("relationship visibility bytes overflowed".to_string())
        })
    })?;
    workspace
        .try_grow(
            source_key_bytes
                .checked_mul(2)
                .and_then(crate::resource::ByteCount::from_usize)
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "relationship visibility bytes overflowed".to_string(),
                    )
                })?,
        )
        .map_err(crate::safety::resource_limit_error)?;

    acl::check_table_acl(edge_mapping.edge_table_oid)?;
    let table_name = regclass_text(edge_mapping.edge_table_oid)?;
    let source_key_predicate = relationship_source_key_predicate(edge_mapping);
    let query = format!(
        "SELECT {source_key_predicate} AS graph_relationship_id
           FROM {table_name} edge_row
          WHERE {source_key_predicate} = ANY($1::text[])
          LIMIT $2"
    );
    let mut visible = std::collections::HashSet::new();
    Spi::connect(|client| {
        let result = client
            .select(
                &query,
                None,
                &[
                    source_keys.clone().into(),
                    i64::try_from(source_keys.len()).unwrap_or(i64::MAX).into(),
                ],
            )
            .map_err(|err| {
                safety::GraphError::Internal(format!("relationship visibility check failed: {err}"))
            })?;
        for (index, row) in result.into_iter().enumerate() {
            if index.is_multiple_of(1_024) {
                crate::resource::check_postgres_interrupts();
                governor
                    .check_elapsed(crate::resource::ResourcePhase::QueryHydrate)
                    .map_err(crate::safety::resource_limit_error)?;
            }
            let source_key = row
                .get::<String>(1)
                .map_err(|err| {
                    safety::GraphError::Internal(format!(
                        "relationship visibility key read failed: {err}"
                    ))
                })?
                .ok_or_else(|| {
                    safety::GraphError::Internal(
                        "relationship visibility returned NULL key".to_string(),
                    )
                })?;
            visible.insert(source_key);
        }
        Ok::<(), safety::GraphError>(())
    })?;
    if source_keys
        .into_iter()
        .any(|source_key| !visible.contains(&source_key))
    {
        return Err(safety::GraphError::GqlExecution {
            reason: "GQL relationship source row is not visible to the current role".to_string(),
        });
    }
    workspace.retain_until_governor_drop();
    Ok(())
}

fn hydrate_gql_node_rows(
    rows: &[crate::query::execute::GqlNodeRow],
    needed: bool,
) -> safety::GraphResult<crate::query::value::HydratedRows> {
    let governor = gql_query_governor()?;
    hydrate_gql_node_rows_governed(rows, needed, &governor)
}

fn hydrate_gql_node_rows_governed(
    rows: &[crate::query::execute::GqlNodeRow],
    needed: bool,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<crate::query::value::HydratedRows> {
    let mut hydrated = crate::query::value::HydratedRows::new();
    if !needed {
        return Ok(hydrated);
    }
    for row in rows {
        if row.optional_null {
            continue;
        }
        let key = (row.node.table_oid, row.node.node_id.clone());
        if hydrated.contains_key(&key) {
            continue;
        }
        let node = hydrate_required_node_governed(&row.node, governor)?;
        hydrated.insert(key, node);
    }
    Ok(hydrated)
}

fn hydrate_gql_relationship_rows_governed(
    rows: &[crate::query::execute::GqlRow],
    plan: &crate::query::physical_plan::PhysicalPlan,
    needed: bool,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<crate::query::value::HydratedRelationships> {
    let mut hydrated = crate::query::value::HydratedRelationships::new();
    if !needed {
        return Ok(hydrated);
    }
    let Some(edge_mapping) = &plan.edge_mapping else {
        return Ok(hydrated);
    };
    let key_bytes = rows.iter().try_fold(0usize, |total, row| {
        total
            .checked_add(row.source.node_id.len())
            .and_then(|bytes| {
                row.target.as_ref().map_or(Some(bytes), |target| {
                    bytes.checked_add(target.node_id.len())
                })
            })
            .ok_or_else(|| {
                safety::GraphError::Internal(
                    "relationship hydration key size overflowed".to_string(),
                )
            })
    })?;
    let mut workspace =
        crate::sql_hydration::reserve_hydration_workspace(governor, rows.len(), key_bytes)?;
    for row in rows {
        check_query_hydrate_progress(governor, hydrated.len())?;
        let (Some(start), Some(end)) = (&row.rel_start, &row.rel_end) else {
            continue;
        };
        let key = crate::query::value::RelationshipKey::with_relationship_id(
            &plan.rel_type,
            row.relationship_id,
            start,
            end,
        );
        if hydrated.contains_key(&key) {
            continue;
        }
        let relationship =
            hydrate_required_relationship(edge_mapping, row.relationship_id, &mut workspace)?;
        hydrated.insert(key, relationship.0);
    }
    workspace.retain_until_governor_drop();
    Ok(hydrated)
}

fn check_query_hydrate_progress(
    governor: &crate::resource::ResourceGovernor,
    index: usize,
) -> safety::GraphResult<()> {
    if index.is_multiple_of(1_024) {
        crate::resource::check_postgres_interrupts();
        governor
            .check_elapsed(crate::resource::ResourcePhase::QueryHydrate)
            .map_err(crate::safety::resource_limit_error)?;
    }
    Ok(())
}

fn hydrate_required_relationship(
    edge_mapping: &crate::query::catalog_snapshot::EdgeMappingInfo,
    relationship_id: Option<crate::edge_store::RelationshipId>,
    workspace: &mut crate::resource::ResourceLease<'_>,
) -> safety::GraphResult<pgrx::JsonB> {
    let identity = relationship_source_identity_owned(edge_mapping, relationship_id, workspace)?;
    let table_name = regclass_text(edge_mapping.edge_table_oid)?;
    let source_key_predicate = relationship_source_key_predicate(edge_mapping);
    let json_sizes = Spi::connect(|client| {
        let query = format!(
            "SELECT pg_catalog.pg_column_size(pg_catalog.to_jsonb(edge_row.*))::bigint,
                    pg_catalog.octet_length(pg_catalog.to_jsonb(edge_row.*)::text)::bigint
               FROM {table_name} edge_row
              WHERE {source_key_predicate} = $1
              LIMIT 1"
        );
        let result = client
            .select(&query, None, &[identity.source_key.as_str().into()])
            .map_err(|err| {
                safety::GraphError::Internal(format!(
                    "relationship hydration size preflight failed: {err}"
                ))
            })?;
        if result.is_empty() {
            return Ok(None);
        }
        let row = result.first();
        let binary = row.get::<i64>(1).map_err(|err| {
            safety::GraphError::Internal(format!(
                "relationship hydration JSONB size read failed: {err}"
            ))
        })?;
        let text = row.get::<i64>(2).map_err(|err| {
            safety::GraphError::Internal(format!(
                "relationship hydration JSON text size read failed: {err}"
            ))
        })?;
        Ok::<_, safety::GraphError>(binary.zip(text))
    })?
    .ok_or_else(|| safety::GraphError::GqlExecution {
        reason: format!(
            "GQL relationship source row `{}` is not visible in edge table OID {}",
            identity.source_key, edge_mapping.edge_table_oid
        ),
    })?;
    crate::sql_hydration::reserve_jsonb_materialization(
        workspace,
        1,
        u64::try_from(json_sizes.0.max(0)).unwrap_or(0),
        u64::try_from(json_sizes.1.max(0)).unwrap_or(0),
    )?;
    let mut sql = format!(
        "SELECT to_jsonb(edge_row.*)
         FROM {table_name} edge_row
         WHERE {source_key_predicate} = $1"
    );
    let args = vec![identity.source_key.as_str().into()];
    sql.push_str(" LIMIT 1");
    Spi::connect(|client| {
        let result = client.select(&sql, None, &args).map_err(|err| {
            safety::GraphError::Internal(format!("relationship hydration failed: {err}"))
        })?;
        if result.is_empty() {
            return Err(safety::GraphError::GqlExecution {
                reason: format!(
                    "GQL relationship source row `{}` is not visible in edge table OID {}",
                    identity.source_key, edge_mapping.edge_table_oid
                ),
            });
        }
        result
            .first()
            .get::<pgrx::JsonB>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!("relationship hydration read failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal(
                    "relationship hydration returned a NULL json row".to_string(),
                )
            })
    })
}

fn relationship_source_identity<'a>(
    edge_mapping: &crate::query::catalog_snapshot::EdgeMappingInfo,
    relationship_identities: &'a [Option<crate::edge_store::RelationshipIdentity>],
    relationship_id: Option<crate::edge_store::RelationshipId>,
) -> safety::GraphResult<&'a crate::edge_store::RelationshipIdentity> {
    let relationship_id = relationship_id.ok_or_else(|| safety::GraphError::GqlExecution {
        reason: "GQL relationship hydration requires a stable source-row relationship identity"
            .to_string(),
    })?;
    let identity = relationship_identities
        .get(relationship_id as usize)
        .and_then(Option::as_ref)
        .ok_or_else(|| safety::GraphError::GqlExecution {
            reason: format!("GQL relationship identity `{relationship_id}` is not available"),
        })?;
    if identity.mapping_id != edge_mapping.mapping_id {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL relationship identity `{relationship_id}` belongs to mapping {}, not {}",
                identity.mapping_id, edge_mapping.mapping_id
            ),
        });
    }
    Ok(identity)
}

fn relationship_source_identity_owned(
    edge_mapping: &crate::query::catalog_snapshot::EdgeMappingInfo,
    relationship_id: Option<crate::edge_store::RelationshipId>,
    workspace: &mut crate::resource::ResourceLease<'_>,
) -> safety::GraphResult<crate::edge_store::RelationshipIdentity> {
    let relationship_id = relationship_id.ok_or_else(|| safety::GraphError::GqlExecution {
        reason: "GQL relationship hydration requires a stable source-row relationship identity"
            .to_string(),
    })?;
    let identity = relationship_identity_owned(relationship_id, workspace)?;
    if identity.mapping_id != edge_mapping.mapping_id {
        return Err(safety::GraphError::GqlExecution {
            reason: format!(
                "GQL relationship identity `{relationship_id}` belongs to mapping {}, not {}",
                identity.mapping_id, edge_mapping.mapping_id
            ),
        });
    }
    Ok(identity)
}

fn relationship_identity_owned(
    relationship_id: crate::edge_store::RelationshipId,
    workspace: &mut crate::resource::ResourceLease<'_>,
) -> safety::GraphResult<crate::edge_store::RelationshipIdentity> {
    let index = relationship_id as usize;
    let base_count = ENGINE.with(|engine| engine.borrow().relationship_identities.len());
    if index < base_count {
        return ENGINE.with(|engine| {
            let engine = engine.borrow();
            clone_charged_relationship_identity(
                engine
                    .relationship_identities
                    .get(index)
                    .and_then(Option::as_ref),
                relationship_id,
                workspace,
            )
        });
    }
    crate::projection::tx_delta::with_relationship_identity(index - base_count, |identity| {
        clone_charged_relationship_identity(identity, relationship_id, workspace)
    })
}

fn clone_charged_relationship_identity(
    identity: Option<&crate::edge_store::RelationshipIdentity>,
    relationship_id: crate::edge_store::RelationshipId,
    workspace: &mut crate::resource::ResourceLease<'_>,
) -> safety::GraphResult<crate::edge_store::RelationshipIdentity> {
    let identity = identity.ok_or_else(|| safety::GraphError::GqlExecution {
        reason: format!("GQL relationship identity `{relationship_id}` is not available"),
    })?;
    let bytes = std::mem::size_of::<crate::edge_store::RelationshipIdentity>()
        .checked_add(identity.source_key.len())
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal(
                "relationship identity workspace estimate overflowed".to_string(),
            )
        })?;
    workspace
        .try_grow(bytes)
        .map_err(crate::safety::resource_limit_error)?;
    Ok(identity.clone())
}

fn relationship_source_key_predicate(
    edge_mapping: &crate::query::catalog_snapshot::EdgeMappingInfo,
) -> String {
    relationship_source_key_predicate_for_alias("edge_row", &edge_mapping.source_key_columns)
}

fn relationship_source_key_predicate_for_alias(
    alias: &str,
    source_key_columns: &crate::builder::PrimaryKeySpec,
) -> String {
    let columns = source_key_columns.columns();
    if columns.len() > 1 {
        let parts = columns
            .iter()
            .map(|column| format!("{alias}.{}::text", quote_ident(column)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("jsonb_build_array({parts})::text")
    } else if let Some(column) = columns.first() {
        format!("{alias}.{}::text", quote_ident(column))
    } else {
        "NULL::text".to_string()
    }
}

fn hydrate_required_node(
    coordinate: &crate::query::execute::GqlNodeCoordinate,
) -> safety::GraphResult<serde_json::Value> {
    let governor = gql_query_governor()?;
    hydrate_required_node_governed(coordinate, &governor)
}

fn hydrate_required_node_governed(
    coordinate: &crate::query::execute::GqlNodeCoordinate,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<serde_json::Value> {
    crate::sql_hydration::hydrate_node_governed(
        coordinate.table_oid,
        &coordinate.node_id,
        governor,
    )?
    .map(|json| json.0)
    .ok_or_else(|| safety::GraphError::GqlExecution {
        reason: format!(
            "GQL could not hydrate node `{}` from table OID {}",
            coordinate.node_id, coordinate.table_oid
        ),
    })
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
                    schema_reversed: false,
                    relationship_id: None,
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

#[cfg(feature = "pg_test")]
#[pg_extern(schema = "graph", name = "_test_recheck_delete_edge_predicate")]
fn test_recheck_delete_edge_predicate(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
    edge_label: &str,
    target_property: &str,
    expected_target_value: i64,
) -> bool {
    with_panic_boundary("_test_recheck_delete_edge_predicate()", || {
        super::admin::require_graph_admin_result().unwrap_or_else(|err| err.report());
        let source_table_oid = source_table.to_u32();
        let target_table_oid = target_table.to_u32();
        let source = crate::query::execute::GqlNodeCoordinate {
            table_oid: source_table_oid,
            node_id: source_id.to_string(),
        };
        let target = crate::query::execute::GqlNodeCoordinate {
            table_oid: target_table_oid,
            node_id: target_id.to_string(),
        };
        let plan = crate::query::physical_plan::PhysicalPlan {
            optional: false,
            source_var: "u".to_string(),
            source_table_oid,
            source_label: "source".to_string(),
            rel_type: edge_label.to_string(),
            rel_var: Some("r".to_string()),
            direction: crate::query::logical_plan::BoundDirection::Out,
            hops: crate::query::logical_plan::HopBounds {
                variable: false,
                min: 1,
                max: 1,
            },
            edge_mapping: None,
            target_var: "v".to_string(),
            target_table_oid,
            target_label: "target".to_string(),
            returns: Vec::new(),
            distinct_stages: Vec::new(),
            distinct: false,
            predicate: Some(crate::query::logical_plan::Predicate::Compare {
                lhs: crate::query::logical_plan::ValueExpr::Property {
                    side: crate::query::logical_plan::BindingSide::Target,
                    property: target_property.to_string(),
                },
                op: crate::query::logical_plan::BoundCmpOp::Eq,
                rhs: Some(crate::query::logical_plan::ValueExpr::Literal(
                    serde_json::json!(expected_target_value),
                )),
            }),
            order_by: Vec::new(),
            skip: None,
            limit: None,
        };
        let row = crate::query::execute::GqlRow {
            source: source.clone(),
            target: Some(target.clone()),
            rel_start: Some(source.clone()),
            rel_end: Some(target.clone()),
            relationship_id: None,
            path_nodes: vec![source, target],
            path_relationships: Vec::new(),
            join_node_slots: None,
            join_path_nodes: None,
            join_relationships: None,
            join_path_relationships: None,
        };
        lock_and_recheck_edge_write(&plan, &row, &crate::query::value::QueryParams::new(), None)
            .unwrap_or_else(|err| err.report());
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge_mapping(mapping_id: u64) -> crate::query::catalog_snapshot::EdgeMappingInfo {
        crate::query::catalog_snapshot::EdgeMappingInfo {
            mapping_id,
            edge_table_oid: 30,
            source_table_oid: 10,
            target_table_oid: 20,
            source_column: "user_id".to_string(),
            target_column: "company_id".to_string(),
            source_key_columns: crate::builder::PrimaryKeySpec::from_columns(vec![
                "tenant_id".to_string(),
                "edge_id".to_string(),
            ]),
            bidirectional: false,
            label_column: None,
        }
    }

    #[test]
    fn relationship_identity_validation_rejects_missing_and_wrong_mapping() {
        let mapping = edge_mapping(7);
        let identities = vec![
            None,
            Some(crate::edge_store::RelationshipIdentity {
                mapping_id: 8,
                source_key: "edge-1".to_string(),
            }),
        ];

        assert!(relationship_source_identity(&mapping, &identities, None)
            .unwrap_err()
            .to_string()
            .contains("stable source-row relationship identity"));
        assert!(relationship_source_identity(&mapping, &identities, Some(9))
            .unwrap_err()
            .to_string()
            .contains("is not available"));
        assert!(relationship_source_identity(&mapping, &identities, Some(1))
            .unwrap_err()
            .to_string()
            .contains("belongs to mapping 8, not 7"));
    }

    #[test]
    fn relationship_source_key_predicate_uses_registered_primary_key_columns() {
        assert_eq!(
            relationship_source_key_predicate(&edge_mapping(7)),
            "jsonb_build_array(edge_row.\"tenant_id\"::text, edge_row.\"edge_id\"::text)::text"
        );
    }

    #[test]
    fn wildcard_relationship_mapping_fails_closed_for_mapped_identity_gaps() {
        let coordinate = crate::query::execute::GqlNodeCoordinate {
            table_oid: 10,
            node_id: "node-1".to_string(),
        };
        let relationship =
            |rel_type: &str, relationship_id| crate::query::execute::GqlPathRelationship {
                rel_type: rel_type.to_string(),
                start: coordinate.clone(),
                end: coordinate.clone(),
                relationship_id,
            };
        let mappings = [(7, edge_mapping(7))].into();
        let mapped_types = ["friend".to_string()].into();
        let governor = gql_query_governor().expect("test query governor");
        let mut workspace = crate::sql_hydration::reserve_hydration_workspace(&governor, 1, 0)
            .expect("test identity workspace");

        let missing = wildcard_relationship_mapping(
            &relationship("friend", None),
            &mappings,
            &mapped_types,
            &mut workspace,
        )
        .expect_err("mapped relationship without identity must fail closed");
        assert!(missing.to_string().contains("no stable source identity"));

        ENGINE.with(|engine| {
            engine.borrow_mut().relationship_identities =
                vec![Some(crate::edge_store::RelationshipIdentity {
                    mapping_id: 8,
                    source_key: "edge-1".to_string(),
                })];
        });
        let wrong_mapping = wildcard_relationship_mapping(
            &relationship("friend", Some(0)),
            &mappings,
            &mapped_types,
            &mut workspace,
        )
        .expect_err("identity outside wildcard mappings must fail closed");
        assert!(wrong_mapping
            .to_string()
            .contains("outside the wildcard plan"));

        assert_eq!(
            wildcard_relationship_mapping(
                &relationship("unmapped", None),
                &mappings,
                &mapped_types,
                &mut workspace,
            )
            .expect("unmapped relationship type may omit source-row identity"),
            None
        );
    }
}
