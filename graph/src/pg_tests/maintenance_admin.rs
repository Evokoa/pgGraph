#[pg_test]
fn sql_trigger_sync_handles_primary_key_changes() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.enabled = on").expect("enable graph failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_pk_update_pgtest CASCADE")
        .expect("drop pk update table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_pk_update_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create pk update table failed");
    Spi::run("INSERT INTO public.graph_test_pk_update_pgtest (id, name) VALUES ('old', 'Alice')")
        .expect("insert pk update row failed");
    super::insert_registered_table("public.graph_test_pk_update_pgtest", "id", "name", None)
        .expect("insert registered pk update table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run("SELECT graph.enable_sync()").expect("enable sync failed");

    Spi::run(
            "UPDATE public.graph_test_pk_update_pgtest SET id = 'new', name = 'Alice2' WHERE id = 'old'",
        )
        .expect("update pk failed");
    let log_count = Spi::get_one::<i64>("SELECT count(*) FROM graph._sync_log")
        .expect("sync log count failed")
        .unwrap_or(0);
    assert_eq!(log_count, 1);
    let updates = Spi::get_one::<i64>("SELECT updates_applied FROM graph.apply_sync()")
        .expect("apply sync failed")
        .unwrap_or(0);

    assert_eq!(updates, 1);
    let new_seed = Spi::get_one::<String>(
            "SELECT node_id FROM graph.traverse('graph_test_pk_update_pgtest'::regclass, 'new', 1) LIMIT 1",
        )
        .expect("new key traverse failed")
        .unwrap_or_default();
    let old_search_count = Spi::get_one::<i64>(
            "SELECT count(*) FROM graph.search('name', 'Alice', 'graph_test_pk_update_pgtest'::regclass, mode := 'exact')",
        )
        .expect("old token search failed")
        .unwrap_or(0);
    let search_count = Spi::get_one::<i64>(
            "SELECT count(*) FROM graph.search('name', 'Alice2', 'graph_test_pk_update_pgtest'::regclass, mode := 'exact')",
        )
        .expect("new token search failed")
        .unwrap_or(0);

    assert_eq!(new_seed, "new");
    assert_eq!(old_search_count, 0);
    assert_eq!(search_count, 1);
}

#[pg_test]
fn apply_sync_replays_trigger_update_and_delete_rows() {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    build_friendship_fixture_graph();
    Spi::run("SELECT graph.enable_sync()").expect("enable sync failed");

    Spi::run("UPDATE public.graph_test_users_pgtest SET name = 'Alice Updated' WHERE id = 'u1'")
        .expect("update user failed");
    Spi::run("DELETE FROM public.graph_test_friendships_pgtest WHERE friend_id = 'u2'")
        .expect("delete source friendship failed");
    Spi::run("DELETE FROM public.graph_test_users_pgtest WHERE id = 'u2'")
        .expect("delete user failed");

    let (updates, deletes) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT updates_applied, deletes_applied FROM graph.apply_sync()",
                None,
                &[],
            )
            .expect("apply sync failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<i64>(1)?.unwrap_or(0),
            row.get::<i64>(2)?.unwrap_or(0),
        ))
    })
    .expect("apply sync read failed");
    assert_eq!(updates, 1);
    assert_eq!(deletes, 1);

    let pending_sync_rows = Spi::get_one::<i64>("SELECT pending_sync_rows FROM graph.status()")
        .expect("status read failed")
        .unwrap_or(-1);
    assert_eq!(pending_sync_rows, 0);

    let active_seed_count = Spi::get_one::<i64>(
        "SELECT count(*)
         FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 0, hydrate := false)
         WHERE node_id = 'u1'",
    )
    .expect("active seed traverse failed")
    .unwrap_or(0);
    assert_eq!(active_seed_count, 1);

    let deleted_neighbor_count = Spi::get_one::<i64>(
        "SELECT count(*)
         FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 1, hydrate := false)
         WHERE node_id = 'u2'",
    )
    .expect("deleted neighbor traverse failed")
    .unwrap_or(0);
    assert_eq!(deleted_neighbor_count, 0);
}

#[pg_test]
fn edge_buffer_overflow_from_sql_sync_enters_read_only_mode() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.enabled = on").expect("enable graph failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SET graph.edge_buffer_size = 1000").expect("set edge buffer size failed");
    Spi::run("SET graph.sync_batch_size = 1001").expect("set sync batch size failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_edge_overflow_pgtest CASCADE")
        .expect("drop edge overflow table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_edge_overflow_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL REFERENCES public.graph_test_edge_overflow_pgtest(id),
                name TEXT NOT NULL
            )",
    )
    .expect("create edge overflow table failed");
    Spi::run(
        "INSERT INTO public.graph_test_edge_overflow_pgtest (id, parent_id, name)
             VALUES ('root', NULL, 'Root')",
    )
    .expect("insert root failed");
    super::insert_registered_table(
        "public.graph_test_edge_overflow_pgtest",
        "id",
        "name,parent_id",
        None,
    )
    .expect("insert registered edge overflow table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_edge_overflow_pgtest'::regclass,
                'parent_id',
                'graph_test_edge_overflow_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := false
            )",
    )
    .expect("add self edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run("SELECT graph.enable_sync()").expect("enable sync failed");
    Spi::run(
        "INSERT INTO public.graph_test_edge_overflow_pgtest (id, parent_id, name)
             SELECT 'child-' || n::text, 'root', 'Child ' || n::text
             FROM generate_series(1, 1001) AS n",
    )
    .expect("insert overflowing edge rows failed");

    assert!(sql_raises("SELECT * FROM graph.apply_sync()"));
    let (read_only, read_only_reason, sync_status, edge_buffer_used) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT read_only, read_only_reason, sync_status, edge_buffer_used FROM graph.status()",
                None,
                &[],
            )
            .expect("status query failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<bool>(1)?.unwrap_or(false),
            row.get::<String>(2)?,
            row.get::<String>(3)?.unwrap_or_default(),
            row.get::<i32>(4)?.unwrap_or(0),
        ))
    })
    .expect("status read failed");

    assert!(read_only);
    assert_eq!(read_only_reason.as_deref(), Some("edge_buffer_full"));
    assert_eq!(sync_status, "read_only");
    assert_eq!(edge_buffer_used, 0);
    let (apply_recommended, maintenance_recommended) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT apply_sync_recommended, maintenance_recommended
                 FROM graph.sync_health()",
                None,
                &[],
            )
            .expect("sync health query failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<bool>(1)?.unwrap_or(true),
            row.get::<bool>(2)?.unwrap_or(false),
        ))
    })
    .expect("sync health read failed");
    assert!(!apply_recommended);
    assert!(maintenance_recommended);
    Spi::run("SET graph.edge_buffer_size = 100000").expect("restore edge buffer size failed");
}

