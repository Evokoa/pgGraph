use super::catalog_snapshot::{FakeCatalog, MappedEdgeSpec};
use super::execute::{execute, execute_node_scan, GqlNodeCoordinate, GqlNodeRow};
use super::explain::explain;
use super::lower::{lower, lower_statement};
use super::physical_plan::ReturnSlot;
use super::semantics::bind;
use super::semantics::bind_statement;
use super::sqlpgq_adapter::{
    lower_read as lower_sqlpgq_read, SqlPgqAggregateArg, SqlPgqAggregateFunc, SqlPgqDirection,
    SqlPgqNodePattern, SqlPgqRead, SqlPgqRelationshipPattern, SqlPgqReturnExpr, SqlPgqReturnItem,
    SqlPgqSortItem, SqlPgqSortKey, COMPATIBILITY_MATRIX,
};
use super::value::{project_node_rows, project_rows, HydratedRows, QueryParams};
use crate::edge_store::{EdgeStore, RawEdge};
use crate::engine::Engine;
use crate::gql::errors::GqlErrorKind;
use crate::gql::parse;
use crate::safety::GraphError;
use std::collections::HashMap;

fn fake_catalog() -> FakeCatalog {
    FakeCatalog::new()
        .with_writable_label(
            "users",
            10,
            [
                "id",
                "name",
                "age",
                "profile",
                "profile.plan",
                "profile.tags",
                "profile.flags",
                "profile.missing",
                "profile.explicit_null",
            ],
            ["name", "age", "profile", "profile.plan"],
        )
        .with_writable_label("companies", 20, ["id", "name"], ["name"])
        .with_edge("works_at", 10, 20)
        .with_mapped_edge(MappedEdgeSpec {
            rel_type: "friend",
            from_table_oid: 10,
            to_table_oid: 10,
            edge_table_oid: 30,
            source_column: "user_id",
            target_column: "friend_id",
            bidirectional: false,
        })
}

fn bind_query(query: &str) -> super::logical_plan::LogicalPlan {
    let ast = parse(query).unwrap();
    bind(&ast, &fake_catalog()).unwrap()
}

#[test]
fn binder_accepts_create_node_for_registered_label() {
    let ast =
        crate::gql::parse_statement("CREATE (u:users {id: 'u3', name: $name}) RETURN u").unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::CreateNode(create) = plan else {
        panic!("expected create node plan");
    };

    assert_eq!(create.node.var, "u");
    assert_eq!(create.node.table_oid, 10);
    assert_eq!(create.properties.len(), 2);
    assert_eq!(create.returns.len(), 1);
}

#[test]
fn binder_accepts_set_property_for_writable_column() {
    let ast =
        crate::gql::parse_statement("MATCH (u:users {id: 'u1'}) SET u.name = $name RETURN u.name")
            .unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::SetProperty(set) = plan else {
        panic!("expected set property plan");
    };

    assert_eq!(set.node.var, "u");
    assert_eq!(set.node.table_oid, 10);
    assert_eq!(set.property, "name");
    assert!(set.predicate.is_some());
    assert_eq!(set.returns.len(), 1);
}

#[test]
fn binder_accepts_remove_property_for_writable_column() {
    let ast = crate::gql::parse_statement("MATCH (u:users {id: 'u1'}) REMOVE u.age RETURN u.age")
        .unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::RemoveProperty(remove) = plan else {
        panic!("expected remove property plan");
    };

    assert_eq!(remove.node.var, "u");
    assert_eq!(remove.node.table_oid, 10);
    assert_eq!(remove.property, "age");
    assert!(remove.predicate.is_some());
    assert_eq!(remove.returns.len(), 1);
}

#[test]
fn binder_accepts_remove_jsonb_property_path() {
    let ast = crate::gql::parse_statement(
        "MATCH (u:users {id: 'u1'}) REMOVE u.profile.plan RETURN u.profile.plan AS plan",
    )
    .unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::RemoveProperty(remove) = plan else {
        panic!("expected remove property plan");
    };

    assert_eq!(remove.property, "profile.plan");
    assert_eq!(remove.returns[0].name(), "plan");
}

#[test]
fn binder_rejects_remove_label_for_source_table_labels() {
    let ast =
        crate::gql::parse_statement("MATCH (u:users {id: 'u1'}) REMOVE u:users RETURN u").unwrap();
    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
    assert!(err
        .to_string()
        .contains("labels map to registered source tables"));
}

#[test]
fn binder_rejects_set_property_for_non_writable_column() {
    let ast =
        crate::gql::parse_statement("MATCH (u:users {id: 'u1'}) SET u.id = 'u2' RETURN u").unwrap();
    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Bind { .. }));
    assert!(err.to_string().contains("not a writable mapped column"));
}

#[test]
fn binder_rejects_set_property_for_tenant_column() {
    let catalog = FakeCatalog::new().with_writable_label(
        "tenant_users",
        30,
        ["id", "tenant_id", "name"],
        ["name"],
    );
    let ast = crate::gql::parse_statement(
        "MATCH (u:tenant_users {id: 'u1'}) SET u.tenant_id = 'tenant-b' RETURN u",
    )
    .unwrap();
    let err = bind_statement(&ast, &catalog).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Bind { .. }));
    assert!(err.to_string().contains("not a writable mapped column"));
}

#[test]
fn binder_rejects_jsonb_property_path_returns_for_writes() {
    let create = crate::gql::parse_statement(
        "CREATE (u:users {id: 'u3', name: 'Cara'}) RETURN u.profile.plan",
    )
    .unwrap();
    let create_err = bind_statement(&create, &fake_catalog()).unwrap_err();

    assert!(matches!(create_err.kind, GqlErrorKind::Unsupported { .. }));

    let set = crate::gql::parse_statement(
        "MATCH (u:users {id: 'u1'}) SET u.name = 'Ada' RETURN u.profile.plan",
    )
    .unwrap();
    let set_err = bind_statement(&set, &fake_catalog()).unwrap_err();

    assert!(matches!(set_err.kind, GqlErrorKind::Unsupported { .. }));
}

#[test]
fn binder_accepts_delete_for_mapped_edge_row() {
    let ast =
        crate::gql::parse_statement("MATCH (u:users)-[r:friend]->(v:users) DELETE r RETURN u, v")
            .unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::DeleteEdge(delete) = plan else {
        panic!("expected delete edge plan");
    };

    assert_eq!(delete.source.var, "u");
    assert_eq!(delete.target.var, "v");
    assert_eq!(delete.rel_var, "r");
    assert_eq!(delete.edge.edge_table_oid, 30);
    assert_eq!(delete.edge.source_column, "user_id");
    assert_eq!(delete.edge.target_column, "friend_id");
    assert_eq!(delete.returns.len(), 2);
}

#[test]
fn binder_accepts_detach_delete_for_node_with_mapped_incident_edges() {
    let catalog = FakeCatalog::new()
        .with_writable_label("users", 10, ["id", "name"], ["name"])
        .with_mapped_edge(MappedEdgeSpec {
            rel_type: "friend",
            from_table_oid: 10,
            to_table_oid: 10,
            edge_table_oid: 30,
            source_column: "user_id",
            target_column: "friend_id",
            bidirectional: false,
        });
    let ast =
        crate::gql::parse_statement("MATCH (u:users {id: 'u1'}) DETACH DELETE u RETURN u.name")
            .unwrap();
    let plan = bind_statement(&ast, &catalog).unwrap();
    let super::logical_plan::LogicalStatement::DetachDeleteNode(delete) = plan else {
        panic!("expected detach delete node plan");
    };

    assert_eq!(delete.node.var, "u");
    assert_eq!(delete.node.table_oid, 10);
    assert!(delete.predicate.is_some());
    assert_eq!(delete.incident_edges.len(), 1);
    assert_eq!(delete.incident_edges[0].rel_type, "friend");
    assert_eq!(delete.incident_edges[0].edge.edge_table_oid, 30);
    assert_eq!(delete.returns.len(), 1);
}

