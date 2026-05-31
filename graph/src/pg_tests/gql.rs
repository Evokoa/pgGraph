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
fn gql_with_projection_scope_aliases_and_shadows() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();
    create_error_capture_helper();

    let (source_name, shadowed_id, leak_denied) = Spi::connect(|client| {
        let source_name = client
            .select(
                "SELECT row #>> '{person_name}'
                 FROM graph.gql(
                     'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                      WITH u.name AS person_name, v AS u
                      RETURN person_name, u
                      ORDER BY person_name',
                     hydrate := true
                 )",
                None,
                &[],
            )
            .expect("with alias query failed")
            .first()
            .get::<String>(1)
            .expect("source name read failed")
            .unwrap_or_default();
        let shadowed_id = client
            .select(
                "SELECT row #>> '{u,_id,id}'
                 FROM graph.gql(
                     'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                      WITH u.name AS person_name, v AS u
                      RETURN person_name, u
                      ORDER BY person_name',
                     hydrate := true
                 )",
                None,
                &[],
            )
            .expect("with shadow query failed")
            .first()
            .get::<String>(1)
            .expect("shadowed id read failed")
            .unwrap_or_default();
        let leak_denied = client
            .select(
                "SELECT public.graph_test_sql_raises(
                     'SELECT * FROM graph.gql(
                        ''MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                          WITH v AS person
                          RETURN u''
                      )'
                 )",
                None,
                &[],
            )
            .expect("scope leak error capture failed")
            .first()
            .get::<bool>(1)
            .expect("scope leak bool read failed")
            .unwrap_or(false);
        Ok::<_, pgrx::spi::Error>((source_name, shadowed_id, leak_denied))
    })
    .expect("WITH scope query failed");

    assert_eq!(source_name, "Alice");
    assert_eq!(shadowed_id, "u2");
    assert!(leak_denied);
}

#[pg_test]
fn gql_optional_match_matches_left_outer_sql() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (gql_rows, gql_null_targets, sql_rows, sql_null_targets) = Spi::connect(|client| {
        let gql = client
            .select(
                "SELECT count(*)::bigint,
                        count(*) FILTER (WHERE row->'v' = 'null'::jsonb)::bigint
                 FROM graph.gql(
                     'OPTIONAL MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                      RETURN u, v',
                     hydrate := false
                 )",
                None,
                &[],
            )
            .expect("optional gql query failed")
            .first();
        let sql = client
            .select(
                "SELECT count(*)::bigint,
                        count(*) FILTER (WHERE v.id IS NULL)::bigint
                 FROM public.graph_test_users_pgtest u
                 LEFT JOIN public.graph_test_friendships_pgtest f ON f.user_id = u.id
                 LEFT JOIN public.graph_test_users_pgtest v ON v.id = f.friend_id",
                None,
                &[],
            )
            .expect("left outer sql query failed")
            .first();
        Ok::<_, pgrx::spi::Error>((
            gql.get::<i64>(1)
                .expect("gql row count read failed")
                .unwrap_or_default(),
            gql.get::<i64>(2)
                .expect("gql null count read failed")
                .unwrap_or_default(),
            sql.get::<i64>(1)
                .expect("sql row count read failed")
                .unwrap_or_default(),
            sql.get::<i64>(2)
                .expect("sql null count read failed")
                .unwrap_or_default(),
        ))
    })
    .expect("optional comparison failed");

    assert_eq!(gql_rows, sql_rows);
    assert_eq!(gql_null_targets, sql_null_targets);
    assert_eq!(gql_rows, 2);
    assert_eq!(gql_null_targets, 1);
}

