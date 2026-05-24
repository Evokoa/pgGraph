#[pg_test]
fn traversal_helpers_edge_path_and_composition_apis_work() {
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
    Spi::run("SELECT graph.add_filter_column('graph_test_users_pgtest'::regclass, 'age')")
        .expect("add age filter failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let edge_label = Spi::get_one::<String>(
        "SELECT edge_path->>0
             FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 1, hydrate := false)
             WHERE node_id = 'u2'",
    )
    .expect("edge_path traversal failed")
    .expect("edge_path label missing");
    let helper_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                filter := graph.all(ARRAY[graph.greater_than('age', 40)]),
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("helper filter traversal failed")
    .unwrap_or(0);
    let multi_start_roots = Spi::get_one::<i64>(
        "SELECT count(DISTINCT root_id)
             FROM graph.traverse(
                ARRAY['graph_test_users_pgtest'::regclass, 'graph_test_users_pgtest'::regclass],
                ARRAY['u1'::text, 'u2'::text],
                0,
                hydrate := false
             )",
    )
    .expect("multi-start traversal failed")
    .unwrap_or(0);
    let traverse_search_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse_search(
                'name',
                'Alice',
                table_filter := 'graph_test_users_pgtest'::regclass,
                max_depth := 1,
                hydrate := false
             )
             WHERE root_id = 'u1' AND node_id = 'u2'",
    )
    .expect("traverse_search failed")
    .unwrap_or(0);
    let search_nodes_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search_nodes(
                'name',
                'Alice',
                table_filter := 'graph_test_users_pgtest'::regclass,
                mode := 'exact'
             )
             WHERE node_id = 'u1' AND verified",
    )
    .expect("search_nodes failed")
    .unwrap_or(0);

    assert_eq!(edge_label, "friend");
    assert_eq!(helper_count, 1);
    assert_eq!(multi_start_roots, 2);
    assert_eq!(traverse_search_count, 1);
    assert_eq!(search_nodes_count, 1);
}

#[pg_test]
fn multi_start_traverse_applies_limit_after_global_merge() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (row_count, first_root) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT count(*)::bigint, min(root_id)
                     FROM graph.traverse(
                        ARRAY[
                            'graph_test_users_pgtest'::regclass,
                            'graph_test_users_pgtest'::regclass
                        ],
                        ARRAY['u1'::text, 'u2'::text],
                        0,
                        hydrate := false,
                        max_rows := 1,
                        row_offset := 0
                     )",
                None,
                &[],
            )
            .expect("multi-start limited traversal failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .expect("multi-start limited traversal read failed");

    assert_eq!(row_count, 1);
    assert_eq!(first_root, "u1");
}

