use super::admin::with_panic_boundary;
use super::runtime::{
    current_query_freshness, ensure_current_graph, ensure_current_graph_for_query,
};
use super::search::{search_rows_governed, traverse_search_rows_governed};
use super::traversal::shortest_path_rows_governed;
use super::*;

// Workflow-level SQL functions for common application and AI-tool queries.
//
// These wrappers keep the primitive APIs stable while providing smaller,
// opinionated call shapes for the common search, expand, relationship, and
// path workflows documented in `docs/user_guide/querying.mdx`.

/// Search source rows with hydrated output and app-friendly row pagination.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn find(
    property_key: &str,
    property_value: &str,
    table_name: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    mode: default!(&str, "'contains'"),
    case_sensitive: default!(bool, "false"),
    max_rows: default!(i32, 20),
    row_offset: default!(i32, 0),
    tenant: default!(Option<String>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_id, String),
        name!(match_type, String),
        name!(score, f32),
        name!(verified, bool),
        name!(rank, i32),
        name!(node, Option<pgrx::JsonB>),
    ),
> {
    with_panic_boundary("find()", || {
        validate_nonnegative_arg(max_rows, "max_rows").unwrap_or_else(|err| err.report());
        let row_offset_usize =
            validate_nonnegative_arg(row_offset, "row_offset").unwrap_or_else(|err| err.report());
        check_enabled_result().unwrap_or_else(|err| err.report());
        ensure_current_graph().unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let rows = search_rows_governed(
            property_key,
            property_value,
            table_name,
            mode,
            case_sensitive,
            max_rows,
            row_offset,
            tenant.as_deref(),
            true,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let output_lease =
            reserve_workflow_rows(&governor, rows.len()).unwrap_or_else(|err| err.report());
        let rows = rows
            .into_iter()
            .enumerate()
            .map(
                |(
                    idx,
                    (node_table, node_id, match_type, score, verified, node, node_table_name),
                )| {
                    (
                        node_table,
                        node_table_name,
                        node_id,
                        match_type,
                        score,
                        verified,
                        rank_from_offset(row_offset_usize, idx),
                        node,
                    )
                },
            )
            .collect::<Vec<_>>();
        output_lease.retain_until_governor_drop();
        TableIterator::new(rows)
    })
}

/// Expand from a known graph node using hydrated rows and readable paths.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn expand(
    seed_table: pgrx::pg_sys::Oid,
    seed_id: &str,
    max_depth: default!(i32, "current_setting('graph.default_max_depth')::int"),
    edge_types: default!(Option<Vec<String>>, "NULL"),
    direction: default!(&str, "'any'"),
    target_table: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    target_tables: default!(Option<Vec<pgrx::pg_sys::Oid>>, "NULL"),
    where_node: default!(Option<pgrx::JsonB>, "NULL"),
    tenant: default!(Option<String>, "NULL"),
    max_rows: default!(i32, 50),
    row_offset: default!(i32, 0),
    include_start: default!(bool, "false"),
    hydrate: default!(bool, "true"),
) -> TableIterator<
    'static,
    (
        name!(root_table, pgrx::pg_sys::Oid),
        name!(root_id, String),
        name!(root_table_name, String),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_id, String),
        name!(depth, i32),
        name!(rank, i32),
        name!(path, pgrx::JsonB),
        name!(edge_path, pgrx::JsonB),
        name!(readable_path, String),
        name!(node, Option<pgrx::JsonB>),
        name!(truncated, bool),
    ),
