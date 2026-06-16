#[pg_test]
fn cypher_matches_gql_for_supported_read_subset() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (gql_name, cypher_name, gql_explain, cypher_explain) = Spi::connect(|client| {
        let gql_row = client
            .select(
                "SELECT row #>> '{name}'
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                     RETURN v.name AS name'
                 )",
                None,
                &[],
            )
            .expect("gql read failed")
            .first();
        let cypher_row = client
            .select(
                "SELECT row #>> '{name}'
                 FROM graph.cypher(
                    'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                     RETURN v.name AS name'
                 )",
                None,
                &[],
            )
            .expect("cypher read failed")
            .first();
        let gql_explain = client
            .select(
                "SELECT graph.gql_explain(
                    'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                     RETURN v.name AS name'
                 )",
                None,
                &[],
            )
            .expect("gql explain failed")
            .first();
        let cypher_explain = client
            .select(
                "SELECT graph.cypher_explain(
                    'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                     RETURN v.name AS name'
                 )",
                None,
                &[],
            )
            .expect("cypher explain failed")
            .first();

        Ok::<_, pgrx::spi::Error>((
            gql_row
                .get::<String>(1)
                .expect("gql name read failed")
                .unwrap_or_default(),
            cypher_row
                .get::<String>(1)
                .expect("cypher name read failed")
                .unwrap_or_default(),
            gql_explain
                .get::<String>(1)
                .expect("gql explain read failed")
                .unwrap_or_default(),
            cypher_explain
                .get::<String>(1)
                .expect("cypher explain read failed")
                .unwrap_or_default(),
        ))
    })
    .expect("cypher comparison failed");

    assert_eq!(gql_name, "Bob");
    assert_eq!(cypher_name, gql_name);
    assert_eq!(cypher_explain, gql_explain);
}

#[pg_test]
fn cypher_write_uses_shared_mutable_overlay_execution() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'age']
            )",
    )
    .expect("add users table failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let (returned_name, source_count, tx_added_nodes) = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT row #>> '{name}'
                 FROM graph.cypher(
                    'CREATE (u:graph_test_users_pgtest {id: ''u3'', name: $name, age: 29})
                     RETURN u.name AS name',
                    params := '{\"name\":\"Cara\"}'::jsonb
                 )",
                None,
                &[],
            )
            .expect("cypher create failed")
            .first();
        let source_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM public.graph_test_users_pgtest
                 WHERE id = 'u3' AND name = 'Cara'",
                None,
                &[],
            )
            .expect("source count failed")
            .first()
            .get::<i64>(1)
            .expect("source count read failed")
            .unwrap_or_default();
        let tx_added_nodes = client
            .select("SELECT tx_delta_added_nodes FROM graph.status()", None, &[])
            .expect("status failed")
            .first()
            .get::<i32>(1)
            .expect("tx added nodes read failed")
            .unwrap_or_default();

        Ok::<_, pgrx::spi::Error>((
            row.get::<String>(1)
                .expect("returned name read failed")
                .unwrap_or_default(),
            source_count,
            tx_added_nodes,
        ))
    })
    .expect("cypher write verification failed");

    assert_eq!(returned_name, "Cara");
    assert_eq!(source_count, 1);
    assert_eq!(tx_added_nodes, 1);
}

#[pg_test]
fn cypher_rejects_unsupported_cypher_only_syntax() {
    reset_and_create_fixtures();

    let sqlstate_unwind = sqlstate_for_error("SELECT * FROM graph.cypher('UNWIND [1, 2] AS n RETURN n')");
    assert_eq!(sqlstate_unwind.as_deref(), Some("PG014"));

    let sqlstate_call = sqlstate_for_error("SELECT * FROM graph.cypher('CALL db.labels()')");
    assert_eq!(sqlstate_call.as_deref(), Some("PG014"));

    let sqlstate_ddl = sqlstate_for_error("SELECT * FROM graph.cypher('CREATE INDEX ON :Person(name)')");
    assert_eq!(sqlstate_ddl.as_deref(), Some("PG014"));
}

#[pg_test]
fn cypher_compatibility_matrix_is_separate_and_honest() {
    reset_and_create_fixtures();

    let (supported_rows, full_opencypher_rows) = Spi::connect(|client| {
        let supported_rows = client
            .select(
                "SELECT count(*)::bigint
                 FROM graph.cypher_compatibility()
                 WHERE status = 'supported'",
                None,
                &[],
            )
            .expect("supported compatibility query failed")
            .first()
            .get::<i64>(1)
            .expect("supported compatibility count read failed")
            .unwrap_or_default();
        let full_opencypher_rows = client
            .select(
                "SELECT count(*)::bigint
                 FROM graph.cypher_compatibility()
                 WHERE feature = 'Full openCypher compatibility' AND status = 'not claimed'",
                None,
                &[],
            )
            .expect("full openCypher compatibility query failed")
            .first()
            .get::<i64>(1)
            .expect("full openCypher compatibility count read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((supported_rows, full_opencypher_rows))
    })
    .expect("compatibility verification failed");

    assert!(supported_rows > 0);
    assert_eq!(full_opencypher_rows, 1);
}

#[pg_test]
fn pgq_public_endpoint_is_permanently_rejected() {
    reset_and_create_fixtures();
    
    let sqlstate = sqlstate_for_error("SELECT graph.pgq('MATCH (a)')");
    assert_eq!(sqlstate.as_deref(), Some("PG018"));
}
