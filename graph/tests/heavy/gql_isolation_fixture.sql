CREATE EXTENSION graph;
SELECT graph.reset();
SET graph.persist_on_build = :'persist_on_build';
SET graph.sync_mode = 'trigger';
SET graph.query_freshness = 'apply_pending_sync';
SET graph.mutable_enabled = on;

CREATE TABLE public.graph_gql_isolation_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    age INTEGER NOT NULL DEFAULT 0,
    status TEXT
);
CREATE TABLE public.graph_gql_isolation_edges (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES public.graph_gql_isolation_nodes(id),
    target_id TEXT NOT NULL REFERENCES public.graph_gql_isolation_nodes(id)
);

DO $fixture$
DECLARE
    slug text;
BEGIN
    FOREACH slug IN ARRAY ARRAY['read-committed', 'repeatable-read', 'serializable'] LOOP
        INSERT INTO public.graph_gql_isolation_nodes (id, name, age, status)
        VALUES
            ('set-' || slug, 'Set', 10, 'present'),
            ('remove-' || slug, 'Remove', 10, 'present'),
            ('rel-create-source-' || slug, 'Create source', 0, NULL),
            ('rel-create-target-' || slug, 'Create target', 0, NULL),
            ('rel-delete-source-' || slug, 'Delete source', 0, NULL),
            ('rel-delete-target-' || slug, 'Delete target', 0, NULL),
            ('detach-' || slug, 'Detach', 0, NULL),
            ('detach-target-' || slug, 'Detach target', 0, NULL);
        INSERT INTO public.graph_gql_isolation_edges (id, source_id, target_id)
        VALUES
            ('delete-edge-' || slug, 'rel-delete-source-' || slug, 'rel-delete-target-' || slug),
            ('detach-edge-' || slug, 'detach-' || slug, 'detach-target-' || slug);
    END LOOP;
END
$fixture$;

SELECT graph.add_table(
    'public.graph_gql_isolation_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name', 'age', 'status']
);
SELECT graph.add_edge(
    'public.graph_gql_isolation_edges'::regclass,
    'source_id',
    'public.graph_gql_isolation_nodes'::regclass,
    'target_id',
    'linked',
    bidirectional := false
);
SELECT graph.add_filter_column('public.graph_gql_isolation_nodes'::regclass, 'age');
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT graph.enable_sync();

CREATE FUNCTION public.graph_test_isolation_assert(slug text, mutated boolean)
RETURNS void
LANGUAGE plpgsql
AS $assert$
DECLARE
    expected integer := CASE WHEN mutated THEN 1 ELSE 0 END;
    expected_inverse integer := 1 - expected;
    actual integer;
    actual_age integer;
    actual_status text;
    status_json jsonb;
