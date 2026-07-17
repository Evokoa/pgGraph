//! SQL-layer build, vacuum, and maintenance execution helpers.

use crate::api_types::{BuildExecutionResult, MaintenanceExecutionResult, VacuumExecutionResult};
use crate::catalog::{read_catalog, selected_or_default_graph_metadata};
use crate::sql_sync::{current_sync_mode, max_sync_log_id};
use crate::{acl, builder, config, engine, persistence, safety, ENGINE};
use pgrx::prelude::*;

/// Advisory lock namespace for pgGraph build/vacuum operations.
///
/// The two-int PostgreSQL advisory lock API is used so the key remains stable
/// across 32-bit and 64-bit platforms. The class id `0x7260_8553` is the
/// reserved pgGraph advisory-lock namespace. The object id is derived from the
/// selected graph id so builds for the same graph serialize while independent
/// graphs can proceed without sharing the old global build lock.
pub(crate) const BUILD_LOCK_CLASS_ID: i32 = 1_918_928_211;

pub(crate) fn build_lock_query_for_graph(graph_id: &str) -> String {
    format!(
        "SELECT pg_try_advisory_xact_lock({BUILD_LOCK_CLASS_ID}, {})",
        graph_build_lock_object_id(graph_id)
    )
}

fn graph_build_lock_object_id(graph_id: &str) -> i32 {
    let mut hash = 0x811c_9dc5_u32;
    for byte in graph_id.bytes().filter(|byte| *byte != b'-') {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    i32::from_ne_bytes(hash.to_ne_bytes())
}

fn plan_rebuilt_base_publication(
    root: &std::path::Path,
) -> safety::GraphResult<crate::projection::recovery::RebuiltBasePublicationPlan> {
    match crate::projection::recovery::plan_generation_specific_rebuilt_base(root) {
        Ok(plan) => Ok(plan),
        Err(
            safety::GraphError::CorruptFile { .. } | safety::GraphError::IncompatibleVersion(_),
        ) => crate::projection::recovery::plan_generation_specific_rebuilt_base_for_recovery(root),
        Err(error) => Err(error),
    }
}

pub(crate) type ProgressCallback<'a> =
    dyn FnMut(&'static str, &'static str) -> safety::GraphResult<()> + 'a;

fn report_progress(
    progress: &mut ProgressCallback<'_>,
    phase: &'static str,
    message: &'static str,
) -> safety::GraphResult<()> {
    progress(phase, message)
}

struct PersistedBuildRequest<'a> {
    operation: &'static str,
    graph_id: &'a str,
    tables: &'a [builder::RegisteredTable],
    edges: &'a [builder::RegisteredEdge],
    filter_columns: &'a [builder::RegisteredFilterColumn],
    projection_mode: config::ProjectionMode,
    publication_plan: &'a crate::projection::recovery::RebuiltBasePublicationPlan,
    source_boundary: &'a crate::build_snapshot::SourceSnapshotBoundary,
    vacuum: bool,
}

fn build_persisted_and_publish_engine(
    request: PersistedBuildRequest<'_>,
    progress: &mut ProgressCallback<'_>,
    governor: &crate::resource::ResourceGovernor,
    memory_plan: BuildMemoryPlan,
) -> safety::GraphResult<engine::Engine> {
    let path = request.publication_plan.candidate_base_path();
    let result = (|| {
        report_progress(
            progress,
            "persisting",
            "scanning, writing, and fsyncing graph artifact",
        )?;
        let options = direct_build_options(
            request.graph_id,
            request.publication_plan.generation_id(),
            request.source_boundary,
            request.projection_mode,
            memory_plan,
        )?;
        let direct = crate::persisted_build_pipeline::build_persisted_candidate(
            request.tables,
            request.edges,
            request.filter_columns,
            path,
            governor,
            &options,
        )
        .map_err(|error| {
            contextualize_internal_candidate_error(
                request.operation,
                "direct persisted build",
                error,
            )
        })?;
        let manifest =
            crate::projection::recovery::prepare_generation_specific_rebuilt_base_manifest(
                request.publication_plan,
                request.source_boundary.sync_watermark(),
            )?;

        report_progress(
            progress,
            "validating_persistence",
            "validating persisted graph artifact",
        )?;
        let mut loaded = persistence::load_graph_file_with_projection_candidate_and_residency(
            path,
            manifest.manifest(),
            memory_plan.serving_bytes,
        )
        .map_err(|error| {
            contextualize_internal_candidate_error(
                request.operation,
                "persisted mmap reload",
                error,
            )
        })?;
        if loaded.node_store.node_count() != direct.node_count
            || loaded.edge_store.edge_count() != direct.edge_count
        {
            return Err(safety::GraphError::CorruptFile {
                reason: "direct persisted candidate counts changed during validation".into(),
            });
        }
        loaded.set_catalog_fingerprint(request.source_boundary.catalog_fingerprint());
        loaded.record_applied_sync_id(request.source_boundary.sync_watermark());
        loaded.set_projection_mode(request.projection_mode);
        loaded.build_resource_pressure_events = direct.pressure_events;
        loaded.finish_build(Some(pgrx::datetime::transaction_timestamp()));
        if request.vacuum {
            loaded.mark_vacuum_complete(Some(pgrx::datetime::transaction_timestamp()));
        }

        request.source_boundary.verify(request.operation)?;
        crate::projection::recovery::publish_prepared_generation_specific_rebuilt_base(
            request.publication_plan,
            &manifest,
        )
        .map_err(|err| {
            safety::GraphError::Internal(format!(
                "graph.{operation}(): projection manifest publication failed: {err}",
                operation = request.operation
            ))
        })?;

        let file_size = std::fs::metadata(path)
            .map(|metadata| metadata.len() as f64 / 1_048_576.0)
            .unwrap_or(0.0);
        pgrx::log!(
            "graph: persisted generation {} to {} ({:.1} MB)",
            request.publication_plan.generation_id(),
            path.display(),
            file_size
        );
        Ok(loaded)
    })();
    if result.is_err() {
        cleanup_failed_candidate(request.publication_plan);
    }
    result
}

