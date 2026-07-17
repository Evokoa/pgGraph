//! # Safety — Error handling and crash prevention
//!
//! SQL-facing functions use a uniform boundary that maps [`GraphError`] values
//! to PostgreSQL errors with standard SQLSTATEs and stable pgGraph diagnostic
//! codes. pgrx and PostgreSQL provide the panic/error boundary; this module
//! owns graph-specific diagnostics and actionable hints.
//!
//! See: `docs/contributor_guide/safety-security.mdx`
//! See: `docs/user_guide/troubleshooting.mdx`

use pgrx::pg_sys::panic::ErrorReport;
use pgrx::{PgLogLevel, PgSqlErrorCode};

#[cfg(feature = "development")]
std::thread_local! {
    static TEST_ERROR_UNWIND_OBSERVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test-only stack guard used to prove that SQL error reporting unwinds Rust
/// frames before PostgreSQL transfers control to an exception handler.
#[cfg(feature = "development")]
pub(crate) struct TestErrorUnwindProbe;

#[cfg(feature = "development")]
impl TestErrorUnwindProbe {
    pub(crate) fn arm() -> Self {
        TEST_ERROR_UNWIND_OBSERVED.set(false);
        Self
    }
}

#[cfg(feature = "development")]
impl Drop for TestErrorUnwindProbe {
    fn drop(&mut self) {
        TEST_ERROR_UNWIND_OBSERVED.set(true);
    }
}

#[cfg(feature = "development")]
pub(crate) fn test_error_unwind_observed() -> bool {
    TEST_ERROR_UNWIND_OBSERVED.get()
}

/// Stable pgGraph error identifier included in PostgreSQL error detail.
///
/// These identifiers preserve pgGraph-specific classification while the wire
/// protocol uses standard PostgreSQL SQLSTATE values supported by pgrx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphDiagnosticCode {
    Internal,
    Oom,
    AclDenied,
    NotBuilt,
    EdgeTypeLimit,
    InvalidFilter,
    BuildLocked,
    ResourceLimit,
    EdgeBufferFull,
    CorruptFile,
    NodeNotFound,
    IncompatibleVersion,
    ReadOnly,
    GqlSyntax,
    GqlUnsupported,
    GqlSemantic,
    GqlParameter,
    GqlExecution,
    UnsupportedOperation,
    OverlayLimit,
    Disabled,
    RlsTopologyBoundary,
    SyncLogPruned,
}