#[pg_test]
fn gql_optional_match_filters_orders_and_paginates_null_rows() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (gql_rows, gql_null_names, gql_null_rels, sql_rows, sql_null_names, sql_null_rels) =
        Spi::connect(|client| {
            let gql = client
                .select(
                    "SELECT count(*)::bigint,
                            count(*) FILTER (WHERE row->'friend_name' = 'null'::jsonb)::bigint,
                            count(*) FILTER (WHERE row->'r' = 'null'::jsonb)::bigint
                     FROM graph.gql(
                         'OPTIONAL MATCH (u:graph_test_users_pgtest)-[r:friend]->(v:graph_test_users_pgtest)
                          WHERE v.name = ''Alice''
                          RETURN u.id AS user_id, v.name AS friend_name, r
                          ORDER BY friend_name
                          SKIP 1 LIMIT 1',
                         hydrate := true
                     )",
                    None,
                    &[],
                )
                .expect("optional filtered gql query failed")
                .first();
            let sql = client
                .select(
                    "WITH optional_rows AS (
                         SELECT u.id AS user_id, opt.friend_name, opt.rel_id
                         FROM public.graph_test_users_pgtest u
                         LEFT JOIN (
                             SELECT f.user_id, f.id AS rel_id, v.name AS friend_name
                             FROM public.graph_test_friendships_pgtest f
                             JOIN public.graph_test_users_pgtest v
                               ON v.id = f.friend_id
                              AND v.name = 'Alice'
                         ) opt ON opt.user_id = u.id
                         ORDER BY opt.friend_name NULLS FIRST
                         OFFSET 1 LIMIT 1
                     )
                     SELECT count(*)::bigint,
                            count(*) FILTER (WHERE friend_name IS NULL)::bigint,
                            count(*) FILTER (WHERE rel_id IS NULL)::bigint
                     FROM optional_rows",
                    None,
                    &[],
                )
                .expect("optional filtered sql query failed")
                .first();
            Ok::<_, pgrx::spi::Error>((
                gql.get::<i64>(1)
                    .expect("gql row count read failed")
                    .unwrap_or_default(),
                gql.get::<i64>(2)
                    .expect("gql null name count read failed")
                    .unwrap_or_default(),
                gql.get::<i64>(3)
                    .expect("gql null relationship count read failed")
                    .unwrap_or_default(),
                sql.get::<i64>(1)
                    .expect("sql row count read failed")
                    .unwrap_or_default(),
                sql.get::<i64>(2)
                    .expect("sql null name count read failed")
                    .unwrap_or_default(),
                sql.get::<i64>(3)
                    .expect("sql null relationship count read failed")
                    .unwrap_or_default(),
            ))
        })
        .expect("optional filtered comparison failed");

    assert_eq!(gql_rows, sql_rows);
    assert_eq!(gql_null_names, sql_null_names);
    assert_eq!(gql_null_rels, sql_null_rels);
    assert_eq!((gql_rows, gql_null_names, gql_null_rels), (1, 1, 1));
}

#[pg_test]
fn gql_optional_match_hydrates_source_with_null_target_and_relationship() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (source_name, target_is_null, relationship_is_null) = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT row #>> '{u,name}',
                        row->'v' = 'null'::jsonb,
                        row->'r' = 'null'::jsonb
                 FROM graph.gql(
                     'OPTIONAL MATCH (u:graph_test_users_pgtest)-[r:friend]->(v:graph_test_users_pgtest)
                      RETURN u, r, v
                      ORDER BY u.name',
                     hydrate := true
                 )
                 WHERE row #>> '{u,_id,id}' = 'u2'",
                None,
                &[],
            )
            .expect("optional hydrated gql query failed")
            .first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<String>(1)
                .expect("source name read failed")
                .unwrap_or_default(),
            row.get::<bool>(2)
                .expect("target null read failed")
                .unwrap_or(false),
            row.get::<bool>(3)
                .expect("relationship null read failed")
                .unwrap_or(false),
        ))
    })
    .expect("optional hydrated check failed");

    assert_eq!(source_name, "Bob");
    assert!(target_is_null);
    assert!(relationship_is_null);
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
    let optional_denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'OPTIONAL MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest) RETURN u, v'
             )"
        )
    ))
    .expect("optional acl error capture query failed")
    .unwrap_or(false);
    Spi::run("RESET ROLE").expect("reset role failed");

    assert!(denied);
    assert!(optional_denied);
}

