fn build_p9_open_type_query_fixture(label_count: i32, mutable: bool) {
    reset_and_create_fixtures();
    let mode = if mutable {
        "mutable_overlay"
    } else {
        "csr_readonly"
    };
    Spi::run(&format!(
        "TRUNCATE public.graph_test_friendships_pgtest;
         INSERT INTO public.graph_test_users_pgtest (id, name, age)
         VALUES ('u3', 'Cara', 29);
         ALTER TABLE public.graph_test_friendships_pgtest
           ADD COLUMN rel_type text NOT NULL DEFAULT 'type_1';
         INSERT INTO public.graph_test_friendships_pgtest
           (id, user_id, friend_id, rel_type)
         SELECT 'edge_' || value,
                'u1',
                CASE WHEN value = {label_count} THEN 'u3' ELSE 'u2' END,
                'type_' || value
           FROM generate_series(1, {label_count}) AS value;
         SELECT graph.add_table(
           'graph_test_users_pgtest'::regclass,
           id_column := 'id', columns := ARRAY['name', 'age']);
         SELECT graph.add_edge(
           from_table := 'graph_test_friendships_pgtest'::regclass,
           from_column := 'user_id',
           to_table := 'graph_test_users_pgtest'::regclass,
           to_column := 'friend_id', label := 'fallback',
           bidirectional := false, label_column := 'rel_type');
         SET graph.mutable_enabled = on;
         SELECT * FROM graph.build(mode := '{mode}')"
    ))
    .expect("build P9 open-type query fixture failed");
}

#[pg_test]
fn open_type_255_traversal_paths_gql_and_cypher_filter_exactly() {
    build_p9_open_type_query_fixture(255, false);

    let inventory_count = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.edge_types(after_type_id := 0, max_rows := 256)",
    )
    .expect("read P9 boundary inventory failed")
    .unwrap_or_default();
    let type_254_target = Spi::get_one::<String>(
        "SELECT string_agg(node_id, ',' ORDER BY node_id)
           FROM graph.traverse(
             'graph_test_users_pgtest'::regclass, 'u1', 1,
             edge_types := ARRAY['type_254'], hydrate := false)
          WHERE depth = 1",
    )
    .expect("traverse type_254 failed")
    .unwrap_or_default();
    let type_255_target = Spi::get_one::<String>(
        "SELECT string_agg(node_id, ',' ORDER BY node_id)
           FROM graph.traverse(
             'graph_test_users_pgtest'::regclass, 'u1', 1,
             edge_types := ARRAY['type_255'], hydrate := false)
          WHERE depth = 1",
    )
    .expect("traverse type_255 failed")
    .unwrap_or_default();
    let path_label = Spi::get_one::<String>(
        "SELECT edge_label
           FROM graph.shortest_path(
             'graph_test_users_pgtest'::regclass, 'u1',
             'graph_test_users_pgtest'::regclass, 'u3', 1,
             hydrate := false, edge_types := ARRAY['type_255'])
          WHERE step = 1",
    )
    .expect("shortest path type_255 failed")
    .unwrap_or_default();
    let gql_target = Spi::get_one::<String>(
        "SELECT row #>> '{v,_id,id}'
           FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest {id: ''u1''})
                    -[:type_255]->(v:graph_test_users_pgtest)
              RETURN v', hydrate := false)",
    )
    .expect("GQL type_255 failed")
    .unwrap_or_default();
    let cypher_target = Spi::get_one::<String>(
        "SELECT row #>> '{v,_id,id}'
           FROM graph.cypher(
             'MATCH (u:graph_test_users_pgtest {id: ''u1''})
                    -[:type_255]->(v:graph_test_users_pgtest)
              RETURN v', hydrate := false)",
    )
    .expect("Cypher type_255 failed")
    .unwrap_or_default();

    assert_eq!(inventory_count, 255);
    assert_eq!(type_254_target, "u2");
    assert_eq!(type_255_target, "u3");
    assert_eq!(path_label, "type_255");
    assert_eq!(gql_target, "u3");
    assert_eq!(cypher_target, "u3");
}