impl GraphDiagnosticCode {
    /// Return the stable identifier rendered in PostgreSQL error detail.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "PG000",
            Self::Oom => "PG001",
            Self::AclDenied => "PG002",
            Self::NotBuilt => "PG003",
            Self::EdgeTypeLimit => "PG004",
            Self::InvalidFilter => "PG005",
            Self::BuildLocked => "PG006",
            Self::ResourceLimit => "PG007",
            Self::EdgeBufferFull => "PG008",
            Self::CorruptFile => "PG009",
            Self::NodeNotFound => "PG010",
            Self::IncompatibleVersion => "PG011",
            Self::ReadOnly => "PG012",
            Self::GqlSyntax => "PG013",
            Self::GqlUnsupported => "PG014",
            Self::GqlSemantic => "PG015",
            Self::GqlParameter => "PG016",
            Self::GqlExecution => "PG017",
            Self::UnsupportedOperation => "PG018",
            Self::OverlayLimit => "PG019",
            Self::Disabled => "PG020",
            Self::RlsTopologyBoundary => "PG021",
            Self::SyncLogPruned => "PG022",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GraphErrorClassification {
    diagnostic: GraphDiagnosticCode,
    sqlstate: &'static str,
    postgres_code: PgSqlErrorCode,
}

/// All graph engine errors.
///
/// Each variant maps to a standard PostgreSQL SQLSTATE and a stable
/// [`GraphDiagnosticCode`].
///
/// See: `docs/user_guide/troubleshooting.mdx`
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("graph: memory limit exceeded ({used_mb} MB used, need {need_mb} MB more, limit is {limit_mb} MB)")]
    Oom {
        used_mb: u64,
        need_mb: u64,
        limit_mb: u64,
    }, // PG001

    #[error("Permission denied for table {table}")]
    AclDenied { table: String }, // PG002

    #[error("Graph not built. Call graph.build() first.")]
    NotBuilt, // PG003

    #[error("Edge type limit exceeded (max 254)")]
    EdgeTypeLimit, // PG004

    #[error("Invalid filter condition: {reason}")]
    InvalidFilter { reason: String }, // PG005

    #[error("GQL syntax error: {reason}")]
    GqlSyntax { reason: String }, // PG013

    #[error("Unsupported GQL feature: {reason}")]
    GqlUnsupported { reason: String }, // PG014

    #[error("GQL semantic error: {reason}")]
    GqlSemantic { reason: String }, // PG015

    #[error("GQL parameter error: {reason}")]
    GqlParameter { reason: String }, // PG016

    #[error("GQL execution error: {reason}")]
    GqlExecution { reason: String }, // PG017

    #[error("Unsupported graph operation {operation}: {reason}")]
    UnsupportedOperation { operation: String, reason: String }, // PG018

    #[error("Another graph maintenance operation or registered source transaction is active")]
    BuildLocked, // PG006

    #[error("Graph resource limit exceeded in {phase}: {resource} would be {requested} with {used} already used; limit is {limit}")]
    ResourceLimit {
        resource: String,
        phase: String,
        used: u64,
        requested: u64,
        limit: u64,
    }, // PG007

    #[error("Edge mutation buffer full ({size} entries). Graph is in read-only mode.")]
    EdgeBufferFull { size: usize }, // PG008

    #[error("Graph is read-only: {reason}")]
    ReadOnly { reason: String }, // PG012

    #[error("Graph overlay limit exceeded: {kind} would be {requested}, limit is {limit}")]
    OverlayLimit {
        kind: String,
        requested: usize,
        limit: usize,
    }, // PG019

    #[error("Corrupt .pggraph file: {reason}")]
    CorruptFile { reason: String }, // PG009

    #[error("{0}")]
    IncompatibleVersion(String), // PG011

    #[error("Node not found: {table}.{pk}")]
    NodeNotFound { table: String, pk: String }, // PG010

    #[error("graph extension is disabled (graph.enabled = off)")]
    Disabled, // ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE

    #[error("table {table} has row-level security enabled; graph.build() refuses it unless graph.allow_rls_tables = on")]
    RlsTopologyBoundary { table: String }, // PG021

    #[error("this backend's sync replay position ({applied_sync_id}) predates a sync-log pruning pass (pruned below {pruned_before_id}); rebuild or vacuum to catch up")]
    SyncLogPruned {
        applied_sync_id: i64,
        pruned_before_id: i64,
    }, // PG022

    #[error("Internal error: {0}")]
    Internal(String),
}

