//! Physical plans executable against engine topology stores or PostgreSQL SPI.

use super::logical_plan::{
    AggregateArg, AggregateFunc, BindingSide, BoundDirection, HopBounds, PathFunc, Predicate,
    SortBinding,
};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum GQL matches collected before sorting/projection.
pub(crate) const MAX_GQL_RESULT_ROWS: usize = 10_000;
/// Maximum unique keys a bounded DISTINCT operation may retain.
pub(crate) const MAX_GQL_DISTINCT_KEYS: usize = MAX_GQL_RESULT_ROWS;

/// Physical GQL statement.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PhysicalStatement {
    /// Read-only topology query.
    Read(PhysicalPlan),
    /// Node-only read query.
    NodeScan(PhysicalNodeScan),
    /// Multi-pattern read query.
    JoinRead(PhysicalJoinPlan),
    /// Wildcard single-hop path variable query.
    WildcardPathRead(PhysicalWildcardPathPlan),
    /// PostgreSQL-backed node creation.
    CreateNode(PhysicalCreateNode),
    /// PostgreSQL-backed node property update.
    SetProperty(PhysicalSetProperty),
    /// PostgreSQL-backed node property removal.
    RemoveProperty(PhysicalRemoveProperty),
    /// PostgreSQL-backed edge row deletion.
    DeleteEdge(PhysicalDeleteEdge),
    /// PostgreSQL-backed node detach-delete.
    DetachDeleteNode(PhysicalDetachDeleteNode),
    /// PostgreSQL-backed node merge/upsert.
    MergeNode(PhysicalMergeNode),
}

/// Physical wildcard single-hop path query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalWildcardPathPlan {
    /// Path variable name bound to the whole matched path.
    pub(crate) path_var: String,
    /// Optional source node variable for the first node in path order.
    pub(crate) source_var: Option<String>,
    /// Optional relationship variable.
    pub(crate) rel_var: Option<String>,
    /// Optional target node variable for the second node in path order.
    pub(crate) target_var: Option<String>,
    /// Traversal direction requested by the relationship pattern.
    pub(crate) direction: BoundDirection,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnSlot>,
    /// Optional source-table filter.
    pub(crate) source_table_filter: Option<u32>,
    /// Optional target-table filter.
    pub(crate) target_table_filter: Option<u32>,
    /// Optional relationship type filter.
    pub(crate) rel_type_filter: Option<String>,
    /// Fixed relationship segments in path order.
    pub(crate) segments: Vec<PhysicalWildcardPathSegment>,
    /// Source-table OIDs requiring ACL checks before wildcard expansion.
    pub(crate) required_node_table_oids: BTreeSet<u32>,
    /// GQL labels keyed by source-table OID.
    pub(crate) table_labels: BTreeMap<u32, String>,
    /// Relationship type labels that may appear in wildcard output.
    pub(crate) rel_type_labels: BTreeSet<String>,
    /// Optional hydrated-row predicate over named path-node variables.
    pub(crate) predicate: Option<Predicate>,
    /// Number of rows to skip after projection.
    pub(crate) skip: Option<u64>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u64>,
}

/// One fixed segment in a physical wildcard path plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalWildcardPathSegment {
    /// Optional relationship variable for this segment.
    pub(crate) rel_var: Option<String>,
    /// Optional target node variable for this segment.
    pub(crate) target_var: Option<String>,
    /// Traversal direction requested by the relationship pattern.
    pub(crate) direction: BoundDirection,
    /// Hop bounds for this segment.
    pub(crate) hops: HopBounds,
    /// Optional target-table filter.
    pub(crate) target_table_filter: Option<u32>,
    /// Optional relationship type filter.
    pub(crate) rel_type_filter: Option<String>,
}

