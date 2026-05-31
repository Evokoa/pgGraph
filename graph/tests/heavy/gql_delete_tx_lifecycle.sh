#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_delete_tx}"
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
DROP TABLE IF EXISTS public.graph_gql_delete_tx_edges CASCADE;
DROP TABLE IF EXISTS public.graph_gql_delete_tx_nodes CASCADE;
CREATE TABLE public.graph_gql_delete_tx_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE public.graph_gql_delete_tx_edges (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES public.graph_gql_delete_tx_nodes(id),
    target_id TEXT NOT NULL REFERENCES public.graph_gql_delete_tx_nodes(id)
);
INSERT INTO public.graph_gql_delete_tx_nodes (id, name)
VALUES ('u1', 'Alice'), ('u2', 'Bob');
INSERT INTO public.graph_gql_delete_tx_edges (id, source_id, target_id)
VALUES ('e1', 'u1', 'u2');
SELECT graph.add_table(
    'public.graph_gql_delete_tx_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name']
);
SELECT graph.add_edge(
    'public.graph_gql_delete_tx_edges'::regclass,
    'source_id',
    'public.graph_gql_delete_tx_nodes'::regclass,
    'target_id',
    'friend',
    bidirectional := true
);
SELECT * FROM graph.build(mode := 'mutable_overlay');

BEGIN;
SELECT graph.gql(
    'MATCH (u:graph_gql_delete_tx_nodes {id: ''u1''})-[r:friend]->(v:graph_gql_delete_tx_nodes {id: ''u2''}) DELETE r RETURN u, v'
);
DO $$
DECLARE
    edge_count bigint;
    forward_count bigint;
    reverse_count bigint;
    node_count bigint;
    dirty boolean;
BEGIN
    SELECT count(*) INTO edge_count
    FROM public.graph_gql_delete_tx_edges;
    IF edge_count <> 0 THEN
        RAISE EXCEPTION 'GQL DELETE source edge row remained before rollback, got %',
            edge_count;
    END IF;

    SELECT count(*) INTO forward_count
    FROM graph.traverse(
        'public.graph_gql_delete_tx_nodes'::regclass,
        'u1',
        1,
        edge_types := ARRAY['friend'],
        hydrate := false
    )
    WHERE node_id = 'u2';
    IF forward_count <> 0 THEN
        RAISE EXCEPTION 'GQL DELETE forward tombstone was not visible, got %',
            forward_count;
    END IF;

    SELECT count(*) INTO reverse_count
    FROM graph.traverse(
        'public.graph_gql_delete_tx_nodes'::regclass,
        'u2',
        1,
        edge_types := ARRAY['friend'],
        hydrate := false
    )
    WHERE node_id = 'u1';
    IF reverse_count <> 0 THEN
        RAISE EXCEPTION 'GQL DELETE reverse tombstone was not visible, got %',
            reverse_count;
    END IF;

    SELECT count(*) INTO node_count
    FROM public.graph_gql_delete_tx_nodes;
    IF node_count <> 2 THEN
        RAISE EXCEPTION 'GQL DELETE cascaded to endpoint nodes, got %',
            node_count;
    END IF;

    SELECT tx_delta_dirty INTO dirty
    FROM graph.status();
    IF NOT dirty THEN
        RAISE EXCEPTION 'GQL DELETE expected dirty transaction delta before rollback';
    END IF;
END
$$;
ROLLBACK;

DO $$
DECLARE
    edge_count bigint;
    forward_count bigint;
    reverse_count bigint;
    dirty boolean;
BEGIN
    SELECT count(*) INTO edge_count
    FROM public.graph_gql_delete_tx_edges;
    IF edge_count <> 1 THEN
        RAISE EXCEPTION 'rollback did not restore GQL DELETE source edge row, got %',
            edge_count;
    END IF;

    SELECT count(*) INTO forward_count
    FROM graph.traverse(
        'public.graph_gql_delete_tx_nodes'::regclass,
        'u1',
        1,
        edge_types := ARRAY['friend'],
        hydrate := false
    )
    WHERE node_id = 'u2';
    IF forward_count <> 1 THEN
        RAISE EXCEPTION 'rollback left GQL DELETE forward tombstone behind, got %',
            forward_count;
    END IF;

    SELECT count(*) INTO reverse_count
    FROM graph.traverse(
        'public.graph_gql_delete_tx_nodes'::regclass,
        'u2',
        1,
        edge_types := ARRAY['friend'],
        hydrate := false
    )
    WHERE node_id = 'u1';
    IF reverse_count <> 1 THEN
        RAISE EXCEPTION 'rollback left GQL DELETE reverse tombstone behind, got %',
            reverse_count;
    END IF;

    SELECT tx_delta_dirty INTO dirty
    FROM graph.status();
    IF dirty THEN
        RAISE EXCEPTION 'rollback left GQL DELETE transaction delta dirty';
    END IF;
END
$$;
SQL

echo "GQL DELETE transaction lifecycle checks passed on database: $DBNAME"
