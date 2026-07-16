use super::admin::{check_enabled_result, require_graph_admin_result, with_panic_boundary};
use super::runtime::{
    component_rows, current_query_freshness, ensure_current_graph_for_query,
    hydrate_component_page_governed, largest_component_rows,
};
use super::*;

/// Compute connected components.
///
/// Returns one row per node with its component ID and component size.
/// This is a global O(V+E) algorithm — it touches every node and edge.
#[pg_extern(schema = "graph", security_definer)]
#[search_path(pg_catalog, pg_temp)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn connected_components() -> Result<
    TableIterator<
        'static,
        (
            name!(node_table, pgrx::pg_sys::Oid),
            name!(node_id, String),
            name!(component_id, i64),
            name!(component_size, i32),
        ),
    >,
    Box<pgrx::pg_sys::panic::ErrorReport>,
> {
    with_panic_boundary("connected_components()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());

        let rows = ENGINE.with(|e| {
            let eng = e.borrow();
            let governor = eng
                .analytics_resource_governor()
                .unwrap_or_else(|err| err.report());
            let cc_result = eng
                .connected_components_governed(&governor)
                .unwrap_or_else(|err| err.report());

            let output_bytes = cc_result
                .component
                .len()
                .checked_mul(
                    std::mem::size_of::<connected_components::ComponentRow>()
                        .saturating_add(eng.node_store.max_primary_key_bytes())
                        .saturating_add(64),
                )
                .and_then(crate::resource::ByteCount::from_usize)
                .unwrap_or_else(|| {
                    safety::GraphError::Internal("component output estimate overflowed".to_string())
                        .report()
                });
            let output_lease = governor
                .reserve_memory(
                    crate::resource::ResourcePhase::QueryCandidates,
                    output_bytes,
                )
                .map_err(crate::safety::resource_limit_error)
                .unwrap_or_else(|err| err.report());
            let rows = connected_components::to_component_rows(&cc_result, &eng.node_store)
                .unwrap_or_else(|err| err.report());
            let rows = rows
                .into_iter()
                .map(|r| {
                    (
                        pgrx::pg_sys::Oid::from_u32(r.node_table.0),
                        r.node_id,
                        r.component_id as i64,
                        r.component_size as i32,
                    )
                })
                .collect::<Vec<_>>();
            output_lease.retain_until_governor_drop();
            rows
        });
        Ok(TableIterator::new(rows))
    })
}

/// Summary of connected components (without per-node output).
///
/// Returns a single row with component count, largest component size, isolated
/// node count, and total active node count.
#[pg_extern(schema = "graph", security_definer)]
#[search_path(pg_catalog, pg_temp)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn component_stats() -> Result<
    TableIterator<
        'static,
        (
            name!(num_components, i32),
            name!(largest_component, i32),
            name!(num_isolated_nodes, i32),
            name!(total_active_nodes, i32),
        ),
    >,
    Box<pgrx::pg_sys::panic::ErrorReport>,
> {
    with_panic_boundary("component_stats()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());

        let result = ENGINE.with(|e| {
            let eng = e.borrow();
            let governor = eng
                .analytics_resource_governor()
                .unwrap_or_else(|err| err.report());
            let cc_result = eng
                .connected_components_governed(&governor)
                .unwrap_or_else(|err| err.report());

            // Count isolated nodes (component_size == 1)
            let isolated = cc_result
                .component_sizes
                .values()
                .filter(|&&v| v == 1)
                .count() as i32;
            let active = eng.node_store.active_count() as i32;

            (
                cc_result.num_components as i32,
                cc_result.largest_component_size as i32,
                isolated,
                active,
            )
        });

        Ok(TableIterator::new(vec![result]))
    })
}

/// List connected components ordered by size.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn components(
    max_rows: default!(i32, 100),
    row_offset: default!(i32, 0),
) -> Result<
    TableIterator<
        'static,
        (
            name!(component_id, i64),
            name!(component_size, i64),
            name!(rank, i32),
        ),
    >,
    Box<pgrx::pg_sys::panic::ErrorReport>,
