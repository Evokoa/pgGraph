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
fn gql_explain_uses_registered_table_labels_after_catalog_read() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let explain = Spi::get_one::<String>(
        "SELECT graph.gql_explain(
            'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
             RETURN u.id AS source_id, v.id AS target_id'
         )",
    )
    .expect("gql explain failed")
    .unwrap_or_default();

    assert_eq!(
        explain,
        "Expand(source=u:graph_test_users_pgtest, rel=friend, hops=1..1, target=v:graph_test_users_pgtest, return=[source_id, target_id])"
    );
}

#[pg_test]
fn gql_binds_dynamic_edge_labels_from_registered_label_column() {
    reset_and_create_fixtures();
    Spi::run(
        "ALTER TABLE public.graph_test_friendships_pgtest
         ADD COLUMN rel_type text NOT NULL DEFAULT 'colleague'",
    )
    .expect("add dynamic relationship label column failed");
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
                from_table := 'graph_test_friendships_pgtest'::regclass,
                from_column := 'user_id',
                to_table := 'graph_test_users_pgtest'::regclass,
                to_column := 'friend_id',
                label := 'related_to',
                bidirectional := false,
                label_column := 'rel_type'
            )",
    )
    .expect("add dynamic friendship edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build dynamic relationship graph failed");

    let count = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
         FROM graph.gql(
            'MATCH (u:graph_test_users_pgtest)-[:colleague]->(v:graph_test_users_pgtest)
             RETURN u.id AS source, v.id AS target'
         )",
    )
    .expect("dynamic GQL relationship query failed")
    .unwrap_or_default();

    assert_eq!(count, 1);
}

#[pg_test]
fn gql_defaults_to_hydrated_nodes_and_projects_ids_explicitly() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (default_has_name, explicit_has_name, coordinate_has_name, scalar_id) =
        Spi::connect(|client| {
        let row = client
            .select(
                "SELECT
                    (SELECT row->'u' ? 'name'
                     FROM graph.gql(
                        'MATCH (u:graph_test_users_pgtest {id: ''u1''}) RETURN u'
                     )
                     LIMIT 1),
                    (SELECT row->'u' ? 'name'
                     FROM graph.gql(
                        'MATCH (u:graph_test_users_pgtest {id: ''u1''}) RETURN u',
                        hydrate := true
                     )
                     LIMIT 1),
                    (SELECT row->'u' ? 'name'
                     FROM graph.gql(
                        'MATCH (u:graph_test_users_pgtest {id: ''u1''}) RETURN u',
                        hydrate := false
                     )
                     LIMIT 1),
                    (SELECT row #>> '{id}'
                     FROM graph.gql(
                        'MATCH (u:graph_test_users_pgtest {id: ''u1''}) RETURN u.id AS id'
                     )
                     LIMIT 1)",
                None,
                &[],
            )
            .expect("gql hydrate behavior query failed")
            .first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<bool>(1)
                .expect("default hydrate flag read failed")
                .unwrap_or(false),
            row.get::<bool>(2)
                .expect("explicit hydrate flag read failed")
                .unwrap_or(false),
            row.get::<bool>(3)
                .expect("coordinate hydrate flag read failed")
                .unwrap_or(true),
            row.get::<String>(4)
                .expect("scalar id read failed")
                .unwrap_or_default(),
        ))
    })
    .expect("gql hydrate behavior verification failed");

    assert!(default_has_name);
    assert_eq!(explicit_has_name, default_has_name);
    assert!(!coordinate_has_name);
    assert_eq!(scalar_id, "u1");
}

#[pg_test]
fn gql_hydration_fails_closed_when_source_row_is_not_visible() {
    reset_and_create_fixtures();
    Spi::run("DROP ROLE IF EXISTS graph_gql_hydration_rls").expect("drop role failed");
    Spi::run("CREATE ROLE graph_gql_hydration_rls").expect("create role failed");
    Spi::run("DROP TABLE IF EXISTS public.graph_gql_hydration_rls_pgtest CASCADE")
        .expect("drop hydration rls table failed");
    Spi::run(
        "CREATE TABLE public.graph_gql_hydration_rls_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create hydration rls table failed");
    Spi::run(
        "INSERT INTO public.graph_gql_hydration_rls_pgtest (id, name)
         VALUES ('u1', 'Hidden')",
    )
    .expect("insert hydration rls row failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_gql_hydration_rls_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add hydration rls table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build hydration rls graph failed");
    Spi::run("ALTER TABLE public.graph_gql_hydration_rls_pgtest ENABLE ROW LEVEL SECURITY")
        .expect("enable hydration rls failed");
    Spi::run(
        "GRANT USAGE ON SCHEMA graph, public TO graph_gql_hydration_rls;
         GRANT SELECT ON public.graph_gql_hydration_rls_pgtest TO graph_gql_hydration_rls",
    )
    .expect("grant hydration rls role privileges failed");
    create_error_sqlstate_helper();

    Spi::run("SET ROLE graph_gql_hydration_rls").expect("set hydration rls role failed");
    let sqlstate = Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sqlstate({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MATCH (u:graph_gql_hydration_rls_pgtest) RETURN u',
                hydrate := true
             )"
        )
    ))
    .expect("hydration SQLSTATE capture failed");
    Spi::run("RESET ROLE").expect("reset hydration rls role failed");

    assert_eq!(sqlstate.as_deref(), Some("PG017"));
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
fn gql_aggregates_match_sql_grouping_and_numeric_results() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
         VALUES ('u3', 'Cara', 29), ('u4', 'Drew', 31)",
    )
    .expect("insert aggregate users failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
         VALUES ('f2', 'u3', 'u2'), ('f3', 'u4', 'u1')",
    )
    .expect("insert aggregate friendships failed");
    build_friendship_fixture_graph();

    let (matches_sql, group_count, bob_names) = Spi::connect(|client| {
        let row = client
            .select(
                "WITH gql_rows AS (
                     SELECT jsonb_agg(row ORDER BY row->>'friend_name') AS rows,
                            count(*)::bigint AS group_count
                     FROM graph.gql(
                         'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                          RETURN v.name AS friend_name,
                                 count(u) AS users,
                                 sum(u.age) AS total_age,
                                 avg(u.age) AS avg_age,
                                 min(u.age) AS youngest,
                                 max(u.age) AS oldest,
                                 collect(u.name) AS names
                          ORDER BY friend_name',
                         hydrate := true
                     )
                 ),
                 sql_rows AS (
                     SELECT jsonb_agg(
                                jsonb_build_object(
                                    'friend_name', friend_name,
                                    'users', users,
                                    'total_age', total_age,
                                    'avg_age', avg_age,
                                    'youngest', youngest,
                                    'oldest', oldest,
                                    'names', names
                                )
                                ORDER BY friend_name
                            ) AS rows
                     FROM (
                         SELECT v.name AS friend_name,
                                count(u.id)::bigint AS users,
                                sum(u.age)::float8 AS total_age,
                                avg(u.age)::float8 AS avg_age,
                                min(u.age)::int AS youngest,
                                max(u.age)::int AS oldest,
                                jsonb_agg(to_jsonb(u.name) ORDER BY u.id) AS names
                         FROM public.graph_test_users_pgtest u
                         JOIN public.graph_test_friendships_pgtest f ON f.user_id = u.id
                         JOIN public.graph_test_users_pgtest v ON v.id = f.friend_id
                         GROUP BY v.name
                     ) expected
                 )
                 SELECT gql_rows.rows = sql_rows.rows,
                        gql_rows.group_count,
                        (
                            SELECT jsonb_agg(row->'names')->0
                            FROM graph.gql(
                                'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                                 RETURN v.name AS friend_name, collect(u.name) AS names
                                 ORDER BY friend_name',
                                hydrate := true
                            )
                            WHERE row->>'friend_name' = 'Bob'
                        )
                 FROM gql_rows, sql_rows",
                None,
                &[],
            )
            .expect("aggregate comparison query failed")
            .first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<bool>(1)
                .expect("aggregate equality failed")
                .unwrap_or(false),
            row.get::<i64>(2)
                .expect("aggregate group count failed")
                .unwrap_or_default(),
            row.get::<pgrx::JsonB>(3)
                .expect("aggregate Bob names failed")
                .unwrap(),
        ))
    })
    .expect("aggregate comparison failed");

    assert!(matches_sql);
    assert_eq!(group_count, 2);
    assert_eq!(bob_names.0, serde_json::json!(["Alice", "Cara"]));
}

