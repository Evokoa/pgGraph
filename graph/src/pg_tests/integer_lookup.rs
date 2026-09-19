fn integer_lookup_governor() -> crate::resource::ResourceGovernor {
    crate::resource::ResourceGovernor::new(crate::resource::ResourceLimits::memory_only(
        crate::resource::MemoryBudget::new(crate::resource::ByteCount::from_bytes(
            64 * 1024 * 1024,
        )),
    ))
}

fn integer_lookup_keys(
    lookup: &crate::sql_hydration::SourceKeyLookup,
    keys: &[String],
) -> Vec<String> {
    let mut found: Vec<String> = Spi::connect(|client| {
        client
            .select(
                &format!(
                    "SELECT {} FROM {} src WHERE {}",
                    lookup.key_expr,
                    lookup.table_name,
                    lookup.batch_predicate()
                ),
                None,
                &[lookup.batch_arg(keys, &integer_lookup_governor()).unwrap()],
            )
            .expect("source key batch lookup failed")
            .into_iter()
            .map(|row| row.get::<String>(1).unwrap().unwrap())
            .collect()
    });
    found.sort();
    found
}

#[pg_test]
fn integer_source_lookup_preserves_boundaries_and_noncanonical_no_match() {
    let governor = integer_lookup_governor();
    let columns = crate::builder::PrimaryKeySpec::from_columns(vec!["Key.ID\"quoted".into()]);
    for (table, kind, minimum, maximum) in [
        (
            "graph_test_lookup_int2",
            "smallint",
            i64::from(i16::MIN),
            i64::from(i16::MAX),
        ),
        (
            "graph_test_lookup_int4",
            "integer",
            i64::from(i32::MIN),
            i64::from(i32::MAX),
        ),
        ("graph_test_lookup_int8", "bigint", i64::MIN, i64::MAX),
    ] {
        Spi::run(&format!(
            "CREATE TEMP TABLE {table} (\"Key.ID\"\"quoted\" {kind} PRIMARY KEY);
             INSERT INTO {table} VALUES ({minimum}), (-1), (0), (1), ({maximum})"
        ))
        .unwrap();
        let oid = crate::catalog::table_oid_from_name(table).unwrap();
        let lookup =
            crate::sql_hydration::SourceKeyLookup::prepare(oid, &columns, "src", &governor)
                .unwrap();
        let mut keys = vec![
            minimum.to_string(),
            maximum.to_string(),
            "-1".into(),
            "0".into(),
            "1".into(),
        ];
        let mut expected = keys.clone();
        expected.sort();
        keys.extend(
            [
                "1",
                "2",
                "",
                "+1",
                "01",
                "-01",
                "-0",
                " 1",
                "1 ",
                "1.0",
                "1e0",
                "9223372036854775808",
                "-9223372036854775809",
            ]
            .map(str::to_owned),
        );
        if let Some(value) = minimum.checked_sub(1) {
            keys.push(value.to_string());
        }
        if let Some(value) = maximum.checked_add(1) {
            keys.push(value.to_string());
        }
        assert_eq!(integer_lookup_keys(&lookup, &keys), expected);
        for key in &keys {
            let actual = Spi::connect(|client| {
                let result = client
                    .select(
                        &format!(
                            "SELECT {} FROM {} src WHERE {} LIMIT 1",
                            lookup.key_expr,
                            lookup.table_name,
                            lookup.scalar_predicate()
                        ),
                        None,
                        &[lookup.scalar_arg(key)],
                    )
                    .unwrap();
                if result.is_empty() {
                    None
                } else {
                    result.first().get::<String>(1).unwrap()
                }
            });
            assert_eq!(
                actual.as_ref(),
                expected.iter().find(|candidate| *candidate == key),
                "{kind}: {key:?}"
            );
        }
        assert!(integer_lookup_keys(&lookup, &[]).is_empty());
        assert!(integer_lookup_keys(&lookup, &["+1".into(), "01".into()]).is_empty());
    }
}

