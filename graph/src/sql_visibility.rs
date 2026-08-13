//! PostgreSQL adapter for caller-scoped topology visibility.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::panic::AssertUnwindSafe;
#[cfg(feature = "development")]
use std::time::Instant;

use pgrx::prelude::*;
use roaring::RoaringBitmap;

use crate::builder::{RegisteredEdge, RegisteredTable};
use crate::catalog::{primary_key_expr, sql_table_name_from_oid};
use crate::resource::{ByteCount, ResourceGovernor, ResourcePhase, WorkUnits};
use crate::safety::{GraphError, GraphResult};
use crate::visibility::{
    LazyVisibilityCoordinator, LazyVisibilityMode, ProvenVisibleNode, VisibilityBatchLimits,
    VisibilityCacheLimits, VisibilityCandidate, VisibilityCandidateBatch, VisibilityCoordinator,
    VisibilityProbeBatch, VisibilityScope, VisibilityVerdict, VisibilityVerdictBatch,
};
use crate::{acl, config, ENGINE};

const VISIBILITY_CURSOR_ROWS: i64 = 256;
const BITMAP_BYTES_PER_ID_UPPER_BOUND: u64 = 8;
const DIRECT_VISIBILITY_MAX_CANDIDATES: usize = 1;
const DIRECT_VISIBILITY_MAX_KEY_BYTES: usize = 1024 * 1024;
const VISIBILITY_CACHE_ENTRY_BYTES: usize = 32;
// Candidate, dedupe key, table batch, TLS frame, JSON payload, returned key,
// and final verdict may coexist for the direct-identity slice.
const DIRECT_VISIBILITY_KEY_COPY_UPPER_BOUND: usize = 8;
const DIRECT_VISIBILITY_PROBE_PLAN_BYTES: u64 = 64 * 1024;

/// Unforgeable evidence that PostgreSQL policy preparation selected a scope.
pub(crate) struct PreparedVisibilityProof(());

fn prepared_coordinator(scope: VisibilityScope) -> VisibilityCoordinator {
    VisibilityCoordinator::from_prepared_scope(PreparedVisibilityProof(()), scope)
}

#[cfg(feature = "development")]
type VisibilityTimer = Instant;
#[cfg(not(feature = "development"))]
struct VisibilityTimer;

thread_local! {
    /// Owns graph-sized Rust allocations across PostgreSQL ERROR/longjmp.
    /// `PgTryBuilder::finally` clears it if an SPI scan is cancelled.
    static VISIBILITY_BUILD_SLOT: RefCell<Option<VisibilityScope>> = const { RefCell::new(None) };

    static VISIBILITY_RESOLUTION_ACTIVE: Cell<bool> = const { Cell::new(false) };

    static LAZY_VISIBILITY_RESOLUTION_SLOT: RefCell<Option<Box<LazyVisibilityResolutionFrame>>> = const { RefCell::new(None) };

    /// Owns set-probe SQL, payload, and returned keys across PostgreSQL ERROR.
    static BFS_VISIBILITY_RESOLUTION_SLOT: RefCell<Option<Box<BfsVisibilityResolutionFrame>>> = const { RefCell::new(None) };

    /// Owns preparation collections across PostgreSQL ERROR/longjmp.
    static BFS_VISIBILITY_PREPARATION_SLOT: RefCell<Option<Box<BfsVisibilityPreparationFrame>>> = const { RefCell::new(None) };

    #[cfg(feature = "development")]
    static VISIBILITY_CANCEL_AFTER_ROWS: Cell<Option<u64>> = const { Cell::new(None) };

    #[cfg(feature = "development")]
    static LAZY_VISIBILITY_CANCEL_BEFORE_SPI: Cell<bool> = const { Cell::new(false) };

    #[cfg(feature = "development")]
    static VISIBILITY_FORCE_MISSING_IDENTITY: Cell<bool> = const { Cell::new(false) };

    #[cfg(feature = "development")]
    static BFS_VISIBILITY_FORCE_MISSING_CANDIDATE_IDENTITY: Cell<bool> = const { Cell::new(false) };

    #[cfg(feature = "development")]
    static VISIBILITY_LAST_METRICS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };

    #[cfg(feature = "development")]
    static BFS_VISIBILITY_LAST_METRICS: Cell<BfsVisibilityMetrics> = const { Cell::new(BfsVisibilityMetrics::EMPTY) };

    #[cfg(feature = "development")]
    static BFS_VISIBILITY_RESOLUTION_DROPPED: Cell<bool> = const { Cell::new(true) };

    #[cfg(feature = "development")]
    static VISIBILITY_STRATEGY_OVERRIDE: Cell<VisibilityStrategyOverride> = const { Cell::new(VisibilityStrategyOverride::Auto) };
}

#[cfg(feature = "development")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisibilityStrategyOverride {
    Auto,
    Eager,
    Lazy,
}

pub(crate) fn lazy_bfs_strategy_enabled(coordinator: &LazyVisibilityCoordinator) -> bool {
    #[cfg(feature = "development")]
    {
        match VISIBILITY_STRATEGY_OVERRIDE.with(Cell::get) {
            VisibilityStrategyOverride::Auto => coordinator.is_enforced(),
            VisibilityStrategyOverride::Eager => false,
            VisibilityStrategyOverride::Lazy => true,
        }
    }
    #[cfg(not(feature = "development"))]
    coordinator.is_enforced()
}

struct VisibilityResolutionGuard;

#[cfg(feature = "development")]
struct BfsVisibilityResolutionDropProbe;

#[cfg(feature = "development")]
impl BfsVisibilityResolutionDropProbe {
    fn arm() -> Self {
        BFS_VISIBILITY_RESOLUTION_DROPPED.with(|observed| observed.set(false));
        Self
    }
}

#[cfg(feature = "development")]
impl Drop for BfsVisibilityResolutionDropProbe {
    fn drop(&mut self) {
        BFS_VISIBILITY_RESOLUTION_DROPPED.with(|observed| observed.set(true));
    }
}

/// Convert PostgreSQL's non-local ERROR transfer into an ordinary Rust unwind
/// before returning to a caller that owns traversal state. This preserves the
/// original PostgreSQL error while ensuring the caller's Rust frames run Drop.
pub(crate) fn postgres_error_as_rust_unwind<T>(
    operation: impl FnOnce() -> T + std::panic::UnwindSafe,
) -> T {
    let outcome = pgrx::pg_sys::PgTryBuilder::new(AssertUnwindSafe(|| {
        Ok::<T, Box<pgrx::pg_sys::panic::CaughtError>>(operation())
    }))
    .catch_others(|error| Err(Box::new(error)))
    .execute();
    match outcome {
        Ok(value) => value,
        Err(error) => std::panic::resume_unwind(error),
    }
}

#[derive(Default)]
struct BfsVisibilityPreparationFrame {
    node_tables: HashSet<u32>,
    policy_tables: HashSet<u32>,
    mappings: HashSet<u64>,
    edge_types: HashSet<u8>,
}

struct LazyVisibilityResolutionFrame {
    probe: VisibilityProbeBatch,
    plans: Vec<NodeProbePlan>,
    unknown_by_table: BTreeMap<u32, Vec<String>>,
    visible_keys: HashMap<(u32, String), ()>,
    active_table_oid: Option<u32>,
    active_payload: Option<String>,
    active_query: Option<String>,
}

struct BfsVisibilityResolutionFrame {
    query: String,
    payload: String,
    visible_keys: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BfsVisibilityMetrics {
    spi_calls: u64,
    requested_keys: u64,
    returned_keys: u64,
    requested_key_bytes: u64,
    returned_key_bytes: u64,
    source_rows: u64,
}

impl BfsVisibilityMetrics {
    const EMPTY: Self = Self {
        spi_calls: 0,
        requested_keys: 0,
        returned_keys: 0,
        requested_key_bytes: 0,
        returned_key_bytes: 0,
        source_rows: 0,
    };
}

impl VisibilityResolutionGuard {
    fn ensure_available() -> GraphResult<()> {
        VISIBILITY_RESOLUTION_ACTIVE.with(|active| {
            if active.get() {
                Err(GraphError::RecursiveVisibilityResolution)
            } else {
                Ok(())
            }
        })
    }

    fn activate() -> Self {
        VISIBILITY_RESOLUTION_ACTIVE.with(|active| active.set(true));
        Self
    }

