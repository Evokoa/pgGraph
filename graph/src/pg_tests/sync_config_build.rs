#[pg_test]
fn trigger_sync_adds_edge_overlay_after_explicit_apply_sync() {
    reset_and_create_fixtures();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_sync_edges_pgtest CASCADE")
        .expect("drop sync edge table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_sync_edges_pgtest (
                id TEXT PRIMARY KEY,
                friend_id TEXT REFERENCES public.graph_test_sync_edges_pgtest(id),
                name TEXT NOT NULL
            )",
    )
    .expect("create sync edge table failed");
    Spi::run(
        "INSERT INTO public.graph_test_sync_edges_pgtest (id, friend_id, name)
             VALUES ('a', NULL, 'A'), ('b', NULL, 'B')",
    )
    .expect("insert sync edge rows failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_sync_edges_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add sync edge table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_sync_edges_pgtest'::regclass,
                'friend_id',
                'graph_test_sync_edges_pgtest'::regclass,
                'id',
                'friend',
                bidirectional := false
            )",
    )
    .expect("add sync edge failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set trigger sync failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    Spi::run(
        "INSERT INTO public.graph_test_sync_edges_pgtest (id, friend_id, name)
             VALUES ('c', 'a', 'C')",
    )
    .expect("insert c failed");
    Spi::run("SELECT * FROM graph.apply_sync()").expect("explicit apply_sync failed");

    let reaches_a = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse('graph_test_sync_edges_pgtest'::regclass, 'c', 1, hydrate := false)
             WHERE node_id = 'a'",
    )
    .expect("sync edge traverse failed")
    .unwrap_or(0);
    assert_eq!(reaches_a, 1);
}

#[pg_test]
fn sync_mode_trigger_installs_and_manual_removes_graph_triggers() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add users table failed");

    Spi::run("SET graph.sync_mode = 'trigger'").expect("set trigger mode failed");
    Spi::run("SELECT * FROM graph.build()").expect("trigger build failed");
    let trigger_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM pg_trigger
             WHERE tgrelid = 'graph_test_users_pgtest'::regclass
               AND tgname LIKE 'graph_sync_%'",
    )
    .expect("trigger count failed")
    .unwrap_or(0);

    Spi::run("SET graph.sync_mode = 'manual'").expect("set manual mode failed");
    Spi::run("SELECT * FROM graph.build()").expect("manual build failed");
    let manual_trigger_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM pg_trigger
             WHERE tgrelid = 'graph_test_users_pgtest'::regclass
               AND tgname LIKE 'graph_sync_%'",
    )
    .expect("manual trigger count failed")
    .unwrap_or(0);

    assert_eq!(trigger_count, 4);
    assert_eq!(manual_trigger_count, 0);
}

#[pg_test]
fn build_fails_closed_when_sync_checkpoint_read_fails() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add users table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend'
            )",
    )
    .expect("add friendship edge failed");
    Spi::run("ALTER TABLE graph._sync_log RENAME COLUMN id TO broken_id")
        .expect("malform sync log failed");

    assert!(sql_raises("SELECT * FROM graph.build()"));
}

#[pg_test]
fn sync_mode_wal_fails_with_reserved_message() {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'wal'").expect("set wal mode failed");

    let err = super::current_sync_mode().expect_err("wal mode should be reserved");

    assert!(matches!(
        err,
        crate::safety::GraphError::InvalidFilter { .. }
    ));
    assert_eq!(
            err.to_string(),
            "Invalid filter condition: graph.sync_mode = 'wal' is reserved for roadmap work; please use 'trigger' or 'manual'"
        );
}

