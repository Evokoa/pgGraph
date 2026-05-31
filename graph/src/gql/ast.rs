//! Abstract syntax tree for the supported GQL subset.

use super::errors::Span;

/// Parsed GQL statement.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Statement {
    /// Read-only `MATCH ... RETURN` query.
    Read(Query),
    /// `CREATE` node write query.
    Create(CreateQuery),
    /// `MATCH ... SET ... RETURN` property write query.
    Set(SetQuery),
    /// `MATCH ... DELETE ... RETURN` edge write query.
    Delete(DeleteQuery),
}

/// Parsed GQL query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Query {
    /// Required `MATCH` clause.
    pub(crate) match_: MatchClause,
    /// Optional `WHERE` clause.
    pub(crate) where_: Option<Expr>,
    /// Intermediate projection clauses that define downstream scope.
    pub(crate) with_: Vec<WithClause>,
    /// Required `RETURN` clause.
    pub(crate) return_: ReturnClause,
    /// Optional `ORDER BY` sort keys.
    pub(crate) order_by: Vec<SortItem>,
    /// Optional `SKIP` row offset.
    pub(crate) skip: Option<u64>,
    /// Optional `LIMIT` row bound.
    pub(crate) limit: Option<u64>,
    /// Full query span.
    pub(crate) span: Span,
}

/// `WITH` clause.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WithClause {
    /// `WITH DISTINCT`; parsed now and rejected during binding until Phase 3D.
    pub(crate) distinct: bool,
    /// Projected expressions visible to downstream clauses.
    pub(crate) items: Vec<ReturnItem>,
    /// Clause span.
    pub(crate) span: Span,
}

/// Parsed `CREATE` node query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreateQuery {
    /// Created node clause.
    pub(crate) create: CreateClause,
    /// Required `RETURN` clause.
    pub(crate) return_: ReturnClause,
    /// Full query span.
    pub(crate) span: Span,
}

/// Parsed mapped property update query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SetQuery {
    /// Required `MATCH` clause selecting the updated node.
    pub(crate) match_: MatchClause,
    /// Optional `WHERE` clause.
    pub(crate) where_: Option<Expr>,
    /// Property update clause.
    pub(crate) set: SetClause,
    /// Required `RETURN` clause.
    pub(crate) return_: ReturnClause,
    /// Full query span.
    pub(crate) span: Span,
}

/// Parsed mapped edge delete query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeleteQuery {
    /// Required `MATCH` clause selecting the relationship.
    pub(crate) match_: MatchClause,
    /// Optional `WHERE` clause.
    pub(crate) where_: Option<Expr>,
    /// Relationship delete clause.
    pub(crate) delete: DeleteClause,
    /// Required `RETURN` clause.
    pub(crate) return_: ReturnClause,
    /// Full query span.
    pub(crate) span: Span,
}

/// `CREATE` clause.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreateClause {
    /// Node to create.
    pub(crate) node: NodePat,
    /// Clause span.
    pub(crate) span: Span,
}

/// `SET` clause for one node property.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SetClause {
    /// Updated node property.
    pub(crate) target: PropertyRef,
    /// New property value.
    pub(crate) value: Operand,
    /// Clause span.
    pub(crate) span: Span,
}

/// `DELETE` clause for one relationship variable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeleteClause {
    /// Relationship variable to delete.
    pub(crate) var: Ident,
    /// Clause span.
    pub(crate) span: Span,
}

/// Variable property reference.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PropertyRef {
    /// Variable name.
    pub(crate) var: Ident,
    /// Property name.
    pub(crate) property: Ident,
    /// Full reference span.
    pub(crate) span: Span,
}

/// `MATCH` clause.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MatchClause {
    /// Whether this clause null-extends unmatched rows.
    pub(crate) optional: bool,
    /// Linear graph pattern.
    pub(crate) pattern: Pattern,
    /// Clause span.
    pub(crate) span: Span,
}

/// Single linear path pattern.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pattern {
    /// First node pattern.
    pub(crate) start: NodePat,
    /// Relationship/node pairs after the start node.
    pub(crate) tail: Vec<(RelPat, NodePat)>,
    /// Pattern span.
    pub(crate) span: Span,
}

/// Node pattern.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NodePat {
    /// Optional variable.
    pub(crate) var: Option<Ident>,
    /// Optional label.
    pub(crate) label: Option<Ident>,
    /// Inline property predicates.
    pub(crate) props: Vec<(Ident, Operand)>,
    /// Pattern span.
    pub(crate) span: Span,
}

impl NodePat {
    /// Return the node variable text.
    #[cfg(test)]
    pub(crate) fn var_text(&self) -> Option<&str> {
        self.var.as_ref().map(|ident| ident.text.as_str())
    }

    /// Return the node label text.
    #[cfg(test)]
    pub(crate) fn label_text(&self) -> Option<&str> {
        self.label.as_ref().map(|ident| ident.text.as_str())
    }
}

/// Relationship pattern.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RelPat {
    /// Optional variable.
    pub(crate) var: Option<Ident>,
    /// Optional relationship type.
    pub(crate) rel_type: Option<Ident>,
    /// Traversal direction.
    pub(crate) direction: Direction,
    /// Optional bounded variable-length relationship.
    pub(crate) var_len: Option<VarLen>,
    /// Inline property predicates.
    pub(crate) props: Vec<(Ident, Operand)>,
    /// Pattern span.
    pub(crate) span: Span,
}

