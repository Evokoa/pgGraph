#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_create_tx}"
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
DROP TABLE IF EXISTS public.graph_gql_create_tx_nodes CASCADE;
CREATE TABLE public.graph_gql_create_tx_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
INSERT INTO public.graph_gql_create_tx_nodes (id, name)
VALUES ('u1', 'Alice');
SELECT graph.add_table(
    'public.graph_gql_create_tx_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name']
);
SELECT * FROM graph.build(mode := 'mutable_overlay');
DO $$
DECLARE
    mode text;
    added_nodes integer;
BEGIN
    SELECT projection_mode, tx_delta_added_nodes INTO mode, added_nodes
    FROM graph.status();
    IF mode <> 'mutable_overlay' OR added_nodes <> 0 THEN
        RAISE EXCEPTION 'fixture expected clean mutable overlay, got mode=%, added_nodes=%',
            mode, added_nodes;
    END IF;
END
$$;

BEGIN;
SELECT graph.gql(
    'CREATE (u:graph_gql_create_tx_nodes {id: ''rollback-node'', name: ''Rolled Back''}) RETURN u'
);
DO $$
DECLARE
    source_count bigint;
    match_count bigint;
    dirty boolean;
    added_nodes integer;
BEGIN
    SELECT count(*) INTO source_count
    FROM public.graph_gql_create_tx_nodes
    WHERE id = 'rollback-node';
    IF source_count <> 1 THEN
        RAISE EXCEPTION 'GQL CREATE source row was not visible before rollback, got %',
            source_count;
    END IF;

    SELECT count(*) INTO match_count
    FROM graph.gql(
        'MATCH (u:graph_gql_create_tx_nodes {id: ''rollback-node''}) RETURN u',
        hydrate := false
    );
    IF match_count <> 1 THEN
        RAISE EXCEPTION 'GQL CREATE node delta was not visible to node MATCH before rollback, got %',
            match_count;
    END IF;

    SELECT tx_delta_dirty, tx_delta_added_nodes INTO dirty, added_nodes
    FROM graph.status();
    IF NOT dirty OR added_nodes <> 1 THEN
        RAISE EXCEPTION 'GQL CREATE expected dirty node delta before rollback, got dirty=%, added_nodes=%',
            dirty, added_nodes;
    END IF;
END
$$;
ROLLBACK;

DO $$
DECLARE
    source_count bigint;
    dirty boolean;
    added_nodes integer;
BEGIN
    SELECT count(*) INTO source_count
    FROM public.graph_gql_create_tx_nodes
    WHERE id = 'rollback-node';
    IF source_count <> 0 THEN
        RAISE EXCEPTION 'rollback left GQL CREATE source row behind, got % rows',
            source_count;
    END IF;

    SELECT tx_delta_dirty, tx_delta_added_nodes INTO dirty, added_nodes
    FROM graph.status();
    IF dirty OR added_nodes <> 0 THEN
        RAISE EXCEPTION 'rollback left GQL CREATE node delta behind, got dirty=%, added_nodes=%',
            dirty, added_nodes;
    END IF;
END
$$;

BEGIN;
SELECT graph.gql(
    'CREATE (u:graph_gql_create_tx_nodes {id: ''commit-node'', name: ''Committed''}) RETURN u'
);
DO $$
DECLARE
    dirty boolean;
    added_nodes integer;
BEGIN
    SELECT tx_delta_dirty, tx_delta_added_nodes INTO dirty, added_nodes
    FROM graph.status();
    IF NOT dirty OR added_nodes <> 1 THEN
        RAISE EXCEPTION 'GQL CREATE expected dirty node delta before commit, got dirty=%, added_nodes=%',
            dirty, added_nodes;
    END IF;
END
$$;
COMMIT;

DO $$
DECLARE
    source_count bigint;
    dirty boolean;
    added_nodes integer;
BEGIN
    SELECT count(*) INTO source_count
    FROM public.graph_gql_create_tx_nodes
    WHERE id = 'commit-node';
    IF source_count <> 1 THEN
        RAISE EXCEPTION 'commit did not preserve GQL CREATE source row, got % rows',
            source_count;
    END IF;

    SELECT tx_delta_dirty, tx_delta_added_nodes INTO dirty, added_nodes
    FROM graph.status();
    IF dirty OR added_nodes <> 0 THEN
        RAISE EXCEPTION 'commit left GQL CREATE node delta behind, got dirty=%, added_nodes=%',
            dirty, added_nodes;
    END IF;
END
$$;
SQL

echo "GQL CREATE transaction lifecycle checks passed on database: $DBNAME"