#[pg_test]
fn edge_buffer_overflow_reserves_high_fanout_row_before_mutation() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.enabled = on").expect("enable graph failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SET graph.edge_buffer_size = 1000").expect("set edge buffer size failed");
    Spi::run("SET graph.sync_batch_size = 2").expect("set sync batch size failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_edge_reserve_pgtest CASCADE")
        .expect("drop edge reserve table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_edge_reserve_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL REFERENCES public.graph_test_edge_reserve_pgtest(id),
                name TEXT NOT NULL
            )",
    )
    .expect("create edge reserve table failed");
    Spi::run(
        "INSERT INTO public.graph_test_edge_reserve_pgtest (id, parent_id, name)
             VALUES ('root', NULL, 'Root')",
    )
    .expect("insert root failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_edge_reserve_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'parent_id']
            )",
    )
    .expect("add edge reserve table failed");
    for edge_idx in 1..=254 {
        Spi::run(&format!(
            "SELECT graph.add_edge(
                    'graph_test_edge_reserve_pgtest'::regclass,
                    'parent_id',
                    'graph_test_edge_reserve_pgtest'::regclass,
                    'id',
                    'reserve_edge_{edge_idx}',
                    bidirectional := true
                )"
        ))
        .expect("add high-fanout edge failed");
    }
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    let node_count_before = Spi::get_one::<i32>("SELECT node_count FROM graph.status()")
        .expect("status failed")
        .unwrap_or(0);
    Spi::run("SELECT graph.enable_sync()").expect("enable sync failed");
    Spi::run(
        "INSERT INTO public.graph_test_edge_reserve_pgtest (id, parent_id, name)
             VALUES
                ('child-1', 'root', 'Child 1'),
                ('child-2', 'root', 'Child 2')",
    )
    .expect("insert high-fanout children failed");
    let child_1_sync_id =
        Spi::get_one::<i64>("SELECT id FROM graph._sync_log WHERE new_pk = 'child-1'")
            .expect("child-1 sync id query failed")
            .unwrap_or(0);

    assert!(sql_raises("SELECT * FROM graph.apply_sync()"));
    let (read_only, edge_buffer_used, node_count_after, applied_sync_id) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT read_only, edge_buffer_used, node_count, applied_sync_id
                 FROM graph.status()",
                None,
                &[],
            )
            .expect("status query failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<bool>(1)?.unwrap_or(false),
            row.get::<i32>(2)?.unwrap_or(0),
            row.get::<i32>(3)?.unwrap_or(0),
            row.get::<i64>(4)?.unwrap_or(0),
        ))
    })
    .expect("status read failed");
    Spi::run("SET graph.query_freshness = 'off'").expect("set query freshness off failed");
    let child_1_visible = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_edge_reserve_pgtest'::regclass,
                'child-1',
                0,
                hydrate := false
             )",
    )
    .expect("child-1 traversal failed")
    .unwrap_or(0);

    assert!(read_only);
    assert_eq!(edge_buffer_used, 508);
    assert_eq!(node_count_after, node_count_before + 1);
    assert_eq!(child_1_visible, 1);
    assert_eq!(applied_sync_id, child_1_sync_id);
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
    Spi::run("SET graph.edge_buffer_size = 100000").expect("restore edge buffer size failed");
    Spi::run("RESET graph.sync_batch_size").expect("reset sync batch size failed");
}

#[pg_test]
fn sync_log_batch_reader_respects_limit_and_high_watermark() {
    reset_and_create_fixtures();
    super::insert_registered_table("public.graph_test_users_pgtest", "id", "name", None)
        .expect("insert registered users table failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             SELECT 'batch-' || n::text, 'Batch ' || n::text, n
             FROM generate_series(1, 5) AS n",
    )
    .expect("insert batched users failed");

    let first_two = super::sql_sync::read_sync_log_entries_after(0, 2, None)
        .expect("limited sync read failed");
    let high_water = first_two[1].id;
    let bounded = super::sql_sync::read_sync_log_entries_after(0, 10, Some(high_water))
        .expect("high-water sync read failed");

    assert_eq!(first_two.len(), 2);
    assert_eq!(bounded.len(), 2);
    assert_eq!(bounded.last().map(|entry| entry.id), Some(high_water));
}

#[pg_test]
fn apply_sync_drains_trigger_log_across_multiple_batches() {
    reset_and_create_fixtures();
    super::insert_registered_table("public.graph_test_users_pgtest", "id", "name", None)
        .expect("insert registered users table failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SET graph.sync_batch_size = 2").expect("set sync batch size failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             SELECT 'multi-' || n::text, 'Multi ' || n::text, n
             FROM generate_series(1, 5) AS n",
    )
    .expect("insert multi-batch users failed");
    let max_id = Spi::get_one::<i64>("SELECT max(id) FROM graph._sync_log")
        .expect("sync max id failed")
        .unwrap_or(0);

    let inserts = Spi::get_one::<i64>("SELECT inserts_applied FROM graph.apply_sync()")
        .expect("apply sync failed")
        .unwrap_or(0);
    let (pending, applied) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pending_sync_rows, applied_sync_id FROM graph.status()",
                None,
                &[],
            )
            .expect("status query failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<i64>(1)?.unwrap_or(0),
            row.get::<i64>(2)?.unwrap_or(0),
        ))
    })
    .expect("status read failed");

    assert_eq!(inserts, 5);
    assert_eq!(pending, 0);
    assert_eq!(applied, max_id);
    Spi::run("RESET graph.sync_batch_size").expect("reset sync batch size failed");
}

#[pg_test]
fn apply_sync_until_stops_at_captured_high_watermark() {
    reset_and_create_fixtures();
    super::insert_registered_table("public.graph_test_users_pgtest", "id", "name", None)
        .expect("insert registered users table failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('before-high-water', 'Before', 1)",
    )
    .expect("insert before high-water row failed");
    let high_water = Spi::get_one::<i64>("SELECT max(id) FROM graph._sync_log")
        .expect("sync max id failed")
        .unwrap_or(0);
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('after-high-water', 'After', 2)",
    )
    .expect("insert after high-water row failed");

    let stats = super::sql_sync::apply_sync_until(Some(high_water), 10)
        .expect("apply until high-water failed");
    let applied = Spi::get_one::<i64>("SELECT applied_sync_id FROM graph.status()")
        .expect("status failed")
        .unwrap_or(0);
    let pending = super::sql_sync::pending_sync_rows(applied).expect("pending read failed");

    assert_eq!(stats.inserts, 1);
    assert_eq!(applied, high_water);
    assert_eq!(pending, 1);
}

#[pg_test]
fn maintenance_rebuilds_persisted_graph_from_source_with_pending_sync() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = on").expect("enable persist_on_build failed");
    Spi::run("SET graph.enabled = on").expect("enable graph failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    clear_graph_catalog_for_test();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_crash_replay_pgtest CASCADE")
        .expect("drop crash replay table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_crash_replay_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create crash replay table failed");
    Spi::run(
        "INSERT INTO public.graph_test_crash_replay_pgtest (id, name)
             VALUES ('predelete', 'Gone')",
    )
    .expect("insert predelete row failed");
    super::insert_registered_table("public.graph_test_crash_replay_pgtest", "id", "name", None)
        .expect("insert registered crash replay table failed");
    Spi::run("SELECT graph.enable_sync()").expect("enable sync failed");
    Spi::run("DELETE FROM public.graph_test_crash_replay_pgtest WHERE id = 'predelete'")
        .expect("delete prebuild row failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    let graph_path = crate::persistence::graph_file_path().expect("graph path failed");
    let checkpoint = crate::persistence::read_sync_checkpoint(&graph_path)
        .expect("checkpoint read failed")
        .unwrap_or(0);
    assert!(checkpoint >= 1);
    Spi::run(
        "INSERT INTO public.graph_test_crash_replay_pgtest (id, name)
             VALUES ('after-crash', 'After')",
    )
    .expect("insert postbuild row failed");

    crate::ENGINE.with(|e| {
        *e.borrow_mut() = crate::engine::Engine::new();
    });
    Spi::run("SET graph.auto_load = on").expect("enable auto_load failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set trigger sync_mode failed");

    let stale_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'After',
                'graph_test_crash_replay_pgtest'::regclass,
                mode := 'exact'
             )",
    )
    .expect("stale search failed")
    .unwrap_or(0);
    let maintenance_status =
        Spi::get_one::<String>("SELECT status FROM graph.maintenance(concurrently := false)")
            .expect("maintenance rebuild failed")
            .unwrap_or_default();
    let count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'After',
                'graph_test_crash_replay_pgtest'::regclass,
                mode := 'exact'
             )",
    )
    .expect("crash replay search failed")
    .unwrap_or(0);

    assert_eq!(stale_count, 1);
    assert_eq!(maintenance_status, "completed");
    assert_eq!(count, 1);
    Spi::run("SET graph.persist_on_build = off").expect("restore persist_on_build failed");
}

