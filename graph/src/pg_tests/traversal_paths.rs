#[pg_test]
fn traverse_uses_graph_default_max_depth_when_omitted() {
    reset_and_create_fixtures();
    Spi::run("SELECT * FROM graph.auto_discover('public')").expect("auto_discover failed");
    Spi::run("SET graph.default_max_depth = 1").expect("set default depth failed");

    let max_depth_default = Spi::get_one::<i32>(
        "SELECT max(depth) FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1')",
    )
    .expect("default depth traverse failed")
    .unwrap_or(-1);
    let max_depth_explicit = Spi::get_one::<i32>(
        "SELECT max(depth) FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 5)",
    )
    .expect("explicit depth traverse failed")
    .unwrap_or(-1);

    assert_eq!(max_depth_default, 1);
    assert!(max_depth_explicit >= 2);
}

#[pg_test]
fn traverse_hydrates_source_rows_by_default_and_can_opt_out() {
    reset_and_create_fixtures();
    Spi::run("SELECT * FROM graph.auto_discover('public')").expect("auto_discover failed");

    let hydrated_name = Spi::get_one::<String>(
        "SELECT node->>'name'
             FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 0)
             WHERE node_id = 'u1'",
    )
    .expect("hydrated traverse failed")
    .expect("hydrated node missing");
    let opt_out_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 0, hydrate := false)
             WHERE node IS NULL",
    )
    .expect("coordinate-only traverse failed")
    .unwrap_or(0);

    assert_eq!(hydrated_name, "Alice");
    assert_eq!(opt_out_count, 1);
}

#[pg_test]
fn shortest_path_hydrates_source_rows_by_default_and_can_opt_out() {
    reset_and_create_fixtures();
    Spi::run("SELECT * FROM graph.auto_discover('public')").expect("auto_discover failed");

    let hydrated_name = Spi::get_one::<String>(
        "SELECT node->>'name'
             FROM graph.shortest_path(
                'graph_test_users_pgtest'::regclass,
                'u1',
                'graph_test_users_pgtest'::regclass,
                'u2',
                5
             )
             WHERE node_id = 'u2'",
    )
    .expect("hydrated shortest_path failed")
    .expect("hydrated path node missing");
    let opt_out_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.shortest_path(
                'graph_test_users_pgtest'::regclass,
                'u1',
                'graph_test_users_pgtest'::regclass,
                'u2',
                5,
                hydrate := false
             )
             WHERE node IS NULL",
    )
    .expect("coordinate-only shortest_path failed")
    .unwrap_or(0);

    assert_eq!(hydrated_name, "Bob");
    assert_eq!(opt_out_count, 3);
}

#[pg_test]
fn shortest_path_v1_acceptance_shape_limits_and_empty_results() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('u3', 'Carol', 29)",
    )
    .expect("insert disconnected user failed");
    Spi::run("SELECT * FROM graph.auto_discover('public')").expect("auto_discover failed");

    let column_shape = Spi::get_one::<bool>(
            "SELECT pg_get_function_result(p.oid) =
                    'TABLE(step integer, node_table oid, node_id text, edge_label text, node jsonb, node_table_name text)'
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname = 'graph'
               AND p.proname = 'shortest_path'",
        )
        .expect("shortest_path shape inspection failed")
        .unwrap_or(false);
    let ordered_path = Spi::get_one::<Vec<String>>(
        "SELECT array_agg(node_id ORDER BY step)
             FROM graph.shortest_path(
                'graph_test_users_pgtest'::regclass,
                'u1',
                'graph_test_users_pgtest'::regclass,
                'u2',
                5,
                hydrate := false
             )",
    )
    .expect("ordered shortest_path failed")
    .unwrap_or_default();
    let max_depth_blocked = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.shortest_path(
                'graph_test_users_pgtest'::regclass,
                'u1',
                'graph_test_users_pgtest'::regclass,
                'u2',
                0,
                hydrate := false
             )",
    )
    .expect("max_depth shortest_path failed")
    .unwrap_or(-1);
    let no_path_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.shortest_path(
                'graph_test_users_pgtest'::regclass,
                'u1',
                'graph_test_users_pgtest'::regclass,
                'u3',
                5,
                hydrate := false
             )",
    )
    .expect("no-path shortest_path failed")
    .unwrap_or(-1);

    assert!(column_shape);
    assert_eq!(
        ordered_path,
        vec!["u1".to_string(), "f1".to_string(), "u2".to_string()]
    );
    assert_eq!(max_depth_blocked, 0);
    assert_eq!(no_path_count, 0);
}

#[cfg(feature = "development")]
fn build_unweighted_path_fixture(bidirectional: bool, edges: &str) {
    reset_and_create_fixtures();
    Spi::run(
        "TRUNCATE public.graph_test_friendships_pgtest;
         DELETE FROM public.graph_test_users_pgtest;
         INSERT INTO public.graph_test_users_pgtest (id, name, age) VALUES
           ('s', 'source', 1), ('a', 'a', 2), ('b', 'b', 3),
           ('c', 'c', 4), ('d', 'd', 5), ('h', 'hidden', 6),
           ('x', 'behind-hidden', 7), ('t', 'target', 8)",
    )
    .expect("create path nodes failed");
    Spi::run(&format!(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id) VALUES {edges}"
    ))
    .expect("create path edges failed");
    Spi::run(
        "SELECT graph.add_table(
           'graph_test_users_pgtest'::regclass,
           id_column := 'id', columns := ARRAY['name', 'age'])",
    )
    .expect("register path nodes failed");
    Spi::run(&format!(
        "SELECT graph.add_edge(
           'graph_test_friendships_pgtest'::regclass,
           'user_id', 'graph_test_users_pgtest'::regclass,
           'friend_id', 'friend', bidirectional := {bidirectional})"
    ))
    .expect("register path relationships failed");
    Spi::run("SELECT * FROM graph.build()").expect("build path fixture failed");
}

#[cfg(feature = "development")]
fn configure_unweighted_path_rls(node_policy: &str, relationship_policy: &str) {
    Spi::run(&format!(
        "DO $$ BEGIN
           IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'graph_path_rls_reader') THEN
             REVOKE ALL PRIVILEGES ON SCHEMA graph, public FROM graph_path_rls_reader;
           END IF;
         END $$;
         DROP ROLE IF EXISTS graph_path_rls_reader;
         CREATE ROLE graph_path_rls_reader;
         ALTER TABLE public.graph_test_users_pgtest ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.graph_test_friendships_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_path_visible_nodes
           ON public.graph_test_users_pgtest FOR SELECT
           TO graph_path_rls_reader USING ({node_policy});
         CREATE POLICY graph_path_visible_relationships
           ON public.graph_test_friendships_pgtest FOR SELECT
           TO graph_path_rls_reader USING ({relationship_policy});
         GRANT USAGE ON SCHEMA graph, public TO graph_path_rls_reader;
         GRANT SELECT ON public.graph_test_users_pgtest,
                         public.graph_test_friendships_pgtest
           TO graph_path_rls_reader"
    ))
    .expect("configure path RLS failed");
}