#[test]
fn binder_accepts_merge_node_with_create_and_match_branches() {
    let ast = crate::gql::parse_statement(
        "MERGE (u:users {id: $id, name: $name}) ON CREATE SET u.age = 1 ON MATCH SET u.name = $name RETURN u.name",
    )
    .unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::MergeNode(merge) = plan else {
        panic!("expected merge node plan");
    };

    assert_eq!(merge.node.var, "u");
    assert_eq!(merge.node.table_oid, 10);
    assert_eq!(merge.properties.len(), 2);
    assert_eq!(
        merge
            .on_create
            .as_ref()
            .map(|property| property.property.as_str()),
        Some("age")
    );
    assert_eq!(
        merge
            .on_match
            .as_ref()
            .map(|property| property.property.as_str()),
        Some("name")
    );
    assert_eq!(merge.returns.len(), 1);
}

#[test]
fn binder_rejects_merge_branch_for_non_writable_column() {
    let ast =
        crate::gql::parse_statement("MERGE (u:users {id: $id}) ON MATCH SET u.id = 'u2' RETURN u")
            .unwrap();
    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Bind { .. }));
    assert!(err.to_string().contains("not a writable mapped column"));
}

#[test]
fn binder_rejects_detach_delete_unknown_variable() {
    let ast =
        crate::gql::parse_statement("MATCH (u:users {id: 'u1'}) DETACH DELETE v RETURN u").unwrap();
    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Bind { .. }));
    assert!(err.to_string().contains("unknown DETACH DELETE variable"));
}

#[test]
fn sqlpgq_adapter_lowers_node_pattern_into_shared_ir() {
    let read = SqlPgqRead {
        source: SqlPgqNodePattern {
            var: "u".to_string(),
            label: "users".to_string(),
        },
        relationship: None,
        optional: false,
        returns: vec![
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Property {
                    var: "u".to_string(),
                    property: "name".to_string(),
                },
                alias: Some("name".to_string()),
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Aggregate {
                    func: SqlPgqAggregateFunc::Count,
                    distinct: false,
                    arg: SqlPgqAggregateArg::All,
                },
                alias: Some("total".to_string()),
            },
        ],
        distinct: false,
        order_by: vec![SqlPgqSortItem {
            key: SqlPgqSortKey::Alias("name".to_string()),
            desc: false,
        }],
        skip: None,
        limit: Some(10),
    };

    let logical = lower_sqlpgq_read(&read, &fake_catalog()).unwrap();

    let super::logical_plan::LogicalStatement::NodeScan(scan) = logical else {
        panic!("expected SQL/PGQ node pattern to lower into node scan");
    };
    assert_eq!(scan.node.var, "u");
    assert_eq!(scan.node.label, "users");
    assert_eq!(scan.returns.len(), 2);
    assert_eq!(scan.limit, Some(10));
}

#[test]
fn sqlpgq_adapter_lowers_relationship_pattern_into_shared_ir() {
    let read = SqlPgqRead {
        source: SqlPgqNodePattern {
            var: "u".to_string(),
            label: "users".to_string(),
        },
        relationship: Some((
            SqlPgqRelationshipPattern {
                var: Some("r".to_string()),
                rel_type: "works_at".to_string(),
                direction: SqlPgqDirection::Out,
                hops: Some((1, 2)),
            },
            SqlPgqNodePattern {
                var: "c".to_string(),
                label: "companies".to_string(),
            },
        )),
        optional: true,
        returns: vec![
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Var("u".to_string()),
                alias: None,
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Property {
                    var: "u".to_string(),
                    property: "name".to_string(),
                },
                alias: Some("name".to_string()),
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::PathFunction {
                    name: "length".to_string(),
                    arg: "r".to_string(),
                },
                alias: Some("hops".to_string()),
            },
        ],
        distinct: true,
        order_by: vec![SqlPgqSortItem {
            key: SqlPgqSortKey::Alias("name".to_string()),
            desc: true,
        }],
        skip: Some(1),
        limit: Some(5),
    };

    let logical = lower_sqlpgq_read(&read, &fake_catalog()).unwrap();

    let super::logical_plan::LogicalStatement::Read(plan) = logical else {
        panic!("expected SQL/PGQ relationship pattern to lower into read plan");
    };
    assert!(plan.optional);
    assert!(plan.distinct);
    assert_eq!(plan.relationship.var.as_deref(), Some("r"));
    assert_eq!(plan.relationship.hops.max, 2);
    assert_eq!(plan.order_by.len(), 1);
}

#[test]
fn sqlpgq_adapter_lowers_direction_and_property_sort_variants() {
    for (source_label, target_label, direction, expected) in [
        (
            "companies",
            "users",
            SqlPgqDirection::In,
            super::logical_plan::BoundDirection::In,
        ),
        (
            "users",
            "companies",
            SqlPgqDirection::Undirected,
            super::logical_plan::BoundDirection::Undirected,
        ),
    ] {
        let read = SqlPgqRead {
            source: SqlPgqNodePattern {
                var: "u".to_string(),
                label: source_label.to_string(),
            },
            relationship: Some((
                SqlPgqRelationshipPattern {
                    var: Some("r".to_string()),
                    rel_type: "works_at".to_string(),
                    direction,
                    hops: None,
                },
                SqlPgqNodePattern {
                    var: "c".to_string(),
                    label: target_label.to_string(),
                },
            )),
            optional: false,
            returns: vec![SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Property {
                    var: "c".to_string(),
                    property: "name".to_string(),
                },
                alias: Some("company".to_string()),
            }],
            distinct: false,
            order_by: vec![SqlPgqSortItem {
                key: SqlPgqSortKey::Property {
                    var: "u".to_string(),
                    property: "name".to_string(),
                },
                desc: false,
            }],
            skip: None,
            limit: None,
        };

        let logical = lower_sqlpgq_read(&read, &fake_catalog()).unwrap();

        let super::logical_plan::LogicalStatement::Read(plan) = logical else {
            panic!("expected SQL/PGQ relationship pattern to lower into read plan");
        };
        assert_eq!(plan.relationship.direction, expected);
        assert!(matches!(
            plan.order_by.first().map(|sort| &sort.key),
            Some(super::logical_plan::SortBindingKey::Property {
                side: super::logical_plan::BindingSide::Source,
                property
            }) if property == "name"
        ));
    }
}

#[test]
fn sqlpgq_adapter_lowers_all_supported_aggregate_variants() {
    let read = SqlPgqRead {
        source: SqlPgqNodePattern {
            var: "u".to_string(),
            label: "users".to_string(),
        },
        relationship: None,
        optional: false,
        returns: vec![
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Aggregate {
                    func: SqlPgqAggregateFunc::Count,
                    distinct: true,
                    arg: SqlPgqAggregateArg::All,
                },
                alias: Some("count_rows".to_string()),
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Aggregate {
                    func: SqlPgqAggregateFunc::Count,
                    distinct: false,
                    arg: SqlPgqAggregateArg::Var("u".to_string()),
                },
                alias: Some("count_users".to_string()),
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Aggregate {
                    func: SqlPgqAggregateFunc::Sum,
                    distinct: false,
                    arg: SqlPgqAggregateArg::Property {
                        var: "u".to_string(),
                        property: "age".to_string(),
                    },
                },
                alias: Some("sum_age".to_string()),
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Aggregate {
                    func: SqlPgqAggregateFunc::Avg,
                    distinct: false,
                    arg: SqlPgqAggregateArg::Property {
                        var: "u".to_string(),
                        property: "age".to_string(),
                    },
                },
                alias: Some("avg_age".to_string()),
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Aggregate {
                    func: SqlPgqAggregateFunc::Min,
                    distinct: false,
                    arg: SqlPgqAggregateArg::Property {
                        var: "u".to_string(),
                        property: "name".to_string(),
                    },
                },
                alias: Some("min_name".to_string()),
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Aggregate {
                    func: SqlPgqAggregateFunc::Max,
                    distinct: false,
                    arg: SqlPgqAggregateArg::Property {
                        var: "u".to_string(),
                        property: "name".to_string(),
                    },
                },
                alias: Some("max_name".to_string()),
            },
            SqlPgqReturnItem {
                expr: SqlPgqReturnExpr::Aggregate {
                    func: SqlPgqAggregateFunc::Collect,
                    distinct: false,
                    arg: SqlPgqAggregateArg::Property {
                        var: "u".to_string(),
                        property: "name".to_string(),
                    },
                },
                alias: Some("names".to_string()),
            },
        ],
        distinct: false,
        order_by: Vec::new(),
        skip: None,
        limit: None,
    };

    let logical = lower_sqlpgq_read(&read, &fake_catalog()).unwrap();

    let super::logical_plan::LogicalStatement::NodeScan(scan) = logical else {
        panic!("expected SQL/PGQ aggregate pattern to lower into node scan");
    };
    let aggregate_funcs = scan
        .returns
        .iter()
        .map(|binding| match binding {
            super::logical_plan::ReturnBinding::Aggregate { func, .. } => *func,
            other => panic!("expected aggregate binding, got {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        aggregate_funcs,
        vec![
            super::logical_plan::AggregateFunc::Count,
            super::logical_plan::AggregateFunc::Count,
            super::logical_plan::AggregateFunc::Sum,
            super::logical_plan::AggregateFunc::Avg,
            super::logical_plan::AggregateFunc::Min,
            super::logical_plan::AggregateFunc::Max,
            super::logical_plan::AggregateFunc::Collect,
        ]
    );
}

#[test]
fn sqlpgq_adapter_rejects_out_of_matrix_patterns() {
    let optional_node = SqlPgqRead {
        source: SqlPgqNodePattern {
            var: "u".to_string(),
            label: "users".to_string(),
        },
        relationship: None,
        optional: true,
        returns: vec![SqlPgqReturnItem {
            expr: SqlPgqReturnExpr::Var("u".to_string()),
            alias: None,
        }],
        distinct: false,
        order_by: Vec::new(),
        skip: None,
        limit: None,
    };
    let err = lower_sqlpgq_read(&optional_node, &fake_catalog()).unwrap_err();
    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));

    let zero_hop = SqlPgqRead {
        relationship: Some((
            SqlPgqRelationshipPattern {
                var: Some("r".to_string()),
                rel_type: "works_at".to_string(),
                direction: SqlPgqDirection::Out,
                hops: Some((0, 1)),
            },
            SqlPgqNodePattern {
                var: "c".to_string(),
                label: "companies".to_string(),
            },
        )),
        optional: false,
        ..optional_node
    };
    let err = lower_sqlpgq_read(&zero_hop, &fake_catalog()).unwrap_err();
    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
}