/// Single-hop physical plan for Phase 1B.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalPlan {
    /// Whether unmatched source rows should be null-extended.
    pub(crate) optional: bool,
    /// Source node variable.
    pub(crate) source_var: String,
    /// Source table OID.
    pub(crate) source_table_oid: u32,
    /// Source label.
    pub(crate) source_label: String,
    /// Relationship type label.
    pub(crate) rel_type: String,
    /// Optional relationship variable.
    pub(crate) rel_var: Option<String>,
    /// Traversal direction.
    pub(crate) direction: BoundDirection,
    /// Hop bounds.
    pub(crate) hops: HopBounds,
    /// Target node variable.
    pub(crate) target_var: String,
    /// Target table OID.
    pub(crate) target_table_oid: u32,
    /// Target label.
    pub(crate) target_label: String,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnSlot>,
    /// Row-stream DISTINCT projection stages introduced by `WITH DISTINCT`.
    pub(crate) distinct_stages: Vec<Vec<ReturnSlot>>,
    /// Whether final projected rows should be deduplicated.
    pub(crate) distinct: bool,
    /// Optional hydrated-row predicate.
    pub(crate) predicate: Option<Predicate>,
    /// Sort keys in requested order.
    pub(crate) order_by: Vec<SortBinding>,
    /// Number of rows to skip after ordering.
    pub(crate) skip: Option<u64>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u64>,
}

/// Physical multi-pattern join query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalJoinPlan {
    /// Node variables in row-slot order.
    pub(crate) node_slots: Vec<PhysicalJoinNodeSlot>,
    /// Relationship variables in row-slot order.
    pub(crate) rel_slots: Vec<PhysicalJoinRelSlot>,
    /// Path variables in row-slot order.
    pub(crate) path_slots: Vec<PhysicalJoinPathSlot>,
    /// Single-hop patterns in source order.
    pub(crate) patterns: Vec<PhysicalJoinPattern>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnSlot>,
    /// Whether final projected rows should be deduplicated.
    pub(crate) distinct: bool,
    /// Optional hydrated-row predicate evaluated after all joined slots bind.
    pub(crate) predicate: Option<Predicate>,
    /// Sort keys in requested order.
    pub(crate) order_by: Vec<SortBinding>,
    /// Table OIDs requiring ACL checks before execution.
    pub(crate) required_table_oids: BTreeSet<u32>,
    /// Number of rows to skip after projection.
    pub(crate) skip: Option<u64>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u64>,
}

/// Physical node variable slot in a multi-pattern join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalJoinNodeSlot {
    /// Node variable.
    pub(crate) var: String,
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source label.
    pub(crate) label: String,
}

/// Physical relationship variable slot in a multi-pattern join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalJoinRelSlot {
    /// Relationship variable.
    pub(crate) var: String,
    /// Pattern index that binds this relationship.
    pub(crate) pattern_slot: usize,
}

/// Physical path variable slot in a multi-pattern join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalJoinPathSlot {
    /// Path variable.
    pub(crate) var: String,
    /// Pattern index that binds this path.
    pub(crate) pattern_slot: usize,
}

/// Physical fixed single-hop pattern in a multi-pattern join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalJoinPattern {
    /// Source node slot index.
    pub(crate) source_slot: usize,
    /// Relationship type label.
    pub(crate) rel_type: String,
    /// Traversal direction.
    pub(crate) direction: BoundDirection,
    /// Target node slot index.
    pub(crate) target_slot: usize,
}

/// Physical node creation plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalCreateNode {
    /// Created node variable.
    pub(crate) var: String,
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source label.
    pub(crate) label: String,
    /// Property values to insert into PostgreSQL.
    pub(crate) properties: Vec<CreatePropertySlot>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnSlot>,
}

/// Physical node merge/upsert plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalMergeNode {
    /// Merged node variable.
    pub(crate) var: String,
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source label.
    pub(crate) label: String,
    /// Property values used for the insert branch and identity lookup.
    pub(crate) properties: Vec<CreatePropertySlot>,
    /// Optional property assignment applied only to inserted rows.
    pub(crate) on_create: Option<CreatePropertySlot>,
    /// Optional property assignment applied only to matched rows.
    pub(crate) on_match: Option<CreatePropertySlot>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnSlot>,
}

