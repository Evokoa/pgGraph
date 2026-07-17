//! # ACL — Access Control List pre-flight checks
//!
//! Query helpers call `check_table_acl()` before reading source-table rows or
//! returning hydrated data. Write helpers call `check_table_insert_acl()`,
//! `check_table_update_acl()`, or `check_table_delete_acl()` before modifying
//! mapped rows.
//!
//! `table_has_row_security()` backs the build-time RLS topology boundary
//! gate: topology-read functions (`graph.traverse()`, `shortest_path()`,
//! the component functions) return coordinates and adjacency from the
//! builder-scoped graph artifact under table-level ACL only, not row-level
//! security, so `graph.build()` refuses tables with row security enabled
//! unless `graph.allow_rls_tables` is explicitly set.
//!
//! See: `docs/contributor_guide/safety-security.mdx`
//! See: `docs/user_guide/administration-and-security.mdx`

use crate::safety::{GraphError, GraphResult};

/// Check if the current user has SELECT privilege on the given table OID.
///
/// Uses PostgreSQL's native relation ACL checker.
///
/// # Errors
/// Returns `GraphError::AclDenied` if the user lacks SELECT on the table.
pub fn check_table_acl(table_oid: u32) -> GraphResult<()> {
    check_table_acl_mode(table_oid, pgrx::pg_sys::ACL_SELECT as pgrx::pg_sys::AclMode)
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

/// Returns `true` when the given table has row-level security enabled
/// (`relrowsecurity`) or forced even for the table owner
/// (`relforcerowsecurity`).
///
/// # Errors
///
/// Returns `GraphError::Internal` if the table's `pg_class` row cannot be
/// read, which should not happen for an OID already resolved through
/// registration.
pub fn table_has_row_security(table_oid: u32) -> GraphResult<bool> {
    pgrx::Spi::get_one_with_args::<bool>(
        "SELECT relrowsecurity OR relforcerowsecurity
         FROM pg_catalog.pg_class
         WHERE oid = $1",
        &[pgrx::pg_sys::Oid::from_u32(table_oid).into()],
    )
    .map_err(|err| GraphError::Internal(format!("row-security lookup failed: {err}")))?
    .ok_or_else(|| GraphError::Internal(format!("table OID {table_oid} has no pg_class row")))
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
