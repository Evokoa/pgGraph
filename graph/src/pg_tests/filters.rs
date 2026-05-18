#[pg_test]
fn traverse_accepts_structured_jsonb_numeric_filters() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.add_table('graph_test_users_pgtest'::regclass, 'id', ARRAY['name'])")
        .expect("add table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend'
            )",
    )
    .expect("add edge-table edge failed");
    Spi::run("SELECT graph.add_filter_column('graph_test_users_pgtest'::regclass, 'age')")
        .expect("add age filter failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let matching_neighbor = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := '{\"node\":{\"where\":{\"age\":{\"gt\":40}}}}'::jsonb,
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("structured filter traverse failed")
    .unwrap_or(0);
    let excluded_neighbor = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := '{\"node\":{\"where\":{\"age\":{\"gt\":100}}}}'::jsonb,
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("structured exclusion traverse failed")
    .unwrap_or(0);

    assert_eq!(matching_neighbor, 1);
    assert_eq!(excluded_neighbor, 0);
}

#[pg_test]
fn traverse_verifies_unindexed_filters_during_hydration_before_pagination() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('u3', 'Cara', 29)",
    )
    .expect("insert extra user failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
             VALUES ('f2', 'u1', 'u3')",
    )
    .expect("insert extra friendship failed");
    Spi::run("SELECT graph.add_table('graph_test_users_pgtest'::regclass, 'id', ARRAY['name'])")
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
    .expect("add edge-table edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let (matching_id, node_is_null) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT node_id, node IS NULL
                     FROM graph.traverse(
                        'graph_test_users_pgtest'::regclass,
                        'u1',
                        1,
                        filter := '{\"node\":{\"where\":{\"name\":{\"eq\":\"Cara\"}}}}'::jsonb,
                        hydrate := false,
                        max_rows := 1,
                        row_offset := 0
                     )",
                None,
                &[],
            )
            .expect("hydration filter traverse failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<String>(1)
                .expect("node_id read failed")
                .unwrap_or_default(),
            row.get::<bool>(2)
                .expect("node null read failed")
                .unwrap_or(false),
        ))
    })
    .expect("hydration filter result read failed");

    assert_eq!(matching_id, "u3");
    assert!(node_is_null);
}