#[pg_test]
fn gql_applies_session_tenant_scope_to_topology() {
    reset_and_create_fixtures();
    Spi::run("SET LOCAL graph.enforce_tenant_scope = on")
        .expect("enable tenant enforcement failed");
    Spi::run("SET LOCAL graph.tenant_setting = 'app.graph_gql_tenant'")
        .expect("set tenant GUC failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_gql_tenant_pgtest CASCADE")
        .expect("drop tenant table failed");
    Spi::run(
        "CREATE TABLE public.graph_gql_tenant_pgtest (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                name TEXT NOT NULL,
                parent_id TEXT REFERENCES public.graph_gql_tenant_pgtest(id)
            )",
    )
    .expect("create tenant table failed");
    Spi::run(
        "INSERT INTO public.graph_gql_tenant_pgtest (id, tenant_id, name, parent_id) VALUES
                ('a1', 'tenant-a', 'Root A', NULL),
                ('a2', 'tenant-a', 'Child A', 'a1'),
                ('b1', 'tenant-b', 'Root B', NULL),
                ('b2', 'tenant-b', 'Child B', 'b1')",
    )
    .expect("insert tenant rows failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_gql_tenant_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name'],
                tenant_column := 'tenant_id'
            )",
    )
    .expect("add tenant table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_gql_tenant_pgtest'::regclass,
                'parent_id',
                'graph_gql_tenant_pgtest'::regclass,
                'id',
                'parent',
                bidirectional := false
            )",
    )
    .expect("add tenant edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build tenant graph failed");

    Spi::run("SET LOCAL app.graph_gql_tenant = 'tenant-a'").expect("set tenant-a failed");
    let tenant_a_child = Spi::get_one::<String>(
        "SELECT row #>> '{child,_id,id}'
             FROM graph.gql(
                'MATCH (child:graph_gql_tenant_pgtest)-[:parent]->(parent:graph_gql_tenant_pgtest) RETURN child, parent'
             )",
    )
    .expect("tenant-a gql failed")
    .unwrap_or_default();

    Spi::run("SET LOCAL app.graph_gql_tenant = 'tenant-b'").expect("set tenant-b failed");
    let tenant_b_child = Spi::get_one::<String>(
        "SELECT row #>> '{child,_id,id}'
             FROM graph.gql(
                'MATCH (child:graph_gql_tenant_pgtest)-[:parent]->(parent:graph_gql_tenant_pgtest) RETURN child, parent'
             )",
    )
    .expect("tenant-b gql failed")
    .unwrap_or_default();

    Spi::run("RESET app.graph_gql_tenant").expect("reset tenant value failed");
    Spi::run("RESET graph.tenant_setting").expect("reset tenant setting failed");
    Spi::run("SET graph.enforce_tenant_scope = off").expect("disable tenant enforcement failed");

    assert_eq!(tenant_a_child, "a2");
    assert_eq!(tenant_b_child, "b2");
}

#[pg_test]
fn gql_reads_transaction_delta_edge_overlay() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    Spi::run(
        "SELECT graph._test_record_tx_edge(
            'graph_test_users_pgtest'::regclass,
            'u2',
            'graph_test_users_pgtest'::regclass,
            'u1',
            'friend',
            'insert'
        )",
    )
    .expect("record tx edge insert failed");

    let reverse_count = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
         FROM graph.gql(
            'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest) RETURN u, v',
            hydrate := false
         )
         WHERE row #>> '{u,_id,id}' = 'u2'
           AND row #>> '{v,_id,id}' = 'u1'",
    )
    .expect("gql tx overlay read failed")
    .unwrap_or_default();

    assert_eq!(reverse_count, 1);
}

#[pg_test]
fn gql_hides_transaction_delta_edge_delete() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    Spi::run(
        "SELECT graph._test_record_tx_edge(
            'graph_test_users_pgtest'::regclass,
            'u1',
            'graph_test_users_pgtest'::regclass,
            'u2',
            'friend',
            'delete'
        )",
    )
    .expect("record tx edge delete failed");

    let base_edge_count = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
         FROM graph.gql(
            'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest) RETURN u, v',
            hydrate := false
         )
         WHERE row #>> '{u,_id,id}' = 'u1'
           AND row #>> '{v,_id,id}' = 'u2'",
    )
    .expect("gql tx overlay delete read failed")
    .unwrap_or_default();

    assert_eq!(base_edge_count, 0);
}

#[pg_test]
fn gql_create_node_requires_mutable_overlay_projection() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();
    create_error_capture_helper();

    let (denied, source_count) = Spi::connect(|client| {
        let denied = client
            .select(
                &format!(
                    "SELECT public.graph_test_sql_raises({})",
                    super::sql_literal(
                        "SELECT * FROM graph.gql(
                'CREATE (u:graph_test_users_pgtest {id: ''u3'', name: ''Cara'', age: 29}) RETURN u'
             )"
                    )
                ),
                None,
                &[],
            )
            .expect("create error capture query failed")
            .first()
            .get::<bool>(1)
            .expect("create error capture read failed")
            .unwrap_or(false);
        let source_count = client
            .select(
                "SELECT count(*)::bigint FROM public.graph_test_users_pgtest WHERE id = 'u3'",
                None,
                &[],
            )
            .expect("source count query failed")
            .first()
            .get::<i64>(1)
            .expect("source count read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((denied, source_count))
    })
    .expect("readonly create verification failed");

    assert!(denied);
    assert_eq!(source_count, 0);
}

