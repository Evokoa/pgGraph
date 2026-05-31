//! Logical plans produced by GQL semantic binding.

use crate::gql::ast::LiteralValue;

/// Bound logical statement.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LogicalStatement {
    /// Read-only query.
    Read(LogicalPlan),
    /// Node-only read query.
    NodeScan(LogicalNodeScan),
    /// Node creation write.
    CreateNode(LogicalCreateNode),
    /// Mapped node property update.
    SetProperty(LogicalSetProperty),
    /// Mapped node property removal.
    RemoveProperty(LogicalRemoveProperty),
    /// Mapped edge row deletion.
    DeleteEdge(LogicalDeleteEdge),
    /// Mapped node detach-delete.
    DetachDeleteNode(LogicalDetachDeleteNode),
    /// Mapped node merge/upsert.
    MergeNode(LogicalMergeNode),
}

/// Bound read-only logical query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalPlan {
    /// Whether unmatched source rows should be null-extended.
    pub(crate) optional: bool,
    /// Source node binding.
    pub(crate) source: BoundNode,
    /// Single relationship expansion.
    pub(crate) relationship: BoundRel,
    /// Target node binding.
    pub(crate) target: BoundNode,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnBinding>,
    /// Row-stream DISTINCT projection stages introduced by `WITH DISTINCT`.
    pub(crate) distinct_stages: Vec<Vec<ReturnBinding>>,
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

/// Bound node-only logical query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalNodeScan {
    /// Scanned node binding.
    pub(crate) node: BoundNode,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnBinding>,
    /// Row-stream DISTINCT projection stages introduced by `WITH DISTINCT`.
    pub(crate) distinct_stages: Vec<Vec<ReturnBinding>>,
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

/// Bound node creation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalCreateNode {
    /// Created node binding.
    pub(crate) node: BoundNode,
    /// Property values to insert into PostgreSQL.
    pub(crate) properties: Vec<CreateProperty>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnBinding>,
}

/// Bound node merge/upsert.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalMergeNode {
    /// Merged node binding.
    pub(crate) node: BoundNode,
    /// Property values used for the insert branch and identity lookup.
    pub(crate) properties: Vec<CreateProperty>,
    /// Optional property assignment applied only to inserted rows.
    pub(crate) on_create: Option<CreateProperty>,
    /// Optional property assignment applied only to matched rows.
    pub(crate) on_match: Option<CreateProperty>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnBinding>,
}

/// Bound node property update.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalSetProperty {
    /// Matched node binding.
    pub(crate) node: BoundNode,
    /// Optional hydrated-row predicate selecting the row.
    pub(crate) predicate: Option<Predicate>,
    /// Source table column name to update.
    pub(crate) property: String,
    /// New property value.
    pub(crate) value: CreateValue,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnBinding>,
}

/// Bound node property removal.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalRemoveProperty {
    /// Matched node binding.
    pub(crate) node: BoundNode,
    /// Optional hydrated-row predicate selecting the row.
    pub(crate) predicate: Option<Predicate>,
    /// Source table column or registered JSONB property path to remove.
    pub(crate) property: String,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnBinding>,
}

/// Bound mapped edge deletion.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalDeleteEdge {
    /// Source node binding in the query pattern.
    pub(crate) source: BoundNode,
    /// Relationship binding.
    pub(crate) relationship: BoundRel,
    /// Relationship variable targeted by `DELETE`.
    pub(crate) rel_var: String,
    /// Target node binding in the query pattern.
    pub(crate) target: BoundNode,
    /// Registered edge row mapping.
    pub(crate) edge: BoundMappedEdge,
    /// Optional hydrated-row predicate selecting the relationship match.
    pub(crate) predicate: Option<Predicate>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnBinding>,
}

/// Bound mapped node detach-delete.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalDetachDeleteNode {
    /// Matched node binding.
    pub(crate) node: BoundNode,
    /// Optional hydrated-row predicate selecting the row.
    pub(crate) predicate: Option<Predicate>,
    /// Registered edge-row mappings that may contain incident edges.
    pub(crate) incident_edges: Vec<BoundIncidentEdge>,
    /// Return slots in requested order.
    pub(crate) returns: Vec<CreateReturnBinding>,
}

/// Bound incident edge row details for `DETACH DELETE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundIncidentEdge {
    /// Relationship type label.
    pub(crate) rel_type: String,
    /// Registered edge row mapping.
    pub(crate) edge: BoundMappedEdge,
}

/// Bound mapped edge row details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundMappedEdge {
    /// Edge row table OID.
    pub(crate) edge_table_oid: u32,
    /// Registered source node table OID.
    pub(crate) source_table_oid: u32,
    /// Registered target node table OID.
    pub(crate) target_table_oid: u32,
    /// Edge row source key column.
    pub(crate) source_column: String,
    /// Edge row target key column.
    pub(crate) target_column: String,
    /// Whether the edge is registered bidirectionally.
    pub(crate) bidirectional: bool,
}