#[pg_test]
fn traverse_hydration_filters_compare_large_numbers_exactly() {
    reset_and_create_fixtures();
    Spi::run(
        "ALTER TABLE public.graph_test_users_pgtest
                ADD COLUMN exact_score BIGINT,
                ADD COLUMN score_text TEXT",
    )
    .expect("add exact score columns failed");
    Spi::run(
        "UPDATE public.graph_test_users_pgtest
	             SET exact_score = CASE
	                    WHEN id = 'u1' THEN 9007199254740992
	                    ELSE 9007199254740993
	                 END,
	                 score_text = CASE
	                    WHEN id = 'u1' THEN '9007199254740992'
	                    ELSE '9007199254740993'
	                 END",
    )
    .expect("update exact score columns failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'age', 'exact_score', 'score_text']
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
    .expect("add edge-table edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let exact_numeric_match = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := '{\"node\":{\"where\":{\"exact_score\":{\"eq\":9007199254740993}}}}'::jsonb,
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("exact numeric filter traverse failed")
    .unwrap_or(0);
    let rounded_numeric_miss = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := '{\"node\":{\"where\":{\"exact_score\":{\"eq\":9007199254740992}}}}'::jsonb,
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("rounded numeric filter traverse failed")
    .unwrap_or(0);
    let string_numeric_miss = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := '{\"node\":{\"where\":{\"score_text\":{\"eq\":9007199254740993}}}}'::jsonb,
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("string numeric mismatch traverse failed")
    .unwrap_or(0);

    assert_eq!(exact_numeric_match, 1);
    assert_eq!(rounded_numeric_miss, 0);
    assert_eq!(string_numeric_miss, 0);
}

#[pg_test]
fn traverse_pushes_registered_typed_filters_into_memory_index() {
    reset_and_create_fixtures();
    Spi::run(
        "ALTER TABLE public.graph_test_users_pgtest
                ADD COLUMN active BOOLEAN,
                ADD COLUMN joined_on DATE,
                ADD COLUMN seen_at TIMESTAMPTZ,
                ADD COLUMN owner_uuid UUID,
                ADD COLUMN risk_score BIGINT,
                ADD COLUMN nullable_note TEXT,
                ADD COLUMN prefs JSONB,
                ADD COLUMN labels TEXT[]",
    )
    .expect("add typed filter columns failed");
    Spi::run(
            "UPDATE public.graph_test_users_pgtest
             SET active = (id = 'u1'),
                 joined_on = CASE WHEN id = 'u1' THEN DATE '2023-01-01' ELSE DATE '2024-01-01' END,
                 seen_at = CASE WHEN id = 'u1' THEN TIMESTAMPTZ '2023-01-01 00:00:00+00'
                                ELSE TIMESTAMPTZ '2024-01-01 00:00:00+00' END,
                 owner_uuid = CASE WHEN id = 'u1' THEN '00000000-0000-0000-0000-000000000001'::uuid
                                   ELSE '00000000-0000-0000-0000-000000000002'::uuid END,
                 risk_score = CASE WHEN id = 'u1' THEN -5 ELSE 5000000000 END,
                 nullable_note = CASE WHEN id = 'u1' THEN 'present' ELSE NULL END,
                 prefs = CASE WHEN id = 'u1' THEN '{\"tier\":\"starter\"}'::jsonb ELSE '{\"tier\":\"enterprise\"}'::jsonb END,
                 labels = CASE WHEN id = 'u1' THEN ARRAY['seed','low']::text[] ELSE ARRAY['target','high']::text[] END",
        )
        .expect("populate typed filter columns failed");
    Spi::run(
            "SELECT graph.add_table('graph_test_users_pgtest'::regclass, 'id', ARRAY['name', 'prefs', 'labels'])",
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
    .expect("add edge-table edge failed");
    for (column, column_type) in [
        ("name", "text"),
        ("active", "boolean"),
        ("joined_on", "date"),
        ("seen_at", "timestamptz"),
        ("owner_uuid", "uuid"),
        ("risk_score", "numeric"),
        ("nullable_note", "text"),
    ] {
        Spi::run(&format!(
                "SELECT graph.add_filter_column('graph_test_users_pgtest'::regclass, {}, column_type := {})",
                super::sql_literal(column),
                super::sql_literal(column_type)
            ))
            .expect("add typed filter column failed");
    }
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let cases = [
            ("text", "{\"node\":{\"where\":{\"name\":{\"eq\":\"Bob\"}}}}"),
            (
                "boolean",
                "{\"node\":{\"where\":{\"active\":{\"eq\":false}}}}",
            ),
            (
                "date",
                "{\"node\":{\"where\":{\"joined_on\":{\"gte\":\"2024-01-01\"}}}}",
            ),
            (
                "timestamptz",
                "{\"node\":{\"where\":{\"seen_at\":{\"gte\":\"2024-01-01T00:00:00Z\"}}}}",
            ),
            (
                "uuid",
                "{\"node\":{\"where\":{\"owner_uuid\":{\"eq\":\"00000000-0000-0000-0000-000000000002\"}}}}",
            ),
            (
                "numeric_i64",
                "{\"node\":{\"where\":{\"risk_score\":{\"gt\":4294967296}}}}",
            ),
            (
                "null_text",
                "{\"node\":{\"where\":{\"nullable_note\":{\"eq\":null}}}}",
            ),
        ];

    for (label, filter) in cases {
        let count = Spi::get_one::<i64>(&format!(
            "SELECT count(*)
                 FROM graph.traverse(
                    'graph_test_users_pgtest'::regclass,
                    'u1',
                    1,
                    filter := {}::jsonb,
                    hydrate := false
                 )
                 WHERE node_id = 'u2'",
            super::sql_literal(filter)
        ))
        .expect("typed filter traverse failed")
        .unwrap_or(0);
        assert_eq!(count, 1, "{} filter should match u2", label);
    }

    let hydration_ok = Spi::get_one::<bool>(
        "SELECT (node->'prefs'->>'tier') = 'enterprise'
                    AND (node->'labels') ? 'target'
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := '{\"node\":{\"where\":{\"risk_score\":{\"gt\":4294967296}}}}'::jsonb,
                hydrate := true
             )
             WHERE node_id = 'u2'",
    )
    .expect("jsonb/array hydration query failed")
    .unwrap_or(false);
    assert!(hydration_ok);
}

#[pg_test]
fn sparse_typed_filters_survive_persisted_load_traverse_search_and_sync() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = on").expect("enable persist_on_build failed");
    Spi::run("SET graph.enabled = on").expect("enable graph failed");
    Spi::run("SET graph.sync_mode = 'trigger'").expect("set trigger sync failed");
    clear_graph_catalog_for_test();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_sparse_filters_pgtest CASCADE")
        .expect("drop sparse filter table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_sparse_filters_pgtest (
                id TEXT PRIMARY KEY,
                parent_id TEXT REFERENCES public.graph_test_sparse_filters_pgtest(id),
                name TEXT NOT NULL,
                active BOOLEAN,
                joined_on DATE,
                seen_at TIMESTAMPTZ,
                owner_uuid UUID,
                risk_score BIGINT
            )",
    )
    .expect("create sparse filter table failed");
    Spi::run(
        "INSERT INTO public.graph_test_sparse_filters_pgtest
                (id, parent_id, name, active, joined_on, seen_at, owner_uuid, risk_score)
             VALUES
                ('root', NULL, 'Root', NULL, NULL, NULL, NULL, NULL),
                ('child', 'root', 'Child', true, DATE '2024-01-01',
                 TIMESTAMPTZ '2024-01-01 00:00:00+00',
                 '00000000-0000-0000-0000-000000000002'::uuid, 10)",
    )
    .expect("insert sparse filter root/child failed");
    Spi::run(
        "INSERT INTO public.graph_test_sparse_filters_pgtest (id, parent_id, name)
             SELECT 'filler-' || gs::text, 'root', 'Filler ' || gs::text
             FROM generate_series(1, 18) AS gs",
    )
    .expect("insert sparse filter fillers failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_sparse_filters_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY[
                    'name',
                    'parent_id',
                    'active',
                    'joined_on',
                    'seen_at',
                    'owner_uuid',
                    'risk_score'
                ]
            )",
    )
    .expect("add sparse filter table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_sparse_filters_pgtest'::regclass,
                'parent_id',
                'graph_test_sparse_filters_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := false
            )",
    )
    .expect("add sparse filter edge failed");
    for (column, column_type) in [
        ("active", "boolean"),
        ("joined_on", "date"),
        ("seen_at", "timestamptz"),
        ("owner_uuid", "uuid"),
        ("risk_score", "numeric"),
    ] {
        Spi::run(&format!(
            "SELECT graph.add_filter_column(
                    'graph_test_sparse_filters_pgtest'::regclass,
                    {},
                    column_type := {}
                )",
            super::sql_literal(column),
            super::sql_literal(column_type)
        ))
        .expect("add sparse typed filter column failed");
    }
    Spi::run("SELECT * FROM graph.build()").expect("build sparse filter graph failed");

    crate::ENGINE.with(|e| {
        *e.borrow_mut() = crate::engine::Engine::new();
    });
    Spi::run("SET graph.auto_load = on").expect("enable auto_load failed");

    let old_filter = "{\"node\":{\"where\":{\"active\":{\"eq\":true},\"joined_on\":{\"eq\":\"2024-01-01\"},\"seen_at\":{\"eq\":\"2024-01-01T00:00:00Z\"},\"owner_uuid\":{\"eq\":\"00000000-0000-0000-0000-000000000002\"},\"risk_score\":{\"eq\":10}}}}";
    let loaded_match = Spi::get_one::<i64>(&format!(
        "SELECT count(*)
             FROM graph.traverse_search(
                'name',
                'Root',
                table_filter := 'graph_test_sparse_filters_pgtest'::regclass,
                search_mode := 'exact',
                max_depth := 1,
                direction := 'in',
                filter := {}::jsonb,
                hydrate := false
             )
             WHERE root_id = 'root' AND node_id = 'child'",
        super::sql_literal(old_filter)
    ))
    .expect("persisted sparse traverse_search failed")
    .unwrap_or(0);
    assert_eq!(loaded_match, 1);

    Spi::run(
        "UPDATE public.graph_test_sparse_filters_pgtest
             SET joined_on = DATE '2025-01-01',
                 seen_at = TIMESTAMPTZ '2025-01-01 00:00:00+00',
                 owner_uuid = '00000000-0000-0000-0000-000000000003'::uuid,
                 risk_score = 11
             WHERE id = 'child'",
    )
    .expect("update sparse filter child failed");
    let updates = Spi::get_one::<i64>("SELECT updates_applied FROM graph.apply_sync()")
        .expect("apply sparse filter sync failed")
        .unwrap_or(0);
    assert_eq!(updates, 1);

    let new_filter = "{\"node\":{\"where\":{\"active\":{\"eq\":true},\"joined_on\":{\"eq\":\"2025-01-01\"},\"seen_at\":{\"eq\":\"2025-01-01T00:00:00Z\"},\"owner_uuid\":{\"eq\":\"00000000-0000-0000-0000-000000000003\"},\"risk_score\":{\"eq\":11}}}}";
    let (old_after_sync, new_after_sync) = Spi::connect(|client| {
        let old_query = format!(
            "SELECT count(*)
                 FROM graph.traverse(
                    'graph_test_sparse_filters_pgtest'::regclass,
                    'root',
                    1,
                    direction := 'in',
                    filter := {}::jsonb,
                    hydrate := false
                 )
                 WHERE node_id = 'child'",
            super::sql_literal(old_filter)
        );
        let new_query = format!(
            "SELECT count(*)
                 FROM graph.traverse(
                    'graph_test_sparse_filters_pgtest'::regclass,
                    'root',
                    1,
                    direction := 'in',
                    filter := {}::jsonb,
                    hydrate := false
                 )
                 WHERE node_id = 'child'",
            super::sql_literal(new_filter)
        );
        let old_rows = client.select(&old_query, None, &[])?;
        let new_rows = client.select(&new_query, None, &[])?;
        Ok::<_, pgrx::spi::Error>((
            old_rows.first().get::<i64>(1)?.unwrap_or(0),
            new_rows.first().get::<i64>(1)?.unwrap_or(0),
        ))
    })
    .expect("read sparse filter sync results failed");

    assert_eq!(old_after_sync, 0);
    assert_eq!(new_after_sync, 1);
    Spi::run("SET graph.auto_load = off").expect("restore auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("restore persist_on_build failed");
    Spi::run("SET graph.sync_mode = 'manual'").expect("restore sync mode failed");
}