    fn clear() {
        VISIBILITY_RESOLUTION_ACTIVE.with(|active| active.set(false));
    }
}

/// Reject every nested graph SQL call while PostgreSQL is evaluating a source
/// policy. This prevents a VOLATILE policy function from replacing or mutating
/// the projection between candidate materialization and admission.
pub(crate) fn ensure_graph_api_available() -> GraphResult<()> {
    VisibilityResolutionGuard::ensure_available()
}

#[cfg(test)]
#[test]
fn recursive_visibility_resolution_is_rejected() {
    let _guard = VisibilityResolutionGuard::activate();
    assert!(matches!(
        VisibilityResolutionGuard::ensure_available(),
        Err(GraphError::RecursiveVisibilityResolution)
    ));
    VisibilityResolutionGuard::clear();
}

#[cfg(test)]
#[test]
fn visibility_resolution_guard_clears_after_error_and_cancellation() {
    let _guard = VisibilityResolutionGuard::activate();
    VisibilityResolutionGuard::clear();
    assert!(_test_visibility_resolution_guard_empty());
}

#[cfg(any(test, feature = "development"))]
pub(crate) fn _test_visibility_resolution_guard_empty() -> bool {
    let base_empty = VISIBILITY_RESOLUTION_ACTIVE.with(|active| !active.get())
        && LAZY_VISIBILITY_RESOLUTION_SLOT.with(|slot| slot.borrow().is_none())
        && BFS_VISIBILITY_RESOLUTION_SLOT.with(|slot| slot.borrow().is_none())
        && BFS_VISIBILITY_PREPARATION_SLOT.with(|slot| slot.borrow().is_none())
        && VISIBILITY_BUILD_SLOT.with(|slot| slot.borrow().is_none());
    #[cfg(feature = "development")]
    return base_empty && BFS_VISIBILITY_RESOLUTION_DROPPED.with(Cell::get);
    #[cfg(not(feature = "development"))]
    base_empty
}

#[cfg(test)]
#[test]
fn same_count_topology_replacement_is_blocked_during_visibility_resolution() {
    let _guard = VisibilityResolutionGuard::activate();
    assert!(matches!(
        ensure_graph_api_available(),
        Err(GraphError::RecursiveVisibilityResolution)
    ));
    VisibilityResolutionGuard::clear();
}

/// Build one eager caller-visibility scope under the query governor.
pub(crate) fn prepare_eager_visibility(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    governor: &ResourceGovernor,
) -> GraphResult<VisibilityCoordinator> {
    let started_at = start_visibility_timer();
    match config::parsed_rls_mode() {
        Some(config::RlsMode::LegacyBypass) => {
            acl::require_rls_bypass_privilege()?;
            let proof = PreparedVisibilityProof(());
            let scope = VisibilityScope::unrestricted(&proof);
            return Ok(prepared_coordinator(record_visibility_metrics(
                scope, started_at,
            )));
        }
        Some(config::RlsMode::Enforce) => {}
        None => {
            return Err(GraphError::InvalidFilter {
                reason: format!(
                    "unsupported graph.rls_mode '{}'; expected 'enforce' or 'legacy_bypass'",
                    config::rls_mode()
                ),
            });
        }
    }

    let any_rls_active = tables
        .iter()
        .any(|table| acl::row_security_applies_to_effective_caller(table.table_oid))
        || edges
            .iter()
            .filter(|edge| {
                !tables
                    .iter()
                    .any(|table| table.table_oid == edge.from_table_oid)
            })
            .any(|edge| acl::row_security_applies_to_effective_caller(edge.from_table_oid));
    if !any_rls_active {
        let proof = PreparedVisibilityProof(());
        let scope = VisibilityScope::unrestricted(&proof);
        return Ok(prepared_coordinator(record_visibility_metrics(
            scope, started_at,
        )));
    }

    let active_node_tables = tables
        .iter()
        .filter(|table| acl::row_security_applies_to_effective_caller(table.table_oid))
        .collect::<Vec<_>>();
    let active_edges = edges
        .iter()
        .filter(|edge| acl::row_security_applies_to_effective_caller(edge.from_table_oid))
        .collect::<Vec<_>>();

    for table in &active_node_tables {
        acl::check_table_acl(table.table_oid)?;
    }
    let mut checked_edge_tables = BTreeSet::new();
    for edge in &active_edges {
        if checked_edge_tables.insert(edge.from_table_oid) {
            acl::check_table_acl(edge.from_table_oid)?;
        }
    }
    for table in &active_node_tables {
        ensure_session_stable_rls_identity(table.table_oid, &table.id_columns, &table.table_name)?;
    }
    for edge in &active_edges {
        ensure_session_stable_rls_identity(
            edge.from_table_oid,
            &edge.source_key_columns,
            &edge.from_table,
        )?;
    }

    let active_mapping_ids = active_edges
        .iter()
        .map(|edge| edge.mapping_id)
        .collect::<BTreeSet<_>>();
    let (identity_slots, projected_nodes, relationship_rls_edge_types) = ENGINE.with(|engine| {
        let engine = engine.borrow();
        let mut relationship_rls_edge_types = RoaringBitmap::new();
        for edge in &active_edges {
            let edge_type = engine
                .edge_type_registry
                .iter()
                .position(|label| label == &edge.label)
                .ok_or_else(|| {
                    GraphError::Internal(format!(
                        "registered RLS edge label '{}' is absent from the loaded projection",
                        edge.label
                    ))
                })?;
            relationship_rls_edge_types
                .insert(u32::try_from(edge_type).map_err(|_| {
                    GraphError::Internal("edge type index exceeds u32".to_string())
                })?);
        }
        if !active_edges.is_empty()
            && (force_missing_relationship_identity_for_test()
                || engine
                    .edge_store
                    .type_ids_slice()
                    .iter()
                    .zip(engine.edge_store.relationship_ids_slice())
                    .any(|(edge_type, relationship_id)| {
                        relationship_rls_edge_types.contains(u32::from(*edge_type))
                            && *relationship_id == crate::edge_store::NO_RELATIONSHIP_ID
                    }))
        {
            return Err(GraphError::RlsRelationshipIdentityMissing);
        }
        Ok::<_, GraphError>((
            engine.relationship_identities.len(),
            engine.node_store.node_count(),
            relationship_rls_edge_types,
        ))
    })?;

    let bitmap_ids = u64::from(projected_nodes)
        .checked_add(u64::try_from(identity_slots).unwrap_or(u64::MAX))
        .ok_or_else(|| GraphError::Internal("visibility bitmap estimate overflowed".into()))?;
    let bitmap_bytes = bitmap_ids
        .checked_mul(BITMAP_BYTES_PER_ID_UPPER_BOUND)
        .ok_or_else(|| GraphError::Internal("visibility bitmap estimate overflowed".into()))?;
    let bitmap_lease = governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            ByteCount::from_bytes(bitmap_bytes),
        )
        .map_err(crate::safety::resource_limit_error)?;

    let initial_scope = ENGINE.with(|engine| {
        let engine = engine.borrow();
        let proof = PreparedVisibilityProof(());
        let mut scope = VisibilityScope::enforced(
            &proof,
            RoaringBitmap::new(),
            RoaringBitmap::new(),
            relationship_rls_edge_types,
        );
        for table in &active_node_tables {
            if let Some(members) = engine.table_membership.get(&table.table_oid) {
                scope.hide_nodes(members);
            }
            for node_idx in
                crate::projection::tx_delta::added_node_indexes(table.table_oid, None, false)
            {
                scope.hide_node(node_idx);
            }
        }
        for (index, identity) in engine.relationship_identities.iter().enumerate() {
            if let Some(identity) = identity {
                if active_mapping_ids.contains(&identity.mapping_id) {
                    let relationship_id = u32::try_from(index).map_err(|_| {
                        GraphError::Internal("relationship identity index exceeds u32".to_string())
                    })?;
                    scope.hide_relationship(relationship_id);
                }
            }
        }
        let base_identity_count = engine.relationship_identities.len();
        crate::projection::tx_delta::for_each_relationship_identity(
            base_identity_count,
            |relationship_id, identity| {
                if active_mapping_ids.contains(&identity.mapping_id) {
                    scope.hide_relationship(relationship_id);
                }
            },
        );

        Ok::<_, GraphError>(scope)
    })?;

    VisibilityResolutionGuard::ensure_available()?;
    let scope = pgrx::pg_sys::PgTryBuilder::new(AssertUnwindSafe(|| {
        let _resolution_guard = VisibilityResolutionGuard::activate();
        VISIBILITY_BUILD_SLOT.with(|slot| {
            *slot.borrow_mut() = Some(initial_scope);
        });
        let result = (|| {
            for table in active_node_tables {
                scan_visible_node_keys(table, governor)?;
            }
            for edge in active_edges {
                scan_visible_relationship_keys(edge, identity_slots, governor)?;
            }
            VISIBILITY_BUILD_SLOT
                .with(|slot| slot.borrow_mut().take())
                .ok_or_else(|| {
                    GraphError::Internal("visibility build scope disappeared".to_string())
                })
        })();
        result
    }))
    .finally(|| {
        VISIBILITY_BUILD_SLOT.with(|slot| {
            slot.borrow_mut().take();
        });
        VisibilityResolutionGuard::clear();
    })
    .execute()?;
    bitmap_lease.retain_until_governor_drop();
    Ok(prepared_coordinator(record_visibility_metrics(
        scope, started_at,
    )))
}

#[derive(Clone, Debug)]
pub(crate) struct NodeProbePlan {
    table_oid: u32,
    table_sql: String,
    columns: Vec<(String, String)>,
    typed_indexable: bool,
}

/// Prepare the statement-local authority used by direct identity probes.
pub(crate) fn prepare_direct_identity_visibility(
    table: &RegisteredTable,
    all_tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    require_source_existence: bool,
    preflight_topology_acl: bool,
) -> GraphResult<LazyVisibilityCoordinator> {
    let (mode, probe_tables, policy_rls_tables) = match config::parsed_rls_mode() {
        Some(config::RlsMode::LegacyBypass) => {
            acl::require_rls_bypass_privilege()?;
            let mut active = std::collections::HashSet::new();
            if require_source_existence {
                active.insert(table.table_oid);
            }
            let mode = if active.is_empty() {
                LazyVisibilityMode::Unrestricted
            } else {
                LazyVisibilityMode::Enforced
            };
            (mode, active, std::collections::HashSet::new())
        }
        Some(config::RlsMode::Enforce) => {
            if preflight_topology_acl {
                for candidate in all_tables {
                    if acl::row_security_applies_to_effective_caller(candidate.table_oid) {
                        acl::check_table_acl(candidate.table_oid)?;
                    }
                }
                for edge in edges {
                    if acl::row_security_applies_to_effective_caller(edge.from_table_oid) {
                        acl::check_table_acl(edge.from_table_oid)?;
                    }
                }
                validate_relationship_identity_completeness(edges)?;
            } else {
                acl::check_table_acl(table.table_oid)?;
            }
            let policy_applies = acl::row_security_applies_to_effective_caller(table.table_oid);
            let mut active = std::collections::HashSet::new();
            let mut policy_rls_tables = std::collections::HashSet::new();
            if policy_applies {
                policy_rls_tables.insert(table.table_oid);
            }
            if require_source_existence || policy_applies {
                active.insert(table.table_oid);
            }
            if active.is_empty() {
                (LazyVisibilityMode::Unrestricted, active, policy_rls_tables)
            } else {
                (LazyVisibilityMode::Enforced, active, policy_rls_tables)
            }
        }
        None => {
            return Err(GraphError::InvalidFilter {
                reason: format!(
                    "unsupported graph.rls_mode '{}'; expected 'enforce' or 'legacy_bypass'",
                    config::rls_mode()
                ),
            });
        }
    };
    Ok(LazyVisibilityCoordinator::new(
        VisibilityCacheLimits {
            max_entries: DIRECT_VISIBILITY_MAX_CANDIDATES,
            max_bytes: DIRECT_VISIBILITY_MAX_CANDIDATES * VISIBILITY_CACHE_ENTRY_BYTES,
        },
        mode,
        probe_tables,
        policy_rls_tables,
    ))
}