#[cfg(feature = "development")]
fn unweighted_path_ids(source: &str, target: &str) -> Vec<String> {
    Spi::get_one::<Vec<String>>(&format!(
        "SELECT COALESCE(array_agg(node_id ORDER BY step), '{{}}'::text[])
           FROM graph.shortest_path(
             'graph_test_users_pgtest'::regclass, {},
             'graph_test_users_pgtest'::regclass, {},
             20, hydrate := false)",
        super::sql_literal(source),
        super::sql_literal(target)
    ))
    .expect("shortest path query failed")
    .unwrap_or_default()
}

#[cfg(feature = "development")]
fn forced_unweighted_path(strategy: &str, source: &str, target: &str) -> Vec<String> {
    Spi::run(&format!(
        "SELECT graph._test_set_visibility_strategy({})",
        super::sql_literal(strategy)
    ))
    .expect("set path visibility strategy failed");
    unweighted_path_ids(source, target)
}

#[cfg(feature = "development")]
fn forced_typed_unweighted_path(strategy: &str, source: &str, target: &str) -> Vec<String> {
    Spi::run(&format!(
        "SELECT graph._test_set_visibility_strategy({})",
        super::sql_literal(strategy)
    ))
    .expect("set typed-path visibility strategy failed");
    Spi::get_one::<Vec<String>>(&format!(
        "SELECT COALESCE(array_agg(node_id ORDER BY step), '{{}}'::text[])
           FROM graph.shortest_path(
             'graph_test_users_pgtest'::regclass, {},
             'graph_test_users_pgtest'::regclass, {},
             20, false, ARRAY['friend'])",
        super::sql_literal(source),
        super::sql_literal(target)
    ))
    .expect("typed shortest path query failed")
    .unwrap_or_default()
}

#[cfg(feature = "development")]
fn forced_empty_typed_unweighted_path(strategy: &str, source: &str, target: &str) -> Vec<String> {
    Spi::run(&format!(
        "SELECT graph._test_set_visibility_strategy({})",
        super::sql_literal(strategy)
    ))
    .expect("set empty typed-path visibility strategy failed");
    Spi::get_one::<Vec<String>>(&format!(
        "SELECT COALESCE(array_agg(node_id ORDER BY step), '{{}}'::text[])
           FROM graph.shortest_path(
             'graph_test_users_pgtest'::regclass, {},
             'graph_test_users_pgtest'::regclass, {},
             20, false, ARRAY[]::text[])",
        super::sql_literal(source),
        super::sql_literal(target)
    ))
    .expect("empty typed shortest path query failed")
    .unwrap_or_default()
}

#[cfg(feature = "development")]
fn forced_workflow_path(strategy: &str, source: &str, target: &str) -> Vec<String> {
    Spi::run(&format!(
        "SELECT graph._test_set_visibility_strategy({})",
        super::sql_literal(strategy)
    ))
    .expect("set workflow path visibility strategy failed");
    Spi::get_one::<Vec<String>>(&format!(
        "SELECT COALESCE(array_agg(node_id ORDER BY step), '{{}}'::text[])
           FROM graph.path(
             'graph_test_users_pgtest'::regclass, {},
             'graph_test_users_pgtest'::regclass, {})",
        super::sql_literal(source),
        super::sql_literal(target)
    ))
    .expect("workflow path query failed")
    .unwrap_or_default()
}

#[cfg(feature = "development")]
fn path_visibility_metrics() -> pgrx::JsonB {
    Spi::get_one::<pgrx::JsonB>("SELECT graph._test_visibility_metrics()")
        .expect("read path visibility metrics failed")
        .expect("path visibility metrics missing")
}

#[cfg(feature = "development")]
fn build_weighted_path_rls_fixture() {
    reset_and_create_fixtures();
    Spi::run(
        "DROP TABLE IF EXISTS public.graph_test_weighted_path_edges_p45 CASCADE;
         DROP TABLE IF EXISTS public.graph_test_weighted_path_nodes_p45 CASCADE;
         DO $$ BEGIN
           IF EXISTS (
             SELECT 1 FROM pg_roles WHERE rolname = 'graph_weighted_path_rls_reader'
           ) THEN
             REVOKE ALL PRIVILEGES ON SCHEMA graph, public
               FROM graph_weighted_path_rls_reader;
           END IF;
         END $$;
         DROP ROLE IF EXISTS graph_weighted_path_rls_reader;
         CREATE TABLE public.graph_test_weighted_path_nodes_p45 (
           id text PRIMARY KEY,
           name text NOT NULL
         );
         CREATE TABLE public.graph_test_weighted_path_edges_p45 (
           id text PRIMARY KEY,
           src text NOT NULL REFERENCES public.graph_test_weighted_path_nodes_p45(id),
           dst text NOT NULL REFERENCES public.graph_test_weighted_path_nodes_p45(id),
           cost integer NOT NULL CHECK (cost >= 0),
           rel_type text NOT NULL
         );
         INSERT INTO public.graph_test_weighted_path_nodes_p45 (id, name) VALUES
           ('s', 'source'), ('a', 'tie-a'), ('b', 'tie-b'),
           ('c', 'relationship-hidden'), ('h', 'node-hidden'),
           ('x', 'behind-hidden'), ('t', 'target');
         INSERT INTO public.graph_test_weighted_path_edges_p45
           (id, src, dst, cost, rel_type) VALUES
           ('direct', 's', 't', 10, 'direct'),
           ('sa', 's', 'a', 2, 'route'), ('at', 'a', 't', 2, 'route'),
           ('sb', 's', 'b', 2, 'route'), ('bt', 'b', 't', 2, 'route'),
           ('sc', 's', 'c', 1, 'secret'), ('ct', 'c', 't', 1, 'secret'),
           ('sh', 's', 'h', 1, 'shortcut'), ('hx', 'h', 'x', 1, 'shortcut');
         SELECT graph.add_table(
           'graph_test_weighted_path_nodes_p45'::regclass,
           id_column := 'id', columns := ARRAY['name']);
         SELECT graph.add_edge(
           'graph_test_weighted_path_edges_p45'::regclass,
           from_column := 'src',
           to_table := 'graph_test_weighted_path_nodes_p45'::regclass,
           to_column := 'dst',
           label := 'route',
           bidirectional := false,
           weight_column := 'cost',
           label_column := 'rel_type');
         SELECT * FROM graph.build();
         CREATE ROLE graph_weighted_path_rls_reader;
         ALTER TABLE public.graph_test_weighted_path_nodes_p45 ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.graph_test_weighted_path_edges_p45 ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_weighted_path_visible_nodes
           ON public.graph_test_weighted_path_nodes_p45 FOR SELECT
           TO graph_weighted_path_rls_reader USING (id <> 'h');
         CREATE POLICY graph_weighted_path_visible_edges
           ON public.graph_test_weighted_path_edges_p45 FOR SELECT
           TO graph_weighted_path_rls_reader USING (rel_type <> 'secret');
         GRANT USAGE ON SCHEMA graph, public TO graph_weighted_path_rls_reader;
         GRANT SELECT ON public.graph_test_weighted_path_nodes_p45,
                         public.graph_test_weighted_path_edges_p45
           TO graph_weighted_path_rls_reader",
    )
    .expect("build weighted path RLS fixture failed");
}

