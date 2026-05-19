use pgrx::prelude::*;

fn create_error_capture_helper() {
    Spi::run(
        "CREATE OR REPLACE FUNCTION public.graph_test_sql_raises(statement text)
             RETURNS boolean
             LANGUAGE plpgsql
             AS $$
             BEGIN
                 EXECUTE statement;
                 RETURN false;
             EXCEPTION WHEN others THEN
                 RETURN true;
             END
             $$",
    )
    .expect("create error capture helper failed");
}

fn sql_raises(statement: &str) -> bool {
    create_error_capture_helper();
    Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(statement)
    ))
    .expect("error capture query failed")
    .unwrap_or(false)
}

fn explain_source_search_query(property_value: &str, mode: &str, table_oid: u32) -> String {
    let search_mode = super::types::SearchMode::parse(mode).expect("valid search mode");
    let (query, params) = super::sql_search::source_table_search_sql_and_params_for_test(
        "name",
        property_value,
        Some(table_oid),
        search_mode,
        false,
        None,
        false,
    )
    .expect("source search SQL generation failed")
    .into_iter()
    .next()
    .expect("source search SQL missing");

    Spi::connect(|client| {
        let params = params
            .iter()
            .map(|param| param.as_str().into())
            .collect::<Vec<_>>();
        let result = client
            .select(&format!("EXPLAIN {}", query), None, &params)
            .expect("explain source search query failed");
        let mut lines = Vec::new();
        for row in result {
            lines.push(
                row.get::<String>(1)
                    .expect("explain row read failed")
                    .unwrap_or_default(),
            );
        }
        Ok::<_, pgrx::spi::Error>(lines.join("\n"))
    })
    .expect("explain source search plan read failed")
}

fn clear_graph_catalog_for_test() {
    Spi::run(
        "TRUNCATE graph._registered_filter_columns,
                      graph._registered_edges,
                      graph._registered_tables,
                      graph._build_jobs,
                      graph._sync_log,
                      graph._sync_buffer
             RESTART IDENTITY",
    )
    .expect("clear graph catalog failed");
}

fn reset_and_create_fixtures() {
    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run("SET graph.persist_on_build = off").expect("disable persist_on_build failed");
    Spi::run("SET graph.enabled = on").expect("enable graph failed");
    Spi::run("SET graph.sync_mode = 'manual'").expect("reset sync_mode failed");
    Spi::run("SET graph.build_scan_mode = 'select'").expect("reset build_scan_mode failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_junction_pgtest CASCADE")
        .expect("drop junction failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_friendships_pgtest CASCADE")
        .expect("drop friendships failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_users_pgtest CASCADE")
        .expect("drop users failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_bad_pgtest CASCADE").expect("drop bad failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_composite_pgtest CASCADE")
        .expect("drop composite failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_weighted_edges_pgtest CASCADE")
        .expect("drop weighted edges failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_weighted_nodes_pgtest CASCADE")
        .expect("drop weighted nodes failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_test_filter_context_pgtest CASCADE")
        .expect("drop filter context failed");

    Spi::run(
        "CREATE TABLE public.graph_test_users_pgtest (
                id   TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                age  INT NOT NULL DEFAULT 0
            )",
    )
    .expect("create users failed");

    Spi::run(
        "CREATE TABLE public.graph_test_friendships_pgtest (
                id        TEXT PRIMARY KEY,
                user_id   TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id),
                friend_id TEXT NOT NULL REFERENCES public.graph_test_users_pgtest(id)
            )",
    )
    .expect("create friendships failed");

    Spi::run(
        "CREATE TABLE public.graph_test_bad_pgtest (
                id   TEXT PRIMARY KEY,
                note TEXT NOT NULL
            )",
    )
    .expect("create bad failed");

    // Composite entity table (at least one PK col is NOT a FK)
    Spi::run(
        "CREATE TABLE public.graph_test_composite_pgtest (
                org_id  TEXT NOT NULL,
                user_id TEXT NOT NULL,
                label   TEXT,
                PRIMARY KEY (org_id, user_id)
            )",
    )
    .expect("create composite failed");

    Spi::run(
            "INSERT INTO public.graph_test_users_pgtest (id, name, age) VALUES ('u1', 'Alice', 37), ('u2', 'Bob', 41)",
        )
        .expect("insert users failed");
    Spi::run(
            "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id) VALUES ('f1', 'u1', 'u2')",
        )
        .expect("insert friendships failed");
    Spi::run(
            "INSERT INTO public.graph_test_composite_pgtest (org_id, user_id, label) VALUES ('org1', 'emp1', 'Engineer'), ('org1', 'emp2', 'Manager')",
        )
        .expect("insert composite failed");
}

fn build_friendship_fixture_graph() {
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
    Spi::run("SELECT * FROM graph.build()").expect("build friendship graph failed");
}

