//! SQL/PGQ adapter seam for lowering PostgreSQL-owned graph patterns.
//!
//! This module intentionally does not parse SQL/PGQ text. PostgreSQL owns SQL
//! parsing and SQL/PGQ graph-table semantics; pgGraph accepts a typed pattern
//! shape once a stable PostgreSQL hook or catalog mapping can supply one.

#![allow(dead_code)]

use crate::gql::ast::{
    self, AggregateArg, Direction, Ident, MatchClause, NodePat, Pattern, Query, RelPat,
    ReturnClause, ReturnExpr, ReturnItem, SortItem, SortKey, Statement, VarLen, WithClause,
};
use crate::gql::errors::{GqlError, Span};

use super::catalog_snapshot::CatalogSnapshot;
use super::logical_plan::LogicalStatement;

const ADAPTER_SPAN: Span = Span { start: 0, end: 0 };

/// One row in the SQL/PGQ compatibility matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SqlPgqCompatibilityRow {
    /// SQL/PGQ feature area.
    pub(crate) feature: &'static str,
    /// Current adapter status.
    pub(crate) status: &'static str,
    /// How the feature maps into pgGraph, or why it is rejected.
    pub(crate) notes: &'static str,
}

/// Compatibility matrix for the current typed SQL/PGQ adapter seam.
pub(crate) const COMPATIBILITY_MATRIX: &[SqlPgqCompatibilityRow] = &[
    SqlPgqCompatibilityRow {
        feature: "node pattern",
        status: "supported",
        notes: "typed adapter lowers a labeled node pattern into the shared node-scan IR",
    },
    SqlPgqCompatibilityRow {
        feature: "single relationship pattern",
        status: "supported",
        notes: "typed adapter lowers a single labeled relationship pattern into the shared read IR",
    },
    SqlPgqCompatibilityRow {
        feature: "optional relationship pattern",
        status: "supported",
        notes: "typed adapter maps optional patterns to the same null-extension plan used by GQL",
    },
    SqlPgqCompatibilityRow {
        feature: "projection and ordering",
        status: "supported",
        notes: "node variables, relationship variables, properties, path functions, DISTINCT, ORDER BY, SKIP, and LIMIT map through shared binding",
    },
    SqlPgqCompatibilityRow {
        feature: "aggregates",
        status: "supported",
        notes: "RETURN aggregates map to the shared aggregate IR when the typed hook supplies aggregate expressions",
    },
    SqlPgqCompatibilityRow {
        feature: "predicates",
        status: "deferred",
        notes: "typed predicate lowering is intentionally absent until PostgreSQL hook semantics are stable",
    },
    SqlPgqCompatibilityRow {
        feature: "GRAPH_TABLE SQL text",
        status: "not a pgGraph parser",
        notes: "PostgreSQL remains responsible for SQL parsing; pgGraph accepts only typed hook output",
    },
    SqlPgqCompatibilityRow {
        feature: "SQL/PGQ DDL",
        status: "rejected",
        notes: "CREATE PROPERTY GRAPH and related DDL remain PostgreSQL catalog features, not pgGraph DDL",
    },
    SqlPgqCompatibilityRow {
        feature: "multi-pattern joins",
        status: "deferred",
        notes: "requires the later multi-stage row-stream join planner before adapter exposure",
    },
];

/// Direction of a typed SQL/PGQ relationship pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqlPgqDirection {
    /// Source to target.
    Out,
    /// Target to source.
    In,
    /// Either direction.
    Undirected,
}

/// Typed SQL/PGQ node pattern supplied by PostgreSQL-owned parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqlPgqNodePattern {
    /// Variable name.
    pub(crate) var: String,
    /// Registered pgGraph label/table name.
    pub(crate) label: String,
}

/// Typed SQL/PGQ relationship pattern supplied by PostgreSQL-owned parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqlPgqRelationshipPattern {
    /// Optional relationship variable name.
    pub(crate) var: Option<String>,
    /// Registered pgGraph relationship type.
    pub(crate) rel_type: String,
    /// Relationship direction.
    pub(crate) direction: SqlPgqDirection,
    /// Optional bounded hop range.
    pub(crate) hops: Option<(u32, u32)>,
}