#[test]
fn sqlpgq_adapter_maintains_own_compatibility_matrix() {
    assert!(COMPATIBILITY_MATRIX
        .iter()
        .any(|row| row.feature == "single relationship pattern" && row.status == "supported"));
    assert!(COMPATIBILITY_MATRIX
        .iter()
        .any(|row| row.feature == "GRAPH_TABLE SQL text" && row.status != "supported"));
    assert!(COMPATIBILITY_MATRIX
        .iter()
        .any(|row| row.feature == "predicates" && row.status == "deferred"));
}

#[test]
fn binder_rejects_delete_for_unmapped_relationship_row() {
    let ast = crate::gql::parse_statement(
        "MATCH (u:users)-[r:works_at]->(c:companies) DELETE r RETURN u",
    )
    .unwrap();
    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
    assert!(err.to_string().contains("registered edge row table"));
}

#[test]
fn binder_rejects_undirected_delete_edge() {
    let ast = crate::gql::parse_statement("MATCH (u:users)-[r:friend]-(v:users) DELETE r RETURN u")
        .unwrap();
    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
    assert!(err.to_string().contains("directed relationship pattern"));
}

#[test]
fn binder_accepts_single_directed_match_returning_coordinates() {
    let plan = bind_query("MATCH (u:users)-[:works_at]->(c:companies) RETURN u, c");

    assert!(!plan.optional);
    assert_eq!(plan.source.var, "u");
    assert_eq!(plan.source.table_oid, 10);
    assert_eq!(plan.relationship.rel_type, "works_at");
    assert_eq!(plan.target.var, "c");
    assert_eq!(plan.target.table_oid, 20);
    assert_eq!(plan.returns.len(), 2);
}

#[test]
fn binder_accepts_optional_relationship_match() {
    let plan = bind_query("OPTIONAL MATCH (u:users)-[:works_at]->(c:companies) RETURN u, c");

    assert!(plan.optional);
    assert_eq!(plan.source.var, "u");
    assert_eq!(plan.target.var, "c");
}

#[test]
fn binder_rejects_node_only_optional_match() {
    let ast = crate::gql::parse_statement("OPTIONAL MATCH (u:users) RETURN u").unwrap();
    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
    assert!(err.to_string().contains("node-only OPTIONAL MATCH"));
}

#[test]
fn binder_accepts_node_only_match_for_registered_label() {
    let ast = crate::gql::parse_statement("MATCH (u:users {id: 'u1'}) RETURN u, u.name").unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::NodeScan(scan) = plan else {
        panic!("expected node scan plan");
    };

    assert_eq!(scan.node.var, "u");
    assert_eq!(scan.node.table_oid, 10);
    assert!(scan.predicate.is_some());
    assert_eq!(scan.returns.len(), 2);
}

#[test]
fn binder_rejects_unknown_label_and_relationship_type() {
    let unknown_label = parse("MATCH (u:missing)-[:works_at]->(c:companies) RETURN u").unwrap();
    let label_err = bind(&unknown_label, &fake_catalog()).unwrap_err();
    assert!(matches!(label_err.kind, GqlErrorKind::Bind { .. }));

    let unknown_type = parse("MATCH (u:users)-[:owns]->(c:companies) RETURN u").unwrap();
    let type_err = bind(&unknown_type, &fake_catalog()).unwrap_err();
    assert!(matches!(type_err.kind, GqlErrorKind::Bind { .. }));
}

#[test]
fn binder_rejects_out_of_slice_1b_shapes() {
    for query in [
        "MATCH (u:users)-[:works_at*0..1]->(c:companies) RETURN u",
        "MATCH (u:users)-[:works_at*1..65]->(c:companies) RETURN u",
        "MATCH (u:users)-[:works_at]->(c:companies) RETURN u ORDER BY u",
        "MATCH (u:users)-[:works_at]->(c:companies) RETURN u LIMIT 10001",
        "MATCH (u:users)-[:works_at]->(c:companies) RETURN u SKIP 9999 LIMIT 10",
    ] {
        let ast = parse(query).unwrap();
        let err = bind(&ast, &fake_catalog()).unwrap_err();
        assert!(
            matches!(err.kind, GqlErrorKind::Unsupported { .. }),
            "{query}"
        );
    }
}

#[test]
fn binder_accepts_aggregate_return_slots_and_grouping_keys() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN c.name AS company, count(u) AS users, sum(u.age) AS total_age \
         ORDER BY users DESC",
    );
    let physical = lower(logical);

    assert_eq!(physical.returns.len(), 3);
    assert!(matches!(
        physical.returns[1],
        ReturnSlot::Aggregate {
            func: super::logical_plan::AggregateFunc::Count,
            ..
        }
    ));
    assert_eq!(physical.order_by.len(), 1);
}

#[test]
fn binder_orders_aggregate_query_by_returned_property_expression() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN c.name, count(u) AS users ORDER BY c.name",
    );

    assert_eq!(logical.order_by.len(), 1);
}

#[test]
fn binder_accepts_distinct_return_with_and_aggregates() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WITH DISTINCT c.name AS company, c \
         RETURN DISTINCT company, count(DISTINCT c) AS companies",
    );
    let physical = lower(logical);

    assert!(physical.distinct);
    assert_eq!(physical.distinct_stages.len(), 1);
    assert_eq!(physical.distinct_stages[0].len(), 2);
    assert!(matches!(
        physical.returns[1],
        ReturnSlot::Aggregate { distinct: true, .. }
    ));
}

