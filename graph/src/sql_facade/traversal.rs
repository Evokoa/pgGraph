use super::admin::{check_enabled, check_enabled_result, with_panic_boundary};
use super::runtime::{current_query_freshness, ensure_current_graph_for_query};
use super::*;

type DirectNodeRow = (
    name!(graph_id, String),
    name!(graph_name, String),
    name!(node_table, pgrx::pg_sys::Oid),
    name!(node_table_name, String),
    name!(node_id, String),
    name!(node_idx, i64),
    name!(node, Option<pgrx::JsonB>),
);

pub(super) type ShortestPathSqlRow = (
    i32,
    pgrx::pg_sys::Oid,
    String,
    Option<String>,
    Option<pgrx::JsonB>,
    String,
);

/// BFS traversal from a seed node.
///
/// See: `docs/user_guide/querying.mdx`
#[pg_extern(schema = "graph", cost = 1000)]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
pub(super) fn traverse(
    seed_table: pgrx::pg_sys::Oid,
    seed_id: &str,
    max_depth: default!(
        i32,
        "COALESCE(NULLIF(current_setting('graph.default_max_depth', true), '')::int, 5)"
    ),
    edge_types: default!(Option<Vec<String>>, "NULL"),
    direction: default!(&str, "'any'"),
    node_tables: default!(Option<Vec<pgrx::pg_sys::Oid>>, "NULL"),
    filter: default!(Option<pgrx::JsonB>, "NULL"),
    tenant: default!(Option<String>, "NULL"),
    strategy: default!(&str, "'bfs'"),
    uniqueness: default!(&str, "'node_global'"),
    include_start: default!(bool, "true"),
    hydrate: default!(bool, "true"),
    max_rows: default!(i32, 1000),
    row_offset: default!(i32, 0),
    max_nodes: default!(
        i32,
        "COALESCE(NULLIF(current_setting('graph.max_nodes', true), '')::int, 100000)"
    ),
    max_frontier: default!(
        i32,
        "COALESCE(NULLIF(current_setting('graph.max_frontier', true), '')::int, 100000)"
    ),
) -> TableIterator<
    'static,
    (
        name!(root_table, pgrx::pg_sys::Oid),
        name!(root_id, String),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_id, String),
        name!(depth, i32),
        name!(path, pgrx::JsonB),
        name!(edge_path, pgrx::JsonB),
        name!(node, Option<pgrx::JsonB>),
        name!(root_table_name, String),
        name!(node_table_name, String),
        name!(capped, bool),
    ),
> {
    with_panic_boundary("traverse()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let tenant_scope = crate::sql_sync::resolve_tenant_scope_for_query(
            tenant.as_deref(),
            &query_start.graph,
            &query_start.tables,
        )
        .unwrap_or_else(|err| err.report());
        let (direction, strategy, _uniqueness) = crate::sql_traversal::validate_traverse_options(
            direction,
            tenant_scope.as_deref(),
            strategy,
            uniqueness,
        )
        .unwrap_or_else(|err| err.report());
        let request = TraverseRequest {
            root_table: seed_table,
            root_id: seed_id,
            max_depth,
            edge_types: edge_types.as_deref(),
            node_tables: node_tables.as_deref(),
            filter: filter.as_ref(),
            tenant: tenant_scope.as_deref(),
            direction,
            strategy,
            include_start,
            hydrate,
            limit: max_rows,
            offset: row_offset,
            max_nodes,
            max_frontier,
        };
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        if max_depth == 0 {
            let rows = execute_depth_zero_lazy(
                &request,
                &query_start.tables,
                &query_start.edges,
                &query_start.filter_columns,
                &governor,
            )
            .unwrap_or_else(|err| err.report());
            return TableIterator::new(rows);
        }
        let coordinator = crate::sql_visibility::prepare_eager_visibility(
            &query_start.tables,
            &query_start.edges,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let context = coordinator.context(&governor);
        let rows = execute_traverse_rows_in_context(
            &request,
            &context,
            &query_start.tables,
            &query_start.filter_columns,
        )
        .unwrap_or_else(|err| err.report());

        TableIterator::new(rows)
    })
}