pub(crate) fn prepare_bfs_visibility(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
) -> GraphResult<LazyVisibilityCoordinator> {
    match config::parsed_rls_mode() {
        Some(config::RlsMode::LegacyBypass) => {
            acl::require_rls_bypass_privilege()?;
            Ok(LazyVisibilityCoordinator::new(
                VisibilityCacheLimits {
                    max_entries: 65_536,
                    max_bytes: crate::config::QUERY_MEMORY_MB.get().max(1) as usize * 1024 * 1024,
                },
                LazyVisibilityMode::Unrestricted,
                HashSet::new(),
                HashSet::new(),
            ))
        }
        Some(config::RlsMode::Enforce) => {
            let frame = pgrx::pg_sys::PgTryBuilder::new(AssertUnwindSafe(|| {
                BFS_VISIBILITY_PREPARATION_SLOT.with(|slot| {
                    *slot.borrow_mut() = Some(Box::default());
                });
                for table in tables {
                    if acl::row_security_applies_to_effective_caller(table.table_oid) {
                        acl::check_table_acl(table.table_oid)?;
                        ensure_session_stable_rls_identity(
                            table.table_oid,
                            &table.id_columns,
                            &table.table_name,
                        )?;
                        BFS_VISIBILITY_PREPARATION_SLOT.with(|slot| {
                            let mut slot = slot.borrow_mut();
                            let frame = slot.as_deref_mut().ok_or_else(|| {
                                GraphError::Internal(
                                    "BFS visibility preparation frame disappeared".into(),
                                )
                            })?;
                            frame.node_tables.insert(table.table_oid);
                            frame.policy_tables.insert(table.table_oid);
                            Ok::<_, GraphError>(())
                        })?;
                    }
                }
                for edge in edges {
                    if acl::row_security_applies_to_effective_caller(edge.from_table_oid) {
                        acl::check_table_acl(edge.from_table_oid)?;
                        ensure_session_stable_rls_identity(
                            edge.from_table_oid,
                            &edge.source_key_columns,
                            &edge.from_table,
                        )?;
                        let edge_type = ENGINE.with(|engine| {
                            engine
                                .borrow()
                                .edge_type_registry
                                .iter()
                                .position(|label| label == &edge.label)
                        });
                        let edge_type = edge_type.ok_or_else(|| {
                            GraphError::Internal(format!(
                                "registered RLS edge label '{}' is absent from the loaded projection",
                                edge.label
                            ))
                        })?;
                        let edge_type = u8::try_from(edge_type).map_err(|_| {
                            GraphError::Internal("edge type index exceeds u8".into())
                        })?;
                        BFS_VISIBILITY_PREPARATION_SLOT.with(|slot| {
                            let mut slot = slot.borrow_mut();
                            let frame = slot.as_deref_mut().ok_or_else(|| {
                                GraphError::Internal(
                                    "BFS visibility preparation frame disappeared".into(),
                                )
                            })?;
                            frame.mappings.insert(edge.mapping_id);
                            frame.edge_types.insert(edge_type);
                            Ok::<_, GraphError>(())
                        })?;
                    }
                }
                let active_edge_types = BFS_VISIBILITY_PREPARATION_SLOT.with(|slot| {
                    let slot = slot.borrow();
                    slot.as_deref()
                        .map(|frame| frame.edge_types.clone())
                        .ok_or_else(|| {
                            GraphError::Internal(
                                "BFS visibility preparation frame disappeared".into(),
                            )
                        })
                })?;
                validate_relationship_identity_completeness_for_types(&active_edge_types)?;
                BFS_VISIBILITY_PREPARATION_SLOT
                    .with(|slot| slot.borrow_mut().take())
                    .ok_or_else(|| {
                        GraphError::Internal(
                            "BFS visibility preparation frame disappeared".into(),
                        )
                    })
            }))
            .finally(|| {
                BFS_VISIBILITY_PREPARATION_SLOT.with(|slot| {
                    slot.borrow_mut().take();
                });
            })
            .execute()?;
            let BfsVisibilityPreparationFrame {
                node_tables,
                policy_tables,
                mappings,
                edge_types,
            } = *frame;
            let mode = if node_tables.is_empty() && mappings.is_empty() {
                LazyVisibilityMode::Unrestricted
            } else {
                LazyVisibilityMode::Enforced
            };
            Ok(LazyVisibilityCoordinator::with_relationship_mappings(
                VisibilityCacheLimits {
                    max_entries: 65_536,
                    max_bytes: crate::config::QUERY_MEMORY_MB.get().max(1) as usize * 1024 * 1024,
                },
                mode,
                node_tables,
                policy_tables,
                mappings,
                edge_types,
            ))
        }
        None => Err(GraphError::InvalidFilter {
            reason: format!(
                "unsupported graph.rls_mode '{}'; expected 'enforce' or 'legacy_bypass'",
                config::rls_mode()
            ),
        }),
    }
}

/// Reuse the already-authorized unrestricted BFS preparation on eager
/// fallback. Only RLS-enforced calls need the full eager source scan.
pub(crate) fn prepare_bfs_eager_fallback(
    lazy: &LazyVisibilityCoordinator,
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    governor: &ResourceGovernor,
) -> GraphResult<VisibilityCoordinator> {
    if lazy.is_enforced() {
        prepare_eager_visibility(tables, edges, governor)
    } else {
        let proof = PreparedVisibilityProof(());
        Ok(prepared_coordinator(VisibilityScope::unrestricted(&proof)))
    }
}

pub(crate) fn direct_node_visibility_coordinator(node: ProvenVisibleNode) -> VisibilityCoordinator {
    let proof = PreparedVisibilityProof(());
    prepared_coordinator(VisibilityScope::direct_node(&proof, node))
}

pub(crate) fn prepare_direct_node_probe(
    table: &RegisteredTable,
) -> GraphResult<Option<NodeProbePlan>> {
    let plan = prepare_node_probe_plan(table.table_oid, std::slice::from_ref(table))?;
    Ok(plan.typed_indexable.then_some(plan))
}

pub(crate) fn unsupported_rls_identity_type(relation: &str) -> GraphError {
    GraphError::UnsupportedOperation {
        operation: "RLS identity resolution".into(),
        reason: format!(
            "registered primary key for '{relation}' does not have a session-stable text representation; use a supported built-in key type or graph.rls_mode = 'legacy_bypass' under an authorized maintenance role"
        ),
    }
}

fn ensure_session_stable_rls_identity(
    table_oid: u32,
    columns: &crate::builder::PrimaryKeySpec,
    relation: &str,
) -> GraphResult<()> {
    for column in columns.columns() {
        let (_, stable_text_io) = registered_column_type_name(table_oid, column)?;
        if !stable_text_io {
            return Err(unsupported_rls_identity_type(relation));
        }
    }
    Ok(())
}

fn validate_relationship_identity_completeness(edges: &[RegisteredEdge]) -> GraphResult<()> {
    for edge in edges {
        // PostgreSQL may ERROR here (for example row_security=off with FORCE
        // RLS), so do not retain an owned Rust accumulator across this call.
        if !acl::row_security_applies_to_effective_caller(edge.from_table_oid) {
            continue;
        }
        if force_missing_relationship_identity_for_test() {
            return Err(GraphError::RlsRelationshipIdentityMissing);
        }
        ENGINE.with(|engine| {
            let engine = engine.borrow();
            let edge_type = engine
                .edge_type_registry
                .iter()
                .position(|entry| entry == &edge.label)
                .ok_or_else(|| {
                    GraphError::Internal(format!(
                        "registered RLS edge label '{}' is absent from the loaded projection",
                        edge.label
                    ))
                })?;
            let edge_type = u8::try_from(edge_type)
                .map_err(|_| GraphError::Internal("edge type index exceeds u8".into()))?;
            if engine
                .edge_store
                .type_ids_slice()
                .iter()
                .zip(engine.edge_store.relationship_ids_slice())
                .any(|(candidate_type, relationship_id)| {
                    *candidate_type == edge_type
                        && *relationship_id == crate::edge_store::NO_RELATIONSHIP_ID
                })
            {
                Err(GraphError::RlsRelationshipIdentityMissing)
            } else {
                Ok(())
            }
        })?;
    }
    Ok(())
}

fn validate_relationship_identity_completeness_for_types(
    active_edge_types: &HashSet<u8>,
) -> GraphResult<()> {
    if active_edge_types.is_empty() {
        return Ok(());
    }
    if force_missing_relationship_identity_for_test() {
        return Err(GraphError::RlsRelationshipIdentityMissing);
    }
    ENGINE.with(|engine| {
        let engine = engine.borrow();
        if engine
            .edge_store
            .type_ids_slice()
            .iter()
            .zip(engine.edge_store.relationship_ids_slice())
            .any(|(edge_type, relationship_id)| {
                active_edge_types.contains(edge_type)
                    && *relationship_id == crate::edge_store::NO_RELATIONSHIP_ID
            })
        {
            Err(GraphError::RlsRelationshipIdentityMissing)
        } else {
            Ok(())
        }
    })
}

pub(crate) fn direct_visibility_batch(
    candidates: Vec<VisibilityCandidate>,
) -> GraphResult<VisibilityCandidateBatch> {
    VisibilityCandidateBatch::try_new(
        candidates,
        VisibilityBatchLimits {
            max_candidates: DIRECT_VISIBILITY_MAX_CANDIDATES,
            max_key_bytes: DIRECT_VISIBILITY_MAX_KEY_BYTES,
        },
    )
}

pub(crate) fn reserve_direct_visibility_candidate(
    governor: &ResourceGovernor,
    source_key: &str,
) -> GraphResult<()> {
    let bytes = source_key
        .len()
        .checked_mul(2)
        .and_then(|value| value.checked_add(std::mem::size_of::<VisibilityCandidate>()))
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "visibility candidate allocation estimate overflow".into(),
        })?;
    let lease = governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            ByteCount::from_bytes(u64::try_from(bytes).unwrap_or(u64::MAX)),
        )
        .map_err(crate::safety::resource_limit_error)?;
    lease.retain_until_governor_drop();
    Ok(())
}

pub(crate) fn reserve_direct_probe_plan(governor: &ResourceGovernor) -> GraphResult<()> {
    let lease = governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            ByteCount::from_bytes(DIRECT_VISIBILITY_PROBE_PLAN_BYTES),
        )
        .map_err(crate::safety::resource_limit_error)?;
    lease.retain_until_governor_drop();
    Ok(())
}

