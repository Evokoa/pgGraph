use super::admin::{check_enabled_result, require_graph_admin_result, with_panic_boundary};
use super::*;

/// Reset the engine — clear graph and remove persisted files.
#[pg_extern(schema = "graph")]
fn reset() {
    with_panic_boundary("reset()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        ENGINE.with(|e| {
            *e.borrow_mut() = Engine::new();
        });
        crate::runtime_state::clear_loaded_graph();

        let graph =
            catalog::selected_or_default_graph_metadata().unwrap_or_else(|err| err.report());
        persistence::remove_graph_artifacts_for(&graph.graph_id).unwrap_or_else(|err| err.report());
        pgrx::notice!(
            "graph: removed persisted files for graph {} ({})",
            graph.graph_name,
            graph.graph_id
        );
    })
}

#[pg_extern(schema = "graph")]
fn select_graph(
    graph_name: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(loaded, bool),
    ),
> {
    with_panic_boundary("select_graph()", || {
        let graph = resolve_visible_runtime_graph(graph_name, tenant, namespace);
        catalog::set_selected_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report());
        let mut loaded = crate::runtime_state::selected_graph_matches_loaded_slot(&graph.graph_id);
        if !loaded && crate::runtime_state::loaded_graph_id().is_some() {
            ENGINE.with(|engine| {
                *engine.borrow_mut() = Engine::new();
            });
            crate::runtime_state::clear_loaded_graph();
        }
        if !loaded && graph.residency == "hot" && config::HOT_EAGER_LOAD.get() {
            if let Err(err) = load_selected_graph_from_disk(&graph, true) {
                pgrx::warning!("graph: hot eager-load skipped: {}", err);
            }
            loaded = crate::runtime_state::selected_graph_matches_loaded_slot(&graph.graph_id);
        }
        TableIterator::new(vec![(graph.graph_id, graph.graph_name, loaded)])
    })
}

#[pg_extern(schema = "graph")]
fn load_graph(
    graph_name: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(loaded, bool),
        name!(node_count, Option<i64>),
        name!(edge_count, Option<i64>),
        name!(memory_used_mb, Option<f64>),
        name!(projection_mode, Option<String>),
    ),
> {
    with_panic_boundary("load_graph()", || {
        let graph = resolve_visible_runtime_graph(graph_name, tenant, namespace);
        catalog::require_graph_privilege(&graph, catalog::GraphPrivilege::Admin)
            .unwrap_or_else(|err| err.report());
        catalog::set_selected_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report());
        load_selected_graph_from_disk(&graph, false).unwrap_or_else(|err| err.report());
        let snapshot =
            ENGINE.with(|engine| crate::runtime_state::loaded_graph_snapshot(&engine.borrow()));
        let row = snapshot
            .filter(|snapshot| snapshot.graph_id == graph.graph_id)
            .map(|snapshot| {
                (
                    graph.graph_id.clone(),
                    graph.graph_name.clone(),
                    true,
                    Some(snapshot.node_count),
                    Some(snapshot.edge_count),
                    Some(snapshot.memory_used_mb),
                    Some(snapshot.projection_mode),
                )
            })
            .unwrap_or((
                graph.graph_id,
                graph.graph_name,
                false,
                None,
                None,
                None,
                None,
            ));
        TableIterator::new(vec![row])
    })
}

#[pg_extern(schema = "graph")]
fn unload_graph(
    graph_name: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(unloaded, bool),
    ),
> {
    with_panic_boundary("unload_graph()", || {
        let graph = resolve_visible_runtime_graph(graph_name, tenant, namespace);
        catalog::require_graph_privilege(&graph, catalog::GraphPrivilege::Admin)
            .unwrap_or_else(|err| err.report());
        let unloaded = crate::runtime_state::selected_graph_matches_loaded_slot(&graph.graph_id);
        if unloaded {
            ENGINE.with(|engine| {
                *engine.borrow_mut() = Engine::new();
            });
            crate::runtime_state::clear_loaded_graph();
        }
        TableIterator::new(vec![(graph.graph_id, graph.graph_name, unloaded)])
    })
}