#[pg_test]
fn gql_jsonb_property_paths_return_lists_maps_and_distinguish_missing_from_null() {
    reset_and_create_fixtures();
    Spi::run(
        "ALTER TABLE public.graph_test_users_pgtest
         ADD COLUMN profile jsonb NOT NULL DEFAULT '{}'::jsonb",
    )
    .expect("add profile column failed");
    Spi::run(
        "UPDATE public.graph_test_users_pgtest
         SET profile = CASE id
           WHEN 'u1' THEN '{\"plan\":\"pro\",\"tags\":[\"founder\",7],\"flags\":{\"beta\":true},\"explicit_null\":null}'::jsonb
           ELSE '{\"plan\":\"free\",\"tags\":[\"reader\"],\"flags\":{\"beta\":false}}'::jsonb
         END",
    )
    .expect("update profile json failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY[
                    'name',
                    'age',
                    'profile',
                    'profile.plan',
                    'profile.tags',
                    'profile.flags',
                    'profile.missing',
                    'profile.explicit_null'
                ]
            )",
    )
    .expect("add users table with profile failed");
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
    Spi::run("SELECT * FROM graph.build()").expect("build graph failed");

    let (tags, flags, missing, explicit_null_matches, missing_null_matches) =
        Spi::connect(|client| {
            let row = client
                .select(
                    "SELECT row->'tags',
                            row->'flags',
                            row->'missing',
                            (
                              SELECT count(*)::bigint
                              FROM graph.gql(
                                'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                                 WHERE u.profile.explicit_null IS NULL
                                 RETURN u.name AS name',
                                hydrate := true
                              )
                            ),
                            (
                              SELECT count(*)::bigint
                              FROM graph.gql(
                                'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                                 WHERE u.profile.missing IS NULL
                                 RETURN u.name AS name',
                                hydrate := true
                              )
                            )
                     FROM graph.gql(
                       'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                        WHERE u.profile.plan = ''pro''
                        RETURN u.profile.tags AS tags, u.profile.flags AS flags, u.profile.missing AS missing',
                       hydrate := true
                     )",
                    None,
                    &[],
                )
                .expect("jsonb gql query failed")
                .first();
            Ok::<_, pgrx::spi::Error>((
                row.get::<pgrx::JsonB>(1)
                    .expect("tags read failed")
                    .map(|json| json.0)
                    .unwrap_or(serde_json::Value::Null),
                row.get::<pgrx::JsonB>(2)
                    .expect("flags read failed")
                    .map(|json| json.0)
                    .unwrap_or(serde_json::Value::Null),
                row.get::<pgrx::JsonB>(3)
                    .expect("missing read failed")
                    .map(|json| json.0)
                    .unwrap_or(serde_json::Value::Null),
                row.get::<i64>(4)
                    .expect("explicit null count read failed")
                    .unwrap_or_default(),
                row.get::<i64>(5)
                    .expect("missing null count read failed")
                    .unwrap_or_default(),
            ))
        })
        .expect("jsonb property query failed");

    assert_eq!(tags, serde_json::json!(["founder", 7]));
    assert_eq!(flags, serde_json::json!({"beta": true}));
    assert!(missing.is_null());
    assert_eq!(explicit_null_matches, 1);
    assert_eq!(missing_null_matches, 0);
}

#[pg_test]
fn gql_rejects_jsonb_property_paths_on_non_jsonb_columns() {
    reset_and_create_fixtures();

    let denied = sql_raises(
        "SELECT graph.add_table(
             'graph_test_users_pgtest'::regclass,
             id_column := 'id',
             columns := ARRAY['name.first']
         )",
    );

    assert!(denied);
}