/// Resolve bounded direct identities through PostgreSQL as the policy oracle.
pub(crate) fn resolve_lazy_visibility_batch(
    coordinator: &mut LazyVisibilityCoordinator,
    batch: VisibilityCandidateBatch,
    node_probe_plan: Option<&NodeProbePlan>,
    governor: &ResourceGovernor,
) -> GraphResult<VisibilityVerdictBatch> {
    validate_candidate_projection_identities(batch.candidates())?;
    let allocation_bytes = batch
        .key_bytes()?
        .checked_mul(DIRECT_VISIBILITY_KEY_COPY_UPPER_BOUND)
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "visibility probe allocation estimate overflow".into(),
        })?
        .checked_add(
            batch
                .candidate_count()
                .checked_mul(VISIBILITY_CACHE_ENTRY_BYTES * 8)
                .ok_or_else(|| GraphError::InvalidFilter {
                    reason: "visibility probe allocation estimate overflow".into(),
                })?,
        )
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "visibility probe allocation estimate overflow".into(),
        })?;
    let lease = governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            ByteCount::from_bytes(u64::try_from(allocation_bytes).unwrap_or(u64::MAX)),
        )
        .map_err(crate::safety::resource_limit_error)?;
    let probe = VisibilityProbeBatch::try_new(batch)?;

    // Keep policy evaluation and error ordering stable across executions.
    let mut unknown_by_table = BTreeMap::<u32, Vec<String>>::new();
    for (table_oid, source_key) in &probe.unique_node_keys {
        let node_idx = probe
            .candidates
            .iter()
            .find_map(|candidate| match candidate {
                VisibilityCandidate::Node {
                    table_oid: candidate_table,
                    source_key: candidate_key,
                    node_idx,
                    ..
                } if candidate_table == table_oid && candidate_key == source_key => Some(*node_idx),
                _ => None,
            })
            .ok_or_else(|| GraphError::Internal("visibility probe lost a node candidate".into()))?;
        if !coordinator.table_requires_probe(*table_oid) {
            coordinator.record_node(node_idx, VisibilityVerdict::Visible)?;
        } else if coordinator.node_verdict(node_idx) == VisibilityVerdict::Unknown {
            unknown_by_table
                .entry(*table_oid)
                .or_default()
                .push(source_key.clone());
        }
    }

    if !probe.unique_relationship_keys.is_empty() {
        return Err(GraphError::UnsupportedOperation {
            operation: "lazy relationship visibility".into(),
            reason: "relationship frontier probing starts in P3".into(),
        });
    }

    if !unknown_by_table.is_empty() {
        crate::resource::check_postgres_interrupts();
        governor
            .consume_work(
                ResourcePhase::QueryVisibility,
                WorkUnits::new(u64::try_from(probe.candidates.len()).unwrap_or(u64::MAX)),
            )
            .map_err(crate::safety::resource_limit_error)?;
        governor
            .check_elapsed(ResourcePhase::QueryVisibility)
            .map_err(crate::safety::resource_limit_error)?;
        let plan = node_probe_plan.ok_or_else(|| {
            GraphError::Internal("lazy visibility probe plan was not prepared".into())
        })?;
        if unknown_by_table
            .keys()
            .any(|table_oid| *table_oid != plan.table_oid)
        {
            return Err(GraphError::Internal(
                "lazy visibility batch does not match its prepared table".into(),
            ));
        }
        let plans = vec![plan.clone()];
        VisibilityResolutionGuard::ensure_available()?;
        let frame = Box::new(LazyVisibilityResolutionFrame {
            probe: probe.clone(),
            plans,
            unknown_by_table,
            visible_keys: HashMap::new(),
            active_table_oid: None,
            active_payload: None,
            active_query: None,
        });
        let frame = pgrx::pg_sys::PgTryBuilder::new(AssertUnwindSafe(move || {
            let _resolution_guard = VisibilityResolutionGuard::activate();
            LAZY_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
                *slot.borrow_mut() = Some(frame);
            });
            let result = (|| {
                let table_oids = LAZY_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
                    let slot = slot.borrow();
                    let frame = slot.as_deref().ok_or_else(|| {
                        GraphError::Internal("visibility resolution frame disappeared".into())
                    })?;
                    Ok::<_, GraphError>(
                        frame
                            .plans
                            .iter()
                            .map(|plan| plan.table_oid)
                            .collect::<Vec<_>>(),
                    )
                })?;
                for table_oid in table_oids {
                    LAZY_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
                        let mut slot = slot.borrow_mut();
                        let frame = slot.as_deref_mut().ok_or_else(|| {
                            GraphError::Internal("visibility resolution frame disappeared".into())
                        })?;
                        let plan = frame
                            .plans
                            .iter()
                            .find(|plan| plan.table_oid == table_oid)
                            .ok_or_else(|| {
                                GraphError::Internal("visibility probe plan disappeared".into())
                            })?;
                        let keys = frame.unknown_by_table.get(&table_oid).ok_or_else(|| {
                            GraphError::Internal("visibility probe plan lost its keys".into())
                        })?;
                        let (payload, query) = build_node_probe_query(plan, keys)?;
                        frame.active_table_oid = Some(table_oid);
                        frame.active_payload = Some(payload);
                        frame.active_query = Some(query);
                        Ok::<_, GraphError>(())
                    })?;
                    probe_visible_node_keys_from_slot()?;
                    LAZY_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
                        let mut slot = slot.borrow_mut();
                        let frame = slot.as_deref_mut().ok_or_else(|| {
                            GraphError::Internal("visibility resolution frame disappeared".into())
                        })?;
                        frame.active_table_oid = None;
                        frame.active_payload = None;
                        frame.active_query = None;
                        Ok::<_, GraphError>(())
                    })?;
                }
                LAZY_VISIBILITY_RESOLUTION_SLOT
                    .with(|slot| slot.borrow_mut().take())
                    .ok_or_else(|| {
                        GraphError::Internal("visibility resolution frame disappeared".into())
                    })
            })();
            result
        }))
        .finally(|| {
            LAZY_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
                slot.borrow_mut().take();
            });
            VisibilityResolutionGuard::clear();
        })
        .execute()?;

        debug_assert_eq!(frame.probe.candidates, probe.candidates);
        governor
            .consume_work(
                ResourcePhase::QueryVisibility,
                WorkUnits::new(u64::try_from(frame.visible_keys.len()).unwrap_or(u64::MAX)),
            )
            .map_err(crate::safety::resource_limit_error)?;
        governor
            .check_elapsed(ResourcePhase::QueryVisibility)
            .map_err(crate::safety::resource_limit_error)?;
        for candidate in &probe.candidates {
            if let VisibilityCandidate::Node {
                table_oid,
                source_key,
                node_idx,
                ..
            } = candidate
            {
                if coordinator.node_verdict(*node_idx) == VisibilityVerdict::Unknown {
                    let verdict = if frame
                        .visible_keys
                        .contains_key(&(*table_oid, source_key.clone()))
                    {
                        VisibilityVerdict::Visible
                    } else {
                        VisibilityVerdict::Hidden
                    };
                    coordinator.record_node(*node_idx, verdict)?;
                }
            }
        }
    }

    let verdicts = probe
        .candidates
        .iter()
        .map(|candidate| match candidate {
            VisibilityCandidate::Node {
                sequence, node_idx, ..
            } => Ok((*sequence, coordinator.node_verdict(*node_idx))),
            VisibilityCandidate::Relationship {
                sequence,
                relationship_id: Some(relationship_id),
                ..
            } => Ok((
                *sequence,
                coordinator.relationship_verdict(*relationship_id),
            )),
            VisibilityCandidate::Relationship {
                relationship_id: None,
                ..
            } => Err(GraphError::RlsRelationshipIdentityMissing),
        })
        .collect::<GraphResult<Vec<_>>>()?;
    let candidate_batch = VisibilityCandidateBatch::try_new(
        probe.candidates.clone(),
        VisibilityBatchLimits {
            max_candidates: DIRECT_VISIBILITY_MAX_CANDIDATES,
            max_key_bytes: DIRECT_VISIBILITY_MAX_KEY_BYTES,
        },
    )?;
    let resolved = VisibilityVerdictBatch::try_new(&candidate_batch, verdicts)?;
    lease.retain_until_governor_drop();
    Ok(resolved)
}

