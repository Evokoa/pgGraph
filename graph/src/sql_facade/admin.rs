use super::*;

pub(super) fn check_enabled() {
    if !config::ENABLED.get() {
        safety::GraphError::Disabled.report();
    }
}

#[pg_extern(schema = "graph")]
fn test_enabled() -> bool {
    config::ENABLED.get()
}

/// Create graph metadata for the current role.
#[pg_extern(schema = "graph")]
fn create_graph(
    graph_name: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
    graph_kind: default!(&str, "'user'"),
    residency: default!(&str, "'hot'"),
    materialization: default!(&str, "'shared'"),
    projection_mode: default!(&str, "'csr_readonly'"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("create_graph()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let metadata = catalog::create_graph_metadata(
            graph_name,
            tenant,
            namespace,
            graph_kind,
            residency,
            materialization,
            projection_mode,
        )
        .unwrap_or_else(|err| err.report());
        graph_metadata_iterator(vec![metadata])
    })
}

/// Alter graph metadata for the current role.
#[pg_extern(schema = "graph")]
fn alter_graph(
    graph_name: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
    graph_kind: default!(Option<&str>, "NULL"),
    residency: default!(Option<&str>, "NULL"),
    materialization: default!(Option<&str>, "NULL"),
    projection_mode: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("alter_graph()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let metadata = catalog::update_graph_metadata(
            graph_name,
            tenant,
            namespace,
            graph_kind,
            residency,
            materialization,
            projection_mode,
        )
        .unwrap_or_else(|err| err.report());
        graph_metadata_iterator(vec![metadata])
    })
}

/// Drop graph metadata for the current role.
#[pg_extern(schema = "graph")]
fn drop_graph(
    graph_name: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("drop_graph()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let metadata = catalog::drop_graph_metadata(graph_name, tenant, namespace)
            .unwrap_or_else(|err| err.report());
        persistence::remove_graph_artifacts_for(&metadata.graph_id).unwrap_or_else(|err| {
            safety::GraphError::Internal(format!(
                "graph '{}' was dropped but artifact cleanup failed: {}",
                metadata.graph_name, err
            ))
            .report()
        });
        graph_metadata_iterator(vec![metadata])
    })
}

/// List graph metadata visible to the current role.
#[pg_extern(schema = "graph")]
fn list_graphs() -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("list_graphs()", || {
        let rows = catalog::list_graph_metadata().unwrap_or_else(|err| err.report());
        graph_metadata_iterator(rows)
    })
}

/// Return the session-selected graph metadata.
#[pg_extern(schema = "graph")]
fn current_graph() -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("current_graph()", || {
        let metadata =
            catalog::selected_or_default_graph_metadata().unwrap_or_else(|err| err.report());
        graph_metadata_iterator(vec![metadata])
    })
}

/// Select graph metadata for later graph-scoped calls.
#[pg_extern(schema = "graph")]
fn set_current_graph(
    graph_name: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("set_current_graph()", || {
        let metadata = catalog::resolve_visible_graph_metadata(graph_name, tenant, namespace)
            .unwrap_or_else(|err| err.report())
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("graph '{}' does not exist", graph_name),
                }
                .report()
            });
        catalog::set_selected_graph_id(&metadata.graph_id).unwrap_or_else(|err| err.report());
        graph_metadata_iterator(vec![metadata])
    })
}

#[pg_extern(schema = "graph")]
fn grant_graph(
    graph_name: &str,
    grantee: &str,
    privilege: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(grantee, pgrx::pg_sys::Oid),
        name!(privilege, String),
        name!(grantor, pgrx::pg_sys::Oid),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("grant_graph()", || {
        let grant =
            catalog::grant_graph_privilege(graph_name, tenant, namespace, grantee, privilege)
                .unwrap_or_else(|err| err.report());
        graph_grant_iterator(vec![grant])
    })
}

#[pg_extern(schema = "graph")]
fn revoke_graph(
    graph_name: &str,
    grantee: &str,
    privilege: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(grantee, pgrx::pg_sys::Oid),
        name!(privilege, String),
        name!(grantor, pgrx::pg_sys::Oid),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("revoke_graph()", || {
        let grant =
            catalog::revoke_graph_privilege(graph_name, tenant, namespace, grantee, privilege)
                .unwrap_or_else(|err| err.report());
        graph_grant_iterator(vec![grant])
    })
}

#[pg_extern(schema = "graph")]
fn graph_privileges(
    graph_name: default!(Option<&str>, "NULL"),
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(grantee, pgrx::pg_sys::Oid),
        name!(privilege, String),
        name!(grantor, pgrx::pg_sys::Oid),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("graph_privileges()", || {
        let grants = catalog::graph_privileges(graph_name, tenant, namespace)
            .unwrap_or_else(|err| err.report());
        graph_grant_iterator(grants)
    })
}

#[pg_extern(schema = "graph")]
fn transfer_graph_ownership(
    graph_name: &str,
    new_owner: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("transfer_graph_ownership()", || {
        let metadata = catalog::transfer_graph_ownership(graph_name, tenant, namespace, new_owner)
            .unwrap_or_else(|err| err.report());
        graph_metadata_iterator(vec![metadata])
    })
}

#[pg_extern(schema = "graph")]
fn set_graph_residency(
    graph_name: &str,
    residency: &str,
    tenant: default!(Option<&str>, "NULL"),
    namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("set_graph_residency()", || {
        let graph = resolve_graph_for_registration(graph_name, tenant, namespace);
        catalog::require_graph_privilege(&graph, catalog::GraphPrivilege::Admin)
            .unwrap_or_else(|err| err.report());
        let metadata = catalog::update_graph_metadata(
            graph_name,
            tenant,
            namespace,
            None,
            Some(residency),
            None,
            None,
        )
        .unwrap_or_else(|err| err.report());
        graph_metadata_iterator(vec![metadata])
    })
}

#[pg_extern(schema = "graph")]
fn set_graph_quota(
    scope_type: &str,
    dimension: &str,
    limit_value: i64,
    scope_key: default!(Option<&str>, "NULL"),
    enforcement: default!(&str, "'hard'"),
) -> TableIterator<
    'static,
    (
        name!(scope_type, String),
        name!(scope_key, String),
        name!(dimension, String),
        name!(limit_value, i64),
        name!(enforcement, String),
        name!(updated_by, pgrx::pg_sys::Oid),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("set_graph_quota()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let quota =
            catalog::set_graph_quota(scope_type, scope_key, dimension, limit_value, enforcement)
                .unwrap_or_else(|err| err.report());
        graph_quota_iterator(vec![quota])
    })
}

#[pg_extern(schema = "graph")]
fn graph_quotas() -> TableIterator<
    'static,
    (
        name!(scope_type, String),
        name!(scope_key, String),
        name!(dimension, String),
        name!(limit_value, i64),
        name!(enforcement, String),
        name!(updated_by, pgrx::pg_sys::Oid),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    with_panic_boundary("graph_quotas()", || {
        let quotas = catalog::graph_quotas().unwrap_or_else(|err| err.report());
        graph_quota_iterator(quotas)
    })
}

#[pg_extern(schema = "graph")]
fn graph_quota_usage() -> TableIterator<
    'static,
    (
        name!(scope_type, String),
        name!(scope_key, String),
        name!(dimension, String),
        name!(limit_value, Option<i64>),
        name!(usage_value, i64),
        name!(enforcement, Option<String>),
        name!(exceeded, bool),
    ),
> {
    with_panic_boundary("graph_quota_usage()", || {
        let loaded_graphs_per_backend =
            i64::from(crate::runtime_state::loaded_graph_id().is_some());
        let usage = catalog::graph_quota_usage(loaded_graphs_per_backend)
            .unwrap_or_else(|err| err.report());
        graph_quota_usage_iterator(usage)
    })
}

pub(crate) fn check_enabled_result() -> safety::GraphResult<()> {
    if config::ENABLED.get() {
        Ok(())
    } else {
        Err(safety::GraphError::Disabled)
    }
}

fn graph_metadata_iterator(
    rows: Vec<catalog::GraphMetadata>,
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(owner_role, pgrx::pg_sys::Oid),
        name!(created_by, pgrx::pg_sys::Oid),
        name!(tenant, Option<String>),
        name!(namespace, Option<String>),
        name!(graph_kind, String),
        name!(residency, String),
        name!(materialization, String),
        name!(projection_mode, String),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    TableIterator::new(rows.into_iter().map(|row| {
        (
            row.graph_id,
            row.graph_name,
            row.owner_role,
            row.created_by,
            row.tenant,
            row.namespace,
            row.graph_kind,
            row.residency,
            row.materialization,
            row.projection_mode,
            row.created_at,
            row.updated_at,
        )
    }))
}

fn graph_grant_iterator(
    rows: Vec<catalog::GraphGrant>,
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(grantee, pgrx::pg_sys::Oid),
        name!(privilege, String),
        name!(grantor, pgrx::pg_sys::Oid),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    TableIterator::new(rows.into_iter().map(|row| {
        (
            row.graph_id,
            row.graph_name,
            row.grantee,
            row.privilege,
            row.grantor,
            row.created_at,
            row.updated_at,
        )
    }))
}

fn graph_quota_iterator(
    rows: Vec<catalog::GraphQuota>,
) -> TableIterator<
    'static,
    (
        name!(scope_type, String),
        name!(scope_key, String),
        name!(dimension, String),
        name!(limit_value, i64),
        name!(enforcement, String),
        name!(updated_by, pgrx::pg_sys::Oid),
        name!(created_at, TimestampWithTimeZone),
        name!(updated_at, TimestampWithTimeZone),
    ),
> {
    TableIterator::new(rows.into_iter().map(|row| {
        (
            row.scope_type,
            row.scope_key,
            row.dimension,
            row.limit_value,
            row.enforcement,
            row.updated_by,
            row.created_at,
            row.updated_at,
        )
    }))
}

fn graph_quota_usage_iterator(
    rows: Vec<catalog::GraphQuotaUsage>,
) -> TableIterator<
    'static,
    (
        name!(scope_type, String),
        name!(scope_key, String),
        name!(dimension, String),
        name!(limit_value, Option<i64>),
        name!(usage_value, i64),
        name!(enforcement, Option<String>),
        name!(exceeded, bool),
    ),
> {
    TableIterator::new(rows.into_iter().map(|row| {
        (
            row.scope_type,
            row.scope_key,
            row.dimension,
            row.limit_value,
            row.usage_value,
            row.enforcement,
            row.exceeded,
        )
    }))
}

fn resolve_graph_for_registration(
    graph_name: &str,
    graph_tenant: Option<&str>,
    graph_namespace: Option<&str>,
) -> catalog::GraphMetadata {
    catalog::resolve_visible_graph_metadata(graph_name, graph_tenant, graph_namespace)
        .unwrap_or_else(|err| err.report())
        .unwrap_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: format!("graph '{}' does not exist", graph_name),
            }
            .report()
        })
}

pub(super) fn require_graph_admin_result() -> safety::GraphResult<()> {
    let allowed = Spi::connect(|client| {
        let result = client.select(
            "SELECT
                COALESCE((SELECT rolsuper FROM pg_roles WHERE rolname = current_user), false)
                OR has_schema_privilege(current_user, 'graph', 'CREATE')",
            None,
            &[],
        )?;
        Ok::<_, pgrx::spi::SpiError>(
            result
                .first()
                .get::<bool>(1)
                .ok()
                .flatten()
                .unwrap_or(false),
        )
    })
    .map_err(|err| {
        safety::GraphError::Internal(format!("graph admin privilege check failed: {}", err))
    })?;

    if allowed {
        Ok(())
    } else {
        Err(safety::GraphError::AclDenied {
            table: "graph schema admin".to_string(),
        })
    }
}

fn require_selected_graph_build_result() -> safety::GraphResult<()> {
    let graph = catalog::selected_or_default_graph_metadata()?;
    catalog::require_graph_privilege(&graph, catalog::GraphPrivilege::Build)
}

fn require_graph_build_result(graph: &catalog::GraphMetadata) -> safety::GraphResult<()> {
    catalog::require_graph_privilege(graph, catalog::GraphPrivilege::Build)
}

fn registered_table_name(table_oid: u32) -> safety::GraphResult<Option<String>> {
    let graph = catalog::selected_or_default_graph_metadata()?;
    registered_table_name_for_graph(&graph.graph_id, table_oid)
}

fn registered_table_name_for_graph(
    graph_id: &str,
    table_oid: u32,
) -> safety::GraphResult<Option<String>> {
    Spi::connect(|client| {
        let table_oid = pgrx::pg_sys::Oid::from_u32(table_oid);
        let mut result = client
            .select(
                "SELECT table_name
                    FROM graph._registered_tables
                    WHERE graph_id = $1::uuid
                      AND (to_regclass(table_name) = $2::oid
                       OR (
                           position('.' in table_name) = 0
                           AND EXISTS (
                               SELECT 1
                               FROM pg_class
                               WHERE oid = $2::oid
                                 AND relname = table_name
                           )
                       ))
                    ORDER BY table_name
                    LIMIT 1",
                None,
                &[graph_id.into(), table_oid.into()],
            )
            .map_err(|err| {
                safety::GraphError::Internal(format!("registered table lookup failed: {}", err))
            })?;
        if let Some(row) = result.next() {
            return row.get::<String>(1).map_err(|err| {
                safety::GraphError::Internal(format!(
                    "registered table lookup read failed: {}",
                    err
                ))
            });
        }
        Ok(None)
    })
}

pub(super) fn with_panic_boundary<T>(_context: &str, f: impl FnOnce() -> T) -> T {
    // pgrx already installs the real panic boundary around #[pg_extern] calls.
    // Catching inside SPI/user-code paths can accidentally intercept pgrx
    // ErrorReport panics and either erase the SQLSTATE or abort the backend, so
    // this helper is deliberately just a uniform call site.
    f()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledMaintenanceInputs {
    pub(crate) pending_sync_rows: i64,
    pub(crate) disabled_trigger_count: i32,
    pub(crate) edge_buffer_used: i32,
    pub(crate) needs_vacuum: bool,
    pub(crate) needs_rebuild: bool,
    pub(crate) read_only: bool,
    pub(crate) compaction_recommended: bool,
}

impl From<&crate::types::EngineStatus> for ScheduledMaintenanceInputs {
    fn from(status: &crate::types::EngineStatus) -> Self {
        Self {
            pending_sync_rows: status.pending_sync_rows,
            disabled_trigger_count: status.disabled_trigger_count,
            edge_buffer_used: status.edge_buffer_used,
            needs_vacuum: status.needs_vacuum,
            needs_rebuild: status.needs_rebuild,
            read_only: status.read_only,
            compaction_recommended: status.compaction_recommended,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledMaintenanceDecision {
    pub(crate) apply_sync: bool,
    pub(crate) start_maintenance: bool,
}

pub(crate) fn scheduled_maintenance_decision(
    inputs: ScheduledMaintenanceInputs,
) -> ScheduledMaintenanceDecision {
    let apply_sync = inputs.pending_sync_rows > 0
        && inputs.disabled_trigger_count == 0
        && !inputs.needs_rebuild
        && !inputs.read_only;
    let start_maintenance = inputs.read_only
        || inputs.needs_rebuild
        || inputs.needs_vacuum
        || inputs.edge_buffer_used > 0
        || inputs.compaction_recommended;

    ScheduledMaintenanceDecision {
        apply_sync,
        start_maintenance,
    }
}

#[cfg(test)]
mod scheduled_maintenance_tests {
    use super::{
        scheduled_maintenance_decision, ScheduledMaintenanceDecision, ScheduledMaintenanceInputs,
    };

    #[test]
    fn scheduled_maintenance_decision_recommends_apply_when_trigger_sync_is_safe() {
        let decision = scheduled_maintenance_decision(ScheduledMaintenanceInputs {
            pending_sync_rows: 2,
            disabled_trigger_count: 0,
            edge_buffer_used: 0,
            needs_vacuum: false,
            needs_rebuild: false,
            read_only: false,
            compaction_recommended: false,
        });

        assert_eq!(
            decision,
            ScheduledMaintenanceDecision {
                apply_sync: true,
                start_maintenance: false,
            }
        );
    }

    #[test]
    fn scheduled_maintenance_decision_blocks_apply_for_rebuild_or_read_only() {
        for mut inputs in [
            ScheduledMaintenanceInputs {
                pending_sync_rows: 2,
                disabled_trigger_count: 1,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 2,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: true,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 2,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: true,
                compaction_recommended: false,
            },
        ] {
            let decision = scheduled_maintenance_decision(inputs);
            assert!(!decision.apply_sync);

            inputs.pending_sync_rows = 0;
            let no_pending_decision = scheduled_maintenance_decision(inputs);
            assert!(!no_pending_decision.apply_sync);
        }
    }

    #[test]
    fn scheduled_maintenance_decision_starts_for_vacuum_overlay_rebuild_or_read_only() {
        for inputs in [
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 1,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: true,
                needs_rebuild: false,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: true,
                read_only: false,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: true,
                compaction_recommended: false,
            },
            ScheduledMaintenanceInputs {
                pending_sync_rows: 0,
                disabled_trigger_count: 0,
                edge_buffer_used: 0,
                needs_vacuum: false,
                needs_rebuild: false,
                read_only: false,
                compaction_recommended: true,
            },
        ] {
            let decision = scheduled_maintenance_decision(inputs);
            assert!(decision.start_maintenance);
        }
    }
}

/// Return current engine status.
///
/// See: `docs/user_guide/api-reference.mdx`
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn status() -> TableIterator<
    'static,
    (
        name!(node_count, i32),
        name!(edge_count, i32),
        name!(memory_used_mb, f64),
        name!(memory_limit_mb, i32),
        name!(sync_mode, String),
        name!(sync_status, String),
        name!(last_build, Option<TimestampWithTimeZone>),
        name!(last_vacuum, Option<TimestampWithTimeZone>),
        name!(edge_types, Vec<String>),
        name!(edge_buffer_used, i32),
        name!(has_unidirectional_edges, bool),
        name!(schema_status, String),
        name!(sync_lag, i64),
        name!(pending_edge_deltas, i32),
        name!(needs_vacuum, bool),
        name!(needs_rebuild, bool),
        name!(applied_sync_id, i64),
        name!(pending_sync_rows, i64),
        name!(invalid_reason, Option<String>),
        name!(disabled_trigger_count, i32),
        name!(read_only, bool),
        name!(read_only_reason, Option<String>),
        name!(projection_mode, String),
        name!(overlay_tombstone_count, i32),
        name!(overlay_memory_bytes, i64),
        name!(compaction_recommended, bool),
        name!(tx_delta_dirty, bool),
        name!(tx_delta_added_nodes, i32),
        name!(tx_delta_deleted_nodes, i32),
        name!(tx_delta_added_edges, i32),
        name!(tx_delta_deleted_edges, i32),
        name!(tx_delta_memory_bytes, i64),
    ),
> {
    with_panic_boundary("status()", || {
        let s = refreshed_engine_status().unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            s.node_count,
            s.edge_count,
            s.memory_used_mb,
            s.memory_limit_mb,
            s.sync_mode,
            s.sync_status,
            s.last_build,
            s.last_vacuum,
            s.edge_types,
            s.edge_buffer_used,
            s.has_unidirectional_edges,
            s.schema_state,
            s.sync_lag,
            s.edge_buffer_used,
            s.needs_vacuum,
            s.needs_rebuild,
            s.applied_sync_id,
            s.pending_sync_rows,
            s.invalid_reason,
            s.disabled_trigger_count,
            s.read_only,
            s.read_only_reason,
            s.projection_mode,
            s.overlay_tombstone_count,
            s.overlay_memory_bytes,
            s.compaction_recommended,
            s.tx_delta_dirty,
            s.tx_delta_added_nodes,
            s.tx_delta_deleted_nodes,
            s.tx_delta_added_edges,
            s.tx_delta_deleted_edges,
            s.tx_delta_memory_bytes,
        )])
    })
}

