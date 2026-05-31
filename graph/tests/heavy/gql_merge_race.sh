#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_merge_race}"
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
DROP TABLE IF EXISTS public.graph_gql_merge_race_nodes CASCADE;
CREATE TABLE public.graph_gql_merge_race_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    age INT NOT NULL DEFAULT 0
);
SELECT graph.add_table(
    'public.graph_gql_merge_race_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name', 'age']
);
SELECT * FROM graph.build(mode := 'mutable_overlay');
SQL

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/a.out" &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_sleep(4);
SELECT row #>> '{name}', (row #>> '{age}')::int
FROM graph.gql(
    'MERGE (u:graph_gql_merge_race_nodes {id: ''race-node'', name: ''from-a''})
     ON CREATE SET u.age = 1
     ON MATCH SET u.name = ''matched-a''
     RETURN u.name AS name, u.age AS age'
);
SQL
pid_a=$!

sleep 1

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/b.out" &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_sleep(1);
SELECT row #>> '{name}', (row #>> '{age}')::int
FROM graph.gql(
    'MERGE (u:graph_gql_merge_race_nodes {id: ''race-node'', name: ''from-b''})
     ON CREATE SET u.age = 2
     ON MATCH SET u.name = ''matched-b''
     RETURN u.name AS name, u.age AS age'
);
SQL
pid_b=$!

wait "$pid_a"
wait "$pid_b"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
DO $$
DECLARE
    source_count bigint;
    inserted_age int;
    final_name text;
BEGIN
    SELECT count(*), min(age), max(name)
    INTO source_count, inserted_age, final_name
    FROM public.graph_gql_merge_race_nodes
    WHERE id = 'race-node';

    IF source_count <> 1 THEN
        RAISE EXCEPTION 'GQL MERGE race expected one source row, got %',
            source_count;
    END IF;

    IF inserted_age NOT IN (1, 2) THEN
        RAISE EXCEPTION 'GQL MERGE race expected inserted ON CREATE age 1 or 2, got %',
            inserted_age;
    END IF;

    IF final_name NOT IN ('matched-a', 'matched-b') THEN
        RAISE EXCEPTION 'GQL MERGE race expected a matched final name, got %',
            final_name;
    END IF;
END
$$;
SQL

echo "GQL MERGE race checks passed on database: $DBNAME"
