BEGIN;
SELECT nextval('public.graph_pgbench_node_seq') AS id,
       nextval('public.graph_pgbench_node_seq') AS next_id
\gset
INSERT INTO public.graph_pgbench_nodes (id, name, score)
VALUES (:id::text, 'name-' || :id::text, (:id % 1000)::int);
UPDATE public.graph_pgbench_nodes
SET name = 'updated-' || :id::text, score = ((:id + 1) % 1000)::int
WHERE id = :id::text;
UPDATE public.graph_pgbench_nodes
SET id = :next_id::text,
    name = 'renamed-' || :next_id::text,
    score = (:next_id % 1000)::int
WHERE id = :id::text;
DELETE FROM public.graph_pgbench_nodes WHERE id = :next_id::text;
COMMIT;