/// Return the number of unexpired active-generation backend heartbeats.
#[pg_extern(schema = "graph")]
fn active_generation_count() -> i32 {
    with_panic_boundary("active_generation_count()", || {
        crate::projection::manifest::expire_stale_generation_heartbeats()
            .unwrap_or_else(|err| err.report());
        crate::projection::manifest::active_generation_count().unwrap_or_else(|err| err.report())
    })
}

/// Return durable projection status and maintenance recommendations.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn projection_status() -> TableIterator<
    'static,
    (
        name!(manifest_generation, Option<i64>),
        name!(manifest_watermark, Option<i64>),
        name!(pending_durable_rows, i64),
        name!(segment_count, i32),
        name!(segment_bytes, i64),
        name!(l0_segment_count, i32),
        name!(l1_segment_count, i32),
        name!(l2_segment_count, i32),
        name!(edge_segment_count, i32),
        name!(node_segment_count, i32),
        name!(dirty_chunk_count, i32),
        name!(dirty_chunk_bytes, i64),
        name!(tombstone_ratio, f64),
        name!(compaction_backlog, i32),
        name!(obsolete_file_count, i32),
        name!(obsolete_bytes, i64),
        name!(active_generation_count, i32),
        name!(artifact_validation_state, String),
        name!(last_ingestion_unix_micros, Option<i64>),
        name!(last_compaction_unix_micros, Option<i64>),
        name!(last_gc_unix_micros, Option<i64>),
        name!(last_repair_unix_micros, Option<i64>),
        name!(ingest_recommended, bool),
        name!(compaction_recommended, bool),
        name!(gc_recommended, bool),
        name!(repair_recommended, bool),
    ),
> {
    with_panic_boundary("projection_status()", || {
        let s = projection_status_snapshot().unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            s.manifest_generation,
            s.manifest_watermark,
            s.pending_durable_rows,
            s.segment_count,
            s.segment_bytes,
            s.l0_segment_count,
            s.l1_segment_count,
            s.l2_segment_count,
            s.edge_segment_count,
            s.node_segment_count,
            s.dirty_chunk_count,
            s.dirty_chunk_bytes,
            s.tombstone_ratio,
            s.compaction_backlog,
            s.obsolete_file_count,
            s.obsolete_bytes,
            s.active_generation_count,
            s.artifact_validation_state,
            s.last_ingestion_unix_micros,
            s.last_compaction_unix_micros,
            s.last_gc_unix_micros,
            s.last_repair_unix_micros,
            s.ingest_recommended,
            s.compaction_recommended,
            s.gc_recommended,
            s.repair_recommended,
        )])
    })
}

/// Delete obsolete durable projection files that are no longer retained.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn projection_gc() -> TableIterator<
    'static,
    (
        name!(valid_generations_scanned, i32),
        name!(retained_generations, Vec<i64>),
        name!(active_generations, Vec<i64>),
        name!(obsolete_candidates, i32),
        name!(protected_candidates, i32),
        name!(deleted_files, i32),
        name!(deleted_bytes, i64),
    ),
> {
    with_panic_boundary("projection_gc()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        crate::projection::manifest::expire_stale_generation_heartbeats()
            .unwrap_or_else(|err| err.report());
        let artifact = crate::persistence::graph_file_path().unwrap_or_else(|err| err.report());
        let root = crate::persistence::projection_manifest_root(&artifact);
        let summary = crate::projection::gc::collect_projection_garbage(&root)
            .unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            saturating_i32(summary.valid_generations_scanned),
            u64_vec_to_i64(summary.retained_generations),
            u64_vec_to_i64(summary.active_generations),
            saturating_i32(summary.obsolete_candidates),
            saturating_i32(summary.protected_candidates),
            saturating_i32(summary.deleted_files),
            saturating_i64(summary.deleted_bytes),
        )])
    })
}

/// Repair or rebuild corrupt durable projection artifacts.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx TableIterator tuple shape is the SQL row contract"
)]
fn projection_repair() -> TableIterator<
    'static,
    (
        name!(action, String),
        name!(generation_id, Option<i64>),
        name!(rebuilt, bool),
        name!(chunks_rewritten, i32),
        name!(reason, Option<String>),
    ),
> {
    with_panic_boundary("projection_repair()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let artifact = crate::persistence::graph_file_path().unwrap_or_else(|err| err.report());
        let root = crate::persistence::projection_manifest_root(&artifact);
        let plan = crate::projection::recovery::plan_projection_recovery_for_artifact(
            &root,
            Some(&artifact),
        )
        .unwrap_or_else(|err| err.report());

        let mut action = plan.action;
        let mut generation_id = plan.generation_id;
        let mut rebuilt = false;
        let mut chunks_rewritten = 0;
        let reason = plan.reason.clone();

        match plan.action {
            crate::projection::recovery::ProjectionRecoveryAction::NoProjection
            | crate::projection::recovery::ProjectionRecoveryAction::Healthy => {}
            crate::projection::recovery::ProjectionRecoveryAction::TargetedChunkRepair => {
                let result = ENGINE
                    .with(|engine| {
                        let eng = engine.borrow();
                        crate::projection::recovery::repair_active_base_chunks(
                            &root,
                            &crate::projection::chunk::EdgeStoreChunkSource::new(&eng.edge_store),
                        )
                    })
                    .unwrap_or_else(|err| err.report());
                if let Some(result) = result {
                    generation_id = Some(result.manifest.generation_id);
                    chunks_rewritten = saturating_i32(result.chunks_rewritten);
                    reload_persisted_engine_with_projection(&artifact)
                        .unwrap_or_else(|err| err.report());
                }
            }
            crate::projection::recovery::ProjectionRecoveryAction::FullRebuild => {
                let next_generation =
                    crate::projection::recovery::next_rebuild_generation_id(&root)
                        .unwrap_or_else(|err| err.report());
                let manifest =
                    run_full_projection_rebuild_repair(&root, &artifact, next_generation)
                        .unwrap_or_else(|err| err.report());
                action = crate::projection::recovery::ProjectionRecoveryAction::FullRebuild;
                generation_id = Some(manifest.generation_id);
                rebuilt = true;
            }
        }

        TableIterator::new(vec![(
            projection_recovery_action_text(action).to_string(),
            generation_id.map(saturating_i64),
            rebuilt,
            chunks_rewritten,
            reason,
        )])
    })
}

fn run_full_projection_rebuild_repair(
    root: &std::path::Path,
    artifact: &std::path::Path,
    next_generation: u64,
) -> safety::GraphResult<crate::projection::manifest::ProjectionManifest> {
    let quarantined = crate::projection::recovery::quarantine_latest_manifest(root)?;
    let result = (|| {
        execute_maintenance_rebuild(true)?;
        let manifest = crate::projection::recovery::publish_rebuilt_base_manifest(
            artifact,
            next_generation,
            max_sync_log_id()?,
        )?;
        reload_persisted_engine_with_projection(artifact)?;
        Ok(manifest)
    })();

    match result {
        Ok(manifest) => Ok(manifest),
        Err(err) => {
            if let Some(quarantine_path) = quarantined {
                if let Err(restore_err) =
                    crate::projection::recovery::restore_quarantined_manifest(&quarantine_path)
                {
                    return Err(safety::GraphError::Internal(format!(
                        "projection repair failed ({err}); additionally failed to restore quarantined manifest: {restore_err}"
                    )));
                }
            }
            Err(err)
        }
    }
}

fn projection_recovery_action_text(
    action: crate::projection::recovery::ProjectionRecoveryAction,
) -> &'static str {
    match action {
        crate::projection::recovery::ProjectionRecoveryAction::NoProjection => "no_projection",
        crate::projection::recovery::ProjectionRecoveryAction::Healthy => "healthy",
        crate::projection::recovery::ProjectionRecoveryAction::TargetedChunkRepair => {
            "targeted_chunk_repair"
        }
        crate::projection::recovery::ProjectionRecoveryAction::FullRebuild => "full_rebuild",
    }
}

fn reload_persisted_engine_with_projection(path: &std::path::Path) -> safety::GraphResult<()> {
    let loaded = crate::persistence::load_graph_file(path)?;
    ENGINE.with(|engine| {
        *engine.borrow_mut() = loaded;
    });
    Ok(())
}

fn saturating_i32(value: usize) -> i32 {
    value.min(i32::MAX as usize) as i32
}

fn saturating_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn u64_vec_to_i64(values: Vec<u64>) -> Vec<i64> {
    values.into_iter().map(saturating_i64).collect()
}

/// Return backend-local and projected instance memory estimates.
///
/// `concurrent_backends` is an operator-supplied sizing assumption, not a live
/// backend count. Shared mmap bytes are counted once; backend-private heap is
/// multiplied by the supplied backend count.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn memory_profile(
    concurrent_backends: default!(i32, 1),
) -> TableIterator<
    'static,
    (
        name!(active_backend_private_mb, f64),
        name!(active_backend_shared_mb, f64),
        name!(active_backend_total_mb, f64),
        name!(estimated_instance_private_mb, f64),
        name!(estimated_instance_shared_mb, f64),
        name!(estimated_instance_total_mb, f64),
        name!(memory_limit_mb, i32),
        name!(assumed_concurrent_backends, i32),
    ),
> {
    with_panic_boundary("memory_profile()", || {
        let memory_limit_mb = config::MEMORY_LIMIT_MB.get();
        let profile = ENGINE.with(|e| {
            e.borrow()
                .memory_profile(concurrent_backends, memory_limit_mb)
        });
        TableIterator::new(vec![(
            profile.active_backend_private_mb,
            profile.active_backend_shared_mb,
            profile.active_backend_total_mb,
            profile.estimated_instance_private_mb,
            profile.estimated_instance_shared_mb,
            profile.estimated_instance_total_mb,
            profile.memory_limit_mb,
            profile.assumed_concurrent_backends,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn sync_health() -> TableIterator<
    'static,
    (
        name!(sync_mode, String),
        name!(query_freshness, String),
        name!(sync_batch_size, i32),
        name!(applied_sync_id, i64),
        name!(max_sync_log_id, i64),
        name!(pending_sync_rows, i64),
        name!(disabled_trigger_count, i32),
        name!(edge_buffer_used, i32),
        name!(edge_buffer_size, i32),
        name!(needs_vacuum, bool),
        name!(needs_rebuild, bool),
        name!(read_only, bool),
        name!(read_only_reason, Option<String>),
        name!(projection_mode, String),
        name!(overlay_tombstone_count, i32),
        name!(overlay_memory_bytes, i64),
        name!(compaction_recommended, bool),
        name!(tx_delta_dirty, bool),
        name!(tx_delta_added_nodes, i32),
        name!(tx_delta_deleted_nodes, i32),
        name!(tx_delta_added_edges, i32),
        name!(tx_delta_deleted_edges, i32),
        name!(tx_delta_memory_bytes, i64),
        name!(apply_sync_recommended, bool),
        name!(maintenance_recommended, bool),
        name!(durable_ingest_recommended, bool),
        name!(durable_compaction_recommended, bool),
        name!(durable_gc_recommended, bool),
        name!(durable_repair_recommended, bool),
    ),
> {
    with_panic_boundary("sync_health()", || {
        let s = refreshed_engine_status().unwrap_or_else(|err| err.report());
        let max_sync_log_id = max_sync_log_id().unwrap_or_else(|err| err.report());
        let edge_buffer_size = config::EDGE_BUFFER_SIZE.get();
        let decision = scheduled_maintenance_decision((&s).into());
        let projection = projection_metadata_status_snapshot().unwrap_or_else(|err| err.report());

        TableIterator::new(vec![(
            s.sync_mode,
            config::query_freshness(),
            config::sync_batch_size().min(i32::MAX as usize) as i32,
            s.applied_sync_id,
            max_sync_log_id,
            s.pending_sync_rows,
            s.disabled_trigger_count,
            s.edge_buffer_used,
            edge_buffer_size,
            s.needs_vacuum,
            s.needs_rebuild,
            s.read_only,
            s.read_only_reason,
            s.projection_mode,
            s.overlay_tombstone_count,
            s.overlay_memory_bytes,
            s.compaction_recommended,
            s.tx_delta_dirty,
            s.tx_delta_added_nodes,
            s.tx_delta_deleted_nodes,
            s.tx_delta_added_edges,
            s.tx_delta_deleted_edges,
            s.tx_delta_memory_bytes,
            decision.apply_sync,
            decision.start_maintenance,
            projection.ingest_recommended,
            projection.compaction_recommended,
            projection.gc_recommended,
            projection.repair_recommended,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn run_scheduled_maintenance() -> TableIterator<
    'static,
    (
        name!(applied_sync, bool),
        name!(maintenance_started, bool),
        name!(maintenance_job_id, Option<String>),
        name!(pending_sync_rows, i64),
        name!(edge_buffer_used, i32),
        name!(message, String),
    ),
> {
    with_panic_boundary("run_scheduled_maintenance()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let mut status = refreshed_engine_status().unwrap_or_else(|err| err.report());
        let mut applied_sync = false;

        let mut decision = scheduled_maintenance_decision((&status).into());
        if decision.apply_sync {
            apply_sync_internal().unwrap_or_else(|err| err.report());
            applied_sync = true;
            status = refreshed_engine_status().unwrap_or_else(|err| err.report());
            decision = scheduled_maintenance_decision((&status).into());
        }
        if let Err(err) = ingest_projection_internal(None, None) {
            if !matches!(err, safety::GraphError::NotBuilt) {
                err.report();
            }
        }

        let mut maintenance_job_id = None;
        if decision.start_maintenance {
            let job_id = create_maintenance_job().unwrap_or_else(|err| err.report());
            if let Err(err) = launch_maintenance_worker(&job_id) {
                let _ = update_maintenance_job_failed(&job_id, &err.to_string());
                err.report();
            }
            maintenance_job_id = Some(job_id);
            status = refreshed_engine_status().unwrap_or_else(|err| err.report());
        }

        let maintenance_started = maintenance_job_id.is_some();
        let message = match (applied_sync, maintenance_started) {
            (true, true) => "applied sync and started maintenance",
            (true, false) => "applied sync",
            (false, true) => "started maintenance",
            (false, false) => "no scheduled graph maintenance needed",
        }
        .to_string();

        TableIterator::new(vec![(
            applied_sync,
            maintenance_started,
            maintenance_job_id,
            status.pending_sync_rows,
            status.edge_buffer_used,
            message,
        )])
    })
}

fn refreshed_engine_status() -> safety::GraphResult<crate::types::EngineStatus> {
    crate::projection::manifest::expire_stale_generation_heartbeats()?;
    let graph = catalog::selected_or_default_graph_metadata()?;
    super::runtime::clear_loaded_graph_if_mismatched(&graph.graph_id);
    let disabled_trigger_count = disabled_graph_trigger_count()?;
    let catalog_state = current_catalog_state();
    let applied_sync_id = ENGINE.with(|e| e.borrow().applied_sync_id);
    let pending = pending_sync_rows(applied_sync_id)?;

    ENGINE.with(|e| {
        let mut eng = e.borrow_mut();
        eng.refresh_observed_state(disabled_trigger_count, pending, &catalog_state);
        if let Some(manifest) = eng.projection_manifest_full.as_ref() {
            crate::projection::manifest::record_loaded_generation_heartbeat(manifest)?;
        }
        Ok(eng.status())
    })
}

fn projection_status_snapshot() -> safety::GraphResult<crate::projection::status::ProjectionStatus>
{
    crate::projection::manifest::expire_stale_generation_heartbeats()?;
    let artifact = crate::persistence::graph_file_path()?;
    let root = crate::persistence::projection_manifest_root(&artifact);
    crate::projection::status::collect_projection_status(
        &root,
        Some(&artifact),
        max_sync_log_id()?,
        crate::projection::manifest::active_generation_count()?,
        crate::config::compaction_threshold(),
    )
}

fn projection_metadata_status_snapshot(
) -> safety::GraphResult<crate::projection::status::ProjectionStatus> {
    crate::projection::manifest::expire_stale_generation_heartbeats()?;
    let artifact = crate::persistence::graph_file_path()?;
    let root = crate::persistence::projection_manifest_root(&artifact);
    crate::projection::status::collect_projection_metadata_status(
        &root,
        max_sync_log_id()?,
        crate::projection::manifest::active_generation_count()?,
        crate::config::compaction_threshold(),
    )
}

/// Build the graph from registered tables and edges.
///
/// Optionally persists the result to disk based on `graph.persist_on_build`.
///
/// See: `docs/user_guide/build-and-persistence.mdx`
#[pg_extern(schema = "graph")]
pub(super) fn build() -> TableIterator<
    'static,
    (
        name!(nodes_loaded, i64),
        name!(edges_loaded, i64),
        name!(build_time_ms, f64),
        name!(memory_used_mb, f64),
        name!(sync_mode, String),
        name!(projection_mode, String),
    ),
> {
    with_panic_boundary("build()", || {
        require_selected_graph_build_result().unwrap_or_else(|err| err.report());
        let result = execute_build(false).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            result.nodes_loaded,
            result.edges_loaded,
            result.build_time_ms,
            result.memory_used_mb,
            result.sync_mode,
            result.projection_mode,
        )])
    })
}

/// Build a named graph without requiring a separate `set_current_graph()` call.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_graph(
    graph_name: &str,
    force_persist: default!(bool, "false"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(nodes_loaded, i64),
        name!(edges_loaded, i64),
        name!(build_time_ms, f64),
        name!(memory_used_mb, f64),
        name!(sync_mode, String),
        name!(projection_mode, String),
    ),
> {
    with_panic_boundary("build_graph()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        require_graph_build_result(&graph).unwrap_or_else(|err| err.report());
        catalog::set_selected_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report());
        let result = execute_build(force_persist).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            result.nodes_loaded,
            result.edges_loaded,
            result.build_time_ms,
            result.memory_used_mb,
            result.sync_mode,
            result.projection_mode,
        )])
    })
}

/// Queue a background build for a named graph without changing legacy overloads.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_async_graph(
    graph_name: &str,
    projection_mode: default!(Option<&str>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(build_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(status, String),
        name!(nodes_loaded, Option<i64>),
        name!(edges_loaded, Option<i64>),
        name!(build_time_ms, Option<f64>),
        name!(memory_used_mb, Option<f64>),
        name!(sync_mode, String),
        name!(projection_mode, String),
    ),
> {
    with_panic_boundary("build_async_graph()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        require_graph_build_result(&graph).unwrap_or_else(|err| err.report());
        catalog::set_selected_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report());
        let projection_mode = match projection_mode {
            Some(mode) => config::parse_projection_mode(mode).unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!(
                        "unsupported graph projection mode '{mode}'; expected 'csr_readonly' or 'mutable_overlay'"
                    ),
                }
                .report()
            }),
            None => configured_projection_mode().unwrap_or_else(|err| err.report()),
        };
        let build_id = create_build_job(projection_mode).unwrap_or_else(|err| err.report());
        if let Err(err) = launch_build_worker(&build_id) {
            let _ = update_build_job_failed(&build_id, &err.to_string());
            err.report();
        }
        let row = build_job_row(&build_id)
            .unwrap_or_else(|err| err.report())
            .unwrap_or(BuildJobRow {
                build_id,
                graph_id: graph.graph_id.clone(),
                status: JobStatus::Queued.as_str().to_string(),
                nodes_loaded: None,
                edges_loaded: None,
                build_time_ms: None,
                memory_used_mb: None,
                sync_mode: current_sync_mode()
                    .map(|mode| mode.as_str().to_string())
                    .unwrap_or_else(|_| "manual".to_string()),
                projection_mode: projection_mode.as_str().to_string(),
                progress_phase: JobStatus::Queued.as_str().to_string(),
                progress_message: Some("queued for background build".to_string()),
                started_at: None,
                finished_at: None,
                error: None,
            });
        TableIterator::new(vec![(
            row.build_id,
            graph.graph_id,
            graph.graph_name,
            row.status,
            row.nodes_loaded,
            row.edges_loaded,
            row.build_time_ms,
            row.memory_used_mb,
            row.sync_mode,
            row.projection_mode,
        )])
    })
}