#[pg_test]
fn integer_source_lookup_keeps_text_domain_and_composite_fallbacks() {
    Spi::run(
        "CREATE DOMAIN graph_test_lookup_domain AS bigint;
         CREATE TEMP TABLE graph_test_lookup_text (id text PRIMARY KEY);
         INSERT INTO graph_test_lookup_text VALUES ('01'), ('+1'), ('-0'), ('1');
         CREATE TEMP TABLE graph_test_lookup_domain_table (id graph_test_lookup_domain PRIMARY KEY);
         INSERT INTO graph_test_lookup_domain_table VALUES (1);
         CREATE TEMP TABLE graph_test_lookup_composite (id bigint, part text, PRIMARY KEY (id, part));
         INSERT INTO graph_test_lookup_composite VALUES (1, 'x')"
    ).unwrap();
    let governor = integer_lookup_governor();
    for (table, columns, keys, expected) in [
        (
            "graph_test_lookup_text",
            vec!["id"],
            vec!["01", "+1", "-0", "1"],
            vec!["+1", "-0", "01", "1"],
        ),
        (
            "graph_test_lookup_domain_table",
            vec!["id"],
            vec!["01", "+1", "1"],
            vec!["1"],
        ),
        (
            "graph_test_lookup_composite",
            vec!["id", "part"],
            vec!["[\"1\", \"x\"]", "[1, \"x\"]"],
            vec!["[\"1\", \"x\"]"],
        ),
    ] {
        let columns = crate::builder::PrimaryKeySpec::from_columns(
            columns.into_iter().map(str::to_owned).collect(),
        );
        let oid = crate::catalog::table_oid_from_name(table).unwrap();
        let lookup =
            crate::sql_hydration::SourceKeyLookup::prepare(oid, &columns, "src", &governor)
                .unwrap();
        assert_eq!(
            lookup.scalar_predicate(),
            format!("{} = $1", crate::catalog::primary_key_expr("src", &columns))
        );
        assert_eq!(
            integer_lookup_keys(
                &lookup,
                &keys.into_iter().map(str::to_owned).collect::<Vec<_>>()
            ),
            expected
        );
    }
}

#[pg_test]
fn integer_source_lookup_qualified_name_survives_search_path_changes() {
    Spi::run(
        "CREATE SCHEMA graph_lookup_original;
         CREATE SCHEMA graph_lookup_shadow;
         CREATE TABLE graph_lookup_original.same_name (id bigint PRIMARY KEY, payload text);
         CREATE TABLE graph_lookup_shadow.same_name (id bigint PRIMARY KEY, payload text);
         INSERT INTO graph_lookup_original.same_name VALUES (1, 'original');
         INSERT INTO graph_lookup_shadow.same_name VALUES (1, 'shadow');
         SET LOCAL search_path = graph_lookup_original, pg_catalog",
    )
    .unwrap();
    let oid = crate::catalog::table_oid_from_name("graph_lookup_original.same_name").unwrap();
    let governor = integer_lookup_governor();
    let columns = crate::builder::PrimaryKeySpec::from_columns(vec!["id".into()]);
    let lookup =
        crate::sql_hydration::SourceKeyLookup::prepare(oid, &columns, "src", &governor).unwrap();
    Spi::run("SET LOCAL search_path = graph_lookup_shadow, pg_catalog").unwrap();
    for batch in [false, true] {
        let keys = vec!["1".into()];
        let predicate = if batch {
            lookup.batch_predicate()
        } else {
            lookup.scalar_predicate()
        };
        let arg = if batch {
            lookup.batch_arg(&keys, &governor).unwrap()
        } else {
            lookup.scalar_arg("1")
        };
        let source = Spi::connect(|client| {
            client
                .select(
                    &format!(
                        "SELECT pg_catalog.to_jsonb(src.*) FROM {} src WHERE {predicate}",
                        lookup.table_name
                    ),
                    None,
                    &[arg],
                )
                .unwrap()
                .first()
                .get::<pgrx::JsonB>(1)
                .unwrap()
                .unwrap()
                .0
        });
        assert_eq!(
            source,
            serde_json::json!({"id": 1, "payload": "original"}),
            "batch={batch}"
        );
    }
}

#[pg_test]
fn integer_source_lookup_pins_relation_before_source_query() {
    Spi::run("CREATE TEMP TABLE graph_test_lookup_lock (id bigint PRIMARY KEY)").unwrap();
    let oid = crate::catalog::table_oid_from_name("graph_test_lookup_lock").unwrap();
    let count_locks = || {
        Spi::get_one_with_args::<i64>(
            "SELECT count(*) FROM pg_catalog.pg_locks
             WHERE pid = pg_catalog.pg_backend_pid() AND relation = $1
               AND mode = 'AccessShareLock' AND granted",
            &[pgrx::pg_sys::Oid::from_u32(oid).into()],
        )
        .unwrap()
        .unwrap()
    };
    assert_eq!(count_locks(), 0, "fixture has not read the source table");
    let governor = integer_lookup_governor();
    let columns = crate::builder::PrimaryKeySpec::from_columns(vec!["id".into()]);
    let lookup =
        crate::sql_hydration::SourceKeyLookup::prepare(oid, &columns, "src", &governor).unwrap();
    assert_eq!(
        count_locks(),
        1,
        "type metadata must already be pinned before source SELECT"
    );
    drop(lookup);
    assert_eq!(
        count_locks(),
        1,
        "PostgreSQL retains the lock until transaction end"
    );
}