#[pg_test]
fn apply_sync_accepts_auto_loaded_mmap_graph_node_edge_and_truncate_deltas() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = on").expect("enable persist_on_build failed");
    Spi::run("SET graph.enabled = on").expect("enable graph failed");
    Spi::run("SET graph.sync_mode = 'manual'").expect("set sync_mode failed");
    clear_graph_catalog_for_test();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_mmap_sync_pgtest CASCADE")
        .expect("drop mmap sync table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_mmap_sync_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL REFERENCES public.graph_test_mmap_sync_pgtest(id),
                name TEXT NOT NULL
            )",
    )
    .expect("create mmap sync table failed");
    Spi::run(
        "INSERT INTO public.graph_test_mmap_sync_pgtest (id, parent_id, name)
             VALUES ('root', NULL, 'Root')",
    )
    .expect("insert mmap sync root failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_mmap_sync_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'parent_id']
            )",
    )
    .expect("add mmap sync table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_mmap_sync_pgtest'::regclass,
                'parent_id',
                'graph_test_mmap_sync_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := false
            )",
    )
    .expect("add mmap sync edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    crate::ENGINE.with(|e| {
        *e.borrow_mut() = crate::engine::Engine::new();
    });
    Spi::run("SET graph.auto_load = on").expect("enable auto_load failed");
    let loaded_root = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Root',
                'graph_test_mmap_sync_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )",
    )
    .expect("auto-load search failed")
    .unwrap_or(0);
    assert_eq!(loaded_root, 1);
    let loaded_from_mmap = crate::ENGINE.with(|e| e.borrow().node_store.is_mmap_backed());
    assert!(loaded_from_mmap);

    Spi::run(
        "INSERT INTO public.graph_test_mmap_sync_pgtest (id, parent_id, name)
             VALUES ('child', 'root', 'Child')",
    )
    .expect("insert child source row failed");
    Spi::run(
        "INSERT INTO graph._sync_log (
                op,
                table_oid,
                table_name,
                new_pk,
                properties,
                new_row
             )
             VALUES (
                'I',
                'public.graph_test_mmap_sync_pgtest'::regclass,
                'public.graph_test_mmap_sync_pgtest',
                'child',
                '{\"name\":\"Child\",\"parent_id\":\"root\"}'::jsonb,
                '{\"id\":\"child\",\"name\":\"Child\",\"parent_id\":\"root\"}'::jsonb
             )",
    )
    .expect("insert child sync log failed");
    let inserts = Spi::get_one::<i64>("SELECT inserts_applied FROM graph.apply_sync()")
        .expect("apply mmap insert sync failed")
        .unwrap_or(0);
    let child_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Child',
                'graph_test_mmap_sync_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )",
    )
    .expect("child search after mmap sync failed")
    .unwrap_or(0);
    let reaches_root = Spi::get_one::<i64>(
            "SELECT count(*)
             FROM graph.traverse('graph_test_mmap_sync_pgtest'::regclass, 'child', 1, hydrate := false)
             WHERE node_id = 'root'",
        )
        .expect("mmap edge overlay traverse failed")
        .unwrap_or(0);
    let materialized = crate::ENGINE.with(|e| !e.borrow().node_store.is_mmap_backed());

    assert_eq!(inserts, 1);
    assert_eq!(child_count, 1);
    assert_eq!(reaches_root, 1);
    assert!(materialized);

    Spi::run("TRUNCATE public.graph_test_mmap_sync_pgtest")
        .expect("truncate mmap sync source table failed");
    Spi::run(
        "INSERT INTO graph._sync_log (op, table_oid, table_name)
             VALUES (
                'T',
                'public.graph_test_mmap_sync_pgtest'::regclass,
                'public.graph_test_mmap_sync_pgtest'
             )",
    )
    .expect("insert truncate sync log failed");
    Spi::run("SELECT * FROM graph.apply_sync()").expect("apply mmap truncate sync failed");
    let remaining = Spi::get_one::<i64>(
        "SELECT (
                SELECT count(*)
                FROM graph.search(
                    'name',
                    'Root',
                    'graph_test_mmap_sync_pgtest'::regclass,
                    mode := 'exact',
                    hydrate := false
                )
             ) + (
                SELECT count(*)
                FROM graph.search(
                    'name',
                    'Child',
                    'graph_test_mmap_sync_pgtest'::regclass,
                    mode := 'exact',
                    hydrate := false
                )
             )",
    )
    .expect("remaining search after truncate failed")
    .unwrap_or(0);

    assert_eq!(remaining, 0);
    Spi::run("SET graph.persist_on_build = off").expect("restore persist_on_build failed");
}

#[pg_test]
fn maintenance_applies_trigger_sync_without_query_time_mutation() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_auto_sync_pgtest CASCADE")
        .expect("drop auto sync table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_auto_sync_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create auto sync table failed");
    Spi::run("INSERT INTO public.graph_test_auto_sync_pgtest (id, name) VALUES ('one', 'Before')")
        .expect("insert initial row failed");
    super::insert_registered_table("public.graph_test_auto_sync_pgtest", "id", "name", None)
        .expect("insert registered auto sync table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    Spi::run("INSERT INTO public.graph_test_auto_sync_pgtest (id, name) VALUES ('two', 'After')")
        .expect("insert synced row failed");

    let stale_found = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'After',
                'graph_test_auto_sync_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )",
    )
    .expect("stale search failed")
    .unwrap_or(0);
    let pending_before = Spi::get_one::<i64>("SELECT pending_sync_rows FROM graph.status()")
        .expect("pending status failed")
        .unwrap_or(0);

    let maintenance_status =
        Spi::get_one::<String>("SELECT status FROM graph.maintenance(concurrently := false)")
            .expect("maintenance failed")
            .unwrap_or_default();

    let found = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'After',
                'graph_test_auto_sync_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )",
    )
    .expect("maintained search failed")
    .unwrap_or(0);
    let applied = Spi::get_one::<i64>("SELECT applied_sync_id FROM graph.status()")
        .expect("status failed")
        .unwrap_or(0);

    assert_eq!(stale_found, 1);
    assert!(pending_before > 0);
    assert_eq!(maintenance_status, "completed");
    assert_eq!(found, 1);
    assert!(applied > 0);
}

#[pg_test]
fn traverse_auto_sync_opt_in_applies_pending_edge_insert() {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SET graph.query_freshness = 'apply_pending_sync'")
        .expect("set query freshness failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_traverse_auto_sync_pgtest CASCADE")
        .expect("drop traverse auto sync table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_traverse_auto_sync_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL REFERENCES public.graph_test_traverse_auto_sync_pgtest(id),
                name TEXT NOT NULL
            )",
    )
    .expect("create traverse auto sync table failed");
    Spi::run(
        "INSERT INTO public.graph_test_traverse_auto_sync_pgtest (id, parent_id, name)
             VALUES ('root', NULL, 'Root')",
    )
    .expect("insert traverse auto sync root failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_traverse_auto_sync_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'parent_id']
            )",
    )
    .expect("add traverse auto sync table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_traverse_auto_sync_pgtest'::regclass,
                'parent_id',
                'graph_test_traverse_auto_sync_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := false
            )",
    )
    .expect("add traverse auto sync edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "INSERT INTO public.graph_test_traverse_auto_sync_pgtest (id, parent_id, name)
             VALUES ('child', 'root', 'Child')",
    )
    .expect("insert pending child failed");

    let reaches_root = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_traverse_auto_sync_pgtest'::regclass,
                'child',
                1,
                hydrate := false
             )
             WHERE node_id = 'root'",
    )
    .expect("auto-sync traversal failed")
    .unwrap_or(0);
    let pending = Spi::get_one::<i64>("SELECT pending_sync_rows FROM graph.status()")
        .expect("status failed")
        .unwrap_or(-1);

    assert_eq!(reaches_root, 1);
    assert_eq!(pending, 0);
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
}