/// Return expression from a typed SQL/PGQ projection list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SqlPgqReturnExpr {
    /// Return a bound variable.
    Var(String),
    /// Return a node property.
    Property { var: String, property: String },
    /// Return one of the supported path functions over a relationship variable.
    PathFunction { name: String, arg: String },
    /// Return an aggregate expression.
    Aggregate {
        func: SqlPgqAggregateFunc,
        distinct: bool,
        arg: SqlPgqAggregateArg,
    },
}

/// Supported SQL/PGQ aggregate functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqlPgqAggregateFunc {
    /// `count`.
    Count,
    /// `sum`.
    Sum,
    /// `avg`.
    Avg,
    /// `min`.
    Min,
    /// `max`.
    Max,
    /// `collect`.
    Collect,
}

/// Aggregate argument from a typed SQL/PGQ projection list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SqlPgqAggregateArg {
    /// `*`.
    All,
    /// Bound variable.
    Var(String),
    /// Node property.
    Property { var: String, property: String },
}

/// One typed SQL/PGQ return item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqlPgqReturnItem {
    /// Return expression.
    pub(crate) expr: SqlPgqReturnExpr,
    /// Optional alias.
    pub(crate) alias: Option<String>,
}

/// Sort key from a typed SQL/PGQ query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SqlPgqSortKey {
    /// Sort by output alias.
    Alias(String),
    /// Sort by a node property.
    Property { var: String, property: String },
}

/// One typed SQL/PGQ sort item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqlPgqSortItem {
    /// Sort key.
    pub(crate) key: SqlPgqSortKey,
    /// Descending sort.
    pub(crate) desc: bool,
}

/// Typed read pattern produced by a future PostgreSQL SQL/PGQ hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqlPgqRead {
    /// Source node pattern.
    pub(crate) source: SqlPgqNodePattern,
    /// Optional single relationship and target node.
    pub(crate) relationship: Option<(SqlPgqRelationshipPattern, SqlPgqNodePattern)>,
    /// Whether the relationship pattern is optional.
    pub(crate) optional: bool,
    /// Final projection list.
    pub(crate) returns: Vec<SqlPgqReturnItem>,
    /// Whether final rows should be distinct.
    pub(crate) distinct: bool,
    /// Sort list.
    pub(crate) order_by: Vec<SqlPgqSortItem>,
    /// Rows to skip.
    pub(crate) skip: Option<u64>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u64>,
}