impl GraphError {
    fn classification(&self) -> GraphErrorClassification {
        let (diagnostic, sqlstate, postgres_code) = match self {
            GraphError::Oom { .. } => (
                GraphDiagnosticCode::Oom,
                "53200",
                PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
            ),
            GraphError::AclDenied { .. } => (
                GraphDiagnosticCode::AclDenied,
                "42501",
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            ),
            GraphError::NotBuilt => (
                GraphDiagnosticCode::NotBuilt,
                "55000",
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            ),
            GraphError::EdgeTypeLimit => (
                GraphDiagnosticCode::EdgeTypeLimit,
                "54000",
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            ),
            GraphError::InvalidFilter { .. } => (
                GraphDiagnosticCode::InvalidFilter,
                "22023",
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            ),
            GraphError::GqlSyntax { .. } => (
                GraphDiagnosticCode::GqlSyntax,
                "42601",
                PgSqlErrorCode::ERRCODE_SYNTAX_ERROR,
            ),
            GraphError::GqlUnsupported { .. } => (
                GraphDiagnosticCode::GqlUnsupported,
                "0A000",
                PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            ),
            GraphError::GqlSemantic { .. } => (
                GraphDiagnosticCode::GqlSemantic,
                "22023",
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            ),
            GraphError::GqlParameter { .. } => (
                GraphDiagnosticCode::GqlParameter,
                "22023",
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            ),
            GraphError::GqlExecution { .. } => (
                GraphDiagnosticCode::GqlExecution,
                "22000",
                PgSqlErrorCode::ERRCODE_DATA_EXCEPTION,
            ),
            GraphError::UnsupportedOperation { .. } => (
                GraphDiagnosticCode::UnsupportedOperation,
                "0A000",
                PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            ),
            GraphError::BuildLocked => (
                GraphDiagnosticCode::BuildLocked,
                "55P03",
                PgSqlErrorCode::ERRCODE_LOCK_NOT_AVAILABLE,
            ),
            GraphError::ResourceLimit { .. } => (
                GraphDiagnosticCode::ResourceLimit,
                "54000",
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            ),
            GraphError::EdgeBufferFull { .. } => (
                GraphDiagnosticCode::EdgeBufferFull,
                "54000",
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            ),
            GraphError::ReadOnly { .. } => (
                GraphDiagnosticCode::ReadOnly,
                "55000",
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            ),
            GraphError::OverlayLimit { .. } => (
                GraphDiagnosticCode::OverlayLimit,
                "54000",
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            ),
            GraphError::CorruptFile { .. } => (
                GraphDiagnosticCode::CorruptFile,
                "XX001",
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            ),
            GraphError::IncompatibleVersion(_) => (
                GraphDiagnosticCode::IncompatibleVersion,
                "0A000",
                PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            ),
            GraphError::NodeNotFound { .. } => (
                GraphDiagnosticCode::NodeNotFound,
                "P0002",
                PgSqlErrorCode::ERRCODE_NO_DATA_FOUND,
            ),
            GraphError::Disabled => (
                GraphDiagnosticCode::Disabled,
                "55000",
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            ),
            GraphError::RlsTopologyBoundary { .. } => (
                GraphDiagnosticCode::RlsTopologyBoundary,
                "55000",
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            ),
            GraphError::SyncLogPruned { .. } => (
                GraphDiagnosticCode::SyncLogPruned,
                "55000",
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            ),
            GraphError::Internal(_) => (
                GraphDiagnosticCode::Internal,
                "XX000",
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            ),
        };
        GraphErrorClassification {
            diagnostic,
            sqlstate,
            postgres_code,
        }
    }

    /// Return the stable pgGraph diagnostic code for this error.
    ///
    /// See: `docs/user_guide/troubleshooting.mdx`
    pub(crate) fn diagnostic_code(&self) -> GraphDiagnosticCode {
        self.classification().diagnostic
    }