fn contextualize_internal_candidate_error(
    operation: &str,
    stage: &str,
    error: safety::GraphError,
) -> safety::GraphError {
    match error {
        safety::GraphError::Internal(reason) => {
            safety::GraphError::Internal(format!("graph.{operation}(): {stage} failed: {reason}"))
        }
        typed => typed,
    }
}

fn cleanup_failed_candidate(
    publication_plan: &crate::projection::recovery::RebuiltBasePublicationPlan,
) {
    match publication_plan.candidate_is_current() {
        Ok(false) => remove_staged_base(publication_plan.candidate_base_path()),
        Ok(true) => warn_preserved_candidate(publication_plan.generation_id(), None),
        Err(error) => warn_preserved_candidate(publication_plan.generation_id(), Some(&error)),
    }
}

#[cfg(not(test))]
fn warn_preserved_candidate(generation_id: u64, error: Option<&safety::GraphError>) {
    match error {
        None => pgrx::warning!(
            "graph: preserving generation {} candidate after publication error because it is current",
            generation_id
        ),
        Some(error) => pgrx::warning!(
            "graph: preserving generation {} candidate after publication error because current generation could not be verified: {}",
            generation_id,
            error
        ),
    }
}

#[cfg(test)]
fn warn_preserved_candidate(_generation_id: u64, _error: Option<&safety::GraphError>) {}

fn direct_build_options(
    graph_id: &str,
    generation_id: u64,
    source_boundary: &crate::build_snapshot::SourceSnapshotBoundary,
    projection_mode: config::ProjectionMode,
    memory_plan: BuildMemoryPlan,
) -> safety::GraphResult<crate::persisted_build_pipeline::DirectBuildOptions> {
    const MERGE_FANOUT: usize = 4;
    const MAX_RUN_FILES: usize = 64;
    const MAX_RECORD_CEILING: u64 = 1024 * 1024;
    let divisor = u64::try_from((MERGE_FANOUT + 1) * 4)
        .map_err(|_| safety::GraphError::Internal("merge reservation divisor overflowed".into()))?;
    let max_record_bytes = usize::try_from(
        (memory_plan.replacement_budget_bytes.as_u64() / divisor).clamp(32, MAX_RECORD_CEILING),
    )
    .map_err(|_| safety::GraphError::Internal("maximum run record size overflowed".into()))?;
    Ok(crate::persisted_build_pipeline::DirectBuildOptions {
        graph_id: parse_graph_uuid(graph_id)?,
        build_id: direct_build_id(generation_id, source_boundary.catalog_fingerprint()),
        catalog_fingerprint: source_boundary.catalog_fingerprint(),
        batch_size: config::BUILD_BATCH_SIZE.get().max(1) as usize,
        run_target: memory_plan.batch_bytes,
        max_record_bytes,
        max_run_files: MAX_RUN_FILES,
        merge_fanout: MERGE_FANOUT,
        applied_sync_id: source_boundary.sync_watermark(),
        projection_mode,
    })
}

fn parse_graph_uuid(value: &str) -> safety::GraphResult<[u8; 16]> {
    if value.len() != 36
        || value
            .bytes()
            .enumerate()
            .any(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) != (byte == b'-'))
    {
        return Err(safety::GraphError::Internal(
            "selected graph ID is not a canonical UUID".into(),
        ));
    }
    let mut output = [0_u8; 16];
    let mut digits = value.bytes().filter(|byte| *byte != b'-');
    for byte in &mut output {
        let high = digits.next().and_then(hex_digit);
        let low = digits.next().and_then(hex_digit);
        *byte = match (high, low) {
            (Some(high), Some(low)) => (high << 4) | low,
            _ => {
                return Err(safety::GraphError::Internal(
                    "selected graph ID is not a canonical UUID".into(),
                ))
            }
        };
    }
    if digits.next().is_some() {
        return Err(safety::GraphError::Internal(
            "selected graph ID is not a canonical UUID".into(),
        ));
    }
    Ok(output)
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn direct_build_id(generation_id: u64, catalog_fingerprint: u64) -> [u8; 16] {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    let nonce = time ^ catalog_fingerprint ^ u64::from(std::process::id());
    let mut build_id = [0_u8; 16];
    build_id[..8].copy_from_slice(&generation_id.to_le_bytes());
    build_id[8..].copy_from_slice(&nonce.to_le_bytes());
    build_id
}