/// Bound property value for a write.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreateProperty {
    /// Source table column name.
    pub(crate) property: String,
    /// Value expression.
    pub(crate) value: CreateValue,
}

/// Bound write value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CreateValue {
    /// Literal scalar.
    Literal(LiteralValue),
    /// Query parameter by name.
    Param(String),
}

/// Return slot for `CREATE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CreateReturnBinding {
    /// Whole created node variable.
    Node { name: String },
    /// Created node property.
    Property { property: String, name: String },
}

impl CreateReturnBinding {
    /// Return the output column name.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Node { name } | Self::Property { name, .. } => name,
        }
    }
}

/// Bound node variable and table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundNode {
    /// GQL variable name.
    pub(crate) var: String,
    /// GQL label text.
    pub(crate) label: String,
    /// Backing source table OID.
    pub(crate) table_oid: u32,
    /// Registered property columns.
    pub(crate) properties: std::collections::BTreeSet<String>,
}

/// Bound relationship type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundRel {
    /// Optional GQL relationship variable name.
    pub(crate) var: Option<String>,
    /// GQL relationship type text.
    pub(crate) rel_type: String,
    /// Traversal direction.
    pub(crate) direction: BoundDirection,
    /// Hop bounds.
    pub(crate) hops: HopBounds,
}

/// Bound relationship direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundDirection {
    /// Source to target.
    Out,
    /// Target to source.
    In,
    /// Either direction.
    Undirected,
}

/// Bound hop count range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HopBounds {
    /// Minimum hop count.
    pub(crate) min: u32,
    /// Maximum hop count.
    pub(crate) max: u32,
}

/// Bound `RETURN` variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReturnBinding {
    /// Whole node variable.
    Node { side: BindingSide, name: String },
    /// Whole relationship variable.
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
    /// Node property.
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

impl ReturnBinding {
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

    /// Return whether this binding projects a scalar value sortable by output name.
    pub(crate) fn is_sortable_scalar(&self) -> bool {
        matches!(
            self,
            Self::Property { .. }
                | Self::Aggregate { .. }
                | Self::PathFunction {
                    func: PathFunc::Length,
                    ..
                }
        )
    }

    /// Return whether this binding is an aggregate.
    pub(crate) fn is_aggregate(&self) -> bool {
        matches!(self, Self::Aggregate { .. })
    }
}

/// Bound path function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathFunc {
    /// `nodes(path)`.
    Nodes,
    /// `relationships(path)`.
    Relationships,
    /// `length(path)`.
    Length,
}

/// Bound aggregate function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggregateFunc {
    /// `count(...)`.
    Count,
    /// `sum(...)`.
    Sum,
    /// `avg(...)`.
    Avg,
    /// `min(...)`.
    Min,
    /// `max(...)`.
    Max,
    /// `collect(...)`.
    Collect,
}

/// Bound aggregate argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AggregateArg {
    /// `*`.
    All,
    /// Node variable.
    Node(BindingSide),
    /// Relationship variable.
    Relationship,
    /// Node property.
    Property {
        /// Source or target binding.
        side: BindingSide,
        /// Source property name.
        property: String,
    },
}

/// Bound sort key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SortBinding {
    /// Value to sort by.
    pub(crate) key: SortBindingKey,
    /// Sort descending when true.
    pub(crate) desc: bool,
}

/// Bound sort value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SortBindingKey {
    /// Sort by a return column name.
    ReturnName(String),
    /// Sort by a node property.
    Property {
        /// Source or target binding.
        side: BindingSide,
        /// Source property name.
        property: String,
    },
}

/// Which side of the one-hop match a value references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingSide {
    /// Source node.
    Source,
    /// Target node.
    Target,
}

/// Bound boolean predicate.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Predicate {
    /// Logical conjunction.
    And(Box<Predicate>, Box<Predicate>),
    /// Logical disjunction.
    Or(Box<Predicate>, Box<Predicate>),
    /// Logical negation.
    Not(Box<Predicate>),
    /// Comparison predicate.
    Compare {
        /// Left operand.
        lhs: ValueExpr,
        /// Comparison operator.
        op: BoundCmpOp,
        /// Optional right operand.
        rhs: Option<ValueExpr>,
    },
}

/// Bound value expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ValueExpr {
    /// Property read.
    Property {
        /// Source or target binding.
        side: BindingSide,
        /// Property name.
        property: String,
    },
    /// Literal scalar.
    Literal(serde_json::Value),
    /// Query parameter by name.
    Param(String),
    /// Literal list.
    List(Vec<serde_json::Value>),
}

/// Bound comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundCmpOp {
    /// `=`
    Eq,
    /// `<>`
    Neq,
    /// `<`
    Lt,
    /// `<=`
    Lte,
    /// `>`
    Gt,
    /// `>=`
    Gte,
    /// `IN`
    In,
    /// `IS NULL`
    IsNull,
    /// `IS NOT NULL`
    IsNotNull,
}