#[cfg(feature = "development")]
fn forced_weighted_path_detail(
    strategy: &str,
    source: &str,
    target: &str,
    edge_types: Option<&[&str]>,
) -> String {
    Spi::run(&format!(
        "SELECT graph._test_set_visibility_strategy({})",
        super::sql_literal(strategy)
    ))
    .expect("set weighted-path visibility strategy failed");
    let edge_types = edge_types.map(|labels| {
        labels
            .iter()
            .map(|label| super::sql_literal(label))
            .collect::<Vec<_>>()
            .join(", ")
    });
    let edge_types = edge_types
        .map(|labels| format!(", ARRAY[{labels}]::text[]"))
        .unwrap_or_default();
    Spi::get_one::<String>(&format!(
        "SELECT string_agg(
           step::text || ':' || node_id || ':' ||
           coalesce(edge_label, '<start>') || ':' ||
           coalesce(edge_weight::text, '<start>') || ':' ||
           step_cost::text || ':' || total_cost::text,
           ',' ORDER BY step)
         FROM graph.weighted_shortest_path(
           'graph_test_weighted_path_nodes_p45'::regclass, {},
           'graph_test_weighted_path_nodes_p45'::regclass, {}{edge_types})",
        super::sql_literal(source),
        super::sql_literal(target),
    ))
    .expect("weighted shortest-path detail query failed")
    .unwrap_or_default()
}

#[cfg(feature = "development")]
#[pg_test]
fn weighted_paths_lazy_match_eager_rls_ties_filters_and_metadata() {
    build_weighted_path_rls_fixture();
    Spi::run("SET ROLE graph_weighted_path_rls_reader")
        .expect("set weighted-path RLS reader failed");

    let eager = forced_weighted_path_detail("eager", "s", "t", None);
    let lazy = forced_weighted_path_detail("lazy", "s", "t", None);
    let lazy_metrics = path_visibility_metrics();
    let typed = forced_weighted_path_detail("lazy", "s", "t", Some(&["route"]));
    let typed_metrics = path_visibility_metrics();
    let hidden_endpoint = forced_weighted_path_detail("lazy", "s", "h", None);
    let hidden_source = forced_weighted_path_detail("lazy", "h", "x", None);
    let hidden_identity_endpoint = forced_weighted_path_detail("lazy", "h", "h", None);
    let hidden_source_missing_target = forced_weighted_path_detail("lazy", "h", "missing", None);
    let behind_hidden = forced_weighted_path_detail("lazy", "s", "x", None);

    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore weighted-path visibility strategy failed");

    let expected = "0:s:<start>:<start>:0:4,1:a:route:2:2:4,2:t:route:2:4:4";
    assert_eq!(
        eager, expected,
        "eager fixture must freeze strict tie order"
    );
    assert_eq!(lazy, eager, "lazy weighted path must match eager exactly");
    assert_eq!(typed, eager, "typed lazy path must preserve step metadata");
    assert!(hidden_endpoint.is_empty(), "a hidden target must be absent");
    assert!(hidden_source.is_empty(), "a hidden source must be absent");
    assert!(
        hidden_identity_endpoint.is_empty(),
        "a hidden source-equals-target endpoint must remain absent"
    );
    assert!(
        hidden_source_missing_target.is_empty(),
        "a hidden source must short-circuit missing-target diagnostics"
    );
    assert!(
        behind_hidden.is_empty(),
        "a hidden intermediate must block the visible node behind it"
    );
    for metrics in [lazy_metrics, typed_metrics] {
        assert_eq!(
            metrics.0["selected_strategy"].as_str(),
            Some("lazy"),
            "forced lazy weighted paths must select the resumable oracle"
        );
        assert!(
            metrics.0["spi_calls"].as_u64().unwrap_or_default() > 0,
            "RLS-enforced weighted paths must probe PostgreSQL visibility"
        );
    }
}

#[cfg(feature = "development")]
#[pg_test]
fn weighted_paths_lazy_durable_segments_match_eager() {
    build_weighted_path_rls_fixture();
    create_error_sqlstate_helper();
    create_error_detail_helper();
    Spi::run(
        "SET graph.mutable_enabled = on;
         SET graph.persist_on_build = on;
         SET graph.sync_mode = 'trigger';
         SET graph.query_freshness = 'off';
         SELECT * FROM graph.build(mode := 'mutable_overlay');
         INSERT INTO public.graph_test_weighted_path_edges_p45
           (id, src, dst, cost, rel_type)
         VALUES ('durable-sx', 's', 'x', 1, 'route'),
                ('durable-xt', 'x', 't', 1, 'route'),
                ('durable-hidden', 's', 't', 1, 'secret')",
    )
    .expect("create weighted durable delta failed");
    let published = Spi::get_one::<i64>("SELECT segments_published FROM graph.ingest_projection()")
        .expect("ingest weighted fixture failed")
        .unwrap_or_default();
    assert!(published > 0);
    Spi::run("SET graph.auto_load = on").expect("enable weighted durable auto-load failed");
    super::ENGINE.with(|engine| *engine.borrow_mut() = super::engine::Engine::new());
    assert!(
        !forced_weighted_path_detail("auto", "s", "t", None).is_empty(),
        "owner warmup must load the durable weighted projection"
    );
    Spi::run("SET ROLE graph_weighted_path_rls_reader")
        .expect("set weighted reader failed");
    let eager = forced_weighted_path_detail("eager", "s", "t", None);
    Spi::run("SET ROLE graph_weighted_path_rls_reader")
        .expect("restore weighted reader before durable lazy query failed");
    let lazy = forced_weighted_path_detail("lazy", "s", "t", None);
    let metrics = path_visibility_metrics();
    Spi::run(
        "RESET ROLE;
         SELECT graph._test_arm_missing_bfs_candidate_relationship_identity();
         SET ROLE graph_weighted_path_rls_reader",
    )
    .expect("arm durable weighted missing-identity probe failed");
    let durable_statement = "SELECT * FROM graph.weighted_shortest_path(
       'graph_test_weighted_path_nodes_p45'::regclass, 'x',
       'graph_test_weighted_path_nodes_p45'::regclass, 't')";
    let durable_identity_state = captured_path_sqlstate(durable_statement);
    Spi::run(
        "RESET ROLE;
         SELECT graph._test_arm_missing_bfs_candidate_relationship_identity();
         SET ROLE graph_weighted_path_rls_reader",
    )
    .expect("rearm durable weighted identity detail failed");
    let durable_identity_detail = captured_path_detail(durable_statement).unwrap_or_default();
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore weighted strategy failed");
    assert_eq!(lazy, eager);
    assert!(lazy.contains(":x:route:1:"));
    assert!(
        !lazy.contains(":secret:"),
        "a lower-cost durable dynamic label hidden by relationship RLS must not be admitted"
    );
    assert_eq!(metrics.0["selected_strategy"].as_str(), Some("lazy"));
    assert!(metrics.0["spi_calls"].as_u64().unwrap_or_default() > 0);
    assert_eq!(durable_identity_state.as_deref(), Some("55000"));
    assert!(durable_identity_detail.contains("PG023"));
}