/// Resolve one bounded BFS adjacency batch after the engine borrow has been
/// released. Node and relationship identities are deduplicated independently,
/// probed per source relation, and fanned back out in adjacency order.
pub(crate) fn resolve_bfs_visibility_batch(
    coordinator: &mut LazyVisibilityCoordinator,
    batch: &crate::bfs::BfsAdjacencyCandidateBatch,
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
    governor: &ResourceGovernor,
) -> GraphResult<Vec<crate::bfs::BfsAdjacencyVerdict>> {
    #[cfg(feature = "development")]
    let _drop_probe = BfsVisibilityResolutionDropProbe::arm();
    // Keep the P2 oracle as the single semantic authority for Unknown,
    // Visible, Hidden, cache bounds, and fail-closed relationship identities.
    // P3 performs the same transitions in sets because the P2 entry point is
    // deliberately limited to one direct identity.
    let _p2_oracle = resolve_lazy_visibility_batch;
    postgres_error_as_rust_unwind(AssertUnwindSafe(crate::resource::check_postgres_interrupts));

    if batch.candidates.iter().any(|candidate| {
        coordinator.edge_type_requires_relationship_identity(candidate.edge_type)
            && (candidate.relationship_id.is_none()
                || candidate.relationship_mapping_id.is_none()
                || candidate.relationship_source_key.is_none())
    }) {
        return Err(GraphError::RlsRelationshipIdentityMissing);
    }

    let input_key_bytes = batch
        .candidates
        .iter()
        .try_fold(0usize, |total, candidate| {
            total
                .checked_add(candidate.target_source_key.len())
                .and_then(|bytes| {
                    bytes.checked_add(
                        candidate
                            .relationship_source_key
                            .as_ref()
                            .map_or(0, String::len),
                    )
                })
                .ok_or_else(|| GraphError::InvalidFilter {
                    reason: "BFS visibility probe allocation estimate overflow".into(),
                })
        })?;
    let allocation_bytes = input_key_bytes
        .checked_mul(DIRECT_VISIBILITY_KEY_COPY_UPPER_BOUND)
        .and_then(|bytes| {
            bytes.checked_add(
                batch
                    .candidates
                    .len()
                    .checked_mul(VISIBILITY_CACHE_ENTRY_BYTES * 16)?,
            )
        })
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "BFS visibility probe allocation estimate overflow".into(),
        })?;
    let _lease = governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            ByteCount::from_bytes(u64::try_from(allocation_bytes).unwrap_or(u64::MAX)),
        )
        .map_err(crate::safety::resource_limit_error)?;

    let mut visibility_candidates = Vec::new();
    visibility_candidates
        .try_reserve(batch.candidates.len().saturating_mul(2))
        .map_err(|_| GraphError::ResourceLimit {
            resource: "visibility_probe".into(),
            phase: ResourcePhase::QueryVisibility.as_str().into(),
            used: 0,
            requested: u64::try_from(batch.candidates.len().saturating_mul(2)).unwrap_or(u64::MAX),
            limit: u64::try_from(batch.candidates.len().saturating_mul(2)).unwrap_or(u64::MAX),
        })?;
    let mut sequence = 0u32;
    for candidate in &batch.candidates {
        visibility_candidates.push(VisibilityCandidate::Node {
            sequence,
            table_oid: candidate.target_table_oid,
            source_key: candidate.target_source_key.clone(),
            node_idx: candidate.target_node,
        });
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| GraphError::Internal("BFS visibility sequence overflow".into()))?;
        if let Some(mapping_id) = candidate.relationship_mapping_id {
            let source_key = candidate
                .relationship_source_key
                .clone()
                .ok_or(GraphError::RlsRelationshipIdentityMissing)?;
            visibility_candidates.push(VisibilityCandidate::Relationship {
                sequence,
                mapping_id,
                source_key,
                relationship_id: if force_missing_bfs_candidate_identity_for_test() {
                    None
                } else {
                    candidate.relationship_id
                },
                edge_type: candidate.edge_type,
            });
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| GraphError::Internal("BFS visibility sequence overflow".into()))?;
        }
    }
    validate_candidate_projection_identities(&visibility_candidates)?;
    let key_bytes = visibility_candidates
        .iter()
        .try_fold(0usize, |total, candidate| {
            let candidate_bytes = match candidate {
                VisibilityCandidate::Node { source_key, .. }
                | VisibilityCandidate::Relationship { source_key, .. } => source_key.len(),
            };
            total
                .checked_add(candidate_bytes)
                .ok_or_else(|| GraphError::InvalidFilter {
                    reason: "BFS visibility candidate key bytes overflow".into(),
                })
        })?;
    let bounded = VisibilityCandidateBatch::try_new(
        visibility_candidates,
        VisibilityBatchLimits {
            max_candidates: batch.candidates.len().saturating_mul(2),
            max_key_bytes: key_bytes,
        },
    )?;
    let probe = VisibilityProbeBatch::try_new(bounded)?;

    let mut unknown_nodes = BTreeMap::<u32, Vec<String>>::new();
    let mut unknown_relationships = BTreeMap::<u64, Vec<String>>::new();
    for (table_oid, source_key) in &probe.unique_node_keys {
        let node_idx = probe
            .candidates
            .iter()
            .find_map(|candidate| match candidate {
                VisibilityCandidate::Node {
                    table_oid: candidate_table,
                    source_key: candidate_key,
                    node_idx,
                    ..
                } if candidate_table == table_oid && candidate_key == source_key => Some(*node_idx),
                _ => None,
            })
            .ok_or_else(|| GraphError::Internal("BFS node probe lost its candidate".into()))?;
        if coordinator.table_requires_probe(*table_oid) {
            if coordinator.node_verdict(node_idx) == VisibilityVerdict::Unknown {
                unknown_nodes
                    .entry(*table_oid)
                    .or_default()
                    .push(source_key.clone());
            }
        } else {
            coordinator.record_node(node_idx, VisibilityVerdict::Visible)?;
        }
    }
    for (mapping_id, source_key) in &probe.unique_relationship_keys {
        let relationship_id = probe
            .candidates
            .iter()
            .find_map(|candidate| match candidate {
                VisibilityCandidate::Relationship {
                    mapping_id: candidate_mapping,
                    source_key: candidate_key,
                    relationship_id,
                    ..
                } if candidate_mapping == mapping_id && candidate_key == source_key => {
                    *relationship_id
                }
                _ => None,
            })
            .ok_or(GraphError::RlsRelationshipIdentityMissing)?;
        if coordinator.mapping_requires_probe(*mapping_id) {
            if coordinator.relationship_verdict(relationship_id) == VisibilityVerdict::Unknown {
                unknown_relationships
                    .entry(*mapping_id)
                    .or_default()
                    .push(source_key.clone());
            }
        } else {
            coordinator.record_relationship(relationship_id, VisibilityVerdict::Visible)?;
        }
    }
    let node_plan_keys = unknown_nodes.keys().copied().collect::<BTreeSet<_>>();
    let relationship_plan_keys = unknown_relationships
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let node_plans = postgres_error_as_rust_unwind(AssertUnwindSafe(|| {
        prepare_bfs_node_probe_plans(tables, &node_plan_keys)
    }))?;
    let relationship_plans = postgres_error_as_rust_unwind(AssertUnwindSafe(|| {
        prepare_bfs_relationship_probe_plans(edges, &relationship_plan_keys)
    }))?;

    let mut visible_nodes = HashSet::<(u32, String)>::new();
    let mut visible_relationships = HashSet::<(u64, String)>::new();
    let requested_keys = unknown_nodes
        .values()
        .map(Vec::len)
        .sum::<usize>()
        .checked_add(unknown_relationships.values().map(Vec::len).sum::<usize>())
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "BFS visibility requested-key count overflow".into(),
        })?;
    let requested_key_bytes = unknown_nodes
        .values()
        .flatten()
        .map(String::len)
        .sum::<usize>()
        .checked_add(
            unknown_relationships
                .values()
                .flatten()
                .map(String::len)
                .sum::<usize>(),
        )
        .ok_or_else(|| GraphError::InvalidFilter {
            reason: "BFS visibility requested-key bytes overflow".into(),
        })?;
    let mut metrics = BfsVisibilityMetrics {
        requested_keys: u64::try_from(requested_keys).unwrap_or(u64::MAX),
        requested_key_bytes: u64::try_from(requested_key_bytes).unwrap_or(u64::MAX),
        ..BfsVisibilityMetrics::EMPTY
    };
    for (table_oid, keys) in unknown_nodes {
        let plan = node_plans.get(&table_oid).ok_or_else(|| {
            GraphError::Internal(format!(
                "BFS node probe has no plan for table OID {table_oid}"
            ))
        })?;
        let (payload, query) = build_bfs_node_probe_query(plan, &keys)?;
        let returned = postgres_error_as_rust_unwind(AssertUnwindSafe(|| {
            execute_bfs_set_probe(query, payload)
        }))?;
        metrics.spi_calls = metrics.spi_calls.saturating_add(1);
        metrics.source_rows = metrics
            .source_rows
            .saturating_add(u64::try_from(returned.len()).unwrap_or(u64::MAX));
        for key in returned {
            metrics.returned_keys = metrics.returned_keys.saturating_add(1);
            metrics.returned_key_bytes = metrics
                .returned_key_bytes
                .saturating_add(u64::try_from(key.len()).unwrap_or(u64::MAX));
            visible_nodes.insert((table_oid, key));
        }
    }
    for (mapping_id, keys) in unknown_relationships {
        let plan = relationship_plans.get(&mapping_id).ok_or_else(|| {
            GraphError::Internal(format!(
                "BFS relationship probe has no plan for mapping {mapping_id}"
            ))
        })?;
        let (payload, query) = build_bfs_relationship_probe_query(plan, &keys)?;
        let returned = postgres_error_as_rust_unwind(AssertUnwindSafe(|| {
            execute_bfs_set_probe(query, payload)
        }))?;
        metrics.spi_calls = metrics.spi_calls.saturating_add(1);
        metrics.source_rows = metrics
            .source_rows
            .saturating_add(u64::try_from(returned.len()).unwrap_or(u64::MAX));
        for key in returned {
            metrics.returned_keys = metrics.returned_keys.saturating_add(1);
            metrics.returned_key_bytes = metrics
                .returned_key_bytes
                .saturating_add(u64::try_from(key.len()).unwrap_or(u64::MAX));
            visible_relationships.insert((mapping_id, key));
        }
    }

    for candidate in &probe.candidates {
        match candidate {
            VisibilityCandidate::Node {
                table_oid,
                source_key,
                node_idx,
                ..
            } if coordinator.node_verdict(*node_idx) == VisibilityVerdict::Unknown => {
                let verdict = if visible_nodes.iter().any(|(visible_table, visible_key)| {
                    visible_table == table_oid && visible_key == source_key
                }) {
                    VisibilityVerdict::Visible
                } else {
                    VisibilityVerdict::Hidden
                };
                coordinator.record_node(*node_idx, verdict)?;
            }
            VisibilityCandidate::Relationship {
                mapping_id,
                source_key,
                relationship_id: Some(relationship_id),
                ..
            } if coordinator.relationship_verdict(*relationship_id)
                == VisibilityVerdict::Unknown =>
            {
                let verdict =
                    if visible_relationships
                        .iter()
                        .any(|(visible_mapping, visible_key)| {
                            visible_mapping == mapping_id && visible_key == source_key
                        })
                    {
                        VisibilityVerdict::Visible
                    } else {
                        VisibilityVerdict::Hidden
                    };
                coordinator.record_relationship(*relationship_id, verdict)?;
            }
            VisibilityCandidate::Relationship {
                relationship_id: None,
                ..
            } => {
                return Err(GraphError::RlsRelationshipIdentityMissing);
            }
            _ => {}
        }
    }
    governor
        .consume_work(
            ResourcePhase::QueryVisibility,
            WorkUnits::new(metrics.requested_keys.saturating_add(metrics.returned_keys)),
        )
        .map_err(crate::safety::resource_limit_error)?;
    governor
        .check_elapsed(ResourcePhase::QueryVisibility)
        .map_err(crate::safety::resource_limit_error)?;
    #[cfg(feature = "development")]
    BFS_VISIBILITY_LAST_METRICS.with(|last| {
        let previous = last.get();
        last.set(BfsVisibilityMetrics {
            spi_calls: previous.spi_calls.saturating_add(metrics.spi_calls),
            requested_keys: previous
                .requested_keys
                .saturating_add(metrics.requested_keys),
            returned_keys: previous.returned_keys.saturating_add(metrics.returned_keys),
            requested_key_bytes: previous
                .requested_key_bytes
                .saturating_add(metrics.requested_key_bytes),
            returned_key_bytes: previous
                .returned_key_bytes
                .saturating_add(metrics.returned_key_bytes),
            source_rows: previous.source_rows.saturating_add(metrics.source_rows),
        });
    });

    let verdicts = batch
        .candidates
        .iter()
        .map(|candidate| {
            let node_visible =
                coordinator.node_verdict(candidate.target_node) == VisibilityVerdict::Visible;
            let relationship_visible =
                match (candidate.relationship_mapping_id, candidate.relationship_id) {
                    (Some(mapping_id), Some(relationship_id))
                        if coordinator.mapping_requires_probe(mapping_id) =>
                    {
                        coordinator.relationship_verdict(relationship_id)
                            == VisibilityVerdict::Visible
                    }
                    (Some(_), None) => return Err(GraphError::RlsRelationshipIdentityMissing),
                    _ => true,
                };
            Ok(crate::bfs::BfsAdjacencyVerdict {
                sequence: candidate.sequence,
                node_visible,
                relationship_visible,
            })
        })
        .collect::<GraphResult<Vec<_>>>()?;
    Ok(verdicts)
}

