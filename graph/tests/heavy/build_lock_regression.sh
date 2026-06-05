#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_build_lock}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-build-lock.XXXXXX")"

LOCK_PID=""

terminate_lock_holder() {
  local lock_pids
  lock_pids="$(psql -X -qAt -d "$DBNAME" <<'SQL' 2>/dev/null || true
SELECT pid
FROM pg_locks
WHERE locktype = 'advisory'
  AND classid = 1918928211
  AND objid = 1735552871
  AND objsubid = 2
  AND pid <> pg_backend_pid();
SQL
)"
  while IFS= read -r lock_pid; do
    if [[ -n "$lock_pid" ]]; then
      psql -X -qAt -d "$DBNAME" -c "SELECT pg_terminate_backend($lock_pid)" >/dev/null 2>&1 || true
    fi
  done <<<"$lock_pids"

  if [[ -n "$LOCK_PID" ]]; then
    kill "$LOCK_PID" >/dev/null 2>&1 || true
    wait "$LOCK_PID" >/dev/null 2>&1 || true
    LOCK_PID=""
  fi
}

cleanup() {
  terminate_lock_holder
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

cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
SET graph.auto_load = off;
SET graph.persist_on_build = off;
CREATE TABLE public.graph_lock_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE public.graph_lock_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_lock_nodes(id),
    to_id TEXT NOT NULL REFERENCES public.graph_lock_nodes(id)
);
INSERT INTO public.graph_lock_nodes (id, name)
VALUES ('a', 'alpha'), ('b', 'beta');
INSERT INTO public.graph_lock_edges (from_id, to_id)
VALUES ('a', 'b');
SELECT graph.add_table('public.graph_lock_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge(
    'public.graph_lock_edges'::regclass,
    'from_id',
    'public.graph_lock_nodes'::regclass,
    'id',
    'linked',
    false
);
SQL

psql -X -v ON_ERROR_STOP=1 "$DBNAME" >"$WORKDIR/lock-holder.log" 2>&1 <<'SQL' &
SELECT pg_advisory_lock(1918928211, 1735552871);
SELECT pg_sleep(300);
SELECT pg_advisory_unlock(1918928211, 1735552871);
SQL
LOCK_PID=$!

for _ in $(seq 1 100); do
  if psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL' | grep -qx "held"
WITH attempted AS (
    SELECT pg_try_advisory_lock(1918928211, 1735552871) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, 1735552871)::text
    ELSE 'held'
END
FROM attempted;
SQL
  then
    break
  fi
  sleep 0.1
done

lock_held="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
WITH attempted AS (
    SELECT pg_try_advisory_lock(1918928211, 1735552871) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, 1735552871)::text
    ELSE 'held'
END
FROM attempted;
SQL
)"
if [[ "$lock_held" != "held" ]]; then
  echo "build/vacuum advisory lock was not held by the simulated session"
  cat "$WORKDIR/lock-holder.log"
  exit 1
fi

set +e
psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET statement_timeout = '5s'; SELECT * FROM graph.build();" \
  >"$WORKDIR/build-while-locked.log" 2>&1
build_status=$?
set -e

if [[ "$build_status" -eq 0 ]]; then
  echo "graph.build() succeeded while the build/vacuum advisory lock was held"
  cat "$WORKDIR/build-while-locked.log"
  exit 1
fi

if ! grep -q "SQLSTATE: PG006" "$WORKDIR/build-while-locked.log"; then
  echo "graph.build() did not report PG006 while the advisory lock was held"
  cat "$WORKDIR/build-while-locked.log"
  exit 1
fi

if ! grep -q "Another build() or vacuum() is already running" "$WORKDIR/build-while-locked.log"; then
  echo "graph.build() did not report the BuildLocked message"
  cat "$WORKDIR/build-while-locked.log"
  exit 1
fi

terminate_lock_holder

released="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
WITH attempted AS (
    SELECT pg_try_advisory_lock(1918928211, 1735552871) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, 1735552871)::text
    ELSE 'held'