fn reset_and_create_synthetic_fixture(node_count: i32, hub_fanout: i32, persist: bool) {
    assert!(node_count >= 100, "synthetic fixture needs at least 100 nodes");
    assert!(hub_fanout >= 2, "synthetic fixture needs hub fanout >= 2");

    let hub_fanout = hub_fanout.min(node_count);

    Spi::run("SELECT pg_advisory_xact_lock(1918928211, 1735552872)")
        .expect("test fixture lock failed");
    Spi::run("SELECT graph.reset()").expect("reset failed");
    Spi::run("SET graph.auto_load = off").expect("disable auto_load failed");
    Spi::run(if persist {
        "SET graph.persist_on_build = on"
    } else {
        "SET graph.persist_on_build = off"
    })
    .expect("set persist_on_build failed");
    Spi::run("SET graph.enabled = on").expect("enable graph failed");
    Spi::run("SET graph.sync_mode = 'manual'").expect("reset sync_mode failed");
    Spi::run("SET graph.build_scan_mode = 'select'").expect("reset build_scan_mode failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_synth_edges_pgtest CASCADE")
        .expect("drop synthetic edges failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_synth_nodes_pgtest CASCADE")
        .expect("drop synthetic nodes failed");

    Spi::run(
        "CREATE TABLE public.graph_synth_nodes_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                tenant TEXT NOT NULL,
                score BIGINT NOT NULL,
                active BOOLEAN NOT NULL,
                parent_id TEXT
            )",
    )
    .expect("create synthetic nodes failed");

    Spi::run(&format!(
        "INSERT INTO public.graph_synth_nodes_pgtest (id, name, tenant, score, active, parent_id)
         SELECT
             i::text,
             'node-' || i::text,
             'tenant-' || (i % 10)::text,
             (i % 1000)::bigint,
             (i % 7) <> 0,
             CASE WHEN i > 1 THEN (i / 2)::bigint::text ELSE NULL END
         FROM generate_series(1, {}) AS i",
        node_count
    ))
    .expect("insert synthetic nodes failed");

    Spi::run(
        "CREATE TABLE public.graph_synth_edges_pgtest (
                id BIGSERIAL PRIMARY KEY,
                from_id TEXT NOT NULL REFERENCES public.graph_synth_nodes_pgtest(id),
                to_id TEXT NOT NULL REFERENCES public.graph_synth_nodes_pgtest(id),
                weight INT NOT NULL
            )",
    )
    .expect("create synthetic edges failed");

    Spi::run(&format!(
        "INSERT INTO public.graph_synth_edges_pgtest (from_id, to_id, weight)
         SELECT i::text, (i + 1)::text, 1
         FROM generate_series(1, {}) AS i",
        node_count - 1
    ))
    .expect("insert synthetic chain edges failed");

    Spi::run(&format!(
        "INSERT INTO public.graph_synth_edges_pgtest (from_id, to_id, weight)
         SELECT i::text, (i + 10)::text, 2
         FROM generate_series(1, {}) AS i",
        node_count - 10
    ))
    .expect("insert synthetic skip edges failed");

    Spi::run(&format!(
        "INSERT INTO public.graph_synth_edges_pgtest (from_id, to_id, weight)
         SELECT '1', i::text, 3
         FROM generate_series(100, {}) AS i
         WHERE i <= {}",
        hub_fanout, node_count
    ))
    .expect("insert synthetic hub edges failed");

    Spi::run(
        "SELECT graph.add_table(
                'public.graph_synth_nodes_pgtest'::regclass,
                'id',
                ARRAY['name', 'tenant', 'score', 'active']
            )",
    )
    .expect("add synthetic node table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'public.graph_synth_nodes_pgtest'::regclass,
                'parent_id',
                'public.graph_synth_nodes_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := false
            )",
    )
    .expect("add synthetic parent edge failed");
    Spi::run(
        "SELECT graph.add_edge(
                'public.graph_synth_edges_pgtest'::regclass,
                'from_id',
                'public.graph_synth_nodes_pgtest'::regclass,
                'to_id',
                'synthetic',
                bidirectional := false,
                weight_column := 'weight'
            )",
    )
    .expect("add synthetic weighted edge failed");
    Spi::run(
        "SELECT graph.add_filter_column(
                'public.graph_synth_nodes_pgtest'::regclass,
                'score',
                column_type := 'numeric'
            )",
    )
    .expect("add synthetic score filter failed");
    Spi::run(
        "SELECT graph.add_filter_column(
                'public.graph_synth_nodes_pgtest'::regclass,
                'tenant',
                column_type := 'text'
            )",
    )
    .expect("add synthetic tenant filter failed");
    Spi::run("SELECT * FROM graph.build()").expect("build synthetic graph failed");
}
