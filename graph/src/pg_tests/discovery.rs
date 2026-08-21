#[pg_test]
fn auto_discover_builds_and_includes_composite_pk_entity_tables() {
    reset_and_create_fixtures();

    let (build_rows, users_seen, friendships_seen, composite_seen) = Spi::connect(|client| {
            let result = client
                .select(
                    "WITH discovery AS (
                        SELECT * FROM graph.auto_discover('public')
                    )
                    SELECT
                        (SELECT count(*) FROM discovery WHERE item_type = 'build'),
                        (SELECT count(*) FROM discovery WHERE item_type = 'table' AND item_name = 'graph_test_users_pgtest'),
                        (SELECT count(*) FROM discovery WHERE item_type = 'table' AND item_name = 'graph_test_friendships_pgtest'),
                        (SELECT count(*) FROM discovery WHERE item_type = 'table' AND item_name = 'graph_test_composite_pgtest')",
                    None,
                    &[],
                )
                .expect("auto_discover query failed");
            let row = result.first();
            Ok::<_, pgrx::spi::Error>(
                (
                    row.get::<i64>(1).expect("build_rows read failed").unwrap_or(0),
                    row.get::<i64>(2).expect("users_seen read failed").unwrap_or(0),
                    row.get::<i64>(3)
                        .expect("friendships_seen read failed")
                        .unwrap_or(0),
                    row.get::<i64>(4)
                        .expect("composite_seen read failed")
                        .unwrap_or(0),
                ),
            )
        })
        .expect("auto_discover row parse failed");

    assert_eq!(build_rows, 1);
    assert_eq!(users_seen, 1);
    assert_eq!(friendships_seen, 1);
    // Composite entity table should now be discovered (not skipped)
    assert_eq!(composite_seen, 1);

    let node_count = Spi::get_one::<i32>("SELECT node_count FROM graph.status()")
        .expect("status query failed")
        .unwrap_or(0);
    let edge_count = Spi::get_one::<i32>("SELECT edge_count FROM graph.status()")
        .expect("edge status query failed")
        .unwrap_or(0);
    assert!(node_count > 0);
    assert!(edge_count > 0);
}

#[pg_test]
fn auto_discover_classifies_junction_tables_as_edges() {
    reset_and_create_fixtures();

    // Create a junction table: composite PK where ALL columns are FKs
    Spi::run(
        "CREATE TABLE public.graph_test_junction_pgtest (
                user_id   TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
                friend_id TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
                PRIMARY KEY (user_id, friend_id)
            )",
    )
    .expect("create junction failed");

    Spi::run(
        "INSERT INTO public.graph_test_junction_pgtest (user_id, friend_id) VALUES ('u1', 'u2')",
    )
    .expect("insert junction failed");

    // Use discover_schema() directly to test classification without triggering build()
    let (tables, edges, discoveries) =
        crate::discover::discover_schema("public").expect("discover_schema failed");

    // Junction table should NOT appear as a registered table (node)
    let junction_as_node = tables
        .iter()
        .any(|t| t.table_name.contains("graph_test_junction_pgtest"));
    assert!(
        !junction_as_node,
        "junction table should not be registered as a node"
    );

    // Junction table should be classified as 'junction' in discoveries
    let junction_discovery = discoveries
        .iter()
        .find(|d| d.item_name == "graph_test_junction_pgtest");
    assert!(
        junction_discovery.is_some(),
        "junction table should appear in discoveries"
    );
    assert_eq!(
        junction_discovery.unwrap().item_type,
        "junction",
        "junction table should have item_type 'junction'"
    );

    let junction_edges = edges
        .iter()
        .filter(|edge| edge.from_table.contains("graph_test_junction_pgtest"))
        .collect::<Vec<_>>();
    assert_eq!(
        junction_edges.len(),
        1,
        "a two-endpoint junction must register as one relationship edge"
    );
    assert_eq!(junction_edges[0].from_column, "user_id");
    assert_eq!(junction_edges[0].to_column, "friend_id");
}

