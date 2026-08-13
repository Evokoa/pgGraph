fn build_p8_transaction_label_fixture() {
    reset_and_create_fixtures();
    Spi::run(
        "SET graph.mutable_enabled = on;
         ALTER TABLE public.graph_test_friendships_pgtest
           ADD COLUMN rel_type TEXT NOT NULL DEFAULT 'base';
         SELECT graph.add_table(
           'graph_test_users_pgtest'::regclass,
           id_column := 'id', columns := ARRAY['name', 'age']);
         SELECT graph.add_edge(
           from_table := 'graph_test_friendships_pgtest'::regclass,
           from_column := 'user_id',
           to_table := 'graph_test_users_pgtest'::regclass,
           to_column := 'friend_id',
           label := 'fallback', bidirectional := false,
           label_column := 'rel_type');
         SELECT * FROM graph.build(mode := 'mutable_overlay')",
    )
    .expect("build P8.3 transaction label fixture failed");
}

fn create_p8_transaction_edge(edge_id: &str, rel_type: &str) -> String {
    Spi::get_one::<String>(&format!(
        "SELECT row #>> '{{r,_type}}'
           FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest {{id: ''u2''}}),
                    (v:graph_test_users_pgtest {{id: ''u1''}})
              CREATE (u)-[r:{rel_type} {{id: ''{edge_id}''}}]->(v)
              RETURN r',
             hydrate := false)",
    ))
    .expect("create transaction-local dynamic relationship failed")
    .unwrap_or_default()
}

#[pg_test]
fn tx_unseen_dynamic_label_filters_and_returns_exact_type() {
    build_p8_transaction_label_fixture();
    let returned_type = create_p8_transaction_edge("tx-alpha-edge", "tx_alpha");
    let gql_count = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest {id: ''u2''})
                    -[r:tx_alpha]->
                    (v:graph_test_users_pgtest {id: ''u1''})
              RETURN r', hydrate := false)",
    )
    .expect("match transaction-local unseen relationship type failed")
    .unwrap_or_default();
    let traverse_edge_type = Spi::get_one::<String>(
        "SELECT edge_path->>0
           FROM graph.traverse(
             'graph_test_users_pgtest'::regclass, 'u2', 1,
             edge_types := ARRAY['tx_alpha'], hydrate := false)
          WHERE node_id = 'u1'",
    )
    .expect("filter transaction-local unseen relationship type failed")
    .unwrap_or_default();
    let path_edge_type = Spi::get_one::<String>(
        "SELECT edge_label
           FROM graph.shortest_path(
             'graph_test_users_pgtest'::regclass, 'u2',
             'graph_test_users_pgtest'::regclass, 'u1', 1,
             hydrate := false, edge_types := ARRAY['tx_alpha'])
          WHERE step = 1",
    )
    .expect("path transaction-local unseen relationship type failed")
    .unwrap_or_default();

    assert_eq!(returned_type, "tx_alpha");
    assert_eq!(gql_count, 1);
    assert_eq!(traverse_edge_type, "tx_alpha");
    assert_eq!(path_edge_type, "tx_alpha");
}