> {
    with_panic_boundary("expand()", || {
        let row_offset_usize =
            validate_nonnegative_arg(row_offset, "row_offset").unwrap_or_else(|err| err.report());
        validate_nonnegative_arg(max_rows, "max_rows").unwrap_or_else(|err| err.report());
        let node_tables =
            workflow_target_tables(target_table, target_tables).unwrap_or_else(|err| err.report());
        check_enabled_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let tenant_scope =
            resolve_tenant_scope(tenant.as_deref()).unwrap_or_else(|err| err.report());
        let (direction, strategy, _uniqueness) = crate::sql_traversal::validate_traverse_options(
            direction,
            tenant_scope.as_deref(),
            "bfs",
            "node_global",
        )
        .unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let request = TraverseRequest {
            root_table: seed_table,
            root_id: seed_id,
            max_depth,
            edge_types: edge_types.as_deref(),
            node_tables: node_tables.as_deref(),
            filter: where_node.as_ref(),
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
        let rows =
            execute_traverse_rows_governed(&request, &governor).unwrap_or_else(|err| err.report());
        let mut truncated = max_rows > 0 && rows.len() == max_rows as usize;
        let mut workspace = workflow_workspace(&governor).unwrap_or_else(|err| err.report());
        let mut output = Vec::new();
        reserve_workflow_vector(&mut workspace, &mut output, rows.len())
            .unwrap_or_else(|err| err.report());
        for (
            idx,
            (
                root_table,
                root_id,
                node_table,
                node_id,
                depth,
                path,
                edge_path,
                node,
                root_table_name,
                node_table_name,
                capped,
            ),
        ) in rows.into_iter().enumerate()
        {
            truncated |= capped;
            workflow_step(&governor, 1).unwrap_or_else(|err| err.report());
            let readable_path =
                readable_path_governed(&path, &edge_path, &governor, &mut workspace)
                    .unwrap_or_else(|err| err.report());
            output.push((
                root_table,
                root_id,
                root_table_name,
                node_table,
                node_table_name,
                node_id,
                depth,
                rank_from_offset(row_offset_usize, idx),
                path,
                edge_path,
                readable_path,
                node,
                truncated,
            ));
        }
        workspace.retain_until_governor_drop();
        TableIterator::new(output)
    })
}

/// Search for a seed row, traverse related nodes, and hydrate the final page.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn find_related(
    property_key: &str,
    property_value: &str,
    source_table: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    search_mode: default!(
        &str,
        "COALESCE(NULLIF(current_setting('graph.default_search_mode'), ''), 'contains')"
    ),
    case_sensitive: default!(
        bool,
        "current_setting('graph.default_case_sensitive')::boolean"
    ),
    search_max_rows: default!(i32, 1),
    search_row_offset: default!(i32, 0),
    max_depth: default!(i32, "current_setting('graph.default_max_depth')::int"),
    edge_types: default!(Option<Vec<String>>, "NULL"),
    direction: default!(&str, "'any'"),
    target_table: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    target_tables: default!(Option<Vec<pgrx::pg_sys::Oid>>, "NULL"),
    where_node: default!(Option<pgrx::JsonB>, "NULL"),
    tenant: default!(Option<String>, "NULL"),
    max_rows: default!(i32, 20),
    row_offset: default!(i32, 0),
    include_counts: default!(bool, "false"),
    candidate_limit: default!(i32, 10000),
    include_start: default!(bool, "false"),
) -> TableIterator<
    'static,
    (
        name!(root_table, pgrx::pg_sys::Oid),
        name!(root_id, String),
        name!(root_table_name, String),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_id, String),
        name!(depth, i32),
        name!(score, f32),
        name!(rank, i32),
        name!(path, pgrx::JsonB),
        name!(edge_path, pgrx::JsonB),
        name!(readable_path, String),
        name!(node, Option<pgrx::JsonB>),
        name!(candidate_count, Option<i64>),
        name!(filtered_count, Option<i64>),
        name!(truncated, bool),
    ),