#[pg_test]
fn auto_discover_classifies_typed_surrogate_key_junction_as_dynamic_edge() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name) VALUES ('shared', 'Wrong source');
         CREATE TABLE public.graph_test_relationship_sources_pgtest (
             id TEXT PRIMARY KEY,
             name TEXT NOT NULL
         );
         INSERT INTO public.graph_test_relationship_sources_pgtest VALUES ('shared', 'Right source');
         CREATE TABLE public.graph_test_typed_junction_pgtest (
                id                TEXT PRIMARY KEY,
                source_id         TEXT NOT NULL REFERENCES public.graph_test_relationship_sources_pgtest(id),
                target_id         TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
                relationship_name VARCHAR(255) NOT NULL
            )",
    )
    .expect("create typed junction failed");
    Spi::run(
        "INSERT INTO public.graph_test_typed_junction_pgtest
             (id, source_id, target_id, relationship_name)
         VALUES ('r1', 'shared', 'u2', 'co_authored_with')",
    )
    .expect("insert typed junction failed");

    let (tables, edges, discoveries) =
        crate::discover::discover_schema("public").expect("discover_schema failed");

    assert!(
        !tables
            .iter()
            .any(|table| table.table_name.contains("graph_test_typed_junction_pgtest")),
        "a conventional typed relationship table must not be registered as a node"
    );
    assert!(discoveries.iter().any(|item| {
        item.item_type == "junction" && item.item_name == "graph_test_typed_junction_pgtest"
    }));

    let typed_edges = edges
        .iter()
        .filter(|edge| {
            edge.from_table
                .contains("graph_test_typed_junction_pgtest")
        })
        .collect::<Vec<_>>();
    assert_eq!(typed_edges.len(), 1);
    assert_eq!(typed_edges[0].from_column, "source_id");
    assert_eq!(typed_edges[0].to_column, "target_id");
    assert_eq!(
        typed_edges[0].label_column.as_deref(),
        Some("relationship_name")
    );

    Spi::run(
        "SELECT * FROM graph.auto_discover_tables(
             ARRAY[
                 'graph_test_users_pgtest'::regclass,
                 'graph_test_relationship_sources_pgtest'::regclass,
                 'graph_test_typed_junction_pgtest'::regclass
             ]
         )",
    )
    .expect("targeted typed-junction discovery failed");
    let registered = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1
             FROM graph.registered_edges()
             WHERE from_table LIKE '%graph_test_typed_junction_pgtest'
               AND from_column = 'source_id'
               AND to_column = 'target_id'
               AND label_column = 'relationship_name'
         )",
    )
    .expect("read typed-junction registration failed")
    .unwrap_or(false);
    let projected_type = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1
             FROM graph.edge_types()
             WHERE label = 'co_authored_with'
         )",
    )
    .expect("read discovered dynamic relationship type failed")
    .unwrap_or(false);
    assert!(registered);
    assert!(projected_type);

    let reached_expected_target = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1
             FROM graph.traverse(
                 'graph_test_relationship_sources_pgtest'::regclass,
                 'shared',
                 1,
                 edge_types := ARRAY['co_authored_with'],
                 direction := 'out',
                 include_start := false,
                 hydrate := false
             )
             WHERE node_table = 'graph_test_users_pgtest'::regclass
               AND node_id = 'u2'
         )",
    )
    .expect("traverse discovered typed relationship failed")
    .unwrap_or(false);
    assert!(
        reached_expected_target,
        "build must bind the source endpoint through its declared foreign key"
    );
}