#[pg_test]
fn gql_create_node_inserts_mapped_row_and_records_delta() {
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

    let (created_id, created_name, source_count, tx_added_nodes, node_match_count) =
        Spi::connect(|client| {
        let created = client
            .select(
                "SELECT
                    row #>> '{u,_id,id}',
                    row #>> '{u,name}'
                 FROM graph.gql(
                    'CREATE (u:graph_test_users_pgtest {id: ''u3'', name: $name, age: 29}) RETURN u',
                    params := '{\"name\":\"Cara\"}'::jsonb
                 )",
                None,
                &[],
            )
            .expect("gql create failed")
            .first();
        let source_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM public.graph_test_users_pgtest
                 WHERE id = 'u3' AND name = 'Cara' AND age = 29",
                None,
                &[],
            )
            .expect("source row count failed")
            .first()
            .get::<i64>(1)
            .expect("source row count read failed")
            .unwrap_or_default();
        let tx_added_nodes = client
            .select("SELECT tx_delta_added_nodes FROM graph.status()", None, &[])
            .expect("status query failed")
            .first()
            .get::<i32>(1)
            .expect("tx added node count read failed")
            .unwrap_or_default();
        let node_match_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u3''}) RETURN u',
                    hydrate := false
                 )",
                None,
                &[],
            )
            .expect("node scan query failed")
            .first()
            .get::<i64>(1)
            .expect("node scan count read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((
            created
                .get::<String>(1)
                .expect("created id read failed")
                .unwrap_or_default(),
            created
                .get::<String>(2)
                .expect("created name read failed")
                .unwrap_or_default(),
            source_count,
            tx_added_nodes,
            node_match_count,
        ))
        })
        .expect("create verification failed");

    assert_eq!(created_id, "u3");
    assert_eq!(created_name, "Cara");
    assert_eq!(source_count, 1);
    assert_eq!(tx_added_nodes, 1);
    assert_eq!(node_match_count, 1);
}

#[pg_test]
fn gql_create_node_delta_limit_aborts_statement_before_source_insert() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("SET graph.max_tx_delta_nodes = 0").expect("tighten node delta limit failed");
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

    let sqlstate = sqlstate_for_error(
        "SELECT * FROM graph.gql(
            'CREATE (u:graph_test_users_pgtest {id: ''u3'', name: ''Cara'', age: 29}) RETURN u'
         )",
    );
    let source_count = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM public.graph_test_users_pgtest WHERE id = 'u3'",
    )
    .expect("source count failed")
    .unwrap_or_default();
    let tx_added_nodes = Spi::get_one::<i32>("SELECT tx_delta_added_nodes FROM graph.status()")
        .expect("status tx node count failed")
        .unwrap_or_default();

    Spi::run("RESET graph.max_tx_delta_nodes").expect("reset node delta limit failed");

    assert_eq!(sqlstate.as_deref(), Some("PG019"));
    assert_eq!(source_count, 0);
    assert_eq!(tx_added_nodes, 0);
}

#[pg_test]
fn transaction_local_delta_pressure_does_not_recommend_maintenance() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("SET graph.compaction_threshold = 1").expect("tighten compaction threshold failed");
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

    let (tx_added_nodes, compaction_recommended, maintenance_recommended) =
        Spi::connect(|client| {
            client
                .select(
                    "SELECT * FROM graph.gql(
                        'CREATE (u:graph_test_users_pgtest {id: ''u3'', name: ''Cara'', age: 29}) RETURN u'
                     )",
                    None,
                    &[],
                )
                .expect("gql create failed");
            let status = client
                .select(
                    "SELECT tx_delta_added_nodes, compaction_recommended FROM graph.status()",
                    None,
                    &[],
                )
                .expect("status failed")
                .first();
            let health = client
                .select(
                    "SELECT maintenance_recommended FROM graph.sync_health()",
                    None,
                    &[],
                )
                .expect("sync health failed")
                .first();
            Ok::<_, pgrx::spi::Error>((
                status
                    .get::<i32>(1)
                    .expect("tx count read failed")
                    .unwrap_or_default(),
                status
                    .get::<bool>(2)
                    .expect("compaction read failed")
                    .unwrap_or(false),
                health
                    .get::<bool>(1)
                    .expect("maintenance read failed")
                    .unwrap_or(false),
            ))
        })
        .expect("tx pressure verification failed");

    Spi::run("RESET graph.compaction_threshold").expect("reset compaction threshold failed");

    assert_eq!(tx_added_nodes, 1);
    assert!(!compaction_recommended);
    assert!(!maintenance_recommended);
}