fn execute_depth_zero_lazy(
    request: &TraverseRequest<'_>,
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filter_columns: &[builder::RegisteredFilterColumn],
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Vec<crate::api_types::TraverseRow>> {
    let table = tables
        .iter()
        .find(|table| table.table_oid == request.root_table.to_u32())
        .ok_or_else(|| safety::GraphError::Internal("unregistered traversal root table".into()))?;
    let mut lazy = crate::sql_visibility::prepare_direct_identity_visibility(
        table, tables, edges, false, true,
    )?;
    let probe_plan = if lazy.table_requires_probe(table.table_oid) {
        crate::sql_visibility::reserve_direct_probe_plan(governor)?;
        crate::sql_visibility::prepare_direct_node_probe(table)?
    } else {
        None
    };
    let node_idx = ENGINE.with(|engine| {
        let engine = engine.borrow();
        engine
            .resolve(request.root_table.to_u32(), request.root_id)
            .or_else(|| {
                let table_is_tenanted = engine
                    .tenanted_table_oids
                    .contains(&request.root_table.to_u32());
                crate::projection::tx_delta::resolve_added_node(
                    request.root_table.to_u32(),
                    request.root_id,
                    request.tenant,
                    table_is_tenanted,
                )
            })
    });
    let Some(node_idx) = node_idx else {
        return Err(safety::GraphError::NodeNotFound {
            table: request.root_table.to_u32().to_string(),
            pk: request.root_id.to_string(),
        });
    };
    if lazy.table_requires_probe(table.table_oid) && probe_plan.is_none() {
        return Err(unsupported_direct_rls_key_type(table));
    }
    crate::sql_visibility::reserve_direct_visibility_candidate(governor, request.root_id)?;
    let batch = crate::sql_visibility::direct_visibility_batch(vec![
        crate::visibility::VisibilityCandidate::Node {
            sequence: 0,
            table_oid: request.root_table.to_u32(),
            source_key: request.root_id.to_string(),
            node_idx,
        },
    ])?;
    let verdicts = crate::sql_visibility::resolve_lazy_visibility_batch(
        &mut lazy,
        batch,
        probe_plan.as_ref(),
        governor,
    )?;
    if verdicts.verdicts() != [(0, crate::visibility::VisibilityVerdict::Visible)] {
        return Ok(Vec::new());
    }
    let visible_node = lazy.prove_visible_node(node_idx)?;
    let coordinator = crate::sql_visibility::direct_node_visibility_coordinator(visible_node);
    execute_traverse_rows_in_context(
        request,
        &coordinator.context(governor),
        tables,
        filter_columns,
    )
}

/// Resolve one registered node by graph name, label, and business id.
#[pg_extern(schema = "graph", cost = 100)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn get_node(
    graph_name: &str,
    label: &str,
    id: &str,
    hydrate: default!(bool, "true"),
    tenant: default!(Option<String>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_id, String),
        name!(node_idx, i64),
        name!(node, Option<pgrx::JsonB>),
    ),
> {
    with_panic_boundary("get_node()", || {
        let rows = direct_get_node_rows(
            graph_name,
            label,
            id,
            hydrate,
            tenant.as_deref(),
            graph_tenant,
            graph_namespace,
        )
        .unwrap_or_else(|err| err.report());
        TableIterator::new(rows)
    })
}

/// Return the one-hop neighbors for a registered node business id.
#[pg_extern(schema = "graph", cost = 1000)]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn get_neighbors(
    graph_name: &str,
    label: &str,
    id: &str,
    direction: default!(&str, "'any'"),
    edge_types: default!(Option<Vec<String>>, "NULL"),
    tenant: default!(Option<String>, "NULL"),
    hydrate: default!(bool, "true"),
    max_rows: default!(i32, 1000),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(root_table, pgrx::pg_sys::Oid),
        name!(root_id, String),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_id, String),
        name!(depth, i32),
        name!(path, pgrx::JsonB),
        name!(edge_path, pgrx::JsonB),
        name!(node, Option<pgrx::JsonB>),
        name!(root_table_name, String),
        name!(node_table_name, String),
        name!(capped, bool),
    ),