#[pg_extern(schema = "graph")]
fn loaded_graphs() -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(residency, String),
        name!(node_count, i64),
        name!(edge_count, i64),
        name!(memory_used_mb, f64),
        name!(projection_mode, String),
        name!(last_access_unix_micros, i64),
    ),
> {
    with_panic_boundary("loaded_graphs()", || {
        let row = ENGINE.with(|engine| {
            crate::runtime_state::loaded_graph_snapshot(&engine.borrow()).map(|mut snapshot| {
                if let Ok(graphs) = catalog::list_graph_metadata() {
                    if let Some(graph) = graphs
                        .into_iter()
                        .find(|graph| graph.graph_id == snapshot.graph_id)
                    {
                        snapshot.residency = graph.residency;
                    }
                }
                (
                    snapshot.graph_id,
                    snapshot.graph_name,
                    snapshot.residency,
                    snapshot.node_count,
                    snapshot.edge_count,
                    snapshot.memory_used_mb,
                    snapshot.projection_mode,
                    snapshot.last_access_unix_micros,
                )
            })
        });
        TableIterator::new(row.into_iter().collect::<Vec<_>>())
    })
}

#[pg_extern(schema = "graph")]
fn graph_runtime_status() -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(residency, String),
        name!(loaded, bool),
        name!(artifact_exists, bool),
        name!(artifact_bytes, Option<i64>),
        name!(node_count, Option<i64>),
        name!(edge_count, Option<i64>),
        name!(memory_used_mb, Option<f64>),
        name!(projection_mode, Option<String>),
        name!(last_access_unix_micros, Option<i64>),
    ),
> {
    with_panic_boundary("graph_runtime_status()", || {
        let loaded =
            ENGINE.with(|engine| crate::runtime_state::loaded_graph_snapshot(&engine.borrow()));
        let rows = catalog::list_graph_metadata()
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .map(|graph| {
                let artifact = persistence::graph_file_path_for_uncreated(&graph.graph_id).ok();
                let artifact_metadata = artifact.as_ref().and_then(|path| path.metadata().ok());
                let artifact_exists = artifact_metadata.is_some();
                let artifact_bytes =
                    artifact_metadata.and_then(|metadata| i64::try_from(metadata.len()).ok());
                let snapshot = loaded
                    .as_ref()
                    .filter(|snapshot| snapshot.graph_id == graph.graph_id);
                (
                    graph.graph_id,
                    graph.graph_name,
                    graph.residency,
                    snapshot.is_some(),
                    artifact_exists,
                    artifact_bytes,
                    snapshot.map(|snapshot| snapshot.node_count),
                    snapshot.map(|snapshot| snapshot.edge_count),
                    snapshot.map(|snapshot| snapshot.memory_used_mb),
                    snapshot.map(|snapshot| snapshot.projection_mode.clone()),
                    snapshot.map(|snapshot| snapshot.last_access_unix_micros),
                )
            })
            .collect::<Vec<_>>();
        TableIterator::new(rows)
    })
}

fn resolve_visible_runtime_graph(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
) -> catalog::GraphMetadata {
    catalog::resolve_visible_graph_metadata(graph_name, tenant, namespace)
        .unwrap_or_else(|err| err.report())
        .unwrap_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: format!("graph '{}' does not exist", graph_name),
            }
            .report()
        })
}

// ─────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────

pub(super) fn largest_component_id() -> safety::GraphResult<i64> {
    check_enabled_result()?;
    require_graph_admin_result()?;
    ensure_current_graph_for_query(current_query_freshness()?)?;
    ENGINE.with(|e| {
        let eng = e.borrow();
        let cc_result = eng.connected_components()?;
        cc_result
            .component_sizes
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
            .map(|(&component_id, _)| component_id as i64)
            .ok_or(safety::GraphError::NotBuilt)
    })
}