#[pg_guard]
/// Background worker entrypoint for asynchronous graph builds.
///
/// PostgreSQL invokes this function by name after `graph.build(concurrently :=
/// true)` registers a dynamic background worker. Worker metadata is read from
/// pgrx's background-worker `extra` field as typed JSON metadata.
pub extern "C-unwind" fn graph_build_worker_main(_arg: pgrx::pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    let extra = BackgroundWorker::get_extra();
    let metadata = match WorkerMetadata::decode(extra) {
        Ok(metadata) => metadata,
        Err(err) => {
            pgrx::warning!(
                "graph build worker received malformed worker metadata: {}",
                err
            );
            return;
        }
    };

    BackgroundWorker::connect_worker_to_spi(Some(&metadata.database), Some(&metadata.username));

    for _ in 0..50 {
        let job_visible = BackgroundWorker::transaction(|| {
            build_job_row(&metadata.job_id).is_ok_and(|row| row.is_some())
        });
        if job_visible {
            let result = BackgroundWorker::transaction(|| run_build_job(&metadata.job_id));
            if let Err(err) = result {
                let message = err.to_string();
                let record_result = BackgroundWorker::transaction(|| {
                    update_build_job_failed(&metadata.job_id, &message)
                });
                if let Err(record_err) = record_result {
                    pgrx::warning!(
                        "graph concurrent build {} failed and failure status could not be recorded: {}",
                        metadata.job_id,
                        record_err
                    );
                }
                pgrx::warning!(
                    "graph concurrent build {} failed: {}",
                    metadata.job_id,
                    message
                );
            }
            return;
        }
        if !BackgroundWorker::wait_latch(Some(Duration::from_millis(100))) {
            return;
        }
    }

    pgrx::warning!(
        "graph concurrent build {} was not visible to worker before timeout",
        metadata.job_id
    );
}

#[pg_guard]
/// Background worker entrypoint for asynchronous graph maintenance.
///
/// PostgreSQL invokes this function by name after
/// `graph.maintenance(concurrently := true)` registers a dynamic background
/// worker. Worker metadata is read from pgrx's background-worker `extra` field
/// as typed JSON metadata.
pub extern "C-unwind" fn graph_maintenance_worker_main(_arg: pgrx::pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    let extra = BackgroundWorker::get_extra();
    let metadata = match WorkerMetadata::decode(extra) {
        Ok(metadata) => metadata,
        Err(err) => {
            pgrx::warning!(
                "graph maintenance worker received malformed worker metadata: {}",
                err
            );
            return;
        }
    };

    BackgroundWorker::connect_worker_to_spi(Some(&metadata.database), Some(&metadata.username));

    for _ in 0..50 {
        let job_visible = BackgroundWorker::transaction(|| {
            maintenance_job_row(&metadata.job_id).is_ok_and(|row| row.is_some())
        });
        if job_visible {
            let result = BackgroundWorker::transaction(|| run_maintenance_job(&metadata.job_id));
            if let Err(err) = result {
                let message = err.to_string();
                let record_result = BackgroundWorker::transaction(|| {
                    update_maintenance_job_failed(&metadata.job_id, &message)
                });
                if let Err(record_err) = record_result {
                    pgrx::warning!(
                        "graph maintenance {} failed and failure status could not be recorded: {}",
                        metadata.job_id,
                        record_err
                    );
                }
                pgrx::warning!("graph maintenance {} failed: {}", metadata.job_id, message);
            }
            return;
        }
        if !BackgroundWorker::wait_latch(Some(Duration::from_millis(100))) {
            return;
        }
    }

    pgrx::warning!(
        "graph maintenance {} was not visible to worker before timeout",
        metadata.job_id
    );
}

#[pg_guard]
/// Background worker entrypoint for one-shot due-job execution.
///
/// PostgreSQL invokes this function by name after `graph.run_due_jobs_async()`
/// registers a dynamic background worker. Worker metadata is read from pgrx's
/// background-worker `extra` field as typed JSON metadata.
pub extern "C-unwind" fn graph_due_jobs_worker_main(_arg: pgrx::pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    let extra = BackgroundWorker::get_extra();
    let metadata = match SchedulerWorkerMetadata::decode(extra) {
        Ok(metadata) => metadata,
        Err(err) => {
            pgrx::warning!(
                "graph due jobs worker received malformed worker metadata: {}",
                err
            );
            return;
        }
    };

    BackgroundWorker::connect_worker_to_spi(Some(&metadata.database), Some(&metadata.username));
    let result =
        BackgroundWorker::transaction(|| run_due_jobs_result(metadata.max_jobs, "internal"));
    if let Err(err) = result {
        pgrx::warning!("graph due jobs worker failed: {}", err);
    }
}

/// Overload for `graph.build(concurrently := bool)`.
///
/// With `concurrently := false`, this delegates to the synchronous build path
/// and wraps the result in durable-job-shaped columns. With
/// `concurrently := true`, it creates a durable build job and launches a
/// dynamic background worker.
#[pg_extern(schema = "graph", name = "build")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_with_concurrently(
    concurrently: bool,
) -> TableIterator<
    'static,
    (
        name!(build_id, String),
        name!(status, String),
        name!(nodes_loaded, Option<i64>),
        name!(edges_loaded, Option<i64>),
        name!(build_time_ms, Option<f64>),
        name!(memory_used_mb, Option<f64>),
        name!(sync_mode, String),
        name!(projection_mode, String),
    ),
> {
    with_panic_boundary("build(concurrently)", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        if concurrently {
            let projection_mode = configured_projection_mode().unwrap_or_else(|err| err.report());
            let build_id = create_build_job(projection_mode).unwrap_or_else(|err| err.report());
            if let Err(err) = launch_build_worker(&build_id) {
                let _ = update_build_job_failed(&build_id, &err.to_string());
                err.report();
            }
            let row = build_job_row(&build_id)
                .unwrap_or_else(|err| err.report())
                .unwrap_or(BuildJobRow {
                    build_id,
                    graph_id: catalog::selected_or_default_graph_metadata()
                        .map(|graph| graph.graph_id)
                        .unwrap_or_default(),
                    status: JobStatus::Queued.as_str().to_string(),
                    nodes_loaded: None,
                    edges_loaded: None,
                    build_time_ms: None,
                    memory_used_mb: None,
                    sync_mode: current_sync_mode()
                        .map(|mode| mode.as_str().to_string())
                        .unwrap_or_else(|_| "manual".to_string()),
                    projection_mode: projection_mode.as_str().to_string(),
                    progress_phase: JobStatus::Queued.as_str().to_string(),
                    progress_message: Some("queued for background build".to_string()),
                    started_at: None,
                    finished_at: None,
                    error: None,
                });
            return TableIterator::new(vec![(
                row.build_id,
                row.status,
                row.nodes_loaded,
                row.edges_loaded,
                row.build_time_ms,
                row.memory_used_mb,
                row.sync_mode,
                row.projection_mode,
            )]);
        }
        let rows = build().collect::<Vec<_>>();
        let Some((
            nodes_loaded,
            edges_loaded,
            build_time_ms,
            memory_used_mb,
            sync_mode,
            projection_mode,
        )) = rows.into_iter().next()
        else {
            return TableIterator::new(Vec::new());
        };
        TableIterator::new(vec![(
            "00000000-0000-0000-0000-000000000000".to_string(),
            JobStatus::Completed.as_str().to_string(),
            Some(nodes_loaded),
            Some(edges_loaded),
            Some(build_time_ms),
            Some(memory_used_mb),
            sync_mode,
            projection_mode,
        )])
    })
}

/// Overload for `graph.build(mode := text)`.
#[pg_extern(schema = "graph", name = "build")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_with_mode(
    mode: &str,
) -> TableIterator<
    'static,
    (
        name!(nodes_loaded, i64),
        name!(edges_loaded, i64),
        name!(build_time_ms, f64),
        name!(memory_used_mb, f64),
        name!(sync_mode, String),
        name!(projection_mode, String),
    ),
> {
    with_panic_boundary("build(mode)", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let projection_mode = config::parse_projection_mode(mode).unwrap_or_else(|| {
            safety::GraphError::InvalidFilter {
                reason: format!(
                    "unsupported graph projection mode '{mode}'; expected 'csr_readonly' or 'mutable_overlay'"
                ),
            }
            .report()
        });
        let result =
            execute_build_with_mode(false, projection_mode).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            result.nodes_loaded,
            result.edges_loaded,
            result.build_time_ms,
            result.memory_used_mb,
            result.sync_mode,
            result.projection_mode,
        )])
    })
}

/// Return durable build-job status, or backend-local status for the zero UUID
/// used by synchronous builds.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_status(
    build_id: &str,
) -> TableIterator<
    'static,
    (
        name!(build_id, String),
        name!(status, String),
        name!(nodes_loaded, Option<i64>),
        name!(edges_loaded, Option<i64>),
        name!(build_time_ms, Option<f64>),
        name!(memory_used_mb, Option<f64>),
        name!(progress_phase, String),
        name!(progress_message, Option<String>),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("build_status()", || {
        let selected_graph_id = catalog::selected_or_default_graph_metadata()
            .unwrap_or_else(|err| err.report())
            .graph_id;
        if let Some(row) = build_job_row(build_id).unwrap_or_else(|err| err.report()) {
            if row.graph_id != selected_graph_id {
                return build_not_found_status(build_id);
            }
            return TableIterator::new(vec![(
                row.build_id,
                row.status,
                row.nodes_loaded,
                row.edges_loaded,
                row.build_time_ms,
                row.memory_used_mb,
                row.progress_phase,
                row.progress_message,
                row.started_at,
                row.finished_at,
                row.error,
            )]);
        }
        let status = ENGINE.with(|e| {
            let eng = e.borrow();
            if eng.built {
                JobStatus::Completed.as_str()
            } else {
                "not_found"
            }
        });
        TableIterator::new(vec![(
            build_id.to_string(),
            status.to_string(),
            None,
            None,
            None,
            None,
            status.to_string(),
            None,
            None,
            None,
            None,
        )])
    })
}

fn build_not_found_status(
    build_id: &str,
) -> TableIterator<
    'static,
    (
        name!(build_id, String),
        name!(status, String),
        name!(nodes_loaded, Option<i64>),
        name!(edges_loaded, Option<i64>),
        name!(build_time_ms, Option<f64>),
        name!(memory_used_mb, Option<f64>),
        name!(progress_phase, String),
        name!(progress_message, Option<String>),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(error, Option<String>),
    ),
> {
    TableIterator::new(vec![(
        build_id.to_string(),
        "not_found".to_string(),
        None,
        None,
        None,
        None,
        "not_found".to_string(),
        None,
        None,
        None,
        None,
    )])
}

/// Return recent durable build jobs for a named graph.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn build_status_for_graph(
    graph_name: &str,
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
    max_rows: default!(i32, 50),
) -> TableIterator<
    'static,
    (
        name!(build_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(status, String),
        name!(nodes_loaded, Option<i64>),
        name!(edges_loaded, Option<i64>),
        name!(build_time_ms, Option<f64>),
        name!(memory_used_mb, Option<f64>),
        name!(sync_mode, String),
        name!(projection_mode, String),
        name!(progress_phase, String),
        name!(progress_message, Option<String>),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("build_status_for_graph()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        let limit = max_rows.clamp(1, 500);
        let rows = Spi::connect(|client| {
            let selected = client.select(
                "SELECT b.build_id, b.graph_id::text, g.graph_name, b.status,
                        b.nodes_loaded, b.edges_loaded, b.build_time_ms,
                        b.memory_used_mb, b.sync_mode, b.projection_mode,
                        b.progress_phase, b.progress_message, b.started_at,
                        b.finished_at, b.error
                   FROM graph._build_jobs b
                   JOIN graph._graphs g ON g.graph_id = b.graph_id
                  WHERE b.graph_id = $1::uuid
                  ORDER BY b.created_at DESC
                  LIMIT $2",
                None,
                &[graph.graph_id.into(), limit.into()],
            )?;
            let mut out = Vec::new();
            for row in selected {
                out.push((
                    row.get::<String>(1)?.unwrap_or_default(),
                    row.get::<String>(2)?.unwrap_or_default(),
                    row.get::<String>(3)?.unwrap_or_default(),
                    row.get::<String>(4)?
                        .unwrap_or_else(|| "not_found".to_string()),
                    row.get::<i64>(5)?,
                    row.get::<i64>(6)?,
                    row.get::<f64>(7)?,
                    row.get::<f64>(8)?,
                    row.get::<String>(9)?
                        .unwrap_or_else(|| "manual".to_string()),
                    row.get::<String>(10)?.unwrap_or_else(|| {
                        config::ProjectionMode::CsrReadonly.as_str().to_string()
                    }),
                    row.get::<String>(11)?
                        .unwrap_or_else(|| "unknown".to_string()),
                    row.get::<String>(12)?,
                    row.get::<TimestampWithTimeZone>(13)?,
                    row.get::<TimestampWithTimeZone>(14)?,
                    row.get::<String>(15)?,
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(out)
        })
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("build status read failed: {}", err)).report()
        });
        TableIterator::new(rows)
    })
}