> {
    with_panic_boundary("get_neighbors()", || {
        let rows = direct_get_neighbors_rows(
            graph_name,
            label,
            id,
            direction,
            edge_types.as_deref(),
            tenant.as_deref(),
            hydrate,
            max_rows,
            graph_tenant,
            graph_namespace,
        )
        .unwrap_or_else(|err| err.report());
        TableIterator::new(rows)
    })
}

/// Multi-start BFS traversal.
///
/// This overload accepts parallel arrays because pgrx composite-array ergonomics
/// are awkward for callers today. Each `starts_tables[i]` pairs with
/// `start_ids[i]`.
#[pg_extern(schema = "graph", name = "traverse", cost = 1000)]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn traverse_many(
    start_tables: Vec<pgrx::pg_sys::Oid>,
    start_ids: Vec<String>,
    max_depth: default!(
        i32,
        "COALESCE(NULLIF(current_setting('graph.default_max_depth', true), '')::int, 5)"
    ),
    edge_types: default!(Option<Vec<String>>, "NULL"),
    direction: default!(&str, "'any'"),
    node_tables: default!(Option<Vec<pgrx::pg_sys::Oid>>, "NULL"),
    filter: default!(Option<pgrx::JsonB>, "NULL"),
    tenant: default!(Option<String>, "NULL"),
    strategy: default!(&str, "'bfs'"),
    uniqueness: default!(&str, "'node_global'"),
    include_start: default!(bool, "true"),
    hydrate: default!(bool, "true"),
    max_rows: default!(i32, 1000),
    row_offset: default!(i32, 0),
    max_nodes: default!(
        i32,
        "COALESCE(NULLIF(current_setting('graph.max_nodes', true), '')::int, 100000)"
    ),
    max_frontier: default!(
        i32,
        "COALESCE(NULLIF(current_setting('graph.max_frontier', true), '')::int, 100000)"
    ),
) -> TableIterator<
    'static,
    (
        name!(root_table, pgrx::pg_sys::Oid),
        name!(root_id, String),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_id, String),
        name!(depth, i32),
        name!(path, pgrx::JsonB),
        name!(edge_path, pgrx::JsonB),
        name!(node, Option<pgrx::JsonB>),
        name!(root_table_name, String),
        name!(node_table_name, String),
        name!(capped, bool),
    ),
> {
    with_panic_boundary("traverse_many()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let tenant_scope = crate::sql_sync::resolve_tenant_scope_for_query(
            tenant.as_deref(),
            &query_start.graph,
            &query_start.tables,
        )
        .unwrap_or_else(|err| err.report());
        if start_tables.len() != start_ids.len() {
            safety::GraphError::InvalidFilter {
                reason: "start_tables and start_ids must have the same length".to_string(),
            }
            .report();
        }
        let (direction, strategy, uniqueness) = crate::sql_traversal::validate_traverse_options(
            direction,
            tenant_scope.as_deref(),
            strategy,
            uniqueness,
        )
        .unwrap_or_else(|err| err.report());

        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let coordinator = crate::sql_visibility::prepare_eager_visibility(
            &query_start.tables,
            &query_start.edges,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let context = coordinator.context(&governor);
        let mut candidates = Vec::new();
        for (table, id) in start_tables.into_iter().zip(start_ids) {
            let request = TraverseRequest {
                root_table: table,
                root_id: &id,
                max_depth,
                edge_types: edge_types.as_deref(),
                node_tables: node_tables.as_deref(),
                filter: filter.as_ref(),
                tenant: tenant_scope.as_deref(),
                direction,
                strategy,
                include_start,
                hydrate,
                limit: max_rows,
                offset: row_offset,
                max_nodes,
                max_frontier,
            };
            let mut start_candidates = execute_traverse_candidates_in_context(
                &request,
                &context,
                &query_start.tables,
                &query_start.filter_columns,
            )
            .unwrap_or_else(|err| err.report());
            candidates.append(&mut start_candidates);
        }
        sort_traverse_candidates_for_many_governed(&mut candidates, &governor)
            .unwrap_or_else(|err| err.report());
        apply_traversal_uniqueness_governed(&mut candidates, uniqueness, &governor)
            .unwrap_or_else(|err| err.report());
        let rows = paginate_and_format_traverse_candidates_governed(
            candidates,
            hydrate,
            row_offset,
            max_rows,
            &governor,
            &query_start.tables,
        )
        .unwrap_or_else(|err| err.report());

        TableIterator::new(rows)
    })
}