BEGIN
    SELECT count(*)::integer INTO actual
    FROM public.graph_gql_isolation_nodes WHERE id = 'created-' || slug;
    IF actual <> expected THEN
        RAISE EXCEPTION 'node CREATE source state for %: expected %, got %', slug, expected, actual;
    END IF;
    SELECT count(*)::integer INTO actual
    FROM graph.gql(
        format('MATCH (u:graph_gql_isolation_nodes {id: %L}) RETURN u', 'created-' || slug),
        hydrate := false
    );
    IF actual <> expected THEN
        RAISE EXCEPTION 'node CREATE graph state for %: expected %, got %', slug, expected, actual;
    END IF;

    SELECT count(*)::integer INTO actual
    FROM public.graph_gql_isolation_nodes WHERE id = 'merged-' || slug;
    IF actual <> expected THEN
        RAISE EXCEPTION 'MERGE source state for %: expected %, got %', slug, expected, actual;
    END IF;
    SELECT count(*)::integer INTO actual
    FROM graph.gql(
        format('MATCH (u:graph_gql_isolation_nodes {id: %L}) RETURN u', 'merged-' || slug),
        hydrate := false
    );
    IF actual <> expected THEN
        RAISE EXCEPTION 'MERGE graph state for %: expected %, got %', slug, expected, actual;
    END IF;

    SELECT age INTO actual_age
    FROM public.graph_gql_isolation_nodes WHERE id = 'set-' || slug;
    IF actual_age <> (CASE WHEN mutated THEN 20 ELSE 10 END) THEN
        RAISE EXCEPTION 'SET source state for %: got %', slug, actual_age;
    END IF;
    SELECT count(*)::integer INTO actual
    FROM graph.gql(
        format(
            'MATCH (u:graph_gql_isolation_nodes {id: %L}) WHERE u.age = %s RETURN u',
            'set-' || slug,
            CASE WHEN mutated THEN 20 ELSE 10 END
        ),
        hydrate := false
    );
    IF actual <> 1 THEN
        RAISE EXCEPTION 'SET graph/filter state for %: expected one row, got %', slug, actual;
    END IF;

    SELECT status INTO actual_status
    FROM public.graph_gql_isolation_nodes WHERE id = 'remove-' || slug;
    IF (mutated AND actual_status IS NOT NULL)
       OR (NOT mutated AND actual_status IS DISTINCT FROM 'present') THEN
        RAISE EXCEPTION 'REMOVE source state for %: got %', slug, actual_status;
    END IF;
    SELECT row -> 'status' INTO status_json
    FROM graph.gql(
        format(
            'MATCH (u:graph_gql_isolation_nodes {id: %L}) RETURN u.status AS status',
            'remove-' || slug
        )
    );
    IF (mutated AND status_json IS DISTINCT FROM 'null'::jsonb)
       OR (NOT mutated AND status_json IS DISTINCT FROM '"present"'::jsonb) THEN
        RAISE EXCEPTION 'REMOVE graph state for %: got %', slug, status_json;
    END IF;

    SELECT count(*)::integer INTO actual
    FROM public.graph_gql_isolation_edges WHERE id = 'create-edge-' || slug;
    IF actual <> expected THEN
        RAISE EXCEPTION 'relationship CREATE source state for %: expected %, got %', slug, expected, actual;
    END IF;
    SELECT count(*)::integer INTO actual
    FROM graph.gql(
        format(
            'MATCH (u:graph_gql_isolation_nodes {id: %L})-[:linked]->(v:graph_gql_isolation_nodes {id: %L}) RETURN u, v',
            'rel-create-source-' || slug,
            'rel-create-target-' || slug
        ),
        hydrate := false
    );
    IF actual <> expected THEN
        RAISE EXCEPTION 'relationship CREATE graph state for %: expected %, got %', slug, expected, actual;
    END IF;

    SELECT count(*)::integer INTO actual
    FROM public.graph_gql_isolation_edges WHERE id = 'delete-edge-' || slug;
    IF actual <> expected_inverse THEN
        RAISE EXCEPTION 'relationship DELETE source state for %: expected %, got %', slug, expected_inverse, actual;
    END IF;
    SELECT count(*)::integer INTO actual
    FROM graph.gql(
        format(
            'MATCH (u:graph_gql_isolation_nodes {id: %L})-[:linked]->(v:graph_gql_isolation_nodes {id: %L}) RETURN u, v',
            'rel-delete-source-' || slug,
            'rel-delete-target-' || slug
        ),
        hydrate := false
    );
    IF actual <> expected_inverse THEN
        RAISE EXCEPTION 'relationship DELETE graph state for %: expected %, got %', slug, expected_inverse, actual;
    END IF;

    SELECT count(*)::integer INTO actual
    FROM public.graph_gql_isolation_nodes WHERE id = 'detach-' || slug;
    IF actual <> expected_inverse THEN
        RAISE EXCEPTION 'DETACH DELETE source state for %: expected %, got %', slug, expected_inverse, actual;
    END IF;
    SELECT count(*)::integer INTO actual
    FROM graph.gql(
        format('MATCH (u:graph_gql_isolation_nodes {id: %L}) RETURN u', 'detach-' || slug),
        hydrate := false
    );
    IF actual <> expected_inverse THEN
        RAISE EXCEPTION 'DETACH DELETE graph state for %: expected %, got %', slug, expected_inverse, actual;
    END IF;
    SELECT count(*)::integer INTO actual
    FROM public.graph_gql_isolation_edges WHERE id = 'detach-edge-' || slug;
    IF actual <> expected_inverse THEN
        RAISE EXCEPTION 'DETACH DELETE edge source state for %: expected %, got %', slug, expected_inverse, actual;
    END IF;
END
$assert$;

CREATE FUNCTION public.graph_test_isolation_apply(slug text)
RETURNS void
LANGUAGE plpgsql
AS $apply$
DECLARE
    returned text;
    returned_json jsonb;
    dirty boolean;