#[pg_test]
fn open_type_parallel_relationships_preserve_identity_and_exact_output() {
    build_p9_open_type_query_fixture(255, false);
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest
           (id, user_id, friend_id, rel_type)
         VALUES ('edge_255_parallel', 'u1', 'u3', 'type_255');
         SELECT * FROM graph.build()",
    )
    .expect("rebuild parallel open-type fixture failed");

    let identities = Spi::get_one::<String>(
        "SELECT string_agg(row #>> '{r,id}', ',' ORDER BY row #>> '{r,id}')
           FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest {id: ''u1''})
                    -[r:type_255]->(v:graph_test_users_pgtest {id: ''u3''})
              RETURN r', hydrate := true)
          WHERE row #>> '{r,_type}' = 'type_255'",
    )
    .expect("read parallel open-type identities failed")
    .unwrap_or_default();
    assert_eq!(identities, "edge_255,edge_255_parallel");
}

#[pg_test]
fn open_type_dynamic_label_equality_lowers_to_compact_type_filter() {
    build_p9_open_type_query_fixture(255, false);

    let gql_literal = Spi::get_one::<String>(
        "SELECT row #>> '{v,_id,id}'
           FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest {id: ''u1''})
                    -[r]->(v:graph_test_users_pgtest)
              WHERE r.rel_type = ''type_255''
              RETURN v', hydrate := false)",
    )
    .expect("dynamic label-column equality query failed")
    .unwrap_or_default();
    let gql_param = Spi::get_one::<String>(
        "SELECT row #>> '{v,_id,id}'
           FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest {id: ''u1''})
                    -[r]->(v:graph_test_users_pgtest)
              WHERE r.rel_type = $type
              RETURN v',
             params := '{\"type\":\"type_255\"}'::jsonb,
             hydrate := false)",
    )
    .expect("parameterized dynamic label-column equality query failed")
    .unwrap_or_default();
    let cypher_param = Spi::get_one::<String>(
        "SELECT row #>> '{v,_id,id}'
           FROM graph.cypher(
             'MATCH (u:graph_test_users_pgtest {id: ''u1''})
                    -[r]->(v:graph_test_users_pgtest)
              WHERE $type = r.rel_type
              RETURN v',
             params := '{\"type\":\"type_255\"}'::jsonb,
             hydrate := false)",
    )
    .expect("Cypher dynamic label-column equality query failed")
    .unwrap_or_default();
    let explain = Spi::get_one::<String>(
        "SELECT graph.gql_explain(
           'MATCH (u:graph_test_users_pgtest)-[r]->(v:graph_test_users_pgtest)
            WHERE r.rel_type = $type RETURN v')",
    )
    .expect("explain dynamic label-column equality failed")
    .unwrap_or_default();

    assert_eq!(gql_literal, "u3");
    assert_eq!(gql_param, "u3");
    assert_eq!(cypher_param, "u3");
    assert!(explain.contains("rel=dynamic(rel_type)"));
}

