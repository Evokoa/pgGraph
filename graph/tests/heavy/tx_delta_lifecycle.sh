#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_tx_delta}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-tx-delta.XXXXXX")"

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

cargo pgrx install --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE pg_test" \
  --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

run_sql() {
  local sql="$1"
  psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c "$sql" >/dev/null
}

expect_value() {
  local expected="$1"
  local sql="$2"
  local label="${3:-SQL value check}"
  local out

  out="$(psql -X -q -v ON_ERROR_STOP=1 -tA -d "$DBNAME" -c "$sql")"
  if [[ "$out" != "$expected" ]]; then
    echo "$label expected '$expected' but got '$out' for SQL:"
    echo "$sql"
    exit 1
  fi
}

assert_reverse_count() {
  local expected="$1"
  local label="$2"

  expect_value "$expected" "
    SELECT count(*)::bigint
    FROM graph.gql(
      'MATCH (u:graph_tx_delta_nodes)-[:friend]->(v:graph_tx_delta_nodes) RETURN u, v',
      hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u2'
      AND row #>> '{v,_id,id}' = 'u1';" "$label"
}

wait_for_session_a_overlay() {
  local attempts=30
  local out

  for _ in $(seq 1 "$attempts"); do
    out="$(psql -X -q -v ON_ERROR_STOP=1 -tA -d "$DBNAME" -c \
      "SELECT count(*)::bigint
       FROM pg_locks
       WHERE locktype = 'advisory'
         AND classid = 1918928211
         AND objid = 1735552873;")"
    if [[ "$out" == "1" ]]; then
      return 0
    fi
    sleep 0.2
  done

  echo "Timed out waiting for session A to record its overlay"
  exit 1
}

cat >"$WORKDIR/fixture.sql" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
SET graph.persist_on_build = on;
SET graph.sync_mode = 'trigger';
SET graph.query_freshness = 'apply_pending_sync';
DROP TABLE IF EXISTS public.graph_tx_delta_edges CASCADE;
DROP TABLE IF EXISTS public.graph_tx_delta_nodes CASCADE;
CREATE TABLE public.graph_tx_delta_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    friend_id TEXT REFERENCES public.graph_tx_delta_nodes(id)
);
INSERT INTO public.graph_tx_delta_nodes (id, name, friend_id)
VALUES ('u1', 'Alice', 'u2'), ('u2', 'Bob', NULL);
SELECT graph.add_table(
    'public.graph_tx_delta_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name']
);
SELECT graph.add_edge(
    'public.graph_tx_delta_nodes'::regclass,
    'friend_id',
    'public.graph_tx_delta_nodes'::regclass,
    'id',
    'friend',
    bidirectional := false
);
SELECT * FROM graph.build();
SELECT graph.enable_sync();
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -f "$WORKDIR/fixture.sql" >/dev/null
run_sql "ALTER DATABASE $DBNAME SET graph.sync_mode = 'trigger';"
run_sql "ALTER DATABASE $DBNAME SET graph.query_freshness = 'apply_pending_sync';"

expect_value "t" "SELECT to_regprocedure('graph._test_record_tx_edge(oid,text,oid,text,text,text)') IS NOT NULL;"
assert_reverse_count 0 "initial graph"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
BEGIN;
SELECT graph._test_record_tx_edge(
    'public.graph_tx_delta_nodes'::regclass,
    'u2',
    'public.graph_tx_delta_nodes'::regclass,
    'u1',
    'friend',
    'insert'
);
DO $$
DECLARE
    actual_count bigint;
