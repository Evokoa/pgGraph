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

/// Graph-level privileges granted through `graph._graph_grants`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphPrivilege {
    Read,
    Write,
    Build,
    Admin,
}

impl GraphPrivilege {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Build => "build",
            Self::Admin => "admin",
        }
    }

    fn accepted_grants(self) -> &'static [&'static str] {
        match self {
            Self::Read => &["read", "write", "admin"],
            Self::Write => &["write", "admin"],
            Self::Build => &["build", "admin"],
            Self::Admin => &["admin"],
        }
    }
}

impl TryFrom<&str> for GraphPrivilege {
    type Error = safety::GraphError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "build" => Ok(Self::Build),
            "admin" => Ok(Self::Admin),
            other => Err(safety::GraphError::InvalidFilter {
                reason: format!(
                    "unsupported graph privilege '{}'; expected read, write, build, or admin",
                    other
                ),
            }),
        }
    }
}

/// SQL-visible graph grant row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GraphGrant {
    pub(crate) graph_id: String,
    pub(crate) graph_name: String,
    pub(crate) grantee: pgrx::pg_sys::Oid,
    pub(crate) privilege: String,
    pub(crate) grantor: pgrx::pg_sys::Oid,
    pub(crate) created_at: TimestampWithTimeZone,
    pub(crate) updated_at: TimestampWithTimeZone,
}

/// SQL-visible quota policy row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GraphQuota {
    pub(crate) scope_type: String,
    pub(crate) scope_key: String,
    pub(crate) dimension: String,
    pub(crate) limit_value: i64,
    pub(crate) enforcement: String,
    pub(crate) updated_by: pgrx::pg_sys::Oid,
    pub(crate) created_at: TimestampWithTimeZone,
    pub(crate) updated_at: TimestampWithTimeZone,
}

/// SQL-visible effective quota usage row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GraphQuotaUsage {
    pub(crate) scope_type: String,
    pub(crate) scope_key: String,
    pub(crate) dimension: String,
    pub(crate) limit_value: Option<i64>,
    pub(crate) usage_value: i64,
    pub(crate) enforcement: Option<String>,
    pub(crate) exceeded: bool,
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
    enforce_named_graph_count_quota()?;

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
    if graph_has_registrations(&existing.graph_id)? {
        return Err(safety::GraphError::InvalidFilter {
            reason: format!(
                "graph '{}' still has registrations; remove graph registrations before dropping it",
                graph_name
            ),
        });
    }
    delete_graph_operational_state(&existing.graph_id)?;

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

/// Grants one graph privilege to a PostgreSQL role.
///
/// # Errors
///
/// Returns [`safety::GraphError::AclDenied`] unless the current role owns the
/// graph, has graph-admin schema privileges, or has graph `admin`. Returns
/// [`safety::GraphError::InvalidFilter`] for missing graphs, roles, or
/// unsupported privileges.
pub(crate) fn grant_graph_privilege(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
    grantee: &str,
    privilege: &str,
) -> safety::GraphResult<GraphGrant> {
    let graph =
        resolve_visible_graph_metadata(graph_name, tenant, namespace)?.ok_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: format!("graph '{}' does not exist", graph_name),
            }
        })?;
    require_graph_privilege(&graph, GraphPrivilege::Admin)?;
    let privilege = GraphPrivilege::try_from(privilege)?.as_str();
    let grantee_oid = resolve_role_oid(grantee)?;

    Spi::connect(|client| {
        let rows = client
            .select(
                "INSERT INTO graph._graph_grants (
                     graph_id, grantee, privilege, grantor
                 )
                 VALUES ($1::uuid, $2::oid, $3, current_user::regrole::oid)
                 ON CONFLICT (graph_id, grantee, privilege)
                 DO UPDATE
                    SET grantor = EXCLUDED.grantor,
                        updated_at = now()
                 RETURNING graph_id::text,
                           grantee,
                           privilege,
                           grantor,
                           created_at,
                           updated_at",
                None,
                &[
                    graph.graph_id.as_str().into(),
                    grantee_oid.into(),
                    privilege.into(),
                ],
            )
            .map_err(|err| graph_catalog_error("grant graph privilege", err))?;
        grant_from_first_row(rows, &graph, "granted graph privilege row missing")
    })
}

