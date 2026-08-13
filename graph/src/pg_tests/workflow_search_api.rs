// Workflow search and expansion tests cover wrapper-specific SQL contracts.
// Primitive search/traversal tests own the deeper engine behavior; these tests
// lock down aliases, defaults, pagination, ranking, hydration, and counts.

#[cfg(feature = "development")]
fn build_workflow_rls_fixture() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
         VALUES ('u3', 'Carol', 55);
         UPDATE public.graph_test_users_pgtest SET name = 'Alice' WHERE id = 'u2';
         INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
         VALUES ('f2', 'u2', 'u3'), ('f3', 'u2', 'u1')",
    )
    .expect("create workflow RLS fixture failed");
    build_friendship_fixture_graph();
    Spi::run(
        "DROP ROLE IF EXISTS graph_workflow_rls_reader;
         CREATE ROLE graph_workflow_rls_reader;
         ALTER TABLE public.graph_test_users_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_workflow_visible_nodes
           ON public.graph_test_users_pgtest FOR SELECT
           TO graph_workflow_rls_reader USING (id <> 'u3');
         GRANT USAGE ON SCHEMA graph, public TO graph_workflow_rls_reader;
         GRANT SELECT ON public.graph_test_users_pgtest,
                         public.graph_test_friendships_pgtest
           TO graph_workflow_rls_reader",
    )
    .expect("configure workflow RLS fixture failed");
}

#[cfg(feature = "development")]
fn workflow_json(statement: &str) -> pgrx::JsonB {
    Spi::get_one::<pgrx::JsonB>(statement)
        .expect("workflow JSON query failed")
        .unwrap_or_else(|| pgrx::JsonB(serde_json::Value::Null))
}

#[cfg(feature = "development")]
fn workflow_cancellation(statement: &str) -> bool {
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    pgrx::pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
        Spi::run(statement).expect("armed workflow unexpectedly returned an SPI error");
    }))
    .catch_when(pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED, |_| {
        cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    })
    .execute();
    cancelled.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(feature = "development")]
#[pg_test]
fn workflow_expand_lazy_matches_eager_rows_hydration_and_truncation() {
    build_workflow_rls_fixture();
    Spi::run("SET ROLE graph_workflow_rls_reader").expect("set workflow reader failed");
    let query = "SELECT jsonb_agg(to_jsonb(result) ORDER BY rank)
                   FROM graph.expand(
                     'graph_test_users_pgtest'::regclass, 'u1',
                     max_depth := 2, direction := 'out', max_rows := 1
                   ) AS result";
    Spi::run("SELECT graph._test_set_visibility_strategy('eager')")
        .expect("force eager workflow failed");
    let eager = workflow_json(query);
    Spi::run("SELECT graph._test_set_visibility_strategy('lazy')")
        .expect("force lazy workflow failed");
    let lazy = workflow_json(query);
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("reset workflow strategy failed");
    assert_eq!(lazy.0, eager.0);
    assert_eq!(lazy.0.as_array().map(Vec::len), Some(1));
    let row = &lazy.0.as_array().expect("expand rows")[0];
    assert_eq!(row["truncated"].as_bool(), Some(true));
    assert_eq!(row["node"]["id"].as_str(), Some("u2"));
}