#[pg_test]
fn tx_unseen_dynamic_labels_follow_savepoint_abort_release_and_nesting() {
    build_p8_transaction_label_fixture();
    Spi::run(
        "SELECT * FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u2''}),
                  (v:graph_test_users_pgtest {id: ''u1''})
            CREATE (u)-[r:tx_outer {id: ''tx-outer''}]->(v) RETURN r');
         DO $outer$
         BEGIN
           BEGIN
             PERFORM * FROM graph.gql(
               'MATCH (u:graph_test_users_pgtest {id: ''u2''}),
                      (v:graph_test_users_pgtest {id: ''u1''})
                CREATE (u)-[r:tx_aborted {id: ''tx-aborted''}]->(v) RETURN r');
             RAISE EXCEPTION 'abort first savepoint';
           EXCEPTION WHEN others THEN NULL;
           END;
           PERFORM * FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest {id: ''u2''}),
                    (v:graph_test_users_pgtest {id: ''u1''})
              CREATE (u)-[r:tx_released {id: ''tx-released''}]->(v) RETURN r');
           BEGIN
             PERFORM * FROM graph.gql(
               'MATCH (u:graph_test_users_pgtest {id: ''u2''}),
                      (v:graph_test_users_pgtest {id: ''u1''})
                CREATE (u)-[r:tx_nested_aborted {id: ''tx-nested-aborted''}]->(v) RETURN r');
             RAISE EXCEPTION 'abort nested savepoint';
           EXCEPTION WHEN others THEN NULL;
           END;
         END
         $outer$",
    )
    .expect("exercise P8.3 savepoint label lifecycle failed");

    let source_labels = Spi::get_one::<String>(
        "SELECT string_agg(rel_type, ',' ORDER BY rel_type)
           FROM public.graph_test_friendships_pgtest
          WHERE id LIKE 'tx-%'",
    )
    .expect("read P8.3 savepoint source labels failed")
    .unwrap_or_default();
    let visible_labels = Spi::get_one::<String>(
        "SELECT string_agg(rel_type, ',' ORDER BY rel_type)
           FROM (
             SELECT row #>> '{r,_type}' AS rel_type
               FROM graph.gql(
                 'MATCH (u:graph_test_users_pgtest {id: ''u2''})-[r:tx_outer]->
                        (v:graph_test_users_pgtest {id: ''u1''}) RETURN r',
                 hydrate := false)
             UNION ALL
             SELECT row #>> '{r,_type}' AS rel_type
               FROM graph.gql(
                 'MATCH (u:graph_test_users_pgtest {id: ''u2''})-[r:tx_released]->
                        (v:graph_test_users_pgtest {id: ''u1''}) RETURN r',
                 hydrate := false)
           ) visible",
    )
    .expect("read P8.3 savepoint graph labels failed")
    .unwrap_or_default();

    assert_eq!(source_labels, "tx_outer,tx_released");
    assert_eq!(visible_labels, source_labels);
}

#[pg_test]
fn tx_unseen_dynamic_label_edge_limit_leaves_no_source_or_delta() {
    build_p8_transaction_label_fixture();
    Spi::run("SET graph.max_tx_delta_edges = 0").expect("tighten P8.3 edge limit failed");
    let statement = "SELECT * FROM graph.gql(
      'MATCH (u:graph_test_users_pgtest {id: ''u2''}),
             (v:graph_test_users_pgtest {id: ''u1''})
       CREATE (u)-[r:tx_limited {id: ''tx-limited''}]->(v) RETURN r')";
    assert_eq!(sqlstate_for_error(statement).as_deref(), Some("54000"));
    Spi::run("RESET graph.max_tx_delta_edges").expect("restore P8.3 edge limit failed");

    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*)::bigint FROM public.graph_test_friendships_pgtest
              WHERE id = 'tx-limited'"
        )
        .expect("read rejected P8.3 source row failed"),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<i32>("SELECT tx_delta_added_edges FROM graph.status()")
            .expect("read rejected P8.3 edge delta failed"),
        Some(0)
    );
}

#[cfg(feature = "development")]
#[pg_test]
fn tx_unseen_dynamic_label_missing_relationship_identity_is_pg023() {
    build_p8_transaction_label_fixture();
    create_error_sqlstate_helper();
    create_error_detail_helper();
    Spi::run(
        "CREATE ROLE graph_p8_tx_identity_reader;
         GRANT USAGE ON SCHEMA public, graph TO graph_p8_tx_identity_reader;
         GRANT SELECT ON public.graph_test_friendships_pgtest,
                         public.graph_test_users_pgtest TO graph_p8_tx_identity_reader;
         ALTER TABLE public.graph_test_friendships_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_p8_tx_identity_select
           ON public.graph_test_friendships_pgtest FOR SELECT
           TO graph_p8_tx_identity_reader USING (true);
         GRANT EXECUTE ON FUNCTION public.graph_test_sqlstate(text),
                                   public.graph_test_sql_error_detail(text)
           TO graph_p8_tx_identity_reader",
    )
    .expect("configure P8.3 relationship identity policy failed");
    create_p8_transaction_edge("tx-identity", "tx_identity");
    Spi::run("SET ROLE graph_p8_tx_identity_reader")
        .expect("assume P8.3 identity reader failed");
    let statement = "SELECT * FROM graph.traverse(
      'graph_test_users_pgtest'::regclass, 'u2', 1,
      edge_types := ARRAY['tx_identity'], hydrate := false)";
    Spi::run("SELECT graph._test_arm_missing_relationship_identity()")
        .expect("arm P8.3 missing relationship identity failed");
    let state = Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sqlstate({})",
        super::sql_literal(statement)
    ))
    .expect("capture P8.3 missing relationship identity SQLSTATE failed");
    Spi::run("SELECT graph._test_arm_missing_relationship_identity()")
        .expect("rearm P8.3 missing relationship identity failed");
    let detail = Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sql_error_detail({})",
        super::sql_literal(statement)
    ))
    .expect("capture P8.3 missing relationship identity detail failed")
    .unwrap_or_default();
    Spi::run("RESET ROLE").expect("restore P8.3 identity owner failed");

    assert_eq!(state.as_deref(), Some("55000"));
    assert!(detail.contains("PG023"));
}

