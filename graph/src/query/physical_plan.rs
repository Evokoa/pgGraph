//! Physical plans executable against engine topology stores or PostgreSQL SPI.

use super::logical_plan::{BindingSide, BoundDirection, HopBounds, Predicate, SortBinding};

/// Maximum GQL matches collected before sorting/projection.
pub(crate) const MAX_GQL_RESULT_ROWS: usize = 10_000;

/// Physical GQL statement.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PhysicalStatement {
    /// Read-only topology query.
    Read(PhysicalPlan),
    /// Node-only read query.
    NodeScan(PhysicalNodeScan),
    /// PostgreSQL-backed node creation.
    CreateNode(PhysicalCreateNode),
    /// PostgreSQL-backed node property update.
    SetProperty(PhysicalSetProperty),
    /// PostgreSQL-backed edge row deletion.
    DeleteEdge(PhysicalDeleteEdge),
}

/// Single-hop physical plan for Phase 1B.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalPlan {
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
    /// Optional hydrated-row predicate.
    pub(crate) predicate: Option<Predicate>,
    /// Sort keys in requested order.
    pub(crate) order_by: Vec<SortBinding>,
    /// Number of rows to skip after ordering.
    pub(crate) skip: Option<u64>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u64>,
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
    /// Node property value.
    Property {
        /// Source or target binding.
        side: BindingSide,
        /// Source property name.
        property: String,
        /// Return column name.
        name: String,
    },
}

impl PhysicalPlan {
    /// Table OIDs whose rows must be visible to the current SQL role.
    pub(crate) fn required_table_oids(&self) -> [u32; 2] {
        [self.source_table_oid, self.target_table_oid]
    }

    /// Maximum matches the executor should collect for this plan.
    pub(crate) fn execution_row_cap(&self) -> usize {
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
        !self.order_by.is_empty() || self.limit.is_none() || self.predicate.is_some()
    }
}

impl PhysicalCreateNode {
    /// Table OID whose rows will be inserted.
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

impl PhysicalNodeScan {
    /// Table OID whose rows must be visible to the current SQL role.
    pub(crate) fn required_table_oid(&self) -> u32 {
        self.table_oid
    }

    /// Maximum matches the executor should collect for this plan.
    pub(crate) fn execution_row_cap(&self) -> usize {
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
        !self.order_by.is_empty() || self.limit.is_none() || self.predicate.is_some()
    }
}
