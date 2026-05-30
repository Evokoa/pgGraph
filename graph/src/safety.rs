//! # Safety — Error handling and crash prevention
//!
//! SQL-facing functions use a uniform boundary that maps [`GraphError`] values
//! to PostgreSQL errors with stable SQLSTATEs. pgrx and PostgreSQL provide the
//! panic/error boundary; this module owns graph-specific error codes, hints,
//! and the direct PostgreSQL error-reporting FFI call.
//!
//! See: `docs/contributor_guide/safety-security.mdx`
//! See: `docs/user_guide/troubleshooting.mdx`

use pgrx::pg_sys::AsPgCStr;
use std::os::raw::{c_char, c_int};

/// All graph engine errors. Each variant maps to a SQLSTATE error code.
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

    #[error("Another build() or vacuum() is already running")]
    BuildLocked, // PG006

    #[error("Edge mutation buffer full ({size} entries). Graph is in read-only mode.")]
    EdgeBufferFull { size: usize }, // PG008

    #[error("Graph is read-only: {reason}")]
    ReadOnly { reason: String }, // PG012

    #[error("Corrupt .pggraph file: {reason}")]
    CorruptFile { reason: String }, // PG009

    #[error("{0}")]
    IncompatibleVersion(String), // PG011

    #[error("Node not found: {table}.{pk}")]
    NodeNotFound { table: String, pk: String }, // PG010

    #[error("graph extension is disabled (graph.enabled = off)")]
    Disabled, // ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE

    #[error("Internal error: {0}")]
    Internal(String),
}

impl GraphError {
    /// Return the SQLSTATE error code string for this error.
    ///
    /// See: `docs/user_guide/troubleshooting.mdx`
    pub fn sqlstate(&self) -> &'static str {
        match self {
            GraphError::Oom { .. } => "PG001",
            GraphError::AclDenied { .. } => "PG002",
            GraphError::NotBuilt => "PG003",
            GraphError::EdgeTypeLimit => "PG004",
            GraphError::InvalidFilter { .. } => "PG005",
            GraphError::GqlSyntax { .. } => "PG013",
            GraphError::GqlUnsupported { .. } => "PG014",
            GraphError::GqlSemantic { .. } => "PG015",
            GraphError::GqlParameter { .. } => "PG016",
            GraphError::GqlExecution { .. } => "PG017",
            GraphError::BuildLocked => "PG006",
            GraphError::EdgeBufferFull { .. } => "PG008",
            GraphError::ReadOnly { .. } => "PG012",
            GraphError::CorruptFile { .. } => "PG009",
            GraphError::IncompatibleVersion(_) => "PG011",
            GraphError::NodeNotFound { .. } => "PG010",
            GraphError::Disabled => "55000",
            GraphError::Internal(_) => "XX000",
        }
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
            GraphError::BuildLocked => {
                "Wait for the current build() or vacuum() to complete, or check pg_stat_activity for blocking sessions.".to_string()
            }
            GraphError::EdgeBufferFull { .. } => {
                "Run graph.vacuum() to merge pending mutations, or increase graph.edge_buffer_size.".to_string()
            }
            GraphError::ReadOnly { reason } if reason == "memory_limit" => {
                "Increase graph.memory_limit_mb, set graph.oom_action = 'error', or run graph.build() after reducing graph size.".to_string()
            }
            GraphError::ReadOnly { .. } => {
                "Inspect graph.status().read_only_reason, then run graph.maintenance(), graph.vacuum(), or graph.build() as appropriate.".to_string()
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
            GraphError::Internal(_) => {
                "This is a bug. Please report it with the full error message.".to_string()
            }
        }
    }

    /// Convert this error into a Postgres ERROR via pgrx.
    ///
    /// Uses Postgres' error reporting API directly:
    /// - Actual SQLSTATE code on the wire (not generic `P0001`)
    /// - DETAIL field with human-readable code reference
    /// - HINT with actionable fix guidance
    ///
    /// SQL facade functions call this at the PostgreSQL error boundary.
    #[track_caller]
    pub fn report(self) -> ! {
        let sqlstate = self.sqlstate();
        let hint = self.hint();
        let detail = format!("SQLSTATE: {}", sqlstate);
        let msg = self.to_string();
        let location = std::panic::Location::caller();

        raise_graph_error(
            make_sqlstate(sqlstate),
            msg,
            detail,
            hint,
            location.file().to_string(),
            location.line() as c_int,
        );
    }
}