#[test]
fn binder_accepts_aggregate_distinct_for_node_scan() {
    let ast = parse("MATCH (u:users) RETURN collect(DISTINCT u.name) AS names").unwrap();

    let plan = bind_statement(&crate::gql::ast::Statement::Read(ast), &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::NodeScan(scan) = plan else {
        panic!("expected node scan");
    };

    assert!(matches!(
        scan.returns[0],
        super::logical_plan::ReturnBinding::Aggregate { distinct: true, .. }
    ));
}

#[test]
fn binder_rejects_distinct_order_by_non_returned_property() {
    let ast = parse(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN DISTINCT c.name AS company ORDER BY u.age",
    )
    .unwrap();

    let err = bind(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
    assert!(err.to_string().contains("DISTINCT queries must ORDER BY"));
}

#[test]
fn binder_rejects_aggregate_order_by_scope_alias_not_returned() {
    let ast = crate::gql::parse_statement(
        "MATCH (u:users) WITH u.name AS name, u RETURN count(*) AS total ORDER BY name",
    )
    .unwrap();

    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
    assert!(err.to_string().contains("aggregate queries must ORDER BY"));
}

#[test]
fn binder_rejects_delete_return_aggregates() {
    let ast = crate::gql::parse_statement(
        "MATCH (u:users)-[r:friend]->(v:users) DELETE r RETURN count(*)",
    )
    .unwrap();

    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
    assert!(err.to_string().contains("aggregates over DELETE"));
}

#[test]
fn binder_rejects_variable_length_relationship_return() {
    let logical = bind_query("MATCH (u:users)-[r:works_at*1..2]->(c:companies) RETURN r");
    let physical = lower(logical);

    assert!(matches!(physical.returns[0], ReturnSlot::Path { .. }));
}

#[test]
fn binder_accepts_path_functions_over_relationship_variables() {
    let logical = bind_query(
        "MATCH (u:users)-[r:works_at*1..2]->(c:companies) \
         RETURN nodes(r) AS ns, relationships(r) AS rs, length(r) AS len ORDER BY len DESC",
    );
    let physical = lower(logical);

    assert!(matches!(
        physical.returns[0],
        ReturnSlot::PathFunction {
            func: super::logical_plan::PathFunc::Nodes,
            ..
        }
    ));
    assert!(matches!(
        physical.returns[1],
        ReturnSlot::PathFunction {
            func: super::logical_plan::PathFunc::Relationships,
            ..
        }
    ));
    assert!(matches!(
        physical.returns[2],
        ReturnSlot::PathFunction {
            func: super::logical_plan::PathFunc::Length,
            ..
        }
    ));
    assert_eq!(physical.order_by.len(), 1);
}

#[test]
fn binder_accepts_mixed_case_path_functions() {
    let logical = bind_query(
        "MATCH (u:users)-[r:works_at*1..2]->(c:companies) \
         RETURN NoDeS(r) AS ns, ReLaTiOnShIpS(r) AS rs, LeNgTh(r) AS len",
    );
    let physical = lower(logical);

    assert!(matches!(
        physical.returns[0],
        ReturnSlot::PathFunction {
            func: super::logical_plan::PathFunc::Nodes,
            ..
        }
    ));
    assert!(matches!(
        physical.returns[1],
        ReturnSlot::PathFunction {
            func: super::logical_plan::PathFunc::Relationships,
            ..
        }
    ));
    assert!(matches!(
        physical.returns[2],
        ReturnSlot::PathFunction {
            func: super::logical_plan::PathFunc::Length,
            ..
        }
    ));
}

#[test]
fn binder_rejects_path_functions_outside_return_projection() {
    let ast = crate::gql::parse_statement(
        "MATCH (u:users)-[r:works_at*1..2]->(c:companies) \
         WITH nodes(r) AS ns RETURN ns",
    )
    .unwrap();

    let err = bind_statement(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
    assert!(err.to_string().contains("path-function WITH projections"));
}

#[test]
fn executor_enforces_hard_row_cap_before_projection() {
    let physical = lower(bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) RETURN u ORDER BY u.name",
    ));
    let mut engine = Engine::new();
    for idx in 0..10_002 {
        let user_pk = format!("u{idx}");
        let company_pk = format!("c{idx}");
        let user = engine.node_store.add_node(10, user_pk.clone());
        let company = engine.node_store.add_node(20, company_pk.clone());
        engine.resolution_insert(10, &user_pk, user);
        engine.resolution_insert(20, &company_pk, company);
        engine.insert_table_membership(10, user);
        engine.insert_table_membership(20, company);
    }
    let works_at = engine.register_edge_type("works_at").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        (0..10_002)
            .map(|idx| RawEdge {
                source: idx * 2,
                target: idx * 2 + 1,
                type_id: works_at,
                weight: None,
            })
            .collect(),
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    engine.built = true;

    let err = execute(&engine, &physical, None).unwrap_err();

    assert!(matches!(err, GraphError::GqlExecution { .. }));
    assert!(err.to_string().contains("row cap"));
}

#[test]
fn lowering_preserves_bound_tables_and_return_slots() {
    let logical = bind_query("MATCH (u:users)-[r:works_at]->(c:companies) RETURN c, r, u");
    let physical = lower(logical);

    assert_eq!(physical.source_table_oid, 10);
    assert_eq!(physical.target_table_oid, 20);
    assert_eq!(physical.rel_type, "works_at");
    assert_eq!(physical.rel_var.as_deref(), Some("r"));
    assert_eq!(
        physical.returns,
        vec![
            ReturnSlot::Node {
                side: super::logical_plan::BindingSide::Target,
                name: "c".into()
            },
            ReturnSlot::Relationship { name: "r".into() },
            ReturnSlot::Node {
                side: super::logical_plan::BindingSide::Source,
                name: "u".into()
            }
        ]
    );
}

#[test]
fn binder_allows_with_aliases_downstream() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WITH c AS company, u.name AS employee RETURN company, employee",
    );
    let physical = lower(logical);

    assert_eq!(
        physical.returns,
        vec![
            ReturnSlot::Node {
                side: super::logical_plan::BindingSide::Target,
                name: "company".into()
            },
            ReturnSlot::Property {
                side: super::logical_plan::BindingSide::Source,
                property: "name".into(),
                name: "employee".into()
            }
        ]
    );
}

#[test]
fn binder_with_shadowing_rebinds_name_to_new_scope() {
    let logical = bind_query("MATCH (u:users)-[:works_at]->(c:companies) WITH c AS u RETURN u");
    let physical = lower(logical);

    assert_eq!(
        physical.returns,
        vec![ReturnSlot::Node {
            side: super::logical_plan::BindingSide::Target,
            name: "u".into()
        }]
    );
}

#[test]
fn binder_with_scope_does_not_leak_hidden_variables() {
    let ast = parse(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WITH c AS company RETURN u",
    )
    .unwrap();

    let err = bind(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Bind { .. }));
    assert!(err.to_string().contains("unknown return variable `u`"));
}

#[test]
fn binder_with_property_alias_can_be_returned_and_ordered() {
    let ast = crate::gql::parse_statement(
        "MATCH (u:users) WITH u.name AS name RETURN name ORDER BY name",
    )
    .unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::NodeScan(scan) = plan else {
        panic!("expected node scan plan");
    };

    assert_eq!(
        scan.returns,
        vec![super::logical_plan::ReturnBinding::Property {
            side: super::logical_plan::BindingSide::Source,
            property: "name".into(),
            name: "name".into()
        }]
    );
    assert_eq!(scan.order_by.len(), 1);
}

#[test]
fn binder_with_scope_can_chain_projection_stages() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WITH c AS company WITH company.name AS company_name RETURN company_name",
    );
    let physical = lower(logical);

    assert_eq!(
        physical.returns,
        vec![ReturnSlot::Property {
            side: super::logical_plan::BindingSide::Target,
            property: "name".into(),
            name: "company_name".into()
        }]
    );
}

#[test]
fn binder_rejects_duplicate_pattern_variables_in_initial_scope() {
    for query in [
        "MATCH (u:users)-[:friend]->(u:users) RETURN u",
        "MATCH (u:users)-[u:friend]->(v:users) RETURN u",
    ] {
        let ast = parse(query).unwrap();
        let err = bind(&ast, &fake_catalog()).unwrap_err();

        assert!(
            matches!(err.kind, GqlErrorKind::Bind { .. }),
            "{query}: {err:?}"
        );
        assert!(err.to_string().contains("duplicate variable"), "{query}");
    }
}

#[test]
fn binder_orders_by_with_scalar_alias_not_returned() {
    let ast = crate::gql::parse_statement(
        "MATCH (u:users) WITH u.name AS name, u RETURN u ORDER BY name",
    )
    .unwrap();
    let plan = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::NodeScan(scan) = plan else {
        panic!("expected node scan plan");
    };

    assert_eq!(
        scan.returns,
        vec![super::logical_plan::ReturnBinding::Node {
            side: super::logical_plan::BindingSide::Source,
            name: "u".into()
        }]
    );
    assert_eq!(scan.order_by.len(), 1);
}