fn remove_staged_base(path: &std::path::Path) {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".tmp");
    for staged in [
        path.to_path_buf(),
        std::path::PathBuf::from(temporary),
        persistence::sync_checkpoint_path(path),
        persistence::projection_mode_path(path),
        append_path_suffix(&persistence::sync_checkpoint_path(path), ".tmp"),
        append_path_suffix(&persistence::projection_mode_path(path), ".tmp"),
    ] {
        match std::fs::remove_file(&staged) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => pgrx::warning!(
                "graph: could not remove failed staged artifact {}: {}",
                staged.display(),
                error
            ),
        }
    }
}

fn append_path_suffix(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

pub(crate) fn execute_build(force_persist: bool) -> safety::GraphResult<BuildExecutionResult> {
    let mut progress = |_, _| Ok(());
    let mode = configured_projection_mode()?;
    execute_build_with_mode_and_progress(force_persist, mode, &mut progress)
}

/// Build immediately after trusted schema discovery registered the mapping rows.
pub(crate) fn execute_build_after_discovery(
    force_persist: bool,
) -> safety::GraphResult<BuildExecutionResult> {
    let mode = configured_projection_mode()?;
    let mut progress = |_, _| Ok(());
    execute_build_inner(
        force_persist,
        mode,
        ProjectionModeGate::CheckGuc,
        CatalogLockGate::DiscoveryRegistration,
        "building",
        "building graph from discovered source tables",
        &mut progress,
    )
}

pub(crate) fn execute_build_with_mode(
    force_persist: bool,
    projection_mode: config::ProjectionMode,
) -> safety::GraphResult<BuildExecutionResult> {
    let mut progress = |_, _| Ok(());
    execute_build_with_mode_and_progress(force_persist, projection_mode, &mut progress)
}

pub(crate) fn execute_build_with_mode_and_progress(
    force_persist: bool,
    projection_mode: config::ProjectionMode,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<BuildExecutionResult> {
    execute_build_inner(
        force_persist,
        projection_mode,
        ProjectionModeGate::CheckGuc,
        CatalogLockGate::Caller,
        "building",
        "building graph from registered source tables",
        progress,
    )
}

pub(crate) fn execute_build_with_prevalidated_mode_and_progress(
    force_persist: bool,
    projection_mode: config::ProjectionMode,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<BuildExecutionResult> {
    execute_build_inner(
        force_persist,
        projection_mode,
        ProjectionModeGate::Prevalidated,
        CatalogLockGate::Caller,
        "building",
        "building graph from registered source tables",
        progress,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionModeGate {
    CheckGuc,
    Prevalidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogLockGate {
    Caller,
    DiscoveryRegistration,
}

fn execute_build_inner(
    force_persist: bool,
    projection_mode: config::ProjectionMode,
    mode_gate: ProjectionModeGate,
    catalog_lock_gate: CatalogLockGate,
    build_phase: &'static str,
    build_message: &'static str,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<BuildExecutionResult> {
    let start = std::time::Instant::now();
    let graph = selected_or_default_graph_metadata()?;
    let sync_mode = current_sync_mode()?;
    if mode_gate == ProjectionModeGate::CheckGuc {
        validate_projection_mode_enabled(projection_mode)?;
    }

    acquire_build_lock()?;
    match catalog_lock_gate {
        CatalogLockGate::Caller => crate::build_snapshot::lock_catalog()?,
        CatalogLockGate::DiscoveryRegistration => {
            crate::build_snapshot::lock_catalog_after_discovery()?
        }
    }
    let (tables, edges, filter_columns) = read_catalog()?;

    if tables.is_empty() {
        pgrx::warning!("graph.build(): no tables registered. Call graph.add_table() first.");
        return Ok(BuildExecutionResult {
            nodes_loaded: 0,
            edges_loaded: 0,
            build_time_ms: 0.0,
            memory_used_mb: 0.0,
            sync_mode: sync_mode.as_str().to_string(),
            projection_mode: projection_mode.as_str().to_string(),
        });
    }

    let source_boundary = crate::build_snapshot::SourceSnapshotBoundary::establish(
        &tables,
        &edges,
        &filter_columns,
        sync_mode,
        "build",
    )?;

    check_build_acls_result(&tables, &edges)?;
    let memory_plan = guard_build_memory_headroom(&tables, &edges)?;
    let should_persist = force_persist || config::PERSIST_ON_BUILD.get();
    let publication_plan = should_persist
        .then(|| {
            let path = persistence::graph_file_path()?;
            plan_rebuilt_base_publication(&persistence::projection_manifest_root(&path))
        })
        .transpose()?;

    report_progress(progress, build_phase, build_message)?;
    apply_build_memory_plan(memory_plan)?;
    let new_engine = with_build_resources(memory_plan, |governor| {
        let mut completed = if let Some(publication_plan) = publication_plan.as_ref() {
            build_persisted_and_publish_engine(
                PersistedBuildRequest {
                    operation: "build",
                    graph_id: &graph.graph_id,
                    tables: &tables,
                    edges: &edges,
                    filter_columns: &filter_columns,
                    projection_mode,
                    publication_plan,
                    source_boundary: &source_boundary,
                    vacuum: false,
                },
                progress,
                governor,
                memory_plan,
            )
        } else {
            let mut new_engine = build_replacement_engine(
                &tables,
                &edges,
                &filter_columns,
                governor,
                memory_plan.batch_bytes,
                &source_boundary,
            )?;
            let _engine_memory = reserve_built_engine_memory(governor, &new_engine)?;
            new_engine.set_projection_mode(projection_mode);
            source_boundary.verify("build")?;
            new_engine.finalize_resolution();
            Ok(new_engine)
        }?;
        completed.record_build_resource_stats(
            memory_plan.replacement_budget_bytes,
            replacement_memory_peak(governor, memory_plan),
            governor.memory_peak_phase(),
            completed.build_resource_pressure_events,
        );
        Ok(completed)
    })?;

    source_boundary.verify("build")?;

    let nodes_loaded = new_engine.node_store.node_count() as i64;
    let edges_loaded = new_engine.edge_store.edge_count() as i64;
    let build_time_ms = start.elapsed().as_secs_f64() * 1000.0;
    let memory_used_mb = new_engine.estimated_memory_used_mb();

    ENGINE.with(|e| {
        *e.borrow_mut() = new_engine;
    });
    crate::runtime_state::mark_loaded_graph(&graph);

    Ok(BuildExecutionResult {
        nodes_loaded,
        edges_loaded,
        build_time_ms,
        memory_used_mb,
        sync_mode: sync_mode.as_str().to_string(),
        projection_mode: projection_mode.as_str().to_string(),
    })
}

pub(crate) fn execute_maintenance_rebuild(
    force_persist: bool,
) -> safety::GraphResult<MaintenanceExecutionResult> {
    let mut progress = |_, _| Ok(());
    execute_maintenance_rebuild_with_progress(force_persist, &mut progress)
}

pub(crate) fn execute_maintenance_rebuild_with_progress(
    force_persist: bool,
    progress: &mut ProgressCallback<'_>,
) -> safety::GraphResult<MaintenanceExecutionResult> {
    let previous_applied_sync_id = ENGINE.with(|e| e.borrow().applied_sync_id);
    let build = execute_build_inner(
        force_persist,
        configured_projection_mode()?,
        ProjectionModeGate::CheckGuc,
        CatalogLockGate::Caller,
        "rebuilding",
        "rebuilding graph for maintenance",
        progress,
    )?;
    let after = max_sync_log_id()?;
    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.mark_vacuum_complete(Some(pgrx::datetime::transaction_timestamp()));
    });
    // Best-effort: a pruning failure should not fail an otherwise-successful
    // maintenance rebuild. sync_log_retention_floor's bootstrap safety rule
    // (no evidence => no prune) means a failed or skipped prune never risks
    // data loss, only leaves the log a little larger until the next pass.
    let _ = crate::sql_sync::prune_sync_log_if_safe();
    Ok(MaintenanceExecutionResult {
        sync_rows_applied: after.saturating_sub(previous_applied_sync_id),
        nodes_after: build.nodes_loaded,
        edges_after: build.edges_loaded,
        vacuum_time_ms: build.build_time_ms,
    })
}

pub(crate) fn execute_vacuum(force_persist: bool) -> safety::GraphResult<VacuumExecutionResult> {
    let start = std::time::Instant::now();
    let graph = selected_or_default_graph_metadata()?;
    acquire_build_lock()?;
    crate::build_snapshot::lock_catalog()?;
    let sync_mode = current_sync_mode()?;

    let (nodes_before, active_before, vacuum_projection_mode) = ENGINE.with(|e| {
        let eng = e.borrow();
        if !eng.built {
            return (0i64, 0i64, eng.projection_mode);
        }
        (
            eng.node_store.node_count() as i64,
            eng.node_store.active_count() as i64,
            eng.projection_mode,
        )
    });

    if nodes_before == 0 {
        return Ok(VacuumExecutionResult {
            nodes_before: 0,
            nodes_after: 0,
            tombstones_removed: 0,
            edges_rebuilt: 0,
            vacuum_time_ms: 0.0,
        });
    }

    let (tables, edges, filter_columns) = read_catalog()?;
    let source_boundary = crate::build_snapshot::SourceSnapshotBoundary::establish(
        &tables,
        &edges,
        &filter_columns,
        sync_mode,
        "vacuum",
    )?;
    check_build_acls_result(&tables, &edges)?;
    let memory_plan = guard_build_memory_headroom(&tables, &edges)?;
    apply_build_memory_plan(memory_plan)?;
    let should_persist = force_persist || config::PERSIST_ON_BUILD.get();
    let publication_plan = should_persist
        .then(|| {
            let path = persistence::graph_file_path()?;
            plan_rebuilt_base_publication(&persistence::projection_manifest_root(&path))
        })
        .transpose()?;

    let tombstones_removed = nodes_before - active_before;
    let new_engine = with_build_resources(memory_plan, |governor| {
        let mut completed = if let Some(publication_plan) = publication_plan.as_ref() {
            let mut progress = |_, _| Ok(());
            build_persisted_and_publish_engine(
                PersistedBuildRequest {
                    operation: "vacuum",
                    graph_id: &graph.graph_id,
                    tables: &tables,
                    edges: &edges,
                    filter_columns: &filter_columns,
                    projection_mode: vacuum_projection_mode,
                    publication_plan,
                    source_boundary: &source_boundary,
                    vacuum: true,
                },
                &mut progress,
                governor,
                memory_plan,
            )
        } else {
            let mut new_engine = build_replacement_engine(
                &tables,
                &edges,
                &filter_columns,
                governor,
                memory_plan.batch_bytes,
                &source_boundary,
            )?;
            let _engine_memory = reserve_built_engine_memory(governor, &new_engine)?;
            new_engine.set_projection_mode(vacuum_projection_mode);
            new_engine.mark_vacuum_complete(Some(pgrx::datetime::transaction_timestamp()));
            source_boundary.verify("vacuum")?;
            new_engine.finalize_resolution();
            Ok(new_engine)
        }?;
        completed.record_build_resource_stats(
            memory_plan.replacement_budget_bytes,
            replacement_memory_peak(governor, memory_plan),
            governor.memory_peak_phase(),
            completed.build_resource_pressure_events,
        );
        Ok(completed)
    })?;

    source_boundary.verify("vacuum")?;

    let nodes_after = new_engine.node_store.node_count() as i64;
    let edges_rebuilt = new_engine.edge_store.edge_count() as i64;

    ENGINE.with(|e| {
        *e.borrow_mut() = new_engine;
    });

    Ok(VacuumExecutionResult {
        nodes_before,
        nodes_after,
        tombstones_removed,
        edges_rebuilt,
        vacuum_time_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

pub(crate) fn acquire_build_lock() -> safety::GraphResult<()> {
    let graph = selected_or_default_graph_metadata()?;
    let acquired = Spi::get_one::<bool>(&build_lock_query_for_graph(&graph.graph_id))
        .map_err(|err| {
            safety::GraphError::Internal(format!(
                "could not acquire build/vacuum advisory lock: {}",
                err
            ))
        })?
        .unwrap_or(false);
    if acquired {
        Ok(())
    } else {
        Err(safety::GraphError::BuildLocked)
    }
}

fn build_replacement_engine(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
    filter_columns: &[builder::RegisteredFilterColumn],
    governor: &crate::resource::ResourceGovernor,
    batch_bytes: crate::resource::ByteCount,
    source_boundary: &crate::build_snapshot::SourceSnapshotBoundary,
) -> safety::GraphResult<engine::Engine> {
    let mut new_engine =
        builder::build_graph_with_governor(tables, edges, filter_columns, governor, batch_bytes)?;
    new_engine.set_catalog_fingerprint(source_boundary.catalog_fingerprint());
    new_engine.record_applied_sync_id(source_boundary.sync_watermark());
    Ok(new_engine)
}

fn with_build_resources<T>(
    memory_plan: BuildMemoryPlan,
    work: impl FnOnce(&crate::resource::ResourceGovernor) -> safety::GraphResult<T>,
) -> safety::GraphResult<T> {
    let governor = crate::resource::build_governor(crate::resource::MemoryBudget::new(
        memory_plan.limit_bytes,
    ));
    let _serving = governor
        .reserve_memory(
            crate::resource::ResourcePhase::Serving,
            memory_plan.serving_bytes,
        )
        .map_err(resource_memory_error_to_oom)?;
    let _safety = governor
        .reserve_memory(
            crate::resource::ResourcePhase::SafetyReserve,
            memory_plan.safety_reserve_bytes,
        )
        .map_err(resource_memory_error_to_oom)?;
    let result = work(&governor);
    pgrx::log!(
        "graph: build resource limit={} bytes replacement_budget={} bytes replacement_peak={} bytes peak_phase={} batch_target={} bytes",
        memory_plan.limit_bytes.as_u64(),
        memory_plan.replacement_budget_bytes.as_u64(),
        replacement_memory_peak(&governor, memory_plan).as_u64(),
        governor
            .memory_peak_phase()
            .map(crate::resource::ResourcePhase::as_str)
            .unwrap_or("none"),
        memory_plan.batch_bytes.as_u64()
    );
    result
}

fn replacement_memory_peak(
    governor: &crate::resource::ResourceGovernor,
    memory_plan: BuildMemoryPlan,
) -> crate::resource::ByteCount {
    governor
        .memory_peak()
        .checked_sub(memory_plan.serving_bytes)
        .and_then(|peak| peak.checked_sub(memory_plan.safety_reserve_bytes))
        .unwrap_or(crate::resource::ByteCount::ZERO)
}

fn reserve_built_engine_memory<'a>(
    governor: &'a crate::resource::ResourceGovernor,
    engine: &engine::Engine,
) -> safety::GraphResult<crate::resource::ResourceLease<'a>> {
    let bytes = u64::try_from(engine.estimated_memory_used_bytes()).map_err(|_| {
        safety::GraphError::Internal("built engine memory does not fit u64".to_string())
    })?;
    governor
        .reserve_memory(
            crate::resource::ResourcePhase::Replacement,
            crate::resource::ByteCount::from_bytes(bytes),
        )
        .map_err(resource_memory_error_to_oom)
}

pub(crate) fn configured_projection_mode() -> safety::GraphResult<config::ProjectionMode> {
    config::default_projection_mode().ok_or_else(|| safety::GraphError::InvalidFilter {
        reason: format!(
            "unsupported graph.default_projection_mode '{}'; expected 'csr_readonly' or 'mutable_overlay'",
            config::DEFAULT_PROJECTION_MODE
                .get()
                .as_ref()
                .and_then(|c| c.to_str().ok())
                .unwrap_or("csr_readonly")
        ),
    })
}

pub(crate) fn validate_projection_mode_enabled(
    projection_mode: config::ProjectionMode,
) -> safety::GraphResult<()> {
    if matches!(projection_mode, config::ProjectionMode::MutableOverlay)
        && !config::MUTABLE_ENABLED.get()
    {
        return Err(safety::GraphError::UnsupportedOperation {
            operation: "graph.build(mode => 'mutable_overlay')".to_string(),
            reason: "graph.mutable_enabled is off".to_string(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuildMemoryPlan {
    pub(crate) unload_existing: bool,
    limit_bytes: crate::resource::ByteCount,
    serving_bytes: crate::resource::ByteCount,
    safety_reserve_bytes: crate::resource::ByteCount,
    replacement_budget_bytes: crate::resource::ByteCount,
    batch_bytes: crate::resource::ByteCount,
}

pub(crate) fn guard_build_memory_headroom(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
) -> safety::GraphResult<BuildMemoryPlan> {
    let estimate = builder::estimate_graph_memory(tables, edges)?;
    let existing_bytes = ENGINE.with(|e| {
        let eng = e.borrow();
        if eng.built {
            u64::try_from(eng.estimated_memory_used_bytes()).map_err(|_| {
                safety::GraphError::Internal(
                    "existing engine memory estimate does not fit u64".to_string(),
                )
            })
        } else {
            Ok(0)
        }
    })?;
    let existing_bytes = crate::resource::ByteCount::from_bytes(existing_bytes);
    let build_peak_bytes = conservative_build_peak_bytes(estimate.bytes)?;
    let limit_mb = config::MEMORY_LIMIT_MB.get().max(1) as u64;
    let limit_bytes = crate::resource::ByteCount::from_mib(limit_mb).ok_or_else(|| {
        safety::GraphError::Internal("configured memory limit overflowed u64 bytes".to_string())
    })?;
    let low_memory_build = config::LOW_MEMORY_BUILD.get();
    let safety_reserve_bytes = build_safety_reserve_bytes(limit_bytes)?;
    if low_memory_build
        && existing_bytes > crate::resource::ByteCount::ZERO
        && build_peak_bytes
            .checked_add(safety_reserve_bytes)
            .is_some_and(|required| required <= limit_bytes)
    {
        pgrx::warning!(
            "graph: low-memory build unloading current backend graph before rebuild ({:.0} MB existing, {:.0} MB peak replacement, limit {:.0} MB).",
            existing_bytes.as_mib_f64(),
            build_peak_bytes.as_mib_f64(),
            limit_bytes.as_mib_f64()
        );
        let replacement_budget_bytes = effective_build_bytes(
            limit_bytes,
            crate::resource::ByteCount::ZERO,
            safety_reserve_bytes,
        )?;
        return Ok(BuildMemoryPlan {
            unload_existing: true,
            limit_bytes,
            serving_bytes: crate::resource::ByteCount::ZERO,
            safety_reserve_bytes,
            replacement_budget_bytes,
            batch_bytes: crate::resource::adaptive_build_batch_target(replacement_budget_bytes),
        });
    }

    let governor =
        crate::resource::ResourceGovernor::new(crate::resource::ResourceLimits::memory_only(
            crate::resource::MemoryBudget::new(limit_bytes),
        ));
    let _serving = governor
        .reserve_memory(crate::resource::ResourcePhase::Serving, existing_bytes)
        .map_err(resource_memory_error_to_oom)?;
    let _safety = governor
        .reserve_memory(
            crate::resource::ResourcePhase::SafetyReserve,
            safety_reserve_bytes,
        )
        .map_err(resource_memory_error_to_oom)?;
    let mut build = governor
        .reserve_memory(
            crate::resource::ResourcePhase::Replacement,
            crate::resource::ByteCount::ZERO,
        )
        .map_err(resource_memory_error_to_oom)?;
    if let Err(error) = build.try_grow(build_peak_bytes) {
        if matches!(config::oom_action(), config::OomAction::ReadOnly) {
            pgrx::warning!(
                    "graph.oom_action = 'readonly' is retained as a deprecated compatibility value; over-budget graph construction is rejected before allocation"
                );
        }
        return Err(resource_memory_error_to_oom(error));
    }
    debug_assert_eq!(build.amount(), build_peak_bytes);
    debug_assert_eq!(
        governor.memory_peak_phase(),
        Some(crate::resource::ResourcePhase::Replacement)
    );
    debug_assert_eq!(
        governor.memory_peak(),
        existing_bytes
            .checked_add(safety_reserve_bytes)
            .and_then(|bytes| bytes.checked_add(build_peak_bytes))
            .unwrap_or(limit_bytes)
    );
    governor
        .check_elapsed(crate::resource::ResourcePhase::Replacement)
        .map_err(|error| {
            safety::GraphError::Internal(format!("build resource preflight failed: {error}"))
        })?;

    let replacement_budget_bytes =
        effective_build_bytes(limit_bytes, existing_bytes, safety_reserve_bytes)?;
    Ok(BuildMemoryPlan {
        unload_existing: false,
        limit_bytes,
        serving_bytes: existing_bytes,
        safety_reserve_bytes,
        replacement_budget_bytes,
        batch_bytes: crate::resource::adaptive_build_batch_target(replacement_budget_bytes),
    })
}

fn effective_build_bytes(
    limit: crate::resource::ByteCount,
    serving: crate::resource::ByteCount,
    safety_reserve: crate::resource::ByteCount,
) -> safety::GraphResult<crate::resource::ByteCount> {
    limit
        .checked_sub(serving)
        .and_then(|remaining| remaining.checked_sub(safety_reserve))
        .ok_or_else(|| safety::GraphError::Oom {
            used_mb: serving.ceil_mib(),
            need_mb: safety_reserve.ceil_mib(),
            limit_mb: limit.as_u64() / 1_048_576,
        })
}

fn build_safety_reserve_bytes(
    limit: crate::resource::ByteCount,
) -> safety::GraphResult<crate::resource::ByteCount> {
    let maximum = crate::resource::ByteCount::from_mib(64).ok_or_else(|| {
        safety::GraphError::Internal("build safety reserve overflowed u64".to_string())
    })?;
    Ok(crate::resource::ByteCount::from_bytes(
        (limit.as_u64() / 16).min(maximum.as_u64()),
    ))
}

fn apply_build_memory_plan(plan: BuildMemoryPlan) -> safety::GraphResult<()> {
    if plan.unload_existing {
        ENGINE.with(|e| {
            *e.borrow_mut() = engine::Engine::new();
        });
        crate::runtime_state::clear_loaded_graph();
    }
    Ok(())
}

fn conservative_build_peak_bytes(
    final_graph_bytes: crate::resource::ByteCount,
) -> safety::GraphResult<crate::resource::ByteCount> {
    let scaled = final_graph_bytes
        .checked_mul(5)
        .and_then(|bytes| bytes.checked_add(crate::resource::ByteCount::from_bytes(3)))
        .map(|bytes| crate::resource::ByteCount::from_bytes(bytes.as_u64() / 4))
        .ok_or_else(|| {
            safety::GraphError::Internal("build peak estimate overflowed u64".to_string())
        })?;
    let minimum_overhead = crate::resource::ByteCount::from_mib(32).ok_or_else(|| {
        safety::GraphError::Internal("build overhead constant overflowed u64".to_string())
    })?;
    let with_overhead = final_graph_bytes
        .checked_add(minimum_overhead)
        .ok_or_else(|| {
            safety::GraphError::Internal("build peak estimate overflowed u64".to_string())
        })?;
    Ok(scaled.max(with_overhead))
}

fn resource_memory_error_to_oom(error: crate::resource::ResourceLimitError) -> safety::GraphError {
    debug_assert_eq!(error.kind(), crate::resource::ResourceKind::Memory);
    safety::GraphError::Oom {
        used_mb: crate::resource::ByteCount::from_bytes(error.used()).ceil_mib(),
        need_mb: crate::resource::ByteCount::from_bytes(error.requested()).ceil_mib(),
        limit_mb: crate::resource::ByteCount::from_bytes(error.limit()).as_u64() / 1_048_576,
    }
}

pub(crate) fn check_build_acls_result(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
) -> safety::GraphResult<()> {
    for table in tables {
        let oid = table.table_oid;
        acl::check_table_acl(oid)?;
    }
    for edge in edges {
        let from_oid = edge.from_table_oid;
        let to_oid = edge.to_table_oid;
        acl::check_table_acl(from_oid)?;
        acl::check_table_acl(to_oid)?;
    }
    check_build_rls_boundary(tables, edges)?;
    Ok(())
}

/// Refuses to build over a table with row-level security enabled unless
/// `graph.allow_rls_tables = on`. Topology-read functions return
/// coordinates and adjacency from the builder-scoped graph artifact under
/// table-level ACL only; they do not evaluate row-level security per
/// calling role. See
/// `docs/user_guide/administration-and-security.mdx#row-level-security-and-topology-reads`.
fn check_build_rls_boundary(
    tables: &[builder::RegisteredTable],
    edges: &[builder::RegisteredEdge],
) -> safety::GraphResult<()> {
    if crate::config::ALLOW_RLS_TABLES.get() {
        return Ok(());
    }
    for table in tables {
        if acl::table_has_row_security(table.table_oid)? {
            return Err(safety::GraphError::RlsTopologyBoundary {
                table: table.table_name.clone(),
            });
        }
    }
    for edge in edges {
        if acl::table_has_row_security(edge.from_table_oid)? {
            return Err(safety::GraphError::RlsTopologyBoundary {
                table: edge.from_table.clone(),
            });
        }
        if acl::table_has_row_security(edge.to_table_oid)? {
            return Err(safety::GraphError::RlsTopologyBoundary {
                table: edge.to_table.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_lock_query_for_graph, cleanup_failed_candidate, conservative_build_peak_bytes,
        contextualize_internal_candidate_error, parse_graph_uuid, validate_projection_mode_enabled,
        BUILD_LOCK_CLASS_ID,
    };
    use crate::config::ProjectionMode;
    use crate::projection::manifest::{ProjectionManifest, ProjectionManifestStore};
    use crate::projection::recovery::plan_generation_specific_rebuilt_base;
    use crate::resource::ByteCount;
    use std::fs;

    #[test]
    fn build_lock_query_uses_named_advisory_lock_class() {
        assert_eq!(BUILD_LOCK_CLASS_ID, 1_918_928_211);
        assert!(
            build_lock_query_for_graph("00000000-0000-0000-0000-000000000001")
                .starts_with("SELECT pg_try_advisory_xact_lock(1918928211, ")
        );
    }

    #[test]
    fn graph_build_lock_query_is_stable_per_graph() {
        let graph_a = "00000000-0000-0000-0000-000000000001";
        let graph_b = "00000000-0000-0000-0000-000000000002";

        assert_eq!(
            build_lock_query_for_graph(graph_a),
            build_lock_query_for_graph(graph_a)
        );
        assert_ne!(
            build_lock_query_for_graph(graph_a),
            build_lock_query_for_graph(graph_b)
        );
    }

    #[test]
    fn direct_build_graph_id_requires_canonical_uuid_text() {
        assert_eq!(
            parse_graph_uuid("00112233-4455-6677-8899-aabbccddeeff")
                .expect("canonical UUID parses"),
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ]
        );
        assert!(parse_graph_uuid("00112233445566778899aabbccddeeff").is_err());
        assert!(parse_graph_uuid("00112233-4455-6677-8899-aabbccddeezz").is_err());
    }

    #[test]
    fn failed_publication_cleanup_preserves_candidate_that_became_current() {
        let root = std::env::temp_dir().join(format!(
            "pggraph-build-cleanup-current-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("test root creates");
        let plan = plan_generation_specific_rebuilt_base(&root).expect("publication plans");
        fs::write(plan.candidate_base_path(), b"candidate").expect("candidate writes");
        let manifest = ProjectionManifest::base_only(
            plan.generation_id(),
            plan.candidate_base_name(),
            "crc32:00000000",
            crate::persistence::graph_artifact_version(),
            0,
            1,
        );
        ProjectionManifestStore::new(&root)
            .publish_if_current(&manifest, plan.expected_current_generation())
            .expect("current pointer publishes");

        cleanup_failed_candidate(&plan);

        assert!(plan.candidate_base_path().is_file());
        fs::remove_dir_all(&root).expect("test root removes");
    }

    #[test]
    fn persisted_candidate_context_preserves_typed_resource_errors() {
        let error = crate::safety::GraphError::Oom {
            used_mb: 7,
            need_mb: 2,
            limit_mb: 8,
        };
        assert!(matches!(
            contextualize_internal_candidate_error("build", "persisted mmap reload", error),
            crate::safety::GraphError::Oom {
                used_mb: 7,
                need_mb: 2,
                limit_mb: 8
            }
        ));
    }

    #[test]
    fn csr_readonly_projection_mode_is_always_allowed() {
        validate_projection_mode_enabled(ProjectionMode::CsrReadonly)
            .expect("csr_readonly should be allowed");
    }

    #[test]
    fn build_peak_estimate_keeps_minimum_rebuild_overhead() {
        let input = ByteCount::from_mib(100).expect("test input should fit");
        let expected = ByteCount::from_mib(132).expect("test expectation should fit");
        let actual = conservative_build_peak_bytes(input).expect("peak estimate should fit");
        assert_eq!(actual, expected);
    }

    #[test]
    fn build_peak_estimate_scales_large_graphs() {
        let input = ByteCount::from_mib(1024).expect("test input should fit");
        let expected = ByteCount::from_mib(1280).expect("test expectation should fit");
        let actual = conservative_build_peak_bytes(input).expect("peak estimate should fit");
        assert_eq!(actual, expected);
    }
}