#[pg_test]
fn topology_reads_auto_sync_by_default() {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_default_auto_sync_pgtest CASCADE")
        .expect("drop default auto sync table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_default_auto_sync_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL REFERENCES public.graph_test_default_auto_sync_pgtest(id),
                name TEXT NOT NULL
            )",
    )
    .expect("create default auto sync table failed");
    Spi::run(
        "INSERT INTO public.graph_test_default_auto_sync_pgtest (id, parent_id, name)
             VALUES ('root', NULL, 'Root')",
    )
    .expect("insert default auto sync root failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_default_auto_sync_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'parent_id']
            )",
    )
    .expect("add default auto sync table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_default_auto_sync_pgtest'::regclass,
                'parent_id',
                'graph_test_default_auto_sync_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := false
            )",
    )
    .expect("add default auto sync edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "INSERT INTO public.graph_test_default_auto_sync_pgtest (id, parent_id, name)
             VALUES ('default-child', 'root', 'Default Child')",
    )
    .expect("insert pending default child failed");

    let sees_child = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_default_auto_sync_pgtest'::regclass,
                'default-child',
                0,
                hydrate := false
             )
             WHERE node_id = 'default-child'",
    )
    .expect("default auto-sync traversal failed")
    .unwrap_or(0);
    let pending = Spi::get_one::<i64>("SELECT pending_sync_rows FROM graph.status()")
        .expect("status failed")
        .unwrap_or(-1);

    assert_eq!(sees_child, 1);
    assert_eq!(pending, 0);
}

#[pg_test]
fn traverse_auto_sync_replay_error_fails_closed() {
    reset_and_create_fixtures();
    super::insert_registered_table("public.graph_test_users_pgtest", "id", "name", None)
        .expect("insert registered users table failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SET graph.query_freshness = 'apply_pending_sync'")
        .expect("set query freshness failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "INSERT INTO graph._sync_log (op, table_oid, table_name, new_pk)
             VALUES (
                'X',
                'public.graph_test_users_pgtest'::regclass,
                'public.graph_test_users_pgtest',
                'bad'
             )",
    )
    .expect("insert invalid sync row failed");

    assert!(sql_raises(
        "SELECT count(*)
             FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 1, hydrate := false)"
    ));
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
}

fn setup_topology_auto_sync_fixture() -> i64 {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SET graph.query_freshness = 'apply_pending_sync'")
        .expect("set query freshness failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_topology_auto_sync_pgtest CASCADE")
        .expect("drop topology auto sync table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_topology_auto_sync_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL REFERENCES public.graph_test_topology_auto_sync_pgtest(id),
                name TEXT NOT NULL,
                cost INT NOT NULL DEFAULT 1
            )",
    )
    .expect("create topology auto sync table failed");
    Spi::run(
        "INSERT INTO public.graph_test_topology_auto_sync_pgtest (id, parent_id, name, cost)
             VALUES
                ('root', NULL, 'Root', 0),
                ('base-child', 'root', 'Base Child', 3)",
    )
    .expect("insert topology base rows failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_topology_auto_sync_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'parent_id', 'cost']
            )",
    )
    .expect("add topology table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_topology_auto_sync_pgtest'::regclass,
                from_column := 'parent_id',
                to_table := 'graph_test_topology_auto_sync_pgtest'::regclass,
                to_column := 'id',
                label := 'parent',
                bidirectional := true,
                weight_column := 'cost'
            )",
    )
    .expect("add topology edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let base_component_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.components()")
        .expect("base components failed")
        .unwrap_or(0);

    base_component_count
}

fn insert_topology_auto_sync_node(id: &str, parent_id: Option<&str>, name: &str) {
    let parent_sql = parent_id
        .map(super::sql_literal)
        .unwrap_or_else(|| "NULL".to_string());
    Spi::run(&format!(
        "INSERT INTO public.graph_test_topology_auto_sync_pgtest (id, parent_id, name, cost)
             VALUES ({}, {}, {}, 1)",
        super::sql_literal(id),
        parent_sql,
        super::sql_literal(name)
    ))
    .expect("insert pending topology node failed");
}

#[pg_test]
fn topology_reads_auto_sync_traversal_and_paths() {
    setup_topology_auto_sync_fixture();

    Spi::run(
        "INSERT INTO public.graph_test_topology_auto_sync_pgtest (id, parent_id, name, cost)
             VALUES ('weighted-node', NULL, 'Weighted Node', 1)",
    )
    .expect("insert pending weighted node failed");
    let weighted_cost = Spi::get_one::<i64>(
        "SELECT total_cost
             FROM graph.weighted_shortest_path(
                'graph_test_topology_auto_sync_pgtest'::regclass,
                'weighted-node',
                'graph_test_topology_auto_sync_pgtest'::regclass,
                'weighted-node'
             )",
    )
    .expect("weighted path failed")
    .unwrap_or(0);

    Spi::run(
        "INSERT INTO public.graph_test_topology_auto_sync_pgtest (id, parent_id, name, cost)
             VALUES ('multi-start-child', 'root', 'Multi Start Child', 7)",
    )
    .expect("insert pending multi-start child failed");
    let multi_start_sees_child = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                ARRAY['graph_test_topology_auto_sync_pgtest'::regclass],
                ARRAY['multi-start-child'::text],
                0,
                hydrate := false
             )
             WHERE node_id = 'multi-start-child'",
    )
    .expect("multi-start traverse failed")
    .unwrap_or(0);

    Spi::run(
        "INSERT INTO public.graph_test_topology_auto_sync_pgtest (id, parent_id, name, cost)
             VALUES ('shortest-node', NULL, 'Shortest Node', 1)",
    )
    .expect("insert pending shortest node failed");
    let shortest_rows = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.shortest_path(
                'graph_test_topology_auto_sync_pgtest'::regclass,
                'shortest-node',
                'graph_test_topology_auto_sync_pgtest'::regclass,
                'shortest-node',
                20,
                hydrate := false
             )",
    )
    .expect("shortest path failed")
    .unwrap_or(0);

    Spi::run(
        "INSERT INTO public.graph_test_topology_auto_sync_pgtest (id, parent_id, name, cost)
             VALUES ('search-node', NULL, 'Search Node', 1)",
    )
    .expect("insert pending search node failed");
    let traverse_search_sees_child = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse_search(
                'name',
                'Search Node',
                table_filter := 'graph_test_topology_auto_sync_pgtest'::regclass,
                search_mode := 'exact',
                max_depth := 0,
                hydrate := false
             )
             WHERE node_id = 'search-node'",
    )
    .expect("traverse_search failed")
    .unwrap_or(0);

    assert_eq!(multi_start_sees_child, 1);
    assert_eq!(shortest_rows, 1);
    assert_eq!(weighted_cost, 0);
    assert_eq!(traverse_search_sees_child, 1);
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
}

#[pg_test]
fn weighted_shortest_path_rejects_pending_edge_overlay_with_pg018() {
    setup_topology_auto_sync_fixture();

    insert_topology_auto_sync_node("pending-weighted-child", Some("root"), "Pending Weighted Child");
    let sqlstate = sqlstate_for_error(
        "SELECT * FROM graph.weighted_shortest_path(
            'graph_test_topology_auto_sync_pgtest'::regclass,
            'root',
            'graph_test_topology_auto_sync_pgtest'::regclass,
            'pending-weighted-child'
         )",
    );

    assert_eq!(sqlstate.as_deref(), Some("PG018"));
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
}

