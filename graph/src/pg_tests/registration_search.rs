use crate::catalog::table_oid_from_name;

#[pg_test]
fn registered_tables_and_edges_reflect_public_registration_apis() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_composite_pgtest'::regclass,
                id_columns := ARRAY['org_id', 'user_id'],
                columns := ARRAY['label']
            )",
    )
    .expect("add_table id_columns overload failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add_table compatibility overload failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_friendships_pgtest'::regclass,
                'user_id',
                'graph_test_users_pgtest'::regclass,
                'friend_id',
                'knows'
            )",
    )
    .expect("add_edge failed");

    let composite_keys = Spi::get_one::<Vec<String>>(
        "SELECT id_columns
             FROM graph.registered_tables()
             WHERE table_name = 'graph_test_composite_pgtest'",
    )
    .expect("registered_tables query failed")
    .expect("composite table registration missing");
    let edge_label = Spi::get_one::<String>(
        "SELECT label
             FROM graph.registered_edges()
             WHERE from_table = 'graph_test_friendships_pgtest'",
    )
    .expect("registered_edges query failed")
    .expect("edge registration missing");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    let composite_seed =
        Spi::get_one::<String>("SELECT jsonb_build_array('org1'::text, 'emp1'::text)::text")
            .expect("composite seed expression failed")
            .expect("composite seed expression returned NULL");
    let composite_traverse_count = Spi::get_one::<i64>(&format!(
        "SELECT count(*) FROM graph.traverse('graph_test_composite_pgtest'::regclass, {}, 1)",
        super::sql_literal(&composite_seed)
    ))
    .expect("composite traversal failed")
    .unwrap_or(0);
    let friendship_traverse_count = Spi::get_one::<i64>(
            "SELECT count(*)
             FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 1, edge_types := ARRAY['knows'], hydrate := false)
             WHERE node_id = 'u2'",
        )
        .expect("friendship traversal failed")
        .unwrap_or(0);

    assert_eq!(
        composite_keys,
        vec!["org_id".to_string(), "user_id".to_string()]
    );
    assert_eq!(edge_label, "knows");
    assert_eq!(composite_traverse_count, 1);
    assert_eq!(friendship_traverse_count, 1);
}

#[pg_test]
fn sql_search_defaults_to_contains_and_supports_exact_mode() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add_table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let contains_count = Spi::get_one::<i64>("SELECT count(*) FROM graph.search('name', 'ali')")
        .expect("contains search failed")
        .unwrap_or(0);
    let exact_count =
        Spi::get_one::<i64>("SELECT count(*) FROM graph.search('name', 'ali', mode := 'exact')")
            .expect("exact search failed")
            .unwrap_or(0);
    let hydrated_name = Spi::get_one::<String>(
        "SELECT node->>'name'
             FROM graph.search('name', 'ali', max_rows := 1)
             WHERE verified AND match_type = 'contains'",
    )
    .expect("hydrated search failed")
    .expect("hydrated search row missing");
    let opt_out_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search('name', 'ali', hydrate := false)
             WHERE verified AND node IS NULL",
    )
    .expect("coordinate search failed")
    .unwrap_or(0);
    let case_sensitive_exact = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search('name', 'alice', mode := 'exact', case_sensitive := true)",
    )
    .expect("case-sensitive search failed")
    .unwrap_or(0);
    let paged_second = Spi::get_one::<String>(
        "SELECT node_id
             FROM graph.search('name', 'b', max_rows := 1, row_offset := 0)",
    )
    .expect("paged search failed");

    assert_eq!(contains_count, 1);
    assert_eq!(exact_count, 0);
    assert_eq!(hydrated_name, "Alice");
    assert_eq!(opt_out_count, 1);
    assert_eq!(case_sensitive_exact, 0);
    assert_eq!(paged_second.as_deref(), Some("u2"));
}

#[pg_test]
fn token_search_limit_uses_exact_token_sql_candidates() {
    reset_and_create_fixtures();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_token_limit_pgtest CASCADE")
        .expect("drop token limit table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_token_limit_pgtest (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
    )
    .expect("create token limit table failed");
    Spi::run(
        "INSERT INTO public.graph_test_token_limit_pgtest (id, name)
             VALUES ('a-cart', 'cart'), ('b-art', 'art')",
    )
    .expect("insert token limit rows failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_token_limit_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add token limit table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let matched_id = Spi::get_one::<String>(
        "SELECT node_id
             FROM graph.search(
                'name',
                'art',
                'graph_test_token_limit_pgtest'::regclass,
                mode := 'token',
                max_rows := 1,
                hydrate := false
             )",
    )
    .expect("token limited search failed")
    .expect("token limited search returned no row");

    assert_eq!(matched_id, "b-art");
}

