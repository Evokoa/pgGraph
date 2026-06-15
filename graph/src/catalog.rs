//! SPI-backed graph catalog access, registration, and validation helpers.

mod graphs;
mod read;
mod validate;
mod write;

pub(crate) use crate::builder::split_catalog_columns;
pub(crate) use graphs::{
    create_graph_metadata, drop_graph_metadata, enforce_loaded_graph_quota, grant_graph_privilege,
    graph_privileges, graph_quota_usage, graph_quotas, list_graph_metadata,
    require_graph_privilege, resolve_visible_graph_metadata, revoke_graph_privilege,
    selected_or_default_graph_metadata, set_graph_quota, set_selected_graph_id,
    transfer_graph_ownership, update_graph_metadata, GraphGrant, GraphMetadata, GraphPrivilege,
    GraphQuota, GraphQuotaUsage,
};
pub(crate) use read::{catalog_fingerprint, current_catalog_state, read_catalog};
#[cfg(feature = "pg_test")]
pub(crate) use validate::validate_numeric_column;
pub(crate) use validate::{
    estimated_table_rows, primary_key_expr, regclass_text, sql_table_name_from_catalog,
    table_oid_from_name, validate_column_exists, validate_edge_endpoint_columns,
    validate_filter_column_type, validate_registered_table,
};
pub(crate) use write::{
    insert_registered_edge, insert_registered_edge_for_graph, insert_registered_table,
    insert_registered_table_for_graph, RegisteredEdgeInsert,
};
