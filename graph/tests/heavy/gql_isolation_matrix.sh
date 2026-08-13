#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_isolation}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
PERSIST_ON_BUILD="${PERSIST_ON_BUILD:-off}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/pggraph-gql-isolation.XXXXXX")"
ACTIVE_PIDS=()

if [[ "$PERSIST_ON_BUILD" != "on" && "$PERSIST_ON_BUILD" != "off" ]]; then
  echo "PERSIST_ON_BUILD must be 'on' or 'off'" >&2
  exit 2
fi

cleanup() {
  set +u
  local pid
  if [[ ${#ACTIVE_PIDS[@]} -eq 0 ]]; then
    rm -rf "$WORKDIR"
    return
  fi
  for pid in "${ACTIVE_PIDS[@]}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  for pid in "${ACTIVE_PIDS[@]}"; do
    wait "$pid" >/dev/null 2>&1 || true
  done
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

psql -X -v ON_ERROR_STOP=1 -v persist_on_build="$PERSIST_ON_BUILD" \
  -d "$DBNAME" -f "$SCRIPT_DIR/gql_isolation_fixture.sql" >/dev/null

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.sync_mode = 'trigger'" >/dev/null
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.query_freshness = 'apply_pending_sync'" >/dev/null
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.mutable_enabled = on" >/dev/null
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.persist_on_build = '$PERSIST_ON_BUILD'" >/dev/null

wait_for_reader() {
  local lock_key="$1"
  local attempts=50
  local count

  for _ in $(seq 1 "$attempts"); do
    count="$(psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
      "SELECT count(*) FROM pg_locks
       WHERE locktype = 'advisory'
         AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
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
  local reader_out="$WORKDIR/$slug-reader.out"
  local writer_out="$WORKDIR/$slug-writer.out"
  local full_profile=true
  if [[ "$PERSIST_ON_BUILD" == "on" ]]; then
    full_profile=false
  fi

  psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v isolation="$isolation" \
    -v slug="$slug" \
    -v full_profile="$full_profile" \
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
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
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
BEGIN ISOLATION LEVEL :isolation;
SELECT public.graph_test_isolation_apply_profile(:'slug', :'full_profile'::boolean);
SELECT 'writer_tx=ok';
COMMIT;
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
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
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
  ACTIVE_PIDS+=("$writer_pid")

  wait_for_reader "$writer_ready_key" "$isolation writer"

  psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v isolation="$isolation" \
    -v slug="$slug" \
    -v expected_after="$expected_after" \
    -v full_profile="$full_profile" \
    -v lock_key="$lock_key" \
    -v writer_done_key="$writer_done_key" \
    -v reader_ack_key="$reader_ack_key" \
    >"$reader_out" <<'SQL' &
\o /dev/null
SELECT * FROM graph.build(mode := 'mutable_overlay');
\o
BEGIN ISOLATION LEVEL :isolation;
SELECT public.graph_test_isolation_assert_profile(:'slug', false, :'full_profile'::boolean);
SELECT 'before=ok';
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
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
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
SELECT public.graph_test_isolation_assert_profile(
  :'slug',
  :expected_after = 1,
  :'full_profile'::boolean
);
SELECT 'after=ok';
SELECT pg_advisory_lock(:reader_ack_key);
SELECT pg_advisory_lock(:writer_done_key);
SELECT pg_advisory_unlock(:writer_done_key);
SELECT pg_advisory_unlock(:lock_key);
SELECT pg_advisory_unlock(:reader_ack_key);
COMMIT;
SELECT public.graph_test_isolation_assert_profile(:'slug', true, :'full_profile'::boolean);
SELECT 'post=ok';
SQL
  local reader_pid=$!
  ACTIVE_PIDS+=("$reader_pid")

  wait_for_reader "$lock_key" "$isolation"
  wait "$writer_pid"
  wait "$reader_pid"

  if ! grep -q 'writer_tx=ok' "$writer_out"; then
    echo "$isolation writer did not verify transaction-local state and returned values:" >&2
    cat "$writer_out" >&2
    return 1
  fi

  for expected in 'before=ok' 'after=ok' 'post=ok'; do
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

run_direct_identity_snapshot() {
  local reader_ready_key=771101
  local writer_done_key=771102
  local reader_ack_key=771103
  local reader_out="$WORKDIR/direct-identity-snapshot-reader.out"
  local writer_out="$WORKDIR/direct-identity-snapshot-writer.out"

  psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
SELECT graph.reset();
DROP TABLE IF EXISTS public.graph_rls_snapshot_nodes CASCADE;
DROP TABLE IF EXISTS public.graph_rls_snapshot_control CASCADE;
DROP ROLE IF EXISTS graph_rls_snapshot_reader;
CREATE TABLE public.graph_rls_snapshot_nodes (
  id text PRIMARY KEY,
  name text NOT NULL
);
CREATE TABLE public.graph_rls_snapshot_control (
  visible_id text PRIMARY KEY
);
INSERT INTO public.graph_rls_snapshot_nodes VALUES ('u1', 'one'), ('u2', 'two');
INSERT INTO public.graph_rls_snapshot_control VALUES ('u1');
SELECT graph.add_table(
  'public.graph_rls_snapshot_nodes'::regclass,
  id_column := 'id',
  columns := ARRAY['name']
);
SELECT * FROM graph.build();
ALTER TABLE public.graph_rls_snapshot_nodes ENABLE ROW LEVEL SECURITY;
CREATE POLICY graph_rls_snapshot_policy ON public.graph_rls_snapshot_nodes
  USING (id IN (SELECT visible_id FROM public.graph_rls_snapshot_control));
CREATE ROLE graph_rls_snapshot_reader;
GRANT USAGE ON SCHEMA graph, public TO graph_rls_snapshot_reader;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA graph TO graph_rls_snapshot_reader;
GRANT SELECT ON public.graph_rls_snapshot_nodes,
  public.graph_rls_snapshot_control TO graph_rls_snapshot_reader;
SQL

  psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v reader_ready_key="$reader_ready_key" \
    -v writer_done_key="$writer_done_key" \
    -v reader_ack_key="$reader_ack_key" >"$reader_out" <<'SQL' &
\o /dev/null
SELECT * FROM graph.build();
\o
BEGIN ISOLATION LEVEL REPEATABLE READ;
SET ROLE graph_rls_snapshot_reader;
SELECT CASE WHEN (
  SELECT count(*) FROM graph.get_node(
    'default', 'graph_rls_snapshot_nodes', 'u1', hydrate := false
  )
) = 1 THEN 'snapshot_before=ok' ELSE 'snapshot_before=bad' END;
SELECT pg_advisory_lock(:reader_ready_key);
SELECT set_config('pggraph.snapshot_writer_done_key', :'writer_done_key', false);
DO $$
DECLARE attempt integer;
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
        AND objid = current_setting('pggraph.snapshot_writer_done_key')::oid
        AND granted
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for snapshot writer';
END
$$;
SELECT CASE WHEN (
  SELECT count(*) FROM graph.get_node(
    'default', 'graph_rls_snapshot_nodes', 'u1', hydrate := false
  )
) = 1 AND (
  SELECT count(*) FROM graph.get_node(
    'default', 'graph_rls_snapshot_nodes', 'u2', hydrate := false
  )
) = 0 THEN 'snapshot_repeat=ok' ELSE 'snapshot_repeat=bad' END;
SELECT pg_advisory_lock(:reader_ack_key);
SELECT pg_advisory_lock(:writer_done_key);
SELECT pg_advisory_unlock(:writer_done_key);
SELECT pg_advisory_unlock(:reader_ready_key);
SELECT pg_advisory_unlock(:reader_ack_key);
COMMIT;
SELECT CASE WHEN (
  SELECT count(*) FROM graph.get_node(
    'default', 'graph_rls_snapshot_nodes', 'u2', hydrate := false
  )
) = 1 THEN 'snapshot_after=ok' ELSE 'snapshot_after=bad' END;
SQL
  local reader_pid=$!
  ACTIVE_PIDS+=("$reader_pid")

  wait_for_reader "$reader_ready_key" "direct identity snapshot reader"

  psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v writer_done_key="$writer_done_key" \
    -v reader_ack_key="$reader_ack_key" >"$writer_out" <<'SQL' &
UPDATE public.graph_rls_snapshot_control SET visible_id = 'u2';
SELECT pg_advisory_lock(:writer_done_key);
SELECT set_config('pggraph.snapshot_reader_ack_key', :'reader_ack_key', false);
DO $$
DECLARE attempt integer;
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
        AND objid = current_setting('pggraph.snapshot_reader_ack_key')::oid
        AND granted
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for snapshot reader acknowledgement';
END
$$;
SELECT pg_advisory_unlock(:writer_done_key);
SELECT 'snapshot_writer=ok';
SQL
  local writer_pid=$!
  ACTIVE_PIDS+=("$writer_pid")

  wait "$writer_pid"
  wait "$reader_pid"

  for expected in snapshot_before=ok snapshot_repeat=ok snapshot_after=ok; do
    if ! grep -qx "$expected" "$reader_out"; then
      echo "direct identity snapshot reader did not report '$expected':" >&2
      cat "$reader_out" >&2
      return 1
    fi
  done
  if ! grep -qx 'snapshot_writer=ok' "$writer_out"; then
    echo "direct identity snapshot writer did not complete:" >&2
    cat "$writer_out" >&2
    return 1
  fi
}

run_direct_identity_snapshot

echo "GQL isolation matrix checks passed on database: $DBNAME"