#[cfg(feature = "development")]
#[pg_test]
fn weighted_paths_durable_unseen_dynamic_label_remains_pg018() {
    build_weighted_path_rls_fixture();
    create_error_sqlstate_helper();
    Spi::run(
        "SET graph.mutable_enabled = on;
         SET graph.persist_on_build = on;
         SET graph.sync_mode = 'trigger';
         SET graph.query_freshness = 'off';
         SELECT * FROM graph.build(mode := 'mutable_overlay');
         INSERT INTO public.graph_test_weighted_path_edges_p45
           (id, src, dst, cost, rel_type)
         VALUES ('durable-new-label', 's', 't', 1, 'runtime-only')",
    )
    .expect("create unseen weighted dynamic label failed");

    let state = captured_path_sqlstate("SELECT * FROM graph.ingest_projection()");
    assert_eq!(
        state.as_deref(),
        Some("0A000"),
        "a durable label absent from the persisted projection must require a rebuild"
    );
}

#[cfg(feature = "development")]
#[pg_test]
fn weighted_paths_pending_edge_overlay_remains_pg018() {
    build_weighted_path_rls_fixture();
    create_error_sqlstate_helper();
    Spi::run(
        "SET graph.sync_mode = 'trigger';
         SELECT * FROM graph.build();
         SET graph.sync_mode = 'trigger';
         INSERT INTO public.graph_test_weighted_path_edges_p45
           (id, src, dst, cost, rel_type)
         VALUES ('pending', 's', 't', 1, 'route');
         SET graph.query_freshness = 'apply_pending_sync'",
    )
    .expect("create pending weighted overlay failed");
    let state = captured_path_sqlstate(
        "SELECT * FROM graph.weighted_shortest_path(
           'graph_test_weighted_path_nodes_p45'::regclass, 's',
           'graph_test_weighted_path_nodes_p45'::regclass, 't')",
    );
    assert_eq!(state.as_deref(), Some("0A000"));
}

#[cfg(feature = "development")]
#[pg_test]
fn weighted_paths_tx_node_state_falls_back_eager() {
    build_weighted_path_rls_fixture();
    Spi::run(
        "SET graph.mutable_enabled = on;
         SET graph.persist_on_build = on;
         SET graph.sync_mode = 'trigger';
         SET graph.query_freshness = 'off';
         SELECT * FROM graph.build(mode := 'mutable_overlay');
         SELECT * FROM graph.gql(
           'CREATE (u:graph_test_weighted_path_nodes_p45 {id: ''tx'', name: ''tx''}) RETURN u',
           hydrate := false);
         SET ROLE graph_weighted_path_rls_reader",
    )
    .expect("create weighted tx-node fallback failed");
    let eager = forced_weighted_path_detail("eager", "s", "t", None);
    let fallback = forced_weighted_path_detail("lazy", "s", "t", None);
    let metrics = path_visibility_metrics();
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore weighted strategy failed");
    assert_eq!(fallback, eager);
    assert_eq!(metrics.0["selected_strategy"].as_str(), Some("eager"));
}

#[cfg(feature = "development")]
#[pg_test]
fn weighted_paths_lazy_resource_identity_cancellation_and_retry() {
    build_weighted_path_rls_fixture();
    create_error_sqlstate_helper();
    create_error_detail_helper();
    let statement = "SELECT * FROM graph.weighted_shortest_path(
       'graph_test_weighted_path_nodes_p45'::regclass, 's',
       'graph_test_weighted_path_nodes_p45'::regclass, 't')";
    Spi::run(
        "SET ROLE graph_weighted_path_rls_reader;
         SELECT graph._test_set_visibility_strategy('lazy');
         SET LOCAL graph.query_work_limit = 1",
    )
    .expect("configure capped weighted path failed");
    let work_state = captured_path_sqlstate(statement);
    Spi::run(
        "SET LOCAL graph.query_work_limit = 1000000;
         SET LOCAL graph.query_memory_mb = 1",
    )
    .expect("configure weighted path memory cap failed");
    let memory_state = captured_path_sqlstate(statement);
    Spi::run(
        "RESET ROLE;
         SET LOCAL graph.query_memory_mb = 1024;
         SELECT graph._test_arm_missing_bfs_candidate_relationship_identity();
         SET ROLE graph_weighted_path_rls_reader",
    )
    .expect("arm weighted missing relationship identity failed");
    let identity_state = captured_path_sqlstate(statement);
    Spi::run(
        "RESET ROLE;
         SELECT graph._test_arm_missing_bfs_candidate_relationship_identity();
         SET ROLE graph_weighted_path_rls_reader",
    )
    .expect("rearm weighted missing relationship identity failed");
    let identity_detail = captured_path_detail(statement).unwrap_or_default();
    Spi::run("SET ROLE graph_weighted_path_rls_reader")
        .expect("set weighted reader failed");
    let expected = forced_weighted_path_detail("eager", "s", "t", None);
    Spi::run("SELECT graph._test_set_visibility_strategy('lazy')")
        .expect("select lazy weighted cancellation strategy failed");
    Spi::run("SELECT graph._test_arm_lazy_visibility_cancel()")
        .expect("arm weighted cancellation failed");
    let cancelled = workflow_cancellation(statement);
    let state_empty = Spi::get_one::<bool>(
        "SELECT graph._test_visibility_resolution_state_empty()",
    )
    .expect("inspect weighted cancellation cleanup failed")
    .unwrap_or(false);
    Spi::run("SET ROLE graph_weighted_path_rls_reader")
        .expect("restore weighted reader after cancellation failed");
    let retry = forced_weighted_path_detail("lazy", "s", "t", None);
    Spi::run(
        "RESET ROLE;
         DROP POLICY graph_weighted_path_visible_nodes
           ON public.graph_test_weighted_path_nodes_p45;
         CREATE FUNCTION public.graph_weighted_path_error_policy()
           RETURNS boolean LANGUAGE plpgsql VOLATILE AS $$
           BEGIN PERFORM 1 / 0; RETURN true; END $$;
         CREATE POLICY graph_weighted_path_visible_nodes
           ON public.graph_test_weighted_path_nodes_p45 FOR SELECT
           TO graph_weighted_path_rls_reader
           USING (public.graph_weighted_path_error_policy());
         GRANT EXECUTE ON FUNCTION public.graph_weighted_path_error_policy()
           TO graph_weighted_path_rls_reader;
         SET ROLE graph_weighted_path_rls_reader",
    )
    .expect("configure weighted path policy error failed");
    let policy_state = captured_path_sqlstate(statement);
    Spi::run(
        "RESET ROLE;
         DROP POLICY graph_weighted_path_visible_nodes
           ON public.graph_test_weighted_path_nodes_p45;
         CREATE POLICY graph_weighted_path_visible_nodes
           ON public.graph_test_weighted_path_nodes_p45 FOR SELECT
           TO graph_weighted_path_rls_reader USING (id <> 'h');
         SET ROLE graph_weighted_path_rls_reader",
    )
    .expect("restore weighted path policy failed");
    let state_empty_after_policy_error = Spi::get_one::<bool>(
        "SELECT graph._test_visibility_resolution_state_empty()",
    )
    .expect("inspect weighted policy-error cleanup failed")
    .unwrap_or(false);
    let retry_after_policy_error = forced_weighted_path_detail("lazy", "s", "t", None);
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore weighted strategy failed");
    assert_eq!(work_state.as_deref(), Some("54000"));
    assert_eq!(memory_state.as_deref(), Some("54000"));
    assert_eq!(identity_state.as_deref(), Some("55000"));
    assert!(identity_detail.contains("PG023"));
    assert!(expected.contains(":a:route:"));
    assert!(cancelled);
    assert!(state_empty);
    assert!(!retry.is_empty(), "weighted path retry must succeed after cancellation");
    assert_eq!(policy_state.as_deref(), Some("22012"));
    assert!(state_empty_after_policy_error);
    assert_eq!(retry_after_policy_error, retry);
}