    /// Return the standard PostgreSQL SQLSTATE string emitted on the wire.
    pub fn sqlstate(&self) -> &'static str {
        self.classification().sqlstate
    }

    fn postgres_error_code(&self) -> PgSqlErrorCode {
        self.classification().postgres_code
    }

    /// Return the HINT string for this error.
    ///
    /// Provides actionable guidance for the user to resolve the error.
    pub fn hint(&self) -> String {
        match self {
            GraphError::Oom { limit_mb, .. } => {
                format!(
                    "Increase graph.memory_limit_mb (current: {} MB) or reduce the number of registered tables.",
                    limit_mb
                )
            }
            GraphError::AclDenied { table } => {
                format!("GRANT SELECT ON {} TO current_user;", table)
            }
            GraphError::NotBuilt => "Run: SELECT graph.build();".to_string(),
            GraphError::EdgeTypeLimit => {
                "Reduce the number of distinct edge labels. Maximum is 254.".to_string()
            }
            GraphError::InvalidFilter { .. } => {
                "Use JSONB filter helpers such as graph.eq(), graph.gt(), graph.gte(), graph.lt(), graph.lte(), graph.between(), graph.on_node(), and graph.all(); referenced columns must be registered with graph.add_filter_column().".to_string()
            }
            GraphError::GqlSyntax { .. } => {
                "Check the GQL query text against the supported read-only subset.".to_string()
            }
            GraphError::GqlUnsupported { .. } => {
                "Remove the unsupported GQL construct or rewrite the query using the documented compatibility matrix.".to_string()
            }
            GraphError::GqlSemantic { .. } => {
                "Verify labels, relationship types, aliases, and return bindings against registered graph metadata.".to_string()
            }
            GraphError::GqlParameter { .. } => {
                "Pass graph.gql() parameters as a JSON object and include every $parameter referenced by the query.".to_string()
            }
            GraphError::GqlExecution { .. } => {
                "Reduce result cardinality with labels, predicates, direction, hop bounds, or LIMIT; rebuild the graph if registered metadata changed.".to_string()
            }
            GraphError::UnsupportedOperation { .. } => {
                "Use a supported query shape, or run graph.vacuum()/graph.maintenance() to merge pending graph overlays before retrying.".to_string()
            }
            GraphError::BuildLocked => {
                "Wait for the active graph operation or source transaction to complete, then retry; inspect pg_stat_activity for long-running sessions.".to_string()
            }
            GraphError::ResourceLimit { resource, .. } if resource == "work units" => {
                "Reduce graph depth, candidates, or result cardinality, or increase graph.query_work_limit.".to_string()
            }
            GraphError::ResourceLimit { resource, .. } if resource == "disk bytes" => {
                "Reduce the operation size or increase graph.spill_disk_limit_mb after confirming free disk space.".to_string()
            }
            GraphError::ResourceLimit { .. } => {
                "Reduce the operation size or raise the applicable graph query or maintenance resource limit.".to_string()
            }
            GraphError::EdgeBufferFull { .. } => {
                "Run graph.vacuum() to merge pending mutations, or increase graph.edge_buffer_size.".to_string()
            }
            GraphError::ReadOnly { reason } if reason == "memory_limit" => {
                "Increase graph.memory_limit_mb or run graph.build() after reducing graph size.".to_string()
            }
            GraphError::ReadOnly { .. } => {
                "Inspect graph.status().read_only_reason, then run graph.maintenance(), graph.vacuum(), or graph.build() as appropriate.".to_string()
            }
            GraphError::OverlayLimit { kind, .. } if kind == "tx_delta_nodes" => {
                "Commit or roll back the current transaction, or increase graph.max_tx_delta_nodes.".to_string()
            }
            GraphError::OverlayLimit { kind, .. } if kind == "tx_delta_edges" => {
                "Commit or roll back the current transaction, or increase graph.max_tx_delta_edges.".to_string()
            }
            GraphError::OverlayLimit { .. } => {
                "Commit or roll back the current transaction, or increase graph.max_overlay_memory_mb.".to_string()
            }
            GraphError::CorruptFile { .. } => {
                "Run graph.build() to reconstruct the graph from source tables.".to_string()
            }
            GraphError::IncompatibleVersion(_) => {
                "Run graph.build() to regenerate the launch .pggraph artifact.".to_string()
            }
            GraphError::NodeNotFound { .. } => {
                "Verify the table and primary key exist. The graph may need a rebuild if data has changed.".to_string()
            }
            GraphError::Disabled => {
                "SET graph.enabled = on; or remove the setting from postgresql.conf.".to_string()
            }
            GraphError::RlsTopologyBoundary { table } => {
                format!(
                    "Topology reads (traverse/shortest_path/components) return coordinates and \
                     adjacency from the builder-scoped graph artifact under table-level ACL only, \
                     not {table}'s row-level security policies. Review \
                     docs/user_guide/administration-and-security.mdx#row-level-security-and-topology-reads, \
                     then set graph.allow_rls_tables = on to build over this table anyway."
                )
            }
            GraphError::SyncLogPruned { .. } => {
                "This backend went idle long enough for its sync-log watermark heartbeat to \
                 expire, and graph.maintenance() has since pruned rows it had not replayed yet. \
                 Incremental catch-up is no longer safe from this position; run graph.build() or \
                 graph.vacuum() to get a fresh consistent base, then resume normal sync."
                    .to_string()
            }
            GraphError::Internal(_) => {
                "This is a bug. Please report it with the full error message.".to_string()
            }
        }
    }

    /// Convert this error into a Postgres ERROR via pgrx.
    ///
    /// Uses pgrx's supported error-reporting boundary:
    /// - standard PostgreSQL SQLSTATE on the wire;
    /// - stable pgGraph diagnostic code in `DETAIL`;
    /// - HINT with actionable fix guidance
    ///
    /// SQL facade functions call this at the PostgreSQL error boundary.
    #[track_caller]
    pub fn report(self) -> ! {
        let sqlstate = self.postgres_error_code();
        let diagnostic = self.diagnostic_code();
        let hint = self.hint();
        let detail = format!("pgGraph diagnostic: {}", diagnostic.as_str());
        let msg = self.to_string();
        ErrorReport::new(sqlstate, msg, "GraphError::report")
            .set_detail(detail)
            .set_hint(hint)
            .report(PgLogLevel::ERROR);
        unreachable!("pgrx ERROR reporting returned unexpectedly");
    }
}