#[pg_test]
fn gql_distinct_matches_sql_distinct_counts() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
         VALUES ('u3', 'Cara', 29), ('u4', 'Drew', 31)",
    )
    .expect("insert distinct users failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
         VALUES ('f2', 'u3', 'u2'), ('f3', 'u4', 'u2'), ('f4', 'u4', 'u1')",
    )
    .expect("insert distinct friendships failed");
    build_friendship_fixture_graph();

    let (
        return_distinct_matches,
        with_distinct_matches,
        aggregate_distinct_matches,
        collect_distinct_matches,
        optional_null_distinct_matches,
    ) =
        Spi::connect(|client| {
            let row = client
                .select(
                    "WITH sql_names AS (
                         SELECT jsonb_agg(name ORDER BY name) AS rows,
                                count(*)::bigint AS name_count
                         FROM (
                             SELECT DISTINCT v.name AS name
                             FROM public.graph_test_users_pgtest u
                             JOIN public.graph_test_friendships_pgtest f ON f.user_id = u.id
                             JOIN public.graph_test_users_pgtest v ON v.id = f.friend_id
                         ) distinct_names
                     ),
                     gql_names AS (
                         SELECT jsonb_agg(row->>'friend' ORDER BY row->>'friend') AS rows
                         FROM graph.gql(
                             'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                              RETURN DISTINCT v.name AS friend ORDER BY friend',
                             hydrate := true
                         )
                     ),
                     with_distinct AS (
                         SELECT (row->>'friends')::bigint AS friends
                         FROM graph.gql(
                             'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                              WITH DISTINCT v.name AS friend
                              RETURN count(*) AS friends',
                             hydrate := true
                         )
                     ),
                     aggregate_distinct AS (
                         SELECT row
                         FROM graph.gql(
                             'MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                              RETURN count(DISTINCT v.name) AS friends,
                                     collect(DISTINCT v.name) AS names',
                             hydrate := true
                         )
                     ),
                     gql_collect_names AS (
                         SELECT jsonb_agg(value ORDER BY value) AS rows
                         FROM aggregate_distinct,
                              jsonb_array_elements_text(row->'names') AS value
                     ),
                     optional_gql AS (
                         SELECT row
                         FROM graph.gql(
                             'OPTIONAL MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                              RETURN count(DISTINCT v.name) AS named_friends,
                                     collect(DISTINCT v.name) AS names',
                             hydrate := true
                         )
                     ),
                     optional_sql AS (
                         SELECT count(DISTINCT v.name)::bigint AS named_friends,
                                false AS has_null
                         FROM public.graph_test_users_pgtest u
                         LEFT JOIN public.graph_test_friendships_pgtest f ON f.user_id = u.id
                         LEFT JOIN public.graph_test_users_pgtest v ON v.id = f.friend_id
                     ),
                     optional_sql_names AS (
                         SELECT jsonb_agg(name ORDER BY name) AS rows
                         FROM (
                             SELECT DISTINCT v.name AS name
                             FROM public.graph_test_users_pgtest u
                             LEFT JOIN public.graph_test_friendships_pgtest f ON f.user_id = u.id
                             LEFT JOIN public.graph_test_users_pgtest v ON v.id = f.friend_id
                             WHERE v.name IS NOT NULL
                         ) distinct_values
                     ),
                     optional_gql_names AS (
                         SELECT jsonb_agg(value ORDER BY value) AS rows
                         FROM optional_gql,
                              jsonb_array_elements(row->'names') AS value
                         WHERE value <> 'null'::jsonb
                     ),
                     optional_gql_null AS (
                         SELECT EXISTS (
                             SELECT 1
                             FROM optional_gql,
                                  jsonb_array_elements(row->'names') AS value
                             WHERE value = 'null'::jsonb
                         ) AS has_null
                     )
                     SELECT gql_names.rows = sql_names.rows,
                            (SELECT friends FROM with_distinct) = sql_names.name_count,
                            (SELECT (row->>'friends')::bigint FROM aggregate_distinct)
                                = sql_names.name_count,
                            (SELECT rows FROM gql_collect_names) = sql_names.rows,
                            (
                                SELECT (row->>'named_friends')::bigint = optional_sql.named_friends
                                       AND optional_gql_names.rows = optional_sql_names.rows
                                       AND optional_gql_null.has_null = optional_sql.has_null
                                FROM optional_gql, optional_sql, optional_gql_names,
                                     optional_sql_names, optional_gql_null
                            )
                     FROM gql_names, sql_names",
                    None,
                    &[],
                )
                .expect("distinct comparison query failed")
                .first();
            Ok::<_, pgrx::spi::Error>((
                row.get::<bool>(1)
                    .expect("return distinct equality failed")
                    .unwrap_or(false),
                row.get::<bool>(2)
                    .expect("with distinct count failed")
                    .unwrap_or(false),
                row.get::<bool>(3)
                    .expect("aggregate distinct count failed")
                    .unwrap_or(false),
                row.get::<bool>(4)
                    .expect("collect distinct names failed")
                    .unwrap_or(false),
                row.get::<bool>(5)
                    .expect("optional null distinct failed")
                    .unwrap_or(false),
            ))
        })
        .expect("distinct comparison failed");

    assert!(return_distinct_matches);
    assert!(with_distinct_matches);
    assert!(aggregate_distinct_matches);
    assert!(collect_distinct_matches);
    assert!(optional_null_distinct_matches);
}

#[pg_test]
fn gql_path_values_and_functions_have_stable_shape() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age)
         VALUES ('u3', 'Cara', 29)",
    )
    .expect("insert path user failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
         VALUES ('f2', 'u2', 'u3')",
    )
    .expect("insert path friendship failed");
    build_friendship_fixture_graph();

    let (path_len, node_ids, relationship_count, raw_path_matches) = Spi::connect(|client| {
        let row = client
            .select(
                "WITH selected AS (
                     SELECT row
                     FROM graph.gql(
                         'MATCH (u:graph_test_users_pgtest)-[p:friend*2..2]->(v:graph_test_users_pgtest)
                          WHERE u.id = ''u1'' AND v.id = ''u3''
                          RETURN p,
                                 nodes(p) AS ns,
                                 relationships(p) AS rs,
                                 length(p) AS len',
                         hydrate := true
                     )
                     LIMIT 1
                 )
                 SELECT (row->>'len')::bigint,
                        (
                            SELECT jsonb_agg(node->>'id' ORDER BY ord)
                            FROM selected,
                                 jsonb_array_elements(row->'ns') WITH ORDINALITY AS n(node, ord)
                        ),
                        jsonb_array_length(row->'rs'),
                        row->'p'->'_path'->'nodes' = row->'ns'
                            AND row->'p'->'_path'->'relationships' = row->'rs'
                 FROM selected",
                None,
                &[],
            )
            .expect("path shape query failed")
            .first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<i64>(1)
                .expect("path length read failed")
                .unwrap_or_default(),
            row.get::<pgrx::JsonB>(2)
                .expect("path node ids read failed")
                .unwrap(),
            row.get::<i32>(3)
                .expect("relationship count read failed")
                .unwrap_or_default(),
            row.get::<bool>(4)
                .expect("raw path equality read failed")
                .unwrap_or(false),
        ))
    })
    .expect("path shape comparison failed");

    assert_eq!(path_len, 2);
    assert_eq!(node_ids.0, serde_json::json!(["u1", "u2", "u3"]));
    assert_eq!(relationship_count, 2);
    assert!(raw_path_matches);
}