#[pg_test]
fn integer_source_lookup_resolves_current_type_in_each_operation() {
    Spi::run(
        "CREATE TEMP TABLE graph_test_lookup_ddl (id bigint PRIMARY KEY);
              INSERT INTO graph_test_lookup_ddl VALUES (1)",
    )
    .unwrap();
    let governor = integer_lookup_governor();
    let oid = crate::catalog::table_oid_from_name("graph_test_lookup_ddl").unwrap();
    let columns = crate::builder::PrimaryKeySpec::from_columns(vec!["id".into()]);
    {
        let lookup =
            crate::sql_hydration::SourceKeyLookup::prepare(oid, &columns, "src", &governor)
                .unwrap();
        assert!(!lookup.scalar_predicate().contains("::text"));
        assert!(integer_lookup_keys(&lookup, &["01".into()]).is_empty());
    }
    Spi::run(
        "ALTER TABLE graph_test_lookup_ddl ALTER COLUMN id TYPE text;
              INSERT INTO graph_test_lookup_ddl VALUES ('01')",
    )
    .unwrap();
    let lookup =
        crate::sql_hydration::SourceKeyLookup::prepare(oid, &columns, "src", &governor).unwrap();
    assert!(lookup.scalar_predicate().contains("::text"));
    assert_eq!(
        integer_lookup_keys(&lookup, &["01".into(), "1".into()]),
        ["01", "1"]
    );
}

#[pg_test]
fn integer_source_lookup_generated_hydration_predicates_use_primary_key_indexes() {
    let governor = integer_lookup_governor();
    let columns = crate::builder::PrimaryKeySpec::from_columns(vec!["id".into()]);
    for (table, kind) in [
        ("graph_test_lookup_plan_int2", "smallint"),
        ("graph_test_lookup_plan_int4", "integer"),
        ("graph_test_lookup_plan_int8", "bigint"),
        ("graph_test_lookup_plan_text", "text"),
    ] {
        // The primary key is the only index. Default planner costs choose a
        // selective lookup at this size without disabling sequential scans.
        Spi::run(&format!(
            "CREATE TEMP TABLE {table} (id {kind} PRIMARY KEY, payload text);
             INSERT INTO {table} SELECT i::{kind}, repeat('x', 128) FROM generate_series(1, 8192) i;
             ANALYZE {table}"
        ))
        .unwrap();
        let oid = crate::catalog::table_oid_from_name(table).unwrap();
        let lookup =
            crate::sql_hydration::SourceKeyLookup::prepare(oid, &columns, "src", &governor)
                .unwrap();
        for batch in [false, true] {
            for size_preflight in [false, true] {
                let select = match (batch, size_preflight) {
                    (false, false) => "to_jsonb(src.*)",
                    (false, true) => "pg_catalog.pg_column_size(pg_catalog.to_jsonb(src.*))::bigint, pg_catalog.octet_length(pg_catalog.to_jsonb(src.*)::text)::bigint",
                    (true, false) => "src.id::text AS graph_node_id, to_jsonb(src.*)",
                    (true, true) => "pg_catalog.count(*)::bigint, COALESCE(pg_catalog.sum(pg_catalog.pg_column_size(pg_catalog.to_jsonb(src.*))), 0)::bigint, COALESCE(pg_catalog.sum(pg_catalog.octet_length(pg_catalog.to_jsonb(src.*)::text)), 0)::bigint",
                };
                let predicate = if batch {
                    lookup.batch_predicate()
                } else {
                    lookup.scalar_predicate()
                };
                let keys = vec!["4096".into(), "4097".into()];
                let arg = if batch {
                    lookup.batch_arg(&keys, &governor).unwrap()
                } else {
                    lookup.scalar_arg("4096")
                };
                let limit = if batch { "" } else { " LIMIT 1" };
                let plan = Spi::connect(|client| {
                    client.select(&format!("EXPLAIN (FORMAT JSON) SELECT {select} FROM {} src WHERE {predicate}{limit}", lookup.table_name), None, &[arg])
                        .unwrap().first().get::<pgrx::Json>(1).unwrap().unwrap().0.to_string()
                });
                assert!(
                    plan.contains("Index Cond"),
                    "{kind} batch={batch} size={size_preflight}: {plan}"
                );
                assert!(
                    !plan.contains("Seq Scan"),
                    "{kind} batch={batch} size={size_preflight}: {plan}"
                );
                if kind != "text" {
                    assert!(!predicate.contains("::text"));
                }
            }
        }
    }
}