> {
    with_panic_boundary("find_related()", || {
        let row_offset_usize =
            validate_nonnegative_arg(row_offset, "row_offset").unwrap_or_else(|err| err.report());
        validate_nonnegative_arg(max_rows, "max_rows").unwrap_or_else(|err| err.report());
        validate_nonnegative_arg(search_max_rows, "search_max_rows")
            .unwrap_or_else(|err| err.report());
        validate_nonnegative_arg(candidate_limit, "candidate_limit")
            .unwrap_or_else(|err| err.report());
        let node_tables =
            workflow_target_tables(target_table, target_tables).unwrap_or_else(|err| err.report());
        check_enabled_result().unwrap_or_else(|err| err.report());
        ensure_current_graph().unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let filtered = traverse_search_rows_governed(
            property_key,
            property_value,
            source_table,
            search_mode,
            case_sensitive,
            search_max_rows,
            search_row_offset,
            max_depth,
            edge_types.as_deref(),
            direction,
            node_tables.as_deref(),
            where_node.as_ref(),
            tenant.as_deref(),
            "bfs",
            "node_per_root",
            include_start,
            false,
            candidate_limit,
            0,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let broad_count = if include_counts {
            Some(
                traverse_search_rows_governed(
                    property_key,
                    property_value,
                    source_table,
                    search_mode,
                    case_sensitive,
                    search_max_rows,
                    search_row_offset,
                    max_depth,
                    edge_types.as_deref(),
                    direction,
                    None,
                    None,
                    tenant.as_deref(),
                    "bfs",
                    "node_per_root",
                    include_start,
                    false,
                    candidate_limit,
                    0,
                    &governor,
                )
                .unwrap_or_else(|err| err.report())
                .len() as i64,
            )
        } else {
            None
        };
        let filtered_count = include_counts.then_some(filtered.len() as i64);
        let truncated = (candidate_limit > 0 && filtered.len() == candidate_limit as usize)
            || filtered.iter().any(|row| row.10);
        let page_len = filtered
            .len()
            .saturating_sub(row_offset_usize)
            .min(max_rows as usize);
        let mut workspace = workflow_workspace(&governor).unwrap_or_else(|err| err.report());
        let mut rows = Vec::new();
        reserve_workflow_vector(&mut workspace, &mut rows, page_len)
            .unwrap_or_else(|err| err.report());
        for (
            idx,
            (
                root_table,
                root_id,
                node_table,
                node_id,
                depth,
                path,
                edge_path,
                _node,
                root_table_name,
                node_table_name,
                _capped,
            ),
        ) in filtered
            .into_iter()
            .skip(row_offset_usize)
            .take(max_rows as usize)
            .enumerate()
        {
            workflow_step(&governor, 1).unwrap_or_else(|err| err.report());
            let node = hydrate_node_governed(node_table.to_u32(), &node_id, &governor)
                .unwrap_or_else(|err| err.report());
            let readable_path =
                readable_path_governed(&path, &edge_path, &governor, &mut workspace)
                    .unwrap_or_else(|err| err.report());
            rows.push((
                root_table,
                root_id,
                root_table_name,
                node_table,
                node_table_name,
                node_id,
                depth,
                1.0,
                rank_from_offset(row_offset_usize, idx),
                path,
                edge_path,
                readable_path,
                node,
                broad_count,
                filtered_count,
                truncated,
            ));
        }
        workspace.retain_until_governor_drop();
        TableIterator::new(rows)
    })
}

/// Return a hydrated shortest path with a repeated readable path summary.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn path(
    source_table: pgrx::pg_sys::Oid,
    source_id: &str,
    target_table: pgrx::pg_sys::Oid,
    target_id: &str,
    max_depth: default!(i32, 20),
) -> TableIterator<
    'static,
    (
        name!(step, i32),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_id, String),
        name!(edge_label, Option<String>),
        name!(readable_path, String),
        name!(node, Option<pgrx::JsonB>),
    ),
> {
    with_panic_boundary("path()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        acl::check_table_acl(source_table.to_u32()).unwrap_or_else(|err| err.report());
        acl::check_table_acl(target_table.to_u32()).unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
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
            true,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let mut workspace = workflow_workspace(&governor).unwrap_or_else(|err| err.report());
        let readable = readable_shortest_path_governed(&rows, &governor, &mut workspace)
            .unwrap_or_else(|err| err.report());
        let mut output = Vec::new();
        reserve_workflow_vector(&mut workspace, &mut output, rows.len())
            .unwrap_or_else(|err| err.report());
        for (step, node_table, node_id, edge_label, node, node_table_name) in rows {
            workflow_step(&governor, 1).unwrap_or_else(|err| err.report());
            let readable_copy =
                fallible_string_copy(&readable, &mut workspace).unwrap_or_else(|err| err.report());
            output.push((
                step,
                node_table,
                node_table_name,
                node_id,
                edge_label,
                readable_copy,
                node,
            ));
        }
        workspace.retain_until_governor_drop();
        TableIterator::new(output)
    })
}