#[pg_test]
fn gql_wildcard_path_values_and_functions_have_stable_shape() {
    reset_and_create_fixtures();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_companies_pgtest CASCADE")
        .expect("drop companies failed");
    Spi::run(
        "CREATE TABLE public.graph_test_companies_pgtest (
                id   TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create companies failed");
    Spi::run(
        "ALTER TABLE public.graph_test_users_pgtest
         ADD COLUMN company_id TEXT REFERENCES public.graph_test_companies_pgtest(id)",
    )
    .expect("add company reference failed");
    Spi::run(
        "INSERT INTO public.graph_test_companies_pgtest (id, name)
         VALUES ('c1', 'Acme')",
    )
    .expect("insert company failed");
    Spi::run("UPDATE public.graph_test_users_pgtest SET company_id = 'c1' WHERE id = 'u1'")
        .expect("set company failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'age']
            )",
    )
    .expect("add users table failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_companies_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add companies table failed");
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
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_users_pgtest'::regclass,
                'company_id',
                'graph_test_companies_pgtest'::regclass,
                'id',
                'works_at',
                bidirectional := false
            )",
    )
    .expect("add works_at edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build wildcard path graph failed");

    let (
        row_count,
        friend_rows,
        works_at_rows,
        named_works_at_rows,
        named_shape_matches,
        join_rows,
        join_shape_matches,
        join_predicate_matches,
        unhydrated_join_property_matches,
        wildcard_predicate_rows,
        wildcard_predicate_coordinate_only,
        wildcard_var_len_rows,
        wildcard_var_len_shape,
        shape_matches,
        coordinate_only_has_name,
    ) =
        Spi::connect(|client| {
            let row = client
                .select(
                    "WITH wildcard AS (
                         SELECT row
                         FROM graph.gql(
                             'MATCH p=()-[]->()
                              RETURN p,
                                     nodes(p) AS ns,
                                     relationships(p) AS rs,
                                     length(p) AS len',
                             hydrate := true
                         )
                     ),
                     coordinate_only AS (
                         SELECT row
                         FROM graph.gql('MATCH p=()-[]->() RETURN nodes(p) AS ns', hydrate := false)
                         LIMIT 1
                     ),
                     named_works_at AS (
                         SELECT row
                         FROM graph.gql(
                             'MATCH p=(s)-[r:works_at]->(e)
                              RETURN p, s, r, e',
                             hydrate := true
                         )
                     ),
                     joined_friend AS (
                         SELECT row
                         FROM graph.gql(
                             'MATCH (u:graph_test_users_pgtest)-[:friend]->(c:graph_test_users_pgtest),
                                    (v:graph_test_users_pgtest)-[:friend]->(c)
                              RETURN u.name AS source, v.name AS peer, c.name AS target',
                             hydrate := true
                         )
                     ),
                     joined_friend_predicate AS (
                         SELECT row
                         FROM graph.gql(
                             'MATCH (u:graph_test_users_pgtest)-[:friend]->(c:graph_test_users_pgtest),
                                    (v:graph_test_users_pgtest)-[:friend]->(c)
                              WHERE c.name = $target
                              RETURN u.name AS source, v.name AS peer, c.name AS target',
                             params := '{\"target\":\"Bob\"}'::jsonb,
                             hydrate := false
                         )
                     ),
                     joined_friend_unhydrated_property AS (
                         SELECT row
                         FROM graph.gql(
                             'MATCH (u:graph_test_users_pgtest)-[:friend]->(c:graph_test_users_pgtest),
                                    (v:graph_test_users_pgtest)-[:friend]->(c)
                              RETURN u.name AS source, c.name AS target',
                             hydrate := false
                         )
                     ),
                     wildcard_predicate AS (
                         SELECT row
                         FROM graph.gql(
                             'MATCH p=()-[:friend]->(e)
                              WHERE e.name = $target
                              RETURN e, length(p) AS len',
                             params := '{\"target\":\"Bob\"}'::jsonb,
                             hydrate := false
                         )
                     ),
                     wildcard_var_len AS (
                         SELECT row
                         FROM graph.gql(
                             'MATCH p=()-[*1..2]->()
                              RETURN p, length(p) AS len, relationships(p) AS rs',
                             hydrate := false
                         )
                     )
                     SELECT count(*)::bigint,
                            count(*) FILTER (WHERE row->'rs'->0->>'_type' = 'friend')::bigint,
                            count(*) FILTER (WHERE row->'rs'->0->>'_type' = 'works_at')::bigint,
                            (SELECT count(*)::bigint FROM named_works_at),
                            (SELECT bool_and(
                                row->'s' ? '_id'
                                AND row->'e' ? '_id'
                                AND row->'r'->>'_type' = 'works_at'
                                AND row->'p'->'_path'->'relationships'->0 = row->'r'
                             ) FROM named_works_at),
                            (SELECT count(*)::bigint FROM joined_friend),
                            (SELECT bool_and(
                                row->>'source' = row->>'peer'
                                AND row->>'target' = 'Bob'
                             ) FROM joined_friend),
                            (SELECT bool_and(
                                row->>'source' = 'Alice'
                                AND row->>'peer' = 'Alice'
                                AND row->>'target' = 'Bob'
                             ) FROM joined_friend_predicate),
                            (SELECT bool_and(
                                row->>'source' = 'Alice'
                                AND row->>'target' = 'Bob'
                             ) FROM joined_friend_unhydrated_property),
                            (SELECT count(*)::bigint FROM wildcard_predicate),
                            (SELECT bool_and(
                                (row->>'len')::bigint = 1
                                AND row->'e' ? '_id'
                                AND NOT (row->'e' ? 'name')
                             ) FROM wildcard_predicate),
                            (SELECT count(*)::bigint FROM wildcard_var_len),
                            (SELECT bool_and(
                                (row->>'len')::bigint BETWEEN 1 AND 2
                                AND jsonb_array_length(row->'rs') = (row->>'len')::integer
                                AND row->'p'->'_path'->'relationships' = row->'rs'
                             ) FROM wildcard_var_len),
                            bool_and(
                                (row->>'len')::bigint = 1
                                AND row->'p'->'_path'->'nodes' = row->'ns'
                                AND row->'p'->'_path'->'relationships' = row->'rs'
                                AND row->'rs'->0 ? '_start'
                                AND row->'rs'->0 ? '_end'
                            ),
                            (SELECT row->'ns'->0 ? 'name' FROM coordinate_only)
                     FROM wildcard",
                    None,
                    &[],
                )
                .expect("wildcard path shape query failed")
                .first();
            Ok::<_, pgrx::spi::Error>((
                row.get::<i64>(1)
                    .expect("wildcard row count read failed")
                    .unwrap_or_default(),
                row.get::<i64>(2)
                    .expect("friend row count read failed")
                    .unwrap_or_default(),
                row.get::<i64>(3)
                    .expect("works_at row count read failed")
                    .unwrap_or_default(),
                row.get::<i64>(4)
                    .expect("named works_at row count read failed")
                    .unwrap_or_default(),
                row.get::<bool>(5)
                    .expect("named works_at shape read failed")
                    .unwrap_or(false),
                row.get::<i64>(6)
                    .expect("join row count read failed")
                    .unwrap_or_default(),
                row.get::<bool>(7)
                    .expect("join shape read failed")
                    .unwrap_or(false),
                row.get::<bool>(8)
                    .expect("join predicate read failed")
                    .unwrap_or(false),
                row.get::<bool>(9)
                    .expect("unhydrated join property read failed")
                    .unwrap_or(false),
                row.get::<i64>(10)
                    .expect("wildcard predicate row count read failed")
                    .unwrap_or_default(),
                row.get::<bool>(11)
                    .expect("wildcard predicate shape read failed")
                    .unwrap_or(false),
                row.get::<i64>(12)
                    .expect("wildcard var-len row count read failed")
                    .unwrap_or_default(),
                row.get::<bool>(13)
                    .expect("wildcard var-len shape read failed")
                    .unwrap_or(false),
                row.get::<bool>(14)
                    .expect("shape equality read failed")
                    .unwrap_or(false),
                row.get::<bool>(15)
                    .expect("coordinate-only shape read failed")
                    .unwrap_or(true),
            ))
        })
        .expect("wildcard path shape comparison failed");

    assert_eq!(
        row_count, 2,
        "unexpected wildcard path row count: friend={friend_rows}, works_at={works_at_rows}"
    );
    assert_eq!(friend_rows, 1, "unexpected friend path count");
    assert_eq!(works_at_rows, 1, "unexpected works_at path count");
    assert_eq!(named_works_at_rows, works_at_rows);
    assert!(named_shape_matches);
    assert_eq!(join_rows, 1, "unexpected multi-pattern join count");
    assert!(join_shape_matches);
    assert!(join_predicate_matches);
    assert!(unhydrated_join_property_matches);
    assert_eq!(
        wildcard_predicate_rows, 1,
        "unexpected wildcard predicate count"
    );
    assert!(wildcard_predicate_coordinate_only);
    assert!(
        wildcard_var_len_rows >= row_count,
        "unexpected wildcard var-len count: {wildcard_var_len_rows}"
    );
    assert!(wildcard_var_len_shape);
    assert!(shape_matches);
    assert!(!coordinate_only_has_name);

    let join_relationship_shape = Spi::get_one::<bool>(
        "SELECT bool_and(
                    row->'r'->>'_type' = 'friend'
                    AND row->'r'->'_start'->>'table' = 'graph_test_users_pgtest'
                    AND row->'r'->'_start'->>'id' = 'u1'
                    AND row->'r'->'_end'->>'table' = 'graph_test_users_pgtest'
                    AND row->'r'->'_end'->>'id' = 'u2'
                )
         FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest)-[r:friend]->(c:graph_test_users_pgtest),
                    (v:graph_test_users_pgtest)-[:friend]->(c)
              RETURN r',
             hydrate := false
         )",
    )
    .expect("multi-pattern relationship projection query failed")
    .unwrap_or(false);

    assert!(join_relationship_shape);

    let join_path_shape = Spi::get_one::<bool>(
        "SELECT bool_and(
                    row->'path'->'_path'->'nodes'->0->'_id'->>'id' = 'u1'
                    AND row->'path'->'_path'->'nodes'->1->'_id'->>'id' = 'u2'
                    AND row->'path'->'_path'->'relationships'->0->>'_type' = 'friend'
                    AND row->'path'->'_path'->'relationships'->0->'_start'->>'id' = 'u1'
                    AND row->'path'->'_path'->'relationships'->0->'_end'->>'id' = 'u2'
                )
         FROM graph.gql(
             'MATCH p=(u:graph_test_users_pgtest)-[:friend]->(c:graph_test_users_pgtest),
                    (v:graph_test_users_pgtest)-[:friend]->(c)
              RETURN p AS path',
             hydrate := false
         )",
    )
    .expect("multi-pattern path projection query failed")
    .unwrap_or(false);

    assert!(join_path_shape);

    let join_path_function_shape = Spi::get_one::<bool>(
        "SELECT bool_and(
                    row->'ns'->0->'_id'->>'id' = 'u1'
                    AND row->'ns'->1->'_id'->>'id' = 'u2'
                    AND row->'rs'->0->>'_type' = 'friend'
                    AND (row->>'len')::integer = 1
                )
         FROM graph.gql(
             'MATCH p=(u:graph_test_users_pgtest)-[:friend]->(c:graph_test_users_pgtest),
                    (v:graph_test_users_pgtest)-[:friend]->(c)
              RETURN nodes(p) AS ns, relationships(p) AS rs, length(p) AS len',
             hydrate := false
         )",
    )
    .expect("multi-pattern path-function projection query failed")
    .unwrap_or(false);

    assert!(join_path_function_shape);

    let join_aggregate_shape = Spi::get_one::<bool>(
        "SELECT bool_and(
                    row->>'target' = 'Bob'
                    AND (row->>'rows')::integer = 1
                    AND (row->>'total_age')::float8 = 37
                    AND (row->>'rels')::integer = 1
                    AND row #>> '{rel_values,0,_type}' = 'friend'
                    AND (row->>'paths')::integer = 1
                    AND row #>> '{path_values,0,_path,relationships,0,_type}' = 'friend'
                )
         FROM graph.gql(
             'MATCH p=(u:graph_test_users_pgtest)-[r:friend]->(c:graph_test_users_pgtest),
                    (v:graph_test_users_pgtest)-[:friend]->(c)
              RETURN c.name AS target, count(*) AS rows, sum(v.age) AS total_age,
                     count(DISTINCT r) AS rels, collect(r) AS rel_values,
                     count(DISTINCT p) AS paths, collect(p) AS path_values',
             hydrate := true
         )",
    )
    .expect("multi-pattern aggregate projection query failed")
    .unwrap_or(false);

    assert!(join_aggregate_shape);

    let join_with_shape = Spi::get_one::<bool>(
        "SELECT bool_and(
                    row->>'targets' = '1'
                )
         FROM graph.gql(
             'MATCH (u:graph_test_users_pgtest)-[:friend]->(c:graph_test_users_pgtest),
                    (v:graph_test_users_pgtest)-[:friend]->(c)
              WITH DISTINCT c.name AS target
              RETURN count(*) AS targets',
             hydrate := false
         )",
    )
    .expect("multi-pattern WITH projection query failed")
    .unwrap_or(false);

    assert!(join_with_shape);

    let join_relationship_path_with_shape = Spi::get_one::<bool>(
        "SELECT bool_and(
                    row #>> '{rel,_type}' = 'friend'
                    AND row #>> '{path,_path,relationships,0,_type}' = 'friend'
                )
         FROM graph.gql(
             'MATCH p=(u:graph_test_users_pgtest)-[r:friend]->(c:graph_test_users_pgtest),
                    (v:graph_test_users_pgtest)-[:friend]->(c)
              WITH r AS rel, p AS path, v.name AS peer
              RETURN rel, path
              ORDER BY peer
              LIMIT 1',
             hydrate := false
         )",
    )
    .expect("multi-pattern relationship/path WITH projection query failed")
    .unwrap_or(false);

    assert!(join_relationship_path_with_shape);

    let join_path_function_with_shape = Spi::get_one::<bool>(
        "SELECT bool_and(
                    (row->>'hops')::integer = 1
                    AND row->'ns'->0->'_id'->>'id' = 'u1'
                    AND row->'rs'->0->>'_type' = 'friend'
                )
         FROM graph.gql(
             'MATCH p=(u:graph_test_users_pgtest)-[:friend]->(c:graph_test_users_pgtest),
                    (v:graph_test_users_pgtest)-[:friend]->(c)
              WITH length(p) AS hops, nodes(p) AS ns, relationships(p) AS rs
              RETURN hops, ns, rs
              ORDER BY hops',
             hydrate := false
         )",
    )
    .expect("multi-pattern path-function WITH projection query failed")
    .unwrap_or(false);

    assert!(join_path_function_with_shape);

    let join_aggregate_with_shape = Spi::get_one::<bool>(
        "SELECT bool_and(
                    row->>'target' = 'Bob'
                    AND (row->>'rows')::integer = 1
                    AND (row->>'rels')::integer = 1
                    AND row #>> '{paths,0,_path,relationships,0,_type}' = 'friend'
                )
         FROM graph.gql(
             'MATCH p=(u:graph_test_users_pgtest)-[r:friend]->(c:graph_test_users_pgtest),
                    (v:graph_test_users_pgtest)-[:friend]->(c)
              WITH c.name AS target, count(*) AS rows, count(DISTINCT r) AS rels,
                   collect(p) AS paths
              RETURN target, rows, rels, paths
              ORDER BY rows DESC, target',
             hydrate := false
         )",
    )
    .expect("multi-pattern aggregate WITH projection query failed")
    .unwrap_or(false);

    assert!(join_aggregate_with_shape);
}