#[pg_test]
fn auto_discover_does_not_invent_relationships_from_composite_or_three_way_fks() {
    reset_and_create_fixtures();
    Spi::run(
        "CREATE TABLE public.graph_test_composite_fk_parent_pgtest (
             tenant_id TEXT NOT NULL,
             id TEXT NOT NULL,
             PRIMARY KEY (tenant_id, id)
         );
         CREATE TABLE public.graph_test_composite_fk_child_pgtest (
             tenant_id TEXT NOT NULL,
             id TEXT NOT NULL,
             PRIMARY KEY (tenant_id, id),
             FOREIGN KEY (tenant_id, id)
                 REFERENCES public.graph_test_composite_fk_parent_pgtest(tenant_id, id)
         );
         CREATE TABLE public.graph_test_three_way_junction_pgtest (
             first_id TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
             second_id TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
             third_id TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
             PRIMARY KEY (first_id, second_id, third_id)
         )",
    )
    .expect("create unsupported relationship shapes failed");

    let (tables, edges, discoveries) =
        crate::discover::discover_schema("public").expect("discover_schema failed");
    assert!(tables.iter().any(|table| {
        table
            .table_name
            .contains("graph_test_composite_fk_child_pgtest")
    }));
    assert!(!edges.iter().any(|edge| {
        edge.from_table
            .contains("graph_test_composite_fk_child_pgtest")
    }));
    assert!(!discoveries.iter().any(|item| {
        item.item_type == "junction"
            && item.item_name == "graph_test_three_way_junction_pgtest"
    }));
    assert_eq!(
        edges
            .iter()
            .filter(|edge| {
                edge.from_table
                    .contains("graph_test_three_way_junction_pgtest")
            })
            .count(),
        3,
        "three-way tables retain ordinary FK discovery instead of an invented binary mapping"
    );
}

#[pg_test]
fn auto_discover_rejects_ambiguous_or_non_primary_endpoint_inference() {
    reset_and_create_fixtures();
    Spi::run(
        "CREATE TABLE public.graph_test_duplicate_endpoint_pgtest (
             id TEXT PRIMARY KEY
         );
         INSERT INTO public.graph_test_duplicate_endpoint_pgtest VALUES ('u1');
         CREATE TABLE public.graph_test_ambiguous_typed_edge_pgtest (
             id TEXT PRIMARY KEY,
             endpoint_id TEXT NOT NULL,
             relationship_name TEXT NOT NULL,
             FOREIGN KEY (endpoint_id) REFERENCES public.graph_test_users_pgtest(id),
             FOREIGN KEY (endpoint_id) REFERENCES public.graph_test_duplicate_endpoint_pgtest(id)
         );
         CREATE TABLE public.graph_test_alternate_key_node_pgtest (
             id TEXT PRIMARY KEY,
             external_key TEXT NOT NULL UNIQUE
         );
         CREATE TABLE public.graph_test_alternate_key_edge_pgtest (
             id TEXT PRIMARY KEY,
             source_external_key TEXT NOT NULL
                 REFERENCES public.graph_test_alternate_key_node_pgtest(external_key),
             target_id TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
             relationship_name TEXT NOT NULL
         )",
    )
    .expect("create ambiguous endpoint fixtures failed");

    let (tables, edges, discoveries) =
        crate::discover::discover_schema("public").expect("discover_schema failed");
    for table_name in [
        "graph_test_ambiguous_typed_edge_pgtest",
        "graph_test_alternate_key_edge_pgtest",
    ] {
        assert!(
            tables
                .iter()
                .any(|table| table.table_name.contains(table_name)),
            "{table_name} must remain a node/manual-registration shape"
        );
        assert!(!discoveries
            .iter()
            .any(|item| item.item_type == "junction" && item.item_name == table_name));
        assert!(!edges.iter().any(|edge| {
            edge.from_table.contains(table_name) && edge.label_column.is_some()
        }));
    }
    assert!(!edges.iter().any(|edge| {
        edge.from_table
            .contains("graph_test_alternate_key_edge_pgtest")
            && edge.from_column == "source_external_key"
    }));
}

#[pg_test]
fn auto_discover_tables_registers_only_selected_tables_and_edges() {
    reset_and_create_fixtures();

    Spi::run(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY[
                    'graph_test_users_pgtest'::regclass,
                    'graph_test_bad_pgtest'::regclass
                ]
            )",
    )
    .expect("targeted discovery failed");

    let table_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_tables()")
        .expect("registered table count failed")
        .unwrap_or(0);
    let friendship_registered = Spi::get_one::<bool>(
        "SELECT EXISTS (
                SELECT 1
                FROM graph.registered_tables()
                WHERE table_name LIKE '%graph_test_friendships_pgtest'
            )",
    )
    .expect("friendship registration query failed")
    .unwrap_or(true);
    let edge_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_edges()")
        .expect("registered edge count failed")
        .unwrap_or(-1);

    assert_eq!(table_count, 2);
    assert!(!friendship_registered);
    assert_eq!(edge_count, 0);
}