/// Search both endpoint names and return the shortest discovered connection.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn connection(
    source_key: &str,
    source_value: &str,
    target_key: &str,
    target_value: &str,
    source_table: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    target_table: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    source_k: default!(i32, 3),
    target_k: default!(i32, 3),
    search_mode: default!(&str, "'contains'"),
    max_depth: default!(i32, 6),
) -> TableIterator<
    'static,
    (
        name!(source_table_name, String),
        name!(source_id, String),
        name!(target_table_name, String),
        name!(target_id, String),
        name!(hop_count, i32),
        name!(step, i32),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_id, String),
        name!(edge_label, Option<String>),
        name!(readable_path, String),
        name!(node, Option<pgrx::JsonB>),
    ),
> {
    with_panic_boundary("connection()", || {
        validate_nonnegative_arg(source_k, "source_k").unwrap_or_else(|err| err.report());
        validate_nonnegative_arg(target_k, "target_k").unwrap_or_else(|err| err.report());
        check_enabled_result().unwrap_or_else(|err| err.report());
        ensure_current_graph().unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let sources = search_rows_governed(
            source_key,
            source_value,
            source_table,
            search_mode,
            false,
            source_k,
            0,
            None,
            false,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let targets = search_rows_governed(
            target_key,
            target_value,
            target_table,
            search_mode,
            false,
            target_k,
            0,
            None,
            false,
            &governor,
        )
        .unwrap_or_else(|err| err.report());

        for (source_oid, source_id, _match_type, _score, source_verified, _node, source_name) in
            &sources
        {
            if !source_verified {
                continue;
            }
            for (target_oid, target_id, _match_type, _score, target_verified, _node, target_name) in
                &targets
            {
                if !target_verified {
                    continue;
                }
                let path_rows = shortest_path_rows_governed(
                    *source_oid,
                    source_id,
                    *target_oid,
                    target_id,
                    max_depth,
                    true,
                    &governor,
                )
                .unwrap_or_else(|err| err.report());
                if path_rows.is_empty() {
                    continue;
                }
                let mut workspace =
                    workflow_workspace(&governor).unwrap_or_else(|err| err.report());
                let readable =
                    readable_shortest_path_governed(&path_rows, &governor, &mut workspace)
                        .unwrap_or_else(|err| err.report());
                let hop_count = path_rows.len().saturating_sub(1).min(i32::MAX as usize) as i32;
                let mut rows = Vec::new();
                reserve_workflow_vector(&mut workspace, &mut rows, path_rows.len())
                    .unwrap_or_else(|err| err.report());
                for (step, node_table, node_id, edge_label, node, node_table_name) in path_rows {
                    workflow_step(&governor, 1).unwrap_or_else(|err| err.report());
                    let source_name = fallible_string_copy(source_name, &mut workspace)
                        .unwrap_or_else(|err| err.report());
                    let source_id = fallible_string_copy(source_id, &mut workspace)
                        .unwrap_or_else(|err| err.report());
                    let target_name = fallible_string_copy(target_name, &mut workspace)
                        .unwrap_or_else(|err| err.report());
                    let target_id = fallible_string_copy(target_id, &mut workspace)
                        .unwrap_or_else(|err| err.report());
                    let readable = fallible_string_copy(&readable, &mut workspace)
                        .unwrap_or_else(|err| err.report());
                    rows.push((
                        source_name,
                        source_id,
                        target_name,
                        target_id,
                        hop_count,
                        step,
                        node_table,
                        node_table_name,
                        node_id,
                        edge_label,
                        readable,
                        node,
                    ));
                }
                workspace.retain_until_governor_drop();
                return TableIterator::new(rows);
            }
        }
        TableIterator::new(Vec::new())
    })
}

fn reserve_workflow_rows(
    governor: &crate::resource::ResourceGovernor,
    rows: usize,
) -> safety::GraphResult<crate::resource::ResourceLease<'_>> {
    let bytes = rows
        .checked_mul(256)
        .and_then(crate::resource::ByteCount::from_usize)
        .ok_or_else(|| {
            safety::GraphError::Internal("workflow output estimate overflowed".to_string())
        })?;
    governor
        .reserve_memory(crate::resource::ResourcePhase::QueryCandidates, bytes)
        .map_err(crate::safety::resource_limit_error)
}

