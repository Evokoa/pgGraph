/// Reset the engine — clear graph and remove persisted files.
#[pg_extern(schema = "graph")]
fn reset() {
    with_panic_boundary("reset()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        ENGINE.with(|e| {
            *e.borrow_mut() = Engine::new();
        });

        // Remove persisted file
        let path = persistence::graph_file_path();
        if path.exists() {
            std::fs::remove_file(&path).ok();
            pgrx::notice!("graph: removed persisted file {}", path.display());
        }
        let checkpoint_path = persistence::sync_checkpoint_path(&path);
        if checkpoint_path.exists() {
            std::fs::remove_file(&checkpoint_path).ok();
        }
    });
}

// ─────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────

fn largest_component_id() -> safety::GraphResult<i64> {
    check_enabled_result()?;
    require_graph_admin_result()?;
    ensure_current_graph()?;
    ENGINE.with(|e| {
        let eng = e.borrow();
        let cc_result = eng.connected_components()?;
        let mut sizes = std::collections::HashMap::new();
        for &comp in &cc_result.component {
            if comp != u32::MAX {
                *sizes.entry(comp).or_insert(0i64) += 1;
            }
        }
        sizes
            .into_iter()
            .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
            .map(|(component_id, _)| component_id as i64)
            .ok_or(safety::GraphError::NotBuilt)
    })
}

fn component_rows(
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
    ensure_current_graph()?;
    let offset = usize_from_nonnegative(offset, "offset")?;
    let limit = usize_from_nonnegative(limit, "limit")?;

    let page = ENGINE.with(|e| {
        let eng = e.borrow();
        let cc_result = eng.connected_components()?;
        let rows = connected_components::to_component_rows(&cc_result, &eng.node_store);
        let mut rows = rows
            .into_iter()
            .filter(|row| row.component_id as i64 == component_id)
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.node_table
                .0
                .cmp(&right.node_table.0)
                .then_with(|| left.node_id.cmp(&right.node_id))
        });
        Ok::<_, safety::GraphError>(rows.into_iter().skip(offset).take(limit).collect())
    })?;

    hydrate_component_page(page, hydrate)
}

fn hydrate_component_page(
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
fn maybe_auto_load() {
    if !config::AUTO_LOAD.get() {
        return;
    }

    ENGINE.with(|e| {
        let eng = e.borrow();
        if eng.built {
            return; // Already loaded
        }
        drop(eng); // Release borrow before mutating

        // Check if persisted file exists
        let path = persistence::graph_file_path();
        if !path.exists() {
            return;
        }

        // Load from .pggraph file via mmap.
        pgrx::log!("graph: auto-loading from {} (mmap)", path.display());
        match persistence::load_graph_file(&path) {
            Ok(mut loaded_engine) => {
                if let Ok((tables, edges, filters)) = read_catalog() {
                    loaded_engine.catalog_fingerprint =
                        Some(catalog_fingerprint(&tables, &edges, &filters));
                }
                let nc = loaded_engine.node_store.node_count();
                let ec = loaded_engine.edge_store.edge_count();
                *e.borrow_mut() = loaded_engine;
                pgrx::log!(
                    "graph: loaded {} nodes, {} edges (resolution via mmap, zero-copy)",
                    nc,
                    ec
                );
            }
            Err(err) => {
                pgrx::warning!(
                    "graph: auto-load failed: {:?}. Call graph.build() to reconstruct.",
                    err
                );
            }
        }
    });
}

fn ensure_current_graph() -> safety::GraphResult<()> {
    maybe_auto_load();

    let sync_mode = current_sync_mode()?;

    let disabled = disabled_graph_trigger_count()?;
    let (current_fingerprint, schema_drift_reason) = current_catalog_state()?;
    let applied_sync_id = ENGINE.with(|e| e.borrow().applied_sync_id);
    let pending = pending_sync_rows(applied_sync_id)?;
    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.disabled_trigger_count = disabled;
        eng.pending_sync_rows = pending;
        if disabled > 0 {
            eng.schema_state = engine::SchemaState::Stale;
            eng.invalid_reason = Some(format!("{} graph sync trigger(s) are disabled", disabled));
        }
        if eng.built && schema_drift_reason.is_some() {
            eng.needs_rebuild = true;
            eng.schema_state = engine::SchemaState::Invalid;
            eng.invalid_reason = schema_drift_reason;
        }
        if eng.built
            && eng.catalog_fingerprint.is_some()
            && eng.catalog_fingerprint != Some(current_fingerprint)
        {
            eng.needs_rebuild = true;
            eng.schema_state = engine::SchemaState::Invalid;
            eng.invalid_reason = Some(
                "registered graph catalog changed since graph.build(); rebuild required"
                    .to_string(),
            );
        }
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
            eng.sync_status = engine::SyncStatus::Syncing;
        });
    }
    Ok(())
}

fn current_query_freshness() -> safety::GraphResult<config::QueryFreshness> {
    config::parsed_query_freshness().ok_or_else(|| safety::GraphError::InvalidFilter {
        reason: format!(
            "unsupported graph.query_freshness '{}'; expected 'off', 'apply_pending_sync', or 'error_on_pending'",
            config::query_freshness()
        ),
    })
}

fn ensure_current_graph_for_query(
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
            let high_watermark = max_sync_log_id()?;
            apply_sync_until(Some(high_watermark), config::sync_batch_size())?;
            let pending = ENGINE.with(|e| pending_sync_rows(e.borrow().applied_sync_id))?;
            ENGINE.with(|e| {
                let mut eng = e.borrow_mut();
                eng.pending_sync_rows = pending;
                if pending == 0 && !eng.is_read_only {
                    eng.sync_status = engine::SyncStatus::Idle;
                }
            });
            Ok(())
        }
    }
}