#[pg_test]
fn auto_discover_tables_discovers_fk_edges_inside_selected_set() {
    reset_and_create_fixtures();

    Spi::run(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY[
                    'graph_test_users_pgtest'::regclass,
                    'graph_test_friendships_pgtest'::regclass
                ]
            )",
    )
    .expect("targeted discovery failed");

    let edge_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_edges()")
        .expect("registered edge count failed")
        .unwrap_or(0);
    let bad_registered = Spi::get_one::<bool>(
        "SELECT EXISTS (
                SELECT 1
                FROM graph.registered_tables()
                WHERE table_name LIKE '%graph_test_bad_pgtest'
            )",
    )
    .expect("bad table registration query failed")
    .unwrap_or(true);
    let node_count = Spi::get_one::<i32>("SELECT node_count FROM graph.status()")
        .expect("status query failed")
        .unwrap_or(0);

    assert_eq!(edge_count, 2);
    assert!(!bad_registered);
    assert!(node_count > 0);
}

#[pg_test]
fn auto_discover_tables_handles_composite_entities_and_junctions() {
    reset_and_create_fixtures();
    Spi::run(
        "CREATE TABLE public.graph_test_junction_pgtest (
                user_id   TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
                friend_id TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
                PRIMARY KEY (user_id, friend_id)
            )",
    )
    .expect("create junction failed");
    Spi::run(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY[
                    'graph_test_users_pgtest'::regclass,
                    'graph_test_composite_pgtest'::regclass,
                    'graph_test_junction_pgtest'::regclass
                ]
            )",
    )
    .expect("targeted discovery failed");

    let node_count = Spi::get_one::<i32>(
        "SELECT node_count
             FROM graph.status()",
    )
    .expect("composite registration query failed")
    .unwrap_or(0);
    let junction_registered = Spi::get_one::<bool>(
        "SELECT EXISTS (
                SELECT 1
                FROM graph.registered_tables()
                WHERE table_name LIKE '%graph_test_junction_pgtest'
            )",
    )
    .expect("junction registration query failed")
    .unwrap_or(true);
    let edge_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_edges()")
        .expect("registered edge count failed")
        .unwrap_or(0);

    assert!(node_count >= 4);
    assert!(!junction_registered);
    assert_eq!(edge_count, 1);
}

#[pg_test]
fn auto_discover_tables_is_idempotent() {
    reset_and_create_fixtures();
    let statement = "SELECT * FROM graph.auto_discover_tables(
            ARRAY[
                'graph_test_users_pgtest'::regclass,
                'graph_test_friendships_pgtest'::regclass
            ]
        )";

    Spi::run(statement).expect("first targeted discovery failed");
    Spi::run(statement).expect("second targeted discovery failed");

    let table_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_tables()")
        .expect("registered table count failed")
        .unwrap_or(0);
    let edge_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_edges()")
        .expect("registered edge count failed")
        .unwrap_or(0);
    let node_count = Spi::get_one::<i32>("SELECT node_count FROM graph.status()")
        .expect("status query failed")
        .unwrap_or(0);

    assert_eq!(table_count, 2);
    assert_eq!(edge_count, 2);
    assert!(node_count > 0);
}

#[pg_test]
fn auto_discover_tables_rejects_invalid_inputs() {
    reset_and_create_fixtures();
    Spi::run("DROP VIEW IF EXISTS public.graph_test_view_pgtest").expect("drop view failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_no_key_pgtest")
        .expect("drop no-key table failed");
    Spi::run(
        "CREATE VIEW public.graph_test_view_pgtest AS
             SELECT id, name FROM public.graph_test_users_pgtest",
    )
    .expect("create view failed");
    Spi::run(
        "CREATE TABLE public.graph_test_no_key_pgtest (
                id TEXT,
                note TEXT
            )",
    )
    .expect("create no-key table failed");

    assert!(sql_raises(
        "SELECT * FROM graph.auto_discover_tables(ARRAY[]::regclass[])"
    ));
    assert!(sql_raises(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY[
                    'graph_test_users_pgtest'::regclass,
                    'graph_test_users_pgtest'::regclass
                ]
            )"
    ));
    assert!(sql_raises(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY['graph_test_view_pgtest'::regclass]
            )"
    ));
    assert!(sql_raises(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY['graph_test_no_key_pgtest'::regclass]
            )"
    ));
    assert!(sql_raises(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY['graph_test_users_pgtest'::regclass],
                tenant_column := 'missing_tenant'
            )"
    ));
}