/// Register a table for graph indexing.
#[pg_extern(schema = "graph")]
fn add_table(
    table_name: pgrx::pg_sys::Oid,
    id_column: &str,
    columns: default!(Option<Vec<String>>, "NULL"),
    tenant_column: default!(Option<String>, "NULL"),
) {
    with_panic_boundary("add_table()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        validate_registered_table(
            table_name.to_u32(),
            id_column,
            columns.as_deref(),
            tenant_column.as_deref(),
        )
        .unwrap_or_else(|err| err.report());

        let table_regclass = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        let id_columns = builder::PrimaryKeySpec::from_catalog_text(id_column);
        let cols = builder::PropertyColumns::from_columns(columns.unwrap_or_default());

        insert_registered_table(
            &table_regclass,
            &id_columns,
            &cols,
            tenant_column.as_deref(),
        )
        .unwrap_or_else(|err| err.report());
    });
}

/// Register a table for graph indexing using one or more primary-key columns.
#[pg_extern(schema = "graph", name = "add_table")]
fn add_table_with_id_columns(
    table_name: pgrx::pg_sys::Oid,
    id_columns: Vec<String>,
    columns: default!(Option<Vec<String>>, "NULL"),
    tenant_column: default!(Option<String>, "NULL"),
) {
    let id_column = builder::PrimaryKeySpec::from_columns(id_columns).as_catalog_text();
    add_table(table_name, &id_column, columns, tenant_column);
}

/// Register a table for a named graph without changing session selection.
#[pg_extern(schema = "graph")]
fn add_table_to_graph(
    graph_name: &str,
    table_name: pgrx::pg_sys::Oid,
    id_column: &str,
    columns: default!(Option<Vec<String>>, "NULL"),
    tenant_column: default!(Option<String>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) {
    with_panic_boundary("add_table_to_graph()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        validate_registered_table(
            table_name.to_u32(),
            id_column,
            columns.as_deref(),
            tenant_column.as_deref(),
        )
        .unwrap_or_else(|err| err.report());

        let table_regclass = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        let id_columns = builder::PrimaryKeySpec::from_catalog_text(id_column);
        let cols = builder::PropertyColumns::from_columns(columns.unwrap_or_default());

        insert_registered_table_for_graph(
            &graph.graph_id,
            &table_regclass,
            &id_columns,
            &cols,
            tenant_column.as_deref(),
        )
        .unwrap_or_else(|err| err.report());
    });
}

/// Register a table for a named graph using one or more primary-key columns.
#[pg_extern(schema = "graph", name = "add_table_to_graph")]
fn add_table_to_graph_with_id_columns(
    graph_name: &str,
    table_name: pgrx::pg_sys::Oid,
    id_columns: Vec<String>,
    columns: default!(Option<Vec<String>>, "NULL"),
    tenant_column: default!(Option<String>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) {
    let id_column = builder::PrimaryKeySpec::from_columns(id_columns).as_catalog_text();
    add_table_to_graph(
        graph_name,
        table_name,
        &id_column,
        columns,
        tenant_column,
        graph_tenant,
        graph_namespace,
    );
}

/// Register an edge relationship.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    reason = "pgrx SQL ABI exposes each SQL argument"
)]
fn add_edge(
    from_table: pgrx::pg_sys::Oid,
    from_column: &str,
    to_table: pgrx::pg_sys::Oid,
    to_column: &str,
    label: &str,
    bidirectional: default!(bool, true),
    weight_column: default!(Option<String>, "NULL"),
    label_column: default!(Option<String>, "NULL"),
) {
    with_panic_boundary("add_edge()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let registered_from_table_name =
            registered_table_name(from_table.to_u32()).unwrap_or_else(|err| err.report());
        let from_table_name = registered_from_table_name.clone().unwrap_or_else(|| {
            regclass_text(from_table.to_u32()).unwrap_or_else(|err| err.report())
        });
        let to_table_name = registered_table_name(to_table.to_u32())
            .unwrap_or_else(|err| err.report())
            .unwrap_or_else(|| regclass_text(to_table.to_u32()).unwrap_or_else(|err| err.report()));
        validate_edge_endpoint_columns(
            from_table.to_u32(),
            &from_table_name,
            from_column,
            to_table.to_u32(),
            &to_table_name,
            to_column,
            registered_from_table_name.is_some(),
        )
        .unwrap_or_else(|err| err.report());
        if let Some(weight) = weight_column.as_deref() {
            validate_column_exists(from_table.to_u32(), weight).unwrap_or_else(|err| err.report());
        }
        if let Some(label_column) = label_column.as_deref() {
            validate_column_exists(from_table.to_u32(), label_column)
                .unwrap_or_else(|err| err.report());
        }

        insert_registered_edge(RegisteredEdgeInsert {
            from_table: &from_table_name,
            from_column,
            to_table: &to_table_name,
            to_column,
            label,
            bidirectional,
            weight_column: weight_column.as_deref(),
            label_column: label_column.as_deref(),
        })
        .unwrap_or_else(|err| err.report());
    });
}

/// Register an edge relationship for a named graph without changing session selection.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::too_many_arguments,
    reason = "pgrx SQL ABI exposes each SQL argument"
)]
fn add_edge_to_graph(
    graph_name: &str,
    from_table: pgrx::pg_sys::Oid,
    from_column: &str,
    to_table: pgrx::pg_sys::Oid,
    to_column: &str,
    label: &str,
    bidirectional: default!(bool, true),
    weight_column: default!(Option<String>, "NULL"),
    label_column: default!(Option<String>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) {
    with_panic_boundary("add_edge_to_graph()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        let registered_from_table_name =
            registered_table_name_for_graph(&graph.graph_id, from_table.to_u32())
                .unwrap_or_else(|err| err.report());
        let from_table_name = registered_from_table_name.clone().unwrap_or_else(|| {
            regclass_text(from_table.to_u32()).unwrap_or_else(|err| err.report())
        });
        let to_table_name = registered_table_name_for_graph(&graph.graph_id, to_table.to_u32())
            .unwrap_or_else(|err| err.report())
            .unwrap_or_else(|| regclass_text(to_table.to_u32()).unwrap_or_else(|err| err.report()));
        validate_edge_endpoint_columns(
            from_table.to_u32(),
            &from_table_name,
            from_column,
            to_table.to_u32(),
            &to_table_name,
            to_column,
            registered_from_table_name.is_some(),
        )
        .unwrap_or_else(|err| err.report());
        if let Some(weight) = weight_column.as_deref() {
            validate_column_exists(from_table.to_u32(), weight).unwrap_or_else(|err| err.report());
        }
        if let Some(label_column) = label_column.as_deref() {
            validate_column_exists(from_table.to_u32(), label_column)
                .unwrap_or_else(|err| err.report());
        }

        insert_registered_edge_for_graph(
            &graph.graph_id,
            RegisteredEdgeInsert {
                from_table: &from_table_name,
                from_column,
                to_table: &to_table_name,
                to_column,
                label,
                bidirectional,
                weight_column: weight_column.as_deref(),
                label_column: label_column.as_deref(),
            },
        )
        .unwrap_or_else(|err| err.report());
    });
}

/// List tables registered for graph indexing.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn registered_tables() -> TableIterator<
    'static,
    (
        name!(table_name, String),
        name!(id_columns, Vec<String>),
        name!(columns, Vec<String>),
        name!(tenant_column, Option<String>),
    ),
> {
    with_panic_boundary("registered_tables()", || {
        let graph =
            catalog::selected_or_default_graph_metadata().unwrap_or_else(|err| err.report());
        registered_tables_for_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report())
    })
}

/// List tables registered for a named graph.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn registered_tables_for_graph(
    graph_name: &str,
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(table_name, String),
        name!(id_columns, Vec<String>),
        name!(columns, Vec<String>),
        name!(tenant_column, Option<String>),
    ),
> {
    with_panic_boundary("registered_tables_for_graph()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        registered_tables_for_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report())
    })
}

fn registered_tables_for_graph_id(
    graph_id: &str,
) -> safety::GraphResult<
    TableIterator<
        'static,
        (
            name!(table_name, String),
            name!(id_columns, Vec<String>),
            name!(columns, Vec<String>),
            name!(tenant_column, Option<String>),
        ),
    >,
> {
    let rows = Spi::connect(|client| {
        let result = client.select(
            "SELECT table_name, id_column, columns, tenant_column
                 FROM graph._registered_tables
                 WHERE graph_id = $1::uuid
                 ORDER BY table_name",
            None,
            &[graph_id.into()],
        )?;
        let mut rows = Vec::new();
        for row in result {
            let table_name = row.get::<String>(1)?.unwrap_or_default();
            let id_column = row.get::<String>(2)?.unwrap_or_default();
            let columns = row.get::<String>(3)?.unwrap_or_default();
            let tenant_column = row.get::<String>(4)?.filter(|s| !s.is_empty());
            rows.push((
                table_name,
                split_catalog_columns(&id_column),
                split_catalog_columns(&columns),
                tenant_column,
            ));
        }
        Ok::<_, pgrx::spi::SpiError>(rows)
    })
    .map_err(|err| safety::GraphError::Internal(format!("registered tables read failed: {err}")))?;

    Ok(TableIterator::new(rows))
}

/// List edge relationships registered for graph indexing.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn registered_edges() -> TableIterator<
    'static,
    (
        name!(from_table, String),
        name!(from_column, String),
        name!(to_table, String),
        name!(to_column, String),
        name!(label, String),
        name!(bidirectional, bool),
        name!(weight_column, Option<String>),
        name!(label_column, Option<String>),
    ),
> {
    with_panic_boundary("registered_edges()", || {
        let graph =
            catalog::selected_or_default_graph_metadata().unwrap_or_else(|err| err.report());
        registered_edges_for_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report())
    })
}

/// List edge relationships registered for a named graph.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn registered_edges_for_graph(
    graph_name: &str,
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(from_table, String),
        name!(from_column, String),
        name!(to_table, String),
        name!(to_column, String),
        name!(label, String),
        name!(bidirectional, bool),
        name!(weight_column, Option<String>),
        name!(label_column, Option<String>),
    ),
> {
    with_panic_boundary("registered_edges_for_graph()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        registered_edges_for_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report())
    })
}

fn registered_edges_for_graph_id(
    graph_id: &str,
) -> safety::GraphResult<
    TableIterator<
        'static,
        (
            name!(from_table, String),
            name!(from_column, String),
            name!(to_table, String),
            name!(to_column, String),
            name!(label, String),
            name!(bidirectional, bool),
            name!(weight_column, Option<String>),
            name!(label_column, Option<String>),
        ),
    >,
> {
    let rows = Spi::connect(|client| {
            let result = client.select(
                "SELECT from_table, from_column, to_table, to_column, label, bidirectional, weight_column, label_column
                 FROM graph._registered_edges
                 WHERE graph_id = $1::uuid
                 ORDER BY from_table, from_column, to_table, to_column, label",
                None,
                &[graph_id.into()],
            )?;
            let mut rows = Vec::new();
            for row in result {
                rows.push((
                    row.get::<String>(1)?.unwrap_or_default(),
                    row.get::<String>(2)?.unwrap_or_default(),
                    row.get::<String>(3)?.unwrap_or_default(),
                    row.get::<String>(4)?.unwrap_or_default(),
                    row.get::<String>(5)?.unwrap_or_default(),
                    row.get::<bool>(6)?.unwrap_or(true),
                    row.get::<String>(7)?.filter(|s| !s.is_empty()),
                    row.get::<String>(8)?.filter(|s| !s.is_empty()),
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(rows)
        })
        .map_err(|err| safety::GraphError::Internal(format!("registered edges read failed: {err}")))?;

    Ok(TableIterator::new(rows))
}

/// Register a column for traversal-time filters.
#[pg_extern(schema = "graph")]
fn add_filter_column(
    table_name: pgrx::pg_sys::Oid,
    column_name: &str,
    column_type: default!(&str, "'numeric'"),
) {
    with_panic_boundary("add_filter_column()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        validate_column_exists(table_name.to_u32(), column_name).unwrap_or_else(|err| err.report());
        validate_filter_column_type(table_name.to_u32(), column_name, column_type)
            .unwrap_or_else(|err| err.report());
        let table_regclass = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        let graph =
            catalog::selected_or_default_graph_metadata().unwrap_or_else(|err| err.report());
        insert_filter_column_for_graph(&graph.graph_id, &table_regclass, column_name, column_type)
            .unwrap_or_else(|err| err.report());
    });
}

/// Register a traversal-time filter column for a named graph.
#[pg_extern(schema = "graph")]
fn add_filter_column_to_graph(
    graph_name: &str,
    table_name: pgrx::pg_sys::Oid,
    column_name: &str,
    column_type: default!(&str, "'numeric'"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) {
    with_panic_boundary("add_filter_column_to_graph()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        validate_column_exists(table_name.to_u32(), column_name).unwrap_or_else(|err| err.report());
        validate_filter_column_type(table_name.to_u32(), column_name, column_type)
            .unwrap_or_else(|err| err.report());
        let table_regclass = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        insert_filter_column_for_graph(&graph.graph_id, &table_regclass, column_name, column_type)
            .unwrap_or_else(|err| err.report());
    });
}

fn insert_filter_column_for_graph(
    graph_id: &str,
    table_regclass: &str,
    column_name: &str,
    column_type: &str,
) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "INSERT INTO graph._registered_filter_columns (graph_id, table_name, column_name, column_type)
         VALUES ($1::uuid, $2, $3, $4)
         ON CONFLICT (graph_id, table_name, column_name)
         DO UPDATE SET column_type = EXCLUDED.column_type",
        &[
            graph_id.into(),
            table_regclass.into(),
            column_name.into(),
            column_type.to_ascii_lowercase().into(),
        ],
    )
    .map_err(|err| safety::GraphError::Internal(format!("filter column write failed: {err}")))
}

/// Build a structured equality filter for `graph.traverse(filter := ...)`.
#[pg_extern(schema = "graph")]
fn equals(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "eq", value)
}

#[pg_extern(schema = "graph", name = "equals")]
fn equals_text(column_name: &str, value: &str) -> pgrx::JsonB {
    equals(
        column_name,
        pgrx::JsonB(serde_json::Value::String(value.to_string())),
    )
}

#[pg_extern(schema = "graph", name = "equals")]
fn equals_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    equals(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Alias for `graph.equals()`.
#[pg_extern(schema = "graph")]
fn eq(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    equals(column_name, value)
}

#[pg_extern(schema = "graph", name = "eq")]
fn eq_text(column_name: &str, value: &str) -> pgrx::JsonB {
    equals_text(column_name, value)
}

#[pg_extern(schema = "graph", name = "eq")]
fn eq_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    equals_i64(column_name, value)
}

/// Build a structured inequality filter for `graph.traverse(filter := ...)`.
#[pg_extern(schema = "graph")]
fn not_equals(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "neq", value)
}

/// Alias for `graph.not_equals()`.
#[pg_extern(schema = "graph")]
fn neq(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    not_equals(column_name, value)
}

#[pg_extern(schema = "graph", name = "neq")]
fn neq_text(column_name: &str, value: &str) -> pgrx::JsonB {
    not_equals(
        column_name,
        pgrx::JsonB(serde_json::Value::String(value.to_string())),
    )
}

#[pg_extern(schema = "graph", name = "neq")]
fn neq_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    not_equals(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Build a structured membership filter for `graph.traverse(filter := ...)`.
#[pg_extern(schema = "graph", name = "in")]
fn in_filter(column_name: &str, values: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "in", values)
}

/// Build a structured negative membership filter.
#[pg_extern(schema = "graph")]
fn not_in(column_name: &str, values: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "not_in", values)
}

/// Build a structured substring filter for text traversal filters.
#[pg_extern(schema = "graph")]
fn contains_text(column_name: &str, value: &str) -> pgrx::JsonB {
    filter_helper(
        column_name,
        "contains",
        pgrx::JsonB(serde_json::Value::String(value.to_string())),
    )
}

/// Build a structured prefix filter for text traversal filters.
#[pg_extern(schema = "graph")]
fn prefix_text(column_name: &str, value: &str) -> pgrx::JsonB {
    filter_helper(
        column_name,
        "prefix",
        pgrx::JsonB(serde_json::Value::String(value.to_string())),
    )
}

/// Build a structured SQL NULL filter.
#[pg_extern(schema = "graph")]
fn is_null(column_name: &str) -> pgrx::JsonB {
    filter_helper(column_name, "is_null", pgrx::JsonB(serde_json::Value::Null))
}

/// Build a structured SQL NOT NULL filter.
#[pg_extern(schema = "graph")]
fn is_not_null(column_name: &str) -> pgrx::JsonB {
    filter_helper(
        column_name,
        "is_not_null",
        pgrx::JsonB(serde_json::Value::Null),
    )
}

/// Build a structured greater-than filter for `graph.traverse(filter := ...)`.
#[pg_extern(schema = "graph")]
fn greater_than(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "gt", value)
}

#[pg_extern(schema = "graph", name = "greater_than")]
fn greater_than_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    greater_than(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Alias for `graph.greater_than()`.
#[pg_extern(schema = "graph")]
fn gt(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    greater_than(column_name, value)
}

#[pg_extern(schema = "graph", name = "gt")]
fn gt_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    greater_than_i64(column_name, value)
}

/// Build a structured greater-than-or-equal filter.
#[pg_extern(schema = "graph")]
fn at_least(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "gte", value)
}

/// Alias for `graph.at_least()`.
#[pg_extern(schema = "graph")]
fn gte(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    at_least(column_name, value)
}

#[pg_extern(schema = "graph", name = "gte")]
fn gte_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    at_least(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Build a structured less-than filter.
#[pg_extern(schema = "graph")]
fn less_than(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "lt", value)
}

/// Alias for `graph.less_than()`.
#[pg_extern(schema = "graph")]
fn lt(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    less_than(column_name, value)
}

