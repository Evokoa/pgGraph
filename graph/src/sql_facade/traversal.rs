/// BFS traversal from a seed node.
///
/// See: `docs/user_guide/querying.mdx`
#[pg_extern(schema = "graph", cost = 1000)]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn traverse(
    seed_table: pgrx::pg_sys::Oid,
    seed_id: &str,
    max_depth: default!(i32, "current_setting('graph.default_max_depth')::int"),
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
    max_nodes: default!(i32, "current_setting('graph.max_nodes')::int"),
    max_frontier: default!(i32, "current_setting('graph.max_frontier')::int"),
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
    ),
> {
    with_panic_boundary("traverse()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let tenant_scope =
            resolve_tenant_scope(tenant.as_deref()).unwrap_or_else(|err| err.report());
        let (direction, strategy, uniqueness) =
            crate::sql_traversal::validate_traverse_options(
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
            uniqueness,
            include_start,
            hydrate,
            limit: max_rows,
            offset: row_offset,
            max_nodes,
            max_frontier,
            filter_condition: None,
        };
        let rows = execute_traverse_rows(&request).unwrap_or_else(|err| err.report());

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
    max_depth: default!(i32, "current_setting('graph.default_max_depth')::int"),
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
    max_nodes: default!(i32, "current_setting('graph.max_nodes')::int"),
    max_frontier: default!(i32, "current_setting('graph.max_frontier')::int"),
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
    ),
> {
    with_panic_boundary("traverse_many()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let tenant_scope =
            resolve_tenant_scope(tenant.as_deref()).unwrap_or_else(|err| err.report());
        if start_tables.len() != start_ids.len() {
            safety::GraphError::InvalidFilter {
                reason: "start_tables and start_ids must have the same length".to_string(),
            }
            .report();
        }
        let (direction, strategy, uniqueness) =
            crate::sql_traversal::validate_traverse_options(
                direction,
                tenant_scope.as_deref(),
                strategy,
                uniqueness,
            )
            .unwrap_or_else(|err| err.report());

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
                uniqueness,
                include_start,
                hydrate,
                limit: max_rows,
                offset: row_offset,
                max_nodes,
                max_frontier,
                filter_condition: None,
            };
            let mut start_candidates =
                execute_traverse_candidates(&request).unwrap_or_else(|err| err.report());
            candidates.append(&mut start_candidates);
        }
        sort_traverse_candidates_for_many(&mut candidates);
        let rows =
            paginate_and_format_traverse_candidates(candidates, hydrate, row_offset, max_rows)
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
fn shortest_path(
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
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());

        let steps = ENGINE
            .with(|e| {
                let eng = e.borrow();
                eng.shortest_path(
                    source_table.to_u32(),
                    source_id,
                    target_table.to_u32(),
                    target_id,
                    max_depth,
                )
            })
            .unwrap_or_else(|err| err.report());

        let rows = steps
            .into_iter()
            .map(|s| {
                let node = if hydrate {
                    hydrate_node(s.node_table.0, &s.node_id).unwrap_or_else(|err| err.report())
                } else {
                    None
                };
                (
                    s.step,
                    pgrx::pg_sys::Oid::from_u32(s.node_table.0),
                    s.node_id,
                    s.edge_label,
                    node,
                    regclass_text(s.node_table.0).unwrap_or_else(|err| err.report()),
                )
            })
            .collect::<Vec<_>>();

        TableIterator::new(rows)
    })
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
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());

        let rows = ENGINE.with(|e| {
            let eng = e.borrow();
            eng.weighted_shortest_path(
                source_table.to_u32(),
                source_id,
                target_table.to_u32(),
                target_id,
            )
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .map(|step| {
                (
                    step.step,
                    pgrx::pg_sys::Oid::from_u32(step.node_table.0),
                    regclass_text(step.node_table.0).unwrap_or_else(|err| err.report()),
                    step.node_id,
                    step.edge_label,
                    step.edge_weight.map(i64::from),
                    u64_to_bigint(step.step_cost).unwrap_or_else(|err| err.report()),
                    u64_to_bigint(step.total_cost).unwrap_or_else(|err| err.report()),
                )
            })
            .collect::<Vec<_>>()
        });
        TableIterator::new(rows)
    })
}

fn u64_to_bigint(value: u64) -> safety::GraphResult<i64> {
    i64::try_from(value).map_err(|_| safety::GraphError::Internal(format!(
        "weighted path cost {} exceeds SQL bigint range",
        value
    )))
}

/// Aggregate over traversal results without hydrating every row client-side.
#[pg_extern(schema = "graph")]
fn aggregate(
    traversal: pgrx::JsonB,
    aggregations: pgrx::JsonB,
    scope: default!(&str, "'returned_nodes'"),
    path_limit: default!(i32, "current_setting('graph.max_exact_path_count')::int"),
) -> pgrx::JsonB {
    with_panic_boundary("aggregate()", || {
        aggregate_impl(&traversal.0, &aggregations.0, scope, path_limit)
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
        let (count, exact, capped) =
            path_count_estimate_impl(&traversal.0, crate::config::MAX_EXACT_PATH_COUNT.get())
                .unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(count, exact, capped)])
    })
}