#[pg_test]
fn guc_contract_defaults_ranges_and_contexts_are_registered() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("RESET graph.sync_mode").expect("reset sync_mode failed");
    Spi::run("RESET graph.oom_action").expect("reset oom_action failed");
    Spi::run("RESET graph.tenant_setting").expect("reset tenant_setting failed");
    Spi::run("RESET graph.build_scan_mode").expect("reset build_scan_mode failed");
    Spi::run("RESET graph.enforce_tenant_scope").expect("reset enforce_tenant_scope failed");
    Spi::run("RESET graph.max_exact_path_count").expect("reset max_exact_path_count failed");
    Spi::run("RESET graph.build_batch_size").expect("reset build_batch_size failed");

    assert_eq!(crate::config::sync_mode(), "manual");
    assert_eq!(
        crate::config::parsed_sync_mode(),
        Some(crate::config::SyncMode::Manual)
    );
    assert_eq!(crate::config::oom_action(), crate::config::OomAction::Error);
    assert_eq!(
        crate::config::build_scan_mode(),
        crate::config::BuildScanMode::Select
    );
    assert_eq!(crate::config::tenant_setting(), "");
    assert!(crate::config::ENFORCE_TENANT_SCOPE.get());
    assert_eq!(crate::config::MAX_EXACT_PATH_COUNT.get(), 100_000);
    assert_eq!(crate::config::BUILD_BATCH_SIZE.get(), 10_000);

    let registered = Spi::get_one::<bool>(
        "WITH expected(name, context, min_val, max_val) AS (
                VALUES
                    ('graph.memory_limit_mb', 'superuser', '64', '32768'),
                    ('graph.default_max_depth', 'user', '1', '100'),
                    ('graph.max_nodes', 'user', '1', '10000000'),
                    ('graph.max_frontier', 'user', '1', '10000000'),
                    ('graph.max_exact_path_count', 'user', '1', '10000000'),
                    ('graph.build_batch_size', 'superuser', '1', '1000000'),
                    ('graph.edge_buffer_size', 'superuser', '1000', '10000000'),
                    ('graph.vacuum_interval_secs', 'superuser', '5', '86400')
             ),
             matched AS (
                SELECT e.name,
                       s.context = e.context
                       AND s.min_val = e.min_val
                       AND s.max_val = e.max_val AS ok
                FROM expected e
                JOIN pg_settings s ON s.name = e.name
             )
             SELECT count(*) = 8 AND bool_and(ok)
             FROM matched",
    )
    .expect("pg_settings inspection failed")
    .unwrap_or(false);
    assert!(registered);
    assert!(sql_raises("SET graph.memory_limit_mb = 63"));
    assert!(sql_raises("SET graph.max_exact_path_count = 0"));
    assert!(sql_raises("SET graph.build_batch_size = 0"));
    assert!(sql_raises("SET graph.edge_buffer_size = 999"));
}

#[pg_test]
fn oom_action_error_and_readonly_are_applied_by_build() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.memory_limit_mb = 64").expect("set memory limit failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_oom_pgtest CASCADE")
        .expect("drop oom table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_oom_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create oom table failed");
    Spi::run("INSERT INTO public.graph_test_oom_pgtest VALUES ('one', 'One')")
        .expect("insert oom row failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_oom_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add oom table failed");
    Spi::run(
        "UPDATE pg_class
             SET reltuples = 100000000
             WHERE oid = 'public.graph_test_oom_pgtest'::regclass",
    )
    .expect("inflate reltuples failed");

    Spi::run("SET graph.oom_action = 'error'").expect("set oom error failed");
    assert!(sql_raises("SELECT * FROM graph.build()"));

    Spi::run("SET graph.oom_action = 'readonly'").expect("set oom readonly failed");
    let nodes = Spi::get_one::<i64>("SELECT nodes_loaded FROM graph.build()")
        .expect("readonly build failed")
        .unwrap_or(0);
    let (read_only, read_only_reason) = Spi::connect(|client| {
        let result = client
            .select("SELECT read_only, read_only_reason FROM graph.status()", None, &[])
            .expect("status read_only failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((row.get::<bool>(1)?, row.get::<String>(2)?))
    })
    .expect("status read failed");

    Spi::run("SET graph.oom_action = 'error'").expect("restore oom action failed");
    Spi::run("SET graph.memory_limit_mb = 2048").expect("restore memory limit failed");

    assert_eq!(nodes, 1);
    assert_eq!(read_only, Some(true));
    assert_eq!(read_only_reason.as_deref(), Some("memory_limit"));
}