#[pg_test]
fn multi_start_traverse_honors_node_global_uniqueness() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
             VALUES ('u3', 'Carol', 29)",
    )
    .expect("insert converging user failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
             VALUES ('f2', 'u1', 'u3'), ('f3', 'u2', 'u3')",
    )
    .expect("insert converging friendships failed");
    Spi::run("UPDATE public.graph_test_users_pgtest SET name = 'Root' WHERE id IN ('u1', 'u2')")
        .expect("set searchable roots failed");
    build_friendship_fixture_graph();

    let counts = Spi::connect(|client| {
        let per_root = client
            .select(
                "SELECT count(*)::bigint
                     FROM graph.traverse(
                        ARRAY[
                            'graph_test_users_pgtest'::regclass,
                            'graph_test_users_pgtest'::regclass
                        ],
                        ARRAY['u1'::text, 'u2'::text],
                        max_depth := 1,
                        uniqueness := 'node_per_root',
                        include_start := false,
                        hydrate := false
                     )
                     WHERE node_id = 'u3'",
                None,
                &[],
            )
            .expect("node_per_root traversal failed")
            .first()
            .get::<i64>(1)?
            .unwrap_or_default();
        let global = client
            .select(
                "SELECT count(*)::bigint
                     FROM graph.traverse(
                        ARRAY[
                            'graph_test_users_pgtest'::regclass,
                            'graph_test_users_pgtest'::regclass
                        ],
                        ARRAY['u1'::text, 'u2'::text],
                        max_depth := 1,
                        uniqueness := 'node_global',
                        include_start := false,
                        hydrate := false
                     )
                     WHERE node_id = 'u3'",
                None,
                &[],
            )
            .expect("node_global traversal failed")
            .first()
            .get::<i64>(1)?
            .unwrap_or_default();
        let default_global = client
            .select(
                "SELECT count(*)::bigint
                     FROM graph.traverse(
                        ARRAY[
                            'graph_test_users_pgtest'::regclass,
                            'graph_test_users_pgtest'::regclass
                        ],
                        ARRAY['u1'::text, 'u2'::text],
                        max_depth := 1,
                        include_start := false,
                        hydrate := false
                     )
                     WHERE node_id = 'u3'",
                None,
                &[],
            )
            .expect("default traversal failed")
            .first()
            .get::<i64>(1)?
            .unwrap_or_default();
        let search_per_root = client
            .select(
                "SELECT count(*)::bigint
                     FROM graph.traverse_search(
                        'name',
                        'Root',
                        table_filter := 'graph_test_users_pgtest'::regclass,
                        search_mode := 'exact',
                        max_depth := 1,
                        uniqueness := 'node_per_root',
                        include_start := false,
                        hydrate := false
                     )
                     WHERE node_id = 'u3'",
                None,
                &[],
            )
            .expect("node_per_root traverse_search failed")
            .first()
            .get::<i64>(1)?
            .unwrap_or_default();
        let search_global = client
            .select(
                "SELECT count(*)::bigint
                     FROM graph.traverse_search(
                        'name',
                        'Root',
                        table_filter := 'graph_test_users_pgtest'::regclass,
                        search_mode := 'exact',
                        max_depth := 1,
                        uniqueness := 'node_global',
                        include_start := false,
                        hydrate := false
                     )
                     WHERE node_id = 'u3'",
                None,
                &[],
            )
            .expect("node_global traverse_search failed")
            .first()
            .get::<i64>(1)?
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((
            per_root,
            global,
            default_global,
            search_per_root,
            search_global,
        ))
    })
    .expect("multi-start uniqueness counts failed");
    let (
        per_root_count,
        global_count,
        default_count,
        search_per_root_count,
        search_global_count,
    ) = counts;

    assert_eq!(per_root_count, 2);
    assert_eq!(global_count, 1);
    assert_eq!(default_count, 1);
    assert_eq!(search_per_root_count, 2);
    assert_eq!(search_global_count, 1);
}

#[pg_test]
fn format_path_formats_traversal_path_and_edge_path() {
    let formatted = Spi::get_one::<String>(
        "SELECT graph.format_path(
                '[
                    {\"table\":\"users\",\"id\":\"u1\"},
                    {\"table\":\"users\",\"id\":\"u2\"},
                    {\"table\":\"companies\",\"id\":\"c1\"}
                ]'::jsonb,
                '[\"friend\", \"officer_of\"]'::jsonb
             )",
    )
    .expect("format_path failed")
    .expect("format_path returned null");
    let custom_separator = Spi::get_one::<String>(
        "SELECT graph.format_path(
                '[
                    {\"table\":\"users\",\"id\":\"u1\"},
                    {\"table\":\"users\",\"id\":\"u2\"}
                ]'::jsonb,
                '[\"friend\"]'::jsonb,
                E'\n'
             )",
    )
    .expect("format_path with custom separator failed")
    .expect("format_path returned null");
    let root = Spi::get_one::<String>(
        "SELECT graph.format_path(
                '[{\"table\":\"users\",\"id\":\"u1\"}]'::jsonb,
                '[]'::jsonb
             )",
    )
    .expect("format_path root failed")
    .expect("format_path returned null");

    assert_eq!(
        formatted,
        "users:u1 --friend--> users:u2 | users:u2 --officer_of--> companies:c1"
    );
    assert_eq!(custom_separator, "users:u1 --friend--> users:u2");
    assert_eq!(root, "");
}

#[pg_test]
fn edge_label_column_drives_shortest_path_and_traverse_labels() {
    reset_and_create_fixtures();
    Spi::run("ALTER TABLE public.graph_test_friendships_pgtest ADD COLUMN rel_type TEXT")
        .expect("add rel_type failed");
    Spi::run(
        "UPDATE public.graph_test_friendships_pgtest
             SET rel_type = 'knows'",
    )
    .expect("set rel_type failed");
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
                'relationship',
                label_column := 'rel_type'
            )",
    )
    .expect("add dynamic-label edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let edge_label = Spi::get_one::<String>(
        "SELECT edge_label
             FROM graph.shortest_path(
                'graph_test_users_pgtest'::regclass,
                'u1',
                'graph_test_users_pgtest'::regclass,
                'u2',
                5
             )
             WHERE step = 1",
    )
    .expect("shortest_path label failed")
    .expect("edge_label missing");
    let traverse_label = Spi::get_one::<String>(
        "SELECT edge_path->>0
             FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 1, hydrate := false)
             WHERE node_id = 'u2'",
    )
    .expect("traverse label failed")
    .expect("edge_path label missing");

    assert_eq!(edge_label, "knows");
    assert_eq!(traverse_label, "knows");
}

