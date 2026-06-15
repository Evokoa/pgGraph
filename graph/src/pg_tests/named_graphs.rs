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
fn graph_grants_gate_visibility_queries_and_builds() {
    reset_and_create_fixtures();
    create_error_sqlstate_helper();
    Spi::run(
        "DROP ROLE IF EXISTS graph_phase7_reader;
         DROP ROLE IF EXISTS graph_phase7_no_graph;
         DROP ROLE IF EXISTS graph_phase7_no_source;
         DROP ROLE IF EXISTS graph_phase7_builder;
         CREATE ROLE graph_phase7_reader;
         CREATE ROLE graph_phase7_no_graph;
         CREATE ROLE graph_phase7_no_source;
         CREATE ROLE graph_phase7_builder;
         GRANT USAGE ON SCHEMA graph, public TO
             graph_phase7_reader,
             graph_phase7_no_graph,
             graph_phase7_no_source,
             graph_phase7_builder;
         REVOKE SELECT ON public.graph_test_users_pgtest FROM PUBLIC;
         GRANT SELECT ON public.graph_test_users_pgtest TO
             graph_phase7_reader,
             graph_phase7_no_graph,
             graph_phase7_builder",
    )
    .expect("create phase7 roles and grants failed");
    Spi::run("SELECT graph.create_graph('secure_graph', namespace := 'app')")
        .expect("create secure graph failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'secure_graph',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add secure graph table failed");
    Spi::run(
        "SELECT graph.grant_graph('secure_graph', 'graph_phase7_reader', 'read', namespace := 'app');
         SELECT graph.grant_graph('secure_graph', 'graph_phase7_no_source', 'read', namespace := 'app');
         SELECT graph.grant_graph('secure_graph', 'graph_phase7_builder', 'build', namespace := 'app')",
    )
    .expect("grant graph privileges failed");
    let owner_grant_rows = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.graph_privileges('secure_graph', namespace := 'app')
          WHERE privilege IN ('read', 'build')",
    )
    .expect("owner graph_privileges failed")
    .unwrap_or(0);

    Spi::run("SET ROLE graph_phase7_builder").expect("set builder role failed");
    let builder_nodes = Spi::get_one::<i64>(
        "SELECT nodes_loaded
           FROM graph.build_graph('secure_graph', graph_namespace := 'app')",
    )
    .expect("builder build_graph failed")
    .unwrap_or(0);
    Spi::run("RESET ROLE").expect("reset builder role failed");

    Spi::run("SET ROLE graph_phase7_reader").expect("set reader role failed");
    let reader_current = Spi::get_one::<String>(
        "SELECT graph_name
           FROM graph.set_current_graph('secure_graph', namespace := 'app')",
    )
    .expect("reader set_current_graph failed")
    .expect("reader selected graph missing");
    let reader_nodes = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.traverse(
               'graph_test_users_pgtest'::regclass,
               'u1',
               1,
               hydrate := true
           )",
    )
    .expect("reader traverse failed")
    .unwrap_or(0);
    Spi::run("RESET ROLE").expect("reset reader role failed");

    Spi::run("SET ROLE graph_phase7_no_graph").expect("set no_graph role failed");
    let no_graph_sqlstate =
        sqlstate_for_prepared_helper("SELECT * FROM graph.set_current_graph('secure_graph', namespace := 'app')");
    Spi::run("RESET ROLE").expect("reset no_graph role failed");

    Spi::run("SET ROLE graph_phase7_no_source").expect("set no_source role failed");
    let no_source_current = Spi::get_one::<String>(
        "SELECT graph_name
           FROM graph.set_current_graph('secure_graph', namespace := 'app')",
    )
    .expect("no_source set_current_graph failed")
    .expect("no_source selected graph missing");
    let no_source_sqlstate = sqlstate_for_prepared_helper(
        "SELECT * FROM graph.traverse(
            'graph_test_users_pgtest'::regclass,
            'u1',
            1,
            hydrate := true
        )",
    );
    Spi::run("RESET ROLE").expect("reset no_source role failed");

    assert_eq!(owner_grant_rows, 3);
    assert_eq!(reader_current, "secure_graph");
    assert!(reader_nodes >= 1);
    assert_eq!(no_graph_sqlstate, Some("PG005".to_string()));
    assert_eq!(no_source_current, "secure_graph");
    assert_eq!(no_source_sqlstate, Some("PG002".to_string()));
    assert_eq!(builder_nodes, 2);
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