/// Physical node property update plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalSetProperty {
    /// Matched node variable.
    pub(crate) var: String,
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source label.
    pub(crate) label: String,
    /// Optional hydrated-row predicate selecting the row.
    pub(crate) predicate: Option<Predicate>,
    /// Source table column name to update.
    pub(crate) property: String,
    /// New property value.
    pub(crate) value: CreateValueSlot,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnSlot>,
}

/// Physical node property removal plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalRemoveProperty {
    /// Matched node variable.
    pub(crate) var: String,
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source label.
    pub(crate) label: String,
    /// Optional hydrated-row predicate selecting the row.
    pub(crate) predicate: Option<Predicate>,
    /// Source table column or registered JSONB property path to remove.
    pub(crate) property: String,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnSlot>,
}

/// Physical mapped edge row deletion plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalDeleteEdge {
    /// Source node variable.
    pub(crate) source_var: String,
    /// Source table OID in the query pattern.
    pub(crate) source_table_oid: u32,
    /// Source label.
    pub(crate) source_label: String,
    /// Relationship type label.
    pub(crate) rel_type: String,
    /// Relationship variable.
    pub(crate) rel_var: String,
    /// Traversal direction.
    pub(crate) direction: BoundDirection,
    /// Target node variable.
    pub(crate) target_var: String,
    /// Target table OID in the query pattern.
    pub(crate) target_table_oid: u32,
    /// Target label.
    pub(crate) target_label: String,
    /// Registered edge row table OID.
    pub(crate) edge_table_oid: u32,
    /// Registered source node table OID.
    pub(crate) edge_source_table_oid: u32,
    /// Registered target node table OID.
    pub(crate) edge_target_table_oid: u32,
    /// Edge row source key column.
    pub(crate) source_column: String,
    /// Edge row target key column.
    pub(crate) target_column: String,
    /// Whether the edge row is registered bidirectionally.
    pub(crate) bidirectional: bool,
    /// Optional hydrated-row predicate.
    pub(crate) predicate: Option<Predicate>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnSlot>,
}

/// Physical mapped node detach-delete plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalDetachDeleteNode {
    /// Matched node variable.
    pub(crate) var: String,
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source label.
    pub(crate) label: String,
    /// Optional hydrated-row predicate.
    pub(crate) predicate: Option<Predicate>,
    /// Incident edge-row mappings to delete before the node row.
    pub(crate) incident_edges: Vec<PhysicalIncidentEdge>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnSlot>,
}

/// Physical incident edge-row mapping for `DETACH DELETE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalIncidentEdge {
    /// Relationship type label.
    pub(crate) rel_type: String,
    /// Registered edge row table OID.
    pub(crate) edge_table_oid: u32,
    /// Registered source node table OID.
    pub(crate) edge_source_table_oid: u32,
    /// Registered target node table OID.
    pub(crate) edge_target_table_oid: u32,
    /// Edge row source key column.
    pub(crate) source_column: String,
    /// Edge row target key column.
    pub(crate) target_column: String,
    /// Whether the edge row is registered bidirectionally.
    pub(crate) bidirectional: bool,
}

/// Physical node-only scan plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalNodeScan {
    /// Node variable.
    pub(crate) var: String,
    /// Source table OID.
    pub(crate) table_oid: u32,
    /// Source label.
    pub(crate) label: String,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnSlot>,
    /// Row-stream DISTINCT projection stages introduced by `WITH DISTINCT`.
    pub(crate) distinct_stages: Vec<Vec<ReturnSlot>>,
    /// Whether final projected rows should be deduplicated.
    pub(crate) distinct: bool,
    /// Optional hydrated-row predicate.
    pub(crate) predicate: Option<Predicate>,
    /// Sort keys in requested order.
    pub(crate) order_by: Vec<SortBinding>,
    /// Number of rows to skip after ordering.
    pub(crate) skip: Option<u64>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u64>,
}