/// Revokes one graph privilege from a PostgreSQL role.
pub(crate) fn revoke_graph_privilege(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
    grantee: &str,
    privilege: &str,
) -> safety::GraphResult<GraphGrant> {
    let graph =
        resolve_visible_graph_metadata(graph_name, tenant, namespace)?.ok_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: format!("graph '{}' does not exist", graph_name),
            }
        })?;
    require_graph_privilege(&graph, GraphPrivilege::Admin)?;
    let privilege = GraphPrivilege::try_from(privilege)?.as_str();
    let grantee_oid = resolve_role_oid(grantee)?;

    Spi::connect(|client| {
        let rows = client
            .select(
                "DELETE FROM graph._graph_grants
                  WHERE graph_id = $1::uuid
                    AND grantee = $2::oid
                    AND privilege = $3
                  RETURNING graph_id::text,
                            grantee,
                            privilege,
                            grantor,
                            created_at,
                            updated_at",
                None,
                &[
                    graph.graph_id.as_str().into(),
                    grantee_oid.into(),
                    privilege.into(),
                ],
            )
            .map_err(|err| graph_catalog_error("revoke graph privilege", err))?;
        grant_from_first_row(rows, &graph, "graph privilege grant did not exist")
    })
}

/// Lists graph privileges visible to the current role.
pub(crate) fn graph_privileges(
    graph_name: Option<&str>,
    tenant: Option<&str>,
    namespace: Option<&str>,
) -> safety::GraphResult<Vec<GraphGrant>> {
    let graph_filter = match graph_name {
        Some(graph_name) => Some(
            resolve_visible_graph_metadata(graph_name, tenant, namespace)?.ok_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("graph '{}' does not exist", graph_name),
                }
            })?,
        ),
        None => None,
    };
    if let Some(graph) = &graph_filter {
        require_graph_privilege(graph, GraphPrivilege::Admin)?;
    }

    Spi::connect(|client| {
        let rows = if let Some(graph) = &graph_filter {
            client.select(
                "SELECT g.graph_id::text,
                        gr.graph_name,
                        g.grantee,
                        g.privilege,
                        g.grantor,
                        g.created_at,
                        g.updated_at
                   FROM graph._graph_grants g
                   JOIN graph._graphs gr ON gr.graph_id = g.graph_id
                  WHERE g.graph_id = $1::uuid
                  ORDER BY g.grantee, g.privilege",
                None,
                &[graph.graph_id.as_str().into()],
            )
        } else {
            client.select(
                "SELECT g.graph_id::text,
                        gr.graph_name,
                        g.grantee,
                        g.privilege,
                        g.grantor,
                        g.created_at,
                        g.updated_at
                   FROM graph._graph_grants g
                   JOIN graph._graphs gr ON gr.graph_id = g.graph_id
                  WHERE gr.owner_role = current_user::regrole::oid
                     OR gr.graph_kind = 'global'
                     OR EXISTS (
                         SELECT 1
                           FROM graph._graph_grants own
                          WHERE own.graph_id = gr.graph_id
                            AND own.grantee = current_user::regrole::oid
                            AND own.privilege = 'admin'
                     )
                  ORDER BY gr.graph_name, g.grantee, g.privilege",
                None,
                &[],
            )
        }
        .map_err(|err| graph_catalog_error("list graph privileges", err))?;
        rows.map(grant_from_row).collect()
    })
}

