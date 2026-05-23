//! # graph — Sub-millisecond graph traversal for PostgreSQL
//!
//! `graph` is a PostgreSQL extension written in Rust (via pgrx) that lets you
//! query your existing relational tables as a graph. No external services.
//! No ETL pipelines. No separate graph database. No new query language.
//!
//! See: `docs/user_guide/index.mdx` and `docs/contributor_guide/index.mdx`

#![cfg_attr(
    not(any(test, feature = "pg_test")),
    deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)
)]

use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};
use pgrx::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

// Module declarations ordered by dependency layer.
mod acl;
mod api_types;
mod bfs;
mod builder;
mod catalog;
mod config;
mod connected_components;
mod discover;
mod edge_store;
mod engine;
mod filter_index;
mod node_store;
mod path_finder;
mod persistence;
mod quote;
mod resolution_index;
mod safety;
mod sql_aggregation;
mod sql_build;
mod sql_filters;
mod sql_hydration;
mod sql_jobs;
mod sql_search;
mod sql_sync;
mod sql_traversal;
mod sync;
mod types;

use api_types::{BuildJobRow, ComponentNodeRow, MaintenanceJobRow, TraverseRequest};
use catalog::{
    catalog_fingerprint, current_catalog_state, insert_registered_edge, insert_registered_table,
    read_catalog, regclass_text, split_catalog_columns, validate_column_exists,
    validate_filter_column_type, validate_registered_table, RegisteredEdgeInsert,
};
use engine::Engine;
use sql_aggregation::{aggregate_impl, path_count_estimate_impl};
use sql_build::{execute_build, execute_maintenance_rebuild, execute_vacuum};
use sql_filters::filter_helper;
use sql_hydration::{hydrate_node, hydrate_nodes};
use sql_jobs::{
    build_job_row, create_build_job, create_maintenance_job, launch_build_worker,
    launch_maintenance_worker, maintenance_job_row, run_build_job, run_maintenance_job,
    update_build_job_failed, update_maintenance_job_failed, JobStatus,
};
use sql_search::{source_table_search_rows, validate_search_request};
use sql_sync::{
    apply_sync_internal, apply_sync_until, current_sync_mode, disabled_graph_trigger_count,
    install_sync_triggers, max_sync_log_id, pending_sync_rows, resolve_tenant_scope,
};
use sql_traversal::{
    canonical_node_ref_string, execute_traverse_candidates, execute_traverse_rows,
    format_path_value, paginate_and_format_traverse_candidates, sort_traverse_candidates_for_many,
    usize_from_nonnegative,
};

#[cfg(feature = "pg_test")]
use api_types::{BuildExecutionResult, MaintenanceExecutionResult};
#[cfg(feature = "pg_test")]
use catalog::validate_numeric_column;
#[cfg(feature = "pg_test")]
use quote::quote_literal as sql_literal;
#[cfg(any(test, feature = "fuzzing"))]
use sql_filters::validate_structured_operator_shape;
#[cfg(feature = "pg_test")]
use sql_jobs::{
    update_build_job_completed, update_build_job_started, update_maintenance_job_completed,
    update_maintenance_job_started,
};
#[cfg(any(test, feature = "fuzzing"))]
use sql_sync::parse_sync_properties;
#[cfg(any(test, feature = "fuzzing"))]
use sql_traversal::parse_node_ref_json_parts;
#[cfg(any(test, feature = "fuzzing", feature = "pg_test"))]
use sql_traversal::validate_traverse_options;

/// Helpers exported only for fuzz targets and unit tests.
///
/// These wrappers expose parser and persistence boundaries that can run without
/// requiring a live PostgreSQL backend.
#[cfg(any(test, feature = "fuzzing"))]
pub mod fuzz_support {
    pub use crate::filter_index::FilterIndex;
    pub use crate::persistence::load_graph_file;

    /// Parse sync JSON properties through the same lossy boundary used by SQL
    /// sync replay. Intended for fuzz targets.
    pub fn parse_sync_properties(raw: Option<&str>) -> Vec<(String, String)> {
        crate::parse_sync_properties(raw)
    }

    /// Validate structured-filter operator shape without touching catalog
    /// state. Intended for fuzz targets.
    pub fn validate_structured_operator_shape(operator: &str, value: &serde_json::Value) -> bool {
        crate::validate_structured_operator_shape("fuzz_column", operator, value).is_ok()
    }

    /// Validate traversal direction, strategy, and uniqueness parsing without
    /// requiring a PostgreSQL backend. Intended for fuzz targets.
    pub fn validate_traverse_options(direction: &str, strategy: &str, uniqueness: &str) -> bool {
        crate::validate_traverse_options(direction, None, strategy, uniqueness).is_ok()
    }