/// Lower a typed SQL/PGQ read pattern into the shared logical IR.
///
/// # Errors
///
/// Returns [`GqlError`] when the typed SQL/PGQ hook describes a feature outside
/// the current compatibility matrix or when normal catalog binding fails.
pub(crate) fn lower_read(
    read: &SqlPgqRead,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalStatement, GqlError> {
    let statement = read.to_gql_statement()?;
    super::semantics::bind_statement(&statement, catalog)
}

impl SqlPgqRead {
    fn to_gql_statement(&self) -> Result<Statement, GqlError> {
        if self.optional && self.relationship.is_none() {
            return Err(GqlError::unsupported(
                ADAPTER_SPAN,
                "SQL/PGQ node-only optional patterns require row-stream join planning",
            ));
        }
        if self.returns.is_empty() {
            return Err(GqlError::bind(
                ADAPTER_SPAN,
                "SQL/PGQ adapter requires at least one return item",
            ));
        }
        let start = node_pat(&self.source);
        let tail = self
            .relationship
            .as_ref()
            .map(|(rel, target)| Ok((rel_pat(rel)?, node_pat(target))))
            .transpose()?
            .into_iter()
            .collect();
        Ok(Statement::Read(Query {
            match_: MatchClause {
                optional: self.optional,
                pattern: Pattern {
                    path_var: None,
                    start,
                    tail,
                    span: ADAPTER_SPAN,
                },
                span: ADAPTER_SPAN,
            },
            where_: None,
            with_: Vec::<WithClause>::new(),
            return_: ReturnClause {
                distinct: self.distinct,
                items: self
                    .returns
                    .iter()
                    .map(return_item)
                    .collect::<Result<Vec<_>, _>>()?,
                span: ADAPTER_SPAN,
            },
            order_by: self.order_by.iter().map(sort_item).collect(),
            skip: self.skip,
            limit: self.limit,
            span: ADAPTER_SPAN,
        }))
    }
}

fn node_pat(node: &SqlPgqNodePattern) -> NodePat {
    NodePat {
        var: Some(ident(&node.var)),
        label: Some(ident(&node.label)),
        props: Vec::new(),
        span: ADAPTER_SPAN,
    }
}

fn rel_pat(rel: &SqlPgqRelationshipPattern) -> Result<RelPat, GqlError> {
    let var_len = rel
        .hops
        .map(|(min, max)| {
            if min == 0 || max < min {
                Err(GqlError::unsupported(
                    ADAPTER_SPAN,
                    "SQL/PGQ adapter supports bounded relationship ranges with min >= 1 and max >= min",
                ))
            } else {
                Ok(VarLen {
                    min,
                    max,
                    span: ADAPTER_SPAN,
                })
            }
        })
        .transpose()?;
    Ok(RelPat {
        var: rel.var.as_deref().map(ident),
        rel_type: Some(ident(&rel.rel_type)),
        direction: match rel.direction {
            SqlPgqDirection::Out => Direction::Out,
            SqlPgqDirection::In => Direction::In,
            SqlPgqDirection::Undirected => Direction::Undirected,
        },
        var_len,
        props: Vec::new(),
        span: ADAPTER_SPAN,
    })
}

fn return_item(item: &SqlPgqReturnItem) -> Result<ReturnItem, GqlError> {
    let expr = match &item.expr {
        SqlPgqReturnExpr::Var(var) => ReturnExpr::Var {
            var: ident(var),
            span: ADAPTER_SPAN,
        },
        SqlPgqReturnExpr::Property { var, property } => ReturnExpr::Property {
            var: ident(var),
            property: ident(property),
            span: ADAPTER_SPAN,
        },
        SqlPgqReturnExpr::PathFunction { name, arg } => ReturnExpr::Func {
            name: ident(name),
            args: vec![ident(arg)],
            span: ADAPTER_SPAN,
        },
        SqlPgqReturnExpr::Aggregate {
            func,
            distinct,
            arg,
        } => ReturnExpr::Aggregate {
            func: aggregate_func(*func),
            distinct: *distinct,
            arg: aggregate_arg(arg),
            name: ident(aggregate_name(*func)),
            span: ADAPTER_SPAN,
        },
    };
    Ok(ReturnItem {
        expr,
        alias: item.alias.as_deref().map(ident),
        span: ADAPTER_SPAN,
    })
}

fn aggregate_func(func: SqlPgqAggregateFunc) -> ast::AggregateFunc {
    match func {
        SqlPgqAggregateFunc::Count => ast::AggregateFunc::Count,
        SqlPgqAggregateFunc::Sum => ast::AggregateFunc::Sum,
        SqlPgqAggregateFunc::Avg => ast::AggregateFunc::Avg,
        SqlPgqAggregateFunc::Min => ast::AggregateFunc::Min,
        SqlPgqAggregateFunc::Max => ast::AggregateFunc::Max,
        SqlPgqAggregateFunc::Collect => ast::AggregateFunc::Collect,
    }
}

fn aggregate_name(func: SqlPgqAggregateFunc) -> &'static str {
    match func {
        SqlPgqAggregateFunc::Count => "count",
        SqlPgqAggregateFunc::Sum => "sum",
        SqlPgqAggregateFunc::Avg => "avg",
        SqlPgqAggregateFunc::Min => "min",
        SqlPgqAggregateFunc::Max => "max",
        SqlPgqAggregateFunc::Collect => "collect",
    }
}

fn aggregate_arg(arg: &SqlPgqAggregateArg) -> AggregateArg {
    match arg {
        SqlPgqAggregateArg::All => AggregateArg::All { span: ADAPTER_SPAN },
        SqlPgqAggregateArg::Var(var) => AggregateArg::Var {
            var: ident(var),
            span: ADAPTER_SPAN,
        },
        SqlPgqAggregateArg::Property { var, property } => AggregateArg::Property {
            var: ident(var),
            property: ident(property),
            span: ADAPTER_SPAN,
        },
    }
}

fn sort_item(item: &SqlPgqSortItem) -> SortItem {
    SortItem {
        key: match &item.key {
            SqlPgqSortKey::Alias(alias) => SortKey::Alias {
                alias: ident(alias),
                span: ADAPTER_SPAN,
            },
            SqlPgqSortKey::Property { var, property } => SortKey::Property {
                var: ident(var),
                property: ident(property),
                span: ADAPTER_SPAN,
            },
        },
        desc: item.desc,
        span: ADAPTER_SPAN,
    }
}

fn ident(text: &str) -> Ident {
    Ident {
        text: text.to_string(),
        span: ADAPTER_SPAN,
    }
}