#[pg_test]
fn tx_unseen_dynamic_label_rls_policy_is_enforced() {
    build_p8_transaction_label_fixture();
    Spi::run(
        "CREATE ROLE graph_p8_tx_reader;
         GRANT USAGE ON SCHEMA public, graph TO graph_p8_tx_reader;
         GRANT SELECT, INSERT ON public.graph_test_friendships_pgtest TO graph_p8_tx_reader;
         GRANT SELECT ON public.graph_test_users_pgtest TO graph_p8_tx_reader;
         ALTER TABLE public.graph_test_friendships_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY graph_p8_tx_select ON public.graph_test_friendships_pgtest
           FOR SELECT TO graph_p8_tx_reader
           USING (rel_type <> 'tx_hidden');
         CREATE POLICY graph_p8_tx_insert ON public.graph_test_friendships_pgtest
           FOR INSERT TO graph_p8_tx_reader WITH CHECK (true)",
    )
    .expect("configure P8.3 relationship RLS failed");
    create_p8_transaction_edge("tx-hidden", "tx_hidden");
    create_p8_transaction_edge("tx-visible", "tx_visible");
    Spi::run("SET ROLE graph_p8_tx_reader").expect("assume P8.3 restricted role failed");
    let hidden = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM graph.traverse(
             'graph_test_users_pgtest'::regclass, 'u2', 1,
             edge_types := ARRAY['tx_hidden'], hydrate := false)
          WHERE node_id = 'u1'",
    )
    .expect("query P8.3 restricted relationship failed")
    .unwrap_or_default();
    let visible = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM graph.traverse(
             'graph_test_users_pgtest'::regclass, 'u2', 1,
             edge_types := ARRAY['tx_visible'], hydrate := false)
          WHERE node_id = 'u1'",
    )
    .expect("query P8.3 visible restricted relationship failed")
    .unwrap_or_default();
    Spi::run("RESET ROLE").expect("restore P8.3 owner role failed");
    assert_eq!(hidden, 0);
    assert_eq!(visible, 1);
}

#[pg_test]
fn tx_unseen_dynamic_label_blocks_graph_selection_without_changing_session_state() {
    build_p8_transaction_label_fixture();
    create_p8_transaction_edge("tx-pinned", "tx_pinned");
    Spi::run("SELECT graph.create_graph('p8_other', namespace := 'app')")
        .expect("create P8.3 replacement target failed");

    let before = Spi::get_one::<String>("SELECT graph_name FROM graph.current_graph()")
        .expect("read P8.3 current graph failed")
        .unwrap_or_default();
    let state = sqlstate_for_error(
        "SELECT * FROM graph.set_current_graph('p8_other', namespace := 'app')",
    );
    let after = Spi::get_one::<String>("SELECT graph_name FROM graph.current_graph()")
        .expect("reread P8.3 current graph failed")
        .unwrap_or_default();
    let matched = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u2''})-[r:tx_pinned]->
                  (v:graph_test_users_pgtest {id: ''u1''}) RETURN r',
           hydrate := false)",
    )
    .expect("query P8.3 pinned graph after rejected selection failed")
    .unwrap_or_default();

    assert_eq!(state.as_deref(), Some("55000"));
    assert_eq!(after, before);
    assert_eq!(matched, 1);
}

#[pg_test]
fn tx_unseen_dynamic_label_durable_apply_conflict_does_not_alias_ids() {
    build_p8_transaction_label_fixture();
    Spi::run("SET graph.sync_mode = 'trigger'; SELECT graph.enable_sync()")
        .expect("enable P8.3 durable conflict fixture failed");
    let returned_type = create_p8_transaction_edge("tx-conflict", "tx_conflict");
    let apply_state = sqlstate_for_error("SELECT * FROM graph.apply_sync()");
    let matched = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM graph.gql(
           'MATCH (u:graph_test_users_pgtest {id: ''u2''})-[r:tx_conflict]->
                  (v:graph_test_users_pgtest {id: ''u1''}) RETURN r',
           hydrate := false)",
    )
    .expect("retry P8.3 provisional label after durable conflict failed")
    .unwrap_or_default();

    assert_eq!(returned_type, "tx_conflict");
    assert_eq!(apply_state.as_deref(), Some("55000"));
    assert_eq!(matched, 1, "durable conflict must not alias provisional IDs");
}
