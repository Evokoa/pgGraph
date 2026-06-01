//! openCypher compatibility frontend.
//!
//! This module owns the optional `graph.cypher()` parsing boundary. It accepts
//! only the openCypher syntax that maps cleanly into pgGraph's PostgreSQL-first
//! GQL IR and returns explicit unsupported-feature diagnostics for constructs
//! that would imply full openCypher compatibility.

pub(crate) mod ast;
pub(crate) mod lexer;
pub(crate) mod lower;
pub(crate) mod parser;
pub(crate) mod semantics;

pub(crate) use parser::parse_statement;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::errors::GqlErrorKind;
    use crate::query::catalog_snapshot::FakeCatalog;

    fn fake_catalog() -> FakeCatalog {
        FakeCatalog::new()
            .with_writable_label("users", 10, ["id", "name", "age"], ["name", "age"])
            .with_edge("friend", 10, 10)
    }

    #[test]
    fn parses_compatible_cypher_match() {
        let parsed =
            parse_statement("MATCH (u:users)-[r:friend]->(v:users) RETURN u.name AS name, v")
                .expect("query should parse");

        assert!(matches!(parsed, ast::CypherStatement::Compatible { .. }));
    }

    #[test]
    fn binds_compatible_cypher_to_same_logical_ir_as_gql() {
        let catalog = fake_catalog();
        let cypher = parse_statement("MATCH (u:users)-[r:friend]->(v:users) RETURN u, v")
            .expect("cypher should parse");
        let gql = crate::gql::parse_statement("MATCH (u:users)-[r:friend]->(v:users) RETURN u, v")
            .expect("gql should parse");

        let cypher_logical = semantics::bind_statement(&cypher, &catalog).unwrap();
        let gql_logical = crate::query::semantics::bind_statement(&gql, &catalog).unwrap();

        assert_eq!(cypher_logical, gql_logical);
    }

    #[test]
    fn rejects_unmappable_open_cypher_features_during_binding() {
        let cases = [
            ("CALL db.labels() YIELD label RETURN label", "CALL/YIELD"),
            ("UNWIND [1, 2] AS n RETURN n", "UNWIND"),
            ("FOREACH (x IN [1,2] | CREATE (:users {id: x}))", "FOREACH"),
            (
                "LOAD CSV FROM 'file:///users.csv' AS row RETURN row",
                "LOAD CSV",
            ),
            ("START n=node(1) RETURN n", "START"),
            (
                "MATCH (u:users) RETURN u UNION MATCH (v:users) RETURN v",
                "UNION",
            ),
            (
                "CREATE INDEX users_name IF NOT EXISTS FOR (n:users) ON (n.name)",
                "Cypher DDL",
            ),
        ];

        for (query, feature) in cases {
            let parsed = parse_statement(query).expect("query should parse as unsupported");
            let err = semantics::bind_statement(&parsed, &fake_catalog()).unwrap_err();

            assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
            assert!(
                err.to_string().contains(feature),
                "expected `{}` to mention `{}`",
                err,
                feature
            );
        }
    }

    #[test]
    fn unsupported_keyword_scan_does_not_reject_property_names() {
        let cases = [
            "MATCH (u:users) RETURN u.unwind AS unwind",
            "MATCH (u:index) RETURN u.index AS index",
            "MATCH (call:users) RETURN call.name AS union",
        ];

        for query in cases {
            let parsed = parse_statement(query).expect("query should parse");

            assert!(matches!(parsed, ast::CypherStatement::Compatible { .. }));
        }
    }

    #[test]
    fn parser_totality_corpus_returns_results_without_panicking() {
        let corpus = [
            "",
            "MATCH",
            "MATCH (n)",
            "MATCH (n:users) RETURN n",
            "OPTIONAL MATCH (n:users)-[:friend]->(m:users) RETURN n, m",
            "CREATE (n:users {id: 'u3', name: $name}) RETURN n",
            "MERGE (n:users {id: $id}) ON MATCH SET n.name = $name RETURN n",
            "CALL db.labels() YIELD label RETURN label",
            "MATCH (n) WITH n RETURN n",
            "MATCH (n) RETURN shortestPath((n)-[:friend]->(:users))",
            "CREATE INDEX users_name IF NOT EXISTS FOR (n:users) ON (n.name)",
            "FOREACH (x IN [1,2] | CREATE (:users {id: x}))",
        ];

        for query in corpus {
            let _ = parse_statement(query);
        }
    }

    #[test]
    fn compatibility_matrix_does_not_claim_full_opencypher_parity() {
        assert!(ast::COMPATIBILITY_MATRIX.iter().any(|row| row.feature
            == "Full openCypher compatibility"
            && row.status == "not claimed"));
    }
}