#[pg_test]
fn edge_and_weighted_path_v1_acceptance() {
    reset_and_create_fixtures();
    Spi::run(
        "CREATE TABLE public.graph_test_weighted_nodes_pgtest (
                id   TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create weighted nodes failed");
    Spi::run(
        "CREATE TABLE public.graph_test_weighted_edges_pgtest (
                id       TEXT PRIMARY KEY,
                src      TEXT NOT NULL,
                dst      TEXT NOT NULL,
                cost     INT NOT NULL,
                rel_type TEXT
            )",
    )
    .expect("create weighted edges failed");
    Spi::run(
        "INSERT INTO public.graph_test_weighted_nodes_pgtest (id, name) VALUES
                ('a', 'A'),
                ('b', 'B'),
                ('c', 'C'),
                ('d', 'D')",
    )
    .expect("insert weighted nodes failed");
    Spi::run(
        "INSERT INTO public.graph_test_weighted_edges_pgtest (id, src, dst, cost, rel_type)
             VALUES
                ('e1', 'a', 'b', 5, 'expensive'),
                ('e2', 'b', 'd', 5, NULL),
                ('e3', 'a', 'c', 1, ''),
                ('e4', 'c', 'd', 1, 'cheap')",
    )
    .expect("insert weighted edges failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_weighted_nodes_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add weighted node table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_weighted_edges_pgtest'::regclass,
                from_column := 'src',
                to_table := 'graph_test_weighted_nodes_pgtest'::regclass,
                to_column := 'dst',
                label := 'route',
                bidirectional := false,
                weight_column := 'cost',
                label_column := 'rel_type'
            )",
    )
    .expect("add weighted edge table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let weighted = Spi::get_one::<String>(
        "SELECT string_agg(node_id, '->' ORDER BY step) || ':' || max(total_cost)::text
             FROM graph.weighted_shortest_path(
                'graph_test_weighted_nodes_pgtest'::regclass,
                'a',
                'graph_test_weighted_nodes_pgtest'::regclass,
                'd'
             )",
    )
    .expect("weighted shortest path failed")
    .expect("weighted shortest path returned no rows");
    let weighted_steps = Spi::get_one::<String>(
        "SELECT string_agg(
                    step::text || ':' || node_id || ':' ||
                    coalesce(edge_label, '<start>') || ':' ||
                    coalesce(edge_weight::text, '<start>') || ':' ||
                    step_cost::text || ':' || total_cost::text,
                    ',' ORDER BY step
                )
             FROM graph.weighted_shortest_path(
                'graph_test_weighted_nodes_pgtest'::regclass,
                'a',
                'graph_test_weighted_nodes_pgtest'::regclass,
                'd'
             )",
    )
    .expect("weighted path step detail failed")
    .expect("weighted path step detail missing");
    let weighted_table_identity_rows = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.weighted_shortest_path(
                'graph_test_weighted_nodes_pgtest'::regclass,
                'a',
                'graph_test_weighted_nodes_pgtest'::regclass,
                'd'
             )
             WHERE node_table = 'graph_test_weighted_nodes_pgtest'::regclass::oid
               AND node_table_name = 'graph_test_weighted_nodes_pgtest'::regclass::text",
    )
    .expect("weighted path table identity check failed")
    .unwrap_or(0);
    let fallback_label = Spi::get_one::<String>(
        "SELECT edge_path->>0
             FROM graph.traverse(
                'graph_test_weighted_nodes_pgtest'::regclass,
                'a',
                1,
                direction := 'out',
                hydrate := false
             )
             WHERE node_id = 'c'",
    )
    .expect("fallback label traverse failed")
    .expect("fallback label missing");
    let null_fallback_label = Spi::get_one::<String>(
        "SELECT edge_path->>0
             FROM graph.traverse(
                'graph_test_weighted_nodes_pgtest'::regclass,
                'b',
                1,
                direction := 'out',
                hydrate := false
             )
             WHERE node_id = 'd'",
    )
    .expect("null fallback label traverse failed")
    .expect("null fallback label missing");
    let reverse_out_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_weighted_nodes_pgtest'::regclass,
                'd',
                1,
                direction := 'out',
                hydrate := false
             )
             WHERE node_id IN ('b', 'c')",
    )
    .expect("unidirectional reverse traverse failed")
    .unwrap_or(-1);
    let inbound_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse(
                'graph_test_weighted_nodes_pgtest'::regclass,
                'd',
                1,
                direction := 'in',
                hydrate := false
             )
             WHERE node_id IN ('b', 'c')",
    )
    .expect("inbound unidirectional traverse failed")
    .unwrap_or(-1);
    let no_weight_param = Spi::get_one::<bool>(
        "SELECT bool_and(pg_get_function_arguments(p.oid) NOT LIKE '%weight%')
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname = 'graph'
               AND p.proname = 'weighted_shortest_path'",
    )
    .expect("weighted signature inspection failed")
    .unwrap_or(false);
    let weighted_shape = Spi::get_one::<bool>(
        "SELECT pg_get_function_result(p.oid) =
                    'TABLE(step integer, node_table oid, node_table_name text, node_id text, edge_label text, edge_weight bigint, step_cost bigint, total_cost bigint)'
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname = 'graph'
               AND p.proname = 'weighted_shortest_path'",
    )
    .expect("weighted result shape inspection failed")
    .unwrap_or(false);
    let no_weighted_path_rows = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.weighted_shortest_path(
                'graph_test_weighted_nodes_pgtest'::regclass,
                'd',
                'graph_test_weighted_nodes_pgtest'::regclass,
                'a'
             )",
    )
    .expect("weighted empty path inspection failed")
    .unwrap_or(-1);

    assert_eq!(weighted, "a->c->d:2");
    assert_eq!(weighted_steps, "0:a:<start>:<start>:0:2,1:c:route:1:1:2,2:d:cheap:1:2:2");
    assert_eq!(weighted_table_identity_rows, 3);
    assert_eq!(fallback_label, "route");
    assert_eq!(null_fallback_label, "route");
    assert_eq!(reverse_out_count, 0);
    assert_eq!(inbound_count, 2);
    assert!(no_weight_param);
    assert!(weighted_shape);
    assert_eq!(no_weighted_path_rows, 0);
}

