//! pgrx-free frontend for the supported GQL subset.
//!
//! This module owns lexical analysis, syntax trees, and parsing for graph query
//! text. It deliberately does not touch PostgreSQL state; catalog binding,
//! planning, and execution live in later query layers.

pub(crate) mod ast;
pub(crate) mod errors;
pub(crate) mod lexer;
pub(crate) mod parser;

#[cfg(test)]
pub(crate) use parser::parse;
pub(crate) use parser::parse_statement;

#[cfg(test)]
mod tests {
    use super::ast::{
        AggregateArg, AggregateFunc, CmpOp, Direction, Expr, Literal, LiteralValue, Operand,
        ReturnExpr, SortKey, Statement,
    };
    use super::errors::GqlErrorKind;
    use super::parse;

    #[test]
    fn parses_directed_match_with_where_return_order_skip_limit() {
        let query = "MATCH (u:users {id: $id})-[r:follows]->(v:users) \
                     WHERE v.age >= 21 AND NOT v.deleted IS NOT NULL \
                     RETURN u, v.name AS name ORDER BY name DESC SKIP 2 LIMIT 10";

        let parsed = parse(query).expect("query should parse");

        assert_eq!(parsed.match_.pattern.start.var_text(), Some("u"));
        assert_eq!(parsed.match_.pattern.start.label_text(), Some("users"));
        assert_eq!(parsed.match_.pattern.tail.len(), 1);
        let (rel, dst) = &parsed.match_.pattern.tail[0];
        assert_eq!(rel.var_text(), Some("r"));
        assert_eq!(rel.rel_type_text(), Some("follows"));
        assert_eq!(rel.direction, Direction::Out);
        assert_eq!(dst.var_text(), Some("v"));
        assert!(parsed.where_.is_some());
        assert_eq!(parsed.return_.items.len(), 2);
        assert!(matches!(
            &parsed.return_.items[0].expr,
            ReturnExpr::Var { .. }
        ));
        assert!(matches!(
            &parsed.return_.items[1].expr,
            ReturnExpr::Property { .. }
        ));
        assert_eq!(parsed.order_by.len(), 1);
        assert!(parsed.order_by[0].desc);
        assert!(matches!(&parsed.order_by[0].key, SortKey::Alias { .. }));
        assert_eq!(parsed.skip, Some(2));
        assert_eq!(parsed.limit, Some(10));
    }

    #[test]
    fn parses_undirected_bounded_var_length_relationship() {
        let parsed = parse("MATCH (a)-[:knows*1..3]-(b) RETURN a, b").expect("query should parse");
        let (rel, _) = &parsed.match_.pattern.tail[0];

        assert_eq!(rel.direction, Direction::Undirected);
        let var_len = rel.var_len.expect("relationship should be variable length");
        assert_eq!(var_len.min, 1);
        assert_eq!(var_len.max, 3);
    }

    #[test]
    fn parses_inbound_relationship() {
        let parsed = parse("MATCH (a)<-[:knows]-(b) RETURN a").expect("query should parse");
        let (rel, _) = &parsed.match_.pattern.tail[0];

        assert_eq!(rel.direction, Direction::In);
    }

    #[test]
    fn parses_optional_match_clause() {
        let parsed = parse("OPTIONAL MATCH (a:users)-[:knows]->(b:users) RETURN a, b")
            .expect("query should parse");

        assert!(parsed.match_.optional);
        assert_eq!(parsed.match_.pattern.tail.len(), 1);
    }

    #[test]
    fn parses_property_predicates_and_literal_lists() {
        let parsed = parse(
            "MATCH (u:users) WHERE u.status IN ['active', 'pending'] OR u.age IS NULL RETURN u",
        )
        .expect("query should parse");

        let Some(where_) = parsed.where_ else {
            panic!("WHERE clause should be present");
        };
        let Expr::Or { lhs, rhs, .. } = where_ else {
            panic!("top-level predicate should be OR");
        };
        assert!(matches!(*lhs, Expr::Compare { op: CmpOp::In, .. }));
        assert!(matches!(
            *rhs,
            Expr::Compare {
                op: CmpOp::IsNull,
                ..
            }
        ));
    }

    #[test]
    fn parses_aggregate_return_functions() {
        let parsed = parse(
            "MATCH (u:users) RETURN count(*) AS total, sum(u.age) AS total_age, collect(DISTINCT u.name) AS names",
        )
        .expect("query should parse");
        let item = &parsed.return_.items[0];

        assert!(matches!(
            &item.expr,
            ReturnExpr::Aggregate {
                func: AggregateFunc::Count,
                arg: AggregateArg::All { .. },
                ..
            }
        ));
        assert_eq!(item.alias_text(), Some("total"));
        assert!(matches!(
            &parsed.return_.items[1].expr,
            ReturnExpr::Aggregate {
                func: AggregateFunc::Sum,
                arg: AggregateArg::Property { .. },
                ..
            }
        ));
        assert!(matches!(
            &parsed.return_.items[2].expr,
            ReturnExpr::Aggregate {
                func: AggregateFunc::Collect,
                distinct: true,
                ..
            }
        ));
    }

