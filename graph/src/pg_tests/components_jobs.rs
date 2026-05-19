#[pg_test]
fn schema_admin_role_can_call_global_analytics() {
    reset_and_create_fixtures();
    Spi::run("SELECT * FROM graph.auto_discover('public')").expect("auto_discover failed");

    let num_components = Spi::get_one::<i32>("SELECT num_components FROM graph.component_stats()")
        .expect("component_stats failed")
        .unwrap_or(0);
    let row_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.connected_components()")
        .expect("connected_components failed")
        .unwrap_or(0);

    assert!(num_components > 0);
    let largest_component =
        Spi::get_one::<i32>("SELECT largest_component FROM graph.component_stats()")
            .expect("component largest failed")
            .unwrap_or(0);
    assert!(largest_component > 1);
    assert!(row_count > 0);
}

#[pg_test]
fn component_helper_apis_return_paginated_nodes() {
    reset_and_create_fixtures();
    Spi::run("SELECT * FROM graph.auto_discover('public')").expect("auto_discover failed");

    let component_count =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.components(max_rows := 5)")
            .expect("components helper failed")
            .unwrap_or(0);
    let largest_count = Spi::get_one::<i64>(
        "SELECT count(*) FROM graph.largest_component(max_rows := 5, hydrate := false)",
    )
    .expect("largest_component helper failed")
    .unwrap_or(0);
    let isolated_count = Spi::get_one::<i64>(
        "SELECT count(*) FROM graph.isolated_nodes(max_rows := 5, hydrate := false)",
    )
    .expect("isolated_nodes helper failed")
    .unwrap_or(0);
    let one_component_count = Spi::get_one::<i64>(
        "WITH c AS (
                SELECT component_id FROM graph.components(max_rows := 1)
             )
             SELECT count(*)
             FROM c, graph.component(c.component_id, max_rows := 5, hydrate := false)",
    )
    .expect("component helper failed")
    .unwrap_or(0);

    assert!(component_count > 0);
    assert!(largest_count > 0);
    assert!(isolated_count >= 0);
    assert!(one_component_count > 0);
}

#[pg_test]
fn build_overload_status_and_node_ref_are_available() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add table failed");

    let build_status =
        Spi::get_one::<String>("SELECT status FROM graph.build(concurrently := false)")
            .expect("build overload failed")
            .unwrap_or_default();
    let status_row = Spi::get_one::<String>(
        "SELECT status FROM graph.build_status('00000000-0000-0000-0000-000000000000')",
    )
    .expect("build_status failed")
    .unwrap_or_default();
    let node_ref_type = Spi::get_one::<String>(
        "SELECT pg_typeof(graph.node_ref('graph_test_users_pgtest'::regclass, 'u1'))::text",
    )
    .expect("node_ref type inspection failed")
    .unwrap_or_default();
    let node_id = Spi::get_one::<String>(
        "SELECT (graph.node_ref('graph_test_users_pgtest'::regclass, 'u1')).node_id",
    )
    .expect("node_ref failed")
    .unwrap_or_default();
    let node_ref_string = Spi::get_one::<String>(
        "SELECT graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')",
    )
    .expect("node_ref_string failed")
    .unwrap_or_default();

    assert_eq!(build_status, "completed");
    assert_eq!(status_row, "completed");
    assert_eq!(node_ref_type, "graph.node_ref");
    assert_eq!(node_id, "u1");
    assert_eq!(node_ref_string, "[\"graph_test_users_pgtest\",\"u1\"]");
}