#[cfg(feature = "development")]
#[pg_test]
fn weighted_paths_no_rls_fast_path_has_zero_visibility_spi() {
    build_weighted_path_rls_fixture();
    Spi::run(
        "ALTER TABLE public.graph_test_weighted_path_nodes_p45 DISABLE ROW LEVEL SECURITY;
         ALTER TABLE public.graph_test_weighted_path_edges_p45 DISABLE ROW LEVEL SECURITY;
         SELECT graph._test_set_visibility_strategy('auto')",
    )
    .expect("disable weighted RLS failed");
    let path = forced_weighted_path_detail("auto", "s", "t", None);
    let metrics = path_visibility_metrics();
    assert!(!path.is_empty());
    assert_eq!(metrics.0["spi_calls"].as_u64(), Some(0));
}

#[cfg(feature = "development")]
fn captured_path_sqlstate(statement: &str) -> Option<String> {
    Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sqlstate({})",
        super::sql_literal(statement)
    ))
    .expect("capture path SQLSTATE failed")
}

#[cfg(feature = "development")]
fn captured_path_detail(statement: &str) -> Option<String> {
    Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sql_error_detail({})",
        super::sql_literal(statement)
    ))
    .expect("capture path error detail failed")
}

#[cfg(feature = "development")]
fn assert_forced_path_parity(source: &str, target: &str) -> Vec<String> {
    let eager = forced_unweighted_path("eager", source, target);
    let lazy = forced_unweighted_path("lazy", source, target);
    let metrics = path_visibility_metrics();
    assert_eq!(
        lazy, eager,
        "lazy path must exactly match eager output; metrics={:?}",
        metrics.0
    );
    assert_eq!(metrics.0["strategy"].as_str(), Some("lazy"));
    assert!(
        metrics.0["spi_calls"].as_u64().unwrap_or_default() > 0,
        "forced lazy path selection must perform caller-scoped visibility probes"
    );
    lazy
}

#[cfg(feature = "development")]
#[pg_test]
fn unweighted_paths_lazy_match_eager_single_bidirectional_ties_and_order() {
    const TIED_EDGES: &str =
        "('sa', 's', 'a'), ('at', 'a', 't'), ('sb', 's', 'b'), ('bt', 'b', 't')";
    for bidirectional in [false, true] {
        build_unweighted_path_fixture(bidirectional, TIED_EDGES);
        configure_unweighted_path_rls("true", "true");
        Spi::run("SET ROLE graph_path_rls_reader").expect("set path reader failed");
        let path = assert_forced_path_parity("s", "t");
        let typed_path = forced_typed_unweighted_path("lazy", "s", "t");
        let typed_metrics = path_visibility_metrics();
        let empty_typed_eager = forced_empty_typed_unweighted_path("eager", "s", "t");
        let empty_typed_lazy = forced_empty_typed_unweighted_path("lazy", "s", "t");
        let workflow_eager = forced_workflow_path("eager", "s", "t");
        let workflow_lazy = forced_workflow_path("lazy", "s", "t");
        let workflow_metrics = path_visibility_metrics();
        Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
            .expect("restore path strategy failed");
        assert_eq!(typed_path, path);
        assert_eq!(empty_typed_lazy, empty_typed_eager);
        assert!(empty_typed_lazy.is_empty());
        assert_eq!(workflow_lazy, workflow_eager);
        assert!(workflow_metrics.0["spi_calls"].as_u64().unwrap_or_default() > 0);
        assert!(typed_metrics.0["spi_calls"].as_u64().unwrap_or_default() > 0);
        assert_eq!(path.first().map(String::as_str), Some("s"));
        assert_eq!(path.last().map(String::as_str), Some("t"));
        assert_eq!(path.len(), 3, "chosen two-edge tie must be complete");
    }

    // The first meeting discovered while expanding the selected bidirectional
    // level belongs to the longer s-a-c-d-t route. The complete level also
    // contains the shorter s-b-x-t route, so stopping on the first meeting is
    // observably wrong.
    build_unweighted_path_fixture(
        true,
        "('sa', 's', 'a'), ('sb', 's', 'b'), ('ac', 'a', 'c'),
         ('cd', 'c', 'd'), ('dt', 'd', 't'), ('bx', 'b', 'x'), ('xt', 'x', 't')",
    );
    configure_unweighted_path_rls("true", "true");
    Spi::run("SET ROLE graph_path_rls_reader").expect("set full-level path reader failed");
    let complete_level_path = assert_forced_path_parity("s", "t");
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore full-level path strategy failed");
    assert_eq!(
        complete_level_path,
        ["s", "b", "x", "t"].map(str::to_string)
    );
}

#[cfg(feature = "development")]
#[pg_test]
fn unweighted_paths_lazy_node_and_relationship_rls_choose_visible_route() {
    const ROUTES: &str = "('sh', 's', 'h'), ('ht', 'h', 't'),
        ('hx', 'h', 'x'), ('xt', 'x', 't'),
        ('sa', 's', 'a'), ('ab', 'a', 'b'), ('bt', 'b', 't')";
    build_unweighted_path_fixture(false, ROUTES);
    configure_unweighted_path_rls("id <> 'h'", "true");
    Spi::run("SET ROLE graph_path_rls_reader").expect("set node-policy reader failed");
    let node_filtered = assert_forced_path_parity("s", "t");
    let hidden_intermediate_eager = forced_unweighted_path("eager", "s", "x");
    let hidden_intermediate_lazy = forced_unweighted_path("lazy", "s", "x");
    Spi::run("RESET ROLE").expect("reset node-policy reader failed");
    assert_eq!(node_filtered, ["s", "a", "b", "t"].map(str::to_string));
    assert!(!node_filtered.iter().any(|id| id == "x"));
    assert_eq!(hidden_intermediate_lazy, hidden_intermediate_eager);
    assert!(hidden_intermediate_lazy.is_empty());

    Spi::run(
        "DROP POLICY graph_path_visible_nodes ON public.graph_test_users_pgtest;
         CREATE POLICY graph_path_visible_nodes
           ON public.graph_test_users_pgtest FOR SELECT
           TO graph_path_rls_reader USING (true);
         DROP POLICY graph_path_visible_relationships
           ON public.graph_test_friendships_pgtest;
         CREATE POLICY graph_path_visible_relationships
           ON public.graph_test_friendships_pgtest FOR SELECT
           TO graph_path_rls_reader USING (id NOT IN ('sh', 'ht', 'hx', 'xt'));
         SET ROLE graph_path_rls_reader",
    )
    .expect("configure relationship-policy reader failed");
    let relationship_filtered = assert_forced_path_parity("s", "t");
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore relationship-policy strategy failed");
    assert_eq!(relationship_filtered, node_filtered);
}