#[pg_test]
fn gql_aggregates_return_empty_group_and_optional_null_counts() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let (empty_count, empty_sum_is_null, empty_names, optional_rows, optional_matches) =
        Spi::connect(|client| {
            let empty = client
                .select(
                    "SELECT (row #>> '{rows}')::bigint,
                            row->'total_age' = 'null'::jsonb,
                            row->'names'
                     FROM graph.gql(
                         'MATCH (u:graph_test_users_pgtest)
                          WHERE u.name = ''Missing''
                          RETURN count(*) AS rows, sum(u.age) AS total_age, collect(u.name) AS names',
                         hydrate := true
                     )",
                    None,
                    &[],
                )
                .expect("empty aggregate gql query failed")
                .first();
            let optional = client
                .select(
                    "SELECT (row #>> '{source_rows}')::bigint,
                            (row #>> '{matched_targets}')::bigint
                     FROM graph.gql(
                         'OPTIONAL MATCH (u:graph_test_users_pgtest)-[:friend]->(v:graph_test_users_pgtest)
                          WHERE v.name = ''Alice''
                          RETURN count(*) AS source_rows, count(v) AS matched_targets',
                         hydrate := true
                     )",
                    None,
                    &[],
                )
                .expect("optional aggregate gql query failed")
                .first();
            Ok::<_, pgrx::spi::Error>((
                empty.get::<i64>(1).expect("empty count failed").unwrap_or_default(),
                empty.get::<bool>(2).expect("empty sum failed").unwrap_or(false),
                empty.get::<pgrx::JsonB>(3).expect("empty names failed").unwrap(),
                optional
                    .get::<i64>(1)
                    .expect("optional rows failed")
                    .unwrap_or_default(),
                optional
                    .get::<i64>(2)
                    .expect("optional matches failed")
                    .unwrap_or_default(),
            ))
        })
        .expect("aggregate null comparison failed");

    assert_eq!(empty_count, 0);
    assert!(empty_sum_is_null);
    assert_eq!(empty_names.0, serde_json::json!([]));
    assert_eq!(optional_rows, 2);
    assert_eq!(optional_matches, 0);
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
fn gql_merge_node_inserts_then_updates_mapped_row() {
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

    let (inserted_name, inserted_age, matched_name, matched_age, source_count, tx_added_nodes) =
        Spi::connect(|client| {
            let inserted = client
                .select(
                    "SELECT row #>> '{name}', (row #>> '{age}')::int
                     FROM graph.gql(
                        'MERGE (u:graph_test_users_pgtest {id: ''u3'', name: $name})
                         ON CREATE SET u.age = 31
                         ON MATCH SET u.name = ''Updated''
                         RETURN u.name AS name, u.age AS age',
                        params := '{\"name\":\"Cara\"}'::jsonb
                     )",
                    None,
                    &[],
                )
                .expect("gql merge insert failed")
                .first();
            let matched = client
                .select(
                    "SELECT row #>> '{name}', (row #>> '{age}')::int
                     FROM graph.gql(
                        'MERGE (u:graph_test_users_pgtest {id: ''u3'', name: ''Ignored''})
                         ON CREATE SET u.age = 99
                         ON MATCH SET u.name = $name
                         RETURN u.name AS name, u.age AS age',
                        params := '{\"name\":\"Caroline\"}'::jsonb
                     )",
                    None,
                    &[],
                )
                .expect("gql merge match failed")
                .first();
            let source_count = client
                .select(
                    "SELECT count(*)::bigint
                     FROM public.graph_test_users_pgtest
                     WHERE id = 'u3' AND name = 'Caroline' AND age = 31",
                    None,
                    &[],
                )
                .expect("source merge count failed")
                .first()
                .get::<i64>(1)
                .expect("source merge count read failed")
                .unwrap_or_default();
            let tx_added_nodes = client
                .select("SELECT tx_delta_added_nodes FROM graph.status()", None, &[])
                .expect("status query failed")
                .first()
                .get::<i32>(1)
                .expect("tx added node count read failed")
                .unwrap_or_default();
            Ok::<_, pgrx::spi::Error>((
                inserted
                    .get::<String>(1)
                    .expect("inserted merge name read failed")
                    .unwrap_or_default(),
                inserted
                    .get::<i32>(2)
                    .expect("inserted merge age read failed")
                    .unwrap_or_default(),
                matched
                    .get::<String>(1)
                    .expect("matched merge name read failed")
                    .unwrap_or_default(),
                matched
                    .get::<i32>(2)
                    .expect("matched merge age read failed")
                    .unwrap_or_default(),
                source_count,
                tx_added_nodes,
            ))
        })
        .expect("merge verification failed");

    assert_eq!(inserted_name, "Cara");
    assert_eq!(inserted_age, 31);
    assert_eq!(matched_name, "Caroline");
    assert_eq!(matched_age, 31);
    assert_eq!(source_count, 1);
    assert_eq!(tx_added_nodes, 1);
}

#[pg_test]
fn gql_merge_node_without_on_match_does_not_require_update_acl() {
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
    Spi::run("DROP ROLE IF EXISTS graph_gql_merge_insert_only").expect("drop role failed");
    Spi::run("CREATE ROLE graph_gql_merge_insert_only").expect("create role failed");
    Spi::run(
        "GRANT USAGE ON SCHEMA graph, public TO graph_gql_merge_insert_only;
         GRANT SELECT, INSERT ON public.graph_test_users_pgtest TO graph_gql_merge_insert_only;
         REVOKE UPDATE ON public.graph_test_users_pgtest FROM graph_gql_merge_insert_only, PUBLIC",
    )
    .expect("grant merge ACL role privileges failed");
    create_error_sqlstate_helper();

    Spi::run("SET ROLE graph_gql_merge_insert_only").expect("set merge ACL role failed");
    let inserted_id = Spi::get_one::<String>(
        "SELECT row #>> '{u,_id,id}'
         FROM graph.gql(
            'MERGE (u:graph_test_users_pgtest {id: ''u3'', name: ''Cara''})
             ON CREATE SET u.age = 29
             RETURN u'
         )",
    )
    .expect("insert-only merge insert failed")
    .unwrap_or_default();
    let matched_id = Spi::get_one::<String>(
        "SELECT row #>> '{u,_id,id}'
         FROM graph.gql(
            'MERGE (u:graph_test_users_pgtest {id: ''u3'', name: ''Ignored''})
             RETURN u'
         )",
    )
    .expect("insert-only merge match failed")
    .unwrap_or_default();
    let denied_sqlstate = Spi::get_one::<String>(&format!(
        "SELECT public.graph_test_sqlstate({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MERGE (u:graph_test_users_pgtest {id: ''u3'', name: ''Ignored''})
                 ON MATCH SET u.name = ''Denied''
                 RETURN u'
             )"
        )
    ))
    .expect("merge ACL SQLSTATE capture failed");
    Spi::run("RESET ROLE").expect("reset role failed");

    assert_eq!(inserted_id, "u3");
    assert_eq!(matched_id, "u3");
    assert_eq!(denied_sqlstate.as_deref(), Some("PG002"));
}

