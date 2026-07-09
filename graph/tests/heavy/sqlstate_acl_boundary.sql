-- Boundary checks that intentionally run outside `cargo pgrx test`.
--
-- The pgrx test harness executes #[pg_test] bodies inside generated SQL
-- functions. Nested ERROR catching and SET ROLE are not equivalent to the real
-- client/server boundary there, so SQLSTATE preservation and restricted-role
-- ACL checks live in this standalone script instead.
--
-- NOTE: Prefer `run_sqlstate_acl_boundary.sh` for routine verification.
-- This file keeps the SQL steps together, but client-side SQLSTATE assertions
-- are more stable than nested DO/EXCEPTION blocks for pgrx extensions.

CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
SET graph.auto_load = off;

DROP TABLE IF EXISTS public.graph_boundary_edges CASCADE;
DROP TABLE IF EXISTS public.graph_boundary_nodes CASCADE;
CREATE TABLE public.graph_boundary_nodes (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    age INT NOT NULL,
    friend_id TEXT REFERENCES public.graph_boundary_nodes(id)
);
CREATE TABLE public.graph_boundary_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_boundary_nodes(id),
    to_id TEXT NOT NULL REFERENCES public.graph_boundary_nodes(id)
);
INSERT INTO public.graph_boundary_nodes VALUES ('b', 't2', 'Bob', 20, NULL), ('a', 't1', 'Alice', 10, 'b');
INSERT INTO public.graph_boundary_edges (from_id, to_id) VALUES ('a', 'b');

DO $$
BEGIN
    PERFORM * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1);
    RAISE EXCEPTION 'expected 55000';
EXCEPTION WHEN SQLSTATE '55000' THEN
    NULL;
END $$;

SELECT graph.add_table('public.graph_boundary_nodes'::regclass, 'id', ARRAY['tenant_id', 'name', 'age']);
SELECT graph.add_edge('public.graph_boundary_nodes'::regclass, 'friend_id', 'public.graph_boundary_nodes'::regclass, 'id', 'boundary', bidirectional := false);
SELECT graph.add_filter_column('public.graph_boundary_nodes'::regclass, 'age');
SELECT * FROM graph.build();

DO $$
BEGIN
    PERFORM * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'missing', 1);
    RAISE EXCEPTION 'expected P0002';
EXCEPTION WHEN SQLSTATE 'P0002' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, NULL, '🔥 > 1');
    RAISE EXCEPTION 'expected 22023';
EXCEPTION WHEN SQLSTATE '22023' THEN
    NULL;
END $$;

SET graph.enabled = off;
DO $$
BEGIN
    PERFORM * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1);
    RAISE EXCEPTION 'expected 55000';
EXCEPTION WHEN SQLSTATE '55000' THEN
    NULL;
END $$;
SET graph.enabled = on;

DROP ROLE IF EXISTS graph_boundary_restricted;
CREATE ROLE graph_boundary_restricted;
GRANT USAGE ON SCHEMA graph TO graph_boundary_restricted;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA graph TO graph_boundary_restricted;
GRANT SELECT ON public.graph_boundary_nodes TO graph_boundary_restricted;
ALTER TABLE public.graph_boundary_nodes ENABLE ROW LEVEL SECURITY;
CREATE POLICY graph_boundary_tenant_rls
    ON public.graph_boundary_nodes
    FOR SELECT TO graph_boundary_restricted
    USING (tenant_id = current_setting('graph.boundary_tenant', true));
DROP TABLE IF EXISTS public.graph_boundary_traversal_coords;
CREATE TABLE public.graph_boundary_traversal_coords AS
    SELECT node_table, node_id, depth
    FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, hydrate := false);
GRANT SELECT ON public.graph_boundary_traversal_coords TO graph_boundary_restricted;

SET ROLE graph_boundary_restricted;
SET graph.boundary_tenant = 't1';
DO $$
DECLARE
    topology_rows INTEGER;
    hydrated_rows INTEGER;
BEGIN
    SELECT count(*) INTO topology_rows FROM public.graph_boundary_traversal_coords;
    SELECT count(*) INTO hydrated_rows
    FROM public.graph_boundary_traversal_coords g
    JOIN public.graph_boundary_nodes n ON n.id = g.node_id;
    IF topology_rows <> 2 OR hydrated_rows <> 1 THEN
        RAISE EXCEPTION 'expected topology_rows=2 and hydrated_rows=1, got %, %',
            topology_rows, hydrated_rows;
    END IF;
END $$;
RESET ROLE;

SET ROLE graph_boundary_restricted;
DO $$
BEGIN
    INSERT INTO graph._registered_tables (table_name, id_column) VALUES ('public.nope', 'id');
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN insufficient_privilege THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM graph.add_table('public.graph_boundary_nodes'::regclass, 'id');
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM * FROM graph.build();
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM * FROM graph.vacuum();
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM * FROM graph.maintenance();
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM graph.reset();
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM graph.enable_sync();
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM * FROM graph.apply_sync();
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM * FROM graph.connected_components();
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;

DO $$
BEGIN
    PERFORM * FROM graph.component_stats();
    RAISE EXCEPTION 'expected 42501';
EXCEPTION WHEN SQLSTATE '42501' THEN
    NULL;
END $$;
RESET ROLE;