#[pg_test]
fn build_status_reads_durable_concurrent_job_rows() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add table failed");

    let build_id = super::create_build_job().expect("create build job failed");
    let (queued_status, queued_phase) = Spi::connect(|client| {
        let result = client
            .select(
                &format!(
                    "SELECT status, progress_phase FROM graph.build_status({})",
                    super::sql_literal(&build_id)
                ),
                None,
                &[],
            )
            .expect("queued build_status failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .expect("queued build_status failed");

    let result = super::BuildExecutionResult {
        nodes_loaded: 2,
        edges_loaded: 0,
        build_time_ms: 12.5,
        memory_used_mb: 1.25,
        sync_mode: "manual".to_string(),
    };
    super::update_build_job_started(&build_id).expect("mark build job running failed");
    super::update_build_job_completed(&build_id, &result).expect("mark build job completed failed");

    let completed = Spi::get_one::<bool>(&format!(
        "SELECT status = 'completed'
                    AND nodes_loaded = 2
                    AND edges_loaded = 0
                    AND build_time_ms = 12.5
                    AND memory_used_mb = 1.25
                    AND progress_phase = 'completed'
                    AND progress_message = 'build completed'
                    AND started_at IS NOT NULL
                    AND finished_at IS NOT NULL
                    AND pg_typeof(started_at) = 'timestamp with time zone'::regtype
                    AND pg_typeof(finished_at) = 'timestamp with time zone'::regtype
             FROM graph.build_status({})",
        super::sql_literal(&build_id)
    ))
    .expect("completed build_status failed")
    .unwrap_or(false);

    assert_eq!(queued_status, "queued");
    assert_eq!(queued_phase, "queued");
    assert!(completed);
}

#[pg_test]
fn maintenance_status_reads_durable_job_rows() {
    reset_and_create_fixtures();
    let job_id = super::create_maintenance_job().expect("create maintenance job failed");
    let (queued_status, queued_phase) = Spi::connect(|client| {
        let result = client
            .select(
                &format!(
                    "SELECT status, progress_phase FROM graph.maintenance_status({})",
                    super::sql_literal(&job_id)
                ),
                None,
                &[],
            )
            .expect("queued maintenance_status failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .expect("queued maintenance_status failed");

    let result = super::MaintenanceExecutionResult {
        sync_rows_applied: 3,
        nodes_after: 7,
        edges_after: 11,
        vacuum_time_ms: 2.5,
    };
    super::update_maintenance_job_started(&job_id).expect("mark maintenance job running failed");
    super::update_maintenance_job_completed(&job_id, &result)
        .expect("mark maintenance job completed failed");

    let completed = Spi::get_one::<bool>(&format!(
        "SELECT status = 'completed'
                    AND sync_rows_applied = 3
                    AND nodes_after = 7
                    AND edges_after = 11
                    AND vacuum_time_ms = 2.5
                    AND progress_phase = 'completed'
                    AND progress_message = 'maintenance completed'
                    AND started_at IS NOT NULL
                    AND finished_at IS NOT NULL
                    AND pg_typeof(started_at) = 'timestamp with time zone'::regtype
                    AND pg_typeof(finished_at) = 'timestamp with time zone'::regtype
             FROM graph.maintenance_status({})",
        super::sql_literal(&job_id)
    ))
    .expect("completed maintenance_status failed")
    .unwrap_or(false);

    assert_eq!(queued_status, "queued");
    assert_eq!(queued_phase, "queued");
    assert!(completed);
}

#[pg_test]
fn traverse_accepts_v1_node_ref_array_starts() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add users failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                from_column := 'user_id',
                to_table := 'graph_test_users_pgtest'::regclass,
                to_column := 'friend_id',
                label := 'knows'
            )",
    )
    .expect("add edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let root_count = Spi::get_one::<i64>(
        "SELECT count(DISTINCT root_id)
             FROM graph.traverse(
                ARRAY[
                    graph.node_ref('graph_test_users_pgtest'::regclass, 'u1'),
                    graph.node_ref('graph_test_users_pgtest'::regclass, 'u2')
                ]::graph.node_ref[],
                max_depth := 0,
                hydrate := false
             )",
    )
    .expect("node_ref[] traversal failed")
    .unwrap_or(0);

    assert_eq!(root_count, 2);
}

#[pg_test]
fn traverse_node_ref_array_signature_uses_named_row_pagination() {
    let arguments = Spi::get_one::<String>(
        "SELECT pg_get_function_arguments(p.oid)
           FROM pg_proc p
           JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'graph'
            AND p.proname = 'traverse'
            AND pg_get_function_arguments(p.oid) LIKE 'starts graph.node_ref[]%'",
    )
    .expect("node_ref[] traverse signature inspection failed")
    .expect("node_ref[] traverse signature missing");

    assert!(
        arguments.contains("max_rows integer"),
        "expected max_rows in node_ref[] traverse signature: {arguments}"
    );
    assert!(
        arguments.contains("row_offset integer"),
        "expected row_offset in node_ref[] traverse signature: {arguments}"
    );
    assert!(
        !arguments.contains("\"limit\" integer") && !arguments.contains("\"offset\" integer"),
        "node_ref[] traverse signature should not expose quoted pagination names: {arguments}"
    );
}

#[pg_test]
fn query_results_expose_resolved_table_name_columns() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add users failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                from_column := 'user_id',
                to_table := 'graph_test_users_pgtest'::regclass,
                to_column := 'friend_id',
                label := 'knows'
            )",
    )
    .expect("add edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let search_name = Spi::get_one::<String>(
        "SELECT node_table_name
             FROM graph.search(
                'name',
                'Alice',
                table_filter := 'graph_test_users_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )",
    )
    .expect("search table-name read failed")
    .unwrap_or_default();
    let search_nodes_name = Spi::get_one::<String>(
        "SELECT node_table_name
             FROM graph.search_nodes(
                'name',
                'Alice',
                table_filter := 'graph_test_users_pgtest'::regclass,
                mode := 'exact'
             )",
    )
    .expect("search_nodes table-name read failed")
    .unwrap_or_default();
    let traverse_names = Spi::get_one::<String>(
        "SELECT root_table_name || '|' || node_table_name
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("traverse table-name read failed")
    .unwrap_or_default();
    let shortest_name = Spi::get_one::<String>(
        "SELECT node_table_name
             FROM graph.shortest_path(
                'graph_test_users_pgtest'::regclass,
                'u1',
                'graph_test_users_pgtest'::regclass,
                'u2',
                5,
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("shortest_path table-name read failed")
    .unwrap_or_default();

    assert_eq!(search_name, "graph_test_users_pgtest");
    assert_eq!(search_nodes_name, "graph_test_users_pgtest");
    assert_eq!(
        traverse_names,
        "graph_test_users_pgtest|graph_test_users_pgtest"
    );
    assert_eq!(shortest_name, "graph_test_users_pgtest");
}

#[pg_test]
fn public_traverse_signatures_do_not_expose_string_filter_condition() {
    reset_and_create_fixtures();
    let exposed = Spi::get_one::<bool>(
        "SELECT EXISTS (
                SELECT 1
                FROM pg_proc p
                JOIN pg_namespace n ON n.oid = p.pronamespace
                WHERE n.nspname = 'graph'
                  AND p.proname = 'traverse'
                  AND pg_get_function_arguments(p.oid) LIKE '%filter_condition%'
            )",
    )
    .expect("signature inspection failed")
    .unwrap_or(true);

    assert!(!exposed);
}

#[pg_test]
fn traverse_srfs_declare_conservative_planner_hints() {
    reset_and_create_fixtures();
    let hints_ok = Spi::get_one::<bool>(
        "SELECT bool_and(p.procost >= 1000 AND p.prorows >= 1000)
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname = 'graph'
               AND p.proname = 'traverse'",
    )
    .expect("planner hint inspection failed")
    .unwrap_or(false);

    assert!(hints_ok);
}

#[pg_test]
fn search_signature_matches_launch_contract_and_verifies_long_exact_matches() {
    reset_and_create_fixtures();
    let long_name = "Alice Signal ".repeat(16);
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add table failed");
    Spi::run(&format!(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('u3', {}, 29)",
        super::sql_literal(&long_name)
    ))
    .expect("insert long search fixture failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let signature_matches_contract = Spi::get_one::<bool>(
        "SELECT EXISTS (
                SELECT 1
                FROM pg_proc p
                JOIN pg_namespace n ON n.oid = p.pronamespace
                WHERE n.nspname = 'graph'
                  AND p.proname = 'traverse_search'
                  AND pg_get_function_arguments(p.oid) LIKE '%max_depth integer%'
                  AND pg_get_function_arguments(p.oid) LIKE '%max_rows integer%'
                  AND pg_get_function_arguments(p.oid) LIKE '%row_offset integer%'
                  AND pg_get_function_arguments(p.oid) NOT LIKE '%traverse_limit%'
                  AND pg_get_function_arguments(p.oid) NOT LIKE '%max_nodes%'
                  AND pg_get_function_arguments(p.oid) NOT LIKE '%max_frontier%'
            )",
    )
    .expect("traverse_search signature inspection failed")
    .unwrap_or(false);
    let verified_long_match = Spi::get_one::<bool>(&format!(
        "SELECT verified
             FROM graph.search(
                'name',
                {},
                'graph_test_users_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )
             WHERE node_id = 'u3'",
        super::sql_literal(&long_name.to_lowercase())
    ))
    .expect("verified long exact search failed")
    .unwrap_or(false);
    assert!(signature_matches_contract);
    assert!(verified_long_match);
}