/// Find shortest path between two nodes.
///
/// See: `docs/user_guide/querying.mdx`
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
pub(super) fn shortest_path(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
    max_depth: default!(i32, 20),
    hydrate: default!(bool, "true"),
) -> TableIterator<
    'static,
    (
        name!(step, i32),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_id, String),
        name!(edge_label, Option<String>),
        name!(node, Option<pgrx::JsonB>),
        name!(node_table_name, String),
    ),
> {
    with_panic_boundary("shortest_path()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        acl::check_table_acl(source_table.to_u32()).unwrap_or_else(|err| err.report());
        acl::check_table_acl(target_table.to_u32()).unwrap_or_else(|err| err.report());

        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());

        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let rows = shortest_path_rows_governed(
            source_table,
            source_id,
            target_table,
            target_id,
            max_depth,
            hydrate,
            &governor,
            &query_start.tables,
            &query_start.edges,
        )
        .unwrap_or_else(|err| err.report());

        TableIterator::new(rows)
    })
}

#[pg_extern(schema = "graph", name = "shortest_path")]
#[allow(clippy::type_complexity)]
fn shortest_path_typed(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
    max_depth: i32,
    hydrate: bool,
    edge_types: Vec<String>,
) -> TableIterator<
    'static,
    (
        name!(step, i32),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_id, String),
        name!(edge_label, Option<String>),
        name!(node, Option<pgrx::JsonB>),
        name!(node_table_name, String),
    ),
> {
    with_panic_boundary("shortest_path()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        acl::check_table_acl(source_table.to_u32()).unwrap_or_else(|err| err.report());
        acl::check_table_acl(target_table.to_u32()).unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let coordinator = crate::sql_visibility::prepare_eager_visibility(
            &query_start.tables,
            &query_start.edges,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let edge_type_filter = ENGINE
            .with(|engine| engine.borrow().resolve_edge_type_filter(Some(&edge_types)))
            .unwrap_or_else(|err| err.report());
        let context =
            coordinator.context_with_edge_type_filter(&governor, edge_type_filter.as_ref());
        let rows = shortest_path_rows_in_context(
            source_table,
            source_id,
            target_table,
            target_id,
            max_depth,
            hydrate,
            &context,
            &query_start.tables,
        )
        .unwrap_or_else(|err| err.report());
        TableIterator::new(rows)
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "shortest-path execution keeps SQL coordinates, bounds, budget, and query catalog explicit"
)]
pub(super) fn shortest_path_rows_governed(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
    max_depth: i32,
    hydrate: bool,
    governor: &crate::resource::ResourceGovernor,
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
) -> safety::GraphResult<Vec<ShortestPathSqlRow>> {
    let coordinator = crate::sql_visibility::prepare_eager_visibility(tables, edges, governor)?;
    let context = coordinator.context(governor);
    shortest_path_rows_in_context(
        source_table,
        source_id,
        target_table,
        target_id,
        max_depth,
        hydrate,
        &context,
        tables,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "shortest-path execution keeps SQL coordinates, bounds, context, and query catalog explicit"
)]
pub(super) fn shortest_path_rows_in_context(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
    max_depth: i32,
    hydrate: bool,
    context: &crate::visibility::QueryExecutionContext<'_>,
    tables: &[builder::RegisteredTable],
) -> safety::GraphResult<Vec<ShortestPathSqlRow>> {
    let governor = context.governor;
    let steps = ENGINE.with(|e| {
        e.borrow().shortest_path_governed_in_context(
            source_table.to_u32(),
            source_id,
            target_table.to_u32(),
            target_id,
            max_depth,
            context,
        )
    })?;
    acl::check_table_acls(steps.iter().map(|step| step.node_table.0))?;
    let output_bytes = steps.iter().try_fold(0usize, |bytes, step| {
        bytes
            .checked_add(std::mem::size_of::<ShortestPathSqlRow>())
            .and_then(|bytes| bytes.checked_add(step.node_id.len()))
            .and_then(|bytes| bytes.checked_add(step.edge_label.as_ref().map_or(0, String::len)))
            .and_then(|bytes| bytes.checked_add(512))
            .ok_or_else(|| {
                safety::GraphError::Internal("shortest-path output estimate overflowed".to_string())
            })
    })?;
    let output_bytes = crate::resource::ByteCount::from_usize(output_bytes).ok_or_else(|| {
        safety::GraphError::Internal("shortest-path output estimate does not fit u64".to_string())
    })?;
    let output_lease = governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryCandidates,
            output_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(steps.len())
        .map_err(|_| safety::GraphError::ResourceLimit {
            resource: "memory bytes".to_string(),
            phase: crate::resource::ResourcePhase::QueryCandidates
                .as_str()
                .to_string(),
            used: governor.memory_used().as_u64(),
            requested: output_bytes.as_u64(),
            limit: governor.memory_limit().as_u64(),
        })?;
    for step in steps {
        let node = if hydrate {
            crate::sql_hydration::hydrate_node_governed_with_tables(
                step.node_table.0,
                &step.node_id,
                governor,
                tables,
            )?
        } else {
            None
        };
        rows.push((
            step.step,
            pgrx::pg_sys::Oid::from_u32(step.node_table.0),
            step.node_id,
            step.edge_label,
            node,
            relation_name(step.node_table.0)?,
        ));
    }
    output_lease.retain_until_governor_drop();
    Ok(rows)
}

/// Find weighted shortest path between two nodes using Dijkstra.
///
/// Returns no rows when no weighted path exists or no weight columns were loaded.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each weighted path row column in the return tuple"
)]
fn weighted_shortest_path(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
) -> TableIterator<
    'static,
    (
        name!(step, i32),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_id, String),
        name!(edge_label, Option<String>),
        name!(edge_weight, Option<i64>),
        name!(step_cost, i64),
        name!(total_cost, i64),
    ),