#[cfg(feature = "development")]
#[pg_test]
fn workflow_find_related_lazy_reuses_one_resolver_across_roots_and_counts() {
    build_workflow_rls_fixture();
    Spi::run("SET ROLE graph_workflow_rls_reader").expect("set workflow reader failed");
    let query = |include_counts| format!("SELECT jsonb_agg(to_jsonb(result) ORDER BY root_id, rank)
                   FROM graph.find_related(
                     'name', 'Alice',
                     source_table := 'graph_test_users_pgtest'::regclass,
                     search_mode := 'exact', max_depth := 2,
                     search_max_rows := 2, direction := 'out', max_rows := 10,
                     include_counts := {include_counts}
                   ) AS result");
    Spi::run("SELECT graph._test_set_visibility_strategy('eager')")
        .expect("force eager workflow failed");
    let eager = workflow_json(&query(true));
    Spi::run("SELECT graph._test_set_visibility_strategy('lazy')")
        .expect("force lazy workflow failed");
    let without_counts = workflow_json(&query(false));
    let without_count_metrics = workflow_json("SELECT graph._test_visibility_metrics()");
    Spi::run("SELECT graph._test_set_visibility_strategy('lazy')")
        .expect("reset lazy workflow metrics failed");
    let lazy = workflow_json(&query(true));
    let with_count_metrics = workflow_json("SELECT graph._test_visibility_metrics()");
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("reset workflow strategy failed");
    assert_eq!(lazy.0, eager.0);
    assert!(without_counts.0.as_array().is_some_and(|rows| rows.len() >= 2));
    assert_eq!(
        with_count_metrics.0["spi_calls"],
        without_count_metrics.0["spi_calls"]
    );
    assert_eq!(
        with_count_metrics.0["requested_keys"],
        without_count_metrics.0["requested_keys"]
    );
    assert!(
        with_count_metrics.0["spi_calls"]
            .as_u64()
            .is_some_and(|calls| calls > 0)
    );
}

#[cfg(feature = "development")]
#[pg_test]
fn workflow_lazy_visibility_cancellation_cleans_statement_and_retries() {
    build_workflow_rls_fixture();
    Spi::run(
        "SET ROLE graph_workflow_rls_reader;
         SELECT graph._test_set_visibility_strategy('lazy');
         SELECT graph._test_arm_lazy_visibility_cancel()",
    )
    .expect("arm workflow cancellation failed");
    let lazy_cancelled = workflow_cancellation(
        "SELECT * FROM graph.expand(
               'graph_test_users_pgtest'::regclass, 'u1', max_depth := 2
             )",
    );
    let retry = Spi::get_one::<i64>(
        "SELECT count(*) FROM graph.expand(
           'graph_test_users_pgtest'::regclass, 'u1', max_depth := 2
         )",
    )
    .expect("workflow retry failed")
    .unwrap_or_default();
    Spi::run("SELECT graph._test_arm_search_cancel()")
        .expect("arm workflow search cancellation failed");
    let search_cancelled = workflow_cancellation(
        "SELECT * FROM graph.find_related(
           'name', 'Alice', source_table := 'graph_test_users_pgtest'::regclass,
           search_mode := 'exact', search_max_rows := 2, max_depth := 2
         )",
    );
    let search_retry = Spi::get_one::<i64>(
        "SELECT count(*) FROM graph.find_related(
           'name', 'Alice', source_table := 'graph_test_users_pgtest'::regclass,
           search_mode := 'exact', search_max_rows := 2, max_depth := 2
         )",
    )
    .expect("workflow search retry failed")
    .unwrap_or_default();
    Spi::run("SELECT graph._test_arm_hydration_cancel()")
        .expect("arm workflow hydration cancellation failed");
    let hydration_cancelled = workflow_cancellation(
        "SELECT * FROM graph.expand(
           'graph_test_users_pgtest'::regclass, 'u1', max_depth := 2
         )",
    );
    let hydration_retry = Spi::get_one::<i64>(
        "SELECT count(*) FROM graph.expand(
           'graph_test_users_pgtest'::regclass, 'u1', max_depth := 2
         )",
    )
    .expect("workflow hydration retry failed")
    .unwrap_or_default();
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("reset workflow strategy failed");
    assert!(lazy_cancelled);
    assert!(search_cancelled);
    assert!(hydration_cancelled);
    assert_eq!(retry, 1);
    assert!(search_retry >= 2);
    assert_eq!(hydration_retry, 1);
}

#[cfg(feature = "development")]
#[pg_test]
fn workflow_no_rls_lazy_fast_path_has_zero_visibility_spi() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();
    Spi::run("SELECT graph._test_set_visibility_strategy('auto')")
        .expect("reset workflow strategy failed");
    let count = Spi::get_one::<i64>(
        "SELECT count(*) FROM graph.expand(
           'graph_test_users_pgtest'::regclass, 'u1', max_depth := 1
         )",
    )
    .expect("no-RLS workflow failed")
    .unwrap_or_default();
    let metrics = workflow_json("SELECT graph._test_visibility_metrics()");
    assert_eq!(count, 1);
    assert_eq!(metrics.0["spi_calls"].as_u64(), Some(0));
}

#[pg_test]
fn workflow_find_returns_hydrated_ranked_rows_with_aliases() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (matched, node_table_matches, table_name_present, rank, name) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT count(*)::bigint,
                        bool_and(node_table = 'graph_test_users_pgtest'::regclass),
                        bool_and(node_table_name <> ''),
                        min(rank),
                        min(node->>'name')
                   FROM graph.find(
                        'name',
                        'Alice',
                        table_name := 'graph_test_users_pgtest'::regclass,
                        mode := 'exact',
                        max_rows := 1,
                        row_offset := 0
                   )",
                None,
                &[],
            )
            .expect("workflow find failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<bool>(2)?.unwrap_or(false),
            row.get::<bool>(3)?.unwrap_or(false),
            row.get::<i32>(4)?.unwrap_or_default(),
            row.get::<String>(5)?.unwrap_or_default(),
        ))
    })
    .expect("workflow find result read failed");

    let no_match_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM graph.find(
                'name',
                'Nobody',
                table_name := 'graph_test_users_pgtest'::regclass,
                mode := 'exact'
           )",
    )
    .expect("workflow find empty result failed")
    .unwrap_or(-1);

    assert_eq!(matched, 1);
    assert!(node_table_matches);
    assert!(table_name_present);
    assert_eq!(rank, 1);
    assert_eq!(name, "Alice");
    assert_eq!(no_match_count, 0);
}