/// Transfers graph ownership to another PostgreSQL role.
pub(crate) fn transfer_graph_ownership(
    graph_name: &str,
    tenant: Option<&str>,
    namespace: Option<&str>,
    new_owner: &str,
) -> safety::GraphResult<GraphMetadata> {
    let graph =
        resolve_visible_graph_metadata(graph_name, tenant, namespace)?.ok_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: format!("graph '{}' does not exist", graph_name),
            }
        })?;
    if graph.graph_id == graph_policy::DEFAULT_GRAPH_ID_TEXT {
        return Err(safety::GraphError::InvalidFilter {
            reason: "default graph ownership cannot be transferred".to_string(),
        });
    }
    require_graph_privilege(&graph, GraphPrivilege::Admin)?;
    let new_owner_oid = resolve_role_oid(new_owner)?;

    Spi::connect(|client| {
        let rows = client
            .select(
                "UPDATE graph._graphs
                    SET owner_role = $2::oid,
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
                &[graph.graph_id.as_str().into(), new_owner_oid.into()],
            )
            .map_err(|err| graph_catalog_error("transfer graph ownership", err))?;
        metadata_from_first_row(rows, "transferred graph row missing")
    })
}

/// Upserts a graph quota policy row.
pub(crate) fn set_graph_quota(
    scope_type: &str,
    scope_key: Option<&str>,
    dimension: &str,
    limit_value: i64,
    enforcement: &str,
) -> safety::GraphResult<GraphQuota> {
    validate_quota_scope(scope_type)?;
    validate_quota_dimension(dimension)?;
    validate_quota_enforcement(enforcement)?;
    if limit_value < 0 {
        return Err(safety::GraphError::InvalidFilter {
            reason: "quota limit_value must be non-negative".to_string(),
        });
    }
    let scope_key = normalize_quota_scope_key(scope_type, scope_key)?;

    Spi::connect(|client| {
        let rows = client
            .select(
                "INSERT INTO graph._graph_quotas (
                     scope_type,
                     scope_key,
                     dimension,
                     limit_value,
                     enforcement,
                     updated_by
                 )
                 VALUES ($1, $2, $3, $4, $5, current_user::regrole::oid)
                 ON CONFLICT (scope_type, scope_key, dimension)
                 DO UPDATE
                    SET limit_value = EXCLUDED.limit_value,
                        enforcement = EXCLUDED.enforcement,
                        updated_by = EXCLUDED.updated_by,
                        updated_at = now()
                 RETURNING scope_type,
                           scope_key,
                           dimension,
                           limit_value,
                           enforcement,
                           updated_by,
                           created_at,
                           updated_at",
                None,
                &[
                    scope_type.into(),
                    scope_key.into(),
                    dimension.into(),
                    limit_value.into(),
                    enforcement.into(),
                ],
            )
            .map_err(|err| graph_catalog_error("set graph quota", err))?;
        quota_from_first_row(rows, "set graph quota row missing")
    })
}

/// Lists quota policy rows.
pub(crate) fn graph_quotas() -> safety::GraphResult<Vec<GraphQuota>> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT scope_type,
                        scope_key,
                        dimension,
                        limit_value,
                        enforcement,
                        updated_by,
                        created_at,
                        updated_at
                   FROM graph._graph_quotas
                  ORDER BY scope_type, scope_key, dimension",
                None,
                &[],
            )
            .map_err(|err| graph_catalog_error("list graph quotas", err))?;
        rows.map(quota_from_row).collect()
    })
}

/// Returns current usage for supported quota dimensions.
pub(crate) fn graph_quota_usage(
    loaded_graphs_per_backend: i64,
) -> safety::GraphResult<Vec<GraphQuotaUsage>> {
    let cluster_graphs = graph_count(None)?;
    let owner_oid = current_user_oid()?;
    let owner_key = owner_oid.to_string();
    let owner_graphs = graph_count(Some(owner_oid))?;
    let mut rows = Vec::new();

    rows.push(quota_usage_row(
        "cluster",
        "",
        "max_named_graphs",
        cluster_graphs,
    )?);
    rows.push(quota_usage_row(
        "owner",
        &owner_key,
        "max_named_graphs",
        owner_graphs,
    )?);
    rows.push(quota_usage_row(
        "cluster",
        "",
        "max_loaded_graphs_per_backend",
        loaded_graphs_per_backend,
    )?);
    rows.push(quota_usage_row(
        "owner",
        &owner_key,
        "max_loaded_graphs_per_backend",
        loaded_graphs_per_backend,
    )?);

    Ok(rows)
}