#[pg_test]
fn durable_jobs_are_attributed_to_selected_graph() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('job_graph', namespace := 'app')")
        .expect("create job graph failed");
    Spi::run("SELECT graph.set_current_graph('job_graph', namespace := 'app')")
        .expect("select job graph failed");

    let build_id = Spi::get_one::<String>(
        "SELECT build_id
           FROM graph.build(concurrently := true)",
    )
    .expect("queue build job failed")
    .expect("build job row missing");
    let maintenance_id = Spi::get_one::<String>(
        "SELECT job_id
           FROM graph.maintenance(concurrently := true)",
    )
    .expect("queue maintenance job failed")
    .expect("maintenance job row missing");

    let build_graph = Spi::get_one::<String>(&format!(
        "SELECT g.graph_name
           FROM graph._build_jobs b
           JOIN graph._graphs g ON g.graph_id = b.graph_id
          WHERE b.build_id = {}",
        super::sql_literal(&build_id)
    ))
    .expect("build graph lookup failed")
    .expect("build graph missing");
    let maintenance_graph = Spi::get_one::<String>(&format!(
        "SELECT g.graph_name
           FROM graph._maintenance_jobs m
           JOIN graph._graphs g ON g.graph_id = m.graph_id
          WHERE m.job_id = {}",
        super::sql_literal(&maintenance_id)
    ))
    .expect("maintenance graph lookup failed")
    .expect("maintenance graph missing");

    assert_eq!(build_graph, "job_graph");
    assert_eq!(maintenance_graph, "job_graph");

    let graph_build_status_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.build_status_for_graph('job_graph', graph_namespace := 'app')
          WHERE build_id IS NOT NULL
            AND graph_name = 'job_graph'",
    )
    .expect("graph build status query failed")
    .unwrap_or(0);
    let graph_maintenance_status_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.maintenance_status_for_graph('job_graph', graph_namespace := 'app')
          WHERE job_id IS NOT NULL
            AND graph_name = 'job_graph'",
    )
    .expect("graph maintenance status query failed")
    .unwrap_or(0);

    assert_eq!(graph_build_status_count, 1);
    assert_eq!(graph_maintenance_status_count, 1);
}

#[pg_test]
fn build_graph_uses_named_graph_catalog() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('build_a', namespace := 'app')")
        .expect("create build_a failed");
    Spi::run("SELECT graph.create_graph('build_b', namespace := 'app')")
        .expect("create build_b failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'build_a',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add build_a table failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'build_b',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add build_b users table failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'build_b',
                'graph_test_bad_pgtest'::regclass,
                'id',
                ARRAY['note'],
                graph_namespace := 'app'
            )",
    )
    .expect("add build_b bad table failed");
    Spi::run("INSERT INTO public.graph_test_bad_pgtest (id, note) VALUES ('b1', 'extra')")
        .expect("insert build_b extra node failed");

    let build_a_nodes = Spi::get_one::<i64>(
        "SELECT nodes_loaded
           FROM graph.build_graph('build_a', graph_namespace := 'app')",
    )
    .expect("build_a failed")
    .unwrap_or(0);
    let build_b_nodes = Spi::get_one::<i64>(
        "SELECT nodes_loaded
           FROM graph.build_graph('build_b', graph_namespace := 'app')",
    )
    .expect("build_b failed")
    .unwrap_or(0);
    let current_graph = Spi::get_one::<String>("SELECT graph_name FROM graph.current_graph()")
        .expect("current graph query failed")
        .expect("current graph missing");

    assert_eq!(build_a_nodes, 2);
    assert_eq!(build_b_nodes, 3);
    assert_eq!(current_graph, "build_b");
}

