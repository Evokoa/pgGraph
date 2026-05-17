/// Compute connected components.
///
/// Returns one row per node with its component ID and component size.
/// This is a global O(V+E) algorithm — it touches every node and edge.
#[pg_extern(schema = "graph")]
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
            let cc_result = eng
                .connected_components()
                .unwrap_or_else(|err| err.report());

            let rows = connected_components::to_component_rows(&cc_result, &eng.node_store);
            rows.into_iter()
                .map(|r| {
                    (
                        pgrx::pg_sys::Oid::from_u32(r.node_table.0),
                        r.node_id,
                        r.component_id as i64,
                        r.component_size as i32,
                    )
                })
                .collect::<Vec<_>>()
        });
        Ok(TableIterator::new(rows))
    })
}

/// Summary of connected components (without per-node output).
///
/// Returns a single row with component count, largest component size, isolated
/// node count, and total active node count.
#[pg_extern(schema = "graph")]
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
            let cc_result = eng
                .connected_components()
                .unwrap_or_else(|err| err.report());

            // Count isolated nodes (component_size == 1)
            let mut sizes = std::collections::HashMap::new();
            for &comp in &cc_result.component {
                if comp != u32::MAX {
                    *sizes.entry(comp).or_insert(0u32) += 1;
                }
            }
            let isolated = sizes.values().filter(|&&v| v == 1).count() as i32;
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
            let cc_result = eng
                .connected_components()
                .unwrap_or_else(|err| err.report());
            let mut sizes = std::collections::HashMap::new();
            for &comp in &cc_result.component {
                if comp != u32::MAX {
                    *sizes.entry(comp).or_insert(0i64) += 1;
                }
            }
            let row_offset = usize_from_nonnegative(row_offset, "row_offset")
                .unwrap_or_else(|err| err.report());
            let max_rows =
                usize_from_nonnegative(max_rows, "max_rows").unwrap_or_else(|err| err.report());
            let mut rows = sizes
                .into_iter()
                .map(|(component_id, component_size)| (component_id as i64, component_size))
                .collect::<Vec<_>>();
            rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            rows.into_iter()
                .enumerate()
                .skip(row_offset)
                .take(max_rows)
                .map(|(idx, (component_id, component_size))| {
                    (component_id, component_size, (idx + 1) as i32)
                })
                .collect::<Vec<_>>()
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
        let component_id = largest_component_id().unwrap_or_else(|err| err.report());
        let rows = component_rows(component_id, max_rows, row_offset, hydrate)
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

        let page = ENGINE.with(|e| {
            let eng = e.borrow();
            let cc_result = eng
                .connected_components()
                .unwrap_or_else(|err| err.report());
            let rows = connected_components::to_component_rows(&cc_result, &eng.node_store);
            let mut rows = rows
                .into_iter()
                .filter(|row| row.component_size == 1)
                .collect::<Vec<_>>();
            rows.sort_by(|left, right| {
                left.component_id
                    .cmp(&right.component_id)
                    .then_with(|| left.node_table.0.cmp(&right.node_table.0))
                    .then_with(|| left.node_id.cmp(&right.node_id))
            });
            rows.into_iter()
                .skip(row_offset)
                .take(max_rows)
                .collect::<Vec<_>>()
        });
        let rows = hydrate_component_page(page, hydrate).unwrap_or_else(|err| err.report());

        Ok(TableIterator::new(rows))
    })
}