/// Enforces runtime loaded-graph quota policies before loading a graph.
pub(crate) fn enforce_loaded_graph_quota(projected_loaded_graphs: i64) -> safety::GraphResult<()> {
    let owner_oid = current_user_oid()?;
    enforce_quota_limit(
        "cluster",
        "",
        "max_loaded_graphs_per_backend",
        projected_loaded_graphs,
    )?;
    enforce_quota_limit(
        "owner",
        &owner_oid.to_string(),
        "max_loaded_graphs_per_backend",
        projected_loaded_graphs,
    )
}

fn delete_graph_operational_state(graph_id: &str) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "DELETE FROM graph._build_jobs WHERE graph_id = $1::uuid",
        &[graph_id.into()],
    )
    .map_err(|err| graph_catalog_error("delete graph build jobs", err))?;
    Spi::run_with_args(
        "DELETE FROM graph._maintenance_jobs WHERE graph_id = $1::uuid",
        &[graph_id.into()],
    )
    .map_err(|err| graph_catalog_error("delete graph maintenance jobs", err))?;
    Spi::run_with_args(
        "DELETE FROM graph._projection_generations WHERE graph_id = $1::uuid",
        &[graph_id.into()],
    )
    .map_err(|err| graph_catalog_error("delete graph projection generations", err))?;
    Ok(())
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

/// Returns the session-selected graph, or the compatibility default graph.
///
/// # Errors
///
/// Returns [`safety::GraphError::InvalidFilter`] if the selected graph id no
/// longer resolves. Returns [`safety::GraphError::Internal`] for SPI failures.
pub(crate) fn selected_or_default_graph_metadata() -> safety::GraphResult<GraphMetadata> {
    match selected_graph_id()? {
        Some(graph_id) => resolve_visible_graph_by_id(&graph_id)?.ok_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: "selected graph metadata is missing".to_string(),
            }
        }),
        None => default_graph_metadata(),
    }
}

/// Stores a session-local graph selection by graph id.
///
/// # Errors
///
/// Returns [`safety::GraphError::Internal`] when PostgreSQL rejects the session
/// setting write.
pub(crate) fn set_selected_graph_id(graph_id: &str) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "SELECT set_config('graph.current_graph_id', $1, false)",
        &[graph_id.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("current graph setting write failed: {err}"))
    })
}

fn selected_graph_id() -> safety::GraphResult<Option<String>> {
    Spi::get_one::<String>("SELECT current_setting('graph.current_graph_id', true)")
        .map_err(|err| {
            safety::GraphError::Internal(format!("current graph setting read failed: {err}"))
        })
        .map(|value| value.filter(|value| !value.trim().is_empty()))
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
                    AND (
                        owner_role = current_user::regrole::oid
                        OR graph_kind = 'global'
                        OR EXISTS (
                            SELECT 1
                              FROM graph._graph_grants gg
                             WHERE gg.graph_id = graph._graphs.graph_id
                               AND gg.grantee = current_user::regrole::oid
                        )
                    )
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

fn resolve_visible_graph_by_id(graph_id: &str) -> safety::GraphResult<Option<GraphMetadata>> {
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
                  WHERE graph_id = $1::uuid
                    AND (
                        owner_role = current_user::regrole::oid
                        OR graph_kind = 'global'
                        OR EXISTS (
                            SELECT 1
                              FROM graph._graph_grants gg
                             WHERE gg.graph_id = graph._graphs.graph_id
                               AND gg.grantee = current_user::regrole::oid
                        )
                    )
                  LIMIT 1",
                None,
                &[graph_id.into()],
            )
            .map_err(|err| graph_catalog_error("resolve visible graph by id", err))?;
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
                     OR EXISTS (
                         SELECT 1
                           FROM graph._graph_grants gg
                          WHERE gg.graph_id = graph._graphs.graph_id
                            AND gg.grantee = current_user::regrole::oid
                     )
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

/// Ensures the current role has the requested graph-level privilege.
///
/// Owners and graph schema admins have all graph privileges. `admin` grants all
/// graph privileges, while `write` grants query/read access for mapped write
/// workflows that also need to inspect source rows.
pub(crate) fn require_graph_privilege(
    graph: &GraphMetadata,
    privilege: GraphPrivilege,
) -> safety::GraphResult<()> {
    if has_graph_privilege(&graph.graph_id, privilege)? {
        Ok(())
    } else {
        Err(safety::GraphError::AclDenied {
            table: format!("graph {}", graph.graph_name),
        })
    }
}

fn has_graph_privilege(graph_id: &str, privilege: GraphPrivilege) -> safety::GraphResult<bool> {
    let accepted = privilege
        .accepted_grants()
        .iter()
        .map(|grant| (*grant).to_string())
        .collect::<Vec<_>>();
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT
                COALESCE((SELECT rolsuper FROM pg_roles WHERE rolname = current_user), false)
                OR has_schema_privilege(current_user, 'graph', 'CREATE')
                OR EXISTS (
                    SELECT 1
                      FROM graph._graphs
                     WHERE graph_id = $1::uuid
                       AND (
                           owner_role = current_user::regrole::oid
                           OR (graph_kind = 'global' AND 'read' = ANY($2::text[]))
                       )
                )
                OR EXISTS (
                    SELECT 1
                      FROM graph._graph_grants
                     WHERE graph_id = $1::uuid
                       AND grantee = current_user::regrole::oid
                       AND privilege = ANY($2::text[])
                )",
            None,
            &[graph_id.into(), accepted.into()],
        )?;
        Ok::<_, pgrx::spi::Error>(rows.first().get::<bool>(1).ok().flatten().unwrap_or(false))
    })
    .map_err(|err| graph_catalog_error("check graph privilege", err))
}