pub(crate) fn resource_limit_error(error: crate::resource::ResourceLimitError) -> GraphError {
    GraphError::ResourceLimit {
        resource: error.kind().as_str().to_string(),
        phase: error.phase().as_str().to_string(),
        used: error.used(),
        requested: error.requested(),
        limit: error.limit(),
    }
}

/// Result type alias for graph operations.
pub type GraphResult<T> = Result<T, GraphError>;

#[cfg(test)]
mod tests {
    //! Covers graph error classification, SQLSTATE mapping, and user-facing
    //! diagnostics for extension safety boundaries.

    use super::*;

    // ─── Error classification ───

    #[test]
    fn oom_maps_to_pg001() {
        let err = GraphError::Oom {
            used_mb: 100,
            need_mb: 200,
            limit_mb: 150,
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG001");
        assert_eq!(err.sqlstate(), "53200");
    }

    #[test]
    fn acl_denied_maps_to_pg002() {
        let err = GraphError::AclDenied {
            table: "users".to_string(),
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG002");
        assert_eq!(err.sqlstate(), "42501");
    }

    #[test]
    fn not_built_maps_to_pg003() {
        assert_eq!(GraphError::NotBuilt.diagnostic_code().as_str(), "PG003");
        assert_eq!(GraphError::NotBuilt.sqlstate(), "55000");
    }

    #[test]
    fn edge_type_limit_maps_to_pg004() {
        assert_eq!(
            GraphError::EdgeTypeLimit.diagnostic_code().as_str(),
            "PG004"
        );
        assert_eq!(GraphError::EdgeTypeLimit.sqlstate(), "54000");
    }

    #[test]
    fn invalid_filter_maps_to_pg005() {
        let err = GraphError::InvalidFilter {
            reason: "bad syntax".to_string(),
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG005");
        assert_eq!(err.sqlstate(), "22023");
    }

    #[test]
    fn gql_errors_map_to_stable_sqlstates() {
        assert_eq!(
            GraphError::GqlSyntax { reason: "r".into() }
                .diagnostic_code()
                .as_str(),
            "PG013"
        );
        assert_eq!(
            GraphError::GqlUnsupported { reason: "r".into() }
                .diagnostic_code()
                .as_str(),
            "PG014"
        );
        assert_eq!(
            GraphError::GqlSemantic { reason: "r".into() }
                .diagnostic_code()
                .as_str(),
            "PG015"
        );
        assert_eq!(
            GraphError::GqlParameter { reason: "r".into() }
                .diagnostic_code()
                .as_str(),
            "PG016"
        );
        assert_eq!(
            GraphError::GqlExecution { reason: "r".into() }
                .diagnostic_code()
                .as_str(),
            "PG017"
        );
    }

    #[test]
    fn build_locked_maps_to_pg006() {
        assert_eq!(GraphError::BuildLocked.diagnostic_code().as_str(), "PG006");
        assert_eq!(GraphError::BuildLocked.sqlstate(), "55P03");
    }

    #[test]
    fn resource_limit_maps_to_pg007() {
        let err = GraphError::ResourceLimit {
            resource: "work units".to_string(),
            phase: "query.expand".to_string(),
            used: 10,
            requested: 1,
            limit: 10,
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG007");
        assert_eq!(err.sqlstate(), "54000");
        assert!(err.hint().contains("graph.query_work_limit"));
    }

    #[test]
    fn edge_buffer_full_maps_to_pg008() {
        let err = GraphError::EdgeBufferFull { size: 100000 };
        assert_eq!(err.diagnostic_code().as_str(), "PG008");
        assert_eq!(err.sqlstate(), "54000");
    }

    #[test]
    fn unsupported_operation_maps_to_pg018() {
        let err = GraphError::UnsupportedOperation {
            operation: "op".to_string(),
            reason: "reason".to_string(),
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG018");
        assert_eq!(err.sqlstate(), "0A000");
    }

    #[test]
    fn overlay_limit_maps_to_pg019() {
        let err = GraphError::OverlayLimit {
            kind: "tx_delta_nodes".to_string(),
            requested: 2,
            limit: 1,
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG019");
        assert_eq!(err.sqlstate(), "54000");
        assert!(err.hint().contains("graph.max_tx_delta_nodes"));
    }

    #[test]
    fn read_only_maps_to_pg012() {
        let err = GraphError::ReadOnly {
            reason: "memory_limit".to_string(),
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG012");
        assert_eq!(err.sqlstate(), "55000");
    }

    #[test]
    fn corrupt_file_maps_to_pg009() {
        let err = GraphError::CorruptFile {
            reason: "bad magic".to_string(),
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG009");
        assert_eq!(err.sqlstate(), "XX001");
    }

    #[test]
    fn incompatible_version_maps_to_pg011() {
        let err = GraphError::IncompatibleVersion("outdated".to_string());
        assert_eq!(err.diagnostic_code().as_str(), "PG011");
        assert_eq!(err.sqlstate(), "0A000");
    }

    #[test]
    fn node_not_found_maps_to_pg010() {
        let err = GraphError::NodeNotFound {
            table: "t".to_string(),
            pk: "1".to_string(),
        };
        assert_eq!(err.diagnostic_code().as_str(), "PG010");
        assert_eq!(err.sqlstate(), "P0002");
    }

    #[test]
    fn disabled_maps_to_55000() {
        assert_eq!(GraphError::Disabled.sqlstate(), "55000");
        assert_eq!(GraphError::Disabled.diagnostic_code().as_str(), "PG020");
    }

    #[test]
    fn internal_maps_to_xx000() {
        let err = GraphError::Internal("boom".to_string());
        assert_eq!(err.sqlstate(), "XX000");
        assert_eq!(err.diagnostic_code().as_str(), "PG000");
    }

    #[test]
    fn gql_error_sqlstates_use_standard_postgres_classes() {
        assert_eq!(
            GraphError::GqlSyntax { reason: "r".into() }.sqlstate(),
            "42601"
        );
        assert_eq!(
            GraphError::GqlUnsupported { reason: "r".into() }.sqlstate(),
            "0A000"
        );
        assert_eq!(
            GraphError::GqlSemantic { reason: "r".into() }.sqlstate(),
            "22023"
        );
        assert_eq!(
            GraphError::GqlParameter { reason: "r".into() }.sqlstate(),
            "22023"
        );
        assert_eq!(
            GraphError::GqlExecution { reason: "r".into() }.sqlstate(),
            "22000"
        );
    }

    // ─── Hint messages ───

    #[test]
    fn oom_hint_mentions_memory_limit() {
        let err = GraphError::Oom {
            used_mb: 100,
            need_mb: 200,
            limit_mb: 512,
        };
        let hint = err.hint();
        assert!(
            hint.contains("512"),
            "hint should mention current limit: {}",
            hint
        );
        assert!(
            hint.contains("graph.memory_limit_mb"),
            "hint should name the GUC: {}",
            hint
        );
    }

    #[test]
    fn acl_hint_mentions_grant() {
        let err = GraphError::AclDenied {
            table: "secrets".to_string(),
        };
        let hint = err.hint();
        assert!(
            hint.contains("GRANT SELECT"),
            "hint should suggest GRANT: {}",
            hint
        );
        assert!(
            hint.contains("secrets"),
            "hint should name the table: {}",
            hint
        );
    }

    #[test]
    fn not_built_hint_mentions_build() {
        let hint = GraphError::NotBuilt.hint();
        assert!(
            hint.contains("graph.build()"),
            "hint should mention build(): {}",
            hint
        );
    }

    #[test]
    fn disabled_hint_mentions_enabled() {
        let hint = GraphError::Disabled.hint();
        assert!(
            hint.contains("graph.enabled"),
            "hint should mention the GUC: {}",
            hint
        );
    }

    #[test]
    fn edge_buffer_full_hint_mentions_vacuum() {
        let err = GraphError::EdgeBufferFull { size: 99999 };
        let hint = err.hint();
        assert!(
            hint.contains("vacuum()"),
            "hint should suggest vacuum: {}",
            hint
        );
    }

    // ─── Display messages ───

    #[test]
    fn display_oom_includes_numbers() {
        let err = GraphError::Oom {
            used_mb: 1024,
            need_mb: 512,
            limit_mb: 2048,
        };
        let msg = err.to_string();
        assert!(msg.contains("1024"), "should contain used_mb");
        assert!(msg.contains("512"), "should contain need_mb");
        assert!(msg.contains("2048"), "should contain limit_mb");
    }

    #[test]
    fn display_disabled_is_clear() {
        let msg = GraphError::Disabled.to_string();
        assert!(
            msg.contains("disabled"),
            "message should say disabled: {}",
            msg
        );
    }

    // ─── All variants have non-empty hints ───

    #[test]
    fn all_variants_have_nonempty_hints() {
        let variants: Vec<GraphError> = vec![
            GraphError::Oom {
                used_mb: 0,
                need_mb: 0,
                limit_mb: 0,
            },
            GraphError::AclDenied { table: "t".into() },
            GraphError::NotBuilt,
            GraphError::EdgeTypeLimit,
            GraphError::InvalidFilter { reason: "r".into() },
            GraphError::GqlSyntax { reason: "r".into() },
            GraphError::GqlUnsupported { reason: "r".into() },
            GraphError::GqlSemantic { reason: "r".into() },
            GraphError::GqlParameter { reason: "r".into() },
            GraphError::GqlExecution { reason: "r".into() },
            GraphError::UnsupportedOperation {
                operation: "op".into(),
                reason: "r".into(),
            },
            GraphError::OverlayLimit {
                kind: "tx_delta_nodes".into(),
                requested: 2,
                limit: 1,
            },
            GraphError::BuildLocked,
            GraphError::ResourceLimit {
                resource: "rows".into(),
                phase: "sync.ingest".into(),
                used: 1,
                requested: 1,
                limit: 1,
            },
            GraphError::EdgeBufferFull { size: 0 },
            GraphError::CorruptFile { reason: "r".into() },
            GraphError::IncompatibleVersion("r".into()),
            GraphError::NodeNotFound {
                table: "t".into(),
                pk: "1".into(),
            },
            GraphError::Disabled,
            GraphError::Internal("x".into()),
        ];
        for v in variants {
            let hint = v.hint();
            assert!(!hint.is_empty(), "variant {:?} has empty hint", v);
        }
    }

    // ─── All SQLSTATE codes are 5 chars or PG0xx ───

    #[test]
    fn all_sqlstate_codes_are_valid_format() {
        let variants: Vec<GraphError> = vec![
            GraphError::Oom {
                used_mb: 0,
                need_mb: 0,
                limit_mb: 0,
            },
            GraphError::AclDenied { table: "t".into() },
            GraphError::NotBuilt,
            GraphError::EdgeTypeLimit,
            GraphError::InvalidFilter { reason: "r".into() },
            GraphError::GqlSyntax { reason: "r".into() },
            GraphError::GqlUnsupported { reason: "r".into() },
            GraphError::GqlSemantic { reason: "r".into() },
            GraphError::GqlParameter { reason: "r".into() },
            GraphError::GqlExecution { reason: "r".into() },
            GraphError::UnsupportedOperation {
                operation: "op".into(),
                reason: "r".into(),
            },
            GraphError::OverlayLimit {
                kind: "tx_delta_edges".into(),
                requested: 2,
                limit: 1,
            },
            GraphError::BuildLocked,
            GraphError::ResourceLimit {
                resource: "rows".into(),
                phase: "sync.ingest".into(),
                used: 1,
                requested: 1,
                limit: 1,
            },
            GraphError::EdgeBufferFull { size: 0 },
            GraphError::CorruptFile { reason: "r".into() },
            GraphError::IncompatibleVersion("r".into()),
            GraphError::NodeNotFound {
                table: "t".into(),
                pk: "1".into(),
            },
            GraphError::Disabled,
            GraphError::Internal("x".into()),
        ];
        for v in variants {
            let code = v.sqlstate();
            assert_eq!(
                code.len(),
                5,
                "SQLSTATE must be 5 chars: {} for {:?}",
                code,
                v
            );
        }
    }

    #[test]
    fn diagnostic_codes_are_unique_per_variant() {
        use std::collections::HashMap;
        let variants: Vec<GraphError> = vec![
            GraphError::Oom {
                used_mb: 0,
                need_mb: 0,
                limit_mb: 0,
            },
            GraphError::AclDenied { table: "t".into() },
            GraphError::NotBuilt,
            GraphError::EdgeTypeLimit,
            GraphError::InvalidFilter { reason: "r".into() },
            GraphError::GqlSyntax { reason: "r".into() },
            GraphError::GqlUnsupported { reason: "r".into() },
            GraphError::GqlSemantic { reason: "r".into() },
            GraphError::GqlParameter { reason: "r".into() },
            GraphError::GqlExecution { reason: "r".into() },
            GraphError::UnsupportedOperation {
                operation: "op".into(),
                reason: "r".into(),
            },
            GraphError::OverlayLimit {
                kind: "overlay_memory_bytes".into(),
                requested: 2,
                limit: 1,
            },
            GraphError::BuildLocked,
            GraphError::ResourceLimit {
                resource: "rows".into(),
                phase: "sync.ingest".into(),
                used: 1,
                requested: 1,
                limit: 1,
            },
            GraphError::EdgeBufferFull { size: 0 },
            GraphError::CorruptFile { reason: "r".into() },
            GraphError::IncompatibleVersion("r".into()),
            GraphError::NodeNotFound {
                table: "t".into(),
                pk: "1".into(),
            },
            GraphError::Disabled,
            GraphError::Internal("x".into()),
        ];
        let mut seen: HashMap<&str, String> = HashMap::new();
        for v in &variants {
            let code = v.diagnostic_code().as_str();
            let name = format!("{:?}", v);
            if let Some(existing) = seen.get(code) {
                panic!(
                    "diagnostic code {} is used by both {:?} and {}",
                    code, v, existing
                );
            }
            seen.insert(code, name);
        }
    }

    #[test]
    fn display_messages_contain_context() {
        let err = GraphError::Oom {
            used_mb: 100,
            need_mb: 200,
            limit_mb: 128,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("200"), "OOM display should include need_mb");
        assert!(msg.contains("128"), "OOM display should include limit_mb");

        let err = GraphError::NodeNotFound {
            table: "users".into(),
            pk: "42".into(),
        };
        let msg = format!("{}", err);
        assert!(
            msg.contains("users"),
            "NodeNotFound display should include table"
        );
        assert!(msg.contains("42"), "NodeNotFound display should include pk");

        let err = GraphError::AclDenied {
            table: "secret".into(),
        };
        let msg = format!("{}", err);
        assert!(
            msg.contains("secret"),
            "AclDenied display should include table"
        );
    }
}
