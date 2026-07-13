#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_isolation}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/pggraph-gql-isolation.XXXXXX")"

cleanup() {
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

cd "$GRAPH_DIR"

if [[ -z "$PG_CONFIG" ]]; then
  if [[ -x "/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config"
  elif [[ -x "/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config"
  else
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE" >&2
    exit 2
  fi
fi

cargo pgrx install --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE" \
  --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
CREATE EXTENSION graph;
SELECT graph.reset();
SET graph.persist_on_build = off;
SET graph.sync_mode = 'trigger';
SET graph.query_freshness = 'apply_pending_sync';
SET graph.mutable_enabled = on;
CREATE TABLE public.graph_gql_isolation_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
SELECT graph.add_table(
    'public.graph_gql_isolation_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name']
);
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT graph.enable_sync();
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.sync_mode = 'trigger'" >/dev/null
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.query_freshness = 'apply_pending_sync'" >/dev/null
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.mutable_enabled = on" >/dev/null
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.persist_on_build = off" >/dev/null

wait_for_reader() {
  local lock_key="$1"
  local attempts=50
  local count

  for _ in $(seq 1 "$attempts"); do
    count="$(psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
      "SELECT count(*) FROM pg_locks
       WHERE locktype = 'advisory'
         AND objid = $lock_key
         AND granted")"
    if [[ "$count" == "1" ]]; then
      return 0
    fi
    sleep 0.1
  done

  echo "timed out waiting for $2 isolation reader" >&2
  return 1
}

run_level() {
  local isolation="$1"
  local slug="$2"
  local expected_after="$3"
  local lock_key="$4"
  local writer_ready_key="$((lock_key + 100000))"
  local writer_done_key="$((lock_key + 200000))"
  local reader_ack_key="$((lock_key + 300000))"
  local node_id="node-$slug"
  local reader_out="$WORKDIR/$slug-reader.out"
  local writer_out="$WORKDIR/$slug-writer.out"

  psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v node_id="$node_id" \
    -v writer_ready_key="$writer_ready_key" \
    -v reader_lock_key="$lock_key" \
    -v writer_done_key="$writer_done_key" \
    -v reader_ack_key="$reader_ack_key" >"$writer_out" <<'SQL' &
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(:writer_ready_key);
SELECT set_config('pggraph.reader_lock_key', :'reader_lock_key', false);
DO $$
DECLARE
  attempt integer;
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND objid = current_setting('pggraph.reader_lock_key')::oid
        AND granted
    ) THEN
      RETURN;
    END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for isolation reader';
END
$$;
SELECT * FROM graph.gql(
  format(
    'CREATE (u:graph_gql_isolation_nodes {id: %L, name: %L}) RETURN u',
    :'node_id',
    :'node_id'
  )
);
SELECT pg_advisory_lock(:writer_done_key);
SELECT set_config('pggraph.reader_ack_key', :'reader_ack_key', false);
DO $$
DECLARE
  attempt integer;
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND objid = current_setting('pggraph.reader_ack_key')::oid
        AND granted
    ) THEN
      RETURN;
    END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for isolation reader acknowledgement';
END
$$;
SELECT pg_advisory_unlock(:writer_done_key);
SQL
  local writer_pid=$!

  wait_for_reader "$writer_ready_key" "$isolation writer"

  psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v isolation="$isolation" \
    -v node_id="$node_id" \
    -v lock_key="$lock_key" \
    -v writer_done_key="$writer_done_key" \
    -v reader_ack_key="$reader_ack_key" \
    >"$reader_out" <<'SQL' &
\o /dev/null
SELECT * FROM graph.build(mode := 'mutable_overlay');
\o
BEGIN ISOLATION LEVEL :isolation;
SELECT 'before_source=' || count(*) FROM public.graph_gql_isolation_nodes WHERE id = :'node_id';
SELECT 'before_graph=' || count(*)
FROM graph.gql(
  format('MATCH (u:graph_gql_isolation_nodes {id: %L}) RETURN u', :'node_id'),
  hydrate := false
);
SELECT pg_advisory_lock(:lock_key);
SELECT set_config('pggraph.writer_done_key', :'writer_done_key', false);
DO $$
DECLARE
  attempt integer;
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND objid = current_setting('pggraph.writer_done_key')::oid
        AND granted
    ) THEN
      RETURN;
    END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for isolation writer';
END
$$;
SELECT 'after_source=' || count(*) FROM public.graph_gql_isolation_nodes WHERE id = :'node_id';
SELECT 'after_graph=' || count(*)
FROM graph.gql(
  format('MATCH (u:graph_gql_isolation_nodes {id: %L}) RETURN u', :'node_id'),
  hydrate := false
);
SELECT pg_advisory_lock(:reader_ack_key);
SELECT pg_advisory_lock(:writer_done_key);
SELECT pg_advisory_unlock(:writer_done_key);
SELECT pg_advisory_unlock(:lock_key);
SELECT pg_advisory_unlock(:reader_ack_key);
COMMIT;
SELECT 'post_source=' || count(*) FROM public.graph_gql_isolation_nodes WHERE id = :'node_id';
SELECT 'post_graph=' || count(*)
FROM graph.gql(
  format('MATCH (u:graph_gql_isolation_nodes {id: %L}) RETURN u', :'node_id'),
  hydrate := false
);
SQL
  local reader_pid=$!

  wait_for_reader "$lock_key" "$isolation"
  wait "$writer_pid"
  wait "$reader_pid"

  for expected in \
    'before_source=0' \
    'before_graph=0' \
    "after_source=$expected_after" \
    "after_graph=$expected_after" \
    'post_source=1' \
    'post_graph=1'; do
    if ! grep -qx "$expected" "$reader_out"; then
      echo "$isolation reader did not report '$expected':" >&2
      cat "$reader_out" >&2
      return 1
    fi
  done

}

run_level "READ COMMITTED" "read-committed" 1 771001
run_level "REPEATABLE READ" "repeatable-read" 0 771002
run_level "SERIALIZABLE" "serializable" 0 771003

echo "GQL isolation matrix checks passed on database: $DBNAME"
