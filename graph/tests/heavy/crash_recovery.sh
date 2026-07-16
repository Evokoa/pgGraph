#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_crash}"
PGDATA="${PGDATA:?PGDATA must point at the test Postgres data directory}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
POSTGRES_CTL="${POSTGRES_CTL:-pg_ctl}"
POSTGRES_OPTS="${POSTGRES_OPTS:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSPECTOR="$SCRIPT_DIR/../../../scripts/inspect_pggraph_artifact.py"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/pggraph-common.sh
source "$ROOT_DIR/scripts/lib/pggraph-common.sh"

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

cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

start_postgres() {
  if [[ -n "$POSTGRES_OPTS" ]]; then
    "$POSTGRES_CTL" -D "$PGDATA" -o "$POSTGRES_OPTS" start
  else
    "$POSTGRES_CTL" -D "$PGDATA" start
  fi
}

wait_for_postgres() {
  for _ in $(seq 1 60); do
    pg_isready -d "$DBNAME" >/dev/null 2>&1 && return 0
    sleep 1
  done
  echo "Postgres did not become ready for $DBNAME" >&2
  return 1
}

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
DROP TABLE IF EXISTS public.graph_crash_edges CASCADE;
DROP TABLE IF EXISTS public.graph_crash_nodes CASCADE;
CREATE TABLE public.graph_crash_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE public.graph_crash_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_crash_nodes(id),
    to_id TEXT NOT NULL REFERENCES public.graph_crash_nodes(id)
);
INSERT INTO public.graph_crash_nodes (id, name)
SELECT i::text, 'node-' || i::text
FROM generate_series(1, 50000) AS i;
INSERT INTO public.graph_crash_edges (from_id, to_id)
SELECT i::text, (i + 1)::text
FROM generate_series(1, 49999) AS i;
SELECT graph.add_table('public.graph_crash_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge('public.graph_crash_edges'::regclass, 'from_id', 'public.graph_crash_nodes'::regclass, 'id', 'crash', false);
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
SELECT graph.enable_sync();
INSERT INTO public.graph_crash_nodes VALUES ('50001', 'node-50001');
INSERT INTO public.graph_crash_edges (from_id, to_id) VALUES ('50000', '50001');
SQL

pggraph_validate_disposable_cluster "$DBNAME"
postgres_pid="$(head -n 1 "$PGDATA/postmaster.pid")"
kill -9 "$postgres_pid"
sleep 2
start_postgres
wait_for_postgres

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SET graph.auto_load = on;
DO $$
DECLARE
    nodes INTEGER;
    edges INTEGER;
    traversed BIGINT;
    applied BIGINT;
BEGIN
    SELECT inserts_applied + updates_applied + deletes_applied INTO applied FROM graph.apply_sync();
    IF applied <= 0 THEN
        RAISE EXCEPTION 'committed trigger rows were not recoverable after restart';
    END IF;

    SELECT count(*) INTO traversed FROM graph.traverse('public.graph_crash_nodes'::regclass, '1', 3);
    IF traversed = 0 THEN
        RAISE EXCEPTION 'post-restart traversal returned no rows';
    END IF;

    SELECT node_count, edge_count INTO nodes, edges FROM graph.status();
    IF nodes < 50000 OR edges < 49999 THEN
        RAISE EXCEPTION 'auto-load after restart returned incomplete graph: nodes %, edges %', nodes, edges;
    END IF;

    PERFORM * FROM graph.maintenance();
END
$$;
SQL

data_directory="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT current_setting('data_directory')")"
graph_data_dir="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT COALESCE(NULLIF(current_setting('graph.data_dir', true), ''), 'graph')")"
graph_id="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT graph_id FROM graph.current_graph()")"
if [[ "$graph_data_dir" = /* ]]; then
  graph_logical_path="$graph_data_dir/$graph_id/main.pggraph"
else
  graph_logical_path="$data_directory/$graph_data_dir/$graph_id/main.pggraph"
fi
graph_file="$(python3 "$INSPECTOR" --resolve-only "$graph_logical_path")"
if [[ -f "$graph_file" ]]; then
  printf 'X' | dd of="$graph_file" bs=1 seek=0 count=1 conv=notrunc status=none
  pggraph_validate_disposable_cluster "$DBNAME"
  "$POSTGRES_CTL" -D "$PGDATA" -m immediate stop
  start_postgres
  wait_for_postgres
  psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
SET graph.auto_load = on;
DO $$
BEGIN
    PERFORM * FROM graph.traverse('public.graph_crash_nodes'::regclass, '1', 1);
    RAISE EXCEPTION 'corrupted persisted graph was loaded successfully';
EXCEPTION
    WHEN OTHERS THEN
        IF SQLERRM = 'corrupted persisted graph was loaded successfully' THEN
            RAISE;
        END IF;
END
$$;
SQL
fi

echo "Crash recovery validation passed for $DBNAME"