BEGIN
    SELECT count(*) INTO actual_count
    FROM graph.gql(
        'MATCH (u:graph_tx_delta_nodes)-[:friend]->(v:graph_tx_delta_nodes) RETURN u, v',
        hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u2'
      AND row #>> '{v,_id,id}' = 'u1';
    IF actual_count <> 1 THEN
        RAISE EXCEPTION 'transaction overlay insert expected 1 row, got %', actual_count;
    END IF;
END
$$;
ROLLBACK;
DO $$
DECLARE
    actual_count bigint;
    dirty boolean;
BEGIN
    SELECT tx_delta_dirty INTO dirty FROM graph.status();
    IF dirty THEN
        RAISE EXCEPTION 'rollback left tx_delta_dirty set in same backend';
    END IF;

    SELECT count(*) INTO actual_count
    FROM graph.gql(
        'MATCH (u:graph_tx_delta_nodes)-[:friend]->(v:graph_tx_delta_nodes) RETURN u, v',
        hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u2'
      AND row #>> '{v,_id,id}' = 'u1';
    IF actual_count <> 0 THEN
        RAISE EXCEPTION 'rollback cleanup expected 0 rows in same backend, got %',
            actual_count;
    END IF;
END
$$;
SQL
assert_reverse_count 0 "rollback discard"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
BEGIN;
SELECT graph._test_record_tx_edge(
    'public.graph_tx_delta_nodes'::regclass,
    'u2',
    'public.graph_tx_delta_nodes'::regclass,
    'u1',
    'friend',
    'insert'
);
DO $$
DECLARE
    actual_count bigint;
BEGIN
    SELECT count(*) INTO actual_count
    FROM graph.gql(
        'MATCH (u:graph_tx_delta_nodes)-[:friend]->(v:graph_tx_delta_nodes) RETURN u, v',
        hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u2'
      AND row #>> '{v,_id,id}' = 'u1';
    IF actual_count <> 1 THEN
        RAISE EXCEPTION 'transaction overlay before commit expected 1 row, got %', actual_count;
    END IF;
END
$$;
COMMIT;
DO $$
DECLARE
    actual_count bigint;
    dirty boolean;
BEGIN
    SELECT tx_delta_dirty INTO dirty FROM graph.status();
    IF dirty THEN
        RAISE EXCEPTION 'commit left tx_delta_dirty set in same backend';
    END IF;

    SELECT count(*) INTO actual_count
    FROM graph.gql(
        'MATCH (u:graph_tx_delta_nodes)-[:friend]->(v:graph_tx_delta_nodes) RETURN u, v',
        hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u2'
      AND row #>> '{v,_id,id}' = 'u1';
    IF actual_count <> 0 THEN
        RAISE EXCEPTION 'commit cleanup expected 0 rows in same backend, got %',
            actual_count;
    END IF;
END
$$;
SQL
assert_reverse_count 0 "commit clears backend-local overlay"

cat >"$WORKDIR/session-a.sql" <<'SQL'
BEGIN;
SELECT graph._test_record_tx_edge(
    'public.graph_tx_delta_nodes'::regclass,
    'u2',
    'public.graph_tx_delta_nodes'::regclass,
    'u1',
    'friend',
    'insert'
);
DO $$
DECLARE
    actual_count bigint;
BEGIN
    SELECT count(*) INTO actual_count
    FROM graph.gql(
        'MATCH (u:graph_tx_delta_nodes)-[:friend]->(v:graph_tx_delta_nodes) RETURN u, v',
        hydrate := false
    )
    WHERE row #>> '{u,_id,id}' = 'u2'
      AND row #>> '{v,_id,id}' = 'u1';
    IF actual_count <> 1 THEN
        RAISE EXCEPTION 'session A expected to see its own overlay, got %', actual_count;
    END IF;
END
$$;
SELECT pg_advisory_lock(1918928211, 1735552873);
SELECT pg_sleep(4);
SELECT pg_advisory_unlock(1918928211, 1735552873);
COMMIT;
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -f "$WORKDIR/session-a.sql" \
  >"$WORKDIR/session-a.log" 2>&1 &
session_a_pid=$!
wait_for_session_a_overlay
assert_reverse_count 0 "concurrent backend isolation"
wait "$session_a_pid" || {
  cat "$WORKDIR/session-a.log"
  exit 1
}
assert_reverse_count 0 "post-concurrent commit clear"

run_sql "UPDATE public.graph_tx_delta_nodes SET friend_id = 'u1' WHERE id = 'u2';"
assert_reverse_count 1 "out-of-band source-table sync catch-up"

echo "Transaction delta lifecycle checks passed on database: $DBNAME"