impl RelPat {
    /// Return the relationship variable text.
    #[cfg(test)]
    pub(crate) fn var_text(&self) -> Option<&str> {
        self.var.as_ref().map(|ident| ident.text.as_str())
    }

    /// Return the relationship type text.
    #[cfg(test)]
    pub(crate) fn rel_type_text(&self) -> Option<&str> {
        self.rel_type.as_ref().map(|ident| ident.text.as_str())
    }
}

/// Relationship direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// `-[]->`
    Out,
    /// `<-[]-`
    In,
    /// `-[]-`
    Undirected,
}

/// Bounded variable-length relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VarLen {
    /// Minimum hop count.
    pub(crate) min: u32,
    /// Maximum hop count.
    pub(crate) max: u32,
    /// Variable-length marker span.
    pub(crate) span: Span,
}

/// Boolean predicate expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    /// Logical conjunction.
    And {
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Expression span.
        span: Span,
    },
    /// Logical disjunction.
    Or {
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Expression span.
        span: Span,
    },
    /// Logical negation.
    Not {
        /// Negated expression.
        expr: Box<Expr>,
        /// Expression span.
        span: Span,
    },
    /// Comparison predicate.
    Compare {
        /// Left operand.
        lhs: Operand,
        /// Comparison operator.
        op: CmpOp,
        /// Right operand, absent for null predicates.
        rhs: Option<Operand>,
        /// Predicate span.
        span: Span,
    },
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CmpOp {
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

/// Predicate, property-map, or function operand.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Operand {
    /// Variable property reference.
    Property {
        /// Variable name.
        var: Ident,
        /// Property name.
        property: Ident,
        /// Full operand span.
        span: Span,
    },
    /// Literal value.
    Literal(Literal),
    /// Named query parameter.
    Param {
        /// Parameter name.
        name: Ident,
        /// Full operand span.
        span: Span,
    },
    /// Literal list.
    List {
        /// Literal values.
        values: Vec<Literal>,
        /// Full list span.
        span: Span,
    },
}

/// Literal value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Literal {
    /// Literal value plus source location.
    Value {
        /// Literal value.
        value: LiteralValue,
        /// Literal span.
        span: Span,
    },
}

/// Scalar literal value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LiteralValue {
    /// String literal.
    Str(String),
    /// Integer literal.
    Int(i64),
    /// Floating-point literal.
    Float(f64),
    /// Boolean literal.
    Bool(bool),
    /// Null literal.
    Null,
}

/// `RETURN` clause.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReturnClause {
    /// `RETURN DISTINCT`; parsed now and rejected during binding until Phase 3.
    pub(crate) distinct: bool,
    /// Return expressions.
    pub(crate) items: Vec<ReturnItem>,
    /// Clause span.
    pub(crate) span: Span,
}

/// One `RETURN` expression with optional alias.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReturnItem {
    /// Return expression.
    pub(crate) expr: ReturnExpr,
    /// Optional alias.
    pub(crate) alias: Option<Ident>,
    /// Item span.
    pub(crate) span: Span,
}

impl ReturnItem {
    /// Return the alias text.
    #[cfg(test)]
    pub(crate) fn alias_text(&self) -> Option<&str> {
        self.alias.as_ref().map(|ident| ident.text.as_str())
    }
}

/// Returnable expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReturnExpr {
    /// Whole variable.
    Var {
        /// Variable name.
        var: Ident,
        /// Expression span.
        span: Span,
    },
    /// Variable property reference.
    Property {
        /// Variable name.
        var: Ident,
        /// Property name.
        property: Ident,
        /// Expression span.
        span: Span,
    },
    /// Supported function call.
    Func {
        /// Function name.
        name: Ident,
        /// Identifier arguments.
        args: Vec<Ident>,
        /// Expression span.
        span: Span,
    },
    /// Aggregate function call.
    Aggregate {
        /// Aggregate function.
        func: AggregateFunc,
        /// Whether the aggregate uses DISTINCT.
        distinct: bool,
        /// Aggregate argument.
        arg: AggregateArg,
        /// Function name as written.
        name: Ident,
        /// Expression span.
        span: Span,
    },
}

/// Supported aggregate functions.
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

/// Aggregate argument.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AggregateArg {
    /// `*`.
    All { span: Span },
    /// A variable.
    Var { var: Ident, span: Span },
    /// A variable property.
    Property {
        /// Variable name.
        var: Ident,
        /// Property name.
        property: Ident,
        /// Argument span.
        span: Span,
    },
}

/// Sort item.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SortItem {
    /// Sort key.
    pub(crate) key: SortKey,
    /// `true` for descending order.
    pub(crate) desc: bool,
    /// Item span.
    pub(crate) span: Span,
}

/// Sort key.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SortKey {
    /// Variable property reference.
    Property {
        /// Variable name.
        var: Ident,
        /// Property name.
        property: Ident,
        /// Key span.
        span: Span,
    },
    /// Return alias.
    Alias {
        /// Alias name.
        alias: Ident,
        /// Key span.
        span: Span,
    },
}

/// Identifier with source location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Ident {
    /// Exact source spelling.
    pub(crate) text: String,
    /// Identifier span.
    pub(crate) span: Span,
}