/// Summarize graph neighborhood size by depth and table before hydration.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL ABI exposes each SQL argument and row column"
)]
fn neighborhood(
    property_key: &str,
    property_value: &str,
    source_table: default!(Option<pgrx::pg_sys::Oid>, "NULL"),
    search_mode: default!(&str, "'contains'"),
    search_max_rows: default!(i32, 1),
    max_depth: default!(i32, 4),
    edge_types: default!(Option<Vec<String>>, "NULL"),
    direction: default!(&str, "'any'"),
    tenant: default!(Option<String>, "NULL"),
    sample_k: default!(i32, 5),
    node_limit: default!(i32, 10000),
) -> TableIterator<
    'static,
    (
        name!(depth, i32),
        name!(node_table, pgrx::pg_sys::Oid),
        name!(node_table_name, String),
        name!(node_count, i64),
        name!(sample_nodes, pgrx::JsonB),
        name!(truncated, bool),
    ),
> {
    with_panic_boundary("neighborhood()", || {
        validate_nonnegative_arg(search_max_rows, "search_max_rows")
            .unwrap_or_else(|err| err.report());
        let sample_k =
            validate_nonnegative_arg(sample_k, "sample_k").unwrap_or_else(|err| err.report());
        validate_nonnegative_arg(node_limit, "node_limit").unwrap_or_else(|err| err.report());
        check_enabled_result().unwrap_or_else(|err| err.report());
        ensure_current_graph().unwrap_or_else(|err| err.report());
        let governor = ENGINE
            .with(|engine| engine.borrow().query_resource_governor())
            .unwrap_or_else(|err| err.report());
        let rows = traverse_search_rows_governed(
            property_key,
            property_value,
            source_table,
            search_mode,
            false,
            search_max_rows,
            0,
            max_depth,
            edge_types.as_deref(),
            direction,
            None,
            None,
            tenant.as_deref(),
            "bfs",
            "node_per_root",
            false,
            false,
            node_limit,
            0,
            &governor,
        )
        .unwrap_or_else(|err| err.report());
        let truncated =
            (node_limit > 0 && rows.len() == node_limit as usize) || rows.iter().any(|row| row.10);
        let mut workspace = workflow_workspace(&governor).unwrap_or_else(|err| err.report());
        let mut entries = Vec::new();
        reserve_workflow_vector(&mut workspace, &mut entries, rows.len())
            .unwrap_or_else(|err| err.report());
        for (
            _root_table,
            _root_id,
            node_table,
            node_id,
            depth,
            _path,
            _edge_path,
            _node,
            _root_table_name,
            node_table_name,
            _capped,
        ) in rows
        {
            workflow_step(&governor, 1).unwrap_or_else(|err| err.report());
            entries.push((depth, node_table.to_u32(), node_table_name, node_id));
        }
        let sort_levels = usize::BITS - entries.len().max(1).leading_zeros();
        let sort_work = entries
            .len()
            .checked_mul(sort_levels as usize)
            .and_then(|units| u64::try_from(units).ok())
            .unwrap_or(u64::MAX);
        workflow_step(&governor, sort_work).unwrap_or_else(|err| err.report());
        entries.sort_unstable_by(|left, right| {
            (&left.0, &left.1, &left.2, &left.3).cmp(&(&right.0, &right.1, &right.2, &right.3))
        });

        let mut summary = Vec::new();
        reserve_workflow_vector(&mut workspace, &mut summary, entries.len())
            .unwrap_or_else(|err| err.report());
        let mut start = 0usize;
        while start < entries.len() {
            workflow_step(&governor, 1).unwrap_or_else(|err| err.report());
            let (depth, table_oid, table_name, _) = &entries[start];
            let mut end = start + 1;
            while end < entries.len()
                && entries[end].0 == *depth
                && entries[end].1 == *table_oid
                && entries[end].2 == *table_name
            {
                workflow_step(&governor, 1).unwrap_or_else(|err| err.report());
                end += 1;
            }
            let sample_count = (end - start).min(sample_k);
            let mut sample_nodes = Vec::new();
            reserve_workflow_vector(&mut workspace, &mut sample_nodes, sample_count)
                .unwrap_or_else(|err| err.report());
            for (_, _, node_table_name, node_id) in entries[start..end].iter().take(sample_count) {
                workflow_step(&governor, 1).unwrap_or_else(|err| err.report());
                let json_bytes = node_table_name
                    .len()
                    .checked_add(node_id.len())
                    .and_then(|bytes| bytes.checked_add(256))
                    .unwrap_or_else(|| {
                        safety::GraphError::Internal(
                            "neighborhood sample size overflowed".to_string(),
                        )
                        .report()
                    });
                grow_workflow_workspace(&mut workspace, json_bytes)
                    .unwrap_or_else(|err| err.report());
                sample_nodes.push(serde_json::json!({
                    "table": node_table_name,
                    "id": node_id,
                }));
            }
            let table_name =
                fallible_string_copy(table_name, &mut workspace).unwrap_or_else(|err| err.report());
            summary.push((
                *depth,
                pgrx::pg_sys::Oid::from_u32(*table_oid),
                table_name,
                (end - start) as i64,
                pgrx::JsonB(serde_json::Value::Array(sample_nodes)),
                truncated,
            ));
            start = end;
        }
        workspace.retain_until_governor_drop();
        TableIterator::new(summary)
    })
}