/// Physical property value for a write.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreatePropertySlot {
    /// Source table column name.
    pub(crate) property: String,
    /// Value expression.
    pub(crate) value: CreateValueSlot,
}

/// Physical write value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CreateValueSlot {
    /// Literal scalar.
    Literal(serde_json::Value),
    /// Query parameter by name.
    Param(String),
}

/// Physical return slot for `CREATE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CreateReturnSlot {
    /// Whole created node value.
    Node { name: String },
    /// Created node property value.
    Property { property: String, name: String },
}

/// Physical return slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReturnSlot {
    /// Whole node value.
    Node { side: BindingSide, name: String },
    /// Whole relationship value.
    Relationship { name: String },
    /// Whole path value.
    Path { name: String },
    /// Path function value.
    PathFunction {
        /// Function to evaluate.
        func: PathFunc,
        /// Return column name.
        name: String,
    },
    /// Node property value.
    Property {
        /// Source or target binding.
        side: BindingSide,
        /// Source property name.
        property: String,
        /// Return column name.
        name: String,
    },
    /// Aggregate value.
    Aggregate {
        /// Aggregate function.
        func: AggregateFunc,
        /// Aggregate input.
        arg: AggregateArg,
        /// Whether duplicate aggregate inputs should be ignored.
        distinct: bool,
        /// Return column name.
        name: String,
    },
}

impl ReturnSlot {
    /// Return the output column name.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Node { name, .. }
            | Self::Relationship { name }
            | Self::Path { name }
            | Self::PathFunction { name, .. }
            | Self::Property { name, .. }
            | Self::Aggregate { name, .. } => name,
        }
    }

    /// Return whether this slot contains an aggregate value.
    pub(crate) fn is_aggregate(&self) -> bool {
        matches!(self, Self::Aggregate { .. })
    }
}

fn has_aggregate_return(returns: &[ReturnSlot]) -> bool {
    returns.iter().any(ReturnSlot::is_aggregate)
}

impl PhysicalPlan {
    /// Table OIDs whose rows must be visible to the current SQL role.
    pub(crate) fn required_table_oids(&self) -> [u32; 2] {
        [self.source_table_oid, self.target_table_oid]
    }

    /// Maximum matches the executor should collect for this plan.
    pub(crate) fn execution_row_cap(&self) -> usize {
        if self.distinct || !self.distinct_stages.is_empty() || has_aggregate_return(&self.returns)
        {
            return MAX_GQL_RESULT_ROWS;
        }
        if self.order_by.is_empty() && self.predicate.is_none() {
            if let Some(limit) = self.limit {
                let requested = self.skip.unwrap_or(0).saturating_add(limit);
                return usize::try_from(requested)
                    .unwrap_or(usize::MAX)
                    .min(MAX_GQL_RESULT_ROWS);
            }
        }
        MAX_GQL_RESULT_ROWS
    }

    /// Whether hitting the execution cap means results would be incomplete.
    pub(crate) fn cap_exhaustion_is_error(&self) -> bool {
        self.distinct
            || !self.distinct_stages.is_empty()
            || has_aggregate_return(&self.returns)
            || !self.order_by.is_empty()
            || self.limit.is_none()
            || self.predicate.is_some()
    }
}

impl PhysicalWildcardPathPlan {
    /// Maximum matches the executor should collect for this plan.
    pub(crate) fn execution_row_cap(&self) -> usize {
        if self.predicate.is_none() {
            if let Some(limit) = self.limit {
                let requested = self.skip.unwrap_or(0).saturating_add(limit);
                return usize::try_from(requested)
                    .unwrap_or(usize::MAX)
                    .min(MAX_GQL_RESULT_ROWS);
            }
        }
        MAX_GQL_RESULT_ROWS
    }

    /// Whether hitting the execution cap means results would be incomplete.
    pub(crate) fn cap_exhaustion_is_error(&self) -> bool {
        self.limit.is_none() || self.predicate.is_some()
    }
}

