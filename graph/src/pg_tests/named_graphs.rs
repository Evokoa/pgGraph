#[pg_test]
fn default_graph_compatibility_workflow_still_uses_legacy_sql_surface() {
    reset_and_create_fixtures();

    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'age']
            )",
    )
    .expect("add users table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend',
                bidirectional := false
            )",
    )
    .expect("add friendship edge failed");

    let build_loaded_users = Spi::get_one::<bool>(
        "SELECT nodes_loaded = 2
           FROM graph.build()",
    )
    .expect("build result query failed")
    .unwrap_or(false);
    let reaches_bob = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.traverse(
               'graph_test_users_pgtest'::regclass,
               'u1',
               1,
               hydrate := false
           )
          WHERE node_id = 'u2'",
    )
    .expect("traverse query failed")
    .unwrap_or(0);
    let status_node_count = Spi::get_one::<i32>("SELECT node_count FROM graph.status()")
        .expect("status query failed")
        .expect("status row missing");

    assert!(build_loaded_users);
    assert_eq!(reaches_bob, 1);
    assert_eq!(status_node_count, 2);

    Spi::run("SELECT graph.reset()").expect("reset failed");
    assert_eq!(
        sqlstate_for_error(
            "SELECT * FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                hydrate := false
            )"
        ),
        Some("PG003".to_string())
    );
}

#[pg_test]
fn named_graph_policy_defaults_are_single_sourced() {
    assert_eq!(
        crate::graph_policy::DEFAULT_GRAPH_ID_TEXT,
        "00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(crate::graph_policy::DEFAULT_GRAPH_NAME, "default");
    assert_eq!(crate::graph_policy::DEFAULT_GRAPH_NAMESPACE, "public");
    assert!(crate::graph_policy::is_graph_kind("global"));
    assert!(crate::graph_policy::is_graph_kind("tenant"));
    assert!(crate::graph_policy::is_residency_policy("cold"));
    assert!(crate::graph_policy::is_materialization_policy("shared"));
    assert!(crate::graph_policy::is_projection_mode("csr_readonly"));
    assert_eq!(crate::graph_policy::DEFAULT_SCHEDULER_WAKE_INTERVAL_SECS, 60);
    assert_eq!(crate::graph_policy::DEFAULT_SCHEDULER_BATCH_SIZE, 64);
    assert_eq!(crate::graph_policy::DEFAULT_JOB_MAX_ATTEMPTS, 3);
    assert_eq!(crate::graph_policy::DEFAULT_BACKEND_LOADED_GRAPH_LIMIT, 1);
}

#[pg_test]
fn default_graph_catalog_row_is_bootstrapped_once() {
    let default_rows = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph._graphs
          WHERE graph_id = '00000000-0000-0000-0000-000000000001'::uuid
            AND graph_name = 'default'
            AND tenant IS NULL
            AND namespace = 'public'
            AND graph_kind = 'global'
            AND residency = 'hot'
            AND materialization = 'shared'
            AND projection_mode = 'csr_readonly'",
    )
    .expect("default graph catalog query failed")
    .unwrap_or(0);

    assert_eq!(default_rows, 1);
}

#[pg_test]
fn create_graph_enforces_identity_and_policy_values() {
    let created = Spi::get_one::<String>(
        "SELECT graph_name
           FROM graph.create_graph('customer_360', namespace := 'analytics')",
    )
    .expect("create graph failed")
    .expect("create graph row missing");
    assert_eq!(created, "customer_360");

    let same_name_other_namespace = Spi::get_one::<String>(
        "SELECT namespace
           FROM graph.create_graph('customer_360', namespace := 'sandbox')",
    )
    .expect("create same-name graph in another namespace failed")
    .expect("create same-name graph row missing");
    assert_eq!(same_name_other_namespace, "sandbox");

    assert_eq!(
        sqlstate_for_error("SELECT * FROM graph.create_graph('customer_360', namespace := 'analytics')"),
        Some("PG005".to_string())
    );
    assert_eq!(
        sqlstate_for_error("SELECT * FROM graph.create_graph('bad_kind', graph_kind := 'team')"),
        Some("PG005".to_string())
    );
    assert_eq!(
        sqlstate_for_error("SELECT * FROM graph.create_graph('bad_residency', residency := 'always_loaded')"),
        Some("PG005".to_string())
    );
    assert_eq!(
        sqlstate_for_error("SELECT * FROM graph.create_graph('bad_materialization', materialization := 'physical')"),
        Some("PG005".to_string())
    );
    assert_eq!(
        sqlstate_for_error("SELECT * FROM graph.create_graph('bad_projection', projection_mode := 'mutable')"),
        Some("PG005".to_string())
    );
}