fn resolve_role_oid(role_name: &str) -> safety::GraphResult<pgrx::pg_sys::Oid> {
    if role_name.trim().is_empty() {
        return Err(safety::GraphError::InvalidFilter {
            reason: "role name must not be empty".to_string(),
        });
    }
    Spi::get_one_with_args::<pgrx::pg_sys::Oid>("SELECT to_regrole($1)::oid", &[role_name.into()])
        .map_err(|err| graph_catalog_error("resolve role", err))?
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: format!("role '{}' does not exist", role_name),
        })
}

fn grant_from_first_row(
    mut rows: pgrx::spi::SpiTupleTable<'_>,
    graph: &GraphMetadata,
    missing: &str,
) -> safety::GraphResult<GraphGrant> {
    rows.next()
        .map(|row| grant_from_row_with_graph(row, graph))
        .transpose()?
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: missing.to_string(),
        })
}

fn grant_from_row_with_graph(
    row: pgrx::spi::SpiHeapTupleData<'_>,
    graph: &GraphMetadata,
) -> safety::GraphResult<GraphGrant> {
    Ok(GraphGrant {
        graph_id: required_column(&row, 1, "graph_id")?,
        graph_name: graph.graph_name.clone(),
        grantee: required_column(&row, 2, "grantee")?,
        privilege: required_column(&row, 3, "privilege")?,
        grantor: required_column(&row, 4, "grantor")?,
        created_at: required_column(&row, 5, "created_at")?,
        updated_at: required_column(&row, 6, "updated_at")?,
    })
}

fn grant_from_row(row: pgrx::spi::SpiHeapTupleData<'_>) -> safety::GraphResult<GraphGrant> {
    Ok(GraphGrant {
        graph_id: required_column(&row, 1, "graph_id")?,
        graph_name: required_column(&row, 2, "graph_name")?,
        grantee: required_column(&row, 3, "grantee")?,
        privilege: required_column(&row, 4, "privilege")?,
        grantor: required_column(&row, 5, "grantor")?,
        created_at: required_column(&row, 6, "created_at")?,
        updated_at: required_column(&row, 7, "updated_at")?,
    })
}

fn enforce_named_graph_count_quota() -> safety::GraphResult<()> {
    let owner_oid = current_user_oid()?;
    let cluster_usage = graph_count(None)?.saturating_add(1);
    enforce_quota_limit("cluster", "", "max_named_graphs", cluster_usage)?;
    enforce_quota_limit(
        "owner",
        &owner_oid.to_string(),
        "max_named_graphs",
        graph_count(Some(owner_oid))?.saturating_add(1),
    )
}