#[test]
fn value_projection_returns_relationship_coordinates() {
    let logical = bind_query("MATCH (u:users)-[r:works_at]->(c:companies) RETURN r");
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();

    let projected = project_rows(
        rows,
        &physical,
        &HydratedRows::new(),
        &QueryParams::new(),
        false,
    )
    .unwrap();

    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0]["r"]["_type"], "works_at");
    assert_eq!(projected[0]["r"]["_start"]["table"], "users");
    assert_eq!(projected[0]["r"]["_start"]["id"], "u1");
    assert_eq!(projected[0]["r"]["_end"]["table"], "companies");
    assert_eq!(projected[0]["r"]["_end"]["id"], "c1");
}

#[test]
fn value_projection_returns_inbound_relationship_orientation() {
    let logical = bind_query("MATCH (c:companies)<-[r:works_at]-(u:users) RETURN r");
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();

    let projected = project_rows(
        rows,
        &physical,
        &HydratedRows::new(),
        &QueryParams::new(),
        false,
    )
    .unwrap();

    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0]["r"]["_type"], "works_at");
    assert_eq!(projected[0]["r"]["_start"]["table"], "users");
    assert_eq!(projected[0]["r"]["_start"]["id"], "u1");
    assert_eq!(projected[0]["r"]["_end"]["table"], "companies");
    assert_eq!(projected[0]["r"]["_end"]["id"], "c1");
}

#[test]
fn value_projection_preserves_undirected_opposite_relationships() {
    let logical = bind_query("MATCH (u:users)-[r:works_at]-(c:companies) RETURN r");
    let physical = lower(logical);
    let mut engine = engine_fixture();
    let works_at = engine.register_edge_type("works_at").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 0,
                type_id: works_at,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    let rows = execute(&engine, &physical, None).unwrap();

    let projected = project_rows(
        rows,
        &physical,
        &HydratedRows::new(),
        &QueryParams::new(),
        false,
    )
    .unwrap();

    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0]["r"]["_start"]["table"], "users");
    assert_eq!(projected[0]["r"]["_start"]["id"], "u1");
    assert_eq!(projected[0]["r"]["_end"]["table"], "companies");
    assert_eq!(projected[0]["r"]["_end"]["id"], "c1");
    assert_eq!(projected[1]["r"]["_start"]["table"], "companies");
    assert_eq!(projected[1]["r"]["_start"]["id"], "c1");
    assert_eq!(projected[1]["r"]["_end"]["table"], "users");
    assert_eq!(projected[1]["r"]["_end"]["id"], "u1");
}

#[test]
fn executor_returns_one_hop_coordinate_rows() {
    let logical = bind_query("MATCH (u:users)-[:works_at]->(c:companies) RETURN u, c");
    let physical = lower(logical);
    let engine = engine_fixture();

    let rows = execute(&engine, &physical, None).unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].source.table_oid, 10);
    assert_eq!(rows[0].source.node_id, "u1");
    let target = rows[0].target.as_ref().expect("target");
    assert_eq!(target.table_oid, 20);
    assert_eq!(target.node_id, "c1");
    assert_eq!(rows[1].source.node_id, "u2");
}

#[test]
fn optional_match_null_extends_unmatched_source_rows() {
    let logical = bind_query("OPTIONAL MATCH (u:users)-[:works_at]->(c:companies) RETURN u, c");
    let physical = lower(logical);
    let mut engine = engine_fixture();
    let u3 = engine.node_store.add_node(10, "u3".to_string());
    engine.resolution_insert(10, "u3", u3);
    engine.insert_table_membership(10, u3);

    let rows = execute(&engine, &physical, None).unwrap();

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].source.node_id, "u3");
    assert!(rows[2].target.is_none());
}

#[test]
fn optional_match_predicate_miss_null_extends_source_row() {
    let logical = bind_query(
        "OPTIONAL MATCH (u:users)-[:works_at]->(c:companies) \
         WHERE c.name = 'Acme' RETURN u, c",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0]["u"]["id"], "u1");
    assert_eq!(projected[0]["c"]["id"], "c1");
    assert_eq!(projected[1]["u"]["id"], "u2");
    assert!(projected[1]["c"].is_null());
}

#[test]
fn aggregate_projection_groups_and_computes_numeric_values() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN c.name AS company,
                count(u) AS users,
                sum(u.age) AS total_age,
                avg(u.age) AS avg_age,
                min(u.age) AS youngest,
                max(u.age) AS oldest,
                collect(u.name) AS names
         ORDER BY company",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0]["company"], "Acme");
    assert_eq!(projected[0]["users"], 1);
    assert_eq!(projected[0]["total_age"], 37.0);
    assert_eq!(projected[0]["avg_age"], 37.0);
    assert_eq!(projected[0]["youngest"], 37);
    assert_eq!(projected[0]["oldest"], 37);
    assert_eq!(projected[0]["names"], serde_json::json!(["Ada"]));
}

#[test]
fn aggregate_projection_returns_empty_group_for_empty_input() {
    let ast = crate::gql::parse_statement(
        "MATCH (u:users) WHERE u.name = 'Missing' \
         RETURN count(*) AS rows, sum(u.age) AS total_age, collect(u.name) AS names",
    )
    .unwrap();
    let logical = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::physical_plan::PhysicalStatement::NodeScan(physical) = lower_statement(logical)
    else {
        panic!("expected node scan");
    };
    let engine = engine_fixture();
    let rows = execute_node_scan(&engine, &physical, None).unwrap();
    let projected = project_node_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["rows"], 0);
    assert!(projected[0]["total_age"].is_null());
    assert_eq!(projected[0]["names"], serde_json::json!([]));
}

#[test]
fn aggregate_limit_does_not_truncate_input_rows() {
    let logical =
        bind_query("MATCH (u:users)-[:works_at]->(c:companies) RETURN count(*) AS rows LIMIT 1");
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(physical.execution_row_cap(), 10_000);
    assert!(physical.cap_exhaustion_is_error());
    assert_eq!(projected, vec![serde_json::json!({"rows": 2})]);
}

#[test]
fn path_projection_returns_stable_path_value_and_functions() {
    let logical = bind_query(
        "MATCH (u:users)-[p:works_at*1..2]->(c:companies) \
         RETURN p, nodes(p) AS ns, relationships(p) AS rs, length(p) AS len \
         ORDER BY len DESC",
    );
    let physical = lower(logical);
    let mut engine = engine_fixture();
    let works_at = engine
        .edge_type_registry
        .iter()
        .position(|label| label == "works_at")
        .expect("works_at edge type missing") as u8;
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: works_at,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0]["len"], 2);
    assert_eq!(projected[0]["ns"][0]["_id"]["table"], "users");
    assert_eq!(projected[0]["ns"][0]["id"], "u1");
    assert_eq!(projected[0]["ns"][1]["_id"]["table"], "companies");
    assert_eq!(projected[0]["ns"][1]["id"], "c1");
    assert_eq!(projected[0]["ns"][2]["id"], "c2");
    assert_eq!(
        projected[0]["rs"].as_array().expect("relationships").len(),
        2
    );
    assert_eq!(projected[0]["rs"][0]["_type"], "works_at");
    assert_eq!(projected[0]["rs"][0]["_start"]["id"], "u1");
    assert_eq!(projected[0]["rs"][0]["_end"]["id"], "c1");
    assert_eq!(projected[0]["p"]["_path"]["nodes"], projected[0]["ns"]);
    assert_eq!(
        projected[0]["p"]["_path"]["relationships"],
        projected[0]["rs"]
    );
}