#[cfg(feature = "development")]
#[pg_test]
fn unweighted_paths_lazy_source_target_visibility_preserves_error_ordering() {
    build_unweighted_path_fixture(false, "('st', 's', 't')");
    create_error_sqlstate_helper();
    Spi::run(
        "DROP ROLE IF EXISTS graph_path_rls_reader;
         CREATE ROLE graph_path_rls_reader;
         GRANT USAGE ON SCHEMA graph, public TO graph_path_rls_reader;
         SET ROLE graph_path_rls_reader",
    )
    .expect("configure path ACL reader failed");
    let statement = "SELECT * FROM graph.shortest_path(
        'graph_test_users_pgtest'::regclass, 'missing',
        'graph_test_users_pgtest'::regclass, 'also-missing', 20,
        hydrate := false)";
    let acl_state = captured_path_sqlstate(statement);
    Spi::run("RESET ROLE").expect("reset ACL reader failed");
    assert_eq!(acl_state.as_deref(), Some("42501"));

    Spi::run(
        "ALTER TABLE public.graph_test_users_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_path_visible_nodes
           ON public.graph_test_users_pgtest FOR SELECT
           TO graph_path_rls_reader USING (id = 'a');
         GRANT SELECT ON public.graph_test_users_pgtest,
                         public.graph_test_friendships_pgtest
           TO graph_path_rls_reader;
         SET ROLE graph_path_rls_reader",
    )
    .expect("configure hidden endpoint reader failed");
    let hidden_source_eager = forced_unweighted_path("eager", "s", "t");
    let hidden_source_lazy = forced_unweighted_path("lazy", "s", "t");
    let hidden_source_missing_target_state = captured_path_sqlstate(
        "SELECT * FROM graph.shortest_path(
           'graph_test_users_pgtest'::regclass, 's',
           'graph_test_users_pgtest'::regclass, 'missing-target', 20,
           hydrate := false)",
    );
    let hidden_target_lazy = forced_unweighted_path("lazy", "a", "t");
    let hidden_target_metrics = path_visibility_metrics();
    let hidden_identity_lazy = forced_unweighted_path("lazy", "t", "t");
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore endpoint strategy failed");
    assert_eq!(hidden_source_lazy, hidden_source_eager);
    assert!(hidden_source_lazy.is_empty());
    assert_eq!(hidden_source_missing_target_state, None);
    assert!(hidden_target_lazy.is_empty());
    assert!(hidden_identity_lazy.is_empty());
    assert!(
        hidden_target_metrics.0["spi_calls"]
            .as_u64()
            .unwrap_or_default()
            > 0,
        "hidden endpoints must be resolved through the forced lazy policy oracle"
    );
}

#[cfg(feature = "development")]
fn configure_mutable_path_reader() {
    Spi::run(
        "DO $$ BEGIN
           IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'graph_path_rls_reader') THEN
             REVOKE ALL PRIVILEGES ON SCHEMA graph, public FROM graph_path_rls_reader;
           END IF;
         END $$;
         DROP ROLE IF EXISTS graph_path_rls_reader;
         CREATE ROLE graph_path_rls_reader;
         ALTER TABLE public.graph_test_users_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_path_visible_nodes
           ON public.graph_test_users_pgtest FOR SELECT
           TO graph_path_rls_reader USING (true);
         ALTER TABLE public.graph_test_friendships_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_path_visible_relationships
           ON public.graph_test_friendships_pgtest FOR SELECT
           TO graph_path_rls_reader USING (true);
         GRANT USAGE ON SCHEMA graph, public TO graph_path_rls_reader;
         GRANT SELECT ON public.graph_test_users_pgtest,
                         public.graph_test_friendships_pgtest
           TO graph_path_rls_reader;
         SET ROLE graph_path_rls_reader",
    )
    .expect("configure mutable path reader failed");
}

#[cfg(feature = "development")]
fn build_mutable_path_graph() {
    reset_and_create_fixtures();
    Spi::run(
        "TRUNCATE public.graph_test_friendships_pgtest;
         SET graph.sync_mode = 'trigger';
         SET graph.persist_on_build = on;
         SET graph.mutable_enabled = on;
         SET graph.query_freshness = 'off';
         SELECT graph.add_table(
           'graph_test_users_pgtest'::regclass,
           id_column := 'id', columns := ARRAY['name', 'age']);
         SELECT graph.add_edge(
           'graph_test_friendships_pgtest'::regclass,
           'user_id', 'graph_test_users_pgtest'::regclass,
           'friend_id', 'friend', bidirectional := false);
         SELECT * FROM graph.build(mode := 'mutable_overlay')",
    )
    .expect("build mutable path graph failed");
}

#[cfg(feature = "development")]
fn insert_mutable_path_edge() {
    Spi::run(
        "SELECT *
           FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest {id: ''u1''}),
                    (v:graph_test_users_pgtest {id: ''u2''})
              CREATE (u)-[r:friend {id: ''f_path_mutable''}]->(v)
              RETURN r',
             hydrate := false)",
    )
    .expect("insert mutable path edge failed");
}

#[cfg(feature = "development")]
fn assert_mutable_path_parity() {
    assert_eq!(
        unweighted_path_ids("u1", "u2"),
        ["u1", "u2"].map(str::to_string),
        "mutable fixture must expose the path before caller RLS is enabled"
    );
    configure_mutable_path_reader();
    let path = assert_forced_path_parity("u1", "u2");
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore mutable path strategy failed");
    assert_eq!(path, ["u1", "u2"].map(str::to_string));
}

