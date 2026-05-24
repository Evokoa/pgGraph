//! Shared SQL-facing row, request, and result types.

use crate::types::{PathCoordinate, TraversalDirection, TraversalStrategy};
use pgrx::prelude::*;

pub(crate) type TraverseRow = (
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
);

#[derive(Debug, Clone)]
pub(crate) struct BuildExecutionResult {
    pub(crate) nodes_loaded: i64,
    pub(crate) edges_loaded: i64,
    pub(crate) build_time_ms: f64,
    pub(crate) memory_used_mb: f64,
    pub(crate) sync_mode: String,
}

#[derive(Debug, Clone)]
pub(crate) struct BuildJobRow {
    pub(crate) build_id: String,
    pub(crate) status: String,
    pub(crate) nodes_loaded: Option<i64>,
    pub(crate) edges_loaded: Option<i64>,
    pub(crate) build_time_ms: Option<f64>,
    pub(crate) memory_used_mb: Option<f64>,
    pub(crate) sync_mode: String,
    pub(crate) progress_phase: String,
    pub(crate) progress_message: Option<String>,
    pub(crate) started_at: Option<TimestampWithTimeZone>,
    pub(crate) finished_at: Option<TimestampWithTimeZone>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MaintenanceExecutionResult {
    pub(crate) sync_rows_applied: i64,
    pub(crate) nodes_after: i64,
    pub(crate) edges_after: i64,
    pub(crate) vacuum_time_ms: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct MaintenanceJobRow {
    pub(crate) job_id: String,
    pub(crate) status: String,
    pub(crate) sync_rows_applied: Option<i64>,
    pub(crate) nodes_after: Option<i64>,
    pub(crate) edges_after: Option<i64>,
    pub(crate) vacuum_time_ms: Option<f64>,
    pub(crate) progress_phase: String,
    pub(crate) progress_message: Option<String>,
    pub(crate) started_at: Option<TimestampWithTimeZone>,
    pub(crate) finished_at: Option<TimestampWithTimeZone>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct VacuumExecutionResult {
    pub(crate) nodes_before: i64,
    pub(crate) nodes_after: i64,
    pub(crate) tombstones_removed: i64,
    pub(crate) edges_rebuilt: i64,
    pub(crate) vacuum_time_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TraverseRequest<'a> {
    pub(crate) root_table: pgrx::pg_sys::Oid,
    pub(crate) root_id: &'a str,
    pub(crate) max_depth: i32,
    pub(crate) edge_types: Option<&'a [String]>,
    pub(crate) node_tables: Option<&'a [pgrx::pg_sys::Oid]>,
    pub(crate) filter: Option<&'a pgrx::JsonB>,
    pub(crate) tenant: Option<&'a str>,
    pub(crate) direction: TraversalDirection,
    pub(crate) strategy: TraversalStrategy,
    pub(crate) include_start: bool,
    pub(crate) hydrate: bool,
    pub(crate) limit: i32,
    pub(crate) offset: i32,
    pub(crate) max_nodes: i32,
    pub(crate) max_frontier: i32,
    pub(crate) filter_condition: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub(crate) struct AggregationTraversalRequest {
    pub(crate) starts: Vec<PathCoordinate>,
    pub(crate) direction: TraversalDirection,
    pub(crate) min_depth: i32,
    pub(crate) max_depth: i32,
    pub(crate) edge_types: Option<Vec<String>>,
    pub(crate) node_tables: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggregateKind {
    Sum,
    Avg,
    Count,
}

impl AggregateKind {
    pub(crate) fn key(self) -> &'static str {
        match self {
            AggregateKind::Sum => "sum",
            AggregateKind::Avg => "avg",
            AggregateKind::Count => "count",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AggregateSpec {
    pub(crate) kind: AggregateKind,
    pub(crate) table_oid: u32,
    pub(crate) column: String,
    pub(crate) alias: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AggregateAccumulator {
    pub(crate) sum: f64,
    pub(crate) count: u64,
}

pub(crate) type ComponentNodeRow = (i64, pgrx::pg_sys::Oid, String, Option<pgrx::JsonB>);

pub(crate) type SearchOutputRow = (
    pgrx::pg_sys::Oid,
    String,
    String,
    f32,
    bool,
    Option<pgrx::JsonB>,
    String,
);