#[pg_test]
fn gql_create_node_applies_session_tenant_scope() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("SET graph.enforce_tenant_scope = on").expect("enable tenant enforcement failed");
    Spi::run("SET graph.tenant_setting = 'app.graph_gql_create_tenant'")
        .expect("set tenant setting failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_gql_create_tenant_pgtest CASCADE")
        .expect("drop tenant create table failed");
    Spi::run(
        "CREATE TABLE public.graph_gql_create_tenant_pgtest (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                name TEXT NOT NULL
            )",
    )
    .expect("create tenant create table failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_gql_create_tenant_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name'],
                tenant_column := 'tenant_id'
            )",
    )
    .expect("add tenant create table failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable tenant graph failed");
    Spi::run("SET app.graph_gql_create_tenant = 'tenant-a'").expect("set tenant-a failed");

    let (created_id, created_tenant, source_tenant) = Spi::connect(|client| {
        let created = client
            .select(
                "SELECT
                    row #>> '{u,_id,id}',
                    row #>> '{u,tenant_id}'
                 FROM graph.gql(
                    'CREATE (u:graph_gql_create_tenant_pgtest {id: ''a3'', name: ''Leaf A''}) RETURN u'
                 )",
                None,
                &[],
            )
            .expect("tenant gql create failed")
            .first();
        let source_tenant = client
            .select(
                "SELECT tenant_id
                 FROM public.graph_gql_create_tenant_pgtest
                 WHERE id = 'a3'",
                None,
                &[],
            )
            .expect("source tenant query failed")
            .first()
            .get::<String>(1)
            .expect("source tenant read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((
            created
                .get::<String>(1)
                .expect("created id read failed")
                .unwrap_or_default(),
            created
                .get::<String>(2)
                .expect("created tenant read failed")
                .unwrap_or_default(),
            source_tenant,
        ))
    })
    .expect("tenant create verification failed");

    Spi::run("RESET app.graph_gql_create_tenant").expect("reset tenant-a failed");
    Spi::run("RESET graph.tenant_setting").expect("reset tenant setting failed");
    Spi::run("SET graph.enforce_tenant_scope = off").expect("disable tenant enforcement failed");

    assert_eq!(created_id, "a3");
    assert_eq!(created_tenant, "tenant-a");
    assert_eq!(source_tenant, "tenant-a");
}

#[pg_test]
fn gql_create_node_preserves_source_table_rls() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("SET graph.enforce_tenant_scope = on").expect("enable tenant enforcement failed");
    Spi::run("SET graph.tenant_setting = 'app.graph_gql_rls_tenant'")
        .expect("set tenant setting failed");
    Spi::run("DROP ROLE IF EXISTS graph_gql_create_rls").expect("drop rls role failed");
    Spi::run("CREATE ROLE graph_gql_create_rls").expect("create rls role failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_gql_create_rls_pgtest CASCADE")
        .expect("drop rls create table failed");
    Spi::run(
        "CREATE TABLE public.graph_gql_create_rls_pgtest (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                name TEXT NOT NULL
            )",
    )
    .expect("create rls create table failed");
    Spi::run("ALTER TABLE public.graph_gql_create_rls_pgtest ENABLE ROW LEVEL SECURITY")
        .expect("enable source rls failed");
    Spi::run(
        "CREATE POLICY graph_gql_create_rls_insert
             ON public.graph_gql_create_rls_pgtest
             FOR INSERT
             WITH CHECK (tenant_id = 'tenant-a')",
    )
    .expect("create insert rls policy failed");
    Spi::run(
        "CREATE POLICY graph_gql_create_rls_select
             ON public.graph_gql_create_rls_pgtest
             FOR SELECT
             USING (tenant_id = 'tenant-a')",
    )
    .expect("create select rls policy failed");
    Spi::run(
        "GRANT USAGE ON SCHEMA graph TO graph_gql_create_rls;
         GRANT INSERT, SELECT ON public.graph_gql_create_rls_pgtest TO graph_gql_create_rls",
    )
    .expect("grant rls role privileges failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_gql_create_rls_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name'],
                tenant_column := 'tenant_id'
            )",
    )
    .expect("add rls create table failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable rls graph failed");
    create_error_capture_helper();

    Spi::run("SET ROLE graph_gql_create_rls").expect("set rls role failed");
    Spi::run("SET app.graph_gql_rls_tenant = 'tenant-a'").expect("set tenant-a failed");
    let allowed_id = Spi::get_one::<String>(
        "SELECT row #>> '{u,_id,id}'
         FROM graph.gql(
            'CREATE (u:graph_gql_create_rls_pgtest {id: ''a4'', name: ''Allowed''}) RETURN u'
         )",
    )
    .expect("allowed rls create failed")
    .unwrap_or_default();
    Spi::run("SET app.graph_gql_rls_tenant = 'tenant-b'").expect("set tenant-b failed");
    let denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'CREATE (u:graph_gql_create_rls_pgtest {id: ''b4'', name: ''Denied''}) RETURN u'
             )"
        )
    ))
    .expect("denied rls create capture failed")
    .unwrap_or(false);
    Spi::run("RESET ROLE").expect("reset rls role failed");

    let (allowed_count, denied_count) = Spi::connect(|client| {
        let allowed_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM public.graph_gql_create_rls_pgtest
                 WHERE id = 'a4' AND tenant_id = 'tenant-a'",
                None,
                &[],
            )
            .expect("allowed source count failed")
            .first()
            .get::<i64>(1)
            .expect("allowed count read failed")
            .unwrap_or_default();
        let denied_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM public.graph_gql_create_rls_pgtest
                 WHERE id = 'b4'",
                None,
                &[],
            )
            .expect("denied source count failed")
            .first()
            .get::<i64>(1)
            .expect("denied count read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((allowed_count, denied_count))
    })
    .expect("rls verification failed");

    Spi::run("RESET app.graph_gql_rls_tenant").expect("reset rls tenant failed");
    Spi::run("RESET graph.tenant_setting").expect("reset tenant setting failed");
    Spi::run("SET graph.enforce_tenant_scope = off").expect("disable tenant enforcement failed");

    assert_eq!(allowed_id, "a4");
    assert!(denied);
    assert_eq!(allowed_count, 1);
    assert_eq!(denied_count, 0);
}