fn workflow_target_tables(
    target_table: Option<pgrx::pg_sys::Oid>,
    target_tables: Option<Vec<pgrx::pg_sys::Oid>>,
) -> safety::GraphResult<Option<Vec<pgrx::pg_sys::Oid>>> {
    match (target_table, target_tables) {
        (Some(_), Some(_)) => Err(safety::GraphError::InvalidFilter {
            reason: "target_table and target_tables cannot both be set".to_string(),
        }),
        (Some(table), None) => Ok(Some(vec![table])),
        (None, Some(tables)) => Ok(Some(tables)),
        (None, None) => Ok(None),
    }
}

fn validate_nonnegative_arg(value: i32, name: &str) -> safety::GraphResult<usize> {
    usize_from_nonnegative(value, name)
}

fn rank_from_offset(offset: usize, idx: usize) -> i32 {
    offset
        .saturating_add(idx)
        .saturating_add(1)
        .min(i32::MAX as usize) as i32
}

fn workflow_workspace(
    governor: &crate::resource::ResourceGovernor,
) -> safety::GraphResult<crate::resource::ResourceLease<'_>> {
    governor
        .reserve_memory(
            crate::resource::ResourcePhase::QueryCandidates,
            crate::resource::ByteCount::ZERO,
        )
        .map_err(crate::safety::resource_limit_error)
}

fn grow_workflow_workspace(
    workspace: &mut crate::resource::ResourceLease<'_>,
    bytes: usize,
) -> safety::GraphResult<()> {
    let bytes = crate::resource::ByteCount::from_usize(bytes).ok_or_else(|| {
        safety::GraphError::Internal("workflow output size does not fit u64".to_string())
    })?;
    workspace
        .try_grow(bytes)
        .map_err(crate::safety::resource_limit_error)
}

fn reserve_workflow_vector<T>(
    workspace: &mut crate::resource::ResourceLease<'_>,
    rows: &mut Vec<T>,
    additional: usize,
) -> safety::GraphResult<()> {
    let bytes = std::mem::size_of::<T>()
        .checked_mul(additional)
        .ok_or_else(|| {
            safety::GraphError::Internal("workflow vector size overflowed".to_string())
        })?;
    grow_workflow_workspace(workspace, bytes)?;
    rows.try_reserve_exact(additional).map_err(|error| {
        safety::GraphError::Internal(format!("workflow vector allocation failed: {error}"))
    })
}

