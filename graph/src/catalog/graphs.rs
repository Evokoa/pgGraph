//! Graph identity catalog helpers.

use crate::{graph_policy, safety};
use pgrx::prelude::*;

/// SQL-visible graph metadata.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GraphMetadata {
    pub(crate) graph_id: String,
    pub(crate) graph_name: String,
    pub(crate) owner_role: pgrx::pg_sys::Oid,
    pub(crate) created_by: pgrx::pg_sys::Oid,
    pub(crate) tenant: Option<String>,
    pub(crate) namespace: Option<String>,
    pub(crate) graph_kind: String,
    pub(crate) residency: String,
    pub(crate) materialization: String,
    pub(crate) projection_mode: String,
    pub(crate) created_at: TimestampWithTimeZone,
    pub(crate) updated_at: TimestampWithTimeZone,
}

/// Creates graph catalog metadata for the current role.
///
/// # Errors
///
/// Returns [`safety::GraphError::InvalidFilter`] when policy values are invalid
/// or the identity already exists. Returns [`safety::GraphError::Internal`] for
/// SPI failures.
pub(crate) fn create_graph_metadata(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
    graph_kind: &str,
    residency: &str,
    materialization: &str,
    projection_mode: &str,
) -> safety::GraphResult<GraphMetadata> {
    validate_graph_metadata(
        graph_name,
        namespace,
        graph_kind,
        residency,
        materialization,
        projection_mode,
    )?;
    let namespace = namespace.unwrap_or(graph_policy::DEFAULT_GRAPH_NAMESPACE);
    if resolve_graph_metadata(graph_name, tenant, Some(namespace))?.is_some() {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!(
                "graph '{}' already exists for the current role, tenant, and namespace",
                graph_name
            ),
        });
    }

    Spi::connect(|client| {
        let rows = client
            .select(
                "INSERT INTO graph._graphs (
                     graph_id,
                     graph_name,
                     owner_role,
                     created_by,
                     tenant,
                     namespace,
                     graph_kind,
                     residency,
                     materialization,
                     projection_mode
                 )
                 SELECT md5(clock_timestamp()::text || random()::text || $1)::uuid,
                        $1,
                        current_user::regrole::oid,
                        current_user::regrole::oid,
                        $2,
                        $3,
                        $4,
                        $5,
                        $6,
                        $7
                 RETURNING graph_id::text,
                           graph_name,
                           owner_role,
                           created_by,
                           tenant,
                           namespace,
                           graph_kind,
                           residency,
                           materialization,
                           projection_mode,
                           created_at,
                           updated_at",
                None,
                &[
                    graph_name.into(),
                    tenant.map(str::to_string).into(),
                    namespace.into(),
                    graph_kind.into(),
                    residency.into(),
                    materialization.into(),
                    projection_mode.into(),
                ],
            )
            .map_err(|err| graph_catalog_error("create graph", err))?;
        metadata_from_first_row(rows, "created graph row missing")
    })
}

/// Updates mutable graph metadata.
///
/// # Errors
///
/// Returns [`safety::GraphError::InvalidFilter`] when the graph does not exist
/// or a supplied policy value is invalid. Returns [`safety::GraphError::Internal`]
/// for SPI failures.
pub(crate) fn update_graph_metadata(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
    graph_kind: Option<&str>,
    residency: Option<&str>,
    materialization: Option<&str>,
    projection_mode: Option<&str>,
) -> safety::GraphResult<GraphMetadata> {
    let existing = resolve_graph_metadata(graph_name, tenant, namespace)?.ok_or_else(|| {
        safety::GraphError::InvalidFilter {
            reason: format!("graph '{}' does not exist", graph_name),
        }
    })?;
    let graph_kind = graph_kind.unwrap_or(&existing.graph_kind);
    let residency = residency.unwrap_or(&existing.residency);
    let materialization = materialization.unwrap_or(&existing.materialization);
    let projection_mode = projection_mode.unwrap_or(&existing.projection_mode);
    validate_graph_metadata(
        graph_name,
        existing.namespace.as_deref(),
        graph_kind,
        residency,
        materialization,
        projection_mode,
    )?;

    Spi::connect(|client| {
        let rows = client
            .select(
                "UPDATE graph._graphs
                    SET graph_kind = $2,
                        residency = $3,
                        materialization = $4,
                        projection_mode = $5,
                        updated_at = now()
                  WHERE graph_id = $1::uuid
                  RETURNING graph_id::text,
                            graph_name,
                            owner_role,
                            created_by,
                            tenant,
                            namespace,
                            graph_kind,
                            residency,
                            materialization,
                            projection_mode,
                            created_at,
                            updated_at",
                None,
                &[
                    existing.graph_id.as_str().into(),
                    graph_kind.into(),
                    residency.into(),
                    materialization.into(),
                    projection_mode.into(),
                ],
            )
            .map_err(|err| graph_catalog_error("alter graph", err))?;
        metadata_from_first_row(rows, "updated graph row missing")
    })
}

