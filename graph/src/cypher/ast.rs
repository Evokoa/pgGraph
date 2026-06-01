//! AST boundary for openCypher compatibility input.

use crate::gql::errors::Span;

/// One row in the openCypher compatibility matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CypherCompatibilityRow {
    /// Feature or syntax family.
    pub(crate) feature: &'static str,
    /// Current compatibility status.
    pub(crate) status: &'static str,
    /// Mapping or rejection note.
    pub(crate) notes: &'static str,
}

/// Compatibility matrix for `graph.cypher()`.
pub(crate) const COMPATIBILITY_MATRIX: &[CypherCompatibilityRow] = &[
    CypherCompatibilityRow {
        feature: "node MATCH",
        status: "supported",
        notes: "openCypher node patterns that overlap pgGraph's GQL subset lower through the shared GQL IR",
    },
    CypherCompatibilityRow {
        feature: "single relationship MATCH",
        status: "supported",
        notes: "directed, inbound, undirected, optional, and bounded relationship patterns share GQL planning",
    },
    CypherCompatibilityRow {
        feature: "RETURN, WITH, ORDER BY, SKIP, LIMIT",
        status: "supported",
        notes: "projection, ordering, pagination, DISTINCT, aggregates, and path functions share GQL planning",
    },
    CypherCompatibilityRow {
        feature: "mapped writes",
        status: "supported",
        notes: "CREATE, SET, REMOVE, DELETE, DETACH DELETE, and MERGE are supported only for PostgreSQL-registered mappings",
    },
    CypherCompatibilityRow {
        feature: "Cypher procedures and UNWIND",
        status: "rejected",
        notes: "CALL, YIELD, UNWIND, FOREACH, and procedure APIs do not map to pgGraph's PostgreSQL-first model",
    },
    CypherCompatibilityRow {
        feature: "Cypher DDL",
        status: "rejected",
        notes: "index, constraint, database, and schema DDL remain PostgreSQL responsibilities",
    },
    CypherCompatibilityRow {
        feature: "Full openCypher compatibility",
        status: "not claimed",
        notes: "graph.cypher() is a narrow compatibility surface, not a full openCypher-compatible database API",
    },
];

/// Parsed openCypher compatibility statement.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CypherStatement {
    /// Statement that maps into the shared GQL AST and IR.
    Compatible {
        /// Parsed overlapping GQL statement.
        statement: Box<crate::gql::ast::Statement>,
        /// Full statement span.
        span: Span,
    },
    /// Syntactically recognized openCypher feature outside pgGraph's matrix.
    Unsupported {
        /// Feature family name.
        feature: String,
        /// Span of the rejected syntax.
        span: Span,
    },
}

impl CypherStatement {
    /// Return the full source span for this statement.
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Compatible { span, .. } | Self::Unsupported { span, .. } => *span,
        }
    }
}
