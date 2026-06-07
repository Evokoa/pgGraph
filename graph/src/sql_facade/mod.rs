//! SQL-facing PostgreSQL function facade.
//!
//! This module groups the `#[pg_extern]` wrappers by user-facing area while
//! keeping shared SQL helper imports local to the facade boundary.

pub(crate) use crate::api_types::{
    BuildJobRow, ComponentNodeRow, MaintenanceJobRow, TraverseRequest,
};
pub(crate) use crate::catalog::{
    catalog_fingerprint, current_catalog_state, insert_registered_edge, insert_registered_table,
    read_catalog, regclass_text, split_catalog_columns, validate_column_exists,
    validate_edge_endpoint_columns, validate_filter_column_type, validate_registered_table,
    RegisteredEdgeInsert,
};
pub(crate) use crate::engine::Engine;
pub(crate) use crate::sql_aggregation::{aggregate_impl, path_count_estimate_impl};
pub(crate) use crate::sql_build::{
    configured_projection_mode, execute_build, execute_build_with_mode,
    execute_maintenance_rebuild, execute_vacuum,
};
pub(crate) use crate::sql_filters::filter_helper;
pub(crate) use crate::sql_hydration::{hydrate_node, hydrate_nodes};
pub(crate) use crate::sql_jobs::{
    build_job_row, create_build_job, create_maintenance_job, launch_build_worker,
    launch_maintenance_worker, maintenance_job_row, run_build_job, run_maintenance_job,
    update_build_job_failed, update_maintenance_job_failed, JobStatus, WorkerMetadata,
};
pub(crate) use crate::sql_search::{source_table_search_rows, validate_search_request};
pub(crate) use crate::sql_sync::{
    apply_sync_internal, apply_sync_until, current_sync_mode, disabled_graph_trigger_count,
    ingest_projection_internal, install_sync_triggers, max_sync_log_id, pending_sync_rows,
    resolve_tenant_scope,
};
pub(crate) use crate::sql_traversal::{
    apply_traversal_uniqueness, canonical_node_ref_string, execute_traverse_candidates,
    execute_traverse_rows, format_path_value, paginate_and_format_traverse_candidates,
    sort_traverse_candidates_for_many, usize_from_nonnegative,
};
pub(crate) use crate::{
    acl, builder, catalog, config, connected_components, discover, engine, persistence, safety,
    types, ENGINE,
};
pub(crate) use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};
pub(crate) use pgrx::prelude::*;
pub(crate) use pgrx::Spi;
pub(crate) use std::collections::HashMap;
pub(crate) use std::time::Duration;

mod admin;
mod components;
mod cypher;
mod discovery;
mod gql;
mod runtime;
mod search;
mod traversal;
mod workflow;

pub(crate) use admin::check_enabled_result;
#[cfg(feature = "pg_test")]
pub(crate) use runtime::ensure_current_graph;
