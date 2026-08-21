#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_open_type_high_cardinality}"
if [[ ! "$DBNAME" =~ ^pggraph_[A-Za-z0-9_]+$ ]]; then
    echo "DBNAME must match pggraph_[A-Za-z0-9_]+" >&2
    exit 2
fi

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SET graph.persist_on_build = off;
SET graph.sync_mode = 'manual';
SELECT graph.reset(true);
DROP TABLE IF EXISTS public.p9_edges;
DROP TABLE IF EXISTS public.p9_nodes;

CREATE TABLE public.p9_nodes (id integer PRIMARY KEY);
INSERT INTO public.p9_nodes VALUES (1), (2), (3), (4);
CREATE TABLE public.p9_edges (
    id integer PRIMARY KEY,
    source_id integer NOT NULL REFERENCES public.p9_nodes(id),
    target_id integer NOT NULL REFERENCES public.p9_nodes(id),
    rel_type text NOT NULL
);
INSERT INTO public.p9_edges
SELECT value,
       1,
       CASE value
           WHEN 65534 THEN 2
           WHEN 65535 THEN 3
           WHEN 65536 THEN 4
           ELSE 2
       END,
       'type_' || value
FROM generate_series(1, 65536) AS value;

SELECT graph.add_table('public.p9_nodes'::regclass, 'id');
SELECT graph.add_edge(
    'public.p9_edges'::regclass,
    'source_id', 'public.p9_nodes'::regclass, 'target_id',
    'fallback', false, label_column := 'rel_type');
SELECT * FROM graph.build();

DO $storage$
DECLARE
    inventory_count bigint;
    maximum_type_id bigint;
    page_count bigint;
    page_max bigint;
BEGIN
    inventory_count := 0;
    maximum_type_id := 0;
    LOOP
        SELECT count(*), max(type_id) INTO page_count, page_max
        FROM graph.edge_types(after_type_id := maximum_type_id, max_rows := 10000);
        EXIT WHEN page_count = 0;
        inventory_count := inventory_count + page_count;
        maximum_type_id := page_max;
    END LOOP;
    IF inventory_count IS DISTINCT FROM 65536 THEN
        RAISE EXCEPTION 'edge type inventory count %, expected 65536', inventory_count;
    END IF;
    IF maximum_type_id IS DISTINCT FROM 65536 THEN
        RAISE EXCEPTION 'maximum edge type ID %, expected 65536', maximum_type_id;
    END IF;
END
$storage$;

DO $matrix$
DECLARE
    boundary integer;
    expected_target text;
    observed text;
BEGIN
    FOREACH boundary IN ARRAY ARRAY[65534, 65535, 65536] LOOP
        expected_target := CASE boundary
            WHEN 65534 THEN '2'
            WHEN 65535 THEN '3'
            ELSE '4'
        END;

        SELECT node_id INTO observed
        FROM graph.traverse(
            'public.p9_nodes'::regclass, '1', 1,
            edge_types := ARRAY['type_' || boundary], hydrate := false)
        WHERE depth = 1;
        IF observed IS DISTINCT FROM expected_target THEN
            RAISE EXCEPTION 'graph.traverse boundary % returned %, expected %',
                boundary, observed, expected_target;
        END IF;

        SELECT edge_label INTO observed
        FROM graph.shortest_path(
            'public.p9_nodes'::regclass, '1',
            'public.p9_nodes'::regclass, expected_target, 1,
            hydrate := false, edge_types := ARRAY['type_' || boundary])
        WHERE step = 1;
        IF observed IS DISTINCT FROM 'type_' || boundary THEN
            RAISE EXCEPTION 'graph.shortest_path boundary % returned %', boundary, observed;
        END IF;

        SELECT row #>> '{v,_id,id}' INTO observed
        FROM graph.gql(format(
            'MATCH (u:p9_nodes {id: 1})-[:type_%s]->(v:p9_nodes) RETURN v',
            boundary), hydrate := false);
        IF observed IS DISTINCT FROM expected_target THEN
            RAISE EXCEPTION 'graph.gql boundary % returned %, expected %',
                boundary, observed, expected_target;
        END IF;

        SELECT row #>> '{v,_id,id}' INTO observed
        FROM graph.cypher(format(
            'MATCH (u:p9_nodes {id: 1})-[:type_%s]->(v:p9_nodes) RETURN v',
            boundary), hydrate := false);
        IF observed IS DISTINCT FROM expected_target THEN
            RAISE EXCEPTION 'graph.cypher boundary % returned %, expected %',
                boundary, observed, expected_target;
        END IF;

        SELECT row #>> '{v,_id,id}' INTO observed
        FROM graph.gql(
            'MATCH (u:p9_nodes {id: 1})-[r]->(v:p9_nodes)
             WHERE r.rel_type = $type RETURN v',
            params := jsonb_build_object('type', 'type_' || boundary),
            hydrate := false);
        IF observed IS DISTINCT FROM expected_target THEN
            RAISE EXCEPTION 'dynamic label equality boundary % returned %, expected %',
                boundary, observed, expected_target;
        END IF;
    END LOOP;
END
$matrix$;
SQL