#[pg_test]
fn traverse_rejects_raw_jsonb_filters_outside_registered_catalog_contract() {
    reset_and_create_fixtures();
    Spi::run("SELECT graph.add_table('graph_test_users_pgtest'::regclass, 'id', ARRAY['name'])")
        .expect("add users table failed");
    Spi::run("SELECT graph.add_filter_column('graph_test_users_pgtest'::regclass, 'age')")
        .expect("add age filter failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let unsupported_scope = sql_raises(
        "SELECT *
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := '{\"edge\":{\"where\":{\"age\":{\"eq\":1}}}}'::jsonb,
                hydrate := false
             )",
    );
    let wrong_node_table_context = sql_raises(
        "SELECT *
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                node_tables := ARRAY['graph_test_friendships_pgtest'::regclass],
                filter := '{\"node\":{\"where\":{\"age\":{\"eq\":1}}}}'::jsonb,
                hydrate := false
             )",
    );

    assert!(unsupported_scope);
    assert!(wrong_node_table_context);
}

#[pg_test]
fn traverse_rejects_ambiguous_raw_jsonb_filter_columns() {
    reset_and_create_fixtures();
    Spi::run(
        "CREATE TABLE public.graph_test_filter_context_pgtest (
                id TEXT PRIMARY KEY,
                age INT NOT NULL DEFAULT 0
            )",
    )
    .expect("create filter context table failed");
    Spi::run("SELECT graph.add_table('graph_test_users_pgtest'::regclass, 'id', ARRAY['name'])")
        .expect("add users table failed");
    Spi::run("SELECT graph.add_table('graph_test_filter_context_pgtest'::regclass, 'id', NULL)")
        .expect("add filter context table failed");
    Spi::run("SELECT graph.add_filter_column('graph_test_users_pgtest'::regclass, 'age')")
        .expect("add users age filter failed");
    Spi::run("SELECT graph.add_filter_column('graph_test_filter_context_pgtest'::regclass, 'age')")
        .expect("add context age filter failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let ambiguous_column = sql_raises(
        "SELECT *
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := '{\"node\":{\"where\":{\"age\":{\"eq\":1}}}}'::jsonb,
                hydrate := false
             )",
    );

    assert!(ambiguous_column);
}