/// Encode a 5-character SQLSTATE the same way Postgres' `MAKE_SQLSTATE` does.
///
/// `pgrx::PgSqlErrorCode` is an enum containing only Postgres' built-in codes,
/// so constructing custom `PG00x` values as that enum would be undefined
/// behavior. Keep this as a raw `c_int` and pass it directly to `errcode()`.
fn make_sqlstate(code: &str) -> c_int {
    let b = code.as_bytes();
    debug_assert_eq!(b.len(), 5, "SQLSTATE must be exactly 5 characters");
    let encode = |c: u8| -> c_int { (c.wrapping_sub(b'0') as c_int) & 0x3F };
    encode(b[0])
        | (encode(b[1]) << 6)
        | (encode(b[2]) << 12)
        | (encode(b[3]) << 18)
        | (encode(b[4]) << 24)
}

fn raise_graph_error(
    sqlerrcode: c_int,
    message: String,
    detail: String,
    hint: String,
    file: String,
    line: c_int,
) -> ! {
    const PERCENT_S: *const c_char = c"%s".as_ptr();
    const FUNCTION: *const c_char = c"GraphError::report caller".as_ptr();
    const DEFAULT_DOMAIN: *const c_char = std::ptr::null();

    #[cfg_attr(target_os = "windows", link(name = "postgres"))]
    unsafe extern "C-unwind" {
        fn errstart(elevel: c_int, domain: *const c_char) -> bool;
        fn errcode(sqlerrcode: c_int) -> c_int;
        fn errmsg(fmt: *const c_char, ...) -> c_int;
        fn errdetail(fmt: *const c_char, ...) -> c_int;
        fn errhint(fmt: *const c_char, ...) -> c_int;
        fn errfinish(filename: *const c_char, lineno: c_int, funcname: *const c_char);
    }

    let message = message.as_pg_cstr();
    let detail = detail.as_pg_cstr();
    let hint = hint.as_pg_cstr();
    let file = file.as_pg_cstr();

    // SAFETY: These calls mirror pgrx's internal ErrorReport emission path, but
    // keep the SQLSTATE as a raw Postgres `MAKE_SQLSTATE` integer instead of
    // forcing it through pgrx's built-in-code enum. The message/detail/hint
    // pointers are allocated with Postgres `palloc`, so they remain valid until
    // `errfinish()` transfers control back to Postgres.
    unsafe {
        if errstart(pgrx::PgLogLevel::ERROR as c_int, DEFAULT_DOMAIN) {
            errcode(sqlerrcode);
            errmsg(PERCENT_S, message);
            errdetail(PERCENT_S, detail);
            errhint(PERCENT_S, hint);
            errfinish(file, line, FUNCTION);
        }
    }

    unreachable!("Postgres ERROR reporting returned unexpectedly");
}

/// Result type alias for graph operations.
pub type GraphResult<T> = Result<T, GraphError>;

#[cfg(test)]
mod tests {
    //! Covers graph error classification, SQLSTATE mapping, and user-facing
    //! diagnostics for extension safety boundaries.

    use super::*;

    // ─── SQLSTATE mapping ───

    #[test]
    fn oom_maps_to_pg001() {
        let err = GraphError::Oom {
            used_mb: 100,
            need_mb: 200,
            limit_mb: 150,
        };
        assert_eq!(err.sqlstate(), "PG001");
    }

    #[test]
    fn acl_denied_maps_to_pg002() {
        let err = GraphError::AclDenied {
            table: "users".to_string(),
        };
        assert_eq!(err.sqlstate(), "PG002");
    }

    #[test]
    fn not_built_maps_to_pg003() {
        assert_eq!(GraphError::NotBuilt.sqlstate(), "PG003");
    }

    #[test]
    fn edge_type_limit_maps_to_pg004() {
        assert_eq!(GraphError::EdgeTypeLimit.sqlstate(), "PG004");
    }

    #[test]
    fn invalid_filter_maps_to_pg005() {
        let err = GraphError::InvalidFilter {
            reason: "bad syntax".to_string(),
        };
        assert_eq!(err.sqlstate(), "PG005");
    }