#[pg_test]
fn gql_set_property_updates_source_row_and_filter_index() {
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
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let (returned_age, source_age, filtered_neighbor_count) = Spi::connect(|client| {
        let updated = client
            .select(
                "SELECT row #>> '{age}'
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u2''}) SET u.age = $age RETURN u.age AS age',
                    params := '{\"age\":101}'::jsonb
                 )",
                None,
                &[],
            )
            .expect("gql set failed")
            .first()
            .get::<String>(1)
            .expect("updated age read failed")
            .unwrap_or_default();
        let source_age = client
            .select(
                "SELECT age FROM public.graph_test_users_pgtest WHERE id = 'u2'",
                None,
                &[],
            )
            .expect("source age query failed")
            .first()
            .get::<i32>(1)
            .expect("source age read failed")
            .unwrap_or_default();
        let filtered_neighbor_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM graph.traverse(
                    'graph_test_users_pgtest'::regclass,
                    'u1',
                    1,
                    filter := '{\"node\":{\"where\":{\"age\":{\"gt\":100}}}}'::jsonb,
                    hydrate := false
                 )
                 WHERE node_id = 'u2'",
                None,
                &[],
            )
            .expect("filtered traverse failed")
            .first()
            .get::<i64>(1)
            .expect("filtered count read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((updated, source_age, filtered_neighbor_count))
    })
    .expect("set verification failed");

    assert_eq!(returned_age, "101");
    assert_eq!(source_age, 101);
    assert_eq!(filtered_neighbor_count, 1);
}

#[pg_test]
fn gql_set_property_rejects_type_mismatch_and_readonly_projection() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    build_friendship_fixture_graph();
    create_error_capture_helper();

    let readonly_denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MATCH (u:graph_test_users_pgtest {id: ''u2''}) SET u.age = 42 RETURN u.age'
             )"
        )
    ))
    .expect("readonly set capture failed")
    .unwrap_or(false);

    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");
    let type_denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MATCH (u:graph_test_users_pgtest {id: ''u2''}) SET u.age = ''not numeric'' RETURN u.age'
             )"
        )
    ))
    .expect("type mismatch capture failed")
    .unwrap_or(false);
    let source_age = Spi::get_one::<i32>(
        "SELECT age FROM public.graph_test_users_pgtest WHERE id = 'u2'",
    )
    .expect("source age query failed")
    .unwrap_or_default();

    assert!(readonly_denied);
    assert!(type_denied);
    assert_eq!(source_age, 41);
}

