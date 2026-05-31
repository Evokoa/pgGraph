#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_set_tx}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"

if [[ -z "$PG_CONFIG" ]]; then
  if [[ -x "/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config"
  elif [[ -x "/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config"
  else
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE"
    exit 2
  fi
fi

cargo pgrx install --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE" \
  --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
SET graph.mutable_enabled = on;
DROP TABLE IF EXISTS public.graph_gql_set_tx_edges CASCADE;
DROP TABLE IF EXISTS public.graph_gql_set_tx_nodes CASCADE;
CREATE TABLE public.graph_gql_set_tx_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    age INT NOT NULL
);
CREATE TABLE public.graph_gql_set_tx_edges (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES public.graph_gql_set_tx_nodes(id),
    target_id TEXT NOT NULL REFERENCES public.graph_gql_set_tx_nodes(id)
);
INSERT INTO public.graph_gql_set_tx_nodes (id, name, age)
VALUES ('u1', 'Alice', 37), ('u2', 'Bob', 41);
INSERT INTO public.graph_gql_set_tx_edges (id, source_id, target_id)
VALUES ('e1', 'u1', 'u2');
SELECT graph.add_table(
    'public.graph_gql_set_tx_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name', 'age']
);
SELECT graph.add_edge(
    'public.graph_gql_set_tx_edges'::regclass,
    'source_id',
    'public.graph_gql_set_tx_nodes'::regclass,
    'target_id',
    'friend'
);
SELECT graph.add_filter_column('public.graph_gql_set_tx_nodes'::regclass, 'age');
SELECT * FROM graph.build(mode := 'mutable_overlay');

BEGIN;
SELECT graph.gql(
    'MATCH (u:graph_gql_set_tx_nodes {id: ''u2''}) SET u.age = 101 RETURN u.age'
);
DO $$
DECLARE
    source_age integer;
    filtered_count bigint;
    dirty boolean;
BEGIN
    SELECT age INTO source_age
    FROM public.graph_gql_set_tx_nodes
    WHERE id = 'u2';
    IF source_age <> 101 THEN
        RAISE EXCEPTION 'GQL SET source row was not visible before rollback, got age=%',
            source_age;
    END IF;

    SELECT count(*) INTO filtered_count
    FROM graph.traverse(
        'public.graph_gql_set_tx_nodes'::regclass,
        'u1',
        1,
        filter := '{"node":{"where":{"age":{"gt":100}}}}'::jsonb,
        hydrate := false
    )
    WHERE node_id = 'u2';
    IF filtered_count <> 1 THEN
        RAISE EXCEPTION 'GQL SET filter delta was not visible before rollback, got %',
            filtered_count;
    END IF;

    SELECT tx_delta_dirty INTO dirty
    FROM graph.status();
    IF NOT dirty THEN
        RAISE EXCEPTION 'GQL SET expected dirty transaction delta before rollback';
    END IF;
END
$$;
ROLLBACK;

DO $$
DECLARE
    source_age integer;
    filtered_count bigint;
    dirty boolean;
BEGIN
    SELECT age INTO source_age
    FROM public.graph_gql_set_tx_nodes
    WHERE id = 'u2';
    IF source_age <> 41 THEN
        RAISE EXCEPTION 'rollback left GQL SET source row behind, got age=%',
            source_age;
    END IF;

    SELECT count(*) INTO filtered_count
    FROM graph.traverse(
        'public.graph_gql_set_tx_nodes'::regclass,
        'u1',
        1,
        filter := '{"node":{"where":{"age":{"gt":100}}}}'::jsonb,
        hydrate := false
    )
    WHERE node_id = 'u2';
    IF filtered_count <> 0 THEN
        RAISE EXCEPTION 'rollback left GQL SET filter delta behind, got %',
            filtered_count;
    END IF;

    SELECT tx_delta_dirty INTO dirty
    FROM graph.status();
    IF dirty THEN
        RAISE EXCEPTION 'rollback left GQL SET transaction delta dirty';
    END IF;
END
$$;
SQL

echo "GQL SET transaction lifecycle checks passed on database: $DBNAME"