impl PhysicalJoinPlan {
    /// Table OIDs whose rows must be visible to the current SQL role.
    pub(crate) fn required_table_oids(&self) -> impl Iterator<Item = u32> + '_ {
        self.required_table_oids.iter().copied()
    }

    /// Maximum matches the executor should collect for this plan.
    pub(crate) fn execution_row_cap(&self) -> usize {
        if !self.distinct && self.predicate.is_none() && self.order_by.is_empty() {
            if let Some(limit) = self.limit {
                let requested = self.skip.unwrap_or(0).saturating_add(limit);
                return usize::try_from(requested)
                    .unwrap_or(usize::MAX)
                    .min(MAX_GQL_RESULT_ROWS);
            }
        }
        MAX_GQL_RESULT_ROWS
    }

    /// Whether hitting the execution cap means results would be incomplete.
    pub(crate) fn cap_exhaustion_is_error(&self) -> bool {
        self.limit.is_none()
            || self.distinct
            || self.predicate.is_some()
            || !self.order_by.is_empty()
    }
}

impl PhysicalCreateNode {
    /// Table OID whose rows will be inserted.
    pub(crate) fn required_table_oid(&self) -> u32 {
        self.table_oid
    }
}

impl PhysicalMergeNode {
    /// Table OID whose row will be inserted or locked/updated.
    pub(crate) fn required_table_oid(&self) -> u32 {
        self.table_oid
    }
}

impl PhysicalSetProperty {
    /// Table OID whose row will be updated.
    pub(crate) fn required_table_oid(&self) -> u32 {
        self.table_oid
    }
}

impl PhysicalRemoveProperty {
    /// Table OID whose row will be updated.
    pub(crate) fn required_table_oid(&self) -> u32 {
        self.table_oid
    }
}

impl PhysicalDeleteEdge {
    /// Table OIDs whose node rows must be visible to the current SQL role.
    pub(crate) fn required_node_table_oids(&self) -> [u32; 2] {
        [self.source_table_oid, self.target_table_oid]
    }

    /// Edge row table OID whose row will be deleted.
    pub(crate) fn required_edge_table_oid(&self) -> u32 {
        self.edge_table_oid
    }
}

impl PhysicalDetachDeleteNode {
    /// Node table OID whose row will be deleted.
    pub(crate) fn required_node_table_oid(&self) -> u32 {
        self.table_oid
    }

    /// Incident edge table OIDs whose rows may be deleted.
    pub(crate) fn required_edge_table_oids(&self) -> Vec<u32> {
        let mut oids = self
            .incident_edges
            .iter()
            .map(|edge| edge.edge_table_oid)
            .collect::<Vec<_>>();
        oids.sort_unstable();
        oids.dedup();
        oids
    }
}

impl PhysicalNodeScan {
    /// Table OID whose rows must be visible to the current SQL role.
    pub(crate) fn required_table_oid(&self) -> u32 {
        self.table_oid
    }

    /// Maximum matches the executor should collect for this plan.
    pub(crate) fn execution_row_cap(&self) -> usize {
        if self.distinct || !self.distinct_stages.is_empty() || has_aggregate_return(&self.returns)
        {
            return MAX_GQL_RESULT_ROWS;
        }
        if self.order_by.is_empty() && self.predicate.is_none() {
            if let Some(limit) = self.limit {
                let requested = self.skip.unwrap_or(0).saturating_add(limit);
                return usize::try_from(requested)
                    .unwrap_or(usize::MAX)
                    .min(MAX_GQL_RESULT_ROWS);
            }
        }
        MAX_GQL_RESULT_ROWS
    }

    /// Whether hitting the execution cap means results would be incomplete.
    pub(crate) fn cap_exhaustion_is_error(&self) -> bool {
        self.distinct
            || !self.distinct_stages.is_empty()
            || has_aggregate_return(&self.returns)
            || !self.order_by.is_empty()
            || self.limit.is_none()
            || self.predicate.is_some()
    }
}