#[cfg(feature = "development")]
#[pg_test]
fn unweighted_paths_lazy_overlay_durable_and_tx_match_eager() {
    build_mutable_path_graph();
    insert_mutable_path_edge();
    Spi::run("SELECT graph.apply_sync()").expect("apply classic overlay path failed");
    assert_mutable_path_parity();

    build_mutable_path_graph();
    insert_mutable_path_edge();
    let published = Spi::get_one::<i64>(
        "SELECT segments_published FROM graph.ingest_projection()",
    )
    .expect("ingest durable path edge failed")
    .unwrap_or_default();
    assert!(published > 0, "durable path fixture requires a segment");
    Spi::run("SET graph.auto_load = on").expect("enable durable path auto-load failed");
    super::ENGINE.with(|engine| *engine.borrow_mut() = super::engine::Engine::new());
    assert_mutable_path_parity();

    build_mutable_path_graph();
    insert_mutable_path_edge();
    Spi::run(
        "SELECT * FROM graph.gql(
           'CREATE (u:graph_test_users_pgtest {
              id: ''u3'', name: ''transaction-local'', age: 3
            }) RETURN u',
           hydrate := false)",
    )
    .expect("insert transaction-local path node failed");
    assert_eq!(
        Spi::get_one::<i32>("SELECT tx_delta_added_nodes FROM graph.status()")
            .expect("read transaction-local path status failed")
            .unwrap_or_default(),
        1,
        "path fallback fixture must contain one transaction-local node"
    );
    configure_mutable_path_reader();
    let fallback = forced_unweighted_path("lazy", "u1", "u2");
    let fallback_metrics = path_visibility_metrics();
    assert_eq!(
        fallback_metrics.0["selected_strategy"].as_str(),
        Some("eager"),
        "transaction-local node state must force the eager path oracle"
    );
    let eager = forced_unweighted_path("eager", "u1", "u2");
    assert_eq!(
        fallback, eager,
        "transaction-local node state must retain the eager path oracle"
    );
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore transaction-local path strategy failed");
}

#[cfg(feature = "development")]
#[pg_test]
fn unweighted_paths_lazy_resource_cap_and_missing_identity_fail_closed() {
    build_unweighted_path_fixture(false, "('st', 's', 't')");
    configure_unweighted_path_rls("true", "true");
    create_error_sqlstate_helper();
    create_error_detail_helper();
    Spi::run(
        "SET ROLE graph_path_rls_reader;
         SELECT graph._test_set_visibility_strategy('lazy');
         SET LOCAL graph.query_work_limit = 1",
    )
    .expect("configure capped path failed");
    let statement = "SELECT * FROM graph.shortest_path(
        'graph_test_users_pgtest'::regclass, 's',
        'graph_test_users_pgtest'::regclass, 't', 20,
        hydrate := false)";
    let cap_state = captured_path_sqlstate(statement);
    Spi::run(
        "SET LOCAL graph.query_work_limit = 1000000;
         SET LOCAL graph.query_memory_mb = 1",
    )
    .expect("configure path memory cap failed");
    let memory_state = captured_path_sqlstate(statement);
    Spi::run(
        "RESET ROLE;
         SET LOCAL graph.query_memory_mb = 1024;
         SET LOCAL graph.query_work_limit = 1000000;
         SELECT graph._test_arm_missing_bfs_candidate_relationship_identity();
         SET ROLE graph_path_rls_reader",
    )
    .expect("arm missing path relationship identity failed");
    let identity_state = captured_path_sqlstate(statement);
    Spi::run(
        "RESET ROLE;
         SELECT graph._test_arm_missing_bfs_candidate_relationship_identity();
         SET ROLE graph_path_rls_reader",
    )
    .expect("rearm missing path identity detail failed");
    let identity_detail = captured_path_detail(statement).unwrap_or_default();
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore capped path strategy failed");
    assert_eq!(cap_state.as_deref(), Some("54000"));
    assert_eq!(memory_state.as_deref(), Some("54000"));
    assert_eq!(identity_state.as_deref(), Some("55000"));
    assert!(identity_detail.contains("PG023"));
}

#[cfg(feature = "development")]
#[pg_test]
fn unweighted_paths_lazy_cancellation_policy_error_drop_state_then_retry() {
    build_unweighted_path_fixture(false, "('st', 's', 't')");
    configure_unweighted_path_rls("true", "true");
    create_error_sqlstate_helper();
    Spi::run(
        "SET ROLE graph_path_rls_reader;
         SELECT graph._test_set_visibility_strategy('lazy');
         SELECT graph._test_arm_lazy_visibility_cancel()",
    )
    .expect("arm path cancellation failed");
    let statement = "SELECT * FROM graph.shortest_path(
        'graph_test_users_pgtest'::regclass, 's',
        'graph_test_users_pgtest'::regclass, 't', 20,
        hydrate := false)";
    let cancelled = workflow_cancellation(statement);
    let empty_after_cancel =
        Spi::get_one::<bool>("SELECT graph._test_visibility_resolution_state_empty()")
            .expect("inspect cancelled path state failed")
            .unwrap_or(false);
    let retry_after_cancel = unweighted_path_ids("s", "t");
    Spi::run(
        "RESET ROLE;
         DROP POLICY graph_path_visible_nodes ON public.graph_test_users_pgtest;
         CREATE FUNCTION public.graph_path_error_policy()
           RETURNS boolean LANGUAGE plpgsql VOLATILE AS $$
           BEGIN PERFORM 1 / 0; RETURN true; END $$;
         CREATE POLICY graph_path_visible_nodes
           ON public.graph_test_users_pgtest FOR SELECT
           TO graph_path_rls_reader USING (public.graph_path_error_policy());
         GRANT EXECUTE ON FUNCTION public.graph_path_error_policy()
           TO graph_path_rls_reader;
         SET ROLE graph_path_rls_reader",
    )
    .expect("configure path policy error failed");
    let policy_state = captured_path_sqlstate(statement);
    Spi::run(
        "RESET ROLE;
         DROP POLICY graph_path_visible_nodes ON public.graph_test_users_pgtest;
         CREATE POLICY graph_path_visible_nodes
           ON public.graph_test_users_pgtest FOR SELECT
           TO graph_path_rls_reader USING (true);
         SET ROLE graph_path_rls_reader",
    )
    .expect("replace path policy error failed");
    let empty_after_error =
        Spi::get_one::<bool>("SELECT graph._test_visibility_resolution_state_empty()")
            .expect("inspect failed path state failed")
            .unwrap_or(false);
    let retry_after_error = unweighted_path_ids("s", "t");
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore path error strategy failed");
    assert!(cancelled);
    assert!(empty_after_cancel);
    assert_eq!(retry_after_cancel, ["s", "t"].map(str::to_string));
    assert_eq!(policy_state.as_deref(), Some("22012"));
    assert!(empty_after_error);
    assert_eq!(retry_after_error, retry_after_cancel);
}

#[cfg(feature = "development")]
#[pg_test]
fn unweighted_paths_no_rls_fast_path_has_zero_visibility_spi() {
    build_unweighted_path_fixture(false, "('st', 's', 't')");
    Spi::run("SELECT graph._test_set_visibility_strategy('auto')")
        .expect("set automatic path strategy failed");
    let path = unweighted_path_ids("s", "t");
    let metrics = path_visibility_metrics();
    assert_eq!(path, ["s", "t"].map(str::to_string));
    assert_eq!(metrics.0["spi_calls"].as_u64(), Some(0));
    assert_eq!(metrics.0["requested_keys"].as_u64(), Some(0));
}