fn workflow_step(
    governor: &crate::resource::ResourceGovernor,
    units: u64,
) -> safety::GraphResult<()> {
    crate::resource::check_postgres_interrupts();
    governor
        .check_elapsed(crate::resource::ResourcePhase::QueryCandidates)
        .and_then(|()| {
            governor.consume_work(
                crate::resource::ResourcePhase::QueryCandidates,
                crate::resource::WorkUnits::new(units),
            )
        })
        .map_err(crate::safety::resource_limit_error)
}

fn fallible_string_copy(
    value: &str,
    workspace: &mut crate::resource::ResourceLease<'_>,
) -> safety::GraphResult<String> {
    grow_workflow_workspace(workspace, value.len())?;
    let mut copy = String::new();
    copy.try_reserve_exact(value.len()).map_err(|error| {
        safety::GraphError::Internal(format!("workflow string allocation failed: {error}"))
    })?;
    copy.push_str(value);
    Ok(copy)
}

fn readable_path_governed(
    path: &pgrx::JsonB,
    edge_path: &pgrx::JsonB,
    governor: &crate::resource::ResourceGovernor,
    workspace: &mut crate::resource::ResourceLease<'_>,
) -> safety::GraphResult<String> {
    let path = path
        .0
        .as_array()
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: "path must be a JSON array".to_string(),
        })?;
    let edges = edge_path
        .0
        .as_array()
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: "edge_path must be a JSON array".to_string(),
        })?;
    if edges.len() != path.len().saturating_sub(1) {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!(
                "edge_path length ({}) must equal path length minus one ({})",
                edges.len(),
                path.len().saturating_sub(1)
            ),
        });
    }

    let mut fields = Vec::new();
    reserve_workflow_vector(workspace, &mut fields, edges.len())?;
    let mut output_len = 0usize;
    for (nodes, edge) in path.windows(2).zip(edges) {
        workflow_step(governor, 1)?;
        let from_table = workflow_path_field(&nodes[0], "table")?;
        let from_id = workflow_path_field(&nodes[0], "id")?;
        let label = edge
            .as_str()
            .ok_or_else(|| safety::GraphError::InvalidFilter {
                reason: "edge_path entries must be strings".to_string(),
            })?;
        let to_table = workflow_path_field(&nodes[1], "table")?;
        let to_id = workflow_path_field(&nodes[1], "id")?;
        let hop_len = from_table
            .len()
            .checked_add(from_id.len())
            .and_then(|bytes| bytes.checked_add(label.len()))
            .and_then(|bytes| bytes.checked_add(to_table.len()))
            .and_then(|bytes| bytes.checked_add(to_id.len()))
            .and_then(|bytes| bytes.checked_add(12))
            .ok_or_else(|| {
                safety::GraphError::Internal("readable path size overflowed".to_string())
            })?;
        output_len = output_len
            .checked_add(hop_len)
            .and_then(|bytes| bytes.checked_add(usize::from(!fields.is_empty()) * 3))
            .ok_or_else(|| {
                safety::GraphError::Internal("readable path size overflowed".to_string())
            })?;
        fields.push((from_table, from_id, label, to_table, to_id));
    }

    grow_workflow_workspace(workspace, output_len)?;
    let mut readable = String::new();
    readable.try_reserve_exact(output_len).map_err(|error| {
        safety::GraphError::Internal(format!("readable path allocation failed: {error}"))
    })?;
    for (index, (from_table, from_id, label, to_table, to_id)) in fields.into_iter().enumerate() {
        if index != 0 {
            readable.push_str(" | ");
        }
        use std::fmt::Write as _;
        write!(
            readable,
            "{from_table}:{from_id} --{label}--> {to_table}:{to_id}"
        )
        .map_err(|error| {
            safety::GraphError::Internal(format!("readable path formatting failed: {error}"))
        })?;
    }
    Ok(readable)
}

fn workflow_path_field<'a>(
    node: &'a serde_json::Value,
    field: &str,
) -> safety::GraphResult<&'a str> {
    node.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: format!("path entries must contain string field '{field}'"),
        })
}

type WorkflowPathRow = (
    i32,
    pgrx::pg_sys::Oid,
    String,
    Option<String>,
    Option<pgrx::JsonB>,
    String,
);

