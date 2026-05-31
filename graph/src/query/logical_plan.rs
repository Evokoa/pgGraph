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
}

/// Bound read-only logical query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalPlan {
    /// Source node binding.
    pub(crate) source: BoundNode,
    /// Single relationship expansion.
    pub(crate) relationship: BoundRel,
    /// Target node binding.
    pub(crate) target: BoundNode,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnBinding>,
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
    /// Node property.
    Property {
        /// Source or target binding.
        side: BindingSide,
        /// Source property name.
        property: String,
        /// Return column name.
        name: String,
    },
}

impl ReturnBinding {
    /// Return the output column name.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Node { name, .. } | Self::Relationship { name } | Self::Property { name, .. } => {
                name
            }
        }
    }

    /// Return whether this binding projects a scalar property value.
    pub(crate) fn is_property(&self) -> bool {
        matches!(self, Self::Property { .. })
    }
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