> {
    with_panic_boundary("components()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());

        let rows = ENGINE.with(|e| {
            let eng = e.borrow();
            let governor = eng
                .analytics_resource_governor()
                .unwrap_or_else(|err| err.report());
            let cc_result = eng
                .connected_components_governed(&governor)
                .unwrap_or_else(|err| err.report());
            let row_offset =
                usize_from_nonnegative(row_offset, "row_offset").unwrap_or_else(|err| err.report());
            let max_rows =
                usize_from_nonnegative(max_rows, "max_rows").unwrap_or_else(|err| err.report());
            let blocking_bytes = cc_result
                .component_sizes
                .len()
                .checked_mul(96)
                .and_then(crate::resource::ByteCount::from_usize)
                .unwrap_or_else(|| {
                    safety::GraphError::Internal(
                        "component ordering estimate overflowed".to_string(),
                    )
                    .report()
                });
            let blocking_lease = governor
                .reserve_memory(
                    crate::resource::ResourcePhase::QueryBlocking,
                    blocking_bytes,
                )
                .map_err(crate::safety::resource_limit_error)
                .unwrap_or_else(|err| err.report());
            governor
                .consume_work(
                    crate::resource::ResourcePhase::QueryBlocking,
                    crate::resource::WorkUnits::new(
                        u64::try_from(cc_result.component_sizes.len()).unwrap_or(u64::MAX),
                    ),
                )
                .map_err(crate::safety::resource_limit_error)
                .unwrap_or_else(|err| err.report());
            let rows = connected_components::component_size_rows(&cc_result)
                .into_iter()
                .enumerate()
                .skip(row_offset)
                .take(max_rows)
                .map(|(idx, (component_id, component_size))| {
                    (component_id as i64, component_size as i64, (idx + 1) as i32)
                })
                .collect::<Vec<_>>();
            blocking_lease.retain_until_governor_drop();
            rows
        });

        Ok(TableIterator::new(rows))
    })
}

/// Return nodes in the largest connected component.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn largest_component(
    max_rows: default!(i32, 100),
    row_offset: default!(i32, 0),
    hydrate: default!(bool, "true"),
) -> Result<
    TableIterator<
        'static,
        (
            name!(component_id, i64),
            name!(node_table, pgrx::pg_sys::Oid),
            name!(node_id, String),
            name!(node, Option<pgrx::JsonB>),
        ),
    >,
    Box<pgrx::pg_sys::panic::ErrorReport>,
> {
    with_panic_boundary("largest_component()", || {
        let rows = largest_component_rows(max_rows, row_offset, hydrate)
            .unwrap_or_else(|err| err.report());
        Ok(TableIterator::new(rows))
    })
}

/// Return nodes in one connected component.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn component(
    component_id: i64,
    max_rows: default!(i32, 100),
    row_offset: default!(i32, 0),
    hydrate: default!(bool, "true"),
) -> Result<
    TableIterator<
        'static,
        (
            name!(component_id, i64),
            name!(node_table, pgrx::pg_sys::Oid),
            name!(node_id, String),
            name!(node, Option<pgrx::JsonB>),
        ),
    >,
    Box<pgrx::pg_sys::panic::ErrorReport>,
> {
    with_panic_boundary("component()", || {
        let rows = component_rows(component_id, max_rows, row_offset, hydrate)
            .unwrap_or_else(|err| err.report());
        Ok(TableIterator::new(rows))
    })
}

/// Return isolated nodes, where the component has exactly one active node.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn isolated_nodes(
    max_rows: default!(i32, 100),
    row_offset: default!(i32, 0),
    hydrate: default!(bool, "true"),
) -> Result<
    TableIterator<
        'static,
        (
            name!(component_id, i64),
            name!(node_table, pgrx::pg_sys::Oid),
            name!(node_id, String),
            name!(node, Option<pgrx::JsonB>),
        ),
    >,
    Box<pgrx::pg_sys::panic::ErrorReport>,
> {
    with_panic_boundary("isolated_nodes()", || {
        check_enabled_result().unwrap_or_else(|err| err.report());
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let freshness = current_query_freshness().unwrap_or_else(|err| err.report());
        ensure_current_graph_for_query(freshness).unwrap_or_else(|err| err.report());
        let row_offset =
            usize_from_nonnegative(row_offset, "row_offset").unwrap_or_else(|err| err.report());
        let max_rows =
            usize_from_nonnegative(max_rows, "max_rows").unwrap_or_else(|err| err.report());

        let (page, governor) = ENGINE.with(|e| {
            let eng = e.borrow();
            let governor = eng
                .analytics_resource_governor()
                .unwrap_or_else(|err| err.report());
            let cc_result = eng
                .connected_components_governed(&governor)
                .unwrap_or_else(|err| err.report());
            let page_bytes = max_rows
                .checked_mul(
                    std::mem::size_of::<connected_components::ComponentRow>()
                        .saturating_add(eng.node_store.max_primary_key_bytes()),
                )
                .and_then(crate::resource::ByteCount::from_usize)
                .unwrap_or_else(|| {
                    safety::GraphError::Internal(
                        "isolated-node page estimate overflowed".to_string(),
                    )
                    .report()
                });
            let page_lease = governor
                .reserve_memory(crate::resource::ResourcePhase::QueryBlocking, page_bytes)
                .map_err(crate::safety::resource_limit_error)
                .unwrap_or_else(|err| err.report());
            let page = connected_components::isolated_rows_page(
                &cc_result,
                &eng.node_store,
                row_offset,
                max_rows,
            )
            .unwrap_or_else(|err| err.report());
            page_lease.retain_until_governor_drop();
            (page, governor)
        });
        let rows = hydrate_component_page_governed(page, hydrate, &governor)
            .unwrap_or_else(|err| err.report());

        Ok(TableIterator::new(rows))
    })
}
