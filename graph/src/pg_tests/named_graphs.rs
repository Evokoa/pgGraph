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