#[pg_test]
fn open_type_absent_and_invalid_label_diagnostics_are_surface_stable() {
    build_p9_open_type_query_fixture(255, false);

    let traverse_state = sqlstate_for_error(
        "SELECT * FROM graph.traverse(
           'graph_test_users_pgtest'::regclass, 'u1', 1,
           edge_types := ARRAY['not_loaded'], hydrate := false)",
    );
    let path_state = sqlstate_for_error(
        "SELECT * FROM graph.shortest_path(
           'graph_test_users_pgtest'::regclass, 'u1',
           'graph_test_users_pgtest'::regclass, 'u3', 1,
           hydrate := false, edge_types := ARRAY['not_loaded'])",
    );
    let gql_absent = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest)-[:not_loaded]->
                  (v:graph_test_users_pgtest) RETURN v', hydrate := false)",
    )
    .expect("GQL absent open type failed")
    .unwrap_or_default();
    let cypher_absent = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.cypher(
           'MATCH (u:graph_test_users_pgtest)-[:not_loaded]->
                  (v:graph_test_users_pgtest) RETURN v', hydrate := false)",
    )
    .expect("Cypher absent open type failed")
    .unwrap_or_default();
    let invalid_gql = sqlstate_for_error(
        "SELECT * FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest)-[:not-valid]->
                  (v:graph_test_users_pgtest) RETURN v', hydrate := false)",
    );
    let invalid_cypher = sqlstate_for_error(
        "SELECT * FROM graph.cypher(
           'MATCH (u:graph_test_users_pgtest)-[:not-valid]->
                  (v:graph_test_users_pgtest) RETURN v', hydrate := false)",
    );

    assert_eq!(traverse_state, path_state);
    assert_eq!(traverse_state.as_deref(), Some("22023"));
    assert_eq!((gql_absent, cypher_absent), (0, 0));
    assert!(invalid_gql.is_some());
    assert_eq!(invalid_gql, invalid_cypher);
}

#[pg_test]
fn open_type_acl_rls_force_bypass_and_transaction_matrix() {
    build_p9_open_type_query_fixture(255, true);
    create_error_sqlstate_helper();
    Spi::run(
        "DROP ROLE IF EXISTS graph_p9_open_reader;
         DROP ROLE IF EXISTS graph_p9_open_bypass;
         DROP ROLE IF EXISTS graph_p9_open_no_edge_acl;
         CREATE ROLE graph_p9_open_reader;
         CREATE ROLE graph_p9_open_bypass BYPASSRLS;
         CREATE ROLE graph_p9_open_no_edge_acl;
         GRANT USAGE ON SCHEMA graph, public TO graph_p9_open_reader;
         GRANT USAGE ON SCHEMA graph, public TO graph_p9_open_bypass,
                                                   graph_p9_open_no_edge_acl;
         GRANT SELECT ON public.graph_test_users_pgtest,
                         public.graph_test_friendships_pgtest
           TO graph_p9_open_reader;
         GRANT SELECT ON public.graph_test_users_pgtest,
                         public.graph_test_friendships_pgtest
           TO graph_p9_open_bypass;
         GRANT SELECT ON public.graph_test_users_pgtest
           TO graph_p9_open_no_edge_acl;
         GRANT EXECUTE ON FUNCTION public.graph_test_sqlstate(text)
           TO graph_p9_open_no_edge_acl;
         ALTER TABLE public.graph_test_friendships_pgtest ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.graph_test_friendships_pgtest FORCE ROW LEVEL SECURITY;
         CREATE POLICY graph_p9_open_visible
           ON public.graph_test_friendships_pgtest FOR SELECT
           TO graph_p9_open_reader USING (rel_type <> 'type_255');
         SET ROLE graph_p9_open_reader;
         SELECT graph._test_set_visibility_strategy('eager')",
    )
    .expect("prepare P9 open-type ACL/RLS fixture failed");
    let hidden = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r]->
                  (v:graph_test_users_pgtest)
            WHERE r.rel_type = ''type_255'' RETURN v', hydrate := false)",
    )
    .expect("query RLS-hidden open type failed")
    .unwrap_or_default();
    Spi::run("SELECT graph._test_set_visibility_strategy('lazy')")
        .expect("select P9 lazy strategy failed");
    let hidden_lazy = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r]->
                  (v:graph_test_users_pgtest)
            WHERE r.rel_type = ''type_255'' RETURN v', hydrate := false)",
    )
    .expect("query lazy RLS-hidden open type failed")
    .unwrap_or_default();
    Spi::run("RESET ROLE; SET ROLE graph_p9_open_bypass")
        .expect("select P9 BYPASSRLS role failed");
    let bypass_visible = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r]->
                  (v:graph_test_users_pgtest)
            WHERE r.rel_type = ''type_255'' RETURN v', hydrate := false)",
    )
    .expect("query BYPASSRLS open type failed")
    .unwrap_or_default();
    Spi::run("RESET ROLE; SET ROLE graph_p9_open_no_edge_acl")
        .expect("select P9 denied-ACL role failed");
    let denied_acl = Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sqlstate({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r]->
                  (v:graph_test_users_pgtest)
            WHERE r.rel_type = $type RETURN v', hydrate := false)",
        )
    ))
    .expect("capture denied edge ACL SQLSTATE failed");
    let denied_invalid_acl = Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sqlstate({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r]->
                  (v:graph_test_users_pgtest)
            WHERE r.rel_type = $type RETURN v',
           params := '{\"type\":42}'::jsonb, hydrate := false)",
        )
    ))
    .expect("capture denied invalid-parameter ACL SQLSTATE failed");
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("reset P9 open-type role failed");

    let created = Spi::get_one::<String>(
        "SELECT row #>> '{r,_type}' FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u2''}),
                  (v:graph_test_users_pgtest {id: ''u3''})
            CREATE (u)-[r:type_256 {id: ''tx-type-256''}]->(v)
            RETURN r', hydrate := false)",
    )
    .expect("create transaction-local P9 open type failed")
    .unwrap_or_default();
    let tx_visible = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.traverse(
           'graph_test_users_pgtest'::regclass, 'u2', 1,
           edge_types := ARRAY['type_256'], hydrate := false)
          WHERE node_id = 'u3'",
    )
    .expect("query transaction-local P9 open type failed")
    .unwrap_or_default();

    assert_eq!(hidden, 0);
    assert_eq!(hidden_lazy, 0);
    assert_eq!(bypass_visible, 1);
    assert_eq!(denied_acl.as_deref(), Some("42501"));
    assert_eq!(denied_invalid_acl.as_deref(), Some("42501"));
    assert_eq!(created, "type_256");
    assert_eq!(tx_visible, 1);
}

