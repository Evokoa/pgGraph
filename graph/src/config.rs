//! # Config — GUC (Grand Unified Configuration) parameters
//!
//! PostgreSQL GUC parameters for the graph extension.
//! All settings are prefixed with `graph.` and can be set via:
//! - `postgresql.conf`
//! - `ALTER SYSTEM SET graph.memory_limit_mb = 4096;`
//! - `SET graph.default_max_depth = 10;`
//!
//! See: `docs/user_guide/configuration.mdx`
//! See: `docs/user_guide/api-reference.mdx`

use pgrx::guc::*;

// ─── Master Kill Switch ───

/// Master kill switch for all query functions.
/// When false, query APIs such as traverse/search/path/aggregate return
/// ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE immediately.
/// Admin functions (build, status, reset) are NOT gated.
/// Default: true.
pub static ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);

// ─── Memory ───

/// Maximum memory (MB) the graph engine may consume per backend.
/// Default: 2048 MB. Range: 64–32768.
pub static MEMORY_LIMIT_MB: GucSetting<i32> = GucSetting::<i32>::new(2048);

// ─── Query Defaults ───

/// Default maximum BFS depth when not specified by the caller.
/// Default: 5. Range: 1–100.
pub static DEFAULT_MAX_DEPTH: GucSetting<i32> = GucSetting::<i32>::new(5);

/// Default search mode for search and traverse_search.
/// Default: "contains".
pub static DEFAULT_SEARCH_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(None);

/// Default case handling for text search.
/// Default: false.
pub static DEFAULT_CASE_SENSITIVE: GucSetting<bool> = GucSetting::<bool>::new(false);

/// Default hydrate behavior for APIs that opt into this GUC.
/// Default: true.
pub static DEFAULT_HYDRATE: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Maximum number of nodes a single traverse may visit.
/// Circuit breaker to prevent runaway queries.
/// Default: 100000. Range: 1–10000000.
pub static MAX_NODES: GucSetting<i32> = GucSetting::<i32>::new(100_000);

/// Maximum frontier size during BFS.
/// Circuit breaker for breadth-heavy graphs.
/// Default: 100000. Range: 1–10000000.
pub static MAX_FRONTIER: GucSetting<i32> = GucSetting::<i32>::new(100_000);

/// Maximum exact paths counted by path_count_estimate().
/// Counts above this return early with capped=true.
/// Default: 100000. Range: 1–10000000.
pub static MAX_EXACT_PATH_COUNT: GucSetting<i32> = GucSetting::<i32>::new(100_000);

/// Maximum active rows per SPI cursor build batch.
/// Default: 10000. Range: 1–1000000.
pub static BUILD_BATCH_SIZE: GucSetting<i32> = GucSetting::<i32>::new(10_000);

// ─── Persistence ───

/// Whether to persist the graph to disk after build().
/// Default: true.
pub static PERSIST_ON_BUILD: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Whether to auto-load persisted graph on first query.
/// Default: true.
pub static AUTO_LOAD: GucSetting<bool> = GucSetting::<bool>::new(true);

// ─── Sync ───

/// Maximum pending edge mutations in the backend-local sync overlay.
/// When exceeded, graph enters read-only mode (PG008).
/// Default: 100000. Range: 1000–10000000.
pub static EDGE_BUFFER_SIZE: GucSetting<i32> = GucSetting::<i32>::new(100_000);

/// Maximum sync-log rows replayed in one internal batch.
/// Default: 1000. Range: 1–100000.
pub static SYNC_BATCH_SIZE: GucSetting<i32> = GucSetting::<i32>::new(1_000);

/// Reserved maintenance interval in seconds.
/// Registered for future scheduling work; current code does not schedule vacuum.
/// Default: 60. Range: 5–86400.
pub static VACUUM_INTERVAL_SECS: GucSetting<i32> = GucSetting::<i32>::new(60);

// ─── String GUCs (stored as Option<CString>) ───
// Note: pgrx 0.18 string GUCs use CString internally. We provide getter helpers
// that return String for ergonomic use throughout the codebase.

/// Subdirectory of $PGDATA for .pggraph file storage.
/// Default: "graph".
pub static DATA_DIR: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(None);

