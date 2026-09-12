fn build_integer_hydration_fixture() {
    reset_and_create_fixtures();
    Spi::run(
        "DROP FUNCTION IF EXISTS public.graph_integer_definer_pgtest();
         DROP FUNCTION IF EXISTS public.graph_integer_source_pgtest(text);
         DROP TABLE IF EXISTS public.graph_integer_edges_pgtest CASCADE;
         DROP TABLE IF EXISTS public.graph_integer_nodes_pgtest CASCADE;
         DROP ROLE IF EXISTS graph_integer_reader_pgtest;
         CREATE ROLE graph_integer_reader_pgtest;
         CREATE TABLE public.graph_integer_nodes_pgtest (
             id bigint PRIMARY KEY, payload text NOT NULL
         );
         CREATE TABLE public.graph_integer_edges_pgtest (
             id bigint PRIMARY KEY, src bigint, dst bigint, note text NOT NULL
         );
         INSERT INTO public.graph_integer_nodes_pgtest VALUES
             (1, 'one'), (2, 'two'), (3, 'hidden');
         INSERT INTO public.graph_integer_edges_pgtest VALUES
             (10, 1, 2, 'accepted'), (11, 1, 3, 'hidden endpoint'),
             (12, 1, 2, 'hidden relationship');
         SELECT graph.add_table(
             'graph_integer_nodes_pgtest'::regclass, 'id', ARRAY['payload']);
         SELECT graph.add_edge(
             'graph_integer_edges_pgtest'::regclass, 'src',
             'graph_integer_nodes_pgtest'::regclass, 'dst', 'integer_link', false);
         SELECT * FROM graph.build();
         GRANT USAGE ON SCHEMA graph, public TO graph_integer_reader_pgtest",
    )
    .expect("build integer hydration fixture failed");
}

#[track_caller]
fn integer_hydration_json(statement: &str) -> serde_json::Value {
    Spi::get_one::<pgrx::JsonB>(statement)
        .expect("integer hydration query failed")
        .expect("integer hydration JSON missing")
        .0
}