#[pg_test]
fn topology_reads_auto_sync_aggregation_entrypoints() {
    setup_topology_auto_sync_fixture();

    insert_topology_auto_sync_node("aggregate-node", None, "Aggregate Node");
    let aggregate_count = Spi::get_one::<pgrx::JsonB>(
        "WITH req AS (
            SELECT jsonb_build_object(
                'starts',
                jsonb_build_array(graph.node_ref_string('graph_test_topology_auto_sync_pgtest'::regclass, 'aggregate-node')),
                'direction', 'out',
                'min_depth', 0,
                'max_depth', 0,
                'node_tables', jsonb_build_array('graph_test_topology_auto_sync_pgtest')
            ) AS traversal
         )
         SELECT graph.aggregate(
            traversal,
            '{\"count\":[{\"table\":\"graph_test_topology_auto_sync_pgtest\",\"column\":\"id\",\"as\":\"node_count\"}]}'::jsonb
         )
         FROM req",
    )
    .expect("auto-sync aggregate failed")
    .expect("aggregate result missing")
    .0
    .get("node_count")
    .and_then(|value| value.as_i64())
    .unwrap_or(0);

    insert_topology_auto_sync_node("path-count-node", None, "Path Count Node");
    let exact_path_count = Spi::get_one::<i64>(
        "WITH req AS (
            SELECT jsonb_build_object(
                'starts',
                jsonb_build_array(graph.node_ref_string('graph_test_topology_auto_sync_pgtest'::regclass, 'path-count-node')),
                'direction', 'out',
                'min_depth', 0,
                'max_depth', 0,
                'node_tables', jsonb_build_array('graph_test_topology_auto_sync_pgtest')
            ) AS traversal
         )
         SELECT estimated_paths
         FROM graph.path_count_estimate((SELECT traversal FROM req))
         WHERE exact AND NOT capped",
    )
    .expect("auto-sync path_count_estimate failed")
    .unwrap_or(0);
    let pending = Spi::get_one::<i64>("SELECT pending_sync_rows FROM graph.status()")
        .expect("status failed")
        .unwrap_or(-1);

    assert_eq!(aggregate_count, 1);
    assert_eq!(exact_path_count, 1);
    assert_eq!(pending, 0);
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
}

#[pg_test]
fn topology_reads_auto_sync_component_entrypoints() {
    let base_component_count = setup_topology_auto_sync_fixture();

    insert_topology_auto_sync_node("stats-node", None, "Stats Node");
    let total_active_nodes = Spi::get_one::<i32>("SELECT total_active_nodes FROM graph.component_stats()")
        .expect("component stats failed")
        .unwrap_or(0);

    insert_topology_auto_sync_node("connected-node", None, "Connected Node");
    let connected_components_sees_node = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.connected_components()
             WHERE node_id = 'connected-node'
               AND component_size = 1",
    )
    .expect("connected_components failed")
    .unwrap_or(0);

    insert_topology_auto_sync_node("summary-node", None, "Summary Node");
    let component_count_after_isolate = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.components()",
    )
    .expect("components failed")
    .unwrap_or(0);

    insert_topology_auto_sync_node("isolated-node", None, "Isolated Node");
    let isolated_nodes_sees_node = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.isolated_nodes(hydrate := false)
             WHERE node_id = 'isolated-node'",
    )
    .expect("isolated_nodes failed")
    .unwrap_or(0);

    let root_component_id = Spi::get_one::<i64>(
        "SELECT component_id
             FROM graph.connected_components()
             WHERE node_id = 'root'",
    )
    .expect("root component lookup failed")
    .expect("root component id missing");
    let root_component_rows = Spi::get_one::<i64>(
        &format!(
            "SELECT count(*)
                 FROM graph.component({root_component_id}, hydrate := false)
                 WHERE node_id = 'root'"
        ),
    )
    .expect("component failed")
    .unwrap_or(0);

    assert_eq!(total_active_nodes, 3);
    assert_eq!(connected_components_sees_node, 1);
    assert!(component_count_after_isolate > base_component_count);
    assert_eq!(isolated_nodes_sees_node, 1);
    assert_eq!(root_component_rows, 1);
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
}

#[pg_test]
fn topology_reads_error_on_pending_covers_topology_but_not_search() {
    setup_topology_auto_sync_fixture();
    let root_component_id = Spi::get_one::<i64>(
        "SELECT component_id
             FROM graph.connected_components()
             WHERE node_id = 'root'",
    )
    .expect("root component lookup failed")
    .expect("root component id missing");

    Spi::run(
        "INSERT INTO public.graph_test_topology_auto_sync_pgtest (id, parent_id, name, cost)
             VALUES ('error-node', NULL, 'Error Node', 1)",
    )
    .expect("insert pending error node failed");
    Spi::run("SET graph.query_freshness = 'error_on_pending'")
        .expect("set error_on_pending failed");
    assert!(sql_raises(
        "SELECT * FROM graph.traverse(
            ARRAY['graph_test_topology_auto_sync_pgtest'::regclass],
            ARRAY['error-node'::text],
            0,
            hydrate := false
         )"
    ));
    assert!(sql_raises(
        "SELECT * FROM graph.shortest_path(
            'graph_test_topology_auto_sync_pgtest'::regclass,
            'error-node',
            'graph_test_topology_auto_sync_pgtest'::regclass,
            'error-node',
            20,
            hydrate := false
         )"
    ));
    assert!(sql_raises(
        "SELECT * FROM graph.weighted_shortest_path(
            'graph_test_topology_auto_sync_pgtest'::regclass,
            'error-node',
            'graph_test_topology_auto_sync_pgtest'::regclass,
            'error-node'
         )"
    ));
    assert!(sql_raises("SELECT * FROM graph.component_stats()"));
    assert!(sql_raises("SELECT * FROM graph.connected_components()"));
    assert!(sql_raises("SELECT * FROM graph.components()"));
    assert!(sql_raises("SELECT * FROM graph.isolated_nodes(hydrate := false)"));
    assert!(sql_raises("SELECT * FROM graph.largest_component(hydrate := false)"));
    assert!(sql_raises(&format!(
        "SELECT * FROM graph.component({root_component_id}, hydrate := false)"
    )));
    assert!(sql_raises(
        "WITH req AS (
            SELECT jsonb_build_object(
                'starts',
                jsonb_build_array(graph.node_ref_string('graph_test_topology_auto_sync_pgtest'::regclass, 'error-node')),
                'direction', 'out',
                'min_depth', 0,
                'max_depth', 0,
                'node_tables', jsonb_build_array('graph_test_topology_auto_sync_pgtest')
            ) AS traversal
         )
         SELECT graph.aggregate(
            traversal,
            '{\"count\":[{\"table\":\"graph_test_topology_auto_sync_pgtest\",\"column\":\"id\",\"as\":\"node_count\"}]}'::jsonb
         )
         FROM req"
    ));
    assert!(sql_raises(
        "WITH req AS (
            SELECT jsonb_build_object(
                'starts',
                jsonb_build_array(graph.node_ref_string('graph_test_topology_auto_sync_pgtest'::regclass, 'error-node')),
                'direction', 'out',
                'min_depth', 0,
                'max_depth', 0,
                'node_tables', jsonb_build_array('graph_test_topology_auto_sync_pgtest')
            ) AS traversal
         )
         SELECT *
         FROM graph.path_count_estimate((SELECT traversal FROM req))"
    ));
    assert!(sql_raises(
        "SELECT * FROM graph.traverse_search(
            'name',
            'Error Node',
            table_filter := 'graph_test_topology_auto_sync_pgtest'::regclass,
            search_mode := 'exact',
            max_depth := 0,
            hydrate := false
         )"
    ));
    assert!(!sql_raises(
        "SELECT * FROM graph.search(
            'name',
            'Error Node',
            table_filter := 'graph_test_topology_auto_sync_pgtest'::regclass,
            mode := 'exact'
         )"
    ));
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
}