> {
    with_panic_boundary("weighted_shortest_path()", || {
        check_enabled();
        acl::check_table_acl(source_table.to_u32()).unwrap_or_else(|err| err.report());
        acl::check_table_acl(target_table.to_u32()).unwrap_or_else(|err| err.report());

        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());

        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let coordinator = crate::sql_visibility::prepare_eager_visibility(
            &query_start.tables,
            &query_start.edges,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let context = coordinator.context(&governor);
        let steps = ENGINE.with(|e| {
            let eng = e.borrow();
            eng.weighted_shortest_path_governed_in_context(
                source_table.to_u32(),
                source_id,
                target_table.to_u32(),
                target_id,
                &context,
            )
            .unwrap_or_else(|err| err.report())
        });
        acl::check_table_acls(steps.iter().map(|step| step.node_table.0))
            .unwrap_or_else(|err| err.report());
        let rows = steps
            .into_iter()
            .map(|step| {
                (
                    step.step,
                    pgrx::pg_sys::Oid::from_u32(step.node_table.0),
                    relation_name(step.node_table.0).unwrap_or_else(|err| err.report()),
                    step.node_id,
                    step.edge_label,
                    step.edge_weight.map(i64::from),
                    u64_to_bigint(step.step_cost).unwrap_or_else(|err| err.report()),
                    u64_to_bigint(step.total_cost).unwrap_or_else(|err| err.report()),
                )
            })
            .collect::<Vec<_>>();
        TableIterator::new(rows)
    })
}

#[pg_extern(schema = "graph", name = "weighted_shortest_path")]
#[allow(clippy::type_complexity)]
fn weighted_shortest_path_typed(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
    edge_types: Vec<String>,
) -> TableIterator<
    'static,
    (
        name!(step, i32),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_id, String),
        name!(edge_label, Option<String>),
        name!(edge_weight, Option<i64>),
        name!(step_cost, i64),
        name!(total_cost, i64),
    ),