#[pg_test]
fn gql_merge_node_requires_identity_and_mutable_overlay() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();

    let readonly_sqlstate = sqlstate_for_error(
        "SELECT * FROM graph.gql(
            'MERGE (u:graph_test_users_pgtest {id: ''u3'', name: ''Cara''}) RETURN u'
         )",
    );

    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");
    let missing_identity_sqlstate = sqlstate_for_error(
        "SELECT * FROM graph.gql(
            'MERGE (u:graph_test_users_pgtest {name: ''No Id''}) RETURN u'
         )",
    );

    assert_eq!(readonly_sqlstate.as_deref(), Some("PG018"));
    assert_eq!(missing_identity_sqlstate.as_deref(), Some("PG017"));
}

#[pg_test]
fn gql_merge_node_does_not_evaluate_create_branch_on_match() {
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

    let (matched_name, matched_age, source_count) = Spi::connect(|client| {
        let matched = client
            .select(
                "SELECT row #>> '{name}', (row #>> '{age}')::int
                 FROM graph.gql(
                    'MERGE (u:graph_test_users_pgtest {id: ''u1''})
                     ON CREATE SET u.age = $create_age
                     ON MATCH SET u.name = $match_name
                     RETURN u.name AS name, u.age AS age',
                    params := '{\"match_name\":\"Alice Matched\"}'::jsonb
                 )",
                None,
                &[],
            )
            .expect("gql merge match with missing create param failed")
            .first();
        let source_count = client
            .select(
                "SELECT count(*)::bigint
                 FROM public.graph_test_users_pgtest
                 WHERE id = 'u1' AND name = 'Alice Matched'",
                None,
                &[],
            )
            .expect("source merge branch count failed")
            .first()
            .get::<i64>(1)
            .expect("source merge branch count read failed")
            .unwrap_or_default();
        Ok::<_, pgrx::spi::Error>((
            matched
                .get::<String>(1)
                .expect("matched merge branch name read failed")
                .unwrap_or_default(),
            matched
                .get::<i32>(2)
                .expect("matched merge branch age read failed")
                .unwrap_or_default(),
            source_count,
        ))
    })
    .expect("merge branch verification failed");

    assert_eq!(matched_name, "Alice Matched");
    assert_eq!(matched_age, 37);
    assert_eq!(source_count, 1);
}