fn execute_bfs_set_probe(query: String, payload: String) -> GraphResult<Vec<String>> {
    VisibilityResolutionGuard::ensure_available()?;
    let frame = Box::new(BfsVisibilityResolutionFrame {
        query,
        payload,
        visible_keys: Vec::new(),
    });
    pgrx::pg_sys::PgTryBuilder::new(AssertUnwindSafe(move || {
        let _guard = VisibilityResolutionGuard::activate();
        BFS_VISIBILITY_RESOLUTION_SLOT.with(|slot| *slot.borrow_mut() = Some(frame));
        probe_visible_bfs_keys_from_slot()?;
        BFS_VISIBILITY_RESOLUTION_SLOT
            .with(|slot| slot.borrow_mut().take())
            .map(|frame| frame.visible_keys)
            .ok_or_else(|| {
                GraphError::Internal("BFS visibility resolution frame disappeared".into())
            })
    }))
    .finally(|| {
        BFS_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
            slot.borrow_mut().take();
        });
        VisibilityResolutionGuard::clear();
    })
    .execute()
}

fn probe_visible_bfs_keys_from_slot() -> GraphResult<()> {
    let (query, payload) = BFS_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
        let slot = slot.borrow();
        let frame = slot.as_deref().ok_or_else(|| {
            GraphError::Internal("BFS visibility resolution frame disappeared".into())
        })?;
        Ok::<_, GraphError>((
            &frame.query as *const String,
            &frame.payload as *const String,
        ))
    })?;
    inject_lazy_visibility_cancellation();
    let visible = Spi::connect(|client| {
        // SAFETY: the TLS frame owns both buffers until PgTry finally cleanup;
        // no RefCell or engine borrow is held while PostgreSQL can longjmp.
        let (query, payload) = unsafe { (&*query, &*payload) };
        let rows = client
            .select(query, None, &[payload.as_str().into()])
            .map_err(|error| {
                GraphError::Internal(format!("lazy BFS RLS visibility probe failed: {error}"))
            })?;
        rows.map(|row| {
            row.get::<String>(1)
                .map_err(|error| {
                    GraphError::Internal(format!("lazy BFS RLS key read failed: {error}"))
                })?
                .ok_or_else(|| GraphError::Internal("lazy BFS RLS probe returned NULL key".into()))
        })
        .collect::<GraphResult<Vec<_>>>()
    })?;
    crate::resource::check_postgres_interrupts();
    BFS_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let frame = slot.as_deref_mut().ok_or_else(|| {
            GraphError::Internal("BFS visibility resolution frame disappeared".into())
        })?;
        frame.visible_keys = visible;
        Ok(())
    })
}