> {
    with_panic_boundary("weighted_shortest_path()", || {
        check_enabled();
        acl::check_table_acl(source_table.to_u32()).unwrap_or_else(|err| err.report());
        acl::check_table_acl(target_table.to_u32()).unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let coordinator = crate::sql_visibility::prepare_eager_visibility(
            &query_start.tables,
            &query_start.edges,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let edge_type_filter = ENGINE
            .with(|engine| engine.borrow().resolve_edge_type_filter(Some(&edge_types)))
            .unwrap_or_else(|err| err.report());
        let context =
            coordinator.context_with_edge_type_filter(&governor, edge_type_filter.as_ref());
        let steps = ENGINE.with(|engine| {
            engine
                .borrow()
                .weighted_shortest_path_governed_in_context(
                    source_table.to_u32(),
                    source_id,
                    target_table.to_u32(),
                    target_id,
                    &context,
                )
                .unwrap_or_else(|err| err.report())
        });
        acl::check_table_acls(steps.iter().map(|step| step.node_table.0))
            .unwrap_or_else(|err| err.report());
        TableIterator::new(steps.into_iter().map(|step| {
            (
                step.step,
                pgrx::pg_sys::Oid::from_u32(step.node_table.0),
                relation_name(step.node_table.0).unwrap_or_else(|err| err.report()),
                step.node_id,
                step.edge_label,
                step.edge_weight.map(i64::from),
                u64_to_bigint(step.step_cost).unwrap_or_else(|err| err.report()),
                u64_to_bigint(step.total_cost).unwrap_or_else(|err| err.report()),
            )
        }))
    })
}

fn u64_to_bigint(value: u64) -> safety::GraphResult<i64> {
    i64::try_from(value).map_err(|_| {
        safety::GraphError::Internal(format!(
            "weighted path cost {} exceeds SQL bigint range",
            value
        ))
    })
}

struct DirectNodeMatch {
    graph: catalog::GraphMetadata,
    table: builder::RegisteredTable,
    table_oid: u32,
    node_idx: u32,
}

fn direct_get_node_rows(
    graph_name: &str,
    label: &str,
    id: &str,
    hydrate: bool,
    tenant: Option<&str>,
    graph_tenant: Option<&str>,
    graph_namespace: Option<&str>,
) -> safety::GraphResult<Vec<DirectNodeRow>> {
    check_enabled_result()?;
    with_named_graph(graph_name, graph_tenant, graph_namespace, |query_start| {
        let tenant_scope = crate::sql_sync::resolve_tenant_scope_for_query(
            tenant,
            &query_start.graph,
            &query_start.tables,
        )?;
        let Some(matched) =
            resolve_direct_node(query_start, graph_name, label, id, tenant_scope.as_deref())?
        else {
            return Ok(Vec::new());
        };
        let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
        let mut coordinator = crate::sql_visibility::prepare_direct_identity_visibility(
            &matched.table,
            &query_start.tables,
            &query_start.edges,
            true,
            false,
        )?;
        let probe_plan = if coordinator.table_requires_probe(matched.table_oid) {
            crate::sql_visibility::reserve_direct_probe_plan(&governor)?;
            crate::sql_visibility::prepare_direct_node_probe(&matched.table)?
        } else {
            None
        };
        if coordinator.table_requires_probe(matched.table_oid) && probe_plan.is_none() {
            if coordinator.table_has_policy_rls(matched.table_oid) {
                return Err(unsupported_direct_rls_key_type(&matched.table));
            }
            // In no-RLS/authorized-bypass mode this preserves get_node's 1.1
            // stale-projection existence check; it is not an authorization
            // oracle. RLS-active unsupported key types fail closed above.
            if !source_row_exists_for_projection_key(&matched.table, id)? {
                return Ok(Vec::new());
            }
            return direct_node_row(matched, id, hydrate, &governor, &query_start.tables);
        }
        crate::sql_visibility::reserve_direct_visibility_candidate(&governor, id)?;
        let batch = crate::sql_visibility::direct_visibility_batch(vec![
            crate::visibility::VisibilityCandidate::Node {
                sequence: 0,
                table_oid: matched.table_oid,
                source_key: id.to_string(),
                node_idx: matched.node_idx,
            },
        ])?;
        let verdicts = crate::sql_visibility::resolve_lazy_visibility_batch(
            &mut coordinator,
            batch,
            probe_plan.as_ref(),
            &governor,
        )?;
        if verdicts.verdicts() != [(0, crate::visibility::VisibilityVerdict::Visible)] {
            return Ok(Vec::new());
        }
        direct_node_row(matched, id, hydrate, &governor, &query_start.tables)
    })
}

fn unsupported_direct_rls_key_type(table: &builder::RegisteredTable) -> safety::GraphError {
    crate::sql_visibility::unsupported_rls_identity_type(&table.table_name)
}

fn direct_node_row(
    matched: DirectNodeMatch,
    id: &str,
    hydrate: bool,
    governor: &crate::resource::ResourceGovernor,
    tables: &[builder::RegisteredTable],
) -> safety::GraphResult<Vec<DirectNodeRow>> {
    let node = if hydrate {
        crate::sql_hydration::hydrate_node_governed_with_tables(
            matched.table_oid,
            id,
            governor,
            tables,
        )?
    } else {
        None
    };
    let row = (
        matched.graph.graph_id,
        matched.graph.graph_name,
        pgrx::pg_sys::Oid::from_u32(matched.table_oid),
        relation_name(matched.table_oid)?,
        id.to_string(),
        i64::from(matched.node_idx),
        node,
    );
    Ok(vec![row])
}

#[allow(clippy::too_many_arguments, reason = "mirrors SQL API parameters")]
fn direct_get_neighbors_rows(
    graph_name: &str,
    label: &str,
    id: &str,
    direction: &str,
    edge_types: Option<&[String]>,
    tenant: Option<&str>,
    hydrate: bool,
    max_rows: i32,
    graph_tenant: Option<&str>,
    graph_namespace: Option<&str>,
) -> safety::GraphResult<Vec<crate::api_types::TraverseRow>> {
    check_enabled_result()?;
    with_named_graph(graph_name, graph_tenant, graph_namespace, |query_start| {
        let tenant_scope = crate::sql_sync::resolve_tenant_scope_for_query(
            tenant,
            &query_start.graph,
            &query_start.tables,
        )?;
        let Some(matched) =
            resolve_direct_node(query_start, graph_name, label, id, tenant_scope.as_deref())?
        else {
            return Ok(Vec::new());
        };
        if !source_row_exists_for_projection_key(&matched.table, id)? {
            return Ok(Vec::new());
        }
        let (direction, strategy, _uniqueness) = crate::sql_traversal::validate_traverse_options(
            direction,
            tenant_scope.as_deref(),
            "bfs",
            "node_global",
        )?;
        let request = TraverseRequest {
            root_table: pgrx::pg_sys::Oid::from_u32(matched.table_oid),
            root_id: id,
            max_depth: 1,
            edge_types,
            node_tables: None,
            filter: None,
            tenant: tenant_scope.as_deref(),
            direction,
            strategy,
            include_start: false,
            hydrate,
            limit: max_rows,
            offset: 0,
            max_nodes: config::MAX_NODES.get(),
            max_frontier: config::MAX_FRONTIER.get(),
        };
        let governor = ENGINE.with(|engine| engine.borrow().query_resource_governor())?;
        let coordinator = crate::sql_visibility::prepare_eager_visibility(
            &query_start.tables,
            &query_start.edges,
            &governor,
        )?;
        if !coordinator.allows_node(matched.node_idx) {
            return Ok(Vec::new());
        }
        let context = coordinator.context(&governor);
        execute_traverse_rows_in_context(
            &request,
            &context,
            &query_start.tables,
            &query_start.filter_columns,
        )
    })
}

fn with_named_graph<T>(
    graph_name: &str,
    graph_tenant: Option<&str>,
    graph_namespace: Option<&str>,
    action: impl FnOnce(&super::runtime::QueryStartState) -> safety::GraphResult<T>,
) -> safety::GraphResult<T> {
    let graph_id = catalog::graph_id_with_privilege_via_definer(
        graph_name,
        graph_tenant,
        graph_namespace,
        catalog::GraphPrivilege::Read,
    )?;
    let previous_graph_id = catalog::selected_graph_id()?;
    catalog::set_selected_graph_id(&graph_id)?;
    let result = (|| {
        let freshness = current_query_freshness()?;
        let query_start = ensure_current_graph_for_query(freshness)?;
        action(&query_start)
    })();
    restore_selected_graph(previous_graph_id)?;
    result
}

fn restore_selected_graph(previous_graph_id: Option<String>) -> safety::GraphResult<()> {
    match previous_graph_id {
        Some(graph_id) => catalog::set_selected_graph_id(&graph_id),
        None => catalog::set_selected_graph_id(""),
    }
}

fn resolve_direct_node(
    query_start: &super::runtime::QueryStartState,
    graph_name: &str,
    label: &str,
    id: &str,
    tenant: Option<&str>,
) -> safety::GraphResult<Option<DirectNodeMatch>> {
    let table = registered_table_for_label(&query_start.tables, label).ok_or_else(|| {
        safety::GraphError::InvalidFilter {
            reason: format!("graph '{graph_name}' has no registered node label '{label}'"),
        }
    })?;
    let table_oid = table.table_oid;
    acl::check_table_acl(table_oid)?;
    let node_idx = ENGINE.with(|engine| {
        let engine = engine.borrow();
        let table_is_tenanted = engine.tenanted_table_oids.contains(&table_oid);
        let node_idx = engine.resolve(table_oid, id).or_else(|| {
            crate::projection::tx_delta::resolve_added_node(
                table_oid,
                id,
                tenant,
                table_is_tenanted,
            )
        })?;
        tenant_allows_direct_node(&engine, node_idx, tenant).then_some(node_idx)
    });
    Ok(node_idx.map(|node_idx| DirectNodeMatch {
        graph: query_start.graph.clone(),
        table,
        table_oid,
        node_idx,
    }))
}

fn registered_table_for_label(
    tables: &[builder::RegisteredTable],
    label: &str,
) -> Option<builder::RegisteredTable> {
    tables
        .iter()
        .find(|table| table.table_name == label || catalog_label(&table.table_name) == label)
        .cloned()
}

fn catalog_label(table_name: &str) -> &str {
    table_name.rsplit('.').next().unwrap_or(table_name)
}

fn tenant_allows_direct_node(engine: &Engine, node_idx: u32, tenant: Option<&str>) -> bool {
    match tenant {
        Some(tenant)
            if engine
                .node_store
                .table_oid(node_idx)
                .is_some_and(|table_oid| engine.tenanted_table_oids.contains(&table_oid)) =>
        {
            engine.tenant_contains(tenant, node_idx)
        }
        _ => true,
    }
}

fn source_row_exists_for_projection_key(
    table: &builder::RegisteredTable,
    id: &str,
) -> safety::GraphResult<bool> {
    let table_name = catalog::sql_table_name_from_oid(table.table_oid)?;
    let pk_expr = catalog::primary_key_expr("src", &table.id_columns);
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {} src WHERE {pk_expr} = $1)",
        table_name.as_sql()
    );
    Spi::get_one_with_args::<bool>(&sql, &[id.into()])
        .map(|visible| visible.unwrap_or(false))
        .map_err(|err| {
            safety::GraphError::Internal(format!("source visibility check failed: {err}"))
        })
}