pub(super) fn component_rows(
    component_id: i64,
    limit: i32,
    offset: i32,
    hydrate: bool,
) -> safety::GraphResult<Vec<ComponentNodeRow>> {
    if component_id < 0 {
        return Err(safety::GraphError::InvalidFilter {
            reason: "component_id must be non-negative".to_string(),
        });
    }
    check_enabled_result()?;
    require_graph_admin_result()?;
    ensure_current_graph_for_query(current_query_freshness()?)?;
    let offset = usize_from_nonnegative(offset, "offset")?;
    let limit = usize_from_nonnegative(limit, "limit")?;

    let page = ENGINE.with(|e| {
        let eng = e.borrow();
        let cc_result = eng.connected_components()?;
        Ok::<_, safety::GraphError>(connected_components::component_rows_page(
            &cc_result,
            &eng.node_store,
            component_id as u32,
            offset,
            limit,
        ))
    })?;

    hydrate_component_page(page, hydrate)
}

pub(super) fn hydrate_component_page(
    page: Vec<connected_components::ComponentRow>,
    hydrate: bool,
) -> safety::GraphResult<Vec<ComponentNodeRow>> {
    let traversal_rows = page
        .iter()
        .map(|row| types::TraversalResult {
            node_table: row.node_table,
            node_id: row.node_id.clone(),
            depth: 0,
            path: Vec::new(),
            edge_path: Vec::new(),
        })
        .collect::<Vec<_>>();
    let mut hydrated = if hydrate {
        hydrate_nodes(&traversal_rows)?
    } else {
        HashMap::new()
    };

    Ok(page
        .into_iter()
        .map(|row| {
            let node = hydrated.remove(&(row.node_table.0, row.node_id.clone()));
            (
                row.component_id as i64,
                pgrx::pg_sys::Oid::from_u32(row.node_table.0),
                row.node_id,
                node,
            )
        })
        .collect())
}

/// Auto-load the persisted graph if the engine is empty and auto_load is enabled.
///
/// When a .pggraph file exists, this loads the graph via mmap. NodeStore base
/// arrays, the forward EdgeStore CSR, and the ResolutionIndex are mmap-backed.
/// FilterIndex and the edge type registry are bincode-deserialized into
/// backend-local heap, and the reverse EdgeStore CSR is rebuilt into heap for
/// inbound traversal.
pub(super) fn maybe_auto_load() {
    let graph = match catalog::selected_or_default_graph_metadata() {
        Ok(graph) => graph,
        Err(err) => {
            pgrx::warning!("graph: auto-load skipped: {}", err);
            return;
        }
    };

    clear_loaded_graph_if_mismatched(&graph.graph_id);

    if !config::AUTO_LOAD.get() {
        return;
    }

    if let Err(err) = load_selected_graph_from_disk(&graph, true) {
        pgrx::warning!("graph: auto-load skipped: {}", err);
    }
}

pub(super) fn clear_loaded_graph_if_mismatched(graph_id: &str) {
    if let Some(loaded_graph_id) = crate::runtime_state::loaded_graph_id() {
        if loaded_graph_id != graph_id {
            ENGINE.with(|engine| {
                *engine.borrow_mut() = Engine::new();
            });
            crate::runtime_state::clear_loaded_graph();
        }
    }
}

fn load_selected_graph_from_disk(
    graph: &catalog::GraphMetadata,
    quiet_missing: bool,
) -> safety::GraphResult<bool> {
    if quiet_missing && graph.residency == "cold" {
        return Ok(false);
    }
    ENGINE.with(|e| {
        let eng = e.borrow();
        if eng.built {
            if crate::runtime_state::selected_graph_matches_loaded_slot(&graph.graph_id) {
                crate::runtime_state::touch_loaded_graph(&graph.graph_id);
                return Ok(true);
            }
            drop(eng);
            *e.borrow_mut() = Engine::new();
            crate::runtime_state::clear_loaded_graph();
        } else {
            drop(eng);
        }

        // Check if persisted file exists without creating artifact directories
        // during query-time auto-load or operator load inspection.
        let path = persistence::graph_file_path_for_uncreated(&graph.graph_id)?;
        if !path.exists() {
            if !quiet_missing {
                return Err(safety::GraphError::NotBuilt);
            }
            return Ok(false);
        }

        catalog::enforce_loaded_graph_quota(1)?;
        if config::MAX_LOADED_GRAPHS_PER_BACKEND.get() < 1 {
            return Err(safety::GraphError::InvalidFilter {
                reason: "graph.max_loaded_graphs_per_backend blocks runtime graph loads"
                    .to_string(),
            });
        }

        // Load from .pggraph file via mmap.
        pgrx::log!("graph: loading from {} (mmap)", path.display());
        match persistence::load_graph_file(&path) {
            Ok(mut loaded_engine) => {
                if let Ok((tables, edges, filters)) = read_catalog() {
                    loaded_engine
                        .set_catalog_fingerprint(catalog_fingerprint(&tables, &edges, &filters));
                }
                let nc = loaded_engine.node_store.node_count();
                let ec = loaded_engine.edge_store.edge_count();
                *e.borrow_mut() = loaded_engine;
                crate::runtime_state::mark_loaded_graph(&graph);
                pgrx::log!(
                    "graph: loaded {} nodes, {} edges (resolution via mmap, zero-copy)",
                    nc,
                    ec
                );
                Ok(true)
            }
            Err(err) if quiet_missing => {
                pgrx::warning!(
                    "graph: load failed: {:?}. Call graph.build() to reconstruct.",
                    err
                );
                Ok(false)
            }
            Err(err) => Err(err),
        }
    })
}