#[pg_test]
fn persisted_named_graphs_use_distinct_artifact_roots() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('persist_a', namespace := 'app')")
        .expect("create persist_a failed");
    Spi::run("SELECT graph.create_graph('persist_b', namespace := 'app')")
        .expect("create persist_b failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'persist_a',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add persist_a table failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'persist_b',
                'graph_test_bad_pgtest'::regclass,
                'id',
                ARRAY['note'],
                graph_namespace := 'app'
            )",
    )
    .expect("add persist_b table failed");
    Spi::run("INSERT INTO public.graph_test_bad_pgtest (id, note) VALUES ('p1', 'persisted')")
        .expect("insert persist_b row failed");

    Spi::run("SELECT graph.build_graph('persist_a', force_persist := true, graph_namespace := 'app')")
        .expect("persist_a build failed");
    Spi::run("SELECT graph.build_graph('persist_b', force_persist := true, graph_namespace := 'app')")
        .expect("persist_b build failed");

    let graph_a = Spi::get_one::<String>(
        "SELECT graph_id::text
           FROM graph._graphs
          WHERE graph_name = 'persist_a'
            AND namespace = 'app'",
    )
    .expect("persist_a graph id query failed")
    .expect("persist_a graph id missing");
    let graph_b = Spi::get_one::<String>(
        "SELECT graph_id::text
           FROM graph._graphs
          WHERE graph_name = 'persist_b'
            AND namespace = 'app'",
    )
    .expect("persist_b graph id query failed")
    .expect("persist_b graph id missing");
    let path_a = crate::persistence::graph_file_path_for(&graph_a).expect("persist_a path failed");
    let path_b = crate::persistence::graph_file_path_for(&graph_b).expect("persist_b path failed");

    assert!(path_a.exists());
    assert!(path_b.exists());
    assert_ne!(path_a, path_b);
    assert_eq!(
        path_a
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str()),
        Some(graph_a.as_str())
    );
    assert_eq!(
        path_b
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str()),
        Some(graph_b.as_str())
    );
}

#[pg_test]
fn projection_generation_heartbeats_are_graph_scoped() {
    Spi::run("SELECT graph.create_graph('heartbeat_a', namespace := 'app')")
        .expect("create heartbeat_a failed");
    Spi::run("SELECT graph.create_graph('heartbeat_b', namespace := 'app')")
        .expect("create heartbeat_b failed");
    Spi::run("DELETE FROM graph._projection_generations WHERE generation_id = 9505001")
        .expect("clear old heartbeat rows failed");

    Spi::run("SELECT graph.set_current_graph('heartbeat_a', namespace := 'app')")
        .expect("select heartbeat_a failed");
    crate::projection::manifest::record_active_generation_heartbeat(
        9_505_001,
        std::time::Duration::from_secs(300),
        10,
        crate::projection::manifest::VALIDATION_STATUS_VALID,
    )
    .expect("heartbeat_a record failed");
    let heartbeat_a_count = crate::projection::manifest::active_generation_count()
        .expect("heartbeat_a count failed");

    Spi::run("SELECT graph.set_current_graph('heartbeat_b', namespace := 'app')")
        .expect("select heartbeat_b failed");
    crate::projection::manifest::record_active_generation_heartbeat(
        9_505_001,
        std::time::Duration::from_secs(300),
        20,
        crate::projection::manifest::VALIDATION_STATUS_VALID,
    )
    .expect("heartbeat_b record failed");
    let heartbeat_b_count = crate::projection::manifest::active_generation_count()
        .expect("heartbeat_b count failed");
    let graph_rows = Spi::get_one::<i64>(
        "SELECT count(DISTINCT graph_id)
           FROM graph._projection_generations
          WHERE generation_id = 9505001",
    )
    .expect("heartbeat graph rows query failed")
    .unwrap_or(0);

    assert_eq!(heartbeat_a_count, 1);
    assert_eq!(heartbeat_b_count, 1);
    assert_eq!(graph_rows, 2);
}