#[pg_test]
fn integer_hydration_preserves_rls_and_effective_definer_role() {
    build_integer_hydration_fixture();
    Spi::run(
        "ALTER TABLE public.graph_integer_nodes_pgtest ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.graph_integer_edges_pgtest ENABLE ROW LEVEL SECURITY;
         CREATE POLICY integer_nodes_visible ON public.graph_integer_nodes_pgtest
             USING (id <= 2);
         CREATE POLICY integer_edges_visible ON public.graph_integer_edges_pgtest
             USING (id IN (10, 11));
         GRANT SELECT ON public.graph_integer_nodes_pgtest,
             public.graph_integer_edges_pgtest TO graph_integer_reader_pgtest;
         CREATE FUNCTION public.graph_integer_definer_pgtest()
             RETURNS jsonb LANGUAGE sql VOLATILE SECURITY DEFINER
             SET search_path = pg_catalog, pg_temp
             AS $$
                 SELECT jsonb_agg(jsonb_build_array(
                     (row #>> '{r,id}')::bigint, row #>> '{r,note}')
                     ORDER BY (row #>> '{r,id}')::bigint)
                 FROM graph.gql(
                     'MATCH (a:graph_integer_nodes_pgtest {id: 1})
                      -[r:integer_link]->(b:graph_integer_nodes_pgtest) RETURN r')
             $$;
         ALTER FUNCTION public.graph_integer_definer_pgtest()
             OWNER TO graph_integer_reader_pgtest",
    )
    .expect("configure integer hydration policies failed");

    // The caller is privileged, but the wrapper must hydrate as its owner.
    let definer_rows = integer_hydration_json("SELECT public.graph_integer_definer_pgtest()");
    Spi::run("SET ROLE graph_integer_reader_pgtest").expect("set integer reader failed");
    let source_rows = integer_hydration_json(
        "SELECT jsonb_agg(jsonb_build_array(e.id, e.note) ORDER BY e.id)
         FROM public.graph_integer_edges_pgtest e
         JOIN public.graph_integer_nodes_pgtest a ON a.id = e.src
         JOIN public.graph_integer_nodes_pgtest b ON b.id = e.dst
         WHERE a.id = 1",
    );
    let node_rows = integer_hydration_json(
        "SELECT jsonb_agg(jsonb_build_array(
             (node->>'id')::bigint, node->>'payload') ORDER BY (node->>'id')::bigint)
         FROM graph.traverse('graph_integer_nodes_pgtest'::regclass, '1', 1)",
    );
    let source_nodes = integer_hydration_json(
        "SELECT jsonb_agg(jsonb_build_array(id, payload) ORDER BY id)
         FROM public.graph_integer_nodes_pgtest",
    );
    let single = integer_hydration_json(
        "SELECT node FROM graph.get_node(
             'default', 'graph_integer_nodes_pgtest', '1', hydrate := true)",
    );
    let hidden = Spi::get_one::<i64>(
        "SELECT count(*) FROM graph.get_node(
             'default', 'graph_integer_nodes_pgtest', '3', hydrate := true)",
    )
    .expect("hidden integer node lookup failed");
    Spi::run("RESET ROLE").expect("reset integer reader failed");

    assert_eq!(source_rows, serde_json::json!([[10, "accepted"]]));
    assert_eq!(definer_rows, source_rows);
    assert_eq!(node_rows, source_nodes);
    assert_eq!(source_nodes, serde_json::json!([[1, "one"], [2, "two"]]));
    assert_eq!(single, serde_json::json!({"id": 1, "payload": "one"}));
    assert_eq!(hidden, Some(0));
}

#[pg_test]
fn integer_hydration_checks_acl_before_noncanonical_and_missing_keys() {
    build_integer_hydration_fixture();
    Spi::run("INSERT INTO public.graph_integer_nodes_pgtest VALUES (0, 'zero')")
        .expect("insert canonical zero source failed");
    let oid = Spi::get_one::<pgrx::pg_sys::Oid>(
        "SELECT 'public.graph_integer_nodes_pgtest'::regclass::oid",
    )
    .expect("read integer node OID failed")
    .expect("integer node OID missing");
    assert_eq!(
        crate::sql_hydration::hydrate_node(oid.to_u32(), "0")
            .expect("canonical zero hydration failed")
            .expect("canonical zero hydration missing")
            .0,
        serde_json::json!({"id": 0, "payload": "zero"})
    );
    let invalid = [
        "01",
        "+1",
        " 1",
        "1 ",
        "-0",
        "9223372036854775808",
        "１",
        "",
        "999",
    ];
    for key in invalid {
        assert!(
            crate::sql_hydration::hydrate_node(oid.to_u32(), key)
                .expect("noncanonical integer hydration must return no match")
                .is_none(),
            "unexpected integer identity alias: {key:?}"
        );
    }

    Spi::run("SET ROLE graph_integer_reader_pgtest").expect("set denied integer role failed");
    for key in invalid {
        assert!(
            matches!(
                crate::sql_hydration::hydrate_node(oid.to_u32(), key),
                Err(crate::safety::GraphError::AclDenied { .. })
            ),
            "ACL denial must precede parsing key {key:?}"
        );
    }
    Spi::run("RESET ROLE").expect("reset denied integer role failed");
}

#[pg_test]
fn integer_hydration_observes_command_changes_rollback_and_source_deletion() {
    build_integer_hydration_fixture();
    Spi::run(
        "CREATE FUNCTION public.graph_integer_source_pgtest(candidate text)
             RETURNS text LANGUAGE sql VOLATILE AS $$
                 SELECT payload FROM public.graph_integer_nodes_pgtest
                 WHERE id = candidate::bigint
             $$",
    )
    .expect("create integer source snapshot oracle failed");
    let same_statement = integer_hydration_json(
        "WITH changed AS MATERIALIZED (
             UPDATE public.graph_integer_nodes_pgtest SET payload = 'changed'
             WHERE id = 1 RETURNING 1 AS marker
         )
         SELECT jsonb_build_array(
             (SELECT node->>'payload' FROM graph.get_node(
                 'default', 'graph_integer_nodes_pgtest',
                 CASE WHEN changed.marker = 1 THEN '1' END, hydrate := true)),
             public.graph_integer_source_pgtest(
                 CASE WHEN changed.marker = 1 THEN '1' END))
         FROM changed",
    );
    assert_eq!(same_statement, serde_json::json!(["changed", "changed"]));

    Spi::run(
        "DO $block$
         DECLARE observed text;
         BEGIN
             BEGIN
                 UPDATE public.graph_integer_nodes_pgtest
                     SET payload = 'rolled back' WHERE id = 1;
                 SELECT node->>'payload' INTO observed FROM graph.get_node(
                     'default', 'graph_integer_nodes_pgtest', '1', hydrate := true);
                 IF observed IS DISTINCT FROM 'rolled back' THEN
                     RAISE EXCEPTION 'integer hydration missed transaction-local update';
                 END IF;
                 RAISE EXCEPTION USING ERRCODE = 'ZB001', MESSAGE = 'rollback fixture';
             EXCEPTION WHEN SQLSTATE 'ZB001' THEN NULL;
             END;
         END
         $block$",
    )
    .expect("integer hydration rollback fixture failed");
    assert_eq!(
        integer_hydration_json(
            "SELECT node FROM graph.get_node(
                 'default', 'graph_integer_nodes_pgtest', '1', hydrate := true)"
        ),
        serde_json::json!({"id": 1, "payload": "changed"})
    );
    Spi::run(
        "DELETE FROM public.graph_integer_edges_pgtest WHERE src = 1 OR dst = 1;
         DELETE FROM public.graph_integer_nodes_pgtest WHERE id = 1",
    )
    .expect("delete projected integer source failed");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM graph.get_node(
                 'default', 'graph_integer_nodes_pgtest', '1', hydrate := true)"
        )
        .expect("stale integer hydration lookup failed"),
        Some(0)
    );
}