#[pg_test]
fn tenant_scope_filters_search_and_traversal() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.enforce_tenant_scope = on").expect("enable tenant enforcement failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_tenant_pgtest CASCADE")
        .expect("drop tenant table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_tenant_pgtest (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                name TEXT NOT NULL,
                parent_id TEXT REFERENCES public.graph_test_tenant_pgtest(id)
            )",
    )
    .expect("create tenant table failed");
    Spi::run(
        "INSERT INTO public.graph_test_tenant_pgtest (id, tenant_id, name, parent_id) VALUES
                ('a1', 'tenant-a', 'Shared Name', NULL),
                ('a2', 'tenant-a', 'Child A', 'a1'),
                ('b1', 'tenant-b', 'Shared Name', NULL),
                ('b2', 'tenant-b', 'Child B', 'b1')",
    )
    .expect("insert tenant rows failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_tenant_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name'],
                tenant_column := 'tenant_id'
            )",
    )
    .expect("add tenant table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_tenant_pgtest'::regclass,
                'parent_id',
                'graph_test_tenant_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := true
            )",
    )
    .expect("add tenant edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let tenant_a_search = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Shared Name',
                'graph_test_tenant_pgtest'::regclass,
                mode := 'exact',
                tenant := 'tenant-a',
                hydrate := false
             )",
    )
    .expect("tenant search failed")
    .unwrap_or(0);
    let tenant_a_traverse = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_tenant_pgtest'::regclass,
                'a1',
                2,
                tenant := 'tenant-a',
                hydrate := false
             )
             WHERE node_id LIKE 'b%'",
    )
    .expect("tenant traverse failed")
    .unwrap_or(0);
    let missing_tenant_rejected = sql_raises(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Shared Name',
                'graph_test_tenant_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )",
    );

    Spi::run("SET graph.tenant_setting = 'app.tenant_id'").expect("set tenant_setting failed");
    Spi::run("SET app.tenant_id = 'tenant-b'").expect("set app tenant failed");
    let session_tenant_search = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Shared Name',
                'graph_test_tenant_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )",
    )
    .expect("session tenant search failed")
    .unwrap_or(0);
    let session_tenant_traverse = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_tenant_pgtest'::regclass,
                'b1',
                2,
                hydrate := false
             )
             WHERE node_id LIKE 'a%'",
    )
    .expect("session tenant traverse failed")
    .unwrap_or(0);
    Spi::run("RESET graph.tenant_setting").expect("reset tenant_setting failed");
    Spi::run("RESET app.tenant_id").expect("reset app tenant failed");

    assert_eq!(tenant_a_search, 1);
    assert_eq!(tenant_a_traverse, 0);
    assert!(missing_tenant_rejected);
    assert_eq!(session_tenant_search, 1);
    assert_eq!(session_tenant_traverse, 0);
}

#[pg_test]
fn catalog_tables_are_extension_owned_on_install() {
    let not_owned = Spi::get_one::<i64>(
        "WITH expected(relname) AS (
             VALUES
                 ('_registered_tables'),
                 ('_registered_edges'),
                 ('_build_jobs'),
                 ('_sync_log'),
                 ('_sync_buffer')
         ),
         owned AS (
             SELECT c.relname, e.extname
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_depend d
               ON d.objid = c.oid
              AND d.deptype = 'e'
             LEFT JOIN pg_extension e
               ON e.oid = d.refobjid
             WHERE n.nspname = 'graph'
               AND c.relname IN (
                   '_registered_tables',
                   '_registered_edges',
                   '_build_jobs',
                   '_sync_log',
                   '_sync_buffer'
               )
         )
         SELECT count(*)
         FROM expected
         LEFT JOIN owned USING (relname)
         WHERE owned.extname IS DISTINCT FROM 'graph'",
    )
    .expect("catalog ownership check failed")
    .unwrap_or(0);

    assert_eq!(not_owned, 0);
}

#[pg_test]
fn status_exposes_v1_contract_field_names() {
    reset_and_create_fixtures();
    let has_v1_prefix = Spi::get_one::<bool>(
            "WITH expected(result_prefix) AS (
                VALUES (
                    'TABLE(node_count integer, edge_count integer, memory_used_mb double precision, memory_limit_mb integer, sync_mode text, sync_status text, last_build timestamp with time zone, last_vacuum timestamp with time zone, edge_types text[], edge_buffer_used integer, has_unidirectional_edges boolean, schema_status text, sync_lag bigint, pending_edge_deltas integer, needs_vacuum boolean, needs_rebuild boolean'
                )
             )
             SELECT left(pg_get_function_result(p.oid), length(expected.result_prefix)) = expected.result_prefix
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             CROSS JOIN expected
             WHERE n.nspname = 'graph'
               AND p.proname = 'status'",
        )
        .expect("status signature inspection failed")
        .unwrap_or(false);

    assert!(has_v1_prefix);
}

#[pg_test]
fn sync_health_exposes_operator_contract_field_names() {
    reset_and_create_fixtures();
    let signature_matches = Spi::get_one::<bool>(
            "WITH expected(result_type) AS (
                VALUES (
                    'TABLE(sync_mode text, query_freshness text, sync_batch_size integer, applied_sync_id bigint, max_sync_log_id bigint, pending_sync_rows bigint, disabled_trigger_count integer, edge_buffer_used integer, edge_buffer_size integer, needs_vacuum boolean, needs_rebuild boolean, read_only boolean, read_only_reason text, projection_mode text, overlay_tombstone_count integer, overlay_memory_bytes bigint, compaction_recommended boolean, tx_delta_dirty boolean, tx_delta_added_nodes integer, tx_delta_deleted_nodes integer, tx_delta_added_edges integer, tx_delta_deleted_edges integer, tx_delta_memory_bytes bigint, apply_sync_recommended boolean, maintenance_recommended boolean)'
                )
             )
             SELECT pg_get_function_result(p.oid) = expected.result_type
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             CROSS JOIN expected
             WHERE n.nspname = 'graph'
               AND p.proname = 'sync_health'",
        )
        .expect("sync_health signature inspection failed")
        .unwrap_or(false);

    assert!(signature_matches);
}

#[pg_test]
fn scheduled_maintenance_exposes_operator_contract_field_names() {
    reset_and_create_fixtures();
    let signature_matches = Spi::get_one::<bool>(
            "WITH expected(result_type) AS (
                VALUES (
                    'TABLE(applied_sync boolean, maintenance_started boolean, maintenance_job_id text, pending_sync_rows bigint, edge_buffer_used integer, message text)'
                )
             )
             SELECT pg_get_function_result(p.oid) = expected.result_type
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             CROSS JOIN expected
             WHERE n.nspname = 'graph'
               AND p.proname = 'run_scheduled_maintenance'",
        )
        .expect("run_scheduled_maintenance signature inspection failed")
        .unwrap_or(false);

    assert!(signature_matches);
}

#[pg_test]
fn ingest_projection_exposes_operator_contract_field_names() {
    reset_and_create_fixtures();
    let signature_matches = Spi::get_one::<bool>(
            "WITH expected(result_type) AS (
                VALUES (
                    'TABLE(rows_ingested bigint, segments_published bigint, sync_watermark bigint)'
                )
             )
             SELECT pg_get_function_result(p.oid) = expected.result_type
                AND pg_get_function_arguments(p.oid) = 'max_rows bigint DEFAULT NULL::bigint, max_bytes bigint DEFAULT NULL::bigint'
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             CROSS JOIN expected
             WHERE n.nspname = 'graph'
               AND p.proname = 'ingest_projection'",
        )
        .expect("ingest_projection signature inspection failed")
        .unwrap_or(false);

    assert!(signature_matches);
}