#[pg_test]
fn search_apis_read_committed_source_rows_without_graph_rebuild() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add_table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");
    Spi::run(
        "UPDATE public.graph_test_users_pgtest
             SET name = CASE id
                 WHEN 'u1' THEN 'Alicia Source'
                 WHEN 'u2' THEN 'Carol Token Proof'
             END
             WHERE id IN ('u1', 'u2')",
    )
    .expect("update post-build source rows failed");

    let exact_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Alicia Source',
                'graph_test_users_pgtest'::regclass,
                mode := 'exact',
                hydrate := false
             )",
    )
    .expect("exact source search failed")
    .unwrap_or(0);
    let prefix_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Alic',
                'graph_test_users_pgtest'::regclass,
                mode := 'prefix',
                hydrate := false
             )",
    )
    .expect("prefix source search failed")
    .unwrap_or(0);
    let contains_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Source',
                'graph_test_users_pgtest'::regclass,
                mode := 'contains',
                hydrate := false
             )",
    )
    .expect("contains source search failed")
    .unwrap_or(0);
    let token_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search(
                'name',
                'Carol Proof',
                'graph_test_users_pgtest'::regclass,
                mode := 'token',
                hydrate := false
             )",
    )
    .expect("token source search failed")
    .unwrap_or(0);
    let coordinate_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.search_nodes(
                'name',
                'Alicia Source',
                'graph_test_users_pgtest'::regclass,
                mode := 'exact'
             )
             WHERE node_id = 'u1' AND verified",
    )
    .expect("search_nodes source search failed")
    .unwrap_or(0);
    let traversed_start_count = Spi::get_one::<i64>(
        "SELECT count(*)
             FROM graph.traverse_search(
                'name',
                'Alicia Source',
                'graph_test_users_pgtest'::regclass,
                search_mode := 'exact',
                max_depth := 0,
                hydrate := false
             )
             WHERE root_id = 'u1' AND node_id = 'u1' AND depth = 0",
    )
    .expect("traverse_search source search failed")
    .unwrap_or(0);

    assert_eq!(exact_count, 1);
    assert_eq!(prefix_count, 1);
    assert_eq!(contains_count, 1);
    assert_eq!(token_count, 1);
    assert_eq!(coordinate_count, 1);
    assert_eq!(traversed_start_count, 1);
}

#[pg_test]
fn source_search_queries_use_ordinary_postgres_index_plans() {
    reset_and_create_fixtures();
    Spi::run(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age) VALUES
                ('u3', 'Alice Smith', 29),
                ('u4', 'Alicia Source', 31)",
    )
    .expect("insert indexed search rows failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add_table failed");
    Spi::run("CREATE EXTENSION IF NOT EXISTS pg_trgm").expect("create pg_trgm failed");
    Spi::run(
        "CREATE INDEX graph_test_users_name_lower_idx
             ON public.graph_test_users_pgtest (lower(name::text))",
    )
    .expect("create exact search index failed");
    Spi::run(
        "CREATE INDEX graph_test_users_name_lower_pattern_idx
             ON public.graph_test_users_pgtest (lower(name::text) text_pattern_ops)",
    )
    .expect("create prefix search index failed");
    Spi::run(
        "CREATE INDEX graph_test_users_name_lower_trgm_idx
             ON public.graph_test_users_pgtest USING gin (lower(name::text) gin_trgm_ops)",
    )
    .expect("create trigram search index failed");
    Spi::run("ANALYZE public.graph_test_users_pgtest").expect("analyze search table failed");
    Spi::run("SET enable_seqscan = off").expect("disable seqscan failed");

    let table_oid =
        Spi::get_one::<pgrx::pg_sys::Oid>("SELECT 'graph_test_users_pgtest'::regclass::oid")
            .expect("table oid query failed")
            .expect("table oid missing")
            .to_u32();
    let exact_plan = explain_source_search_query("Alice Smith", "exact", table_oid);
    let prefix_plan = explain_source_search_query("Ali", "prefix", table_oid);
    Spi::run("SET enable_indexscan = off").expect("disable plain indexscan failed");
    let contains_plan = explain_source_search_query("Source", "contains", table_oid);
    let token_plan = explain_source_search_query("Alice Smith", "token", table_oid);
    Spi::run("RESET enable_indexscan").expect("reset plain indexscan failed");
    Spi::run("RESET enable_seqscan").expect("reset seqscan failed");

    assert!(
        exact_plan.contains("graph_test_users_name_lower_idx")
            || exact_plan.contains("graph_test_users_name_lower_pattern_idx"),
        "exact plan did not use an ordinary btree expression index:\n{}",
        exact_plan
    );
    assert!(
        prefix_plan.contains("graph_test_users_name_lower_idx")
            || prefix_plan.contains("graph_test_users_name_lower_pattern_idx"),
        "prefix plan did not use an ordinary btree expression index:\n{}",
        prefix_plan
    );
    assert!(
        contains_plan.contains("graph_test_users_name_lower_trgm_idx"),
        "contains plan did not use ordinary trigram index:\n{}",
        contains_plan
    );
    assert!(
        token_plan.contains("graph_test_users_name_lower_trgm_idx"),
        "token plan did not use ordinary trigram index:\n{}",
        token_plan
    );
}