#[pg_test]
fn workflow_expand_defaults_exclude_start_and_can_page_hydrated_rows() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (matched, start_rows, node_id, depth, rank, readable_path, name, truncated) =
        Spi::connect(|client| {
            let result = client
                .select(
                    "SELECT count(*)::bigint,
                            count(*) FILTER (WHERE node_id = 'u1'),
                            min(node_id),
                            min(depth),
                            min(rank),
                            max(readable_path),
                            min(node->>'name'),
                            bool_or(truncated)
                       FROM graph.expand(
                            'graph_test_users_pgtest'::regclass,
                            'u1',
                            max_depth := 1,
                            target_table := 'graph_test_users_pgtest'::regclass,
                            max_rows := 10
                       )",
                    None,
                    &[],
                )
                .expect("workflow expand failed");
            let row = result.first();
            Ok::<_, pgrx::spi::Error>((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<i64>(2)?.unwrap_or_default(),
                row.get::<String>(3)?.unwrap_or_default(),
                row.get::<i32>(4)?.unwrap_or_default(),
                row.get::<i32>(5)?.unwrap_or_default(),
                row.get::<String>(6)?.unwrap_or_default(),
                row.get::<String>(7)?.unwrap_or_default(),
                row.get::<bool>(8)?.unwrap_or(false),
            ))
        })
        .expect("workflow expand result read failed");

    let include_start_ids = Spi::get_one::<Vec<String>>(
        "SELECT array_agg(node_id ORDER BY rank)
           FROM graph.expand(
                'graph_test_users_pgtest'::regclass,
                'u1',
                max_depth := 1,
                target_table := 'graph_test_users_pgtest'::regclass,
                max_rows := 10,
                include_start := true
           )",
    )
    .expect("workflow expand include_start failed")
    .unwrap_or_default();

    assert_eq!(matched, 1);
    assert_eq!(start_rows, 0);
    assert_eq!(node_id, "u2");
    assert_eq!(depth, 1);
    assert_eq!(rank, 1);
    assert!(readable_path.contains("--friend-->"));
    assert_eq!(name, "Bob");
    assert!(!truncated);
    assert_eq!(include_start_ids, vec!["u1".to_string(), "u2".to_string()]);
}

#[pg_test]
fn workflow_find_related_counts_filtering_and_pages_after_candidates() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('u3', 'Carol', 55)",
    )
    .expect("insert related user failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
             VALUES ('f2', 'u1', 'u3')",
    )
    .expect("insert related edge failed");
    build_friendship_fixture_graph();

    let (node_id, rank, name, candidate_count, filtered_count, truncated) =
        Spi::connect(|client| {
            let result = client
                .select(
                    "SELECT node_id,
                            rank,
                            node->>'name',
                            candidate_count,
                            filtered_count,
                            truncated
                       FROM graph.find_related(
                            'name',
                            'Alice',
                            source_table := 'graph_test_users_pgtest'::regclass,
                            max_depth := 1,
                            target_table := 'graph_test_users_pgtest'::regclass,
                            where_node := graph.gt('age', 40),
                            max_rows := 1,
                            row_offset := 1,
                            include_counts := true
                       )",
                    None,
                    &[],
                )
                .expect("workflow find_related page failed");
            let row = result.first();
            Ok::<_, pgrx::spi::Error>((
                row.get::<String>(1)?.unwrap_or_default(),
                row.get::<i32>(2)?.unwrap_or_default(),
                row.get::<String>(3)?.unwrap_or_default(),
                row.get::<i64>(4)?.unwrap_or_default(),
                row.get::<i64>(5)?.unwrap_or_default(),
                row.get::<bool>(6)?.unwrap_or(false),
            ))
        })
        .expect("workflow find_related page result read failed");

    let count_columns_are_null = Spi::get_one::<bool>(
        "SELECT bool_and(candidate_count IS NULL AND filtered_count IS NULL)
           FROM graph.find_related(
                'name',
                'Alice',
                source_table := 'graph_test_users_pgtest'::regclass,
                max_depth := 1,
                target_table := 'graph_test_users_pgtest'::regclass,
                max_rows := 10,
                include_counts := false
           )",
    )
    .expect("workflow find_related null counts failed")
    .unwrap_or(false);

    assert_eq!(node_id, "u3");
    assert_eq!(rank, 2);
    assert_eq!(name, "Carol");
    assert_eq!(candidate_count, 2);
    assert_eq!(filtered_count, 2);
    assert!(!truncated);
    assert!(count_columns_are_null);
}