/// Sync mode: 'manual', 'trigger', or reserved 'wal'.
/// Default: "manual".
pub static SYNC_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(None);

/// OOM action: 'error' (return SQL error) or 'readonly' (degrade gracefully).
/// Default: "error".
pub static OOM_ACTION: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(None);

/// Session GUC name to read tenant scope from when query tenant is omitted.
/// Default: empty string, meaning no session fallback.
pub static TENANT_SETTING: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(None);

/// Build scan mode: 'select' or 'copy'.
/// Default: "select".
pub static BUILD_SCAN_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(None);

/// Whether tenanted graphs require a query or session tenant.
/// Default: true.
pub static ENFORCE_TENANT_SCOPE: GucSetting<bool> = GucSetting::<bool>::new(true);

// ─── Typed Enums for String GUCs ───

/// Action to take when a build() would exceed `graph.memory_limit_mb`.
///
/// Parsed from the `graph.oom_action` GUC string at the config boundary.
/// Downstream code never sees raw strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OomAction {
    /// Return `GraphError::Oom` — the build is aborted.
    #[default]
    Error,
    /// Log a WARNING and continue building in read-only mode.
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuildScanMode {
    #[default]
    Select,
    Copy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
    #[default]
    Manual,
    Trigger,
    Wal,
}

impl SyncMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncMode::Manual => "manual",
            SyncMode::Trigger => "trigger",
            SyncMode::Wal => "wal",
        }
    }
}

// ─── String GUC Helpers ───

/// Get the data_dir setting as a String.
pub fn data_dir() -> String {
    DATA_DIR
        .get()
        .as_ref()
        .and_then(|c| c.to_str().ok())
        .unwrap_or("graph")
        .to_string()
}

/// Get the sync_mode setting as a String.
pub fn sync_mode() -> String {
    SYNC_MODE
        .get()
        .as_ref()
        .and_then(|c| c.to_str().ok())
        .unwrap_or("manual")
        .to_string()
}

/// Get the oom_action setting as a typed enum.
///
/// Parses the raw GUC string at the boundary. Unrecognised values
/// fall back to `OomAction::Error` (the safe default).
pub fn oom_action() -> OomAction {
    let binding = OOM_ACTION.get();
    let raw = binding
        .as_ref()
        .and_then(|c| c.to_str().ok())
        .unwrap_or("error");

    parse_oom_action(raw)
}

/// Get the tenant_setting GUC as a String.
pub fn tenant_setting() -> String {
    TENANT_SETTING
        .get()
        .as_ref()
        .and_then(|c| c.to_str().ok())
        .unwrap_or("")
        .to_string()
}

pub fn build_scan_mode() -> BuildScanMode {
    let binding = BUILD_SCAN_MODE.get();
    let raw = binding
        .as_ref()
        .and_then(|c| c.to_str().ok())
        .unwrap_or("select");

    parse_build_scan_mode(raw)
}

pub fn parsed_sync_mode() -> Option<SyncMode> {
    let binding = SYNC_MODE.get();
    let raw = binding
        .as_ref()
        .and_then(|c| c.to_str().ok())
        .unwrap_or("manual");

    parse_sync_mode(raw)
}

pub fn sync_batch_size() -> usize {
    SYNC_BATCH_SIZE.get().max(1) as usize
}

/// Parse an OOM action string into a typed policy.
fn parse_oom_action(raw: &str) -> OomAction {
    match raw.trim().to_ascii_lowercase().as_str() {
        "readonly" | "read_only" | "read-only" => OomAction::ReadOnly,
        "error" => OomAction::Error,
        other => {
            // Defensive: unknown value → safe default
            #[cfg(not(test))]
            pgrx::warning!(
                "graph.oom_action: unrecognised value '{}', defaulting to 'error'",
                other
            );
            let _ = other; // suppress unused warning in test cfg
            OomAction::Error
        }
    }
}

fn parse_build_scan_mode(raw: &str) -> BuildScanMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "copy" => BuildScanMode::Copy,
        "select" => BuildScanMode::Select,
        other => {
            #[cfg(not(test))]
            pgrx::warning!(
                "graph.build_scan_mode: unrecognised value '{}', defaulting to 'select'",
                other
            );
            let _ = other;
            BuildScanMode::Select
        }
    }
}