#[pg_test]
fn open_type_cancellation_cleans_query_state_and_same_backend_retries() {
    build_p9_open_type_query_fixture(255, false);
    Spi::run(
        "DROP ROLE IF EXISTS graph_p9_cancel_reader;
         CREATE ROLE graph_p9_cancel_reader;
         GRANT USAGE ON SCHEMA graph, public TO graph_p9_cancel_reader;
         GRANT SELECT ON public.graph_test_users_pgtest,
                         public.graph_test_friendships_pgtest
           TO graph_p9_cancel_reader;
         ALTER TABLE public.graph_test_users_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_p9_cancel_nodes
           ON public.graph_test_users_pgtest FOR SELECT
           TO graph_p9_cancel_reader USING (true);
         ALTER TABLE public.graph_test_friendships_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_p9_cancel_edges
           ON public.graph_test_friendships_pgtest FOR SELECT
           TO graph_p9_cancel_reader USING (true);
         SET ROLE graph_p9_cancel_reader;
         SELECT graph._test_set_visibility_strategy('lazy');
         SELECT graph._test_arm_lazy_visibility_cancel()",
    )
    .expect("arm P9 open-type cancellation fixture failed");
    let statement = "SELECT * FROM graph.gql(
       'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r]->
              (v:graph_test_users_pgtest)
        WHERE r.rel_type = ''type_255'' RETURN v', hydrate := false)";
    let cancelled = workflow_cancellation(statement);
    let clean = Spi::get_one::<bool>("SELECT graph._test_visibility_resolution_state_empty()")
        .expect("inspect P9 cancellation cleanup failed")
        .unwrap_or(false);
    let retry = Spi::get_one::<i64>(&format!("SELECT count(*)::bigint FROM ({statement}) q"))
        .expect("retry P9 open-type query failed")
        .unwrap_or_default();
    Spi::run("RESET ROLE; SELECT graph._test_set_visibility_strategy('auto')")
        .expect("restore P9 cancellation fixture failed");

    assert!(cancelled);
    assert!(clean);
    assert_eq!(retry, 1);
}