#[pg_extern(schema = "graph", name = "lt")]
fn lt_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    less_than(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Build a structured less-than-or-equal filter.
#[pg_extern(schema = "graph")]
fn at_most(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(column_name, "lte", value)
}

/// Alias for `graph.at_most()`.
#[pg_extern(schema = "graph")]
fn lte(column_name: &str, value: pgrx::JsonB) -> pgrx::JsonB {
    at_most(column_name, value)
}

#[pg_extern(schema = "graph", name = "lte")]
fn lte_i64(column_name: &str, value: i64) -> pgrx::JsonB {
    at_most(column_name, pgrx::JsonB(serde_json::Value::from(value)))
}

/// Build a structured inclusive range filter.
#[pg_extern(schema = "graph")]
fn between(column_name: &str, lower: pgrx::JsonB, upper: pgrx::JsonB) -> pgrx::JsonB {
    filter_helper(
        column_name,
        "between",
        pgrx::JsonB(serde_json::Value::Array(vec![lower.0, upper.0])),
    )
}

/// Wrap a filter in the node scope expected by traversal.
#[pg_extern(schema = "graph")]
fn on_node(filter: pgrx::JsonB) -> pgrx::JsonB {
    let Some(where_clause) = filter.0.get("where").cloned() else {
        return filter;
    };
    pgrx::JsonB(serde_json::json!({ "node": { "where": where_clause } }))
}

/// Construct the canonical SDK-friendly node reference string.
#[pg_extern(schema = "graph")]
fn node_ref_string(table_name: pgrx::pg_sys::Oid, node_id: &str) -> String {
    with_panic_boundary("node_ref_string()", || {
        canonical_node_ref_string(table_name.to_u32(), node_id).unwrap_or_else(|err| err.report())
    })
}

/// Format a traversal `path` + `edge_path` pair as readable hop text.
#[pg_extern(schema = "graph")]
fn format_path(
    path: pgrx::JsonB,
    edge_path: pgrx::JsonB,
    separator: default!(&str, "' | '"),
) -> String {
    with_panic_boundary("format_path()", || {
        format_path_value(&path.0, &edge_path.0, separator).unwrap_or_else(|err| err.report())
    })
}

/// Combine structured filters with logical AND.
#[pg_extern(schema = "graph")]
fn all(filters: Vec<pgrx::JsonB>) -> pgrx::JsonB {
    let mut merged = serde_json::Map::new();
    for filter in filters {
        let Some(where_clause) = filter
            .0
            .get("node")
            .and_then(|node| node.get("where"))
            .or_else(|| filter.0.get("where"))
            .and_then(|value| value.as_object())
        else {
            continue;
        };
        for (column, predicate) in where_clause {
            merged.insert(column.clone(), predicate.clone());
        }
    }
    pgrx::JsonB(serde_json::json!({ "where": merged }))
}

/// Unregister a table from graph indexing.
///
/// The graph must be rebuilt after removal.
///
/// See: `docs/user_guide/schema-registration.mdx`
#[pg_extern(schema = "graph")]
fn remove_table(table_name: pgrx::pg_sys::Oid) {
    with_panic_boundary("remove_table()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let graph =
            catalog::selected_or_default_graph_metadata().unwrap_or_else(|err| err.report());
        let table = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        remove_table_from_graph_id(&graph.graph_id, &table).unwrap_or_else(|err| err.report());
        pgrx::notice!(
            "graph: unregistered table {}. Call graph.build() to rebuild.",
            table
        );
    });
}

/// Unregister a table from a named graph without changing session selection.
#[pg_extern(schema = "graph")]
fn remove_table_from_graph(
    graph_name: &str,
    table_name: pgrx::pg_sys::Oid,
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) {
    with_panic_boundary("remove_table_from_graph()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        let table = regclass_text(table_name.to_u32()).unwrap_or_else(|err| err.report());
        remove_table_from_graph_id(&graph.graph_id, &table).unwrap_or_else(|err| err.report());
        pgrx::notice!(
            "graph: unregistered table {} from graph '{}'. Call graph.build() to rebuild.",
            table,
            graph.graph_name
        );
    });
}

fn remove_table_from_graph_id(graph_id: &str, table: &str) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "DELETE FROM graph._registered_tables
          WHERE graph_id = $1::uuid
            AND table_name = $2",
        &[graph_id.into(), table.into()],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("registered table delete failed: {err}"))
    })?;
    Spi::run_with_args(
        "DELETE FROM graph._registered_filter_columns
          WHERE graph_id = $1::uuid
            AND table_name = $2",
        &[graph_id.into(), table.into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("filter column delete failed: {err}")))?;
    Spi::run_with_args(
        "DELETE FROM graph._registered_edges
          WHERE graph_id = $1::uuid
            AND (from_table = $2 OR to_table = $2)",
        &[graph_id.into(), table.into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("registered edge delete failed: {err}")))
}

/// Unregister an edge relationship by label.
///
/// The graph must be rebuilt after removal.
///
/// See: `docs/user_guide/schema-registration.mdx`
#[pg_extern(schema = "graph")]
fn remove_edge(label: &str) {
    with_panic_boundary("remove_edge()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let graph =
            catalog::selected_or_default_graph_metadata().unwrap_or_else(|err| err.report());
        remove_edge_from_graph_id(&graph.graph_id, label).unwrap_or_else(|err| err.report());
        pgrx::notice!(
            "graph: unregistered edge '{}'. Call graph.build() to rebuild.",
            label
        );
    });
}

/// Unregister an edge relationship by label from a named graph.
#[pg_extern(schema = "graph")]
fn remove_edge_from_graph(
    graph_name: &str,
    label: &str,
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) {
    with_panic_boundary("remove_edge_from_graph()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        remove_edge_from_graph_id(&graph.graph_id, label).unwrap_or_else(|err| err.report());
        pgrx::notice!(
            "graph: unregistered edge '{}' from graph '{}'. Call graph.build() to rebuild.",
            label,
            graph.graph_name
        );
    });
}

fn remove_edge_from_graph_id(graph_id: &str, label: &str) -> safety::GraphResult<()> {
    Spi::run_with_args(
        "DELETE FROM graph._registered_edges
          WHERE graph_id = $1::uuid
            AND label = $2",
        &[graph_id.into(), label.into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("registered edge delete failed: {err}")))
}

/// Estimate RAM requirements without building the graph.
///
/// Returns projected node count, edge count, and memory usage based on
/// `pg_class.reltuples` estimates from registered tables.
///
/// See: `docs/user_guide/api-reference.mdx`
#[pg_extern(schema = "graph")]
fn estimate() -> TableIterator<
    'static,
    (
        name!(estimated_nodes, i64),
        name!(estimated_edges, i64),
        name!(estimated_memory_mb, f64),
        name!(memory_limit_mb, i32),
        name!(fits_in_memory, bool),
    ),
> {
    with_panic_boundary("estimate()", || {
        let (tables, edges, _filter_columns) = read_catalog().unwrap_or_else(|err| err.report());
        let mut est_nodes: i64 = 0;
        let mut est_edges: i64 = 0;
        let mut table_counts = std::collections::HashMap::new();

        for table in &tables {
            let count = cached_estimated_table_rows(&mut table_counts, &table.table_name);
            est_nodes += count;
        }

        for edge in &edges {
            let count = cached_estimated_table_rows(&mut table_counts, &edge.from_table);
            let multiplier = if edge.bidirectional { 2 } else { 1 };
            est_edges += count * multiplier;
        }

        // Memory formula: graph topology plus resolution index.
        // NodeStore estimate: table OID + active bit + average primary-key bytes.
        // EdgeStore estimate: forward offsets plus target/type arrays.
        // ResolutionIndex estimate: 16 bytes/node.
        let node_bytes = est_nodes as f64 * (44.0 + 16.0);
        let edge_bytes = (est_nodes as f64 * 4.0) + (est_edges as f64 * 5.0);
        let est_memory_mb = (node_bytes + edge_bytes) / 1_048_576.0;

        let limit = config::MEMORY_LIMIT_MB.get();
        let fits = est_memory_mb <= limit as f64;

        TableIterator::new(vec![(est_nodes, est_edges, est_memory_mb, limit, fits)])
    })
}

/// Apply pending durable sync-log rows, plus any legacy sync-buffer rows, to
/// the backend-local graph.
///
/// See: `docs/user_guide/sync-and-maintenance.mdx`
#[pg_extern(schema = "graph")]
fn apply_sync() -> TableIterator<
    'static,
    (
        name!(inserts_applied, i64),
        name!(updates_applied, i64),
        name!(deletes_applied, i64),
    ),
> {
    with_panic_boundary("apply_sync()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let stats = apply_sync_internal().unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(stats.inserts, stats.updates, stats.deletes)])
    })
}

#[derive(Debug, Clone)]
struct SyncPolicyRow {
    policy_id: String,
    job_id: String,
    graph_id: String,
    graph_name: String,
    enabled: bool,
    schedule_interval_secs: i64,
    max_sync_lag_rows: Option<i64>,
    next_run_at: Option<TimestampWithTimeZone>,
    last_run_at: Option<TimestampWithTimeZone>,
    last_status: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
struct GenericJobRow {
    job_id: String,
    graph_id: String,
    graph_name: String,
    policy_kind: String,
    enabled: bool,
    schedule_interval_secs: i64,
    max_runtime_secs: Option<i64>,
    max_retries: i32,
    next_run_at: Option<TimestampWithTimeZone>,
    last_run_at: Option<TimestampWithTimeZone>,
    last_status: Option<String>,
    last_error: Option<String>,
    last_sqlstate: Option<String>,
}

#[derive(Debug, Clone)]
struct JobRunRow {
    run_id: String,
    job_id: String,
    graph_id: String,
    graph_name: String,
    status: String,
    rows_applied: Option<i64>,
    retry_count: i32,
    execution_mode: String,
    sqlstate: Option<String>,
    error: Option<String>,
    started_at: TimestampWithTimeZone,
    finished_at: Option<TimestampWithTimeZone>,
}

/// Advisory lock namespace for generic graph jobs.
///
/// The object id is derived from graph and job UUID text so concurrent runners
/// for the same job skip rather than replaying the same durable work twice.
const JOB_LOCK_CLASS_ID: i32 = 1_918_928_212;

fn validate_positive_i64(value: i64, name: &str) -> safety::GraphResult<i64> {
    if value > 0 {
        Ok(value)
    } else {
        Err(safety::GraphError::InvalidFilter {
            reason: format!("{name} must be greater than zero"),
        })
    }
}

fn validate_nonnegative_i64(value: i64, name: &str) -> safety::GraphResult<i64> {
    if value >= 0 {
        Ok(value)
    } else {
        Err(safety::GraphError::InvalidFilter {
            reason: format!("{name} must be non-negative"),
        })
    }
}

fn validate_positive_i32(value: i32, name: &str) -> safety::GraphResult<i32> {
    if value > 0 {
        Ok(value)
    } else {
        Err(safety::GraphError::InvalidFilter {
            reason: format!("{name} must be greater than zero"),
        })
    }
}

fn validate_nonnegative_i32(value: i32, name: &str) -> safety::GraphResult<i32> {
    if value >= 0 {
        Ok(value)
    } else {
        Err(safety::GraphError::InvalidFilter {
            reason: format!("{name} must be non-negative"),
        })
    }
}

fn require_admin_for_graph_id(graph_id: &str) -> safety::GraphResult<catalog::GraphMetadata> {
    let graph = catalog::list_graph_metadata()?
        .into_iter()
        .find(|graph| graph.graph_id == graph_id)
        .ok_or_else(|| safety::GraphError::InvalidFilter {
            reason: format!("graph id '{graph_id}' is not visible"),
        })?;
    catalog::require_graph_privilege(&graph, catalog::GraphPrivilege::Admin)?;
    Ok(graph)
}

fn visible_graph_ids() -> safety::GraphResult<Vec<String>> {
    Ok(catalog::list_graph_metadata()?
        .into_iter()
        .map(|graph| graph.graph_id)
        .collect())
}

fn sync_policy_row(policy_id: &str) -> safety::GraphResult<Option<SyncPolicyRow>> {
    let rows = sync_policy_rows(Some(policy_id), None, 1)?;
    Ok(rows.into_iter().next())
}

fn sync_policy_rows(
    policy_id: Option<&str>,
    graph_id: Option<&str>,
    limit: i32,
) -> safety::GraphResult<Vec<SyncPolicyRow>> {
    let limit = limit.clamp(1, 500);
    let visible_graph_ids = visible_graph_ids()?;
    Spi::connect(|client| {
        let selected = client.select(
            "SELECT p.policy_id::text, p.job_id::text, p.graph_id::text, g.graph_name,
                    p.enabled, p.schedule_interval_secs, p.max_sync_lag_rows,
                    p.next_run_at, p.last_run_at, p.last_status, p.last_error
               FROM graph._sync_policies p
               JOIN graph._graphs g ON g.graph_id = p.graph_id
              WHERE ($1::uuid IS NULL OR p.policy_id = $1::uuid)
                AND ($2::uuid IS NULL OR p.graph_id = $2::uuid)
                AND p.graph_id::text = ANY($4::text[])
              ORDER BY p.created_at DESC
              LIMIT $3",
            None,
            &[
                policy_id.into(),
                graph_id.into(),
                limit.into(),
                visible_graph_ids.into(),
            ],
        )?;
        let mut out = Vec::new();
        for row in selected {
            out.push(SyncPolicyRow {
                policy_id: row.get::<String>(1)?.unwrap_or_default(),
                job_id: row.get::<String>(2)?.unwrap_or_default(),
                graph_id: row.get::<String>(3)?.unwrap_or_default(),
                graph_name: row.get::<String>(4)?.unwrap_or_default(),
                enabled: row.get::<bool>(5)?.unwrap_or(false),
                schedule_interval_secs: row.get::<i64>(6)?.unwrap_or(60),
                max_sync_lag_rows: row.get::<i64>(7)?,
                next_run_at: row.get::<TimestampWithTimeZone>(8)?,
                last_run_at: row.get::<TimestampWithTimeZone>(9)?,
                last_status: row.get::<String>(10)?,
                last_error: row.get::<String>(11)?,
            });
        }
        Ok::<_, pgrx::spi::SpiError>(out)
    })
    .map_err(|err| safety::GraphError::Internal(format!("sync policy read failed: {err}")))
}

fn generic_job_rows(
    job_id: Option<&str>,
    graph_id: Option<&str>,
    limit: i32,
) -> safety::GraphResult<Vec<GenericJobRow>> {
    let limit = limit.clamp(1, 500);
    let visible_graph_ids = visible_graph_ids()?;
    Spi::connect(|client| {
        let selected = client.select(
            "SELECT j.job_id::text, j.graph_id::text, g.graph_name, j.policy_kind,
                    j.enabled, j.schedule_interval_secs, j.max_runtime_secs,
                    j.max_retries, j.next_run_at, j.last_run_at, j.last_status,
                    j.last_error, j.last_sqlstate
               FROM graph._jobs j
               JOIN graph._graphs g ON g.graph_id = j.graph_id
              WHERE ($1::uuid IS NULL OR j.job_id = $1::uuid)
                AND ($2::uuid IS NULL OR j.graph_id = $2::uuid)
                AND j.graph_id::text = ANY($4::text[])
              ORDER BY j.created_at DESC
              LIMIT $3",
            None,
            &[
                job_id.into(),
                graph_id.into(),
                limit.into(),
                visible_graph_ids.into(),
            ],
        )?;
        let mut out = Vec::new();
        for row in selected {
            out.push(GenericJobRow {
                job_id: row.get::<String>(1)?.unwrap_or_default(),
                graph_id: row.get::<String>(2)?.unwrap_or_default(),
                graph_name: row.get::<String>(3)?.unwrap_or_default(),
                policy_kind: row.get::<String>(4)?.unwrap_or_default(),
                enabled: row.get::<bool>(5)?.unwrap_or(false),
                schedule_interval_secs: row.get::<i64>(6)?.unwrap_or(60),
                max_runtime_secs: row.get::<i64>(7)?,
                max_retries: row.get::<i32>(8)?.unwrap_or(0),
                next_run_at: row.get::<TimestampWithTimeZone>(9)?,
                last_run_at: row.get::<TimestampWithTimeZone>(10)?,
                last_status: row.get::<String>(11)?,
                last_error: row.get::<String>(12)?,
                last_sqlstate: row.get::<String>(13)?,
            });
        }
        Ok::<_, pgrx::spi::SpiError>(out)
    })
    .map_err(|err| safety::GraphError::Internal(format!("job read failed: {err}")))
}

fn due_generic_job_rows(limit: i32) -> safety::GraphResult<Vec<GenericJobRow>> {
    let limit = limit.clamp(1, 500);
    let visible_graph_ids = visible_graph_ids()?;
    Spi::connect(|client| {
        let selected = client.select(
            "SELECT j.job_id::text, j.graph_id::text, g.graph_name, j.policy_kind,
                    j.enabled, j.schedule_interval_secs, j.max_runtime_secs,
                    j.max_retries, j.next_run_at, j.last_run_at, j.last_status,
                    j.last_error, j.last_sqlstate
               FROM graph._jobs j
               JOIN graph._graphs g ON g.graph_id = j.graph_id
              WHERE j.enabled
                AND (j.next_run_at IS NULL OR j.next_run_at <= now())
                AND j.graph_id::text = ANY($2::text[])
              ORDER BY j.next_run_at NULLS FIRST, j.created_at
              LIMIT $1
              FOR UPDATE OF j SKIP LOCKED",
            None,
            &[limit.into(), visible_graph_ids.into()],
        )?;
        let mut out = Vec::new();
        for row in selected {
            out.push(GenericJobRow {
                job_id: row.get::<String>(1)?.unwrap_or_default(),
                graph_id: row.get::<String>(2)?.unwrap_or_default(),
                graph_name: row.get::<String>(3)?.unwrap_or_default(),
                policy_kind: row.get::<String>(4)?.unwrap_or_default(),
                enabled: row.get::<bool>(5)?.unwrap_or(false),
                schedule_interval_secs: row.get::<i64>(6)?.unwrap_or(60),
                max_runtime_secs: row.get::<i64>(7)?,
                max_retries: row.get::<i32>(8)?.unwrap_or(0),
                next_run_at: row.get::<TimestampWithTimeZone>(9)?,
                last_run_at: row.get::<TimestampWithTimeZone>(10)?,
                last_status: row.get::<String>(11)?,
                last_error: row.get::<String>(12)?,
                last_sqlstate: row.get::<String>(13)?,
            });
        }
        Ok::<_, pgrx::spi::SpiError>(out)
    })
    .map_err(|err| safety::GraphError::Internal(format!("due job read failed: {err}")))
}

fn job_run_rows(
    job_id: Option<&str>,
    graph_id: Option<&str>,
    limit: i32,
) -> safety::GraphResult<Vec<JobRunRow>> {
    let limit = limit.clamp(1, 500);
    let visible_graph_ids = visible_graph_ids()?;
    Spi::connect(|client| {
        let selected = client.select(
            "SELECT r.run_id::text, r.job_id::text, r.graph_id::text, g.graph_name,
                    r.status, r.rows_applied, r.retry_count, r.execution_mode,
                    r.sqlstate, r.error, r.started_at, r.finished_at
               FROM graph._job_runs r
               JOIN graph._graphs g ON g.graph_id = r.graph_id
              WHERE ($1::uuid IS NULL OR r.job_id = $1::uuid)
                AND ($2::uuid IS NULL OR r.graph_id = $2::uuid)
                AND r.graph_id::text = ANY($4::text[])
              ORDER BY r.started_at DESC
              LIMIT $3",
            None,
            &[
                job_id.into(),
                graph_id.into(),
                limit.into(),
                visible_graph_ids.into(),
            ],
        )?;
        let mut out = Vec::new();
        for row in selected {
            out.push(JobRunRow {
                run_id: row.get::<String>(1)?.unwrap_or_default(),
                job_id: row.get::<String>(2)?.unwrap_or_default(),
                graph_id: row.get::<String>(3)?.unwrap_or_default(),
                graph_name: row.get::<String>(4)?.unwrap_or_default(),
                status: row
                    .get::<String>(5)?
                    .unwrap_or_else(|| "unknown".to_string()),
                rows_applied: row.get::<i64>(6)?,
                retry_count: row.get::<i32>(7)?.unwrap_or(0),
                execution_mode: row
                    .get::<String>(8)?
                    .unwrap_or_else(|| "hosted".to_string()),
                sqlstate: row.get::<String>(9)?,
                error: row.get::<String>(10)?,
                started_at: row
                    .get::<TimestampWithTimeZone>(11)?
                    .ok_or_else(|| pgrx::spi::SpiError::InvalidPosition)?,
                finished_at: row.get::<TimestampWithTimeZone>(12)?,
            });
        }
        Ok::<_, pgrx::spi::SpiError>(out)
    })
    .map_err(|err| safety::GraphError::Internal(format!("job run read failed: {err}")))
}

fn insert_job_run(
    job_id: &str,
    graph_id: &str,
    status: &str,
    retry_count: i32,
    execution_mode: &str,
) -> safety::GraphResult<JobRunRow> {
    Spi::connect(|client| {
        let mut rows = client.select(
            "INSERT INTO graph._job_runs (
                    job_id, graph_id, status, retry_count, worker_identity, execution_mode
                )
                VALUES (
                    $1::uuid, $2::uuid, $3, $4,
                    concat(current_user, '@', COALESCE(inet_server_addr()::text, 'local')),
                    $5
                )
                RETURNING run_id::text, job_id::text, graph_id::text, status,
                          rows_applied, retry_count, execution_mode, sqlstate,
                          error, started_at, finished_at",
            None,
            &[
                job_id.into(),
                graph_id.into(),
                status.into(),
                retry_count.into(),
                execution_mode.into(),
            ],
        )?;
        let row = rows.next().ok_or(pgrx::spi::SpiError::InvalidPosition)?;
        Ok::<_, pgrx::spi::SpiError>(JobRunRow {
            run_id: row.get::<String>(1)?.unwrap_or_default(),
            job_id: row.get::<String>(2)?.unwrap_or_default(),
            graph_id: row.get::<String>(3)?.unwrap_or_default(),
            graph_name: String::new(),
            status: row.get::<String>(4)?.unwrap_or_else(|| status.to_string()),
            rows_applied: row.get::<i64>(5)?,
            retry_count: row.get::<i32>(6)?.unwrap_or(0),
            execution_mode: row
                .get::<String>(7)?
                .unwrap_or_else(|| "hosted".to_string()),
            sqlstate: row.get::<String>(8)?,
            error: row.get::<String>(9)?,
            started_at: row
                .get::<TimestampWithTimeZone>(10)?
                .ok_or(pgrx::spi::SpiError::InvalidPosition)?,
            finished_at: row.get::<TimestampWithTimeZone>(11)?,
        })
    })
    .map_err(|err| safety::GraphError::Internal(format!("job run creation failed: {err}")))
}

