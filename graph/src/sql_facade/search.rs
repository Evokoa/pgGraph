use super::admin::{check_enabled, check_enabled_result, with_panic_boundary};
use super::runtime::{
    current_query_freshness, ensure_current_graph, ensure_current_graph_for_query,
};
use super::*;

/// Search for nodes by property value.
///
/// See: `docs/user_guide/querying.mdx`
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
pub(super) fn search(
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
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let rows = search_rows_governed(
            property_key,
            property_value,
            table_filter,
            mode,
            case_sensitive,
            max_rows,
            row_offset,
            tenant.as_deref(),
            hydrate,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        TableIterator::new(rows)
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn search_rows_governed(
    property_key: &str,
    property_value: &str,
    table_filter: Option<pgrx::pg_sys::Oid>,
    mode: &str,
    case_sensitive: bool,
    max_rows: i32,
    row_offset: i32,
    tenant: Option<&str>,
    hydrate: bool,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Vec<crate::api_types::SearchOutputRow>> {
    let tenant_scope = resolve_tenant_scope(tenant)?;
    validate_search_request(
        property_key,
        table_filter.map(|oid| oid.to_u32()),
        tenant_scope.as_deref(),
    )?;
    let mode = types::SearchMode::parse(mode).ok_or_else(|| safety::GraphError::InvalidFilter {
        reason: format!(
            "unsupported search mode '{}'; expected contains, exact, prefix, or token",
            mode
        ),
    })?;
    let row_offset = usize_from_nonnegative(row_offset, "row_offset")?;
    let max_rows = usize_from_nonnegative(max_rows, "max_rows")?;
    source_table_search_rows_governed(
        property_key,
        property_value,
        table_filter.map(|oid| oid.to_u32()),
        mode,
        case_sensitive,
        tenant_scope.as_deref(),
        hydrate,
        row_offset,
        max_rows,
        governor,
    )
}

fn reserve_search_tuple_output(
    governor: &crate::resource::ResourceGovernor,
    rows: usize,
) -> safety::GraphResult<crate::resource::ResourceLease<'_>> {
    let bytes = rows
        .checked_mul(std::mem::size_of::<crate::api_types::SearchOutputRow>())
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("search output estimate overflowed".to_string())
        })?;
    governor
        .reserve_memory(crate::resource::ResourcePhase::QueryCandidates, bytes)
        .map_err(crate::safety::resource_limit_error)
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
        check_enabled();
        ensure_current_graph().unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let rows = search_rows_governed(
            property_key,
            property_value,
            table_filter,
            mode,
            case_sensitive,
            max_rows,
            row_offset,
            tenant.as_deref(),
            false,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let output_lease =
            reserve_search_tuple_output(&governor, rows.len()).unwrap_or_else(|err| err.report());
        let rows = rows
            .into_iter()
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
        output_lease.retain_until_governor_drop();
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
pub(super) fn traverse_search(
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
        name!(capped, bool),
    ),
> {
    with_panic_boundary("traverse_search()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let rows = traverse_search_rows_governed(
            property_key,
            property_value,
            table_filter,
            search_mode,
            case_sensitive,
            search_max_rows,
            search_row_offset,
            max_depth,
            edge_types.as_deref(),
            direction,
            node_tables.as_deref(),
            filter.as_ref(),
            tenant.as_deref(),
            strategy,
            uniqueness,
            include_start,
            hydrate,
            max_rows,
            row_offset,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        TableIterator::new(rows)
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn traverse_search_rows_governed(
    property_key: &str,
    property_value: &str,
    table_filter: Option<pgrx::pg_sys::Oid>,
    search_mode: &str,
    case_sensitive: bool,
    search_max_rows: i32,
    search_row_offset: i32,
    max_depth: i32,
    edge_types: Option<&[String]>,
    direction: &str,
    node_tables: Option<&[pgrx::pg_sys::Oid]>,
    filter: Option<&pgrx::JsonB>,
    tenant: Option<&str>,
    strategy: &str,
    uniqueness: &str,
    include_start: bool,
    hydrate: bool,
    max_rows: i32,
    row_offset: i32,
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<Vec<crate::api_types::TraverseRow>> {
    check_enabled_result()?;
    ensure_current_graph_for_query(current_query_freshness()?)?;
    let tenant_scope = resolve_tenant_scope(tenant)?;
    let (direction, strategy, uniqueness) = crate::sql_traversal::validate_traverse_options(
        direction,
        tenant_scope.as_deref(),
        strategy,
        uniqueness,
    )?;
    let start_rows = usize::try_from(search_max_rows.max(0)).unwrap_or(usize::MAX);
    let max_primary_key = ENGINE.with(|engine| engine.borrow().node_store.max_primary_key_bytes());
    let starts_bytes = start_rows
        .checked_mul(max_primary_key.saturating_add(1_024))
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("traversal search seed estimate overflowed".to_string())
        })?;
    let _starts_workspace = governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryCandidates,
            starts_bytes,
        )
        .map_err(crate::safety::resource_limit_error)?;
    let starts = search_rows_governed(
        property_key,
        property_value,
        table_filter,
        search_mode,
        case_sensitive,
        search_max_rows,
        search_row_offset,
        tenant_scope.as_deref(),
        false,
        governor,
    )?;
    let mut candidates = Vec::new();
    for (root_table, root_id, _match_type, _score, verified, _node, _node_table_name) in starts {
        if !verified {
            continue;
        }
        let request = TraverseRequest {
            root_table,
            root_id: &root_id,
            max_depth,
            edge_types,
            node_tables,
            filter,
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
        let mut start_candidates = execute_traverse_candidates_governed(&request, governor)?;
        candidates.append(&mut start_candidates);
    }
    sort_traverse_candidates_for_many_governed(&mut candidates, governor)?;
    apply_traversal_uniqueness_governed(&mut candidates, uniqueness, governor)?;
    paginate_and_format_traverse_candidates_governed(
        candidates, hydrate, row_offset, max_rows, governor,
    )
}