pub(crate) fn ensure_current_graph() -> safety::GraphResult<()> {
    let graph = catalog::selected_or_default_graph_metadata()?;
    clear_loaded_graph_if_mismatched(&graph.graph_id);
    catalog::require_graph_privilege(&graph, catalog::GraphPrivilege::Read)?;
    maybe_auto_load();

    let sync_mode = current_sync_mode()?;

    let disabled = disabled_graph_trigger_count()?;
    let catalog_state = current_catalog_state()?;
    let applied_sync_id = ENGINE.with(|e| e.borrow().applied_sync_id);
    let pending = pending_sync_rows(applied_sync_id)?;
    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.refresh_observed_state(disabled, pending, &Ok(catalog_state));
        if matches!(eng.schema_state, engine::SchemaState::Invalid) {
            return Err(safety::GraphError::Internal(
                eng.invalid_reason
                    .clone()
                    .unwrap_or_else(|| "registered graph schema is invalid".to_string()),
            ));
        }
        Ok::<_, safety::GraphError>(())
    })?;

    if matches!(sync_mode, config::SyncMode::Trigger) && pending > 0 {
        ENGINE.with(|e| {
            let mut eng = e.borrow_mut();
            eng.mark_syncing();
        });
    }
    Ok(())
}

pub(super) fn current_query_freshness() -> safety::GraphResult<config::QueryFreshness> {
    config::parsed_query_freshness().ok_or_else(|| safety::GraphError::InvalidFilter {
        reason: format!(
            "unsupported graph.query_freshness '{}'; expected 'off', 'apply_pending_sync', or 'error_on_pending'",
            config::query_freshness()
        ),
    })
}

pub(super) fn ensure_current_graph_for_query(
    freshness: config::QueryFreshness,
) -> safety::GraphResult<()> {
    ensure_current_graph()?;

    if !matches!(current_sync_mode()?, config::SyncMode::Trigger) {
        return Ok(());
    }

    let pending = ENGINE.with(|e| e.borrow().pending_sync_rows);
    if pending <= 0 {
        return Ok(());
    }

    match freshness {
        config::QueryFreshness::Off => Ok(()),
        config::QueryFreshness::ErrorOnPending => Err(safety::GraphError::InvalidFilter {
            reason: format!(
                "topology read has {pending} pending sync row(s); call graph.apply_sync() or set graph.query_freshness = 'apply_pending_sync'"
            ),
        }),
        config::QueryFreshness::ApplyPendingSync => {
            // Transaction-local overlays already provide read-your-own-writes.
            // Applying pending sync here would fold uncommitted trigger rows into
            // the backend-local base projection and make rollback leak until reset.
            if crate::projection::tx_delta::stats().dirty {
                return Ok(());
            }

            let high_watermark = max_sync_log_id()?;
            apply_sync_to_high_watermark(high_watermark)?;
            let pending = ENGINE.with(|e| pending_sync_rows(e.borrow().applied_sync_id))?;
            ENGINE.with(|e| {
                let mut eng = e.borrow_mut();
                eng.record_pending_sync_rows(pending);
                if pending == 0 {
                    eng.mark_idle_if_writable();
                }
            });
            Ok(())
        }
    }
}
