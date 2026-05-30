#[pg_test]
fn gql_single_directed_match_matches_traverse_fixture() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (gql_count, source_id, target_id, source_table, traverse_count) = Spi::connect(|client| {
        let gql = client
            .select(
                "SELECT
                     count(*)::bigint,
                     min(row #>> '{u,_id,id}'),
                     min(row #>> '{v,_id,id}'),
                     min(row #>> '{u,_id,table}')
                 FROM graph.gql(
                     'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest) RETURN u, v'
                 )",
                None,
                &[],
            )
            .expect("gql query failed");
        let gql_row = gql.first();
        let traverse_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM graph.traverse(
                     'graph_test_users_pgtest'::regclass,
                     'u1',
                     1,
                     edge_types := ARRAY['friend'],
                     hydrate := false
                 )
                 WHERE node_id = 'u2'",
                None,
                &[],
            )
            .expect("traverse query failed")
            .first()
            .get::<i64>(1)
            .expect("traverse count read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((
            gql_row
                .get::<i64>(1)
                .expect("gql count read failed")
                .unwrap_or_default(),
            gql_row
                .get::<String>(2)
                .expect("source id read failed")
                .unwrap_or_default(),
            gql_row
                .get::<String>(3)
                .expect("target id read failed")
                .unwrap_or_default(),
            gql_row
                .get::<String>(4)
                .expect("source table read failed")
                .unwrap_or_default(),
            traverse_count,
        ))
    })
    .expect("comparison query failed");

    assert_eq!(gql_count, traverse_count);
    assert_eq!(source_id, "u1");
    assert_eq!(target_id, "u2");
    assert_eq!(source_table, "graph_test_users_pgtest");
}

#[pg_test]
fn gql_denies_without_select_on_bound_tables() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();
    Spi::run("DROP ROLE IF EXISTS graph_gql_no_select").expect("drop role failed");
    Spi::run("CREATE ROLE graph_gql_no_select").expect("create role failed");
    Spi::run("GRANT USAGE ON SCHEMA graph TO graph_gql_no_select").expect("grant schema failed");
    Spi::run("REVOKE SELECT ON public.graph_test_users_pgtest FROM PUBLIC")
        .expect("revoke public select failed");
    create_error_capture_helper();
    Spi::run("SET ROLE graph_gql_no_select").expect("set role failed");
    let denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest) RETURN u, v'
             )"
        )
    ))
    .expect("acl error capture query failed")
    .unwrap_or(false);
    Spi::run("RESET ROLE").expect("reset role failed");

    assert!(denied);
}