    #[test]
    fn parses_with_projection_stage() {
        let parsed = parse("MATCH (u:users) WITH u.name AS name RETURN name ORDER BY name")
            .expect("query should parse");

        assert_eq!(parsed.with_.len(), 1);
        assert_eq!(parsed.with_[0].items.len(), 1);
        assert_eq!(parsed.with_[0].items[0].alias_text(), Some("name"));
        assert_eq!(parsed.return_.items.len(), 1);
        assert_eq!(parsed.order_by.len(), 1);
    }

    #[test]
    fn parses_create_node_statement() {
        let parsed = super::parse_statement("CREATE (u:users {id: 'u3', name: $name}) RETURN u")
            .expect("statement should parse");
        let Statement::Create(create) = parsed else {
            panic!("statement should be a create query");
        };

        assert_eq!(create.create.node.var_text(), Some("u"));
        assert_eq!(create.create.node.label_text(), Some("users"));
        assert_eq!(create.create.node.props.len(), 2);
        assert_eq!(create.return_.items.len(), 1);
    }

    #[test]
    fn parses_set_property_statement() {
        let parsed = super::parse_statement(
            "MATCH (u:users {id: 'u1'}) SET u.name = $name RETURN u.name AS name",
        )
        .expect("statement should parse");
        let Statement::Set(set) = parsed else {
            panic!("statement should be a set query");
        };

        assert_eq!(set.match_.pattern.start.var_text(), Some("u"));
        assert_eq!(set.set.target.var.text, "u");
        assert_eq!(set.set.target.property.text, "name");
        assert_eq!(set.return_.items.len(), 1);
    }

    #[test]
    fn parses_delete_edge_statement() {
        let parsed =
            super::parse_statement("MATCH (u:users)-[r:friend]->(v:users) DELETE r RETURN u, v")
                .expect("statement should parse");
        let Statement::Delete(delete) = parsed else {
            panic!("statement should be a delete query");
        };

        assert_eq!(delete.match_.pattern.start.var_text(), Some("u"));
        let (rel, target) = &delete.match_.pattern.tail[0];
        assert_eq!(rel.var_text(), Some("r"));
        assert_eq!(rel.rel_type_text(), Some("friend"));
        assert_eq!(target.var_text(), Some("v"));
        assert_eq!(delete.delete.var.text, "r");
        assert_eq!(delete.return_.items.len(), 2);
    }

    #[test]
    fn rejects_unbounded_variable_length_relationship() {
        let err = parse("MATCH (a)-[:knows*]-(b) RETURN a").expect_err("query should be rejected");

        assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
        assert!(err.to_string().contains("upper bound"));
    }

    #[test]
    fn rejects_excessive_prefix_not_without_recursing() {
        let mut query = "MATCH (u:users) WHERE ".to_string();
        for _ in 0..513 {
            query.push_str("NOT ");
        }
        query.push_str("u.active = true RETURN u");

        let err = parse(&query).expect_err("query should reject excessive NOT depth");

        assert!(matches!(err.kind, GqlErrorKind::Syntax { .. }));
        assert!(err.to_string().contains("too many nested NOT"));
    }

    #[test]
    fn rejects_missing_return_clause() {
        let err = parse("MATCH (a)").expect_err("query should require RETURN");

        assert!(matches!(err.kind, GqlErrorKind::Syntax { .. }));
    }

    #[test]
    fn spans_are_byte_offsets() {
        let parsed = parse("MATCH (ユーザー:users) RETURN ユーザー").expect("query should parse");

        let ident = parsed.match_.pattern.start.var.as_ref().expect("var");
        assert_eq!(
            &"MATCH (ユーザー:users) RETURN ユーザー"[ident.span.range()],
            "ユーザー"
        );
    }

    #[test]
    fn keeps_inline_property_operands_typed() {
        let parsed = parse("MATCH (u:users {active: true, score: 3.5, name: 'Ada'}) RETURN u")
            .expect("query should parse");

        let props = &parsed.match_.pattern.start.props;
        assert!(matches!(
            &props[0].1,
            Operand::Literal(Literal::Value {
                value: LiteralValue::Bool(true),
                ..
            })
        ));
        assert!(matches!(
            &props[1].1,
            Operand::Literal(Literal::Value {
                value: LiteralValue::Float(_),
                ..
            })
        ));
        assert!(matches!(
            &props[2].1,
            Operand::Literal(Literal::Value {
                value: LiteralValue::Str(_),
                ..
            })
        ));
    }
}
