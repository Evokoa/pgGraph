#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_open_type_package}"
if [[ ! "$DBNAME" =~ ^pggraph_[A-Za-z0-9_]+$ ]]; then
  echo "DBNAME must match pggraph_[A-Za-z0-9_]+" >&2
  exit 2
fi

cleanup() {
  dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
CREATE EXTENSION graph;
SET graph.persist_on_build = on;
SELECT graph.reset();

CREATE TABLE public.open_type_nodes (id text PRIMARY KEY);
INSERT INTO public.open_type_nodes VALUES ('u1'), ('u2'), ('u3');
CREATE TABLE public.open_type_edges (
  id text PRIMARY KEY,
  source_id text NOT NULL REFERENCES public.open_type_nodes(id),
  target_id text NOT NULL REFERENCES public.open_type_nodes(id),
  rel_type text NOT NULL
);
INSERT INTO public.open_type_edges
SELECT 'edge_' || value,
       'u1',
       CASE WHEN value = 255 THEN 'u3' ELSE 'u2' END,
       'type_' || value
FROM generate_series(1, 255) AS value;

SELECT graph.add_table('public.open_type_nodes'::regclass, 'id');
SELECT graph.add_edge(
  'public.open_type_edges'::regclass,
  'source_id', 'public.open_type_nodes'::regclass, 'target_id',
  'fallback', false, label_column := 'rel_type');
SELECT * FROM graph.build();
SELECT graph.unload_graph('default');
SELECT * FROM graph.load_graph('default');

DO $matrix$
DECLARE
  observed text;
  inventory_count bigint;
BEGIN
  SELECT count(*) INTO inventory_count
  FROM graph.edge_types(after_type_id := 0, max_rows := 256);
  IF inventory_count IS DISTINCT FROM 255 THEN
    RAISE EXCEPTION 'open-type inventory count %, expected 255', inventory_count;
  END IF;

  SELECT node_id INTO observed
  FROM graph.traverse(
    'public.open_type_nodes'::regclass, 'u1', 1,
    edge_types := ARRAY['type_255'], hydrate := false)
  WHERE depth = 1;
  IF observed IS DISTINCT FROM 'u3' THEN
    RAISE EXCEPTION 'open-type traversal returned %, expected u3', observed;
  END IF;

  SELECT edge_label INTO observed
  FROM graph.shortest_path(
    'public.open_type_nodes'::regclass, 'u1',
    'public.open_type_nodes'::regclass, 'u3', 1,
    edge_types := ARRAY['type_255'], hydrate := false)
  WHERE step = 1;
  IF observed IS DISTINCT FROM 'type_255' THEN
    RAISE EXCEPTION 'open-type path returned %, expected type_255', observed;
  END IF;

  SELECT row #>> '{v,_id,id}' INTO observed
  FROM graph.gql(
    'MATCH (u:open_type_nodes {id: ''u1''})-[:type_255]->(v:open_type_nodes) RETURN v',
    hydrate := false);
  IF observed IS DISTINCT FROM 'u3' THEN
    RAISE EXCEPTION 'open-type GQL returned %, expected u3', observed;
  END IF;

  SELECT row #>> '{v,_id,id}' INTO observed
  FROM graph.cypher(
    'MATCH (u:open_type_nodes {id: ''u1''})-[:type_255]->(v:open_type_nodes) RETURN v',
    hydrate := false);
  IF observed IS DISTINCT FROM 'u3' THEN
    RAISE EXCEPTION 'open-type Cypher returned %, expected u3', observed;
  END IF;
END
$matrix$;

ALTER TABLE public.open_type_edges DISABLE TRIGGER USER;
INSERT INTO public.open_type_edges
VALUES ('edge_over_limit', 'u1', 'u3', repeat('x', 1025));
DO $policy$
DECLARE
  detail text;
BEGIN
  BEGIN
    PERFORM * FROM graph.build();
    RAISE EXCEPTION 'oversized relationship type unexpectedly built';
  EXCEPTION
    WHEN SQLSTATE '54000' THEN
      GET STACKED DIAGNOSTICS detail = PG_EXCEPTION_DETAIL;
      IF position('PG004' IN coalesce(detail, '')) = 0 THEN
        RAISE EXCEPTION 'oversized relationship type omitted PG004 detail: %', detail;
      END IF;
  END;
END
$policy$;
DELETE FROM public.open_type_edges WHERE id = 'edge_over_limit';
ALTER TABLE public.open_type_edges ENABLE TRIGGER USER;

DO $last_good$
DECLARE
  observed text;
BEGIN
  SELECT node_id INTO observed
  FROM graph.traverse(
    'public.open_type_nodes'::regclass, 'u1', 1,
    edge_types := ARRAY['type_255'], hydrate := false)
  WHERE depth = 1;
  IF observed IS DISTINCT FROM 'u3' THEN
    RAISE EXCEPTION 'failed policy build replaced last-good graph: %', observed;
  END IF;
END
$last_good$;
SQL

echo "Installed-package open-type smoke passed on database: $DBNAME"