fn parse_sync_mode(raw: &str) -> Option<SyncMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "manual" => Some(SyncMode::Manual),
        "trigger" => Some(SyncMode::Trigger),
        "wal" => Some(SyncMode::Wal),
        _ => None,
    }
}

/// Register all GUC parameters with PostgreSQL.
///
/// Called from `_PG_init()`.
pub fn register_gucs() {
    // ── Master Kill Switch ──

    GucRegistry::define_bool_guc(
        c"graph.enabled",
        c"Master kill switch for graph query functions.",
        c"When off, graph query APIs return an error. Admin functions still work.",
        &ENABLED,
        GucContext::Userset,
        GucFlags::default(),
    );

    // ── Memory ──

    GucRegistry::define_int_guc(
        c"graph.memory_limit_mb",
        c"Maximum memory (MB) the graph engine may use per backend.",
        c"Default: 2048 MB. Increase for very large graphs.",
        &MEMORY_LIMIT_MB,
        64,
        32768,
        GucContext::Suset,
        GucFlags::default(),
    );

    // ── Query Defaults ──

    GucRegistry::define_int_guc(
        c"graph.default_max_depth",
        c"Default maximum BFS traversal depth.",
        c"Used when max_depth is not specified in graph.traverse().",
        &DEFAULT_MAX_DEPTH,
        1,
        100,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"graph.default_search_mode",
        c"Default search mode for graph.search() and graph.traverse_search().",
        c"Default: 'contains'. Supported values are contains, prefix, exact, and token.",
        &DEFAULT_SEARCH_MODE,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"graph.default_case_sensitive",
        c"Default case handling for graph.search() and graph.traverse_search().",
        c"When false, text search defaults to case-insensitive matching.",
        &DEFAULT_CASE_SENSITIVE,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"graph.default_hydrate",
        c"Default JSONB hydration behavior for APIs that opt into this GUC.",
        c"When true, graph.traverse_search() returns hydrated source rows by default. graph.search() and graph.traverse() use their own SQL defaults.",
        &DEFAULT_HYDRATE,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"graph.max_nodes",
        c"Maximum nodes a single traversal may visit.",
        c"Circuit breaker to prevent runaway queries on highly connected graphs.",
        &MAX_NODES,
        1,
        10_000_000,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"graph.max_frontier",
        c"Maximum BFS frontier size.",
        c"Circuit breaker for breadth-heavy graphs.",
        &MAX_FRONTIER,
        1,
        10_000_000,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"graph.max_exact_path_count",
        c"Maximum exact path count before graph.path_count_estimate() caps.",
        c"Counts above this return early with exact=false and capped=true.",
        &MAX_EXACT_PATH_COUNT,
        1,
        10_000_000,
        GucContext::Userset,
        GucFlags::default(),
    );

    // ── Persistence ──

    GucRegistry::define_bool_guc(
        c"graph.persist_on_build",
        c"Write .pggraph file to disk after build().",
        c"Set to false to skip persistence (in-memory only mode).",
        &PERSIST_ON_BUILD,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"graph.auto_load",
        c"Auto-load persisted graph on first query.",
        c"When true, backends load the .pggraph file on first query if the engine is empty.",
        &AUTO_LOAD,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"graph.build_batch_size",
        c"Maximum active rows per SPI cursor build batch.",
        c"Bounds graph.build() overlay memory during out-of-core construction.",
        &BUILD_BATCH_SIZE,
        1,
        1_000_000,
        GucContext::Suset,
        GucFlags::default(),
    );

    // ── Sync ──

    GucRegistry::define_int_guc(
        c"graph.edge_buffer_size",
        c"Maximum pending edge mutations in the backend-local sync overlay.",
        c"When exceeded, graph enters read-only mode.",
        &EDGE_BUFFER_SIZE,
        1_000,
        10_000_000,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"graph.sync_batch_size",
        c"Maximum sync-log rows replayed in one internal batch.",
        c"Bounds graph.apply_sync() and future query-time sync catch-up replay memory.",
        &SYNC_BATCH_SIZE,
        1,
        100_000,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"graph.vacuum_interval_secs",
        c"Reserved maintenance interval in seconds.",
        c"Registered for future scheduling work; current code does not schedule vacuum. Default: 60.",
        &VACUUM_INTERVAL_SECS,
        5,
        86_400,
        GucContext::Suset,
        GucFlags::default(),
    );

    // ── String GUCs ──

    GucRegistry::define_string_guc(
        c"graph.data_dir",
        c"Subdirectory of $PGDATA for .pggraph file storage.",
        c"Default: 'graph'. The directory is created automatically.",
        &DATA_DIR,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"graph.sync_mode",
        c"Sync strategy: 'manual', 'trigger', or 'wal'.",
        c"Manual: no trigger install. Trigger: trigger-backed sync log. WAL: reserved.",
        &SYNC_MODE,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"graph.oom_action",
        c"Action on OOM: 'error' or 'readonly'.",
        c"'error': return SQL ERROR. 'readonly': degrade to read-only mode.",
        &OOM_ACTION,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"graph.tenant_setting",
        c"Session GUC name used for tenant scope fallback.",
        c"When non-empty, graph queries read current_setting(graph.tenant_setting, true).",
        &TENANT_SETTING,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"graph.build_scan_mode",
        c"Build scan mode: 'select' or 'copy'.",
        c"'select' uses SPI cursors. 'copy' is reserved until a safe COPY reader is available.",
        &BUILD_SCAN_MODE,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"graph.enforce_tenant_scope",
        c"Require tenant scope for tenanted graphs.",
        c"When on, tenanted graph queries without explicit or session tenant fail.",
        &ENFORCE_TENANT_SCOPE,
        GucContext::Userset,
        GucFlags::default(),
    );
}

