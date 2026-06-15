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
    let (tables, _edges, discoveries) =
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
    let cross_tenant_rows = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_targeted_accounts_pgtest'::regclass,
                'a1',
                2,
                tenant := 'tenant-a',
                hydrate := false
             )
             WHERE node_id LIKE 'b%'",
    )
    .expect("tenant traverse failed")
    .unwrap_or(-1);

    Spi::run("RESET graph.enforce_tenant_scope").expect("reset tenant enforcement failed");

    assert_eq!(tenant_columns, vec!["account_id", "account_id"]);
    assert!(missing_tenant_rejected);
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
        Some("PG018".to_string())
    );
}
