//! # ACL — Access Control List pre-flight checks
//!
//! Query helpers call `check_table_acl()` or `check_table_acls()` before
//! reading source-table rows or returning graph coordinates and hydrated data.
//! Write helpers call `check_table_insert_acl()`, `check_table_update_acl()`,
//! or `check_table_delete_acl()` before modifying mapped rows.
//!
//! `row_security_applies_to_effective_caller()` is the unsafe adapter used for
//! query-time policy enablement. It matches the execution identity seen by
//! source SQL, including user-created `SECURITY DEFINER` wrappers. The outer
//! caller adapter remains development-only evidence for catalog mediators.
//!
//! See: `docs/contributor_guide/safety-security.mdx`
//! See: `docs/user_guide/administration-and-security.mdx`

use crate::safety::{GraphError, GraphResult};
use std::collections::BTreeSet;

/// Check if the current user has SELECT privilege on the given table OID.
///
/// Uses PostgreSQL's native relation ACL checker.
///
/// # Errors
/// Returns `GraphError::AclDenied` if the user lacks SELECT on the table.
pub fn check_table_acl(table_oid: u32) -> GraphResult<()> {
    check_table_acl_mode(table_oid, pgrx::pg_sys::ACL_SELECT as pgrx::pg_sys::AclMode)
}

/// Check SELECT privilege on every distinct table OID.
///
/// Sorting the OIDs keeps the first reported denial deterministic when a
/// graph result references more than one inaccessible table.
///
/// # Errors
///
/// Returns [`GraphError::AclDenied`] if the current role lacks `SELECT` on any
/// referenced table.
pub(crate) fn check_table_acls(table_oids: impl IntoIterator<Item = u32>) -> GraphResult<()> {
    for table_oid in table_oids.into_iter().collect::<BTreeSet<_>>() {
        check_table_acl(table_oid)?;
    }
    Ok(())
}

/// Check if the current user has INSERT privilege on the given table OID.
///
/// Uses PostgreSQL's native relation ACL checker.
///
/// # Errors
/// Returns `GraphError::AclDenied` if the user lacks INSERT on the table.
pub fn check_table_insert_acl(table_oid: u32) -> GraphResult<()> {
    check_table_acl_mode(table_oid, pgrx::pg_sys::ACL_INSERT as pgrx::pg_sys::AclMode)
}

/// Check if the current user has UPDATE privilege on the given table OID.
///
/// Uses PostgreSQL's native relation ACL checker.
///
/// # Errors
/// Returns `GraphError::AclDenied` if the user lacks UPDATE on the table.
pub fn check_table_update_acl(table_oid: u32) -> GraphResult<()> {
    check_table_acl_mode(table_oid, pgrx::pg_sys::ACL_UPDATE as pgrx::pg_sys::AclMode)
}

/// Check if the current user has DELETE privilege on the given table OID.
///
/// Uses PostgreSQL's native relation ACL checker.
///
/// # Errors
/// Returns `GraphError::AclDenied` if the user lacks DELETE on the table.
pub fn check_table_delete_acl(table_oid: u32) -> GraphResult<()> {
    check_table_acl_mode(table_oid, pgrx::pg_sys::ACL_DELETE as pgrx::pg_sys::AclMode)
}

/// Return whether PostgreSQL RLS applies to the outer caller for this relation.
///
/// `noError = false` deliberately preserves PostgreSQL's normal error when
/// `row_security = off` cannot be honored safely. `RLS_NONE_ENV` means the
/// caller currently bypasses policies through ownership or `BYPASSRLS` and is
/// therefore unrestricted for this statement.
#[cfg(feature = "development")]
pub(crate) fn row_security_applies_to_outer_caller(table_oid: u32) -> bool {
    let caller_oid = unsafe {
        // SAFETY: This code runs inside a PostgreSQL backend. GetOuterUserId
        // returns the identity outside SECURITY DEFINER frames and retains no
        // Rust-managed memory.
        pgrx::pg_sys::GetOuterUserId()
    };
    let result = unsafe {
        // SAFETY: `table_oid` comes from validated registered catalog state;
        // `caller_oid` is the current backend's outer role. PostgreSQL owns the
        // active snapshot and policy caches. `noError = false` requests normal
        // PostgreSQL error behavior instead of suppressing unsafe environments.
        pgrx::pg_sys::check_enable_rls(pgrx::pg_sys::Oid::from_u32(table_oid), caller_oid, false)
    };
    result == pgrx::pg_sys::CheckEnableRlsResult::RLS_ENABLED as i32
}