#[pg_test]
fn drop_graph_removes_operational_rows_without_raw_fk_errors() {
    Spi::run("SELECT graph.create_graph('drop_ops', namespace := 'app')")
        .expect("create drop_ops failed");
    let graph_id = Spi::get_one::<String>(
        "SELECT graph_id::text
           FROM graph._graphs
          WHERE graph_name = 'drop_ops'
            AND namespace = 'app'",
    )
    .expect("drop_ops graph id query failed")
    .expect("drop_ops graph id missing");
    Spi::run(&format!(
        "INSERT INTO graph._build_jobs (build_id, graph_id, status, sync_mode, projection_mode)
         VALUES ('drop-build-job', {}::uuid, 'completed', 'manual', 'csr_readonly')",
        super::sql_literal(&graph_id)
    ))
    .expect("insert drop_ops build job failed");
    Spi::run(&format!(
        "INSERT INTO graph._maintenance_jobs (job_id, graph_id, status)
         VALUES ('drop-maintenance-job', {}::uuid, 'completed')",
        super::sql_literal(&graph_id)
    ))
    .expect("insert drop_ops maintenance job failed");
    Spi::run(&format!(
        "INSERT INTO graph._projection_generations (
             graph_id, generation_id, backend_pid, database_oid
         )
         VALUES (
             {}::uuid, 9606001, 0,
             (SELECT oid FROM pg_database WHERE datname = current_database())
         )",
        super::sql_literal(&graph_id)
    ))
    .expect("insert drop_ops projection generation failed");

    let dropped = Spi::get_one::<String>(
        "SELECT graph_name
           FROM graph.drop_graph('drop_ops', namespace := 'app')",
    )
    .expect("drop_ops drop failed")
    .expect("drop_ops drop row missing");
    let operational_rows = Spi::get_one::<i64>(&format!(
        "SELECT
             (SELECT count(*) FROM graph._build_jobs WHERE graph_id = {}::uuid)
           + (SELECT count(*) FROM graph._maintenance_jobs WHERE graph_id = {}::uuid)
           + (SELECT count(*) FROM graph._projection_generations WHERE graph_id = {}::uuid)",
        super::sql_literal(&graph_id),
        super::sql_literal(&graph_id),
        super::sql_literal(&graph_id)
    ))
    .expect("drop_ops operational row count failed")
    .unwrap_or(-1);

    assert_eq!(dropped, "drop_ops");
    assert_eq!(operational_rows, 0);
}

#[pg_test]
fn legacy_job_status_apis_are_scoped_to_selected_graph() {
    Spi::run("SELECT graph.create_graph('status_a', namespace := 'app')")
        .expect("create status_a failed");
    Spi::run("SELECT graph.create_graph('status_b', namespace := 'app')")
        .expect("create status_b failed");
    let graph_a = Spi::get_one::<String>(
        "SELECT graph_id::text FROM graph._graphs WHERE graph_name = 'status_a' AND namespace = 'app'",
    )
    .expect("status_a graph id query failed")
    .expect("status_a graph id missing");
    let graph_b = Spi::get_one::<String>(
        "SELECT graph_id::text FROM graph._graphs WHERE graph_name = 'status_b' AND namespace = 'app'",
    )
    .expect("status_b graph id query failed")
    .expect("status_b graph id missing");
    Spi::run(&format!(
        "INSERT INTO graph._build_jobs (build_id, graph_id, status, sync_mode, projection_mode)
         VALUES
             ('status-build-a', {}::uuid, 'queued', 'manual', 'csr_readonly'),
             ('status-build-b', {}::uuid, 'queued', 'manual', 'csr_readonly')",
        super::sql_literal(&graph_a),
        super::sql_literal(&graph_b)
    ))
    .expect("insert status build jobs failed");
    Spi::run(&format!(
        "INSERT INTO graph._maintenance_jobs (job_id, graph_id, status)
         VALUES
             ('status-maintenance-a', {}::uuid, 'queued'),
             ('status-maintenance-b', {}::uuid, 'queued')",
        super::sql_literal(&graph_a),
        super::sql_literal(&graph_b)
    ))
    .expect("insert status maintenance jobs failed");

    Spi::run("SELECT graph.set_current_graph('status_b', namespace := 'app')")
        .expect("select status_b failed");
    let hidden_build_status = Spi::get_one::<String>(
        "SELECT status FROM graph.build_status('status-build-a')",
    )
    .expect("hidden build status query failed")
    .expect("hidden build status row missing");
    let visible_build_status = Spi::get_one::<String>(
        "SELECT status FROM graph.build_status('status-build-b')",
    )
    .expect("visible build status query failed")
    .expect("visible build status row missing");
    let hidden_maintenance_status = Spi::get_one::<String>(
        "SELECT status FROM graph.maintenance_status('status-maintenance-a')",
    )
    .expect("hidden maintenance status query failed")
    .expect("hidden maintenance status row missing");
    let visible_maintenance_rows = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.maintenance_status(NULL)
          WHERE job_id = 'status-maintenance-b'",
    )
    .expect("visible maintenance list query failed")
    .unwrap_or(0);
    let hidden_maintenance_rows = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.maintenance_status(NULL)
          WHERE job_id = 'status-maintenance-a'",
    )
    .expect("hidden maintenance list query failed")
    .unwrap_or(0);

    assert_eq!(hidden_build_status, "not_found");
    assert_eq!(visible_build_status, "queued");
    assert_eq!(hidden_maintenance_status, "not_found");
    assert_eq!(visible_maintenance_rows, 1);
    assert_eq!(hidden_maintenance_rows, 0);
}