fn readable_shortest_path_governed(
    rows: &[WorkflowPathRow],
    governor: &crate::resource::ResourceGovernor,
    workspace: &mut crate::resource::ResourceLease<'_>,
) -> safety::GraphResult<String> {
    let mut output_len = 0usize;
    for (index, pair) in rows.windows(2).enumerate() {
        workflow_step(governor, 1)?;
        let from = &pair[0];
        let to = &pair[1];
        output_len = output_len
            .checked_add(from.5.len())
            .and_then(|bytes| bytes.checked_add(from.2.len()))
            .and_then(|bytes| bytes.checked_add(to.edge_label_string().len()))
            .and_then(|bytes| bytes.checked_add(to.5.len()))
            .and_then(|bytes| bytes.checked_add(to.2.len()))
            .and_then(|bytes| bytes.checked_add(12 + usize::from(index != 0) * 3))
            .ok_or_else(|| {
                safety::GraphError::Internal("readable path size overflowed".to_string())
            })?;
    }
    grow_workflow_workspace(workspace, output_len)?;
    let mut readable = String::new();
    readable.try_reserve_exact(output_len).map_err(|error| {
        safety::GraphError::Internal(format!("readable path allocation failed: {error}"))
    })?;
    for (index, pair) in rows.windows(2).enumerate() {
        if index != 0 {
            readable.push_str(" | ");
        }
        let from = &pair[0];
        let to = &pair[1];
        use std::fmt::Write as _;
        write!(
            readable,
            "{}:{} --{}--> {}:{}",
            from.5,
            from.2,
            to.edge_label_string(),
            to.5,
            to.2
        )
        .map_err(|error| {
            safety::GraphError::Internal(format!("readable path formatting failed: {error}"))
        })?;
    }
    Ok(readable)
}

trait ShortestPathTupleExt {
    fn edge_label_string(&self) -> &str;
}

impl ShortestPathTupleExt for WorkflowPathRow {
    fn edge_label_string(&self) -> &str {
        self.3.as_deref().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn governor(memory_bytes: u64) -> crate::resource::ResourceGovernor {
        crate::resource::ResourceGovernor::new(crate::resource::ResourceLimits::bounded(
            crate::resource::MemoryBudget::new(crate::resource::ByteCount::from_bytes(
                memory_bytes,
            )),
            crate::resource::DiskBudget::UNLIMITED,
            crate::resource::RowCount::UNLIMITED,
            crate::resource::WorkUnits::new(10_000),
            crate::resource::ElapsedBudget::new(Duration::MAX),
        ))
    }

    #[test]
    fn readable_path_rejects_before_allocating_over_budget_output() {
        let governor = governor(128);
        let mut workspace = workflow_workspace(&governor).expect("workspace starts empty");
        let long_id = "x".repeat(1_024);
        let path = pgrx::JsonB(serde_json::json!([
            {"table": "public.nodes", "id": long_id},
            {"table": "public.nodes", "id": "target"}
        ]));
        let edges = pgrx::JsonB(serde_json::json!(["REL"]));

        let error = readable_path_governed(&path, &edges, &governor, &mut workspace)
            .expect_err("readable output must not cross the small query budget");
        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));
    }

    #[test]
    fn repeated_shortest_path_output_is_charged_cumulatively() {
        let governor = governor(512);
        let mut workspace = workflow_workspace(&governor).expect("workspace starts empty");
        let rows = vec![
            (
                0,
                pgrx::pg_sys::Oid::from_u32(1),
                "a".repeat(128),
                None,
                None,
                "public.nodes".to_string(),
            ),
            (
                1,
                pgrx::pg_sys::Oid::from_u32(1),
                "b".repeat(128),
                Some("REL".to_string()),
                None,
                "public.nodes".to_string(),
            ),
        ];
        let readable = readable_shortest_path_governed(&rows, &governor, &mut workspace)
            .expect("the first readable path fits");

        let error = fallible_string_copy(&readable, &mut workspace)
            .expect_err("a repeated output copy must consume the remaining budget");
        assert!(matches!(error, safety::GraphError::ResourceLimit { .. }));
    }
}