#[pg_test]
fn gql_set_property_rejects_registered_tenant_column() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("SET LOCAL graph.enforce_tenant_scope = on")
        .expect("enable tenant enforcement failed");
    Spi::run("SET LOCAL graph.tenant_setting = 'app.graph_gql_set_tenant'")
        .expect("set tenant GUC failed");
    Spi::run("SET LOCAL app.graph_gql_set_tenant = 'tenant-a'").expect("set tenant failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_gql_set_tenant_pgtest CASCADE")
        .expect("drop tenant set table failed");
    Spi::run(
        "CREATE TABLE public.graph_gql_set_tenant_pgtest (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                name TEXT NOT NULL
            )",
    )
    .expect("create tenant set table failed");
    Spi::run(
        "INSERT INTO public.graph_gql_set_tenant_pgtest (id, tenant_id, name)
         VALUES ('a1', 'tenant-a', 'Ada')",
    )
    .expect("insert tenant set row failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_gql_set_tenant_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['tenant_id', 'name'],
                tenant_column := 'tenant_id'
            )",
    )
    .expect("add tenant set table failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable tenant graph failed");
    create_error_capture_helper();

    let denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MATCH (u:graph_gql_set_tenant_pgtest {id: ''a1''}) SET u.tenant_id = ''tenant-b'' RETURN u'
             )"
        )
    ))
    .expect("tenant column set capture failed")
    .unwrap_or(false);
    let tenant = Spi::get_one::<String>(
        "SELECT tenant_id FROM public.graph_gql_set_tenant_pgtest WHERE id = 'a1'",
    )
    .expect("tenant value query failed")
    .unwrap_or_default();

    Spi::run("RESET app.graph_gql_set_tenant").expect("reset tenant failed");
    Spi::run("RESET graph.tenant_setting").expect("reset tenant setting failed");
    Spi::run("SET graph.enforce_tenant_scope = off").expect("disable tenant enforcement failed");

    assert!(denied);
    assert_eq!(tenant, "tenant-a");
}

#[pg_test]
fn gql_delete_edge_removes_source_row_and_tombstones_neighbors() {
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
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend',
                bidirectional := true
            )",
    )
    .expect("add friendship edge failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let (returned_source, returned_target, edge_rows, forward_count, reverse_count, user_count) =
        Spi::connect(|client| {
            let deleted = client
                .select(
                    "SELECT row #>> '{source}', row #>> '{target}'
                     FROM graph.gql(
                        'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r:friend]->(v:graph_test_users_pgtest {id: ''u2''}) DELETE r RETURN u.id AS source, v.id AS target'
                     )",
                    None,
                    &[],
                )
                .expect("gql delete failed")
                .first();
            let edge_rows = client
                .select(
                    "SELECT count(*)::bigint FROM public.graph_test_friendships_pgtest",
                    None,
                    &[],
                )
                .expect("edge row count failed")
                .first()
                .get::<i64>(1)
                .expect("edge count read failed")
                .unwrap_or_default();
            let forward_count = client
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
                .expect("forward traverse failed")
                .first()
                .get::<i64>(1)
                .expect("forward count read failed")
                .unwrap_or_default();
            let reverse_count = client
                .select(
                    "SELECT count(*)::bigint
                     FROM graph.traverse(
                        'graph_test_users_pgtest'::regclass,
                        'u2',
                        1,
                        edge_types := ARRAY['friend'],
                        hydrate := false
                     )
                     WHERE node_id = 'u1'",
                    None,
                    &[],
                )
                .expect("reverse traverse failed")
                .first()
                .get::<i64>(1)
                .expect("reverse count read failed")
                .unwrap_or_default();
            let user_count = client
                .select(
                    "SELECT count(*)::bigint FROM public.graph_test_users_pgtest",
                    None,
                    &[],
                )
                .expect("user count failed")
                .first()
                .get::<i64>(1)
                .expect("user count read failed")
                .unwrap_or_default();
            Ok::<_, pgrx::spi::Error>((
                deleted
                    .get::<String>(1)
                    .expect("source read failed")
                    .unwrap_or_default(),
                deleted
                    .get::<String>(2)
                    .expect("target read failed")
                    .unwrap_or_default(),
                edge_rows,
                forward_count,
                reverse_count,
                user_count,
            ))
        })
        .expect("delete verification failed");

    assert_eq!(returned_source, "u1");
    assert_eq!(returned_target, "u2");
    assert_eq!(edge_rows, 0);
    assert_eq!(forward_count, 0);
    assert_eq!(reverse_count, 0);
    assert_eq!(user_count, 2);
}