END
FROM attempted;
SQL
)"
if [[ "$released" == "held" ]]; then
  echo "simulated build/vacuum advisory lock was not released"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.persist_on_build = on; SELECT * FROM graph.build();" >/dev/null
graph_path="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT current_setting('data_directory') || '/' || COALESCE(NULLIF(current_setting('graph.data_dir', true), ''), 'graph') || '/main.pggraph'")"
if [[ ! -f "$graph_path" && -f "/tmp/graph/main.pggraph" ]]; then
  graph_path="/tmp/graph/main.pggraph"
fi
graph_tmp_path="${graph_path}.tmp"
if [[ ! -f "$graph_path" ]]; then
  echo "expected persisted graph artifact at $graph_path"
  exit 1
fi
artifact_before="$(cksum "$graph_path")"
traverse_before="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT count(*) FROM graph.traverse('public.graph_lock_nodes'::regclass, 'a', 1, edge_types := ARRAY['linked'], direction := 'out', hydrate := false)")"

psql -X -v ON_ERROR_STOP=1 "$DBNAME" >"$WORKDIR/artifact-lock-holder.log" 2>&1 <<'SQL' &
SELECT pg_advisory_lock(1918928211, 1735552871);
SELECT pg_sleep(300);
SELECT pg_advisory_unlock(1918928211, 1735552871);
SQL
LOCK_PID=$!

for _ in $(seq 1 100); do
  if psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL' | grep -qx "held"
WITH attempted AS (
    SELECT pg_try_advisory_lock(1918928211, 1735552871) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, 1735552871)::text
    ELSE 'held'
END
FROM attempted;
SQL
  then
    break
  fi
  sleep 0.1
done

set +e
psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.persist_on_build = on; SET statement_timeout = '5s'; SELECT * FROM graph.build();" \
  >"$WORKDIR/artifact-build-while-locked.log" 2>&1
artifact_build_status=$?
set -e
if [[ "$artifact_build_status" -eq 0 ]]; then
  echo "graph.build() succeeded during artifact preservation lock test"
  cat "$WORKDIR/artifact-build-while-locked.log"
  exit 1
fi
if ! grep -q "SQLSTATE: PG006" "$WORKDIR/artifact-build-while-locked.log"; then
  echo "artifact preservation lock test did not report PG006"
  cat "$WORKDIR/artifact-build-while-locked.log"
  exit 1
fi
artifact_after="$(cksum "$graph_path")"
if [[ "$artifact_after" != "$artifact_before" ]]; then
  echo "persisted graph artifact changed after failed lock attempt"
  echo "before: $artifact_before"
  echo "after:  $artifact_after"
  exit 1
fi
if [[ -e "$graph_tmp_path" ]]; then
  echo "temporary graph artifact remained after failed lock attempt: $graph_tmp_path"
  exit 1
fi
traverse_after="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT count(*) FROM graph.traverse('public.graph_lock_nodes'::regclass, 'a', 1, edge_types := ARRAY['linked'], direction := 'out', hydrate := false)")"
if [[ "$traverse_after" != "$traverse_before" ]]; then
  echo "graph query result changed after failed lock attempt"
  echo "before: $traverse_before"
  echo "after:  $traverse_after"
  exit 1
fi
terminate_lock_holder

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
SELECT graph.reset();
SET graph.auto_load = off;
SET graph.persist_on_build = off;
SET graph.sync_mode = manual;
CREATE TABLE public.graph_lock_slow_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
INSERT INTO public.graph_lock_slow_nodes (id, name)
SELECT i::text, 'node-' || i::text
FROM generate_series(1, 200) AS i;
CREATE OR REPLACE FUNCTION public.graph_lock_pause(value TEXT)
RETURNS TEXT
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM pg_sleep(0.02);
    RETURN value;