#[pg_test]
fn dynamic_sql_values_are_bound_for_search_and_hydration() {
    reset_and_create_fixtures();
    let malicious_id = "u3') OR true --";
    let malicious_name = "Mallory' OR 'x'='x\\trail";
    Spi::run_with_args(
        "INSERT INTO public.graph_test_users_pgtest (id, name, age) VALUES ($1, $2, 29)",
        &[malicious_id.into(), malicious_name.into()],
    )
    .expect("insert malicious user failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add_table failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let (matched_count, matched_id, hydrated_name) = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT count(*)::bigint, min(node_id), min(node->>'name')
                     FROM graph.search('name', $1, mode := 'exact', hydrate := true)",
                None,
                &[malicious_name.into()],
            )
            .expect("parameterized search failed");
        let row = result.first();
        Ok::<_, pgrx::spi::Error>((
            row.get::<i64>(1)?.unwrap_or(0),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<String>(3)?.unwrap_or_default(),
        ))
    })
    .expect("search result read failed");

    let hydrated_id = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT node->>'id'
                     FROM graph.traverse(
                        'graph_test_users_pgtest'::regclass,
                        $1,
                        0,
                        hydrate := true
                     )",
                None,
                &[malicious_id.into()],
            )
            .expect("parameterized traverse hydration failed");
        Ok::<_, pgrx::spi::Error>(result.first().get::<String>(1)?.unwrap_or_default())
    })
    .expect("hydrated id read failed");

    assert_eq!(matched_count, 1);
    assert_eq!(matched_id, malicious_id);
    assert_eq!(hydrated_name, malicious_name);
    assert_eq!(hydrated_id, malicious_id);
}

#[pg_test]
fn generated_source_search_sql_uses_placeholders_for_user_values() {
    reset_and_create_fixtures();
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add_table failed");

    let malicious_name = "Mallory' OR 'x'='x\\trail";
    let queries = crate::sql_search::source_table_search_sql_for_test(
        "name",
        malicious_name,
        None,
        crate::types::SearchMode::Contains,
        false,
        None,
        true,
    )
    .expect("search SQL generation failed");

    assert!(!queries.is_empty());
    for query in queries {
        assert!(query.contains("LIKE $1 ESCAPE"));
        assert!(!query.contains(malicious_name));
        assert!(!query.contains("Mallory"));
        assert!(!query.contains("OR 'x'='x"));
    }

    let token_queries = crate::sql_search::source_table_search_sql_and_params_for_test(
        "name",
        "art') OR true --",
        None,
        crate::types::SearchMode::Token,
        false,
        None,
        true,
    )
    .expect("token search SQL generation failed");

    assert!(!token_queries.is_empty());
    for (query, params) in token_queries {
        assert!(query.contains("~ $1"));
        assert!(!query.contains("art"));
        assert!(!query.contains("OR true"));
        assert_eq!(
            params,
            vec![
                "(^|[^[:alnum:]])art([^[:alnum:]]|$)".to_string(),
                "(^|[^[:alnum:]])or([^[:alnum:]]|$)".to_string(),
                "(^|[^[:alnum:]])true([^[:alnum:]]|$)".to_string(),
            ]
        );
    }
}