#[pg_test]
fn gql_merge_node_delta_limit_aborts_statement_before_source_insert() {
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
            'MERGE (u:graph_test_users_pgtest {id: ''u3'', name: ''Cara'', age: 29}) RETURN u'
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
fn gql_remove_typed_property_sets_source_column_null_idempotently() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("ALTER TABLE public.graph_test_users_pgtest ADD COLUMN status TEXT")
        .expect("add status column failed");
    Spi::run("UPDATE public.graph_test_users_pgtest SET status = 'active'")
        .expect("seed status failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'age', 'status']
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
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let (first_is_null, second_is_null, source_is_null) = Spi::connect(|client| {
        let first = client
            .select(
                "SELECT row->'status' = 'null'::jsonb
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u2''}) REMOVE u.status RETURN u.status AS status'
                 )",
                None,
                &[],
            )
            .expect("first remove failed")
            .first()
            .get::<bool>(1)
            .expect("first null read failed")
            .unwrap_or(false);
        let second = client
            .select(
                "SELECT row->'status' = 'null'::jsonb
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u2''}) REMOVE u.status RETURN u.status AS status'
                 )",
                None,
                &[],
            )
            .expect("second remove failed")
            .first()
            .get::<bool>(1)
            .expect("second null read failed")
            .unwrap_or(false);
        let source = client
            .select(
                "SELECT status IS NULL FROM public.graph_test_users_pgtest WHERE id = 'u2'",
                None,
                &[],
            )
            .expect("source status query failed")
            .first()
            .get::<bool>(1)
            .expect("source status read failed")
            .unwrap_or(false);
        Ok::<_, pgrx::spi::Error>((first, second, source))
    })
    .expect("remove typed verification failed");

    assert!(first_is_null);
    assert!(second_is_null);
    assert!(source_is_null);
}

#[pg_test]
fn gql_remove_jsonb_property_path_drops_key_idempotently() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("ALTER TABLE public.graph_test_users_pgtest ADD COLUMN profile jsonb")
    .expect("add profile column failed");
    Spi::run(
        "UPDATE public.graph_test_users_pgtest
         SET profile = CASE id
           WHEN 'u1' THEN '{\"plan\":\"pro\",\"flags\":{\"beta\":true}}'::jsonb
           ELSE NULL
         END",
    )
    .expect("seed profile failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name', 'age', 'profile', 'profile.plan', 'profile.flags']
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
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let (first_plan_null, second_plan_null, profile, source_has_plan, null_root_preserved) =
        Spi::connect(|client| {
        let first = client
            .select(
                "SELECT row->'plan' = 'null'::jsonb
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u1''}) REMOVE u.profile.plan RETURN u.profile.plan AS plan'
                 )",
                None,
                &[],
            )
            .expect("first jsonb remove failed")
            .first()
            .get::<bool>(1)
            .expect("first plan null read failed")
            .unwrap_or(false);
        let second_row = client
            .select(
                "SELECT row->'plan' = 'null'::jsonb, row->'profile'
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u1''}) REMOVE u.profile.plan RETURN u.profile.plan AS plan, u.profile AS profile'
                 )",
                None,
                &[],
            )
            .expect("second jsonb remove failed")
            .first();
        let second = second_row
            .get::<bool>(1)
            .expect("second plan null read failed")
            .unwrap_or(false);
        let profile = second_row
            .get::<pgrx::JsonB>(2)
            .expect("profile read failed")
            .unwrap();
        let source_has_plan = client
            .select(
                "SELECT profile ? 'plan' FROM public.graph_test_users_pgtest WHERE id = 'u1'",
                None,
                &[],
            )
            .expect("source profile query failed")
            .first()
            .get::<bool>(1)
            .expect("source profile read failed")
            .unwrap_or(true);
        let null_root_preserved = client
            .select(
                "SELECT row->'profile' = 'null'::jsonb
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u2''}) REMOVE u.profile.plan RETURN u.profile AS profile'
                 )",
                None,
                &[],
            )
            .expect("null-root jsonb remove failed")
            .first()
            .get::<bool>(1)
            .expect("null-root profile read failed")
            .unwrap_or(false);
        Ok::<_, pgrx::spi::Error>((first, second, profile, source_has_plan, null_root_preserved))
    })
    .expect("remove jsonb verification failed");

    assert!(first_plan_null);
    assert!(second_plan_null);
    assert_eq!(profile.0, serde_json::json!({"flags": {"beta": true}}));
    assert!(!source_has_plan);
    assert!(null_root_preserved);
}

#[pg_test]
fn gql_remove_property_requires_mutable_overlay_projection() {
    reset_and_create_fixtures();
    build_friendship_fixture_graph();
    create_error_capture_helper();

    let denied = Spi::get_one::<bool>(&format!(
        "SELECT public.graph_test_sql_raises({})",
        super::sql_literal(
            "SELECT * FROM graph.gql(
                'MATCH (u:graph_test_users_pgtest {id: ''u2''}) REMOVE u.age RETURN u.age'
             )"
        )
    ))
    .expect("readonly remove capture failed")
    .unwrap_or(false);
    let source_age = Spi::get_one::<i32>(
        "SELECT age FROM public.graph_test_users_pgtest WHERE id = 'u2'",
    )
    .expect("source age query failed")
    .unwrap_or_default();

    assert!(denied);
    assert_eq!(source_age, 41);
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
fn gql_wildcard_delete_edge_resolves_unique_mapped_row() {
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

    let (returned_source, returned_target, edge_rows) = Spi::connect(|client| {
        let deleted = client
            .select(
                "SELECT row #>> '{source}', row #>> '{target}'
                 FROM graph.gql(
                    'MATCH (u)-[r]->(v)
                     WHERE u.id = ''u1'' AND v.id = ''u2''
                     DELETE r
                     RETURN u.id AS source, v.id AS target'
                 )",
                None,
                &[],
            )
            .expect("wildcard gql delete failed")
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
        ))
    })
    .expect("wildcard delete verification failed");

    assert_eq!(returned_source, "u1");
    assert_eq!(returned_target, "u2");
    assert_eq!(edge_rows, 0);
}