    #[test]
    fn gql_errors_map_to_stable_sqlstates() {
        assert_eq!(
            GraphError::GqlSyntax { reason: "r".into() }.sqlstate(),
            "PG013"
        );
        assert_eq!(
            GraphError::GqlUnsupported { reason: "r".into() }.sqlstate(),
            "PG014"
        );
        assert_eq!(
            GraphError::GqlSemantic { reason: "r".into() }.sqlstate(),
            "PG015"
        );
        assert_eq!(
            GraphError::GqlParameter { reason: "r".into() }.sqlstate(),
            "PG016"
        );
        assert_eq!(
            GraphError::GqlExecution { reason: "r".into() }.sqlstate(),
            "PG017"
        );
    }

    #[test]
    fn build_locked_maps_to_pg006() {
        assert_eq!(GraphError::BuildLocked.sqlstate(), "PG006");
    }

    #[test]
    fn edge_buffer_full_maps_to_pg008() {
        let err = GraphError::EdgeBufferFull { size: 100000 };
        assert_eq!(err.sqlstate(), "PG008");
    }

    #[test]
    fn read_only_maps_to_pg012() {
        let err = GraphError::ReadOnly {
            reason: "memory_limit".to_string(),
        };
        assert_eq!(err.sqlstate(), "PG012");
    }

    #[test]
    fn corrupt_file_maps_to_pg009() {
        let err = GraphError::CorruptFile {
            reason: "bad magic".to_string(),
        };
        assert_eq!(err.sqlstate(), "PG009");
    }

    #[test]
    fn incompatible_version_maps_to_pg011() {
        let err = GraphError::IncompatibleVersion("outdated".to_string());
        assert_eq!(err.sqlstate(), "PG011");
    }

    #[test]
    fn node_not_found_maps_to_pg010() {
        let err = GraphError::NodeNotFound {
            table: "t".to_string(),
            pk: "1".to_string(),
        };
        assert_eq!(err.sqlstate(), "PG010");
    }

    #[test]
    fn disabled_maps_to_55000() {
        assert_eq!(GraphError::Disabled.sqlstate(), "55000");
    }

    #[test]
    fn internal_maps_to_xx000() {
        let err = GraphError::Internal("boom".to_string());
        assert_eq!(err.sqlstate(), "XX000");
    }

    #[test]
    fn custom_sqlstate_is_encoded_for_wire_protocol() {
        assert_eq!(make_sqlstate("PG001"), make_sqlstate_for_test(b"PG001"));
        assert_eq!(make_sqlstate("PG010"), make_sqlstate_for_test(b"PG010"));
        assert_ne!(
            make_sqlstate("PG001"),
            pgrx::PgSqlErrorCode::ERRCODE_RAISE_EXCEPTION as std::os::raw::c_int,
            "custom SQLSTATEs must not collapse to P0001"
        );
    }

    #[test]
    fn standard_sqlstate_encoding_matches_pgrx_builtin_codes() {
        assert_eq!(
            make_sqlstate("55000"),
            pgrx::PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE as std::os::raw::c_int
        );
        assert_eq!(
            make_sqlstate("XX000"),
            pgrx::PgSqlErrorCode::ERRCODE_INTERNAL_ERROR as std::os::raw::c_int
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
            GraphError::BuildLocked,
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
            GraphError::BuildLocked,
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
    fn sqlstate_codes_are_unique_per_variant() {
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
            GraphError::BuildLocked,
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
            let code = v.sqlstate();
            let name = format!("{:?}", v);
            if let Some(existing) = seen.get(code) {
                // XX000 (Internal) is the standard Postgres "internal error" code.
                // It's allowed to be shared because it's the catch-all.
                if code != "XX000" {
                    panic!("SQLSTATE {} is used by both {:?} and {}", code, v, existing);
                }
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

    fn make_sqlstate_for_test(code: &[u8; 5]) -> std::os::raw::c_int {
        let encode = |c: u8| -> std::os::raw::c_int {
            (std::os::raw::c_int::from(c.wrapping_sub(b'0'))) & 0x3F
        };
        encode(code[0])
            | (encode(code[1]) << 6)
            | (encode(code[2]) << 12)
            | (encode(code[3]) << 18)
            | (encode(code[4]) << 24)
    }
}
