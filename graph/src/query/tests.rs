use super::catalog_snapshot::FakeCatalog;
use super::execute::{execute, execute_node_scan};
use super::explain::explain;
use super::lower::{lower, lower_statement};
use super::physical_plan::ReturnSlot;
use super::semantics::bind;
use super::semantics::bind_statement;
use super::value::{project_node_rows, project_rows, HydratedRows, QueryParams};
use crate::edge_store::{EdgeStore, RawEdge};
use crate::engine::Engine;
use crate::gql::errors::GqlErrorKind;
use crate::gql::parse;
use crate::safety::GraphError;
use std::collections::HashMap;

fn fake_catalog() -> FakeCatalog {
    FakeCatalog::new()
        .with_writable_label("users", 10, ["id", "name"], ["name"])
        .with_writable_label("companies", 20, ["id", "name"], ["name"])
        .with_edge("works_at", 10, 20)
        .with_mapped_edge("friend", 10, 10, 30, "user_id", "friend_id", false)
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

    assert_eq!(plan.source.var, "u");
    assert_eq!(plan.source.table_oid, 10);
    assert_eq!(plan.relationship.rel_type, "works_at");
    assert_eq!(plan.target.var, "c");
    assert_eq!(plan.target.table_oid, 20);
    assert_eq!(plan.returns.len(), 2);
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
        "MATCH (u:users)-[:works_at]->(c:companies) RETURN count(u)",
        "MATCH (u:users)-[:works_at]->(c:companies) RETURN DISTINCT u",
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
fn binder_rejects_variable_length_relationship_return() {
    let ast = parse("MATCH (u:users)-[r:works_at*1..2]->(c:companies) RETURN r").unwrap();
    let err = bind(&ast, &fake_catalog()).unwrap_err();

    assert!(matches!(err.kind, GqlErrorKind::Unsupported { .. }));
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
    assert_eq!(rows[0].target.table_oid, 20);
    assert_eq!(rows[0].target.node_id, "c1");
    assert_eq!(rows[1].source.node_id, "u2");
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
    assert_eq!(rows[0].target.node_id, "c1");
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
    assert_eq!(rows[0].target.node_id, "c2");
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
        "Expand(source=u:10, rel=works_at, hops=1..1, target=c:20, return=[u, c])"
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
            serde_json::json!({"id": "u1", "name": "Ada"}),
        ),
        (
            (10, "u2".to_string()),
            serde_json::json!({"id": "u2", "name": "Linus"}),
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