/// Return whether PostgreSQL RLS applies to the effective SQL execution role.
///
/// Visibility SPI runs as this role, including inside a user-created
/// `SECURITY DEFINER` wrapper, so lazy planning must use the same identity.
pub(crate) fn row_security_applies_to_effective_caller(table_oid: u32) -> bool {
    let role_oid = unsafe {
        // SAFETY: This runs inside a PostgreSQL backend and retains no pointer.
        pgrx::pg_sys::GetUserId()
    };
    let result = unsafe {
        // SAFETY: the relation OID is validated catalog state and role_oid is
        // the backend's effective execution identity.
        pgrx::pg_sys::check_enable_rls(pgrx::pg_sys::Oid::from_u32(table_oid), role_oid, false)
    };
    result == pgrx::pg_sys::CheckEnableRlsResult::RLS_ENABLED as i32
}

/// Require the outer caller to hold PostgreSQL's real privilege for the
/// `graph.rls_mode` compatibility bypass.
///
/// This check is repeated at execution because PostgreSQL accepts unknown
/// dotted names as placeholder GUCs before an extension is first loaded. A
/// non-superuser must not turn such a pre-load placeholder into a security
/// bypass merely by causing `_PG_init()` to register the real `SUSET` GUC.
pub(crate) fn require_rls_bypass_privilege() -> GraphResult<()> {
    let caller_oid = unsafe {
        // SAFETY: This code runs inside a PostgreSQL backend and retains no
        // pointer returned by PostgreSQL.
        pgrx::pg_sys::GetOuterUserId()
    };
    let allowed = if unsafe {
        // SAFETY: `caller_oid` is a backend-owned role OID and superuser_arg
        // performs a catalog lookup without retaining Rust memory.
        pgrx::pg_sys::superuser_arg(caller_oid)
    } {
        true
    } else {
        #[cfg(any(feature = "pg15", feature = "pg16", feature = "pg17", feature = "pg18"))]
        {
            unsafe {
                // SAFETY: The parameter name is a static NUL-terminated C
                // string; caller_oid is valid for this backend. PostgreSQL 15+
                // owns and evaluates parameter ACLs.
                pgrx::pg_sys::pg_parameter_aclcheck(
                    c"graph.rls_mode".as_ptr(),
                    caller_oid,
                    pgrx::pg_sys::ACL_SET as pgrx::pg_sys::AclMode,
                ) == pgrx::pg_sys::AclResult::ACLCHECK_OK
            }
        }
        #[cfg(any(feature = "pg13", feature = "pg14"))]
        {
            false
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(GraphError::AclDenied {
            table: "configuration parameter graph.rls_mode".to_string(),
        })
    }
}

fn check_table_acl_mode(table_oid: u32, mode: pgrx::pg_sys::AclMode) -> GraphResult<()> {
    let role_oid = crate::catalog::current_role_oid()?;
    let acl_result = unsafe {
        // SAFETY: This runs inside a PostgreSQL backend process. `table_oid` is
        // an OID supplied by callers that already resolved catalog objects.
        // `role_oid` is resolved from PostgreSQL session state, and the ACL
        // checker does not take ownership of Rust-managed memory.
        pgrx::pg_sys::pg_class_aclcheck(pgrx::pg_sys::Oid::from_u32(table_oid), role_oid, mode)
    };

    if acl_result != pgrx::pg_sys::AclResult::ACLCHECK_OK {
        return Err(GraphError::AclDenied {
            table: format!("OID {}", table_oid),
        });
    }
    Ok(())
}
