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
    /// `MATCH ... REMOVE ... RETURN` property/label write query.
    Remove(RemoveQuery),
    /// `MATCH ... DELETE ... RETURN` edge write query.
    Delete(DeleteQuery),
    /// `MATCH ... DETACH DELETE ... RETURN` node cascade delete query.
    DetachDelete(DetachDeleteQuery),
    /// `MERGE ... RETURN` node upsert query.
    Merge(MergeQuery),
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

/// Parsed mapped property/label removal query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RemoveQuery {
    /// Required `MATCH` clause selecting the updated node.
    pub(crate) match_: MatchClause,
    /// Optional `WHERE` clause.
    pub(crate) where_: Option<Expr>,
    /// Property or label removal clause.
    pub(crate) remove: RemoveClause,
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

/// Parsed mapped node detach-delete query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DetachDeleteQuery {
    /// Required `MATCH` clause selecting the node.
    pub(crate) match_: MatchClause,
    /// Optional `WHERE` clause.
    pub(crate) where_: Option<Expr>,
    /// Node detach-delete clause.
    pub(crate) delete: DetachDeleteClause,
    /// Required `RETURN` clause.
    pub(crate) return_: ReturnClause,
    /// Full query span.
    pub(crate) span: Span,
}

/// Parsed mapped node merge query.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MergeQuery {
    /// Node merge clause.
    pub(crate) merge: MergeClause,
    /// Optional property assignment applied only when the row is inserted.
    pub(crate) on_create: Option<SetClause>,
    /// Optional property assignment applied only when an existing row matches.
    pub(crate) on_match: Option<SetClause>,
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

/// `MERGE` clause for one mapped node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MergeClause {
    /// Node identity/properties to merge.
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

/// `REMOVE` clause for one node property or label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RemoveClause {
    /// Removed property or label target.
    pub(crate) target: RemoveTarget,
    /// Clause span.
    pub(crate) span: Span,
}

/// Target of a `REMOVE` clause.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RemoveTarget {
    /// Remove a mapped source-table property.
    Property(PropertyRef),
    /// Remove a node label.
    Label {
        /// Node variable name.
        var: Ident,
        /// Label name.
        label: Ident,
        /// Full target span.
        span: Span,
    },
}

/// `DELETE` clause for one relationship variable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeleteClause {
    /// Relationship variable to delete.
    pub(crate) var: Ident,
    /// Clause span.
    pub(crate) span: Span,
}

/// `DETACH DELETE` clause for one node variable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DetachDeleteClause {
    /// Node variable to delete after incident edges are removed.
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
    /// All graph patterns in the clause, in source order.
    pub(crate) patterns: Vec<Pattern>,
    /// Clause span.
    pub(crate) span: Span,
}

/// Single linear path pattern.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pattern {
    /// Optional path variable bound to the whole pattern.
    pub(crate) path_var: Option<Ident>,
    /// First node pattern.
    pub(crate) start: NodePat,
    /// Relationship/node pairs after the start node.
    pub(crate) tail: Vec<(RelPat, NodePat)>,
    /// Pattern span.
    pub(crate) span: Span,
}

impl Pattern {
    /// Return the path variable text.
    #[cfg(test)]
    pub(crate) fn path_var_text(&self) -> Option<&str> {
        self.path_var.as_ref().map(|ident| ident.text.as_str())
    }
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
    /// Relationship type selector alternatives.
    pub(crate) rel_types: Vec<Ident>,
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
        let [rel_type] = self.rel_types.as_slice() else {
            return None;
        };
        Some(rel_type.text.as_str())
    }

    /// Return relationship type selector texts.
    #[cfg(test)]
    pub(crate) fn rel_type_texts(&self) -> Vec<&str> {
        self.rel_types
            .iter()
            .map(|ident| ident.text.as_str())
            .collect()
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
    /// `id(variable)` graph identity function.
    Identity {
        /// Referenced variable name.
        var: Ident,
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