fn graph_job_lock_object_id(graph_id: &str, job_id: &str) -> i32 {
    let mut hash = 0x811c_9dc5_u32;
    for byte in graph_id
        .bytes()
        .chain([b':'])
        .chain(job_id.bytes())
        .filter(|byte| *byte != b'-')
    {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    i32::from_ne_bytes(hash.to_ne_bytes())
}

fn try_acquire_job_lock(graph_id: &str, job_id: &str) -> safety::GraphResult<bool> {
    let lock_object_id = graph_job_lock_object_id(graph_id, job_id);
    Spi::get_one::<bool>(&format!(
        "SELECT pg_try_advisory_xact_lock({JOB_LOCK_CLASS_ID}, {lock_object_id})"
    ))
    .map_err(|err| safety::GraphError::Internal(format!("job lock acquisition failed: {err}")))?
    .ok_or_else(|| safety::GraphError::Internal("job lock acquisition returned null".to_string()))
}

fn failed_attempt_count(job_id: &str) -> safety::GraphResult<i32> {
    let count = Spi::get_one_with_args::<i64>(
        "SELECT count(*)::bigint
           FROM graph._job_runs
          WHERE job_id = $1::uuid
            AND status IN ('failed', 'retryable_failed', 'permanent_failed')",
        &[job_id.into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("job retry count read failed: {err}")))?
    .unwrap_or(0);
    i32::try_from(count).map_err(|_| {
        safety::GraphError::Internal("job retry count exceeds supported range".to_string())
    })
}

fn job_run_row_by_run_id(run_id: &str) -> safety::GraphResult<JobRunRow> {
    Spi::connect(|client| {
        let mut rows = client.select(
            "SELECT r.run_id::text, r.job_id::text, r.graph_id::text, g.graph_name,
                    r.status, r.rows_applied, r.retry_count, r.execution_mode,
                    r.sqlstate, r.error, r.started_at, r.finished_at
               FROM graph._job_runs r
               JOIN graph._graphs g ON g.graph_id = r.graph_id
              WHERE r.run_id = $1::uuid
              LIMIT 1",
            None,
            &[run_id.into()],
        )?;
        let row = rows.next().ok_or(pgrx::spi::SpiError::InvalidPosition)?;
        Ok::<_, pgrx::spi::SpiError>(JobRunRow {
            run_id: row.get::<String>(1)?.unwrap_or_default(),
            job_id: row.get::<String>(2)?.unwrap_or_default(),
            graph_id: row.get::<String>(3)?.unwrap_or_default(),
            graph_name: row.get::<String>(4)?.unwrap_or_default(),
            status: row
                .get::<String>(5)?
                .unwrap_or_else(|| "unknown".to_string()),
            rows_applied: row.get::<i64>(6)?,
            retry_count: row.get::<i32>(7)?.unwrap_or(0),
            execution_mode: row
                .get::<String>(8)?
                .unwrap_or_else(|| "hosted".to_string()),
            sqlstate: row.get::<String>(9)?,
            error: row.get::<String>(10)?,
            started_at: row
                .get::<TimestampWithTimeZone>(11)?
                .ok_or(pgrx::spi::SpiError::InvalidPosition)?,
            finished_at: row.get::<TimestampWithTimeZone>(12)?,
        })
    })
    .map_err(|err| safety::GraphError::Internal(format!("job run read failed: {err}")))
}

fn complete_sync_policy_run(
    policy: &SyncPolicyRow,
    run_id: &str,
    rows_applied: i64,
) -> safety::GraphResult<JobRunRow> {
    let completed = JobStatus::Completed.as_str();
    Spi::run_with_args(
        "UPDATE graph._job_runs
            SET status = $2,
                rows_applied = $3,
                finished_at = now()
          WHERE run_id = $1::uuid",
        &[run_id.into(), completed.into(), rows_applied.into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("job run completion failed: {err}")))?;
    Spi::run_with_args(
        "UPDATE graph._jobs
            SET last_run_at = now(),
                last_status = $2,
                last_error = NULL,
                last_sqlstate = NULL,
                next_run_at = now() + ($3::bigint * interval '1 second'),
                updated_at = now()
          WHERE job_id = $1::uuid",
        &[
            policy.job_id.clone().into(),
            completed.into(),
            policy.schedule_interval_secs.into(),
        ],
    )
    .map_err(|err| safety::GraphError::Internal(format!("job completion update failed: {err}")))?;
    Spi::run_with_args(
        "UPDATE graph._sync_policies
            SET last_run_at = now(),
                last_status = $2,
                last_error = NULL,
                next_run_at = now() + ($3::bigint * interval '1 second'),
                updated_at = now()
          WHERE policy_id = $1::uuid",
        &[
            policy.policy_id.clone().into(),
            completed.into(),
            policy.schedule_interval_secs.into(),
        ],
    )
    .map_err(|err| {
        safety::GraphError::Internal(format!("policy completion update failed: {err}"))
    })?;
    job_run_row_by_run_id(run_id)
}

fn fail_sync_policy_run(
    policy: &SyncPolicyRow,
    run_id: &str,
    err: &safety::GraphError,
    retry_count: i32,
    max_retries: i32,
) -> safety::GraphResult<()> {
    let failed = if retry_count < max_retries {
        JobStatus::RetryableFailure.as_str()
    } else {
        JobStatus::PermanentFailure.as_str()
    };
    let sqlstate = err.sqlstate();
    let message = err.to_string();
    Spi::run_with_args(
        "UPDATE graph._job_runs
            SET status = $2,
                sqlstate = $3,
                error = $4,
                finished_at = now()
          WHERE run_id = $1::uuid",
        &[
            run_id.into(),
            failed.into(),
            sqlstate.into(),
            message.clone().into(),
        ],
    )
    .map_err(|err| safety::GraphError::Internal(format!("job run failure update failed: {err}")))?;
    Spi::run_with_args(
        "UPDATE graph._jobs
            SET last_run_at = now(),
                last_status = $2,
                last_error = $3,
                last_sqlstate = $4,
                updated_at = now()
          WHERE job_id = $1::uuid",
        &[
            policy.job_id.clone().into(),
            failed.into(),
            message.clone().into(),
            sqlstate.into(),
        ],
    )
    .map_err(|err| safety::GraphError::Internal(format!("job failure update failed: {err}")))?;
    Spi::run_with_args(
        "UPDATE graph._sync_policies
            SET last_run_at = now(),
                last_status = $2,
                last_error = $3,
                updated_at = now()
          WHERE policy_id = $1::uuid",
        &[
            policy.policy_id.clone().into(),
            failed.into(),
            message.into(),
        ],
    )
    .map_err(|err| safety::GraphError::Internal(format!("policy failure update failed: {err}")))
}

fn run_sync_policy_result(policy_id: &str) -> safety::GraphResult<JobRunRow> {
    run_sync_policy_with_mode(policy_id, "hosted")
}

fn run_sync_policy_with_mode(
    policy_id: &str,
    execution_mode: &str,
) -> safety::GraphResult<JobRunRow> {
    let policy = sync_policy_row(policy_id)?.ok_or_else(|| safety::GraphError::InvalidFilter {
        reason: format!("sync policy '{policy_id}' does not exist"),
    })?;
    let graph = require_admin_for_graph_id(&policy.graph_id)?;
    catalog::set_selected_graph_id(&graph.graph_id)?;
    super::runtime::clear_loaded_graph_if_mismatched(&graph.graph_id);
    let job = generic_job_rows(Some(&policy.job_id), None, 1)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            safety::GraphError::Internal(format!(
                "sync policy '{}' has no backing job",
                policy.policy_id
            ))
        })?;
    let retry_count = failed_attempt_count(&policy.job_id)?;

    if !policy.enabled {
        let disabled = JobStatus::Disabled.as_str();
        let row = insert_job_run(
            &policy.job_id,
            &policy.graph_id,
            disabled,
            retry_count,
            execution_mode,
        )?;
        Spi::run_with_args(
            "UPDATE graph._job_runs
                SET finished_at = now()
              WHERE run_id = $1::uuid",
            &[row.run_id.clone().into()],
        )
        .map_err(|err| {
            safety::GraphError::Internal(format!("disabled job run update failed: {err}"))
        })?;
        Spi::run_with_args(
            "UPDATE graph._jobs
                SET last_run_at = now(),
                    last_status = $2,
                    last_error = NULL,
                    last_sqlstate = NULL,
                    updated_at = now()
              WHERE job_id = $1::uuid",
            &[policy.job_id.clone().into(), disabled.into()],
        )
        .map_err(|err| {
            safety::GraphError::Internal(format!("disabled job status update failed: {err}"))
        })?;
        Spi::run_with_args(
            "UPDATE graph._sync_policies
                SET last_run_at = now(),
                    last_status = $2,
                    last_error = NULL,
                    updated_at = now()
              WHERE policy_id = $1::uuid",
            &[policy.policy_id.clone().into(), disabled.into()],
        )
        .map_err(|err| {
            safety::GraphError::Internal(format!("disabled policy status update failed: {err}"))
        })?;
        let row = job_run_row_by_run_id(&row.run_id)?;
        return Ok(JobRunRow {
            graph_name: graph.graph_name,
            ..row
        });
    }

    if !try_acquire_job_lock(&policy.graph_id, &policy.job_id)? {
        let skipped = JobStatus::LockSkipped.as_str();
        let row = insert_job_run(
            &policy.job_id,
            &policy.graph_id,
            skipped,
            retry_count,
            execution_mode,
        )?;
        let message = "job advisory lock is already held";
        Spi::run_with_args(
            "UPDATE graph._job_runs
                SET error = $2,
                    finished_at = now()
              WHERE run_id = $1::uuid",
            &[row.run_id.clone().into(), message.into()],
        )
        .map_err(|err| {
            safety::GraphError::Internal(format!("lock-skipped job run update failed: {err}"))
        })?;
        Spi::run_with_args(
            "UPDATE graph._jobs
                SET last_run_at = now(),
                    last_status = $2,
                    last_error = $3,
                    last_sqlstate = NULL,
                    updated_at = now()
              WHERE job_id = $1::uuid",
            &[policy.job_id.clone().into(), skipped.into(), message.into()],
        )
        .map_err(|err| {
            safety::GraphError::Internal(format!("lock-skipped job status update failed: {err}"))
        })?;
        Spi::run_with_args(
            "UPDATE graph._sync_policies
                SET last_run_at = now(),
                    last_status = $2,
                    last_error = $3,
                    updated_at = now()
              WHERE policy_id = $1::uuid",
            &[
                policy.policy_id.clone().into(),
                skipped.into(),
                message.into(),
            ],
        )
        .map_err(|err| {
            safety::GraphError::Internal(format!("lock-skipped policy status update failed: {err}"))
        })?;
        let row = job_run_row_by_run_id(&row.run_id)?;
        return Ok(JobRunRow {
            graph_name: graph.graph_name,
            ..row
        });
    }

    let running = JobStatus::Running.as_str();
    let row = insert_job_run(
        &policy.job_id,
        &policy.graph_id,
        running,
        retry_count,
        execution_mode,
    )?;
    match apply_sync_internal() {
        Ok(stats) => {
            let rows_applied = stats.inserts + stats.updates + stats.deletes;
            complete_sync_policy_run(&policy, &row.run_id, rows_applied)
        }
        Err(err) => {
            let _ = fail_sync_policy_run(&policy, &row.run_id, &err, retry_count, job.max_retries);
            Err(err)
        }
    }
}

fn run_job_result(job: &GenericJobRow, execution_mode: &str) -> safety::GraphResult<JobRunRow> {
    require_admin_for_graph_id(&job.graph_id)?;
    if job.policy_kind != "sync_policy" {
        return Err(safety::GraphError::UnsupportedOperation {
            operation: "run_job".to_string(),
            reason: format!("policy kind '{}' is not executable", job.policy_kind),
        });
    }
    let policy_id = Spi::get_one_with_args::<String>(
        "SELECT policy_id::text FROM graph._sync_policies WHERE job_id = $1::uuid",
        &[job.job_id.as_str().into()],
    )
    .map_err(|err| safety::GraphError::Internal(format!("sync policy lookup failed: {err}")))?
    .ok_or_else(|| {
        safety::GraphError::Internal(format!("job '{}' has no sync policy", job.job_id))
    })?;
    run_sync_policy_with_mode(&policy_id, execution_mode)
}

fn run_due_jobs_result(
    max_jobs: i32,
    execution_mode: &str,
) -> safety::GraphResult<Vec<(GenericJobRow, JobRunRow)>> {
    let max_jobs = validate_positive_i32(max_jobs, "max_jobs")?;
    let jobs = due_generic_job_rows(max_jobs)?;
    let mut rows = Vec::with_capacity(jobs.len());
    for job in jobs {
        let row = run_job_result(&job, execution_mode)?;
        rows.push((job, row));
    }
    Ok(rows)
}