#[cfg(test)]
mod tests {
    //! Covers parsing of SQL-facing configuration values and protects fallback
    //! semantics for invalid GUC input.

    use super::{
        parse_build_scan_mode, parse_oom_action, parse_sync_mode, BuildScanMode, OomAction,
        SyncMode,
    };

    #[test]
    fn parse_oom_action_accepts_error() {
        assert_eq!(parse_oom_action("error"), OomAction::Error);
        assert_eq!(parse_oom_action(" ERROR "), OomAction::Error);
    }

    #[test]
    fn parse_oom_action_accepts_readonly_aliases() {
        assert_eq!(parse_oom_action("readonly"), OomAction::ReadOnly);
        assert_eq!(parse_oom_action("read_only"), OomAction::ReadOnly);
        assert_eq!(parse_oom_action("read-only"), OomAction::ReadOnly);
        assert_eq!(parse_oom_action(" ReAdOnLy "), OomAction::ReadOnly);
    }

    #[test]
    fn parse_oom_action_defaults_unknown_to_error() {
        assert_eq!(parse_oom_action("garbage"), OomAction::Error);
        assert_eq!(parse_oom_action(""), OomAction::Error);
    }

    #[test]
    fn parse_build_scan_mode_accepts_supported_modes() {
        assert_eq!(parse_build_scan_mode("select"), BuildScanMode::Select);
        assert_eq!(parse_build_scan_mode(" COPY "), BuildScanMode::Copy);
    }

    #[test]
    fn parse_build_scan_mode_defaults_unknown_to_select() {
        assert_eq!(parse_build_scan_mode("garbage"), BuildScanMode::Select);
        assert_eq!(parse_build_scan_mode(""), BuildScanMode::Select);
    }

    #[test]
    fn parse_sync_mode_accepts_documented_modes() {
        assert_eq!(parse_sync_mode("manual"), Some(SyncMode::Manual));
        assert_eq!(parse_sync_mode(" TRIGGER "), Some(SyncMode::Trigger));
        assert_eq!(parse_sync_mode("wal"), Some(SyncMode::Wal));
    }

    #[test]
    fn parse_sync_mode_rejects_unknown_modes() {
        assert_eq!(parse_sync_mode("async"), None);
        assert_eq!(parse_sync_mode(""), None);
    }
}
