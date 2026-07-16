//! Production composition for bounded direct persisted builds.

use std::path::Path;

use crate::build_runs::RunWorkspace;
use crate::builder::{RegisteredEdge, RegisteredFilterColumn, RegisteredTable};
use crate::config::ProjectionMode;
use crate::persisted_build::{write_semantic_artifact, SemanticDirectBuild};
use crate::persisted_build_scanner::scan_persisted_sources;
use crate::persisted_edge_scanner::scan_persisted_edges;
use crate::resource::{ByteCount, ResourceGovernor};
use crate::safety::{GraphError, GraphResult};

/// Resource and identity parameters for one direct persisted build.
pub(crate) struct DirectBuildOptions {
    pub(crate) graph_id: [u8; 16],
    pub(crate) build_id: [u8; 16],
    pub(crate) catalog_fingerprint: u64,
    pub(crate) batch_size: usize,
    pub(crate) run_target: ByteCount,
    pub(crate) max_record_bytes: usize,
    pub(crate) max_run_files: usize,
    pub(crate) merge_fanout: usize,
    pub(crate) applied_sync_id: i64,
    pub(crate) projection_mode: ProjectionMode,
}

/// Scan authoritative PostgreSQL sources and stream a v6 candidate without an
/// intermediate owned graph engine.
pub(crate) fn build_persisted_candidate(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    filters: &[RegisteredFilterColumn],
    path: &Path,
    governor: &ResourceGovernor,
    options: &DirectBuildOptions,
) -> GraphResult<DirectBuildResult> {
    if options.max_run_files < 4 || options.max_record_bytes < 32 {
        return Err(GraphError::Internal(
            "direct persisted build run limits are too small".into(),
        ));
    }
    let root = path.parent().ok_or_else(|| {
        GraphError::Internal("direct persisted candidate has no parent directory".into())
    })?;
    RunWorkspace::cleanup_abandoned_while_build_locked(root, options.build_id)?;
    let workspace = RunWorkspace::create(
        root,
        options.graph_id,
        options.build_id,
        options.catalog_fingerprint,
        options.max_run_files,
    )?;
    let sources = scan_persisted_sources(
        tables,
        edges,
        filters,
        &workspace,
        governor,
        options.batch_size,
        options.run_target,
        options.max_record_bytes,
        options.merge_fanout,
    )?;
    // Keep the lookup guard alive through endpoint resolution. Its disk lease
    // accounts for the PostgreSQL temporary relation until the final edge row
    // has been staged into governed runs.
    let _node_lookup = &sources.node_lookup;
    let edge_runs = scan_persisted_edges(
        tables,
        edges,
        &workspace,
        governor,
        options.batch_size,
        options.run_target,
        options.max_record_bytes,
        options.merge_fanout,
    )?;
    let semantic = SemanticDirectBuild {
        node_count: sources.node_count,
        forward_edge_count: edge_runs.forward_edge_count,
        inbound_edge_count: edge_runs.inbound_edge_count,
        has_forward_weights: sources.has_forward_weights,
        has_inbound_weights: sources.has_forward_weights,
        has_unidirectional_edges: sources.has_unidirectional_edges,
        applied_sync_id: options.applied_sync_id,
        projection_mode: options.projection_mode,
        nodes: sources.nodes.as_ref(),
        forward_edges: edge_runs.forward.as_ref(),
        inbound_edges: edge_runs.inbound.as_ref(),
        resolution: sources.resolution.as_ref(),
        filters: sources.filters.as_ref(),
        filter_dictionary: sources.filter_dictionary.as_ref(),
        relationship_identities: edge_runs.identities.as_ref(),
        tenant_tokens: sources.tenant_tokens.as_ref(),
        tenant_dictionary: sources.tenant_dictionary.as_ref(),
        filter_columns: filters,
        edge_type_registry: &edge_runs.edge_type_registry,
        tenanted_table_oids: &sources.tenanted_table_oids,
        max_record_bytes: options.max_record_bytes,
        governor,
    };
    write_semantic_artifact(path, &semantic)?;
    Ok(DirectBuildResult {
        node_count: sources.node_count,
        edge_count: edge_runs.forward_edge_count,
        pressure_events: sources.pressure_events,
    })
}
/// Counts validated against the loaded candidate before manifest publication.
pub(crate) struct DirectBuildResult {
    pub(crate) node_count: u32,
    pub(crate) edge_count: u32,
    pub(crate) pressure_events: u64,
}