/// Add an explicit sync policy for a graph.
///
/// Sync policies are durable records. They are executed by calling
/// `graph.run_sync_policy()` or `graph.run_job()`.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn add_sync_policy(
    graph_name: &str,
    schedule_interval_secs: default!(i64, 60),
    max_sync_lag_rows: default!(Option<i64>, "NULL"),
    enabled: default!(bool, true),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(policy_id, String),
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(enabled, bool),
        name!(schedule_interval_secs, i64),
        name!(max_sync_lag_rows, Option<i64>),
        name!(next_run_at, Option<TimestampWithTimeZone>),
        name!(last_status, Option<String>),
    ),
> {
    with_panic_boundary("add_sync_policy()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        catalog::require_graph_privilege(&graph, catalog::GraphPrivilege::Admin)
            .unwrap_or_else(|err| err.report());
        let schedule_interval_secs =
            validate_positive_i64(schedule_interval_secs, "schedule_interval_secs")
                .unwrap_or_else(|err| err.report());
        let max_sync_lag_rows = max_sync_lag_rows
            .map(|value| validate_nonnegative_i64(value, "max_sync_lag_rows"))
            .transpose()
            .unwrap_or_else(|err| err.report());
        catalog::enforce_graph_job_quota().unwrap_or_else(|err| err.report());
        let row = Spi::connect(|client| {
            let mut selected = client.select(
                "WITH inserted_job AS (
                    INSERT INTO graph._jobs (
                        graph_id, policy_kind, enabled, schedule_interval_secs,
                        next_run_at, last_status
                    )
                    VALUES (
                        $1::uuid, 'sync_policy', $2, $3,
                        now() + ($3::bigint * interval '1 second'), 'queued'
                    )
                    RETURNING job_id, graph_id, enabled, schedule_interval_secs, next_run_at
                 ),
                 inserted_policy AS (
                    INSERT INTO graph._sync_policies (
                        graph_id, job_id, schedule_interval_secs, max_sync_lag_rows,
                        enabled, next_run_at, last_status
                    )
                    SELECT graph_id, job_id, schedule_interval_secs, $4::bigint,
                           enabled, next_run_at, 'queued'
                      FROM inserted_job
                    RETURNING policy_id, job_id, graph_id, enabled,
                              schedule_interval_secs, max_sync_lag_rows, next_run_at,
                              last_status
                 )
                 SELECT policy_id::text, job_id::text, graph_id::text, enabled,
                        schedule_interval_secs, max_sync_lag_rows, next_run_at,
                        last_status
                   FROM inserted_policy",
                None,
                &[
                    graph.graph_id.clone().into(),
                    enabled.into(),
                    schedule_interval_secs.into(),
                    max_sync_lag_rows.into(),
                ],
            )?;
            let row = selected
                .next()
                .ok_or(pgrx::spi::SpiError::InvalidPosition)?;
            Ok::<_, pgrx::spi::SpiError>((
                row.get::<String>(1)?.unwrap_or_default(),
                row.get::<String>(2)?.unwrap_or_default(),
                row.get::<String>(3)?.unwrap_or_default(),
                row.get::<bool>(4)?.unwrap_or(enabled),
                row.get::<i64>(5)?.unwrap_or(schedule_interval_secs),
                row.get::<i64>(6)?,
                row.get::<TimestampWithTimeZone>(7)?,
                row.get::<String>(8)?,
            ))
        })
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("sync policy creation failed: {err}")).report()
        });
        TableIterator::new(vec![(
            row.0,
            row.1,
            row.2,
            graph.graph_name,
            row.3,
            row.4,
            row.5,
            row.6,
            row.7,
        )])
    })
}

/// Alter an explicit sync policy.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn alter_sync_policy(
    policy_id: &str,
    schedule_interval_secs: default!(Option<i64>, "NULL"),
    max_sync_lag_rows: default!(Option<i64>, "NULL"),
    enabled: default!(Option<bool>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(policy_id, String),
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(enabled, bool),
        name!(schedule_interval_secs, i64),
        name!(max_sync_lag_rows, Option<i64>),
        name!(next_run_at, Option<TimestampWithTimeZone>),
        name!(last_status, Option<String>),
    ),
> {
    with_panic_boundary("alter_sync_policy()", || {
        let policy = sync_policy_row(policy_id)
            .unwrap_or_else(|err| err.report())
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("sync policy '{policy_id}' does not exist"),
                }
                .report()
            });
        require_admin_for_graph_id(&policy.graph_id).unwrap_or_else(|err| err.report());
        let schedule_interval_secs = schedule_interval_secs
            .map(|value| validate_positive_i64(value, "schedule_interval_secs"))
            .transpose()
            .unwrap_or_else(|err| err.report());
        let max_sync_lag_rows = max_sync_lag_rows
            .map(|value| validate_nonnegative_i64(value, "max_sync_lag_rows"))
            .transpose()
            .unwrap_or_else(|err| err.report());
        Spi::run_with_args(
            "UPDATE graph._sync_policies
                SET schedule_interval_secs = COALESCE($2::bigint, schedule_interval_secs),
                    max_sync_lag_rows = COALESCE($3::bigint, max_sync_lag_rows),
                    enabled = COALESCE($4::boolean, enabled),
                    next_run_at = CASE
                        WHEN $2::bigint IS NULL THEN next_run_at
                        ELSE now() + ($2::bigint * interval '1 second')
                    END,
                    updated_at = now()
              WHERE policy_id = $1::uuid",
            &[
                policy_id.into(),
                schedule_interval_secs.into(),
                max_sync_lag_rows.into(),
                enabled.into(),
            ],
        )
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("sync policy update failed: {err}")).report()
        });
        Spi::run_with_args(
            "UPDATE graph._jobs
                SET schedule_interval_secs = COALESCE($2::bigint, schedule_interval_secs),
                    enabled = COALESCE($3::boolean, enabled),
                    next_run_at = CASE
                        WHEN $2::bigint IS NULL THEN next_run_at
                        ELSE now() + ($2::bigint * interval '1 second')
                    END,
                    updated_at = now()
              WHERE job_id = $1::uuid",
            &[
                policy.job_id.clone().into(),
                schedule_interval_secs.into(),
                enabled.into(),
            ],
        )
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("job update failed: {err}")).report()
        });
        let row = sync_policy_row(policy_id)
            .unwrap_or_else(|err| err.report())
            .unwrap_or_else(|| {
                safety::GraphError::Internal(format!(
                    "sync policy '{policy_id}' disappeared after update"
                ))
                .report()
            });
        TableIterator::new(vec![(
            row.policy_id,
            row.job_id,
            row.graph_id,
            row.graph_name,
            row.enabled,
            row.schedule_interval_secs,
            row.max_sync_lag_rows,
            row.next_run_at,
            row.last_status,
        )])
    })
}

/// Drop an explicit sync policy and its backing job.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn drop_sync_policy(
    policy_id: &str,
) -> TableIterator<
    'static,
    (
        name!(policy_id, String),
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(dropped, bool),
    ),
> {
    with_panic_boundary("drop_sync_policy()", || {
        let policy = sync_policy_row(policy_id)
            .unwrap_or_else(|err| err.report())
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("sync policy '{policy_id}' does not exist"),
                }
                .report()
            });
        require_admin_for_graph_id(&policy.graph_id).unwrap_or_else(|err| err.report());
        Spi::run_with_args(
            "DELETE FROM graph._jobs WHERE job_id = $1::uuid",
            &[policy.job_id.clone().into()],
        )
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("sync policy drop failed: {err}")).report()
        });
        TableIterator::new(vec![(
            policy.policy_id,
            policy.job_id,
            policy.graph_id,
            policy.graph_name,
            true,
        )])
    })
}

/// Run an explicit sync policy immediately.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn run_sync_policy(
    policy_id: &str,
) -> TableIterator<
    'static,
    (
        name!(policy_id, String),
        name!(job_id, String),
        name!(run_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(status, String),
        name!(rows_applied, Option<i64>),
        name!(error, Option<String>),
        name!(started_at, TimestampWithTimeZone),
        name!(finished_at, Option<TimestampWithTimeZone>),
    ),
> {
    with_panic_boundary("run_sync_policy()", || {
        let policy = sync_policy_row(policy_id)
            .unwrap_or_else(|err| err.report())
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("sync policy '{policy_id}' does not exist"),
                }
                .report()
            });
        let row = run_sync_policy_result(policy_id).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            policy.policy_id,
            row.job_id,
            row.run_id,
            row.graph_id,
            row.graph_name,
            row.status,
            row.rows_applied,
            row.error,
            row.started_at,
            row.finished_at,
        )])
    })
}

/// List sync policies visible to the current role.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn sync_policy_status(
    graph_name: default!(Option<&str>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
    max_rows: default!(i32, 50),
) -> TableIterator<
    'static,
    (
        name!(policy_id, String),
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(enabled, bool),
        name!(schedule_interval_secs, i64),
        name!(max_sync_lag_rows, Option<i64>),
        name!(next_run_at, Option<TimestampWithTimeZone>),
        name!(last_run_at, Option<TimestampWithTimeZone>),
        name!(last_status, Option<String>),
        name!(last_error, Option<String>),
    ),
> {
    with_panic_boundary("sync_policy_status()", || {
        let graph_id = graph_name.map(|name| {
            resolve_graph_for_registration(name, graph_tenant, graph_namespace).graph_id
        });
        let rows = sync_policy_rows(None, graph_id.as_deref(), max_rows)
            .unwrap_or_else(|err| err.report());
        TableIterator::new(rows.into_iter().map(|row| {
            (
                row.policy_id,
                row.job_id,
                row.graph_id,
                row.graph_name,
                row.enabled,
                row.schedule_interval_secs,
                row.max_sync_lag_rows,
                row.next_run_at,
                row.last_run_at,
                row.last_status,
                row.last_error,
            )
        }))
    })
}

/// List durable jobs visible to the current role.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn jobs(
    graph_name: default!(Option<&str>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
    max_rows: default!(i32, 50),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(policy_kind, String),
        name!(enabled, bool),
        name!(schedule_interval_secs, i64),
        name!(max_runtime_secs, Option<i64>),
        name!(max_retries, i32),
        name!(next_run_at, Option<TimestampWithTimeZone>),
        name!(last_run_at, Option<TimestampWithTimeZone>),
        name!(last_status, Option<String>),
        name!(last_error, Option<String>),
        name!(last_sqlstate, Option<String>),
    ),
> {
    with_panic_boundary("jobs()", || {
        let graph_id = graph_name.map(|name| {
            resolve_graph_for_registration(name, graph_tenant, graph_namespace).graph_id
        });
        let rows = generic_job_rows(None, graph_id.as_deref(), max_rows)
            .unwrap_or_else(|err| err.report());
        TableIterator::new(rows.into_iter().map(|row| {
            (
                row.job_id,
                row.graph_id,
                row.graph_name,
                row.policy_kind,
                row.enabled,
                row.schedule_interval_secs,
                row.max_runtime_secs,
                row.max_retries,
                row.next_run_at,
                row.last_run_at,
                row.last_status,
                row.last_error,
                row.last_sqlstate,
            )
        }))
    })
}

/// List durable job run history visible to the current role.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn job_runs(
    job_id: default!(Option<&str>, "NULL"),
    graph_name: default!(Option<&str>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
    max_rows: default!(i32, 50),
) -> TableIterator<
    'static,
    (
        name!(run_id, String),
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(status, String),
        name!(rows_applied, Option<i64>),
        name!(retry_count, i32),
        name!(execution_mode, String),
        name!(sqlstate, Option<String>),
        name!(error, Option<String>),
        name!(started_at, TimestampWithTimeZone),
        name!(finished_at, Option<TimestampWithTimeZone>),
    ),
> {
    with_panic_boundary("job_runs()", || {
        let graph_id = graph_name.map(|name| {
            resolve_graph_for_registration(name, graph_tenant, graph_namespace).graph_id
        });
        let rows =
            job_run_rows(job_id, graph_id.as_deref(), max_rows).unwrap_or_else(|err| err.report());
        TableIterator::new(rows.into_iter().map(|row| {
            (
                row.run_id,
                row.job_id,
                row.graph_id,
                row.graph_name,
                row.status,
                row.rows_applied,
                row.retry_count,
                row.execution_mode,
                row.sqlstate,
                row.error,
                row.started_at,
                row.finished_at,
            )
        }))
    })
}

/// Summarize durable job outcomes visible to the current role.
#[pg_extern(schema = "graph")]
fn job_stats(
    graph_name: default!(Option<&str>, "NULL"),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(graph_id, String),
        name!(graph_name, String),
        name!(policy_kind, String),
        name!(job_count, i64),
        name!(run_count, i64),
        name!(completed_runs, i64),
        name!(failed_runs, i64),
        name!(last_run_at, Option<TimestampWithTimeZone>),
    ),
> {
    with_panic_boundary("job_stats()", || {
        let graph_id = graph_name.map(|name| {
            resolve_graph_for_registration(name, graph_tenant, graph_namespace).graph_id
        });
        let visible_graph_ids = visible_graph_ids().unwrap_or_else(|err| err.report());
        let rows = Spi::connect(|client| {
            let selected = client.select(
                "SELECT j.graph_id::text, g.graph_name, j.policy_kind,
                        count(DISTINCT j.job_id)::bigint AS job_count,
                        count(r.run_id)::bigint AS run_count,
                        count(r.run_id) FILTER (WHERE r.status = 'completed')::bigint AS completed_runs,
                        count(r.run_id) FILTER (
                            WHERE r.status IN (
                                'failed', 'retryable_failed',
                                'permanent_failed', 'quota_blocked'
                            )
                        )::bigint AS failed_runs,
                        max(r.started_at) AS last_run_at
                  FROM graph._jobs j
                  JOIN graph._graphs g ON g.graph_id = j.graph_id
                  LEFT JOIN graph._job_runs r ON r.job_id = j.job_id
                  WHERE ($1::uuid IS NULL OR j.graph_id = $1::uuid)
                    AND j.graph_id::text = ANY($2::text[])
                  GROUP BY j.graph_id, g.graph_name, j.policy_kind
                  ORDER BY g.graph_name, j.policy_kind",
                None,
                &[graph_id.as_deref().into(), visible_graph_ids.into()],
            )?;
            let mut out = Vec::new();
            for row in selected {
                out.push((
                    row.get::<String>(1)?.unwrap_or_default(),
                    row.get::<String>(2)?.unwrap_or_default(),
                    row.get::<String>(3)?.unwrap_or_default(),
                    row.get::<i64>(4)?.unwrap_or(0),
                    row.get::<i64>(5)?.unwrap_or(0),
                    row.get::<i64>(6)?.unwrap_or(0),
                    row.get::<i64>(7)?.unwrap_or(0),
                    row.get::<TimestampWithTimeZone>(8)?,
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(out)
        })
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("job stats read failed: {err}")).report()
        });
        TableIterator::new(rows)
    })
}

/// Run a durable job immediately.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn run_job(
    job_id: &str,
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(run_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(policy_kind, String),
        name!(status, String),
        name!(rows_applied, Option<i64>),
        name!(error, Option<String>),
        name!(started_at, TimestampWithTimeZone),
        name!(finished_at, Option<TimestampWithTimeZone>),
    ),
> {
    with_panic_boundary("run_job()", || {
        let job = generic_job_rows(Some(job_id), None, 1)
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("job '{job_id}' does not exist"),
                }
                .report()
            });
        let row = run_job_result(&job, "hosted").unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            row.job_id,
            row.run_id,
            row.graph_id,
            row.graph_name,
            job.policy_kind,
            row.status,
            row.rows_applied,
            row.error,
            row.started_at,
            row.finished_at,
        )])
    })
}

/// Run due durable jobs through the hosted scheduler path.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn run_due_jobs(
    max_jobs: default!(i32, 64),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(run_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(policy_kind, String),
        name!(status, String),
        name!(rows_applied, Option<i64>),
        name!(error, Option<String>),
        name!(started_at, TimestampWithTimeZone),
        name!(finished_at, Option<TimestampWithTimeZone>),
    ),
> {
    with_panic_boundary("run_due_jobs()", || {
        let rows = run_due_jobs_result(max_jobs, "hosted").unwrap_or_else(|err| err.report());
        TableIterator::new(rows.into_iter().map(|(job, row)| {
            (
                row.job_id,
                row.run_id,
                row.graph_id,
                row.graph_name,
                job.policy_kind,
                row.status,
                row.rows_applied,
                row.error,
                row.started_at,
                row.finished_at,
            )
        }))
    })
}

/// Launch one internal worker pass for due durable jobs.
#[pg_extern(schema = "graph")]
fn run_due_jobs_async(max_jobs: default!(i32, 64)) -> bool {
    with_panic_boundary("run_due_jobs_async()", || {
        let max_jobs =
            validate_positive_i32(max_jobs, "max_jobs").unwrap_or_else(|err| err.report());
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        launch_due_jobs_worker(max_jobs).unwrap_or_else(|err| err.report());
        true
    })
}

/// Alter a durable job.
#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn alter_job(
    job_id: &str,
    enabled: default!(Option<bool>, "NULL"),
    schedule_interval_secs: default!(Option<i64>, "NULL"),
    max_runtime_secs: default!(Option<i64>, "NULL"),
    max_retries: default!(Option<i32>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(policy_kind, String),
        name!(enabled, bool),
        name!(schedule_interval_secs, i64),
        name!(max_runtime_secs, Option<i64>),
        name!(max_retries, i32),
        name!(next_run_at, Option<TimestampWithTimeZone>),
        name!(last_status, Option<String>),
    ),