#[pg_test]
fn current_graph_selection_is_separate_from_engine_load_state() {
    Spi::run("SELECT graph.create_graph('session_graph', namespace := 'app')")
        .expect("create session graph failed");

    let default_current = Spi::get_one::<String>("SELECT graph_name FROM graph.current_graph()")
        .expect("current default graph failed")
        .expect("current default graph row missing");
    assert_eq!(default_current, "default");

    let selected = Spi::get_one::<String>(
        "SELECT graph_name
           FROM graph.set_current_graph('session_graph', namespace := 'app')",
    )
    .expect("set current graph failed")
    .expect("selected graph row missing");
    assert_eq!(selected, "session_graph");

    let current = Spi::get_one::<String>("SELECT graph_name FROM graph.current_graph()")
        .expect("current selected graph failed")
        .expect("current selected graph row missing");
    assert_eq!(current, "session_graph");

    assert_eq!(
        sqlstate_for_error("SELECT * FROM graph.set_current_graph('missing_graph')"),
        Some("PG005".to_string())
    );
}

#[pg_test]
fn graph_catalog_mutation_requires_admin_privileges() {
    Spi::run("DROP ROLE IF EXISTS graph_named_graph_no_admin").expect("drop role failed");
    Spi::run("CREATE ROLE graph_named_graph_no_admin").expect("create role failed");
    Spi::run("GRANT USAGE ON SCHEMA graph, public TO graph_named_graph_no_admin")
        .expect("grant schema usage failed");
    create_error_sqlstate_helper();

    Spi::run("SET ROLE graph_named_graph_no_admin").expect("set restricted role failed");
    let create_sqlstate =
        sqlstate_for_prepared_helper("SELECT * FROM graph.create_graph('denied_graph')");
    let selected_default = Spi::get_one::<String>(
        "SELECT graph_name
           FROM graph.set_current_graph('default')",
    )
    .expect("restricted role default graph selection failed")
    .expect("restricted role default graph row missing");
    let direct_write_sqlstate = sqlstate_for_prepared_helper(
        "INSERT INTO graph._graphs (
             graph_id,
             graph_name,
             owner_role,
             created_by,
             namespace,
             graph_kind,
             residency,
             materialization,
             projection_mode
         )
         VALUES (
             '00000000-0000-0000-0000-000000000099'::uuid,
             'direct_denied',
             0::oid,
             0::oid,
             'public',
             'global',
             'hot',
             'shared',
             'csr_readonly'
         )",
    );
    Spi::run("RESET ROLE").expect("reset restricted role failed");

    assert_eq!(create_sqlstate, Some("PG002".to_string()));
    assert_eq!(selected_default, "default");
    assert_eq!(direct_write_sqlstate, Some("42501".to_string()));
}

#[pg_test]
fn graph_scoped_registrations_isolate_tables_edges_and_filters() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('tenant_a', namespace := 'app')")
        .expect("create tenant_a graph failed");
    Spi::run("SELECT graph.create_graph('tenant_b', namespace := 'app')")
        .expect("create tenant_b graph failed");

    Spi::run(
        "SELECT graph.add_table_to_graph(
                'tenant_a',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add users to tenant_a failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'tenant_b',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name', 'age'],
                graph_namespace := 'app'
            )",
    )
    .expect("add users to tenant_b failed");
    Spi::run(
        "SELECT graph.add_filter_column_to_graph(
                'tenant_b',
                'graph_test_users_pgtest'::regclass,
                'age',
                'numeric',
                graph_namespace := 'app'
            )",
    )
    .expect("add tenant_b filter column failed");

    let tenant_a_columns = Spi::get_one::<Vec<String>>(
        "SELECT columns
           FROM graph.registered_tables_for_graph('tenant_a', graph_namespace := 'app')
          WHERE table_name = 'graph_test_users_pgtest'",
    )
    .expect("tenant_a registered tables query failed")
    .expect("tenant_a registered table missing");
    let tenant_b_columns = Spi::get_one::<Vec<String>>(
        "SELECT columns
           FROM graph.registered_tables_for_graph('tenant_b', graph_namespace := 'app')
          WHERE table_name = 'graph_test_users_pgtest'",
    )
    .expect("tenant_b registered tables query failed")
    .expect("tenant_b registered table missing");
    let default_count =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_tables()")
            .expect("default registered tables query failed")
            .unwrap_or(0);
    let tenant_b_filters = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph._registered_filter_columns f
           JOIN graph._graphs g ON g.graph_id = f.graph_id
          WHERE g.graph_name = 'tenant_b'
            AND g.namespace = 'app'
            AND f.column_name = 'age'",
    )
    .expect("tenant_b filters query failed")
    .unwrap_or(0);

    assert_eq!(tenant_a_columns, vec!["name".to_string()]);
    assert_eq!(tenant_b_columns, vec!["name".to_string(), "age".to_string()]);
    assert_eq!(default_count, 0);
    assert_eq!(tenant_b_filters, 1);

    Spi::run(
        "SELECT graph.add_edge_to_graph(
                'tenant_a',
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend',
                false,
                graph_namespace := 'app'
            )",
    )
    .expect("add tenant_a edge failed");
    Spi::run(
        "SELECT graph.add_edge_to_graph(
                'tenant_b',
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend',
                false,
                graph_namespace := 'app'
            )",
    )
    .expect("add tenant_b edge failed");
    Spi::run(
        "SELECT graph.remove_table_from_graph(
                'tenant_a',
                'graph_test_users_pgtest'::regclass,
                graph_namespace := 'app'
            )",
    )
    .expect("remove tenant_a table failed");

    let tenant_a_tables =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_tables_for_graph('tenant_a', graph_namespace := 'app')")
            .expect("tenant_a table count failed")
            .unwrap_or(0);
    let tenant_b_tables =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_tables_for_graph('tenant_b', graph_namespace := 'app')")
            .expect("tenant_b table count failed")
            .unwrap_or(0);
    let tenant_b_edges =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_edges_for_graph('tenant_b', graph_namespace := 'app')")
            .expect("tenant_b edge count failed")
            .unwrap_or(0);

    assert_eq!(tenant_a_tables, 0);
    assert_eq!(tenant_b_tables, 1);
    assert_eq!(tenant_b_edges, 1);
}