#[test]
fn path_projection_preserves_distinct_paths_to_same_target() {
    let logical = bind_query(
        "MATCH (u:users)-[p:friend*1..2]->(v:users) \
         WHERE u.id = 'u1' AND v.id = 'u3' RETURN length(p) AS len ORDER BY len",
    );
    let physical = lower(logical);
    let mut engine = Engine::new();
    for pk in ["u1", "u2", "u3"] {
        let node_idx = engine.node_store.add_node(10, pk.to_string());
        engine.resolution_insert(10, pk, node_idx);
        engine.insert_table_membership(10, node_idx);
    }
    let friend = engine.register_edge_type("friend").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: friend,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: friend,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: friend,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    engine.built = true;
    let hydrated = HashMap::from([
        (
            (10, "u1".to_string()),
            serde_json::json!({"id": "u1", "name": "Ada"}),
        ),
        (
            (10, "u2".to_string()),
            serde_json::json!({"id": "u2", "name": "Linus"}),
        ),
        (
            (10, "u3".to_string()),
            serde_json::json!({"id": "u3", "name": "Grace"}),
        ),
    ]);
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap();

    assert_eq!(
        projected
            .iter()
            .map(|row| row["len"].as_u64().expect("path length"))
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn variable_length_cardinality_does_not_depend_on_returning_path_values() {
    let path_query = lower(bind_query(
        "MATCH (u:users)-[p:friend*1..2]->(v:users) \
         WHERE u.id = 'u1' AND v.id = 'u3' RETURN length(p) AS len ORDER BY len",
    ));
    let count_query = lower(bind_query(
        "MATCH (u:users)-[p:friend*1..2]->(v:users) \
         WHERE u.id = 'u1' AND v.id = 'u3' RETURN count(*) AS paths",
    ));
    let mut engine = Engine::new();
    for pk in ["u1", "u2", "u3"] {
        let node_idx = engine.node_store.add_node(10, pk.to_string());
        engine.resolution_insert(10, pk, node_idx);
        engine.insert_table_membership(10, node_idx);
    }
    let friend = engine.register_edge_type("friend").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: friend,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: friend,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: friend,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    engine.built = true;
    let hydrated = HashMap::from([
        (
            (10, "u1".to_string()),
            serde_json::json!({"id": "u1", "name": "Ada"}),
        ),
        (
            (10, "u2".to_string()),
            serde_json::json!({"id": "u2", "name": "Linus"}),
        ),
        (
            (10, "u3".to_string()),
            serde_json::json!({"id": "u3", "name": "Grace"}),
        ),
    ]);

    let path_rows = execute(&engine, &path_query, None).unwrap();
    let count_rows = execute(&engine, &count_query, None).unwrap();
    let path_projected =
        project_rows(path_rows, &path_query, &hydrated, &QueryParams::new(), true).unwrap();
    let count_projected = project_rows(
        count_rows,
        &count_query,
        &hydrated,
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(path_projected.len(), 2);
    assert_eq!(count_projected, vec![serde_json::json!({"paths": 2})]);
}

#[test]
fn explicit_single_hop_variable_length_preserves_path_distinct_matches() {
    let path_query = lower(bind_query(
        "MATCH (u:users)-[p:friend*1..1]-(v:users) \
         WHERE u.id = 'u1' AND v.id = 'u2' RETURN length(p) AS len ORDER BY len",
    ));
    let count_query = lower(bind_query(
        "MATCH (u:users)-[p:friend*1..1]-(v:users) \
         WHERE u.id = 'u1' AND v.id = 'u2' RETURN count(*) AS paths",
    ));
    let mut engine = Engine::new();
    for pk in ["u1", "u2"] {
        let node_idx = engine.node_store.add_node(10, pk.to_string());
        engine.resolution_insert(10, pk, node_idx);
        engine.insert_table_membership(10, node_idx);
    }
    let friend = engine.register_edge_type("friend").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: friend,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: friend,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    engine.built = true;
    let hydrated = HashMap::from([
        ((10, "u1".to_string()), serde_json::json!({"id": "u1"})),
        ((10, "u2".to_string()), serde_json::json!({"id": "u2"})),
    ]);

    let path_rows = execute(&engine, &path_query, None).unwrap();
    let count_rows = execute(&engine, &count_query, None).unwrap();
    let path_projected =
        project_rows(path_rows, &path_query, &hydrated, &QueryParams::new(), true).unwrap();
    let count_projected = project_rows(
        count_rows,
        &count_query,
        &hydrated,
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(path_projected.len(), 2);
    assert_eq!(
        path_projected
            .iter()
            .map(|row| row["len"].as_u64().expect("path length"))
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    assert_eq!(count_projected, vec![serde_json::json!({"paths": 2})]);
}

#[test]
fn optional_path_functions_return_null_for_unmatched_rows() {
    let logical = bind_query(
        "OPTIONAL MATCH (u:users)-[p:works_at]->(c:companies) \
         WHERE c.name = 'Missing' RETURN nodes(p) AS ns, relationships(p) AS rs, length(p) AS len",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 2);
    assert!(projected.iter().all(|row| row["ns"].is_null()));
    assert!(projected.iter().all(|row| row["rs"].is_null()));
    assert!(projected.iter().all(|row| row["len"].is_null()));
}

#[test]
fn relationship_node_projection_errors_when_required_hydration_is_missing() {
    let logical = bind_query("MATCH (u:users)-[:works_at]->(c:companies) RETURN u");
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let err = project_rows(
        rows,
        &physical,
        &HydratedRows::new(),
        &QueryParams::new(),
        true,
    )
    .unwrap_err();

    assert!(matches!(err, GraphError::GqlExecution { .. }));
    assert!(err.to_string().contains("could not hydrate"));
}

#[test]
fn node_scan_projection_errors_when_required_hydration_is_missing() {
    let ast = crate::gql::parse_statement("MATCH (u:users) RETURN u").unwrap();
    let logical = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::logical_plan::LogicalStatement::NodeScan(scan) = logical else {
        panic!("expected node scan");
    };
    let physical = lower_statement(super::logical_plan::LogicalStatement::NodeScan(scan));
    let super::physical_plan::PhysicalStatement::NodeScan(physical) = physical else {
        panic!("expected physical node scan");
    };
    let engine = engine_fixture();
    let rows = execute_node_scan(&engine, &physical, None).unwrap();
    let err = project_node_rows(
        rows,
        &physical,
        &HydratedRows::new(),
        &QueryParams::new(),
        true,
    )
    .unwrap_err();

    assert!(matches!(err, GraphError::GqlExecution { .. }));
    assert!(err.to_string().contains("could not hydrate"));
}

#[test]
fn missing_hydrated_property_does_not_match_is_null_predicate() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) WHERE u.name IS NULL RETURN u.name AS name",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let err = super::value::filter_rows(rows, &physical, &HydratedRows::new(), &QueryParams::new())
        .unwrap_err();

    assert!(matches!(err, GraphError::GqlExecution { .. }));
    assert!(err.to_string().contains("could not hydrate"));
}

#[test]
fn optional_aggregate_counts_null_extended_rows_like_left_join() {
    let logical = bind_query(
        "OPTIONAL MATCH (u:users)-[:works_at]->(c:companies) \
         WHERE c.name = 'Acme' RETURN count(*) AS source_rows, count(c) AS matched_targets",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["source_rows"], 2);
    assert_eq!(projected[0]["matched_targets"], 1);
}

#[test]
fn optional_collect_skips_null_extended_values() {
    let logical = bind_query(
        "OPTIONAL MATCH (u:users)-[:works_at]->(c:companies) \
         WHERE c.name = 'Acme'
         RETURN collect(c.name) AS names, collect(DISTINCT c.name) AS distinct_names",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["names"], serde_json::json!(["Acme"]));
    assert_eq!(projected[0]["distinct_names"], serde_json::json!(["Acme"]));
}

#[test]
fn distinct_return_deduplicates_before_order_and_limit() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN DISTINCT c.name AS company ORDER BY company LIMIT 1",
    );
    let physical = lower(logical);
    let mut engine = engine_fixture();
    let works_at = engine.register_edge_type("works_at").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 3,
                type_id: works_at,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected, vec![serde_json::json!({"company": "Acme"})]);
}

#[test]
fn with_distinct_deduplicates_input_to_later_aggregate() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WITH DISTINCT c.name AS company \
         RETURN count(*) AS companies",
    );
    let physical = lower(logical);
    let mut engine = engine_fixture();
    let works_at = engine.register_edge_type("works_at").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 3,
                type_id: works_at,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected, vec![serde_json::json!({"companies": 2})]);
}

#[test]
fn aggregate_distinct_deduplicates_inputs_per_group() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN count(DISTINCT c.name) AS companies, collect(DISTINCT c.name) AS names",
    );
    let physical = lower(logical);
    let mut engine = engine_fixture();
    let works_at = engine.register_edge_type("works_at").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 3,
                type_id: works_at,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    let rows = execute(&engine, &physical, None).unwrap();
    let projected = project_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["companies"], 2);
    assert_eq!(projected[0]["names"], serde_json::json!(["Acme", "Bell"]));
}

