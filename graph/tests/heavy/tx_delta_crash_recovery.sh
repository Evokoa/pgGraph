#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_tx_delta_crash}"
PGDATA="${PGDATA:?PGDATA must point at a disposable PostgreSQL data directory}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
POSTGRES_CTL="${POSTGRES_CTL:-pg_ctl}"
POSTGRES_OPTS="${POSTGRES_OPTS:-}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-tx-delta-crash.XXXXXX")"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/pggraph-common.sh
source "$ROOT_DIR/scripts/lib/pggraph-common.sh"

cleanup() {
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

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
  echo "PostgreSQL did not become ready for $DBNAME" >&2
  return 1
}

wait_for_overlay_session() {
  local attempts=60
  local out

  for _ in $(seq 1 "$attempts"); do
    out="$(psql -X -q -v ON_ERROR_STOP=1 -tA -d "$DBNAME" -c \
      "SELECT count(*)::bigint
       FROM pg_locks
       WHERE locktype = 'advisory'
         AND classid = 1918928211
         AND objid = 1735552874;")"
    if [[ "$out" == "1" ]]; then
      return 0
    fi
    sleep 0.5
  done

  echo "Timed out waiting for the uncommitted tx overlay session"
  exit 1
}

cargo pgrx install --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE pg_test" \
  --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

cat >"$WORKDIR/fixture.sql" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
SET graph.persist_on_build = on;
SET graph.auto_load = on;
DROP TABLE IF EXISTS public.graph_tx_delta_crash_nodes CASCADE;
CREATE TABLE public.graph_tx_delta_crash_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    friend_id TEXT REFERENCES public.graph_tx_delta_crash_nodes(id)
);
INSERT INTO public.graph_tx_delta_crash_nodes (id, name, friend_id)
VALUES ('u1', 'Alice', 'u2'), ('u2', 'Bob', NULL);
SELECT graph.add_table(
    'public.graph_tx_delta_crash_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name']
);
SELECT graph.add_edge(
    'public.graph_tx_delta_crash_nodes'::regclass,
    'friend_id',
    'public.graph_tx_delta_crash_nodes'::regclass,
    'id',
    'friend',
    bidirectional := false
);
SELECT * FROM graph.build();
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -f "$WORKDIR/fixture.sql" >/dev/null

cat >"$WORKDIR/open-overlay.sql" <<'SQL'
BEGIN;
SELECT graph._test_record_tx_edge(
    'public.graph_tx_delta_crash_nodes'::regclass,
    'u2',
    'public.graph_tx_delta_crash_nodes'::regclass,
    'u1',
    'friend',
    'insert'
);
DO $$
DECLARE
    reverse_count bigint;
    dirty boolean;
BEGIN
    SELECT tx_delta_dirty INTO dirty FROM graph.status();
    IF NOT dirty THEN
        RAISE EXCEPTION 'expected tx_delta_dirty while uncommitted overlay is open';
    END IF;

    SELECT count(*) INTO reverse_count
    FROM graph.gql(
        'MATCH (u:graph_tx_delta_crash_nodes)-[:friend]->(v:graph_tx_delta_crash_nodes) RETURN u, v',
        hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u2'
      AND row #>> '{v,_id,id}' = 'u1';
    IF reverse_count <> 1 THEN
        RAISE EXCEPTION 'expected uncommitted tx overlay to be visible before crash, got %',
            reverse_count;
    END IF;
END
$$;
SELECT pg_advisory_lock(1918928211, 1735552874);
SELECT pg_sleep(120);
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -f "$WORKDIR/open-overlay.sql" \
  >"$WORKDIR/open-overlay.log" 2>&1 &
overlay_pid=$!
wait_for_overlay_session

pggraph_validate_disposable_cluster "$DBNAME"
postgres_pid="$(head -n 1 "$PGDATA/postmaster.pid")"
kill -9 "$postgres_pid"
sleep 2
wait "$overlay_pid" >/dev/null 2>&1 || true
start_postgres
wait_for_postgres

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SET graph.auto_load = on;
DO $$
DECLARE
    base_count bigint;
    reverse_count bigint;
    dirty boolean;
BEGIN
    SELECT tx_delta_dirty INTO dirty FROM graph.status();
    IF dirty THEN
        RAISE EXCEPTION 'tx_delta_dirty survived postmaster restart';
    END IF;

    SELECT count(*) INTO base_count
    FROM graph.gql(
        'MATCH (u:graph_tx_delta_crash_nodes)-[:friend]->(v:graph_tx_delta_crash_nodes) RETURN u, v',
        hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u1'
      AND row #>> '{v,_id,id}' = 'u2';
    IF base_count <> 1 THEN
        RAISE EXCEPTION 'persisted base graph did not reload after crash, got % base rows',
            base_count;
    END IF;

    SELECT count(*) INTO reverse_count
    FROM graph.gql(
        'MATCH (u:graph_tx_delta_crash_nodes)-[:friend]->(v:graph_tx_delta_crash_nodes) RETURN u, v',
        hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u2'
      AND row #>> '{v,_id,id}' = 'u1';
    IF reverse_count <> 0 THEN
        RAISE EXCEPTION 'uncommitted tx overlay survived crash/reload, got % rows',
            reverse_count;
    END IF;
END
$$;
SQL

echo "Transaction delta crash recovery passed for $DBNAME"