> {
    with_panic_boundary("alter_job()", || {
        let job = generic_job_rows(Some(job_id), None, 1)
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("job '{job_id}' does not exist"),
                }
                .report()
            });
        require_admin_for_graph_id(&job.graph_id).unwrap_or_else(|err| err.report());
        let schedule_interval_secs = schedule_interval_secs
            .map(|value| validate_positive_i64(value, "schedule_interval_secs"))
            .transpose()
            .unwrap_or_else(|err| err.report());
        let max_runtime_secs = max_runtime_secs
            .map(|value| validate_positive_i64(value, "max_runtime_secs"))
            .transpose()
            .unwrap_or_else(|err| err.report());
        let max_retries = max_retries
            .map(|value| validate_nonnegative_i32(value, "max_retries"))
            .transpose()
            .unwrap_or_else(|err| err.report());
        Spi::run_with_args(
            "UPDATE graph._jobs
                SET enabled = COALESCE($2::boolean, enabled),
                    schedule_interval_secs = COALESCE($3::bigint, schedule_interval_secs),
                    max_runtime_secs = COALESCE($4::bigint, max_runtime_secs),
                    max_retries = COALESCE($5::integer, max_retries),
                    next_run_at = CASE
                        WHEN $3::bigint IS NULL THEN next_run_at
                        ELSE now() + ($3::bigint * interval '1 second')
                    END,
                    updated_at = now()
              WHERE job_id = $1::uuid",
            &[
                job_id.into(),
                enabled.into(),
                schedule_interval_secs.into(),
                max_runtime_secs.into(),
                max_retries.into(),
            ],
        )
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("job update failed: {err}")).report()
        });
        if job.policy_kind == "sync_policy" {
            Spi::run_with_args(
                "UPDATE graph._sync_policies
                    SET enabled = COALESCE($2::boolean, enabled),
                        schedule_interval_secs = COALESCE($3::bigint, schedule_interval_secs),
                        next_run_at = CASE
                            WHEN $3::bigint IS NULL THEN next_run_at
                            ELSE now() + ($3::bigint * interval '1 second')
                        END,
                        updated_at = now()
                  WHERE job_id = $1::uuid",
                &[job_id.into(), enabled.into(), schedule_interval_secs.into()],
            )
            .unwrap_or_else(|err| {
                safety::GraphError::Internal(format!("sync policy job update failed: {err}"))
                    .report()
            });
        }
        let row = generic_job_rows(Some(job_id), None, 1)
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                safety::GraphError::Internal(format!("job '{job_id}' disappeared after update"))
                    .report()
            });
        TableIterator::new(vec![(
            row.job_id,
            row.graph_id,
            row.graph_name,
            row.policy_kind,
            row.enabled,
            row.schedule_interval_secs,
            row.max_runtime_secs,
            row.max_retries,
            row.next_run_at,
            row.last_status,
        )])
    })
}

/// Remove a durable job and any dependent policy/run rows.
#[pg_extern(schema = "graph")]
fn remove_job(
    job_id: &str,
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(policy_kind, String),
        name!(removed, bool),
    ),
> {
    with_panic_boundary("remove_job()", || {
        let job = generic_job_rows(Some(job_id), None, 1)
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("job '{job_id}' does not exist"),
                }
                .report()
            });
        require_admin_for_graph_id(&job.graph_id).unwrap_or_else(|err| err.report());
        Spi::run_with_args(
            "DELETE FROM graph._jobs WHERE job_id = $1::uuid",
            &[job_id.into()],
        )
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("job removal failed: {err}")).report()
        });
        TableIterator::new(vec![(
            job.job_id,
            job.graph_id,
            job.graph_name,
            job.policy_kind,
            true,
        )])
    })
}

/// Publish committed sync-log rows into durable projection segments.
#[pg_extern(schema = "graph")]
fn ingest_projection(
    max_rows: default!(Option<i64>, "NULL"),
    max_bytes: default!(Option<i64>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(rows_ingested, i64),
        name!(segments_published, i64),
        name!(sync_watermark, i64),
    ),
> {
    with_panic_boundary("ingest_projection()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let stats =
            ingest_projection_internal(max_rows, max_bytes).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            stats.rows_ingested,
            stats.segments_published,
            stats.sync_watermark,
        )])
    })
}

/// Vacuum the graph by rebuilding from source tables.
///
/// The CSR is immutable, so reclaiming tombstones and merging edge overlays
/// requires reconstructing the active engine.
///
/// **Double memory tax:** During vacuum, both the old and new engine
/// exist in memory simultaneously until the swap completes. Ensure
/// `graph.memory_limit_mb` has ≥2× headroom.
///
/// See: `docs/user_guide/sync-and-maintenance.mdx`
#[pg_extern(schema = "graph")]
fn vacuum() -> TableIterator<
    'static,
    (
        name!(nodes_before, i64),
        name!(nodes_after, i64),
        name!(tombstones_removed, i64),
        name!(edges_rebuilt, i64),
        name!(vacuum_time_ms, f64),
    ),
> {
    with_panic_boundary("vacuum()", || {
        require_selected_graph_build_result().unwrap_or_else(|err| err.report());
        let result = execute_vacuum(false).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            result.nodes_before,
            result.nodes_after,
            result.tombstones_removed,
            result.edges_rebuilt,
            result.vacuum_time_ms,
        )])
    })
}

/// Vacuum a named graph without requiring a separate `set_current_graph()`.
#[pg_extern(schema = "graph")]
fn vacuum_graph(
    graph_name: &str,
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(nodes_before, i64),
        name!(nodes_after, i64),
        name!(tombstones_removed, i64),
        name!(edges_rebuilt, i64),
        name!(vacuum_time_ms, f64),
    ),
> {
    with_panic_boundary("vacuum_graph()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        require_graph_build_result(&graph).unwrap_or_else(|err| err.report());
        catalog::set_selected_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report());
        let result = execute_vacuum(false).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            result.nodes_before,
            result.nodes_after,
            result.tombstones_removed,
            result.edges_rebuilt,
            result.vacuum_time_ms,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn maintenance(
    concurrently: default!(bool, false),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(status, String),
        name!(sync_rows_applied, Option<i64>),
        name!(nodes_after, Option<i64>),
        name!(edges_after, Option<i64>),
        name!(vacuum_time_ms, Option<f64>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("maintenance()", || {
        require_selected_graph_build_result().unwrap_or_else(|err| err.report());
        if concurrently {
            let job_id = create_maintenance_job().unwrap_or_else(|err| err.report());
            if let Err(err) = launch_maintenance_worker(&job_id) {
                let _ = update_maintenance_job_failed(&job_id, &err.to_string());
                err.report();
            }
            let row = maintenance_job_row(&job_id)
                .unwrap_or_else(|err| err.report())
                .unwrap_or(MaintenanceJobRow {
                    job_id,
                    graph_id: catalog::selected_or_default_graph_metadata()
                        .map(|graph| graph.graph_id)
                        .unwrap_or_default(),
                    status: JobStatus::Queued.as_str().to_string(),
                    sync_rows_applied: None,
                    nodes_after: None,
                    edges_after: None,
                    vacuum_time_ms: None,
                    progress_phase: JobStatus::Queued.as_str().to_string(),
                    progress_message: Some("queued for background maintenance".to_string()),
                    started_at: None,
                    finished_at: None,
                    error: None,
                });
            return TableIterator::new(vec![(
                row.job_id,
                row.status,
                row.sync_rows_applied,
                row.nodes_after,
                row.edges_after,
                row.vacuum_time_ms,
                row.error,
            )]);
        }

        let result = execute_maintenance_rebuild(true).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            "00000000-0000-0000-0000-000000000000".to_string(),
            JobStatus::Completed.as_str().to_string(),
            Some(result.sync_rows_applied),
            Some(result.nodes_after),
            Some(result.edges_after),
            Some(result.vacuum_time_ms),
            None,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn maintenance_graph(
    graph_name: &str,
    concurrently: default!(bool, false),
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(status, String),
        name!(sync_rows_applied, Option<i64>),
        name!(nodes_after, Option<i64>),
        name!(edges_after, Option<i64>),
        name!(vacuum_time_ms, Option<f64>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("maintenance_graph()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        require_graph_build_result(&graph).unwrap_or_else(|err| err.report());
        catalog::set_selected_graph_id(&graph.graph_id).unwrap_or_else(|err| err.report());
        if concurrently {
            let job_id = create_maintenance_job().unwrap_or_else(|err| err.report());
            if let Err(err) = launch_maintenance_worker(&job_id) {
                let _ = update_maintenance_job_failed(&job_id, &err.to_string());
                err.report();
            }
            let row = maintenance_job_row(&job_id)
                .unwrap_or_else(|err| err.report())
                .unwrap_or(MaintenanceJobRow {
                    job_id,
                    graph_id: graph.graph_id.clone(),
                    status: JobStatus::Queued.as_str().to_string(),
                    sync_rows_applied: None,
                    nodes_after: None,
                    edges_after: None,
                    vacuum_time_ms: None,
                    progress_phase: JobStatus::Queued.as_str().to_string(),
                    progress_message: Some("queued for background maintenance".to_string()),
                    started_at: None,
                    finished_at: None,
                    error: None,
                });
            return TableIterator::new(vec![(
                row.job_id,
                graph.graph_id,
                graph.graph_name,
                row.status,
                row.sync_rows_applied,
                row.nodes_after,
                row.edges_after,
                row.vacuum_time_ms,
                row.error,
            )]);
        }

        let result = execute_maintenance_rebuild(true).unwrap_or_else(|err| err.report());
        TableIterator::new(vec![(
            "00000000-0000-0000-0000-000000000000".to_string(),
            graph.graph_id,
            graph.graph_name,
            JobStatus::Completed.as_str().to_string(),
            Some(result.sync_rows_applied),
            Some(result.nodes_after),
            Some(result.edges_after),
            Some(result.vacuum_time_ms),
            None,
        )])
    })
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn maintenance_status(
    job_id: default!(Option<&str>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(status, String),
        name!(sync_rows_applied, Option<i64>),
        name!(nodes_after, Option<i64>),
        name!(edges_after, Option<i64>),
        name!(vacuum_time_ms, Option<f64>),
        name!(progress_phase, String),
        name!(progress_message, Option<String>),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("maintenance_status()", || {
        let selected_graph_id = catalog::selected_or_default_graph_metadata()
            .unwrap_or_else(|err| err.report())
            .graph_id;
        if let Some(job_id) = job_id {
            if let Some(row) = maintenance_job_row(job_id).unwrap_or_else(|err| err.report()) {
                if row.graph_id != selected_graph_id {
                    return maintenance_not_found_status(job_id);
                }
                return TableIterator::new(vec![(
                    row.job_id,
                    row.status,
                    row.sync_rows_applied,
                    row.nodes_after,
                    row.edges_after,
                    row.vacuum_time_ms,
                    row.progress_phase,
                    row.progress_message,
                    row.started_at,
                    row.finished_at,
                    row.error,
                )]);
            }
            return maintenance_not_found_status(job_id);
        }

        let rows = Spi::connect(|client| {
            let selected = client.select(
                "SELECT job_id, status, sync_rows_applied, nodes_after, edges_after,
                        vacuum_time_ms, progress_phase, progress_message,
                        started_at, finished_at, error
                 FROM graph._maintenance_jobs
                 WHERE graph_id = $1::uuid
                 ORDER BY created_at DESC
                 LIMIT 50",
                None,
                &[selected_graph_id.into()],
            )?;
            let mut out = Vec::new();
            for row in selected {
                out.push((
                    row.get::<String>(1)?.unwrap_or_default(),
                    row.get::<String>(2)?
                        .unwrap_or_else(|| "not_found".to_string()),
                    row.get::<i64>(3)?,
                    row.get::<i64>(4)?,
                    row.get::<i64>(5)?,
                    row.get::<f64>(6)?,
                    row.get::<String>(7)?
                        .unwrap_or_else(|| "unknown".to_string()),
                    row.get::<String>(8)?,
                    row.get::<TimestampWithTimeZone>(9)?,
                    row.get::<TimestampWithTimeZone>(10)?,
                    row.get::<String>(11)?,
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(out)
        })
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("maintenance status read failed: {}", err))
                .report()
        });
        TableIterator::new(rows)
    })
}

fn maintenance_not_found_status(
    job_id: &str,
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(status, String),
        name!(sync_rows_applied, Option<i64>),
        name!(nodes_after, Option<i64>),
        name!(edges_after, Option<i64>),
        name!(vacuum_time_ms, Option<f64>),
        name!(progress_phase, String),
        name!(progress_message, Option<String>),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(error, Option<String>),
    ),
> {
    TableIterator::new(vec![(
        job_id.to_string(),
        "not_found".to_string(),
        None,
        None,
        None,
        None,
        "not_found".to_string(),
        None,
        None,
        None,
        None,
    )])
}

#[pg_extern(schema = "graph")]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL ABI row shape is intentionally explicit"
)]
fn maintenance_status_for_graph(
    graph_name: &str,
    graph_tenant: default!(Option<&str>, "NULL"),
    graph_namespace: default!(Option<&str>, "NULL"),
    max_rows: default!(i32, 50),
) -> TableIterator<
    'static,
    (
        name!(job_id, String),
        name!(graph_id, String),
        name!(graph_name, String),
        name!(status, String),
        name!(sync_rows_applied, Option<i64>),
        name!(nodes_after, Option<i64>),
        name!(edges_after, Option<i64>),
        name!(vacuum_time_ms, Option<f64>),
        name!(progress_phase, String),
        name!(progress_message, Option<String>),
        name!(started_at, Option<TimestampWithTimeZone>),
        name!(finished_at, Option<TimestampWithTimeZone>),
        name!(error, Option<String>),
    ),
> {
    with_panic_boundary("maintenance_status_for_graph()", || {
        let graph = resolve_graph_for_registration(graph_name, graph_tenant, graph_namespace);
        let limit = max_rows.clamp(1, 500);
        let rows = Spi::connect(|client| {
            let selected = client.select(
                "SELECT m.job_id, m.graph_id::text, g.graph_name, m.status,
                        m.sync_rows_applied, m.nodes_after, m.edges_after,
                        m.vacuum_time_ms, m.progress_phase, m.progress_message,
                        m.started_at, m.finished_at, m.error
                   FROM graph._maintenance_jobs m
                   JOIN graph._graphs g ON g.graph_id = m.graph_id
                  WHERE m.graph_id = $1::uuid
                  ORDER BY m.created_at DESC
                  LIMIT $2",
                None,
                &[graph.graph_id.into(), limit.into()],
            )?;
            let mut out = Vec::new();
            for row in selected {
                out.push((
                    row.get::<String>(1)?.unwrap_or_default(),
                    row.get::<String>(2)?.unwrap_or_default(),
                    row.get::<String>(3)?.unwrap_or_default(),
                    row.get::<String>(4)?
                        .unwrap_or_else(|| "not_found".to_string()),
                    row.get::<i64>(5)?,
                    row.get::<i64>(6)?,
                    row.get::<i64>(7)?,
                    row.get::<f64>(8)?,
                    row.get::<String>(9)?
                        .unwrap_or_else(|| "unknown".to_string()),
                    row.get::<String>(10)?,
                    row.get::<TimestampWithTimeZone>(11)?,
                    row.get::<TimestampWithTimeZone>(12)?,
                    row.get::<String>(13)?,
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(out)
        })
        .unwrap_or_else(|err| {
            safety::GraphError::Internal(format!("maintenance status read failed: {}", err))
                .report()
        });
        TableIterator::new(rows)
    })
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_run_build_job")]
fn test_run_build_job(build_id: &str) -> Option<String> {
    with_panic_boundary("_test_run_build_job()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        run_build_job(build_id).err().map(|err| {
            let message = err.to_string();
            if let Err(record_err) = update_build_job_failed(build_id, &message) {
                pgrx::warning!(
                    "graph test build job {} failed and failure status could not be recorded: {}",
                    build_id,
                    record_err
                );
            }
            message
        })
    })
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_run_maintenance_job")]
fn test_run_maintenance_job(job_id: &str) -> Option<String> {
    with_panic_boundary("_test_run_maintenance_job()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        run_maintenance_job(job_id).err().map(|err| {
            let message = err.to_string();
            if let Err(record_err) = update_maintenance_job_failed(job_id, &message) {
                pgrx::warning!(
                    "graph test maintenance job {} failed and failure status could not be recorded: {}",
                    job_id,
                    record_err
                );
            }
            message
        })
    })
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_run_due_jobs_internal")]
fn test_run_due_jobs_internal(max_jobs: default!(i32, 64)) -> Option<String> {
    with_panic_boundary("_test_run_due_jobs_internal()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        run_due_jobs_result(max_jobs, "internal")
            .err()
            .map(|err| err.to_string())
    })
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_run_job_internal")]
fn test_run_job_internal(job_id: &str) -> Option<String> {
    with_panic_boundary("_test_run_job_internal()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let job = generic_job_rows(Some(job_id), None, 1)
            .unwrap_or_else(|err| err.report())
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                safety::GraphError::InvalidFilter {
                    reason: format!("job '{job_id}' does not exist"),
                }
                .report()
            });
        run_job_result(&job, "internal")
            .err()
            .map(|err| err.to_string())
    })
}

fn cached_estimated_table_rows(
    table_counts: &mut std::collections::HashMap<String, i64>,
    table_name: &str,
) -> i64 {
    if let Some(count) = table_counts.get(table_name) {
        return *count;
    }
    let count = catalog::estimated_table_rows(table_name).unwrap_or(0);
    table_counts.insert(table_name.to_string(), count);
    count
}

/// Enable trigger-based sync for all registered tables.
///
/// Ensures sync catalog tables exist and attaches triggers that write to
/// `graph._sync_log`.
#[pg_extern(schema = "graph")]
fn enable_sync() {
    with_panic_boundary("enable_sync()", || {
        require_graph_admin_result().unwrap_or_else(|err| err.report());
        let installed = install_sync_triggers().unwrap_or_else(|err| err.report());
        pgrx::notice!("graph: sync enabled for {} tables", installed);
    });
}