#[test]
fn distinct_projection_aborts_when_key_cap_is_exceeded() {
    let ast = crate::gql::parse_statement("MATCH (u:users) RETURN DISTINCT u.name").unwrap();
    let logical = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::physical_plan::PhysicalStatement::NodeScan(physical) = lower_statement(logical)
    else {
        panic!("expected node scan");
    };
    let rows = (0..10_001)
        .map(|idx| GqlNodeRow {
            node: GqlNodeCoordinate {
                table_oid: 10,
                node_id: format!("u{idx}"),
            },
        })
        .collect::<Vec<_>>();
    let hydrated = (0..10_001)
        .map(|idx| {
            (
                (10, format!("u{idx}")),
                serde_json::json!({"id": format!("u{idx}"), "name": format!("user-{idx}")}),
            )
        })
        .collect::<HydratedRows>();

    let err = project_node_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap_err();

    assert!(matches!(err, GraphError::GqlExecution { .. }));
    assert!(err.to_string().contains("DISTINCT key cap"));
}

#[test]
fn executor_node_scan_reads_graph_and_transaction_nodes() {
    crate::projection::tx_delta::clear_for_test();
    let ast = crate::gql::parse_statement("MATCH (u:users) RETURN u").unwrap();
    let logical = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::physical_plan::PhysicalStatement::NodeScan(physical) = lower_statement(logical)
    else {
        panic!("expected node scan");
    };
    let engine = engine_fixture();
    crate::projection::tx_delta::record_added_node(10, "u3", None).expect("record tx node");

    let rows = execute_node_scan(&engine, &physical, None).unwrap();

    assert_eq!(
        rows.iter()
            .map(|row| row.node.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["u1", "u2", "u3"]
    );
    crate::projection::tx_delta::clear_for_test();
}

#[test]
fn executor_node_scan_hides_unscoped_transaction_nodes_under_tenant_scope() {
    crate::projection::tx_delta::clear_for_test();
    let ast = crate::gql::parse_statement("MATCH (u:users) RETURN u").unwrap();
    let logical = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::physical_plan::PhysicalStatement::NodeScan(physical) = lower_statement(logical)
    else {
        panic!("expected node scan");
    };
    let mut engine = engine_fixture();
    engine.tenanted_table_oids.insert(10);
    engine.insert_tenant_membership("tenant-a", 0);
    engine.insert_tenant_membership("tenant-a", 1);
    crate::projection::tx_delta::record_added_node(10, "u3", None)
        .expect("record unscoped tx node");
    crate::projection::tx_delta::record_added_node(10, "u4", Some("tenant-a"))
        .expect("record tenant tx node");

    let rows = execute_node_scan(&engine, &physical, Some("tenant-a")).unwrap();

    assert_eq!(
        rows.iter()
            .map(|row| row.node.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["u1", "u2", "u4"]
    );
    crate::projection::tx_delta::clear_for_test();
}

#[test]
fn executor_node_scan_keeps_unscoped_transaction_nodes_for_nontenanted_tables() {
    crate::projection::tx_delta::clear_for_test();
    let ast = crate::gql::parse_statement("MATCH (u:users) RETURN u").unwrap();
    let logical = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::physical_plan::PhysicalStatement::NodeScan(physical) = lower_statement(logical)
    else {
        panic!("expected node scan");
    };
    let engine = engine_fixture();
    crate::projection::tx_delta::record_added_node(10, "u3", None)
        .expect("record unscoped tx node");

    let rows = execute_node_scan(&engine, &physical, Some("tenant-a")).unwrap();

    assert_eq!(
        rows.iter()
            .map(|row| row.node.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["u1", "u2", "u3"]
    );
    crate::projection::tx_delta::clear_for_test();
}

#[test]
fn node_scan_projection_filters_inline_predicates() {
    let ast =
        crate::gql::parse_statement("MATCH (u:users {name: 'Ada'}) RETURN u.name AS name").unwrap();
    let logical = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::physical_plan::PhysicalStatement::NodeScan(physical) = lower_statement(logical)
    else {
        panic!("expected node scan");
    };
    let engine = engine_fixture();
    let rows = execute_node_scan(&engine, &physical, None).unwrap();
    let projected = project_node_rows(
        rows,
        &physical,
        &hydrated_fixture(),
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["name"], "Ada");
}

#[test]
fn node_scan_limit_does_not_hide_later_predicate_matches() {
    crate::projection::tx_delta::clear_for_test();
    let ast = crate::gql::parse_statement("MATCH (u:users {id: 'u3'}) RETURN u LIMIT 1").unwrap();
    let logical = bind_statement(&ast, &fake_catalog()).unwrap();
    let super::physical_plan::PhysicalStatement::NodeScan(physical) = lower_statement(logical)
    else {
        panic!("expected node scan");
    };
    let engine = engine_fixture();
    crate::projection::tx_delta::record_added_node(10, "u3", None).expect("record tx node");

    let rows = execute_node_scan(&engine, &physical, None).unwrap();
    let mut hydrated = hydrated_fixture();
    hydrated.insert(
        (10, "u3".to_string()),
        serde_json::json!({"id": "u3", "name": "Grace"}),
    );
    let projected =
        project_node_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["u"]["id"], "u3");
    crate::projection::tx_delta::clear_for_test();
}

#[test]
fn executor_filters_wrong_target_table_and_edge_type() {
    let logical = bind_query("MATCH (u:users)-[:works_at]->(c:companies) RETURN u, c");
    let physical = lower(logical);
    let mut engine = engine_fixture();
    let works_at = engine.register_edge_type("works_at").unwrap();
    let owns = engine.register_edge_type("owns").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 0,
                target: 1,
                type_id: owns,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 0,
                type_id: works_at,
                weight: None,
            },
        ],
        false,
    );

    let rows = execute(&engine, &physical, None).unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source.node_id, "u1");
    assert_eq!(rows[0].target.as_ref().expect("target").node_id, "c1");
}

#[test]
fn executor_applies_tenant_scope_to_source_and_target_nodes() {
    let logical = bind_query("MATCH (u:users)-[:works_at]->(c:companies) RETURN u, c");
    let physical = lower(logical);
    let mut engine = engine_fixture();
    engine.tenanted_table_oids.insert(10);
    engine.tenanted_table_oids.insert(20);
    engine.insert_tenant_membership("tenant-a", 0);
    engine.insert_tenant_membership("tenant-b", 2);
    engine.insert_tenant_membership("tenant-a", 1);
    engine.insert_tenant_membership("tenant-a", 3);

    let rows = execute(&engine, &physical, Some("tenant-a")).unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source.node_id, "u2");
    assert_eq!(rows[0].target.as_ref().expect("target").node_id, "c2");
}

#[test]
fn executor_applies_tenant_scope_to_var_len_and_undirected_frontiers() {
    let var_len = lower(bind_query(
        "MATCH (u:users)-[:works_at*2..2]->(c:companies) RETURN u, c",
    ));
    let undirected = lower(bind_query(
        "MATCH (u:users)-[:works_at]-(c:companies) RETURN u, c",
    ));
    let mut engine = engine_fixture();
    engine.tenanted_table_oids.insert(10);
    engine.tenanted_table_oids.insert(20);
    engine.insert_tenant_membership("tenant-a", 0);
    engine.insert_tenant_membership("tenant-a", 1);
    engine.insert_tenant_membership("tenant-b", 2);
    engine.insert_tenant_membership("tenant-a", 3);
    let works_at = engine
        .edge_type_registry
        .iter()
        .position(|label| label == "works_at")
        .expect("works_at edge type missing") as u8;
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 2,
                target: 3,
                type_id: works_at,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();

    assert!(execute(&engine, &var_len, Some("tenant-a"))
        .unwrap()
        .is_empty());
    assert!(execute(&engine, &undirected, Some("tenant-a"))
        .unwrap()
        .is_empty());
}

#[test]
fn binder_accepts_where_inline_props_and_property_returns() {
    let plan = bind_query(
        "MATCH (u:users {name: $name})-[:works_at]->(c:companies) \
         WHERE c.name = 'Acme' RETURN u.name AS employee, c",
    );

    assert!(plan.predicate.is_some());
    assert_eq!(plan.returns.len(), 2);
}

#[test]
fn binder_rejects_unknown_and_reserved_properties() {
    for query in [
        "MATCH (u:users)-[:works_at]->(c:companies) RETURN u.missing",
        "MATCH (u:users {_id: 'u1'})-[:works_at]->(c:companies) RETURN u",
    ] {
        let ast = parse(query).unwrap();
        let err = bind(&ast, &fake_catalog()).unwrap_err();
        assert!(matches!(err.kind, GqlErrorKind::Bind { .. }), "{query}");
    }
}