    /// Parse a `graph.node_ref_string()` payload without resolving the table
    /// through Postgres. Intended for fuzz targets.
    pub fn parse_node_ref_json_parts(value: &serde_json::Value) -> bool {
        crate::parse_node_ref_json_parts(value).is_ok()
    }
}

/// Public re-exports for criterion benchmarks.
///
/// Benchmarks link against the `rlib` and need access to internal
/// data structures. This module is always available (bench targets
/// compile with `--lib`) but not part of the pgrx extension surface.
pub mod bench_support {
    pub use crate::bfs::{execute as bfs_execute, BfsConfig, BfsResult};
    pub use crate::edge_store::{EdgeStore as EdgeStoreBuilder, RawEdge};
    pub use crate::filter_index::{FilterColumnType, FilterIndex as FilterIndexBuilder};
    pub use crate::node_store::NodeStore as NodeStoreBuilder;
    pub use crate::types::{EdgeTypeFilter, FilterOp};
}

::pgrx::pg_module_magic!(name, version);
::pgrx::extension_sql_file!(
    "../sql/bootstrap.sql",
    name = "graph_bootstrap_sql",
    requires = [auto_discover]
);

// Declare the 'graph' schema so pgrx can satisfy control-file schema checks.
#[pg_schema]
mod graph {}

// Thread-local engine instance (one per Postgres backend process)
thread_local! {
    static ENGINE: RefCell<Engine> = RefCell::new(Engine::new());
}

// ─────────────────────────────────────────────────────────────────────
// Extension lifecycle
// ─────────────────────────────────────────────────────────────────────

/// Called when the extension is loaded into a backend.
///
/// Registers GUC parameters and eagerly pre-warms the OS page cache for the
/// `.pggraph` file (if it exists). This does NOT load the graph into the engine —
/// that happens lazily on the first query via `maybe_auto_load()`. What this
/// does is call `madvise(MADV_WILLNEED)` to tell the kernel to prefetch the
/// file pages into RAM, so the subsequent mmap in `load_graph_file()` won't
/// block on disk I/O.
///
/// For best results, add to `postgresql.conf`:
/// ```text
/// shared_preload_libraries = 'graph'
/// ```
/// This runs `_PG_init()` at postmaster startup, giving later backend
/// processes a warm page-cache path when the kernel keeps those pages resident.
#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    config::register_gucs();

    // Eagerly pre-warm the OS page cache for the .pggraph file.
    let Ok(path) = persistence::graph_file_path() else {
        return;
    };
    if path.exists() {
        match std::fs::File::open(&path) {
            Ok(file) => {
                // SAFETY: The file descriptor stays alive for the duration of
                // this temporary mapping, and the mapping is only used for
                // read-only page-cache advice.
                if let Ok(mmap) = unsafe { memmap2::Mmap::map(&file) } {
                    // madvise(MADV_WILLNEED) — ask the kernel to page in the
                    // entire file. This is non-blocking: the kernel will
                    // asynchronously read pages from disk into the page cache.
                    #[cfg(unix)]
                    {
                        mmap.advise(memmap2::Advice::WillNeed).ok();
                    }
                    pgrx::log!(
                        "graph: pre-warmed page cache for {} ({:.1} MB)",
                        path.display(),
                        mmap.len() as f64 / 1_048_576.0
                    );
                    // mmap is dropped here — that's fine. The kernel keeps the
                    // pages in the page cache regardless.
                }
            }
            Err(_) => {
                // Not critical — auto-load will handle it later
            }
        }
    }

    pgrx::log!("graph: extension loaded (v{})", env!("CARGO_PKG_VERSION"));
}

// ─────────────────────────────────────────────────────────────────────
// SQL Functions — graph schema
// ─────────────────────────────────────────────────────────────────────

include!("sql_facade/admin.rs");
include!("sql_facade/discovery.rs");
include!("sql_facade/traversal.rs");
include!("sql_facade/search.rs");
include!("sql_facade/workflow.rs");
include!("sql_facade/components.rs");
include!("sql_facade/runtime.rs");

// ─────────────────────────────────────────────────────────────────────
// Test module
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod pg_test;

/// Covers SQL API behavior through PostgreSQL, including registration,
/// discovery, build, search, traversal, path, component, and sync flows.
#[cfg(feature = "pg_test")]
#[pg_schema]
mod tests {
    include!("pg_tests/common.rs");
    include!("pg_tests/discovery.rs");
    include!("pg_tests/traversal_paths.rs");
    include!("pg_tests/filters.rs");
    include!("pg_tests/traversal_api.rs");
    include!("pg_tests/sync_config_build.rs");
    include!("pg_tests/registration_search.rs");
    include!("pg_tests/components_jobs.rs");
    include!("pg_tests/maintenance_admin.rs");
    include!("pg_tests/workflow_search_api.rs");
    include!("pg_tests/workflow_relationship_api.rs");
    include!("pg_tests/workflow_validation.rs");
    include!("pg_tests/synthetic_release.rs");
}