#[pg_test]
fn ingest_projection_publishes_committed_sync_log_rows() {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set trigger sync failed");
    Spi::run("SET graph.persist_on_build = on").expect("enable persist_on_build failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_projection_ingest_pgtest CASCADE")
        .expect("drop projection ingest table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_projection_ingest_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL
                    REFERENCES public.graph_test_projection_ingest_pgtest(id),
                score BIGINT NOT NULL,
                tenant_id TEXT NOT NULL
            )",
    )
    .expect("create projection ingest table failed");
    Spi::run(
        "INSERT INTO public.graph_test_projection_ingest_pgtest (id, parent_id, score, tenant_id)
             VALUES ('root', NULL, 1, 'tenant-a')",
    )
    .expect("insert projection ingest root failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_projection_ingest_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['parent_id', 'score'],
                tenant_column := 'tenant_id'
            )",
    )
    .expect("add projection ingest table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_projection_ingest_pgtest'::regclass,
                from_column := 'parent_id',
                to_table := 'graph_test_projection_ingest_pgtest'::regclass,
                to_column := 'id',
                label := 'parent',
                bidirectional := false
            )",
    )
    .expect("add projection ingest edge failed");
    Spi::run(
        "SELECT graph.add_filter_column(
                'graph_test_projection_ingest_pgtest'::regclass,
                'score',
                column_type := 'numeric'
            )",
    )
    .expect("add projection ingest filter failed");
    Spi::run("SELECT * FROM graph.build()").expect("build projection ingest graph failed");
    let initial = Spi::get_one::<i64>("SELECT rows_ingested FROM graph.ingest_projection()")
        .expect("initial ingest projection query failed")
        .unwrap_or(-1);
    assert_eq!(
        initial, 0,
        "initial projection ingest must not replay rows included in persisted build"
    );
    Spi::run(
        "INSERT INTO public.graph_test_projection_ingest_pgtest (id, parent_id, score, tenant_id)
             VALUES ('child', 'root', 99, 'tenant-a')",
    )
    .expect("insert projection ingest child failed");
    Spi::run("SELECT * FROM graph.apply_sync()").expect("apply projection ingest sync failed");

    let (rows_ingested, segments_published, sync_watermark) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT rows_ingested, segments_published, sync_watermark
                   FROM graph.ingest_projection()",
                None,
                &[],
            )
            .expect("ingest_projection query failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<i64>(1)?.unwrap_or(0),
            row.get::<i64>(2)?.unwrap_or(0),
            row.get::<i64>(3)?.unwrap_or(0),
        ))
    })
    .expect("ingest_projection row read failed");
    let max_sync_id = Spi::get_one::<i64>("SELECT max(id) FROM graph._sync_log")
        .expect("max sync id read failed")
        .unwrap_or(0);

    assert!(rows_ingested >= 3);
    assert!(segments_published >= 2);
    assert_eq!(sync_watermark, max_sync_id);
    Spi::run("SET graph.persist_on_build = off").expect("reset persist_on_build failed");
    Spi::run("RESET graph.sync_mode").expect("reset sync mode failed");
}

#[pg_test]
fn ingest_projection_advances_watermark_for_no_row_sync_batches() {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set trigger sync failed");
    Spi::run("SET graph.persist_on_build = on").expect("enable persist_on_build failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_projection_no_rows_pgtest CASCADE")
        .expect("drop projection no-row table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_projection_no_rows_pgtest (
                id TEXT PRIMARY KEY,
                note TEXT NOT NULL
            )",
    )
    .expect("create projection no-row table failed");
    Spi::run(
        "INSERT INTO public.graph_test_projection_no_rows_pgtest (id, note)
             VALUES ('a', 'A')",
    )
    .expect("insert projection no-row root failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_projection_no_rows_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['note']
            )",
    )
    .expect("add projection no-row table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build projection no-row graph failed");
    Spi::run("TRUNCATE public.graph_test_projection_no_rows_pgtest")
        .expect("truncate projection no-row table failed");
    Spi::run("SELECT * FROM graph.apply_sync()").expect("apply projection no-row sync failed");

    let (rows_ingested, segments_published, sync_watermark) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT rows_ingested, segments_published, sync_watermark
                   FROM graph.ingest_projection()",
                None,
                &[],
            )
            .expect("ingest no-row projection query failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<i64>(1)?.unwrap_or(-1),
            row.get::<i64>(2)?.unwrap_or(-1),
            row.get::<i64>(3)?.unwrap_or(-1),
        ))
    })
    .expect("ingest no-row projection row read failed");
    let max_sync_id = Spi::get_one::<i64>("SELECT max(id) FROM graph._sync_log")
        .expect("max sync id read failed")
        .unwrap_or(0);
    let repeat_rows = Spi::get_one::<i64>("SELECT rows_ingested FROM graph.ingest_projection()")
        .expect("repeat ingest no-row projection query failed")
        .unwrap_or(-1);

    assert_eq!(rows_ingested, 0);
    assert_eq!(segments_published, 0);
    assert_eq!(sync_watermark, max_sync_id);
    assert_eq!(repeat_rows, 0);
    Spi::run("SET graph.persist_on_build = off").expect("reset persist_on_build failed");
    Spi::run("RESET graph.sync_mode").expect("reset sync mode failed");
}

#[pg_test]
fn scheduled_maintenance_noops_when_graph_is_healthy() {
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

    let (applied_sync, maintenance_started, job_id, pending, edge_buffer_used, message) =
        Spi::connect(|client| {
            let result = client
                .select(
                    "SELECT applied_sync,
                            maintenance_started,
                            maintenance_job_id,
                            pending_sync_rows,
                            edge_buffer_used,
                            message
                       FROM graph.run_scheduled_maintenance()",
                    None,
                    &[],
                )
                .expect("scheduled maintenance query failed");
            let row = result.first();
            Ok::<_, pgrx::spi::Error>((
                row.get::<bool>(1)?.unwrap_or(true),
                row.get::<bool>(2)?.unwrap_or(true),
                row.get::<String>(3)?,
                row.get::<i64>(4)?.unwrap_or(-1),
                row.get::<i32>(5)?.unwrap_or(-1),
                row.get::<String>(6)?.unwrap_or_default(),
            ))
        })
        .expect("scheduled maintenance row read failed");

    assert!(!applied_sync);
    assert!(!maintenance_started);
    assert!(job_id.is_none());
    assert_eq!(pending, 0);
    assert_eq!(edge_buffer_used, 0);
    assert_eq!(message, "no scheduled graph maintenance needed");
}

#[pg_test]
fn scheduled_maintenance_applies_sync_and_starts_overlay_maintenance() {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SET graph.query_freshness = 'off'").expect("set query freshness failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_scheduled_maintenance_pgtest CASCADE")
        .expect("drop scheduled maintenance table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_scheduled_maintenance_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL
                    REFERENCES public.graph_test_scheduled_maintenance_pgtest(id),
                name TEXT NOT NULL
            )",
    )
    .expect("create scheduled maintenance table failed");
    Spi::run(
        "INSERT INTO public.graph_test_scheduled_maintenance_pgtest (id, parent_id, name)
             VALUES ('root', NULL, 'Root')",
    )
    .expect("insert scheduled maintenance root failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_scheduled_maintenance_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'parent_id']
            )",
    )
    .expect("add scheduled maintenance table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_scheduled_maintenance_pgtest'::regclass,
                from_column := 'parent_id',
                to_table := 'graph_test_scheduled_maintenance_pgtest'::regclass,
                to_column := 'id',
                label := 'parent',
                bidirectional := false
            )",
    )
    .expect("add scheduled maintenance edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "INSERT INTO public.graph_test_scheduled_maintenance_pgtest (id, parent_id, name)
             VALUES ('child', 'root', 'Child')",
    )
    .expect("insert scheduled maintenance child failed");

    let (applied_sync, maintenance_started, job_id, message) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT applied_sync, maintenance_started, maintenance_job_id, message
                   FROM graph.run_scheduled_maintenance()",
                None,
                &[],
            )
            .expect("scheduled maintenance query failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<bool>(1)?.unwrap_or(false),
            row.get::<bool>(2)?.unwrap_or(false),
            row.get::<String>(3)?,
            row.get::<String>(4)?.unwrap_or_default(),
        ))
    })
    .expect("scheduled maintenance read failed");
    let job_id = job_id.expect("scheduled maintenance did not return a job id");
    let job_exists = Spi::get_one::<bool>(&format!(
        "SELECT EXISTS (
                SELECT 1
                FROM graph.maintenance_status({})
                WHERE status IN ('queued', 'running', 'completed', 'failed')
             )",
        super::sql_literal(&job_id)
    ))
    .expect("scheduled maintenance job status failed")
    .unwrap_or(false);

    assert!(applied_sync);
    assert!(maintenance_started);
    assert_eq!(message, "applied sync and started maintenance");
    assert!(job_exists);
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
    Spi::run("RESET graph.sync_mode").expect("reset sync mode failed");
}