#[pg_test]
fn gql_delete_edge_delta_limit_aborts_before_source_delete() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("SET graph.max_tx_delta_edges = 0").expect("tighten edge delta limit failed");
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
                bidirectional := true
            )",
    )
    .expect("add friendship edge failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let sqlstate = sqlstate_for_error(
        "SELECT * FROM graph.gql(
            'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r:friend]->(v:graph_test_users_pgtest {id: ''u2''}) DELETE r RETURN u'
         )",
    );
    let edge_rows = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM public.graph_test_friendships_pgtest",
    )
    .expect("edge row count failed")
    .unwrap_or_default();
    let tx_deleted_edges = Spi::get_one::<i32>("SELECT tx_delta_deleted_edges FROM graph.status()")
        .expect("status tx edge count failed")
        .unwrap_or_default();

    Spi::run("RESET graph.max_tx_delta_edges").expect("reset edge delta limit failed");

    assert_eq!(sqlstate.as_deref(), Some("PG019"));
    assert_eq!(edge_rows, 1);
    assert_eq!(tx_deleted_edges, 0);
}

#[pg_test]
fn gql_delete_edge_handles_bidirectional_reverse_match() {
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
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'friend',
                bidirectional := true
            )",
    )
    .expect("add friendship edge failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let (edge_rows, forward_count, reverse_count) = Spi::connect(|client| {
        client
            .select(
                "SELECT row
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u2''})-[r:friend]->(v:graph_test_users_pgtest {id: ''u1''}) DELETE r RETURN u, v'
                 )",
                None,
                &[],
            )
            .expect("reverse gql delete failed");
        let edge_rows = client
            .select(
                "SELECT count(*)::bigint FROM public.graph_test_friendships_pgtest",
                None,
                &[],
            )
            .expect("edge row count failed")
            .first()
            .get::<i64>(1)
            .expect("edge count read failed")
            .unwrap_or_default();
        let forward_count = client
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
            .expect("forward traverse failed")
            .first()
            .get::<i64>(1)
            .expect("forward count read failed")
            .unwrap_or_default();
        let reverse_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM graph.traverse(
                    'graph_test_users_pgtest'::regclass,
                    'u2',
                    1,
                    edge_types := ARRAY['friend'],
                    hydrate := false
                 )
                 WHERE node_id = 'u1'",
                None,
                &[],
            )
            .expect("reverse traverse failed")
            .first()
            .get::<i64>(1)
            .expect("reverse count read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((edge_rows, forward_count, reverse_count))
    })
    .expect("reverse delete verification failed");

    assert_eq!(edge_rows, 0);
    assert_eq!(forward_count, 0);
    assert_eq!(reverse_count, 0);
}

#[pg_test]
fn gql_delete_edge_rejects_ambiguous_bidirectional_self_edge_rows() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
         VALUES ('f2', 'u2', 'u1')",
    )
    .expect("insert opposite friendship failed");
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
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
                bidirectional := true
            )",
    )
    .expect("add friendship edge failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");
    create_error_capture_helper();

    let denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MATCH (u:graph_test_users_pgtest {id: ''u2''})-[r:friend]->(v:graph_test_users_pgtest {id: ''u1''}) DELETE r RETURN u, v'
             )"
        )
    ))
    .expect("ambiguous delete capture failed")
    .unwrap_or(false);
    let edge_count = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM public.graph_test_friendships_pgtest",
    )
    .expect("edge count query failed")
    .unwrap_or_default();

    assert!(denied);
    assert_eq!(edge_count, 2);
}

#[pg_test]
fn gql_delete_edge_requires_mutable_overlay_projection() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();
    create_error_capture_helper();

    let denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r:friend]->(v:graph_test_users_pgtest {id: ''u2''}) DELETE r RETURN u, v'
             )"
        )
    ))
    .expect("readonly delete capture failed")
    .unwrap_or(false);
    let edge_count = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM public.graph_test_friendships_pgtest",
    )
    .expect("edge count query failed")
    .unwrap_or_default();

    assert!(denied);
    assert_eq!(edge_count, 1);
}

#[pg_test]
fn gql_create_node_rejects_unregistered_label() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    build_friendship_fixture_graph();
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("rebuild mutable graph failed");
    create_error_capture_helper();

    let denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'CREATE (m:graph_missing_pgtest {id: ''m1'', name: ''Missing''}) RETURN m'
             )"
        )
    ))
    .expect("unregistered label error capture query failed")
    .unwrap_or(false);

    assert!(denied);
}