#[pg_test]
fn public_add_edge_supports_fk_style_registered_source_tables() {
    reset_and_create_fixtures();
    Spi::run(
        "ALTER TABLE public.graph_test_users_pgtest
             ADD COLUMN mentor_id TEXT REFERENCES public.graph_test_users_pgtest(id)",
    )
    .expect("add mentor column failed");
    Spi::run("UPDATE public.graph_test_users_pgtest SET mentor_id = 'u2' WHERE id = 'u1'")
        .expect("update mentor failed");
    Spi::run(
        "SELECT graph.add_table(
                'graph_test_users_pgtest'::regclass,
                id_column := 'id',
                columns := ARRAY['name']
            )",
    )
    .expect("add_table failed");
    Spi::run(
        "SELECT graph.add_edge(
                'graph_test_users_pgtest'::regclass,
                'mentor_id',
                'graph_test_users_pgtest'::regclass,
                'id',
                'mentor',
                bidirectional := false
            )",
    )
    .expect("add FK-style edge failed");
    Spi::run("SELECT * FROM graph.build()").expect("build failed");

    let mentor_count = Spi::get_one::<i64>(
            "SELECT count(*)
             FROM graph.traverse('graph_test_users_pgtest'::regclass, 'u1', 1, edge_types := ARRAY['mentor'], hydrate := false)
             WHERE node_id = 'u2'",
        )
        .expect("FK-style traversal failed")
        .unwrap_or(0);

    assert_eq!(mentor_count, 1);
}

#[pg_test]
fn add_table_identifier_validation_accepts_pk_and_unique_not_null_indexes() {
    reset_and_create_fixtures();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_identifiers_pgtest CASCADE")
        .expect("drop identifiers table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_identifiers_pgtest (
                id TEXT PRIMARY KEY,
                org_id TEXT NOT NULL,
                external_id TEXT NOT NULL,
                sku TEXT NOT NULL,
                variant TEXT NOT NULL,
                nullable_code TEXT,
                loose_code TEXT,
                name TEXT NOT NULL,
                UNIQUE (external_id),
                UNIQUE (sku, variant),
                UNIQUE (nullable_code)
            )",
    )
    .expect("create identifiers table failed");
    let table_oid =
        Spi::get_one::<i32>("SELECT 'graph_test_identifiers_pgtest'::regclass::oid::int")
            .expect("identifier table oid lookup failed")
            .expect("identifier table oid was NULL") as u32;

    let pk_result = super::validate_registered_table(table_oid, "id", None, None);
    let unique_result = super::validate_registered_table(table_oid, "external_id", None, None);
    let composite_unique_result =
        super::validate_registered_table(table_oid, "sku,variant", None, None);

    assert!(pk_result.is_ok());
    assert!(unique_result.is_ok());
    assert!(composite_unique_result.is_ok());
}

#[pg_test]
fn add_table_identifier_validation_rejects_nullable_unique_and_non_unique_columns() {
    reset_and_create_fixtures();
    Spi::run("DROP TABLE IF EXISTS public.graph_test_bad_identifiers_pgtest CASCADE")
        .expect("drop bad identifiers table failed");
    Spi::run(
        "CREATE TABLE public.graph_test_bad_identifiers_pgtest (
                id TEXT PRIMARY KEY,
                nullable_code TEXT,
                loose_code TEXT,
                sku TEXT NOT NULL,
                variant TEXT NOT NULL,
                name TEXT NOT NULL,
                UNIQUE (nullable_code)
            )",
    )
    .expect("create bad identifiers table failed");
    let table_oid =
        Spi::get_one::<i32>("SELECT 'graph_test_bad_identifiers_pgtest'::regclass::oid::int")
            .expect("bad identifier table oid lookup failed")
            .expect("bad identifier table oid was NULL") as u32;

    let nullable_unique = super::validate_registered_table(table_oid, "nullable_code", None, None);
    let non_unique = super::validate_registered_table(table_oid, "loose_code", None, None);
    let composite_non_unique =
        super::validate_registered_table(table_oid, "sku,variant", None, None);

    assert!(nullable_unique.is_err());
    assert!(non_unique.is_err());
    assert!(composite_non_unique.is_err());
}

#[pg_test]
fn add_filter_column_rejects_non_numeric_columns() {
    reset_and_create_fixtures();
    let table_oid = Spi::get_one::<i32>("SELECT 'graph_test_bad_pgtest'::regclass::oid::int")
        .expect("table oid lookup failed")
        .expect("table oid was NULL");

    let result = super::validate_numeric_column(table_oid as u32, "note");
    assert!(result.is_err());
}

#[pg_test]
fn table_oid_lookup_reports_missing_relation_explicitly() {
    reset_and_create_fixtures();

    let result = table_oid_from_name("public.graph_test_missing_relation_pgtest");

    assert!(
        matches!(result, Err(super::safety::GraphError::Internal(message)) if message.contains("relation not found"))
    );
}