fn enforce_quota_limit(
    scope_type: &str,
    scope_key: &str,
    dimension: &str,
    projected_usage: i64,
) -> safety::GraphResult<()> {
    if let Some(policy) = quota_policy(scope_type, scope_key, dimension)? {
        if policy.enforcement == "hard" && projected_usage > policy.limit_value {
            return Err(safety::GraphError::InvalidFilter {
                reason: format!(
                    "quota exceeded for {scope_type}:{scope_key} {dimension}: projected {projected_usage}, limit {}",
                    policy.limit_value
                ),
            });
        } else if policy.enforcement == "warn" && projected_usage > policy.limit_value {
            pgrx::warning!(
                "graph: quota warning for {}:{} {}: projected {}, limit {}",
                scope_type,
                scope_key,
                dimension,
                projected_usage,
                policy.limit_value
            );
        }
    }
    Ok(())
}

fn quota_usage_row(
    scope_type: &str,
    scope_key: &str,
    dimension: &str,
    usage_value: i64,
) -> safety::GraphResult<GraphQuotaUsage> {
    let policy = quota_policy(scope_type, scope_key, dimension)?;
    let (limit_value, enforcement) = policy
        .map(|policy| (Some(policy.limit_value), Some(policy.enforcement)))
        .unwrap_or((None, None));
    let exceeded = limit_value.is_some_and(|limit| usage_value > limit);
    Ok(GraphQuotaUsage {
        scope_type: scope_type.to_string(),
        scope_key: scope_key.to_string(),
        dimension: dimension.to_string(),
        limit_value,
        usage_value,
        enforcement,
        exceeded,
    })
}

fn quota_policy(
    scope_type: &str,
    scope_key: &str,
    dimension: &str,
) -> safety::GraphResult<Option<GraphQuota>> {
    Spi::connect(|client| {
        let mut rows = client
            .select(
                "SELECT scope_type,
                        scope_key,
                        dimension,
                        limit_value,
                        enforcement,
                        updated_by,
                        created_at,
                        updated_at
                   FROM graph._graph_quotas
                  WHERE scope_type = $1
                    AND scope_key = $2
                    AND dimension = $3
                  LIMIT 1",
                None,
                &[scope_type.into(), scope_key.into(), dimension.into()],
            )
            .map_err(|err| graph_catalog_error("read graph quota", err))?;
        rows.next().map(quota_from_row).transpose()
    })
}

fn graph_count(owner_oid: Option<pgrx::pg_sys::Oid>) -> safety::GraphResult<i64> {
    match owner_oid {
        Some(owner_oid) => Spi::get_one_with_args::<i64>(
            "SELECT count(*)::bigint
               FROM graph._graphs
              WHERE graph_id <> $1::uuid
                AND owner_role = $2::oid",
            &[graph_policy::DEFAULT_GRAPH_ID_TEXT.into(), owner_oid.into()],
        ),
        None => Spi::get_one_with_args::<i64>(
            "SELECT count(*)::bigint
               FROM graph._graphs
              WHERE graph_id <> $1::uuid",
            &[graph_policy::DEFAULT_GRAPH_ID_TEXT.into()],
        ),
    }
    .map_err(|err| graph_catalog_error("count graphs for quota", err))?
    .ok_or_else(|| safety::GraphError::Internal("graph quota count returned null".to_string()))
}

fn current_user_oid() -> safety::GraphResult<pgrx::pg_sys::Oid> {
    Spi::get_one::<pgrx::pg_sys::Oid>("SELECT current_user::regrole::oid")
        .map_err(|err| graph_catalog_error("read current user oid", err))?
        .ok_or_else(|| safety::GraphError::Internal("current user oid was null".to_string()))
}