END
$$;
CREATE OR REPLACE VIEW public.graph_lock_slow_nodes_view AS
SELECT public.graph_lock_pause(id) AS id, name
FROM public.graph_lock_slow_nodes;
SELECT graph.add_table('public.graph_lock_slow_nodes'::regclass, 'id', ARRAY['name']);
UPDATE graph._registered_tables
SET table_name = 'public.graph_lock_slow_nodes_view'
WHERE table_name IN ('graph_lock_slow_nodes', 'public.graph_lock_slow_nodes');
SQL

psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.sync_mode = 'manual'; SET graph.persist_on_build = on; SET statement_timeout = '30s'; SELECT * FROM graph.build();" \
  >"$WORKDIR/concurrent-owner.log" 2>&1 &
OWNER_PID=$!

for _ in $(seq 1 100); do
  if psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL' | grep -qx "held"
WITH attempted AS (
    SELECT pg_try_advisory_lock(1918928211, 1735552871) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, 1735552871)::text
    ELSE 'held'
END
FROM attempted;
SQL
  then
    break
  fi
  sleep 0.1
done

owner_holds_lock="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
WITH attempted AS (
    SELECT pg_try_advisory_lock(1918928211, 1735552871) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, 1735552871)::text
    ELSE 'held'
END
FROM attempted;
SQL
)"
if [[ "$owner_holds_lock" != "held" ]]; then
  echo "owner graph.build() did not acquire the build/vacuum advisory lock"
  cat "$WORKDIR/concurrent-owner.log"
  exit 1
fi

failure_count=0
for idx in 1 2 3; do
  set +e
  psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
    -c "SET statement_timeout = '5s'; SELECT * FROM graph.build();" \
    >"$WORKDIR/concurrent-contender-$idx.log" 2>&1
  contender_status=$?
  set -e

  if [[ "$contender_status" -eq 0 ]]; then
    echo "concurrent graph.build() contender $idx succeeded while owner held the lock"
    cat "$WORKDIR/concurrent-contender-$idx.log"
    exit 1
  fi
  if ! grep -q "SQLSTATE: PG006" "$WORKDIR/concurrent-contender-$idx.log"; then
    echo "concurrent graph.build() contender $idx did not report PG006"
    cat "$WORKDIR/concurrent-contender-$idx.log"
    exit 1
  fi
  if ! grep -q "Another build() or vacuum() is already running" "$WORKDIR/concurrent-contender-$idx.log"; then
    echo "concurrent graph.build() contender $idx did not report the BuildLocked message"
    cat "$WORKDIR/concurrent-contender-$idx.log"
    exit 1
  fi
  failure_count=$((failure_count + 1))
done

set +e
psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET statement_timeout = '5s'; SELECT * FROM graph.vacuum();" \
  >"$WORKDIR/concurrent-vacuum.log" 2>&1
vacuum_status=$?
set -e
if [[ "$vacuum_status" -eq 0 ]]; then
  echo "concurrent graph.vacuum() succeeded while graph.build() held the lock"
  cat "$WORKDIR/concurrent-vacuum.log"
  exit 1
fi
if ! grep -q "SQLSTATE: PG006" "$WORKDIR/concurrent-vacuum.log"; then
  echo "concurrent graph.vacuum() did not report PG006"
  cat "$WORKDIR/concurrent-vacuum.log"
  exit 1
fi
if ! grep -q "Another build() or vacuum() is already running" "$WORKDIR/concurrent-vacuum.log"; then
  echo "concurrent graph.vacuum() did not report the BuildLocked message"
  cat "$WORKDIR/concurrent-vacuum.log"
  exit 1
fi

set +e
wait "$OWNER_PID"
owner_status=$?
set -e
if [[ "$owner_status" -ne 0 ]]; then
  echo "owner graph.build() failed"
  cat "$WORKDIR/concurrent-owner.log"
  exit 1
fi
if [[ "$failure_count" -ne 3 ]]; then
  echo "expected 3 concurrent graph.build() PG006 failures, saw $failure_count"
  exit 1
fi
if [[ -e "$graph_tmp_path" ]]; then
  echo "temporary graph artifact remained after concurrent owner build: $graph_tmp_path"
  exit 1
fi

echo "Build advisory lock regression passed for $DBNAME"