#[pg_test]
fn aggregate_sums_averages_and_counts_returned_nodes() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let result = Spi::get_one::<pgrx::JsonB>(
            "WITH req AS (
                SELECT jsonb_build_object(
                    'starts',
                    jsonb_build_array(graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')),
                    'direction', 'out',
                    'min_depth', 0,
                    'max_depth', 1,
                    'edge_types', jsonb_build_array('friend'),
                    'node_tables', jsonb_build_array('graph_test_users_pgtest')
                ) AS traversal
             )
             SELECT graph.aggregate(
                traversal,
                '{
                    \"sum\":[{\"table\":\"graph_test_users_pgtest\",\"column\":\"age\",\"as\":\"total_age\"}],
                    \"avg\":[{\"table\":\"graph_test_users_pgtest\",\"column\":\"age\",\"as\":\"avg_age\"}],
                    \"count\":[{\"table\":\"graph_test_users_pgtest\",\"column\":\"id\",\"as\":\"user_count\"}]
                }'::jsonb
             )
             FROM req",
        )
        .expect("aggregate query failed")
        .expect("aggregate result missing")
        .0;

    assert_eq!(
        result.get("total_age").and_then(|value| value.as_f64()),
        Some(78.0)
    );
    assert_eq!(
        result.get("avg_age").and_then(|value| value.as_f64()),
        Some(39.0)
    );
    assert_eq!(
        result.get("user_count").and_then(|value| value.as_u64()),
        Some(2)
    );
}

#[pg_test]
fn aggregate_supports_chosen_parent_path_scope() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('u3', 'Carol', 29)",
    )
    .expect("insert path endpoint failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
             VALUES ('f2', 'u2', 'u3')",
    )
    .expect("insert second friendship failed");
    build_friendship_fixture_graph();

    let result = Spi::get_one::<pgrx::JsonB>(
            "WITH req AS (
                SELECT jsonb_build_object(
                    'starts',
                    jsonb_build_array(graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')),
                    'direction', 'out',
                    'min_depth', 2,
                    'max_depth', 2,
                    'edge_types', jsonb_build_array('friend'),
                    'node_tables', jsonb_build_array('graph_test_users_pgtest')
                ) AS traversal
             )
             SELECT graph.aggregate(
                traversal,
                '{\"sum\":[{\"table\":\"graph_test_users_pgtest\",\"column\":\"age\",\"as\":\"path_age\"}]}'::jsonb,
                scope := 'chosen_parent_path'
             )
             FROM req",
        )
        .expect("chosen parent path aggregate query failed")
        .expect("chosen parent path aggregate result missing")
        .0;

    assert_eq!(
        result.get("path_age").and_then(|value| value.as_f64()),
        Some(107.0)
    );
}

#[pg_test]
fn path_count_estimate_reports_exact_and_capped_counts() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let exact = Spi::get_one::<bool>(
            "WITH req AS (
                SELECT jsonb_build_object(
                    'starts',
                    jsonb_build_array(graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')),
                    'direction', 'out',
                    'min_depth', 0,
                    'max_depth', 1,
                    'edge_types', jsonb_build_array('friend'),
                    'node_tables', jsonb_build_array('graph_test_users_pgtest')
                ) AS traversal
             )
             SELECT estimated_paths = 2 AND exact AND NOT capped
             FROM graph.path_count_estimate((SELECT traversal FROM req))",
        )
        .expect("exact path count estimate failed")
        .unwrap_or(false);

    Spi::run("SET graph.max_exact_path_count = 1").expect("set path count cap failed");
    let capped = Spi::get_one::<bool>(
            "WITH req AS (
                SELECT jsonb_build_object(
                    'starts',
                    jsonb_build_array(graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')),
                    'direction', 'out',
                    'min_depth', 0,
                    'max_depth', 1,
                    'edge_types', jsonb_build_array('friend'),
                    'node_tables', jsonb_build_array('graph_test_users_pgtest')
                ) AS traversal
             )
             SELECT estimated_paths = 1 AND NOT exact AND capped
             FROM graph.path_count_estimate((SELECT traversal FROM req))",
        )
        .expect("capped path count estimate failed")
        .unwrap_or(false);
    Spi::run("SET graph.max_exact_path_count = 100000").expect("reset path count cap failed");

    assert!(exact);
    assert!(capped);
}

#[pg_test]
fn aggregate_all_possible_paths_counts_duplicate_path_occurrences() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('u3', 'Carol', 29)",
    )
    .expect("insert branch user failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
             VALUES ('f2', 'u1', 'u3')",
    )
    .expect("insert branch friendship failed");
    build_friendship_fixture_graph();

    let result = Spi::get_one::<pgrx::JsonB>(
            "WITH req AS (
                SELECT jsonb_build_object(
                    'starts',
                    jsonb_build_array(graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')),
                    'direction', 'out',
                    'min_depth', 0,
                    'max_depth', 1,
                    'edge_types', jsonb_build_array('friend'),
                    'node_tables', jsonb_build_array('graph_test_users_pgtest')
                ) AS traversal
             )
             SELECT graph.aggregate(
                traversal,
                '{
                    \"sum\":[{\"table\":\"graph_test_users_pgtest\",\"column\":\"age\",\"as\":\"path_age\"}],
                    \"count\":[{\"table\":\"graph_test_users_pgtest\",\"column\":\"id\",\"as\":\"path_node_count\"}]
                }'::jsonb,
                scope := 'all_possible_paths'
             )
             FROM req",
        )
        .expect("all possible paths aggregate query failed")
        .expect("all possible paths aggregate result missing")
        .0;

    assert_eq!(
        result.get("path_age").and_then(|value| value.as_f64()),
        Some(181.0)
    );
    assert_eq!(
        result
            .get("path_node_count")
            .and_then(|value| value.as_u64()),
        Some(5)
    );
}

#[pg_test]
fn aggregate_and_path_count_enforce_strict_json_contract() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let bad_traversal_key = sql_raises(
            "WITH req AS (
                SELECT jsonb_build_object(
                    'starts',
                    jsonb_build_array(graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')),
                    'direction', 'out',
                    'min_depth', 0,
                    'max_depth', 1,
                    'unexpected', true
                ) AS traversal
             )
             SELECT * FROM graph.path_count_estimate((SELECT traversal FROM req))",
        );
    let bad_aggregate_key = sql_raises(
            "WITH req AS (
                SELECT jsonb_build_object(
                    'starts',
                    jsonb_build_array(graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')),
                    'direction', 'out',
                    'min_depth', 0,
                    'max_depth', 1
                ) AS traversal
             )
             SELECT graph.aggregate(
                traversal,
                '{\"median\":[{\"table\":\"graph_test_users_pgtest\",\"column\":\"age\",\"as\":\"age_median\"}]}'::jsonb
             )
             FROM req",
        );
    let all_paths_rejected = sql_raises(
            "WITH req AS (
                SELECT jsonb_build_object(
                    'starts',
                    jsonb_build_array(graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')),
                    'direction', 'out',
                    'min_depth', 0,
                    'max_depth', 1
                ) AS traversal
             )
             SELECT graph.aggregate(
                traversal,
                '{\"count\":[{\"table\":\"graph_test_users_pgtest\",\"column\":\"id\",\"as\":\"user_count\"}]}'::jsonb,
                scope := 'all_possible_paths',
                path_limit := 1
             )
             FROM req",
        );

    assert!(bad_traversal_key);
    assert!(bad_aggregate_key);
    assert!(all_paths_rejected);
}