fn normalize_quota_scope_key(
    scope_type: &str,
    scope_key: Option<&str>,
) -> safety::GraphResult<String> {
    match scope_type {
        "cluster" => Ok(String::new()),
        "owner" => {
            Ok(
                resolve_role_oid(scope_key.ok_or_else(|| safety::GraphError::InvalidFilter {
                    reason: "owner quota scope requires scope_key role name".to_string(),
                })?)?
                .to_string(),
            )
        }
        _ => {
            let scope_key = scope_key.unwrap_or_default().trim();
            if scope_key.is_empty() {
                return Err(safety::GraphError::InvalidFilter {
                    reason: format!("{scope_type} quota scope requires a non-empty scope_key"),
                });
            }
            Ok(scope_key.to_string())
        }
    }
}

fn validate_quota_scope(scope_type: &str) -> safety::GraphResult<()> {
    if matches!(
        scope_type,
        "cluster" | "tenant" | "owner" | "namespace" | "graph"
    ) {
        Ok(())
    } else {
        Err(safety::GraphError::InvalidFilter {
            reason: format!("unsupported quota scope_type '{scope_type}'"),
        })
    }
}

fn validate_quota_dimension(dimension: &str) -> safety::GraphResult<()> {
    if matches!(
        dimension,
        "max_named_graphs"
            | "max_physical_graphs"
            | "max_graph_jobs"
            | "max_sync_lag_rows"
            | "max_artifact_storage_bytes"
            | "max_build_memory_mb"
            | "max_loaded_graphs_per_backend"
            | "max_compaction_work"
    ) {
        Ok(())
    } else {
        Err(safety::GraphError::InvalidFilter {
            reason: format!("unsupported quota dimension '{dimension}'"),
        })
    }
}

fn validate_quota_enforcement(enforcement: &str) -> safety::GraphResult<()> {
    if matches!(enforcement, "hard" | "warn") {
        Ok(())
    } else {
        Err(safety::GraphError::InvalidFilter {
            reason: format!("unsupported quota enforcement '{enforcement}'"),
        })
    }
}

fn quota_from_first_row(
    mut rows: pgrx::spi::SpiTupleTable<'_>,
    missing: &str,
) -> safety::GraphResult<GraphQuota> {
    rows.next()
        .map(quota_from_row)
        .transpose()?
        .ok_or_else(|| safety::GraphError::Internal(missing.to_string()))
}

fn quota_from_row(row: pgrx::spi::SpiHeapTupleData<'_>) -> safety::GraphResult<GraphQuota> {
    Ok(GraphQuota {
        scope_type: required_column(&row, 1, "scope_type")?,
        scope_key: required_column(&row, 2, "scope_key")?,
        dimension: required_column(&row, 3, "dimension")?,
        limit_value: required_column(&row, 4, "limit_value")?,
        enforcement: required_column(&row, 5, "enforcement")?,
        updated_by: required_column(&row, 6, "updated_by")?,
        created_at: required_column(&row, 7, "created_at")?,
        updated_at: required_column(&row, 8, "updated_at")?,
    })
}

fn graph_has_registrations(graph_id: &str) -> safety::GraphResult<bool> {
    Spi::connect(|client| {
        let result = client
            .select(
                "SELECT EXISTS (
                    SELECT 1 FROM graph._registered_tables WHERE graph_id = $1::uuid
                 ) OR EXISTS (
                    SELECT 1 FROM graph._registered_edges WHERE graph_id = $1::uuid
                 ) OR EXISTS (
                    SELECT 1 FROM graph._registered_filter_columns WHERE graph_id = $1::uuid
                 )",
                None,
                &[graph_id.into()],
            )
            .map_err(|err| graph_catalog_error("check graph registrations", err))?;
        result
            .first()
            .get::<bool>(1)
            .map_err(|err| {
                safety::GraphError::Internal(format!("graph registration check failed: {err}"))
            })?
            .ok_or_else(|| {
                safety::GraphError::Internal("graph registration check returned null".to_string())
            })
    })
}

fn graph_catalog_error(operation: &str, err: pgrx::spi::Error) -> safety::GraphError {
    safety::GraphError::Internal(format!("{operation} catalog operation failed: {err}"))
}