#[pg_test]
fn auto_discover_tables_stores_shared_tenant_column_and_enforces_scope() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.enforce_tenant_scope = on").expect("enable tenant enforcement failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_targeted_orders_pgtest CASCADE")
        .expect("drop targeted orders failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_targeted_accounts_pgtest CASCADE")
        .expect("drop targeted accounts failed");
    Spi::run(
        "CREATE TABLE public.graph_test_targeted_accounts_pgtest (
                id TEXT PRIMARY KEY,
                account_id TEXT NOT NULL,
                name TEXT NOT NULL
            )",
    )
    .expect("create targeted accounts failed");
    Spi::run(
        "CREATE TABLE public.graph_test_targeted_orders_pgtest (
                id TEXT PRIMARY KEY,
                account_id TEXT NOT NULL,
                account_ref TEXT NOT NULL REFERENCES public.graph_test_targeted_accounts_pgtest(id),
                note TEXT NOT NULL
            )",
    )
    .expect("create targeted orders failed");
    Spi::run(
        "INSERT INTO public.graph_test_targeted_accounts_pgtest VALUES
                ('a1', 'tenant-a', 'Account A'),
                ('b1', 'tenant-b', 'Account B')",
    )
    .expect("insert targeted accounts failed");
    Spi::run(
        "INSERT INTO public.graph_test_targeted_orders_pgtest VALUES
                ('o1', 'tenant-a', 'a1', 'Order A'),
                ('o2', 'tenant-b', 'b1', 'Order B')",
    )
    .expect("insert targeted orders failed");
    Spi::run(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY[
                    'graph_test_targeted_accounts_pgtest'::regclass,
                    'graph_test_targeted_orders_pgtest'::regclass
                ],
                tenant_column := 'account_id'
            )",
    )
    .expect("targeted tenant discovery failed");

    let tenant_columns = Spi::get_one::<Vec<String>>(
        "SELECT array_agg(tenant_column ORDER BY table_name)
             FROM graph.registered_tables()",
    )
    .expect("tenant column query failed")
    .unwrap_or_default();
    let missing_tenant_rejected = sql_raises(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_targeted_accounts_pgtest'::regclass,
                'a1',
                2,
                hydrate := false
             )",
    );
    // An explicit tenant argument is no longer accepted for a
    // tenant_column-registered graph under enforcement: only the trusted
    // session-setting path may supply the tenant, since a caller-supplied
    // SQL argument is not verified against the calling role's identity.
    let explicit_tenant_argument_rejected = sql_raises(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_targeted_accounts_pgtest'::regclass,
                'a1',
                2,
                tenant := 'tenant-a',
                hydrate := false
             )",
    );

    Spi::run("SET graph.tenant_setting = 'graph_test.tenant'")
        .expect("set tenant_setting name failed");
    Spi::run("SET graph_test.tenant = 'tenant-a'").expect("set session tenant failed");
    let cross_tenant_rows = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_targeted_accounts_pgtest'::regclass,
                'a1',
                2,
                hydrate := false
             )
             WHERE node_id LIKE 'b%'",
    )
    .expect("session-tenant traverse failed")
    .unwrap_or(-1);

    Spi::run("RESET graph_test.tenant").expect("reset session tenant failed");
    Spi::run("RESET graph.tenant_setting").expect("reset tenant_setting name failed");
    Spi::run("RESET graph.enforce_tenant_scope").expect("reset tenant enforcement failed");

    assert_eq!(tenant_columns, vec!["account_id", "account_id"]);
    assert!(missing_tenant_rejected);
    assert!(explicit_tenant_argument_rejected);
    assert_eq!(cross_tenant_rows, 0);
}