#[pg_test]
fn build_memory_headroom_accounts_for_existing_serving_graph() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.memory_limit_mb = 64").expect("set memory limit failed");
    Spi::run("SET graph.oom_action = 'error'").expect("set oom error failed");
    clear_graph_catalog_for_test();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_headroom_pgtest CASCADE")
        .expect("drop headroom table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_headroom_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create headroom table failed");
    Spi::run("INSERT INTO public.graph_test_headroom_pgtest VALUES ('one', 'One')")
        .expect("insert headroom row failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_headroom_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add headroom table failed");

    crate::ENGINE.with(|e| {
        let mut eng = crate::engine::Engine::new();
        for idx in 0..1_200_000u32 {
            eng.node_store.add_node(1, idx.to_string());
        }
        eng.built = true;
        *e.borrow_mut() = eng;
    });

    assert!(sql_raises("SELECT * FROM graph.build()"));

    Spi::run("SET graph.oom_action = 'error'").expect("restore oom action failed");
    Spi::run("SET graph.memory_limit_mb = 2048").expect("restore memory limit failed");
}

#[pg_test]
fn catalog_drift_requires_rebuild() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add users table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run("SELECT graph.add_filter_column('graph_test_users_pgtest'::regclass, 'age')")
        .expect("add filter after build failed");

    let result = super::ensure_current_graph();
    assert!(result.is_err());
}

#[pg_test]
fn schema_drift_detects_live_ddl_changes() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.sync_mode = 'manual'").expect("reset sync mode failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_schema_drift_pgtest CASCADE")
        .expect("drop drift table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_schema_drift_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                score INT NOT NULL,
                parent_id TEXT,
                weight INT,
                edge_label TEXT
            )",
    )
    .expect("create drift table failed");
    Spi::run(
        "INSERT INTO public.graph_test_schema_drift_pgtest
                (id, name, score, parent_id, weight, edge_label)
             VALUES
                ('a', 'A', 10, NULL, NULL, NULL),
                ('b', 'B', 20, 'a', 7, 'parent')",
    )
    .expect("insert drift rows failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_schema_drift_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add drift table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_schema_drift_pgtest'::regclass,
                'parent_id',
                'graph_test_schema_drift_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := false,
                weight_column := 'weight',
                label_column := 'edge_label'
            )",
    )
    .expect("add drift edge failed");
    Spi::run(
        "SELECT graph.add_filter_column(
                'graph_test_schema_drift_pgtest'::regclass,
                'score',
                column_type := 'numeric'
            )",
    )
    .expect("add drift filter failed");
    Spi::run("SELECT * FROM graph.build()").expect("build drift fixture failed");

    Spi::run(
        "ALTER TABLE public.graph_test_schema_drift_pgtest
             ALTER COLUMN score TYPE text USING score::text",
    )
    .expect("ddl drift mutation failed");
    let reason = Spi::get_one::<String>(
        "SELECT COALESCE(invalid_reason, '')
             FROM graph.status()
             WHERE schema_status = 'invalid'
               AND needs_rebuild",
    )
    .expect("status drift inspection failed")
    .unwrap_or_default();

    assert!(reason.contains("filter column"));
}

#[pg_test]
fn build_scan_mode_select_works_and_copy_fails_clearly() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add users table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend'
            )",
    )
    .expect("add friendship edge failed");
    Spi::run("SET graph.build_scan_mode = 'select'").expect("set select mode failed");
    Spi::run("SET graph.build_batch_size = 1").expect("set small build batch failed");
    let shape =
        Spi::get_one::<bool>("SELECT nodes_loaded = 2 AND edges_loaded = 2 FROM graph.build()")
            .expect("select build failed")
            .unwrap_or(false);
    assert!(shape);

    Spi::run("SET graph.build_scan_mode = 'copy'").expect("set copy mode failed");
    let (tables, edges, filter_columns) = super::read_catalog().expect("catalog read failed");
    let copy_result = crate::builder::build_graph(&tables, &edges, &filter_columns);
    assert!(copy_result.is_err());
    Spi::run("SET graph.build_scan_mode = 'select'").expect("restore select mode failed");
    Spi::run("SET graph.build_batch_size = 10000").expect("restore build batch failed");
}