/// Drops a non-default graph metadata row.
///
/// # Errors
///
/// Returns [`safety::GraphError::InvalidFilter`] when the graph does not exist
/// or when callers try to drop the compatibility default graph.
pub(crate) fn drop_graph_metadata(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
) -> safety::GraphResult<GraphMetadata> {
    let existing = resolve_graph_metadata(graph_name, tenant, namespace)?.ok_or_else(|| {
        safety::GraphError::InvalidFilter {
            reason: format!("graph '{}' does not exist", graph_name),
        }
    })?;
    if existing.graph_id == graph_policy::DEFAULT_GRAPH_ID_TEXT {
        return Err(safety::GraphError::InvalidFilter {
            reason: "default graph cannot be dropped".to_string(),
        });
    }

    Spi::connect(|client| {
        let rows = client
            .select(
                "DELETE FROM graph._graphs
                  WHERE graph_id = $1::uuid
                  RETURNING graph_id::text,
                            graph_name,
                            owner_role,
                            created_by,
                            tenant,
                            namespace,
                            graph_kind,
                            residency,
                            materialization,
                            projection_mode,
                            created_at,
                            updated_at",
                None,
                &[existing.graph_id.as_str().into()],
            )
            .map_err(|err| graph_catalog_error("drop graph", err))?;
        metadata_from_first_row(rows, "dropped graph row missing")
    })
}

/// Resolves the compatibility default graph metadata.
///
/// # Errors
///
/// Returns [`safety::GraphError::InvalidFilter`] if bootstrap metadata is
/// missing. Returns [`safety::GraphError::Internal`] for SPI failures.
pub(crate) fn default_graph_metadata() -> safety::GraphResult<GraphMetadata> {
    resolve_graph_by_id(graph_policy::DEFAULT_GRAPH_ID_TEXT)?.ok_or_else(|| {
        safety::GraphError::InvalidFilter {
            reason: "default graph metadata is missing".to_string(),
        }
    })
}

/// Resolves graph metadata by graph name, tenant, namespace, and current role.
///
/// # Errors
///
/// Returns [`safety::GraphError::Internal`] for SPI failures.
pub(crate) fn resolve_graph_metadata(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
) -> safety::GraphResult<Option<GraphMetadata>> {
    let namespace = namespace.unwrap_or(graph_policy::DEFAULT_GRAPH_NAMESPACE);
    Spi::connect(|client| {
        let mut rows = client
            .select(
                "SELECT graph_id::text,
                        graph_name,
                        owner_role,
                        created_by,
                        tenant,
                        namespace,
                        graph_kind,
                        residency,
                        materialization,
                        projection_mode,
                        created_at,
                        updated_at
                   FROM graph._graphs
                  WHERE graph_name = $1
                    AND COALESCE(tenant, '') = COALESCE($2, '')
                    AND COALESCE(namespace, '') = COALESCE($3, '')
                    AND owner_role = current_user::regrole::oid
                  ORDER BY created_at
                  LIMIT 1",
                None,
                &[
                    graph_name.into(),
                    tenant.map(str::to_string).into(),
                    namespace.into(),
                ],
            )
            .map_err(|err| graph_catalog_error("resolve graph", err))?;
        rows.next().map(metadata_from_row).transpose()
    })
}

/// Resolves graph metadata visible to the current role.
///
/// Role-owned graphs are visible to their owner. Global graphs are visible to
/// every role with schema access, which keeps the compatibility default graph
/// selectable without making direct catalog writes public.
///
/// # Errors
///
/// Returns [`safety::GraphError::Internal`] for SPI failures.
pub(crate) fn resolve_visible_graph_metadata(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
) -> safety::GraphResult<Option<GraphMetadata>> {
    let namespace = namespace.unwrap_or(graph_policy::DEFAULT_GRAPH_NAMESPACE);
    Spi::connect(|client| {
        let mut rows = client
            .select(
                "SELECT graph_id::text,
                        graph_name,
                        owner_role,
                        created_by,
                        tenant,
                        namespace,
                        graph_kind,
                        residency,
                        materialization,
                        projection_mode,
                        created_at,
                        updated_at
                   FROM graph._graphs
                  WHERE graph_name = $1
                    AND COALESCE(tenant, '') = COALESCE($2, '')
                    AND COALESCE(namespace, '') = COALESCE($3, '')
                    AND (owner_role = current_user::regrole::oid OR graph_kind = 'global')
                  ORDER BY CASE WHEN owner_role = current_user::regrole::oid THEN 0 ELSE 1 END,
                           created_at
                  LIMIT 1",
                None,
                &[
                    graph_name.into(),
                    tenant.map(str::to_string).into(),
                    namespace.into(),
                ],
            )
            .map_err(|err| graph_catalog_error("resolve visible graph", err))?;
        rows.next().map(metadata_from_row).transpose()
    })
}