#[pg_test]
fn preview_discover_tables_writes_no_registration_rows() {
    reset_and_create_fixtures();

    let preview_rows = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.preview_discover_tables(
                ARRAY[
                    'graph_test_users_pgtest'::regclass,
                    'graph_test_friendships_pgtest'::regclass
                ]
           )",
    )
    .expect("preview discovery failed")
    .unwrap_or(0);
    let table_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_tables()")
        .expect("registered table count failed")
        .unwrap_or(-1);
    let edge_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_edges()")
        .expect("registered edge count failed")
        .unwrap_or(-1);

    assert!(preview_rows > 0);
    assert_eq!(table_count, 0);
    assert_eq!(edge_count, 0);
}

#[pg_test]
fn auto_discover_tables_into_named_graph_does_not_mutate_default_graph() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('discover_a', namespace := 'app')")
        .expect("create discover_a failed");
    Spi::run("SELECT graph.create_graph('discover_b', namespace := 'app')")
        .expect("create discover_b failed");

    Spi::run(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY[
                    'graph_test_users_pgtest'::regclass,
                    'graph_test_friendships_pgtest'::regclass
                ],
                graph_name := 'discover_a',
                graph_namespace := 'app',
                build := false
            )",
    )
    .expect("discover_a targeted discovery failed");
    Spi::run(
        "SELECT * FROM graph.auto_discover_tables(
                ARRAY[
                    'graph_test_users_pgtest'::regclass,
                    'graph_test_bad_pgtest'::regclass
                ],
                graph_name := 'discover_b',
                graph_namespace := 'app',
                build := false
            )",
    )
    .expect("discover_b targeted discovery failed");

    let default_tables = Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_tables()")
        .expect("default table count failed")
        .unwrap_or(-1);
    let discover_a_edges = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.registered_edges_for_graph('discover_a', graph_namespace := 'app')",
    )
    .expect("discover_a edge count failed")
    .unwrap_or(-1);
    let discover_b_edges = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.registered_edges_for_graph('discover_b', graph_namespace := 'app')",
    )
    .expect("discover_b edge count failed")
    .unwrap_or(-1);
    let discover_b_tables = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.registered_tables_for_graph('discover_b', graph_namespace := 'app')",
    )
    .expect("discover_b table count failed")
    .unwrap_or(-1);

    assert_eq!(default_tables, 0);
    assert_eq!(discover_a_edges, 2);
    assert_eq!(discover_b_edges, 0);
    assert_eq!(discover_b_tables, 2);
}

#[pg_test]
fn auto_discover_tables_builds_target_named_graph() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('discover_build', namespace := 'app')")
        .expect("create discover_build failed");

    let build_rows = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.auto_discover_tables(
                ARRAY[
                    'graph_test_users_pgtest'::regclass,
                    'graph_test_friendships_pgtest'::regclass
                ],
                graph_name := 'discover_build',
                graph_namespace := 'app',
                build := true
           )
          WHERE item_type = 'build'",
    )
    .expect("targeted discovery build failed")
    .unwrap_or(0);
    let current_graph = Spi::get_one::<String>("SELECT graph_name FROM graph.current_graph()")
        .expect("current graph query failed")
        .expect("current graph row missing");
    let node_count = Spi::get_one::<i32>("SELECT node_count FROM graph.status()")
        .expect("targeted graph status failed")
        .unwrap_or(0);
    let edge_count = Spi::get_one::<i32>("SELECT edge_count FROM graph.status()")
        .expect("targeted graph edge status failed")
        .unwrap_or(0);

    assert_eq!(build_rows, 1);
    assert_eq!(current_graph, "discover_build");
    assert!(node_count > 0);
    assert!(edge_count > 0);
}

#[pg_test]
fn row_predicate_subgraphs_are_explicitly_rejected() {
    reset_and_create_fixtures();

    assert_eq!(
        sqlstate_for_error(
            "SELECT graph.create_row_predicate_subgraph(
                'predicate_graph',
                '{\"where\":{\"status\":\"active\"}}'::jsonb
            )"
        ),
        Some("0A000".to_string())
    );
}