#[pg_test]
fn traverse_accepts_dfs_out_and_returns_path_coordinates() {
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
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let path_id = Spi::get_one::<String>(
        "SELECT path->0->>'id'
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                direction := 'out',
                strategy := 'dfs',
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("dfs traverse failed")
    .expect("missing first path id");
    let table_name = Spi::get_one::<String>(
        "SELECT path->0->>'table'
             FROM graph.traverse(
                'graph_test_users_pgtest'::regclass,
                'u1',
                1,
                direction := 'out',
                strategy := 'dfs',
                hydrate := false
             )
             WHERE node_id = 'u2'",
    )
    .expect("dfs path table failed")
    .expect("missing first path table");

    assert_eq!(path_id, "u1");
    assert!(table_name.ends_with("graph_test_users_pgtest"));
}

#[pg_test]
fn traverse_accepts_in_direction_and_rejects_weighted_strategy() {
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

    let in_result = super::validate_traverse_options("in", None, "bfs", "node_global");
    let weighted_result = super::validate_traverse_options("any", None, "weighted", "node_global");
    let invalid_uniqueness = super::validate_traverse_options("any", None, "bfs", "node_local");

    assert!(in_result.is_ok());
    assert!(weighted_result.is_err());
    assert!(invalid_uniqueness.is_err());
}

#[pg_test]
fn aggregation_traversal_direction_both_maps_to_any() {
    reset_and_create_fixtures();
    let node_ref = Spi::get_one::<String>(
        "SELECT graph.node_ref_string('graph_test_users_pgtest'::regclass, 'u1')",
    )
    .expect("node_ref_string failed")
    .expect("node_ref_string returned null");
    let request = super::sql_aggregation::parse_aggregation_traversal_request(&serde_json::json!({
        "starts": [node_ref],
        "direction": "both"
    }))
    .expect("parse aggregation traversal request failed");

    assert_eq!(request.direction, super::types::TraversalDirection::Any);
}