#[test]
fn binder_rejects_duplicate_return_names() {
    let ast = parse(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN u.name AS x, c.name AS x",
    )
    .unwrap();

    let err = bind(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Bind { .. }));
}

#[test]
fn binder_rejects_deep_boolean_predicates() {
    let mut query = "MATCH (u:users)-[:works_at]->(c:companies) WHERE ".to_string();
    for _ in 0..513 {
        query.push_str("u.id = 'u1' AND ");
    }
    query.push_str("u.id = 'u1' RETURN u");
    let ast = parse(&query).unwrap();

    let err = bind(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Syntax { .. }));
}

#[test]
fn binder_rejects_excessive_inline_property_predicates() {
    let mut query = "MATCH (u:users {".to_string();
    for idx in 0..513 {
        if idx > 0 {
            query.push_str(", ");
        }
        query.push_str("id: 'u1'");
    }
    query.push_str("})-[:works_at]->(c:companies) RETURN u");
    let ast = parse(&query).unwrap();

    let err = bind(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Syntax { .. }));
}

#[test]
fn binder_rejects_registered_reserved_property_keys() {
    let catalog = FakeCatalog::new()
        .with_label("users", 10, ["id", "_shadow"])
        .with_label("companies", 20, ["id", "name"])
        .with_edge("works_at", 10, 20);
    let ast = parse("MATCH (u:users)-[:works_at]->(c:companies) RETURN u").unwrap();

    let err = bind(&ast, &catalog).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Bind { .. }));
}

#[test]
fn value_projection_filters_predicates_and_hydrates_nodes() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WHERE u.name IN ['Ada', 'Grace'] RETURN u.name AS employee, c",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let hydrated = hydrated_fixture();

    let projected = project_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["employee"], "Ada");
    assert_eq!(projected[0]["c"]["name"], "Acme");
    assert_eq!(projected[0]["c"]["_id"]["table"], "companies");
    assert_eq!(projected[0]["c"]["_labels"][0], "companies");
}

#[test]
fn value_projection_reports_missing_parameters() {
    let logical = bind_query("MATCH (u:users {name: $name})-[:works_at]->(c:companies) RETURN u");
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let hydrated = hydrated_fixture();

    let err = project_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap_err();

    assert!(matches!(err, GraphError::GqlParameter { .. }));
    assert!(err.to_string().contains("missing GQL parameter"));
}

#[test]
fn value_projection_filters_explicit_null_predicates() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WHERE c.name IS NULL RETURN c.name AS company_name",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let mut hydrated = hydrated_fixture();
    hydrated.insert(
        (20, "c1".to_string()),
        serde_json::json!({"id": "c1", "name": null}),
    );

    let projected = project_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap();

    assert_eq!(projected.len(), 1);
    assert!(projected[0]["company_name"].is_null());
}

#[test]
fn value_projection_reads_jsonb_paths_and_distinguishes_missing_from_null() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WHERE u.profile.plan = 'pro' AND u.profile.explicit_null IS NULL \
         RETURN u.profile.tags AS tags, u.profile.flags AS flags, u.profile.missing AS missing",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let hydrated = hydrated_fixture();

    let projected = project_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["tags"], serde_json::json!(["founder", 7]));
    assert_eq!(projected[0]["flags"], serde_json::json!({"beta": true}));
    assert!(projected[0]["missing"].is_null());

    let missing_null_query = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WHERE u.profile.missing IS NULL RETURN u.name AS name",
    );
    let missing_physical = lower(missing_null_query);
    let rows = execute(&engine, &missing_physical, None).unwrap();
    let missing_projected = project_rows(
        rows,
        &missing_physical,
        &hydrated,
        &QueryParams::new(),
        true,
    )
    .unwrap();

    assert!(missing_projected.is_empty());
}

#[test]
fn value_projection_reports_non_orderable_predicate_types() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         WHERE u.name > 42 RETURN u",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let hydrated = hydrated_fixture();

    let err = project_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap_err();

    assert!(matches!(err, GraphError::GqlExecution { .. }));
    assert!(err.to_string().contains("ordered comparisons"));
}

#[test]
fn value_projection_honors_hydrate_false_shape() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN u, c.name AS company_name",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let hydrated = hydrated_fixture();

    let projected = project_rows(rows, &physical, &hydrated, &QueryParams::new(), false).unwrap();

    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0]["u"]["_id"]["id"], "u1");
    assert_eq!(projected[0]["u"]["_labels"][0], "users");
    assert!(projected[0]["u"].get("name").is_none());
    assert_eq!(projected[0]["company_name"], "Acme");
}

#[test]
fn explain_contains_stable_1b_plan_shape() {
    let logical = bind_query("MATCH (u:users)-[:works_at]->(c:companies) RETURN u, c");
    let physical = lower(logical);

    assert_eq!(
        explain(&physical),
        "Expand(source=u:users, rel=works_at, hops=1..1, target=c:companies, return=[u, c])"
    );
}

#[test]
fn explain_marks_optional_relationship_expands() {
    let logical = bind_query("OPTIONAL MATCH (u:users)-[:works_at]->(c:companies) RETURN u, c");
    let physical = lower(logical);

    assert_eq!(
        explain(&physical),
        "OptionalExpand(source=u:users, rel=works_at, hops=1..1, target=c:companies, return=[u, c])"
    );
}

#[test]
fn executor_supports_inbound_undirected_and_bounded_var_length() {
    let inbound = lower(bind_query(
        "MATCH (c:companies)<-[:works_at]-(u:users) RETURN c, u",
    ));
    let undirected = lower(bind_query(
        "MATCH (u:users)-[:works_at]-(c:companies) RETURN u, c",
    ));
    let var_len = lower(bind_query(
        "MATCH (u:users)-[:works_at*1..2]->(c:companies) RETURN u, c",
    ));
    let engine = engine_fixture();

    assert_eq!(execute(&engine, &inbound, None).unwrap().len(), 2);
    assert_eq!(execute(&engine, &undirected, None).unwrap().len(), 2);
    assert_eq!(execute(&engine, &var_len, None).unwrap().len(), 2);
}

#[test]
fn projection_orders_skips_and_limits_rows() {
    let logical = bind_query(
        "MATCH (u:users)-[:works_at]->(c:companies) \
         RETURN u.name AS employee ORDER BY employee DESC SKIP 1 LIMIT 1",
    );
    let physical = lower(logical);
    let engine = engine_fixture();
    let rows = execute(&engine, &physical, None).unwrap();
    let hydrated = hydrated_fixture();

    let projected = project_rows(rows, &physical, &hydrated, &QueryParams::new(), true).unwrap();

    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["employee"], "Ada");
}

fn hydrated_fixture() -> HydratedRows {
    HashMap::from([
        (
            (10, "u1".to_string()),
            serde_json::json!({
                "id": "u1",
                "name": "Ada",
                "age": 37,
                "profile": {
                    "plan": "pro",
                    "tags": ["founder", 7],
                    "flags": {"beta": true},
                    "explicit_null": null
                }
            }),
        ),
        (
            (10, "u2".to_string()),
            serde_json::json!({
                "id": "u2",
                "name": "Linus",
                "age": 41,
                "profile": {
                    "plan": "free",
                    "tags": ["kernel"],
                    "flags": {"beta": false}
                }
            }),
        ),
        (
            (20, "c1".to_string()),
            serde_json::json!({"id": "c1", "name": "Acme"}),
        ),
        (
            (20, "c2".to_string()),
            serde_json::json!({"id": "c2", "name": "Bell"}),
        ),
    ])
}

fn engine_fixture() -> Engine {
    let mut engine = Engine::new();
    for (oid, pk) in [(10, "u1"), (10, "u2"), (20, "c1"), (20, "c2")] {
        let node_idx = engine.node_store.add_node(oid, pk.to_string());
        engine.resolution_insert(oid, pk, node_idx);
        engine.insert_table_membership(oid, node_idx);
    }
    let works_at = engine.register_edge_type("works_at").unwrap();
    engine.edge_store = EdgeStore::from_edges(
        engine.node_store.node_count(),
        vec![
            RawEdge {
                source: 0,
                target: 2,
                type_id: works_at,
                weight: None,
            },
            RawEdge {
                source: 1,
                target: 3,
                type_id: works_at,
                weight: None,
            },
        ],
        false,
    );
    engine.reverse_edge_store = engine.edge_store.reversed();
    engine.built = true;
    engine
}
