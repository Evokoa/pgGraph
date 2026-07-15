\set ON_ERROR_STOP on

CREATE EXTENSION graph;
SELECT graph.reset();

SET graph.mutable_enabled = on;
SET graph.persist_on_build = on;
SET graph.default_projection_mode = 'mutable_overlay';

DROP TABLE IF EXISTS public.graph_process_sanitizer_nodes CASCADE;
CREATE TABLE public.graph_process_sanitizer_nodes (
    id text PRIMARY KEY,
    parent_id text REFERENCES public.graph_process_sanitizer_nodes(id),
    name text NOT NULL
);
INSERT INTO public.graph_process_sanitizer_nodes (id, parent_id, name)
VALUES
    ('root', NULL, 'Root'),
    ('child', 'root', 'Child');

SELECT graph.add_table(
    'public.graph_process_sanitizer_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name', 'parent_id']
);
SELECT graph.add_edge(
    'public.graph_process_sanitizer_nodes'::regclass,
    'parent_id',
    'public.graph_process_sanitizer_nodes'::regclass,
    'id',
    'parent',
    bidirectional := false
);
SELECT * FROM graph.build_graph(
    'default',
    force_persist := true,
    graph_namespace := 'public'
);

DO $$
BEGIN
    IF (SELECT count(*)
        FROM graph.traverse(
            'public.graph_process_sanitizer_nodes'::regclass,
            'child',
            1,
            direction := 'out',
            hydrate := false
        )
        WHERE node_id = 'root') <> 1 THEN
        RAISE EXCEPTION 'persisted traversal did not reach root';
    END IF;
END
$$;

INSERT INTO graph._build_jobs (
    build_id,
    status,
    sync_mode,
    projection_mode
)
VALUES (
    'process-sanitizer-build',
    'queued',
    'manual',
    'mutable_overlay'
);
DO $$
DECLARE
    worker_error text;
    worker_status text;
BEGIN
    SELECT graph._test_run_build_job('process-sanitizer-build')
      INTO worker_error;
    SELECT status
      INTO worker_status
      FROM graph._build_jobs
     WHERE build_id = 'process-sanitizer-build';
    IF worker_error IS NOT NULL OR worker_status <> 'completed' THEN
        RAISE EXCEPTION 'build worker path failed: status=%, error=%',
            worker_status,
            worker_error;
    END IF;
END
$$;

SELECT * FROM graph.unload_graph('default', namespace := 'public');
SELECT * FROM graph.load_graph('default', namespace := 'public');

BEGIN;
SAVEPOINT sanitizer_write;
SELECT graph.gql(
    'CREATE (node:graph_process_sanitizer_nodes {id: ''rolled-back'', name: ''Rolled Back''}) RETURN node'
);
ROLLBACK TO SAVEPOINT sanitizer_write;
RELEASE SAVEPOINT sanitizer_write;
COMMIT;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM public.graph_process_sanitizer_nodes
        WHERE id = 'rolled-back'
    ) THEN
        RAISE EXCEPTION 'subtransaction callback retained rolled-back source row';
    END IF;

    BEGIN
        PERFORM graph._test_error_unwind();
        RAISE EXCEPTION 'error-unwind probe unexpectedly returned';
    EXCEPTION
        WHEN SQLSTATE '22023' THEN NULL;
    END;

    IF NOT graph._test_error_unwind_observed() THEN
        RAISE EXCEPTION 'Rust error frame was not dropped before PostgreSQL ERROR';
    END IF;
END
$$;