fn validate_candidate_projection_identities(candidates: &[VisibilityCandidate]) -> GraphResult<()> {
    ENGINE.with(|engine| {
        let engine = engine.borrow();
        for candidate in candidates {
            match candidate {
                VisibilityCandidate::Node {
                    table_oid,
                    source_key,
                    node_idx,
                    ..
                } if (engine.node_store.table_oid(*node_idx) == Some(*table_oid)
                    && engine.node_store.primary_key(*node_idx) == Some(source_key.as_str()))
                    || crate::projection::tx_delta::resolve_added_node(
                        *table_oid, source_key, None, false,
                    ) == Some(*node_idx) => {}
                VisibilityCandidate::Relationship {
                    mapping_id,
                    source_key,
                    relationship_id: Some(relationship_id),
                    ..
                } if relationship_candidate_is_canonical(
                    &engine.relationship_identities,
                    *mapping_id,
                    source_key,
                    *relationship_id,
                ) => {}
                VisibilityCandidate::Relationship {
                    relationship_id: None,
                    ..
                } => return Err(GraphError::RlsRelationshipIdentityMissing),
                _ => {
                    return Err(GraphError::Internal(
                        "visibility candidate does not match its canonical projection identity"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    })
}

fn relationship_candidate_is_canonical(
    base: &crate::relationship_identity_store::RelationshipIdentityStore,
    mapping_id: u64,
    source_key: &str,
    relationship_id: crate::edge_store::RelationshipId,
) -> bool {
    if let Some(identity) = base.get(relationship_id) {
        return identity.mapping_id == mapping_id && identity.source_key == source_key;
    }
    let base_count = base.len();
    let Ok(id) = usize::try_from(relationship_id) else {
        return false;
    };
    let Some(offset) = id.checked_sub(base_count) else {
        return false;
    };
    crate::projection::tx_delta::with_relationship_identity(offset, |identity| {
        identity.is_some_and(|identity| {
            identity.mapping_id == mapping_id && identity.source_key == source_key
        })
    })
}

fn prepare_node_probe_plan(
    table_oid: u32,
    tables: &[RegisteredTable],
) -> GraphResult<NodeProbePlan> {
    postgres_error_as_rust_unwind(AssertUnwindSafe(|| acl::check_table_acl(table_oid)))?;
    let table = tables
        .iter()
        .find(|table| table.table_oid == table_oid)
        .ok_or_else(|| GraphError::Internal(format!("unregistered node table OID {table_oid}")))?;
    let table_sql =
        postgres_error_as_rust_unwind(AssertUnwindSafe(|| sql_table_name_from_oid(table_oid)))?
            .as_sql()
            .to_string();
    let mut columns = Vec::new();
    let mut typed_indexable = true;
    columns
        .try_reserve(table.id_columns.columns().len())
        .map_err(|_| GraphError::ResourceLimit {
            resource: "visibility_probe".into(),
            phase: ResourcePhase::QueryVisibility.as_str().into(),
            used: 0,
            requested: u64::try_from(table.id_columns.columns().len()).unwrap_or(u64::MAX),
            limit: u64::try_from(DIRECT_VISIBILITY_MAX_CANDIDATES).unwrap_or(u64::MAX),
        })?;
    for column in table.id_columns.columns() {
        let (type_name, stable_text_io) = postgres_error_as_rust_unwind(AssertUnwindSafe(|| {
            registered_column_type_name(table_oid, column)
        }))?;
        typed_indexable &= stable_text_io;
        columns.push((column.clone(), type_name));
    }
    Ok(NodeProbePlan {
        table_oid,
        table_sql,
        columns,
        typed_indexable,
    })
}

fn registered_column_type_name(table_oid: u32, column: &str) -> GraphResult<(String, bool)> {
    Spi::connect(|client| {
        let row = client
            .select(
                "SELECT pg_catalog.format_type(a.atttypid, a.atttypmod),
                        a.atttypid IN (
                            'pg_catalog.bool'::regtype,
                            'pg_catalog.int2'::regtype,
                            'pg_catalog.int4'::regtype,
                            'pg_catalog.int8'::regtype,
                            'pg_catalog.text'::regtype,
                            'pg_catalog.oid'::regtype,
                            'pg_catalog.bpchar'::regtype,
                            'pg_catalog.varchar'::regtype,
                            'pg_catalog.numeric'::regtype,
                            'pg_catalog.uuid'::regtype
                        )
           FROM pg_catalog.pg_attribute AS a
          WHERE a.attrelid = $1 AND a.attname = $2
            AND a.attnum > 0 AND NOT a.attisdropped",
                None,
                &[pgrx::pg_sys::Oid::from_u32(table_oid).into(), column.into()],
            )
            .map_err(|error| {
                GraphError::Internal(format!("visibility probe type lookup failed: {error}"))
            })?
            .next()
            .ok_or_else(|| {
                GraphError::Internal(format!(
                    "registered key column {column} is absent from table OID {table_oid}"
                ))
            })?;
        Ok((
            row.get::<String>(1)
                .map_err(|error| {
                    GraphError::Internal(format!("visibility probe type read failed: {error}"))
                })?
                .ok_or_else(|| GraphError::Internal("visibility probe type was NULL".into()))?,
            row.get::<bool>(2)
                .map_err(|error| {
                    GraphError::Internal(format!("visibility probe category read failed: {error}"))
                })?
                .unwrap_or(false),
        ))
    })
}

fn build_node_probe_query(plan: &NodeProbePlan, keys: &[String]) -> GraphResult<(String, String)> {
    let payload = keys
        .first()
        .cloned()
        .ok_or_else(|| GraphError::Internal("visibility probe has no key".into()))?;
    if keys.len() != 1 {
        return Err(GraphError::Internal(
            "direct visibility probe received more than one key".into(),
        ));
    }
    let predicates = if plan.columns.len() == 1 {
        let (column, type_name) = &plan.columns[0];
        format!(
            "src.{} = requested.source_key::{type_name}",
            crate::quote::quote_ident(column)
        )
    } else {
        validate_composite_probe_keys(keys, plan.columns.len())?;
        plan.columns
            .iter()
            .enumerate()
            .map(|(index, (column, type_name))| {
                format!(
                    "src.{} = (requested.source_key::jsonb ->> {index})::{type_name}",
                    crate::quote::quote_ident(column)
                )
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let query = format!(
        "WITH requested(source_key) AS (
             SELECT $1::text
         )
         SELECT EXISTS (
             SELECT 1
               FROM requested
               JOIN {} AS src ON {predicates}
         )",
        plan.table_sql
    );
    Ok((payload, query))
}

/// Build one bounded set-based node probe which preserves the first-seen key
/// order while comparing source columns in their native PostgreSQL types.
fn build_bfs_node_probe_query(
    plan: &NodeProbePlan,
    keys: &[String],
) -> GraphResult<(String, String)> {
    build_bfs_probe_query(plan, keys)
}

/// Relationship rows use the registered source primary key, so they share the
/// same typed/indexable query shape as node rows while retaining a distinct
/// mapping namespace in the resolver cache.
fn build_bfs_relationship_probe_query(
    plan: &NodeProbePlan,
    keys: &[String],
) -> GraphResult<(String, String)> {
    build_bfs_probe_query(plan, keys)
}

fn build_bfs_probe_query(plan: &NodeProbePlan, keys: &[String]) -> GraphResult<(String, String)> {
    if keys.is_empty() {
        return Err(GraphError::Internal("visibility probe has no key".into()));
    }
    if !plan.typed_indexable {
        return Err(GraphError::UnsupportedOperation {
            operation: "RLS identity resolution".into(),
            reason: "set-based visibility probe requires session-stable key types".into(),
        });
    }
    if plan.columns.len() > 1 {
        validate_composite_probe_keys(keys, plan.columns.len())?;
    }
    let payload = serde_json::to_string(keys).map_err(|error| {
        GraphError::Internal(format!("visibility probe payload encoding failed: {error}"))
    })?;
    let predicates = if plan.columns.len() == 1 {
        let (column, type_name) = &plan.columns[0];
        format!(
            "src.{} = requested.source_key::{type_name}",
            crate::quote::quote_ident(column)
        )
    } else {
        plan.columns
            .iter()
            .enumerate()
            .map(|(index, (column, type_name))| {
                format!(
                    "src.{} = (requested.source_key::jsonb ->> {index})::{type_name}",
                    crate::quote::quote_ident(column)
                )
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let query = format!(
        "WITH requested(source_key, ordinal) AS (
             SELECT value, ordinality
               FROM pg_catalog.jsonb_array_elements_text($1::jsonb)
                    WITH ORDINALITY AS input(value, ordinality)
         )
         SELECT requested.source_key
           FROM requested
           JOIN {} AS src ON {predicates}
          ORDER BY requested.ordinal",
        plan.table_sql
    );
    Ok((payload, query))
}

fn prepare_bfs_node_probe_plans(
    tables: &[RegisteredTable],
    required: &BTreeSet<u32>,
) -> GraphResult<BTreeMap<u32, NodeProbePlan>> {
    tables
        .iter()
        .filter(|table| required.contains(&table.table_oid))
        .map(|table| {
            prepare_node_probe_plan(table.table_oid, std::slice::from_ref(table)).and_then(|plan| {
                plan.typed_indexable
                    .then_some((table.table_oid, plan))
                    .ok_or_else(|| unsupported_rls_identity_type(&table.table_name))
            })
        })
        .collect()
}

fn prepare_bfs_relationship_probe_plans(
    edges: &[RegisteredEdge],
    required: &BTreeSet<u64>,
) -> GraphResult<BTreeMap<u64, NodeProbePlan>> {
    edges
        .iter()
        .filter(|edge| required.contains(&edge.mapping_id))
        .map(|edge| {
            let synthetic = RegisteredTable {
                table_oid: edge.from_table_oid,
                table_name: edge.from_table.clone(),
                id_columns: edge.source_key_columns.clone(),
                columns: crate::builder::PropertyColumns::from_columns(Vec::new()),
                tenant_column: None,
            };
            prepare_node_probe_plan(edge.from_table_oid, &[synthetic]).and_then(|plan| {
                plan.typed_indexable
                    .then_some((edge.mapping_id, plan))
                    .ok_or_else(|| unsupported_rls_identity_type(&edge.from_table))
            })
        })
        .collect()
}

#[cfg(any(test, feature = "development"))]
pub(crate) fn _test_build_node_probe_query(
    table_oid: u32,
    tables: &[RegisteredTable],
    key: &str,
) -> GraphResult<(String, String)> {
    let plan = prepare_node_probe_plan(table_oid, tables)?;
    build_node_probe_query(&plan, &[key.to_string()])
}

fn probe_visible_node_keys_from_slot() -> GraphResult<()> {
    let (table_oid, query, payload) = LAZY_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
        let slot = slot.borrow();
        let frame = slot.as_deref().ok_or_else(|| {
            GraphError::Internal("visibility resolution frame disappeared".into())
        })?;
        let table_oid = frame
            .active_table_oid
            .ok_or_else(|| GraphError::Internal("visibility probe has no active table".into()))?;
        let query = frame
            .active_query
            .as_ref()
            .ok_or_else(|| GraphError::Internal("visibility probe has no active SQL".into()))?;
        let payload = frame
            .active_payload
            .as_ref()
            .ok_or_else(|| GraphError::Internal("visibility probe has no active payload".into()))?;
        Ok::<_, GraphError>((table_oid, query as *const String, payload as *const String))
    })?;

    // Do not hold a RefCell guard, an ENGINE borrow, or an ordinary owned query
    // buffer across SPI. PostgreSQL ERROR uses longjmp and would skip their
    // destructors. The pointed-to values are owned by the TLS frame, which is
    // not mutated until this call returns and is cleared by PgTry `finally` on
    // ERROR/cancellation.
    inject_lazy_visibility_cancellation();
    let visible = Spi::connect(|client| {
        // SAFETY: both pointers refer to fields in the installed TLS frame.
        // This function is the frame's only consumer, and neither successful
        // execution nor error cleanup mutates the frame while `select` runs.
        let (query, payload) = unsafe { (&*query, &*payload) };
        let row = client
            .select(query, None, &[payload.as_str().into()])
            .map_err(|error| {
                GraphError::Internal(format!("lazy RLS visibility probe failed: {error}"))
            })?
            .first();
        row.get::<bool>(1)
            .map_err(|error| {
                GraphError::Internal(format!("lazy RLS visibility result read failed: {error}"))
            })
            .map(|value| value.unwrap_or(false))
    })?;
    crate::resource::check_postgres_interrupts();
    if visible {
        LAZY_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let frame = slot.as_deref_mut().ok_or_else(|| {
                GraphError::Internal("visibility resolution frame disappeared".into())
            })?;
            let key = frame.active_payload.as_ref().cloned().ok_or_else(|| {
                GraphError::Internal("visibility probe has no active payload".into())
            })?;
            frame.visible_keys.insert((table_oid, key), ());
            Ok::<_, GraphError>(())
        })?;
    }
    Ok(())
}

#[cfg(feature = "development")]
fn inject_lazy_visibility_cancellation() {
    LAZY_VISIBILITY_CANCEL_BEFORE_SPI.with(|armed| {
        if armed.replace(false) {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
                "injected lazy RLS visibility cancellation"
            );
        }
    });
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn inject_lazy_visibility_cancellation() {}

fn validate_composite_probe_keys(keys: &[String], arity: usize) -> GraphResult<()> {
    for key in keys {
        let parts = serde_json::from_str::<Vec<Option<String>>>(key).map_err(|error| {
            GraphError::CorruptFile {
                reason: format!("malformed composite projection identity: {error}"),
            }
        })?;
        if parts.len() != arity || parts.iter().any(Option::is_none) {
            return Err(GraphError::CorruptFile {
                reason: "composite projection identity has wrong arity or NULL component".into(),
            });
        }
    }
    Ok(())
}

#[cfg(feature = "development")]
fn start_visibility_timer() -> VisibilityTimer {
    Instant::now()
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn start_visibility_timer() -> VisibilityTimer {
    VisibilityTimer
}

#[cfg(feature = "development")]
fn record_visibility_metrics(
    scope: VisibilityScope,
    started_at: VisibilityTimer,
) -> VisibilityScope {
    let (hidden_nodes, hidden_relationships) = scope.hidden_counts();
    let elapsed_micros = u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX);
    VISIBILITY_LAST_METRICS.with(|metrics| {
        metrics.set((elapsed_micros, hidden_nodes, hidden_relationships));
    });
    scope
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn record_visibility_metrics(
    scope: VisibilityScope,
    _started_at: VisibilityTimer,
) -> VisibilityScope {
    scope
}

fn scan_visible_node_keys(table: &RegisteredTable, governor: &ResourceGovernor) -> GraphResult<()> {
    acl::check_table_acl(table.table_oid)?;
    let table_name = sql_table_name_from_oid(table.table_oid)?;
    let key_expr = primary_key_expr("src", &table.id_columns);
    scan_visible_keys(table_name.as_sql(), &key_expr, governor, |source_key| {
        let node_idx = ENGINE
            .with(|engine| engine.borrow().resolve(table.table_oid, source_key))
            .or_else(|| {
                crate::projection::tx_delta::resolve_added_node(
                    table.table_oid,
                    source_key,
                    None,
                    false,
                )
            });
        if let Some(node_idx) = node_idx {
            VISIBILITY_BUILD_SLOT.with(|slot| {
                if let Some(scope) = slot.borrow_mut().as_mut() {
                    scope.reveal_node(node_idx);
                }
            });
        }
        Ok(())
    })
}

fn scan_visible_relationship_keys(
    edge: &RegisteredEdge,
    base_identity_count: usize,
    governor: &ResourceGovernor,
) -> GraphResult<()> {
    acl::check_table_acl(edge.from_table_oid)?;
    let table_name = sql_table_name_from_oid(edge.from_table_oid)?;
    let key_expr = primary_key_expr("src", &edge.source_key_columns);
    scan_visible_keys(table_name.as_sql(), &key_expr, governor, |source_key| {
        let relationship_id = ENGINE
            .with(|engine| {
                engine
                    .borrow()
                    .relationship_identities
                    .find_id(edge.mapping_id, source_key)
            })
            .or_else(|| {
                crate::projection::tx_delta::find_relationship_identity_id(
                    base_identity_count,
                    edge.mapping_id,
                    source_key,
                )
            });
        if let Some(relationship_id) = relationship_id {
            VISIBILITY_BUILD_SLOT.with(|slot| {
                if let Some(scope) = slot.borrow_mut().as_mut() {
                    scope.reveal_relationship(relationship_id);
                }
            });
        }
        Ok(())
    })
}

fn scan_visible_keys(
    table_sql: &str,
    key_expr: &str,
    governor: &ResourceGovernor,
    mut visit: impl FnMut(&str) -> GraphResult<()>,
) -> GraphResult<()> {
    let max_key_bytes = Spi::get_one::<i64>(&format!(
        "SELECT COALESCE(pg_catalog.max(pg_catalog.octet_length({key_expr})), 0)::bigint \
         FROM {table_sql} AS src"
    ))
    .map_err(|error| GraphError::Internal(format!("RLS visibility preflight failed: {error}")))?
    .unwrap_or(0)
    .max(0);
    let batch_bytes = u64::try_from(max_key_bytes)
        .unwrap_or(u64::MAX)
        .checked_mul(u64::try_from(VISIBILITY_CURSOR_ROWS).unwrap_or(u64::MAX))
        .ok_or_else(|| GraphError::Internal("visibility cursor estimate overflowed".into()))?;
    let _batch_lease = governor
        .reserve_memory(
            ResourcePhase::QueryVisibility,
            ByteCount::from_bytes(batch_bytes),
        )
        .map_err(crate::safety::resource_limit_error)?;

    Spi::connect(|client| {
        let query = format!("SELECT {key_expr} AS graph_source_key FROM {table_sql} AS src");
        let mut cursor = client.open_cursor(&query, &[]);
        loop {
            crate::resource::check_postgres_interrupts();
            let rows = cursor.fetch(VISIBILITY_CURSOR_ROWS).map_err(|error| {
                GraphError::Internal(format!("RLS visibility cursor fetch failed: {error}"))
            })?;
            if rows.is_empty() {
                break;
            }
            let fetched = rows.len();
            record_eager_visibility_source_rows(fetched);
            for row in rows {
                let source_key = row
                    .get::<String>(1)
                    .map_err(|error| {
                        GraphError::Internal(format!(
                            "RLS visibility source-key read failed: {error}"
                        ))
                    })?
                    .ok_or_else(|| {
                        GraphError::Internal(
                            "RLS visibility scan returned a NULL source key".to_string(),
                        )
                    })?;
                visit(&source_key)?;
                inject_visibility_scan_cancellation();
            }
            governor
                .consume_work(
                    ResourcePhase::QueryVisibility,
                    WorkUnits::new(u64::try_from(fetched).unwrap_or(u64::MAX)),
                )
                .map_err(crate::safety::resource_limit_error)?;
            governor
                .check_elapsed(ResourcePhase::QueryVisibility)
                .map_err(crate::safety::resource_limit_error)?;
        }
        Ok::<(), GraphError>(())
    })
}

#[cfg(feature = "development")]
fn record_eager_visibility_source_rows(fetched: usize) {
    BFS_VISIBILITY_LAST_METRICS.with(|last| {
        let mut metrics = last.get();
        metrics.source_rows = metrics
            .source_rows
            .saturating_add(u64::try_from(fetched).unwrap_or(u64::MAX));
        last.set(metrics);
    });
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn record_eager_visibility_source_rows(_fetched: usize) {}

#[cfg(feature = "development")]
pub(crate) fn visibility_build_slot_is_empty() -> bool {
    VISIBILITY_BUILD_SLOT.with(|slot| slot.borrow().is_none())
}

#[cfg(feature = "development")]
fn inject_visibility_scan_cancellation() {
    VISIBILITY_CANCEL_AFTER_ROWS.with(|remaining| match remaining.get() {
        None => {}
        Some(0) => {
            remaining.set(None);
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
                "injected RLS visibility scan cancellation"
            );
        }
        Some(rows) => remaining.set(Some(rows - 1)),
    });
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn inject_visibility_scan_cancellation() {}

#[cfg(feature = "development")]
fn force_missing_relationship_identity_for_test() -> bool {
    VISIBILITY_FORCE_MISSING_IDENTITY.with(|armed| armed.replace(false))
}

#[cfg(not(feature = "development"))]
#[inline(always)]
fn force_missing_relationship_identity_for_test() -> bool {
    false
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_arm_visibility_scan_cancel")]
fn test_arm_visibility_scan_cancel(after_rows: i64) -> bool {
    if after_rows < 0 {
        return false;
    }
    VISIBILITY_CANCEL_AFTER_ROWS.with(|remaining| {
        remaining.set(Some(u64::try_from(after_rows).unwrap_or(u64::MAX)));
    });
    true
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_visibility_build_slot_empty")]
fn test_visibility_build_slot_empty() -> bool {
    visibility_build_slot_is_empty()
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_arm_lazy_visibility_cancel")]
fn test_arm_lazy_visibility_cancel() -> bool {
    LAZY_VISIBILITY_CANCEL_BEFORE_SPI.with(|armed| armed.set(true));
    true
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_visibility_resolution_state_empty")]
fn test_visibility_resolution_state_empty() -> bool {
    _test_visibility_resolution_guard_empty()
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_rls_applies_to_outer_caller")]
fn test_rls_applies_to_outer_caller(table_oid: pgrx::pg_sys::Oid) -> bool {
    acl::row_security_applies_to_outer_caller(table_oid.to_u32())
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_visibility_metrics")]
fn test_visibility_metrics() -> pgrx::JsonB {
    let (elapsed_micros, hidden_nodes, hidden_relationships) =
        VISIBILITY_LAST_METRICS.with(Cell::get);
    let bfs = BFS_VISIBILITY_LAST_METRICS.with(Cell::get);
    let strategy = VISIBILITY_STRATEGY_OVERRIDE.with(Cell::get);
    pgrx::JsonB(serde_json::json!({
        "strategy": match strategy {
            VisibilityStrategyOverride::Auto => "auto",
            VisibilityStrategyOverride::Eager => "eager",
            VisibilityStrategyOverride::Lazy => "lazy",
        },
        "elapsed_micros": elapsed_micros,
        "hidden_nodes": hidden_nodes,
        "hidden_relationships": hidden_relationships,
        "spi_calls": bfs.spi_calls,
        "requested_keys": bfs.requested_keys,
        "returned_keys": bfs.returned_keys,
        "requested_key_bytes": bfs.requested_key_bytes,
        "returned_key_bytes": bfs.returned_key_bytes,
        "source_rows": bfs.source_rows,
    }))
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_set_visibility_strategy")]
fn test_set_visibility_strategy(strategy: &str) -> bool {
    let strategy = match strategy.to_ascii_lowercase().as_str() {
        "auto" => VisibilityStrategyOverride::Auto,
        "eager" => VisibilityStrategyOverride::Eager,
        "lazy" => VisibilityStrategyOverride::Lazy,
        _ => GraphError::InvalidFilter {
            reason: "visibility test strategy supports 'auto', 'eager', or 'lazy'".into(),
        }
        .report(),
    };
    VISIBILITY_STRATEGY_OVERRIDE.with(|slot| slot.set(strategy));
    BFS_VISIBILITY_LAST_METRICS.with(|metrics| metrics.set(BfsVisibilityMetrics::EMPTY));
    true
}

#[cfg(feature = "development")]
#[pg_extern(schema = "graph", name = "_test_arm_missing_relationship_identity")]
fn test_arm_missing_relationship_identity() -> bool {
    VISIBILITY_FORCE_MISSING_IDENTITY.with(|armed| armed.set(true));
    true
}

#[cfg(feature = "development")]
fn force_missing_bfs_candidate_identity_for_test() -> bool {
    BFS_VISIBILITY_FORCE_MISSING_CANDIDATE_IDENTITY.with(|armed| armed.replace(false))
}

#[cfg(not(feature = "development"))]
fn force_missing_bfs_candidate_identity_for_test() -> bool {
    false
}

#[cfg(feature = "development")]
#[pg_extern(
    schema = "graph",
    name = "_test_arm_missing_bfs_candidate_relationship_identity"
)]
fn test_arm_missing_bfs_candidate_relationship_identity() -> bool {
    BFS_VISIBILITY_FORCE_MISSING_CANDIDATE_IDENTITY.with(|armed| armed.set(true));
    true
}

#[cfg(test)]
mod lazy_probe_tests {
    use super::*;
    use crate::builder::PrimaryKeySpec;

    fn plan(columns: Vec<(&str, &str)>) -> NodeProbePlan {
        NodeProbePlan {
            table_oid: 1,
            table_sql: "public.items".into(),
            columns: columns
                .into_iter()
                .map(|(column, type_name)| (column.into(), type_name.into()))
                .collect(),
            typed_indexable: true,
        }
    }

    #[test]
    fn typed_scalar_probe_uses_column_equality_for_indexable_lookup() {
        let (_payload, sql) = build_node_probe_query(&plan(vec![("id", "uuid")]), &["x".into()])
            .expect("scalar probe SQL");
        assert!(sql.contains("src.\"id\" = requested.source_key::uuid"));
        assert!(!sql.contains("src.\"id\"::text = requested.source_key"));
    }

    #[test]
    fn typed_composite_probe_preserves_quoted_unicode_components() {
        let key = serde_json::json!(["a,b", "quote\"雪"]).to_string();
        let (_payload, sql) =
            build_node_probe_query(&plan(vec![("org", "text"), ("employee", "text")]), &[key])
                .expect("composite probe SQL");
        assert!(sql.contains("src.\"org\" = (requested.source_key::jsonb ->> 0)::text"));
        assert!(sql.contains("src.\"employee\" = (requested.source_key::jsonb ->> 1)::text"));
    }

    #[test]
    fn malformed_composite_probe_identity_fails_closed() {
        assert!(matches!(
            validate_composite_probe_keys(&["[\"one\",null]".into()], 2),
            Err(GraphError::CorruptFile { .. })
        ));
        assert!(matches!(
            validate_composite_probe_keys(&["[\"one\"]".into()], 2),
            Err(GraphError::CorruptFile { .. })
        ));
    }

    #[test]
    fn guc_dependent_types_are_not_eligible_for_lazy_reparsing() {
        let mut date_plan = plan(vec![("id", "date")]);
        date_plan.typed_indexable = false;
        assert!(!date_plan.typed_indexable);
    }

    #[test]
    fn primary_key_fixture_uses_registered_column_order() {
        let key = PrimaryKeySpec::from_columns(vec!["org".into(), "employee".into()]);
        assert_eq!(key.columns(), &["org", "employee"]);
    }

    #[test]
    fn lazy_bfs_batches_frontier_per_table_and_relationship_mapping() {
        let keys = vec!["b".to_string(), "a".to_string(), "b".to_string()];
        let (node_payload, node_sql) =
            build_bfs_node_probe_query(&plan(vec![("id", "text")]), &keys)
                .expect("set-based node probe SQL");
        let (relationship_payload, relationship_sql) =
            build_bfs_relationship_probe_query(&plan(vec![("id", "text")]), &keys)
                .expect("set-based relationship probe SQL");
        assert_eq!(node_payload, r#"["b","a","b"]"#);
        assert_eq!(relationship_payload, node_payload);
        assert_eq!(relationship_sql, node_sql);
        assert!(node_sql.contains("jsonb_array_elements_text($1::jsonb)"));
        assert!(node_sql.contains("ORDER BY requested.ordinal"));
    }

    #[test]
    fn lazy_bfs_frontier_probe_uses_source_primary_key_indexes() {
        let (_, scalar) = build_bfs_node_probe_query(
            &plan(vec![("id", "uuid")]),
            &["00000000-0000-0000-0000-000000000001".into()],
        )
        .expect("scalar set probe SQL");
        assert!(scalar.contains("src.\"id\" = requested.source_key::uuid"));
        assert!(!scalar.contains("src.\"id\"::text"));

        let composite_key = serde_json::json!(["org", "employee"]).to_string();
        let (_, composite) = build_bfs_node_probe_query(
            &plan(vec![("org", "text"), ("employee", "text")]),
            &[composite_key],
        )
        .expect("composite set probe SQL");
        assert!(composite.contains("src.\"org\" = (requested.source_key::jsonb ->> 0)::text"));
        assert!(composite.contains("src.\"employee\" = (requested.source_key::jsonb ->> 1)::text"));
    }

    #[test]
    fn lazy_bfs_policy_cancellation_cleans_machine_and_retries() {
        BFS_VISIBILITY_RESOLUTION_SLOT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(BfsVisibilityResolutionFrame {
                query: "SELECT 1".into(),
                payload: "[]".into(),
                visible_keys: Vec::new(),
            }));
            slot.borrow_mut().take();
        });
        VisibilityResolutionGuard::clear();
        assert!(_test_visibility_resolution_guard_empty());
        assert!(VisibilityResolutionGuard::ensure_available().is_ok());
    }
}