/// Resolves graph metadata by canonical UUID text.
///
/// # Errors
///
/// Returns [`safety::GraphError::InvalidFilter`] for invalid UUID text. Returns
/// [`safety::GraphError::Internal`] for SPI failures.
pub(crate) fn resolve_graph_by_id(graph_id: &str) -> safety::GraphResult<Option<GraphMetadata>> {
    graph_policy::GraphId::parse(graph_id).map_err(|err| safety::GraphError::InvalidFilter {
        reason: err.to_string(),
    })?;
    Spi::connect(|client| {
        let mut rows = client
            .select(
                "SELECT graph_id::text,
                        graph_name,
                        owner_role,
                        created_by,
                        tenant,
                        namespace,
                        graph_kind,
                        residency,
                        materialization,
                        projection_mode,
                        created_at,
                        updated_at
                   FROM graph._graphs
                  WHERE graph_id = $1::uuid",
                None,
                &[graph_id.into()],
            )
            .map_err(|err| graph_catalog_error("resolve graph by id", err))?;
        rows.next().map(metadata_from_row).transpose()
    })
}

/// Lists graph metadata rows visible to the current role.
///
/// # Errors
///
/// Returns [`safety::GraphError::Internal`] for SPI failures.
pub(crate) fn list_graph_metadata() -> safety::GraphResult<Vec<GraphMetadata>> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT graph_id::text,
                        graph_name,
                        owner_role,
                        created_by,
                        tenant,
                        namespace,
                        graph_kind,
                        residency,
                        materialization,
                        projection_mode,
                        created_at,
                        updated_at
                   FROM graph._graphs
                  WHERE owner_role = current_user::regrole::oid
                     OR graph_kind = 'global'
                  ORDER BY COALESCE(tenant, ''), COALESCE(namespace, ''), graph_name",
                None,
                &[],
            )
            .map_err(|err| graph_catalog_error("list graphs", err))?;
        rows.map(metadata_from_row).collect()
    })
}

fn validate_graph_metadata(
    graph_name: &str,
    namespace: Option<&str>,
    graph_kind: &str,
    residency: &str,
    materialization: &str,
    projection_mode: &str,
) -> safety::GraphResult<()> {
    if graph_name.trim().is_empty() {
        return Err(safety::GraphError::InvalidFilter {
            reason: "graph_name must not be empty".to_string(),
        });
    }
    if namespace.is_some_and(|namespace| namespace.trim().is_empty()) {
        return Err(safety::GraphError::InvalidFilter {
            reason: "namespace must not be empty".to_string(),
        });
    }
    if !graph_policy::is_graph_kind(graph_kind) {
        return Err(invalid_policy("graph_kind", graph_kind));
    }
    if !graph_policy::is_residency_policy(residency) {
        return Err(invalid_policy("residency", residency));
    }
    if !graph_policy::is_materialization_policy(materialization) {
        return Err(invalid_policy("materialization", materialization));
    }
    if !graph_policy::is_projection_mode(projection_mode) {
        return Err(invalid_policy("projection_mode", projection_mode));
    }
    Ok(())
}

fn invalid_policy(field: &str, value: &str) -> safety::GraphError {
    safety::GraphError::InvalidFilter {
        reason: format!("unsupported graph {field} '{value}'"),
    }
}

fn metadata_from_first_row(
    mut rows: pgrx::spi::SpiTupleTable<'_>,
    missing: &str,
) -> safety::GraphResult<GraphMetadata> {
    rows.next()
        .map(metadata_from_row)
        .transpose()?
        .ok_or_else(|| safety::GraphError::Internal(missing.to_string()))
}

fn metadata_from_row(row: pgrx::spi::SpiHeapTupleData<'_>) -> safety::GraphResult<GraphMetadata> {
    Ok(GraphMetadata {
        graph_id: required_column(&row, 1, "graph_id")?,
        graph_name: required_column(&row, 2, "graph_name")?,
        owner_role: required_column(&row, 3, "owner_role")?,
        created_by: required_column(&row, 4, "created_by")?,
        tenant: optional_column(&row, 5, "tenant")?,
        namespace: optional_column(&row, 6, "namespace")?,
        graph_kind: required_column(&row, 7, "graph_kind")?,
        residency: required_column(&row, 8, "residency")?,
        materialization: required_column(&row, 9, "materialization")?,
        projection_mode: required_column(&row, 10, "projection_mode")?,
        created_at: required_column(&row, 11, "created_at")?,
        updated_at: required_column(&row, 12, "updated_at")?,
    })
}

fn required_column<T: FromDatum + IntoDatum>(
    row: &pgrx::spi::SpiHeapTupleData<'_>,
    ordinal: usize,
    column: &str,
) -> safety::GraphResult<T> {
    row.get::<T>(ordinal)
        .map_err(|err| safety::GraphError::Internal(format!("graph metadata read failed: {err}")))?
        .ok_or_else(|| safety::GraphError::Internal(format!("graph metadata {column} was null")))
}

fn optional_column<T: FromDatum + IntoDatum>(
    row: &pgrx::spi::SpiHeapTupleData<'_>,
    ordinal: usize,
    column: &str,
) -> safety::GraphResult<Option<T>> {
    row.get::<T>(ordinal).map_err(|err| {
        safety::GraphError::Internal(format!("graph metadata {column} read failed: {err}"))
    })
}

fn graph_catalog_error(operation: &str, err: pgrx::spi::Error) -> safety::GraphError {
    safety::GraphError::Internal(format!("{operation} catalog operation failed: {err}"))
}