#[cfg(feature = "development")]
#[pg_test]
fn integer_hydration_retries_after_memory_rejection_and_cancellation() {
    build_integer_hydration_fixture();
    Spi::run(
        "UPDATE public.graph_integer_nodes_pgtest
             SET payload = repeat('x', 65536) WHERE id = 1;
         SET LOCAL graph.query_memory_mb = 1",
    )
    .expect("configure bounded integer hydration failed");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM graph.traverse(
                 'graph_integer_nodes_pgtest'::regclass, '1', 0, hydrate := false)"
        )
        .expect("coordinate-only integer lookup under small budget failed"),
        Some(1)
    );
    let statement = "SELECT * FROM graph.traverse(
        'graph_integer_nodes_pgtest'::regclass, '1', 0, hydrate := true)";
    assert_eq!(sqlstate_for_error(statement).as_deref(), Some("54000"));
    assert_eq!(
        sql_error_detail(statement).as_deref(),
        Some("pgGraph diagnostic: PG007")
    );
    Spi::run(
        "SET LOCAL graph.query_memory_mb = 64;
         SELECT graph._test_arm_hydration_cancel()",
    )
    .expect("arm integer hydration cancellation failed");
    assert!(workflow_cancellation(statement));
    assert_eq!(
        Spi::get_one::<i32>(
            "SELECT length(node->>'payload') FROM graph.traverse(
                 'graph_integer_nodes_pgtest'::regclass, '1', 0, hydrate := true)"
        )
        .expect("integer hydration retry failed"),
        Some(65536)
    );
}
