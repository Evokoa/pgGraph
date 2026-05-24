/// Search for nodes by property value.
///
/// See: `docs/user_guide/querying.mdx`
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn search(
    property_key: &str,
    property_value: &str,
    table_filter: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    mode: default!(&str, "'contains'"),
    case_sensitive: default!(bool, "false"),
    max_rows: default!(i32, 100),
    row_offset: default!(i32, 0),
    tenant: default!(Option<String>, "NULL"),
    hydrate: default!(bool, "true"),
) -> TableIterator<
    'static,
    (
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_id, String),
        name!(match_type, String),
        name!(score, f32),
        name!(verified, bool),
        name!(node, Option<pgrx::JsonB>),
        name!(node_table_name, String),
    ),
> {
    with_panic_boundary("search()", || {
        check_enabled();
        ensure_current_graph().unwrap_or_else(|err| err.report());
        let tenant_scope =
            resolve_tenant_scope(tenant.as_deref()).unwrap_or_else(|err| err.report());
        validate_search_request(
            property_key,
            table_filter.map(|oid| oid.to_u32()),
            tenant_scope.as_deref(),
        )
        .unwrap_or_else(|err| err.report());
        let mode = types::SearchMode::parse(mode).unwrap_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: format!(
                    "unsupported search mode '{}'; expected contains, exact, prefix, or token",
                    mode
                ),
            }
            .report()
        });
        let row_offset =
            usize_from_nonnegative(row_offset, "row_offset").unwrap_or_else(|err| err.report());
        let max_rows =
            usize_from_nonnegative(max_rows, "max_rows").unwrap_or_else(|err| err.report());
        let rows = source_table_search_rows(
            property_key,
            property_value,
            table_filter.map(|oid| oid.to_u32()),
            mode,
            case_sensitive,
            tenant_scope.as_deref(),
            hydrate,
            row_offset,
            max_rows,
        )
        .unwrap_or_else(|err| err.report());
        TableIterator::new(rows)
    })
}

/// Coordinate-only search primitive for diagnostics and composition.
#[pg_extern(schema = "graph")]
#[allow(clippy::too_many_arguments)]
fn search_nodes(
    property_key: &str,
    property_value: &str,
    table_filter: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    mode: default!(&str, "'contains'"),
    case_sensitive: default!(bool, "false"),
    max_rows: default!(i32, 100),
    row_offset: default!(i32, 0),
    tenant: default!(Option<String>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_id, String),
        name!(match_type, String),
        name!(score, f32),
        name!(verified, bool),
        name!(node_table_name, String),
    ),
> {
    with_panic_boundary("search_nodes()", || {
        let rows = search(
            property_key,
            property_value,
            table_filter,
            mode,
            case_sensitive,
            max_rows,
            row_offset,
            tenant,
            false,
        )
        .map(
            |(node_table, node_id, match_type, score, verified, _node, node_table_name)| {
                (
                    node_table,
                    node_id,
                    match_type,
                    score,
                    verified,
                    node_table_name,
                )
            },
        )
        .collect::<Vec<_>>();
        TableIterator::new(rows)
    })
}

/// Search for starting nodes, then traverse from each verified match.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn traverse_search(
    property_key: &str,
    property_value: &str,
    table_filter: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    search_mode: default!(
        &str,
        "COALESCE(NULLIF(current_setting('graph.default_search_mode'), ''), 'contains')"
    ),
    case_sensitive: default!(
        bool,
        "current_setting('graph.default_case_sensitive')::boolean"
    ),
    search_max_rows: default!(i32, 100),
    search_row_offset: default!(i32, 0),
    max_depth: default!(i32, "current_setting('graph.default_max_depth')::int"),
    edge_types: default!(Option<Vec<String>>, "NULL"),
    direction: default!(&str, "'any'"),
    node_tables: default!(Option<Vec<pgrx::pg_sys::Oid>>, "NULL"),
    filter: default!(Option<pgrx::JsonB>, "NULL"),
    tenant: default!(Option<String>, "NULL"),
    strategy: default!(&str, "'bfs'"),
    uniqueness: default!(&str, "'node_per_root'"),
    include_start: default!(bool, "true"),
    hydrate: default!(bool, "current_setting('graph.default_hydrate')::boolean"),
    max_rows: default!(i32, 1000),
    row_offset: default!(i32, 0),
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
    with_panic_boundary("traverse_search()", || {
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

        let starts = search(
            property_key,
            property_value,
            table_filter,
            search_mode,
            case_sensitive,
            search_max_rows,
            search_row_offset,
            tenant_scope.clone(),
            false,
        )
        .collect::<Vec<_>>();

        let mut candidates = Vec::new();
        for (root_table, root_id, _match_type, _score, verified, _node, _node_table_name) in starts
        {
            if !verified {
                continue;
            }
            let request = TraverseRequest {
                root_table,
                root_id: &root_id,
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
                max_nodes: config::MAX_NODES.get(),
                max_frontier: config::MAX_FRONTIER.get(),
            };
            let mut start_candidates =
                execute_traverse_candidates(&request).unwrap_or_else(|err| err.report());
            candidates.append(&mut start_candidates);
        }
        sort_traverse_candidates_for_many(&mut candidates);
        apply_traversal_uniqueness(&mut candidates, uniqueness);
        let rows =
            paginate_and_format_traverse_candidates(candidates, hydrate, row_offset, max_rows)
                .unwrap_or_else(|err| err.report());
        TableIterator::new(rows)
    })
}