#[pg_test]
fn runtime_selection_does_not_reuse_previous_graph_engine() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('runtime_a', namespace := 'app')")
        .expect("create runtime_a failed");
    Spi::run("SELECT graph.create_graph('runtime_b', namespace := 'app')")
        .expect("create runtime_b failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'runtime_a',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add runtime_a table failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'runtime_b',
                'graph_test_bad_pgtest'::regclass,
                'id',
                ARRAY['note'],
                graph_namespace := 'app'
            )",
    )
    .expect("add runtime_b table failed");
    Spi::run("INSERT INTO public.graph_test_bad_pgtest (id, note) VALUES ('r1', 'runtime')")
        .expect("insert runtime_b row failed");

    assert_eq!(
        sqlstate_for_error("SELECT * FROM graph.load_graph('runtime_a', namespace := 'app')"),
        Some("PG003".to_string())
    );

    Spi::run("SELECT graph.build_graph('runtime_a', force_persist := true, graph_namespace := 'app')")
        .expect("runtime_a build failed");
    Spi::run("SELECT graph.build_graph('runtime_b', force_persist := true, graph_namespace := 'app')")
        .expect("runtime_b build failed");

    let loaded_b_nodes =
        Spi::get_one::<i64>("SELECT node_count FROM graph.loaded_graphs()")
            .expect("loaded_graphs after runtime_b failed")
            .unwrap_or(0);
    let select_a_loaded = Spi::get_one::<bool>(
        "SELECT loaded
           FROM graph.select_graph('runtime_a', namespace := 'app')",
    )
    .expect("select runtime_a failed")
    .unwrap_or(true);
    let loaded_after_select_a =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.loaded_graphs()")
            .expect("loaded_graphs after select_a failed")
            .unwrap_or(-1);
    let load_a_nodes = Spi::get_one::<i64>(
        "SELECT node_count
           FROM graph.load_graph('runtime_a', namespace := 'app')",
    )
    .expect("load runtime_a failed")
    .unwrap_or(0);
    let select_b_loaded = Spi::get_one::<bool>(
        "SELECT loaded
           FROM graph.select_graph('runtime_b', namespace := 'app')",
    )
    .expect("select runtime_b failed")
    .unwrap_or(true);
    let load_b_nodes = Spi::get_one::<i64>(
        "SELECT node_count
           FROM graph.load_graph('runtime_b', namespace := 'app')",
    )
    .expect("load runtime_b failed")
    .unwrap_or(0);
    let unloaded_b = Spi::get_one::<bool>(
        "SELECT unloaded
           FROM graph.unload_graph('runtime_b', namespace := 'app')",
    )
    .expect("unload runtime_b failed")
    .unwrap_or(false);
    let loaded_after_unload =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.loaded_graphs()")
            .expect("loaded_graphs after unload failed")
            .unwrap_or(-1);

    assert_eq!(loaded_b_nodes, 1);
    assert!(!select_a_loaded);
    assert_eq!(loaded_after_select_a, 0);
    assert_eq!(load_a_nodes, 2);
    assert!(!select_b_loaded);
    assert_eq!(load_b_nodes, 1);
    assert!(unloaded_b);
    assert_eq!(loaded_after_unload, 0);
}