#[pg_test]
fn sync_health_recommends_apply_then_maintenance_for_edge_overlay() {
    reset_and_create_fixtures();
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set sync_mode failed");
    Spi::run("SET graph.query_freshness = 'off'").expect("set query freshness failed");
    Spi::run("SET graph.sync_batch_size = 3").expect("set sync batch size failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_sync_health_pgtest CASCADE")
        .expect("drop sync health table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_sync_health_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT NULL REFERENCES public.graph_test_sync_health_pgtest(id),
                name TEXT NOT NULL
            )",
    )
    .expect("create sync health table failed");
    Spi::run(
        "INSERT INTO public.graph_test_sync_health_pgtest (id, parent_id, name)
             VALUES ('root', NULL, 'Root')",
    )
    .expect("insert sync health root failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_sync_health_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'parent_id']
            )",
    )
    .expect("add sync health table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_sync_health_pgtest'::regclass,
                from_column := 'parent_id',
                to_table := 'graph_test_sync_health_pgtest'::regclass,
                to_column := 'id',
                label := 'parent',
                bidirectional := false
            )",
    )
    .expect("add sync health edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "INSERT INTO public.graph_test_sync_health_pgtest (id, parent_id, name)
             VALUES ('child', 'root', 'Child')",
    )
    .expect("insert pending sync health child failed");

    let (pending_before, apply_before, maintenance_before, batch_size, freshness) =
        Spi::connect(|client| {
            let result = client
                .select(
                    "SELECT pending_sync_rows,
                            apply_sync_recommended,
                            maintenance_recommended,
                            sync_batch_size,
                            query_freshness
                       FROM graph.sync_health()",
                    None,
                    &[],
                )
                .expect("sync health query failed");
            let row = result.first();
            Ok::<_, pgrx::spi::Error>((
                row.get::<i64>(1)?.unwrap_or(0),
                row.get::<bool>(2)?.unwrap_or(false),
                row.get::<bool>(3)?.unwrap_or(true),
                row.get::<i32>(4)?.unwrap_or(0),
                row.get::<String>(5)?.unwrap_or_default(),
            ))
        })
        .expect("sync health read failed");

    assert_eq!(pending_before, 1);
    assert!(apply_before);
    assert!(!maintenance_before);
    assert_eq!(batch_size, 3);
    assert_eq!(freshness, "off");

    let inserts = Spi::get_one::<i64>("SELECT inserts_applied FROM graph.apply_sync()")
        .expect("apply sync failed")
        .unwrap_or(0);
    let (pending_after, apply_after, maintenance_after, edge_buffer_used, needs_vacuum) =
        Spi::connect(|client| {
            let result = client
                .select(
                    "SELECT pending_sync_rows,
                            apply_sync_recommended,
                            maintenance_recommended,
                            edge_buffer_used,
                            needs_vacuum
                       FROM graph.sync_health()",
                    None,
                    &[],
                )
                .expect("sync health after apply query failed");
            let row = result.first();
            Ok::<_, pgrx::spi::Error>((
                row.get::<i64>(1)?.unwrap_or(-1),
                row.get::<bool>(2)?.unwrap_or(true),
                row.get::<bool>(3)?.unwrap_or(false),
                row.get::<i32>(4)?.unwrap_or(0),
                row.get::<bool>(5)?.unwrap_or(false),
            ))
        })
        .expect("sync health after apply read failed");

    assert_eq!(inserts, 1);
    assert_eq!(pending_after, 0);
    assert!(!apply_after);
    assert!(maintenance_after);
    assert!(edge_buffer_used > 0);
    assert!(needs_vacuum);
    Spi::run("RESET graph.query_freshness").expect("reset query freshness failed");
    Spi::run("RESET graph.sync_batch_size").expect("reset sync batch size failed");
}

#[pg_test]
fn admin_remove_apis_update_catalog_side_effects() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.add_table('graph_test_users_pgtest'::regclass, 'id', ARRAY['name'])")
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
    .expect("add edge failed");
    Spi::run("SELECT graph.add_filter_column('graph_test_users_pgtest'::regclass, 'age')")
        .expect("add filter column failed");

    Spi::run("SELECT graph.remove_edge('friend')").expect("remove edge failed");
    let edge_removed = Spi::get_one::<bool>(
        "SELECT NOT EXISTS (
                SELECT 1 FROM graph._registered_edges WHERE label = 'friend'
            )",
    )
    .expect("edge removal inspection failed")
    .unwrap_or(false);
    assert!(edge_removed);

    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend'
            )",
    )
    .expect("re-add edge failed");
    Spi::run("SELECT graph.remove_table('graph_test_users_pgtest'::regclass)")
        .expect("remove table failed");
    let cleanup_complete = Spi::get_one::<bool>(
        "SELECT NOT EXISTS (
                    SELECT 1 FROM graph._registered_tables
                    WHERE table_name = 'graph_test_users_pgtest'
                )
                AND NOT EXISTS (
                    SELECT 1 FROM graph._registered_filter_columns
                    WHERE table_name = 'graph_test_users_pgtest'
                )
                AND NOT EXISTS (
                    SELECT 1 FROM graph._registered_edges
                    WHERE from_table = 'graph_test_users_pgtest'
                       OR to_table = 'graph_test_users_pgtest'
                )",
    )
    .expect("table removal inspection failed")
    .unwrap_or(false);

    assert!(cleanup_complete);
}

#[pg_test]
fn failed_apply_sync_rows_remain_buffered() {
    reset_and_create_fixtures();
    super::insert_registered_table("public.graph_test_users_pgtest", "id", "name", None)
        .expect("insert registered users table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run("SELECT graph.enable_sync()").expect("enable sync failed");
    Spi::run("DELETE FROM graph._sync_buffer").expect("clear sync buffer failed");
    Spi::run(
        "INSERT INTO graph._sync_buffer (op, table_name, pk, old_pk, new_pk, properties)
             VALUES ('U', 'public.graph_test_users_pgtest', 'missing', 'missing', 'missing',
                     '{\"name\":\"Nobody\"}'::jsonb)",
    )
    .expect("insert failed sync row failed");

    let updates = Spi::get_one::<i64>("SELECT updates_applied FROM graph.apply_sync()")
        .expect("apply sync failed")
        .unwrap_or(0);
    let remaining = Spi::get_one::<i64>("SELECT count(*) FROM graph._sync_buffer")
        .expect("sync buffer count failed")
        .unwrap_or(0);

    assert_eq!(updates, 0);
    assert_eq!(remaining, 1);
}

#[pg_test]
fn failed_legacy_sync_rows_do_not_block_later_valid_rows() {
    reset_and_create_fixtures();
    super::insert_registered_table("public.graph_test_users_pgtest", "id", "name", None)
        .expect("insert registered users table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run("SELECT graph.enable_sync()").expect("enable sync failed");
    Spi::run("DELETE FROM graph._sync_buffer").expect("clear sync buffer failed");
    Spi::run("SET graph.sync_batch_size = 1").expect("set sync batch size failed");
    Spi::run(
        "INSERT INTO graph._sync_buffer (op, table_name, pk, old_pk, new_pk, properties)
             VALUES
                ('U', 'public.graph_test_users_pgtest', 'missing', 'missing', 'missing',
                 '{\"name\":\"Nobody\"}'::jsonb),
                ('U', 'public.graph_test_users_pgtest', 'u1', 'u1', 'u1',
                 '{\"name\":\"Alice Updated\"}'::jsonb)",
    )
    .expect("insert mixed legacy sync rows failed");

    let updates = Spi::get_one::<i64>("SELECT updates_applied FROM graph.apply_sync()")
        .expect("apply sync failed")
        .unwrap_or(0);
    let remaining_missing = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph._sync_buffer
             WHERE COALESCE(new_pk, pk) = 'missing'",
    )
    .expect("remaining missing count failed")
    .unwrap_or(0);
    let remaining_valid = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph._sync_buffer
             WHERE COALESCE(new_pk, pk) = 'u1'",
    )
    .expect("remaining valid count failed")
    .unwrap_or(0);

    assert_eq!(updates, 1);
    assert_eq!(remaining_missing, 1);
    assert_eq!(remaining_valid, 0);
    Spi::run("RESET graph.sync_batch_size").expect("reset sync batch size failed");
}