#[pg_test]
fn gql_delete_dynamic_edge_label_deletes_only_matching_label_row() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run(
        "ALTER TABLE public.graph_test_friendships_pgtest
         ADD COLUMN rel_type text NOT NULL DEFAULT 'colleague'",
    )
    .expect("add dynamic relationship label column failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id, rel_type)
         VALUES ('f2', 'u1', 'u2', 'mentor')",
    )
    .expect("insert second dynamic relationship failed");
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
                from_table := 'graph_test_friendships_pgtest'::regclass,
                from_column := 'user_id',
                to_table := 'graph_test_users_pgtest'::regclass,
                to_column := 'friend_id',
                label := 'related_to',
                bidirectional := false,
                label_column := 'rel_type'
            )",
    )
    .expect("add dynamic friendship edge failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable dynamic relationship graph failed");

    let (returned_source, returned_target, colleague_rows, mentor_rows) =
        Spi::connect(|client| {
            let deleted = client
                .select(
                    "SELECT row #>> '{source}', row #>> '{target}'
                     FROM graph.gql(
                        'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r:colleague]->(v:graph_test_users_pgtest {id: ''u2''}) DELETE r RETURN u.id AS source, v.id AS target'
                     )",
                    None,
                    &[],
                )
                .expect("dynamic label delete failed")
                .first();
            let colleague_rows = client
                .select(
                    "SELECT count(*)::bigint
                     FROM public.graph_test_friendships_pgtest
                     WHERE rel_type = 'colleague'",
                    None,
                    &[],
                )
                .expect("colleague row count failed")
                .first()
                .get::<i64>(1)
                .expect("colleague count read failed")
                .unwrap_or_default();
            let mentor_rows = client
                .select(
                    "SELECT count(*)::bigint
                     FROM public.graph_test_friendships_pgtest
                     WHERE rel_type = 'mentor'",
                    None,
                    &[],
                )
                .expect("mentor row count failed")
                .first()
                .get::<i64>(1)
                .expect("mentor count read failed")
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
                colleague_rows,
                mentor_rows,
            ))
        })
        .expect("dynamic label delete verification failed");

    assert_eq!(returned_source, "u1");
    assert_eq!(returned_target, "u2");
    assert_eq!(colleague_rows, 0);
    assert_eq!(mentor_rows, 1);
}

#[pg_test]
fn gql_delete_dynamic_edge_label_uses_registered_label_for_blank_values() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run("ALTER TABLE public.graph_test_friendships_pgtest ADD COLUMN rel_type text")
        .expect("add nullable dynamic relationship label column failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id, rel_type)
         VALUES ('f2', 'u2', 'u1', '')",
    )
    .expect("insert blank dynamic relationship failed");
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
                from_table := 'graph_test_friendships_pgtest'::regclass,
                from_column := 'user_id',
                to_table := 'graph_test_users_pgtest'::regclass,
                to_column := 'friend_id',
                label := 'related_to',
                bidirectional := false,
                label_column := 'rel_type'
            )",
    )
    .expect("add dynamic friendship edge failed");
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable dynamic relationship graph failed");

    let (returned_source, returned_target, remaining_edges) = Spi::connect(|client| {
        let deleted = client
            .select(
                "SELECT row #>> '{source}', row #>> '{target}'
                 FROM graph.gql(
                    'MATCH (u:graph_test_users_pgtest {id: ''u1''})-[r:related_to]->(v:graph_test_users_pgtest {id: ''u2''}) DELETE r RETURN u.id AS source, v.id AS target'
                 )",
                None,
                &[],
            )
            .expect("dynamic fallback label delete failed")
            .first();
        let remaining_edges = client
            .select(
                "SELECT count(*)::bigint FROM public.graph_test_friendships_pgtest",
                None,
                &[],
            )
            .expect("remaining edge count failed")
            .first()
            .get::<i64>(1)
            .expect("remaining edge count read failed")
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
            remaining_edges,
        ))
    })
    .expect("dynamic fallback label delete verification failed");

    assert_eq!(returned_source, "u1");
    assert_eq!(returned_target, "u2");
    assert_eq!(remaining_edges, 1);
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
fn gql_detach_delete_removes_incident_edges_before_node() {
    reset_and_create_fixtures();
    Spi::run("SET graph.mutable_enabled = on").expect("enable mutable projection failed");
    Spi::run(
        "INSERT INTO public.graph_test_friendships_pgtest (id, user_id, friend_id)
         VALUES ('f2', 'u2', 'u1')",
    )
    .expect("insert reverse friendship failed");
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
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let (returned_name, edge_rows, deleted_user_rows, remaining_user_rows, gql_deleted_visible) =
        Spi::connect(|client| {
            let deleted = client
                .select(
                    "SELECT row #>> '{name}'
                     FROM graph.gql(
                        'MATCH (u:graph_test_users_pgtest {id: ''u1''}) DETACH DELETE u RETURN u.name AS name'
                     )",
                    None,
                    &[],
                )
                .expect("gql detach delete failed")
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
            let deleted_user_rows = client
                .select(
                    "SELECT count(*)::bigint FROM public.graph_test_users_pgtest WHERE id = 'u1'",
                    None,
                    &[],
                )
                .expect("deleted user count failed")
                .first()
                .get::<i64>(1)
                .expect("deleted user read failed")
                .unwrap_or_default();
            let remaining_user_rows = client
                .select(
                    "SELECT count(*)::bigint FROM public.graph_test_users_pgtest WHERE id = 'u2'",
                    None,
                    &[],
                )
                .expect("remaining user count failed")
                .first()
                .get::<i64>(1)
                .expect("remaining user read failed")
                .unwrap_or_default();
            let gql_deleted_visible = client
                .select(
                    "SELECT count(*)::bigint
                     FROM graph.gql(
                        'MATCH (u:graph_test_users_pgtest {id: ''u1''}) RETURN u',
                        hydrate := false
                     )",
                    None,
                    &[],
                )
                .expect("gql deleted visibility check failed")
                .first()
                .get::<i64>(1)
                .expect("gql deleted visibility read failed")
                .unwrap_or_default();
            Ok::<_, pgrx::spi::Error>((
                deleted
                    .get::<String>(1)
                    .expect("deleted name read failed")
                    .unwrap_or_default(),
                edge_rows,
                deleted_user_rows,
                remaining_user_rows,
                gql_deleted_visible,
            ))
        })
        .expect("detach delete verification failed");
    let traverse_deleted_sqlstate = sqlstate_for_error(
        "SELECT *
         FROM graph.traverse(
            'graph_test_users_pgtest'::regclass,
            'u1',
            1,
            edge_types := ARRAY['friend'],
            hydrate := false
         )",
    )
    .unwrap_or_default();

    assert_eq!(returned_name, "Alice");
    assert_eq!(edge_rows, 0);
    assert_eq!(deleted_user_rows, 0);
    assert_eq!(remaining_user_rows, 1);
    assert_eq!(gql_deleted_visible, 0);
    assert_eq!(traverse_deleted_sqlstate, "PG010");
}

#[pg_test]
fn gql_detach_delete_delta_limit_rolls_back_source_rows() {
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
    Spi::run("SELECT * FROM graph.build(mode := 'mutable_overlay')")
        .expect("build mutable graph failed");

    let sqlstate = sqlstate_for_error(
        "SELECT * FROM graph.gql(
            'MATCH (u:graph_test_users_pgtest {id: ''u1''}) DETACH DELETE u RETURN u'
         )",
    );
    let user_rows = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM public.graph_test_users_pgtest WHERE id = 'u1'",
    )
    .expect("user row count failed")
    .unwrap_or_default();
    let edge_rows = Spi::get_one::<i64>(
        "SELECT count(*)::bigint FROM public.graph_test_friendships_pgtest",
    )
    .expect("edge row count failed")
    .unwrap_or_default();

    Spi::run("RESET graph.max_tx_delta_nodes").expect("reset node delta limit failed");

    assert_eq!(sqlstate.as_deref(), Some("PG019"));
    assert_eq!(user_rows, 1);
    assert_eq!(edge_rows, 1);
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
