//! Logical plan produced by GQL semantic binding.

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
    /// GQL relationship type text.
    pub(crate) rel_type: String,
}

/// Bound `RETURN` variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReturnBinding {
    /// Whole node variable.
    Node { side: BindingSide, name: String },
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
            Self::Node { name, .. } | Self::Property { name, .. } => name,
        }
    }
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