#[pg_test]
fn selected_graph_legacy_registration_builds_and_queries_named_graph() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('selected_build', namespace := 'app')")
        .expect("create selected_build graph failed");
    Spi::run("SELECT graph.set_current_graph('selected_build', namespace := 'app')")
        .expect("select named graph failed");

    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'age']
            )",
    )
    .expect("add selected graph table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend',
                bidirectional := false
            )",
    )
    .expect("add selected graph edge failed");

    let selected_table_count =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.registered_tables()")
            .expect("selected registered tables query failed")
            .unwrap_or(0);
    let default_table_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph._registered_tables
          WHERE graph_id = '00000000-0000-0000-0000-000000000001'::uuid",
    )
    .expect("default registration count failed")
    .unwrap_or(0);
    let build_loaded_users = Spi::get_one::<bool>("SELECT nodes_loaded = 2 FROM graph.build()")
        .expect("selected graph build failed")
        .unwrap_or(false);
    let reaches_bob = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.traverse(
               'graph_test_users_pgtest'::regclass,
               'u1',
               1,
               hydrate := false
           )
          WHERE node_id = 'u2'",
    )
    .expect("selected graph traverse failed")
    .unwrap_or(0);

    assert_eq!(selected_table_count, 1);
    assert_eq!(default_table_count, 0);
    assert!(build_loaded_users);
    assert_eq!(reaches_bob, 1);
}

#[pg_test]
fn selected_graph_guc_cannot_expose_another_roles_graph() {
    reset_and_create_fixtures();
    Spi::run("DROP ROLE IF EXISTS graph_named_graph_spoof").expect("drop spoof role failed");
    Spi::run("CREATE ROLE graph_named_graph_spoof").expect("create spoof role failed");
    Spi::run("GRANT USAGE ON SCHEMA graph, public TO graph_named_graph_spoof")
        .expect("grant spoof role schema usage failed");
    Spi::run("SELECT graph.create_graph('private_graph', namespace := 'app')")
        .expect("create private graph failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'private_graph',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add private graph table failed");
    let private_graph_id = Spi::get_one::<String>(
        "SELECT graph_id::text
           FROM graph._graphs
          WHERE graph_name = 'private_graph'
            AND namespace = 'app'",
    )
    .expect("private graph id lookup failed")
    .expect("private graph id missing");
    create_error_sqlstate_helper();

    Spi::run("SET ROLE graph_named_graph_spoof").expect("set spoof role failed");
    Spi::run(&format!(
        "SET graph.current_graph_id = {}",
        super::sql_literal(&private_graph_id)
    ))
    .expect("set spoofed current graph id failed");
    let current_graph_sqlstate = sqlstate_for_prepared_helper("SELECT * FROM graph.current_graph()");
    let registered_tables_sqlstate =
        sqlstate_for_prepared_helper("SELECT * FROM graph.registered_tables()");
    Spi::run("RESET ROLE").expect("reset spoof role failed");

    assert_eq!(current_graph_sqlstate, Some("PG005".to_string()));
    assert_eq!(registered_tables_sqlstate, Some("PG005".to_string()));
}

#[pg_test]
fn drop_graph_rejects_non_empty_graph_with_pggraph_sqlstate() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('non_empty_graph', namespace := 'app')")
        .expect("create non_empty graph failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'non_empty_graph',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add non_empty graph table failed");

    assert_eq!(
        sqlstate_for_error("SELECT * FROM graph.drop_graph('non_empty_graph', namespace := 'app')"),
        Some("PG005".to_string())
    );
}

fn sqlstate_for_prepared_helper(statement: &str) -> Option<String> {
    Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sqlstate({})",
        super::sql_literal(statement)
    ))
    .expect("prepared SQLSTATE helper failed")
}