#[pg_test]
fn development_worker_entrypoints_restore_job_graph_context() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.create_graph('worker_a', namespace := 'app')")
        .expect("create worker_a failed");
    Spi::run("SELECT graph.create_graph('worker_b', namespace := 'app')")
        .expect("create worker_b failed");
    let graph_a = Spi::get_one::<String>(
        "SELECT graph_id::text
           FROM graph._graphs
          WHERE graph_name = 'worker_a'
            AND namespace = 'app'",
    )
    .expect("worker_a graph id query failed")
    .expect("worker_a graph id missing");
    let graph_b = Spi::get_one::<String>(
        "SELECT graph_id::text
           FROM graph._graphs
          WHERE graph_name = 'worker_b'
            AND namespace = 'app'",
    )
    .expect("worker_b graph id query failed")
    .expect("worker_b graph id missing");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'worker_a',
                'graph_test_users_pgtest'::regclass,
                'id',
                ARRAY['name'],
                graph_namespace := 'app'
            )",
    )
    .expect("add worker_a table failed");
    Spi::run(
        "SELECT graph.add_table_to_graph(
                'worker_b',
                'graph_test_bad_pgtest'::regclass,
                'id',
                ARRAY['note'],
                graph_namespace := 'app'
            )",
    )
    .expect("add worker_b table failed");
    Spi::run("INSERT INTO public.graph_test_bad_pgtest (id, note) VALUES ('w1', 'worker')")
        .expect("insert worker_b row failed");
    Spi::run(&format!(
        "INSERT INTO graph._build_jobs (build_id, graph_id, status, sync_mode, projection_mode)
         VALUES ('worker-build-a', {}::uuid, 'queued', 'manual', 'csr_readonly')",
        super::sql_literal(&graph_a)
    ))
    .expect("insert worker_a build job failed");
    Spi::run(&format!(
        "INSERT INTO graph._maintenance_jobs (job_id, graph_id, status)
         VALUES ('worker-maintenance-a', {}::uuid, 'queued')",
        super::sql_literal(&graph_a)
    ))
    .expect("insert worker_a maintenance job failed");

    Spi::run("SELECT graph.set_current_graph('worker_b', namespace := 'app')")
        .expect("select worker_b before build runner failed");
    let build_error = Spi::get_one::<String>("SELECT graph._test_run_build_job('worker-build-a')")
        .expect("worker_a build runner query failed");
    let build_nodes = Spi::get_one::<i64>(
        "SELECT nodes_loaded
           FROM graph._build_jobs
          WHERE build_id = 'worker-build-a'",
    )
    .expect("worker_a build nodes query failed")
    .unwrap_or(0);
    let current_after_build =
        Spi::get_one::<String>("SELECT graph_id::text FROM graph.current_graph()")
            .expect("current graph after build worker failed")
            .expect("current graph after build worker row missing");

    Spi::run("SELECT graph.set_current_graph('worker_b', namespace := 'app')")
        .expect("select worker_b before maintenance runner failed");
    let maintenance_error =
        Spi::get_one::<String>("SELECT graph._test_run_maintenance_job('worker-maintenance-a')")
            .expect("worker_a maintenance runner query failed");
    let maintenance_nodes = Spi::get_one::<i64>(
        "SELECT nodes_after
           FROM graph._maintenance_jobs
          WHERE job_id = 'worker-maintenance-a'",
    )
    .expect("worker_a maintenance nodes query failed")
    .unwrap_or(0);
    let current_after_maintenance =
        Spi::get_one::<String>("SELECT graph_id::text FROM graph.current_graph()")
            .expect("current graph after maintenance worker failed")
            .expect("current graph after maintenance worker row missing");

    assert!(build_error.is_none());
    assert_eq!(build_nodes, 2);
    assert_eq!(current_after_build, graph_a);
    assert!(maintenance_error.is_none());
    assert_eq!(maintenance_nodes, 2);
    assert_eq!(current_after_maintenance, graph_a);
    assert_ne!(graph_a, graph_b);
}

fn sqlstate_for_prepared_helper(statement: &str) -> Option<String> {
    Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sqlstate({})",
        super::sql_literal(statement)
    ))
    .expect("prepared SQLSTATE helper failed")
}