/// Aggregate over traversal results without hydrating every row client-side.
#[pg_extern(schema = "graph")]
fn aggregate(
    traversal: pgrx::JsonB,
    aggregations: pgrx::JsonB,
    scope: default!(&str, "'returned_nodes'"),
    path_limit: default!(
        i32,
        "COALESCE(NULLIF(current_setting('graph.max_exact_path_count', true), '')::int, 100000)"
    ),
) -> pgrx::JsonB {
    with_panic_boundary("aggregate()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        aggregate_impl(
            &traversal.0,
            &aggregations.0,
            scope,
            path_limit,
            &query_start.tables,
            &query_start.edges,
            &query_start.filter_columns,
        )
        .map(pgrx::JsonB)
        .unwrap_or_else(|err| err.report())
    })
}

/// Estimate strict traversal path count with a hard cap.
#[pg_extern(schema = "graph")]
fn path_count_estimate(
    traversal: pgrx::JsonB,
) -> TableIterator<
    'static,
    (
        name!(estimated_paths, i64),
        name!(exact, bool),
        name!(capped, bool),
    ),
> {
    with_panic_boundary("path_count_estimate()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        let query_start =
            ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let (count, exact, capped) = path_count_estimate_impl(
            &traversal.0,
            crate::config::MAX_EXACT_PATH_COUNT.get(),
            &query_start.tables,
            &query_start.edges,
        )
        .unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(count, exact, capped)])
    })
}