BEGIN
    SELECT row ->> 'value' INTO returned
    FROM graph.gql(format(
        'CREATE (u:graph_gql_isolation_nodes {id: %L, name: ''Created''}) RETURN u.id AS value',
        'created-' || slug
    ));
    IF returned IS DISTINCT FROM 'created-' || slug THEN
        RAISE EXCEPTION 'node CREATE returned %, expected %', returned, 'created-' || slug;
    END IF;

    SELECT row ->> 'value' INTO returned
    FROM graph.gql(format(
        'MATCH (u:graph_gql_isolation_nodes {id: %L}), (v:graph_gql_isolation_nodes {id: %L}) CREATE (u)-[r:linked {id: %L}]->(v) RETURN u.id AS value',
        'rel-create-source-' || slug,
        'rel-create-target-' || slug,
        'create-edge-' || slug
    ));
    IF returned IS DISTINCT FROM 'rel-create-source-' || slug THEN
        RAISE EXCEPTION 'relationship CREATE returned %, expected %', returned, 'rel-create-source-' || slug;
    END IF;

    SELECT row ->> 'value' INTO returned
    FROM graph.gql(format(
        'MATCH (u:graph_gql_isolation_nodes {id: %L}) SET u.age = 20 RETURN u.age AS value',
        'set-' || slug
    ));
    IF returned IS DISTINCT FROM '20' THEN
        RAISE EXCEPTION 'SET returned %, expected 20', returned;
    END IF;

    SELECT row -> 'value' INTO returned_json
    FROM graph.gql(format(
        'MATCH (u:graph_gql_isolation_nodes {id: %L}) REMOVE u.status RETURN u.status AS value',
        'remove-' || slug
    ));
    IF returned_json IS DISTINCT FROM 'null'::jsonb THEN
        RAISE EXCEPTION 'REMOVE returned %, expected null', returned_json;
    END IF;

    SELECT row ->> 'value' INTO returned
    FROM graph.gql(format(
        'MATCH (u:graph_gql_isolation_nodes {id: %L})-[r:linked]->(v:graph_gql_isolation_nodes {id: %L}) DELETE r RETURN u.id AS value',
        'rel-delete-source-' || slug,
        'rel-delete-target-' || slug
    ));
    IF returned IS DISTINCT FROM 'rel-delete-source-' || slug THEN
        RAISE EXCEPTION 'relationship DELETE returned %, expected %', returned, 'rel-delete-source-' || slug;
    END IF;

    SELECT row ->> 'value' INTO returned
    FROM graph.gql(format(
        'MATCH (u:graph_gql_isolation_nodes {id: %L}) DETACH DELETE u RETURN u.id AS value',
        'detach-' || slug
    ));
    IF returned IS DISTINCT FROM 'detach-' || slug THEN
        RAISE EXCEPTION 'DETACH DELETE returned %, expected %', returned, 'detach-' || slug;
    END IF;

    SELECT row ->> 'value' INTO returned
    FROM graph.gql(format(
        'MERGE (u:graph_gql_isolation_nodes {id: %L, name: ''Merged''}) ON CREATE SET u.status = ''created'' RETURN u.name AS value',
        'merged-' || slug
    ));
    IF returned IS DISTINCT FROM 'Merged' THEN
        RAISE EXCEPTION 'MERGE returned %, expected Merged', returned;
    END IF;

    PERFORM public.graph_test_isolation_assert(slug, true);
    SELECT tx_delta_dirty INTO dirty FROM graph.status();
    IF NOT dirty THEN
        RAISE EXCEPTION 'writer transaction did not retain local graph delta for %', slug;
    END IF;
END
$apply$;

CREATE FUNCTION public.graph_test_isolation_assert_create(slug text, mutated boolean)
RETURNS void
LANGUAGE plpgsql
AS $assert_create$
DECLARE
    expected integer := CASE WHEN mutated THEN 1 ELSE 0 END;
    actual integer;
BEGIN
    SELECT count(*)::integer INTO actual
    FROM public.graph_gql_isolation_nodes WHERE id = 'created-' || slug;
    IF actual <> expected THEN
        RAISE EXCEPTION 'node CREATE source state for %: expected %, got %', slug, expected, actual;
    END IF;
    SELECT count(*)::integer INTO actual
    FROM graph.gql(
        format('MATCH (u:graph_gql_isolation_nodes {id: %L}) RETURN u', 'created-' || slug),
        hydrate := false
    );
    IF actual <> expected THEN
        RAISE EXCEPTION 'node CREATE graph state for %: expected %, got %', slug, expected, actual;
    END IF;
END
$assert_create$;

CREATE FUNCTION public.graph_test_isolation_apply_create(slug text)
RETURNS void
LANGUAGE plpgsql
AS $apply_create$
DECLARE
    returned text;
    dirty boolean;
BEGIN
    SELECT row ->> 'value' INTO returned
    FROM graph.gql(format(
        'CREATE (u:graph_gql_isolation_nodes {id: %L, name: ''Created''}) RETURN u.id AS value',
        'created-' || slug
    ));
    IF returned IS DISTINCT FROM 'created-' || slug THEN
        RAISE EXCEPTION 'node CREATE returned %, expected %', returned, 'created-' || slug;
    END IF;
    PERFORM public.graph_test_isolation_assert_create(slug, true);
    SELECT tx_delta_dirty INTO dirty FROM graph.status();
    IF NOT dirty THEN
        RAISE EXCEPTION 'writer transaction did not retain local graph delta for %', slug;
    END IF;
END
$apply_create$;

CREATE FUNCTION public.graph_test_isolation_assert_profile(
    slug text,
    mutated boolean,
    full_profile boolean
)
RETURNS void
LANGUAGE plpgsql
AS $assert_profile$
BEGIN
    IF full_profile THEN
        PERFORM public.graph_test_isolation_assert(slug, mutated);
    ELSE
        PERFORM public.graph_test_isolation_assert_create(slug, mutated);
    END IF;
END
$assert_profile$;

CREATE FUNCTION public.graph_test_isolation_apply_profile(slug text, full_profile boolean)
RETURNS void
LANGUAGE plpgsql
AS $apply_profile$
BEGIN
    IF full_profile THEN
        PERFORM public.graph_test_isolation_apply(slug);
    ELSE
        PERFORM public.graph_test_isolation_apply_create(slug);
    END IF;
END
$apply_profile$;
