#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_write_recheck}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

cd "$GRAPH_DIR"

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
DROP TABLE IF EXISTS public.graph_gql_write_recheck_nodes CASCADE;
CREATE TABLE public.graph_gql_write_recheck_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    age INT NOT NULL
);
INSERT INTO public.graph_gql_write_recheck_nodes (id, name, age)
VALUES ('u1', 'Alice', 37), ('u2', 'Bob', 41);
SELECT graph.add_table(
    'public.graph_gql_write_recheck_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name', 'age']
);
SELECT graph.add_filter_column('public.graph_gql_write_recheck_nodes'::regclass, 'age');
SELECT * FROM graph.build(mode := 'mutable_overlay');
SQL

tmpdir="$(mktemp -d)"
ACTIVE_PIDS=()

cleanup() {
  local pid
  for pid in "${ACTIVE_PIDS[@]}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  for pid in "${ACTIVE_PIDS[@]}"; do
    wait "$pid" >/dev/null 2>&1 || true
  done
  rm -rf "$tmpdir"
}
trap cleanup EXIT

wait_for_lock() {
  local lock_key="$1"
  local description="$2"
  local count

  for _ in $(seq 1 100); do
    count="$(psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
      "SELECT count(*) FROM pg_locks
       WHERE locktype = 'advisory'
         AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
         AND objid = $lock_key
         AND granted")"
    if [[ "$count" != "0" ]]; then
      return 0
    fi
    sleep 0.1
  done

  echo "timed out waiting for $description" >&2
  return 1
}

set +e
PGAPPNAME=pggraph-recheck-set psql -X -v ON_ERROR_STOP=1 -v VERBOSITY=sqlstate \
  -d "$DBNAME" >"$tmpdir/writer.out" 2>"$tmpdir/writer.err" <<'SQL' &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(774001);
DO $$
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
        AND objid = 774002
        AND granted
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for SET locker';
END
$$;
\set ON_ERROR_STOP off
SELECT graph.gql(
    'MATCH (u:graph_gql_write_recheck_nodes {id: ''u2''})
     WHERE u.age = 41
     SET u.age = 101
     RETURN u.age'
);
\set ON_ERROR_STOP on
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
writer_pid=$!
ACTIVE_PIDS+=("$writer_pid")
set -e

wait_for_lock 774001 "SET writer"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/locker.out" &
BEGIN;
UPDATE public.graph_gql_write_recheck_nodes
SET age = 99
WHERE id = 'u2';
SELECT pg_advisory_lock(774002);
DO $$
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_stat_activity
      WHERE datname = current_database()
        AND application_name = 'pggraph-recheck-set'
        AND state = 'active'
        AND wait_event_type = 'Lock'
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for blocked SET writer';
END
$$;
COMMIT;
SQL
locker_pid=$!
ACTIVE_PIDS+=("$locker_pid")

set +e
wait "$writer_pid"
writer_status=$?
set -e

wait "$locker_pid"

if [[ "$writer_status" -ne 0 ]]; then
  echo "GQL SET stale predicate client could not continue after statement rollback" >&2
  cat "$tmpdir/writer.out" >&2
  cat "$tmpdir/writer.err" >&2
  exit 1
fi

if [[ "$(grep -c '^ERROR:  ' "$tmpdir/writer.err" || true)" != "1" ]] ||
   ! grep -qx "ERROR:  22000" "$tmpdir/writer.err"; then
  echo "GQL SET stale predicate re-check did not expose SQLSTATE 22000" >&2
  cat "$tmpdir/writer.err" >&2
  exit 1
fi
if ! grep -q "tx_dirty=false" "$tmpdir/writer.out"; then
  echo "GQL SET stale predicate re-check retained transaction-local graph state" >&2
  cat "$tmpdir/writer.out" >&2
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
DO $$
DECLARE
    source_age integer;
BEGIN
    SELECT age INTO source_age
    FROM public.graph_gql_write_recheck_nodes
    WHERE id = 'u2';

    IF source_age <> 99 THEN
        RAISE EXCEPTION 'GQL SET stale predicate re-check expected concurrent age 99, got %',
            source_age;
    END IF;
END
$$;
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
SELECT graph.reset();
SET graph.mutable_enabled = on;
SET graph.enforce_tenant_scope = on;
SET graph.tenant_setting = 'app.graph_gql_write_recheck_tenant';
DROP TABLE IF EXISTS public.graph_gql_write_recheck_tenant_nodes CASCADE;
CREATE TABLE public.graph_gql_write_recheck_tenant_nodes (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL
);
INSERT INTO public.graph_gql_write_recheck_tenant_nodes (id, tenant_id, name)
VALUES ('u1', 'tenant-a', 'Alice');
SELECT graph.add_table(
    'public.graph_gql_write_recheck_tenant_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['tenant_id', 'name'],
    tenant_column := 'tenant_id'
);
SET app.graph_gql_write_recheck_tenant = 'tenant-a';
SELECT * FROM graph.build(mode := 'mutable_overlay');
RESET app.graph_gql_write_recheck_tenant;
RESET graph.tenant_setting;
SET graph.enforce_tenant_scope = off;
SQL

set +e
PGAPPNAME=pggraph-recheck-tenant psql -X -v ON_ERROR_STOP=1 -v VERBOSITY=sqlstate \
  -d "$DBNAME" >"$tmpdir/tenant_writer.out" 2>"$tmpdir/tenant_writer.err" <<'SQL' &
SET graph.mutable_enabled = on;
SET graph.enforce_tenant_scope = on;
SET graph.tenant_setting = 'app.graph_gql_write_recheck_tenant';
SET app.graph_gql_write_recheck_tenant = 'tenant-a';
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(774011);
DO $$
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
        AND objid = 774012
        AND granted
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for tenant locker';
END
$$;
\set ON_ERROR_STOP off
SELECT graph.gql(
    'MATCH (u:graph_gql_write_recheck_tenant_nodes {id: ''u1''})
     SET u.name = ''Updated''
     RETURN u.name'
);
\set ON_ERROR_STOP on
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
tenant_writer_pid=$!
ACTIVE_PIDS+=("$tenant_writer_pid")
set -e

wait_for_lock 774011 "tenant SET writer"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/tenant_locker.out" &
BEGIN;
UPDATE public.graph_gql_write_recheck_tenant_nodes
SET tenant_id = 'tenant-b'
WHERE id = 'u1';
SELECT pg_advisory_lock(774012);
DO $$
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_stat_activity
      WHERE datname = current_database()
        AND application_name = 'pggraph-recheck-tenant'
        AND state = 'active'
        AND wait_event_type = 'Lock'
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for blocked tenant SET writer';
END
$$;
COMMIT;
SQL
tenant_locker_pid=$!
ACTIVE_PIDS+=("$tenant_locker_pid")

set +e
wait "$tenant_writer_pid"
tenant_writer_status=$?
set -e

wait "$tenant_locker_pid"

if [[ "$tenant_writer_status" -ne 0 ]]; then
  echo "GQL SET stale tenant client could not continue after statement rollback" >&2
  cat "$tmpdir/tenant_writer.out" >&2
  cat "$tmpdir/tenant_writer.err" >&2
  exit 1
fi

if [[ "$(grep -c '^ERROR:  ' "$tmpdir/tenant_writer.err" || true)" != "1" ]] ||
   ! grep -qx "ERROR:  22000" "$tmpdir/tenant_writer.err"; then
  echo "GQL SET stale tenant re-check did not expose SQLSTATE 22000" >&2
  cat "$tmpdir/tenant_writer.err" >&2
  exit 1
fi
if ! grep -q "tx_dirty=false" "$tmpdir/tenant_writer.out"; then
  echo "GQL SET stale tenant re-check retained transaction-local graph state" >&2
  cat "$tmpdir/tenant_writer.out" >&2
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
DO $$
DECLARE
    tenant text;
    source_name text;
BEGIN
    SELECT tenant_id, name INTO tenant, source_name
    FROM public.graph_gql_write_recheck_tenant_nodes
    WHERE id = 'u1';

    IF tenant <> 'tenant-b' OR source_name <> 'Alice' THEN
        RAISE EXCEPTION 'GQL SET stale tenant re-check expected tenant-b/Alice, got %/%',
            tenant, source_name;
    END IF;
END
$$;
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
SELECT graph.reset();
SET graph.mutable_enabled = on;
SET graph.enforce_tenant_scope = off;
RESET graph.tenant_setting;
DROP TABLE IF EXISTS public.graph_gql_write_recheck_remove_nodes CASCADE;
CREATE TABLE public.graph_gql_write_recheck_remove_nodes (
    id TEXT PRIMARY KEY,
    age INT NOT NULL,
    status TEXT
);
INSERT INTO public.graph_gql_write_recheck_remove_nodes (id, age, status)
VALUES ('u1', 37, 'active'), ('u2', 41, 'active');
SELECT graph.add_table(
    'public.graph_gql_write_recheck_remove_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['age', 'status']
);
SELECT graph.add_filter_column('public.graph_gql_write_recheck_remove_nodes'::regclass, 'age');
SELECT * FROM graph.build(mode := 'mutable_overlay');
SQL

set +e
PGAPPNAME=pggraph-recheck-remove psql -X -v ON_ERROR_STOP=1 -v VERBOSITY=sqlstate \
  -d "$DBNAME" >"$tmpdir/remove_writer.out" 2>"$tmpdir/remove_writer.err" <<'SQL' &
SET graph.mutable_enabled = on;
SET graph.enforce_tenant_scope = off;
RESET graph.tenant_setting;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(774021);
DO $$
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
        AND objid = 774022
        AND granted
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for REMOVE locker';
END
$$;
\set ON_ERROR_STOP off
SELECT graph.gql(
    'MATCH (u:graph_gql_write_recheck_remove_nodes {id: ''u2''})
     WHERE u.age = 41
     REMOVE u.status
     RETURN u.status'
);
\set ON_ERROR_STOP on
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
remove_writer_pid=$!
ACTIVE_PIDS+=("$remove_writer_pid")
set -e

wait_for_lock 774021 "REMOVE writer"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/remove_locker.out" &
BEGIN;
UPDATE public.graph_gql_write_recheck_remove_nodes
SET age = 99
WHERE id = 'u2';
SELECT pg_advisory_lock(774022);
DO $$
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_stat_activity
      WHERE datname = current_database()
        AND application_name = 'pggraph-recheck-remove'
        AND state = 'active'
        AND wait_event_type = 'Lock'
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for blocked REMOVE writer';
END
$$;
COMMIT;
SQL
remove_locker_pid=$!
ACTIVE_PIDS+=("$remove_locker_pid")

set +e
wait "$remove_writer_pid"
remove_writer_status=$?
set -e

wait "$remove_locker_pid"

if [[ "$remove_writer_status" -ne 0 ]]; then
  echo "GQL REMOVE stale predicate client could not continue after statement rollback" >&2
  cat "$tmpdir/remove_writer.out" >&2
  cat "$tmpdir/remove_writer.err" >&2
  exit 1
fi

if [[ "$(grep -c '^ERROR:  ' "$tmpdir/remove_writer.err" || true)" != "1" ]] ||
   ! grep -qx "ERROR:  22000" "$tmpdir/remove_writer.err"; then
  echo "GQL REMOVE stale predicate re-check did not expose SQLSTATE 22000" >&2
  cat "$tmpdir/remove_writer.err" >&2
  exit 1
fi
if ! grep -q "tx_dirty=false" "$tmpdir/remove_writer.out"; then
  echo "GQL REMOVE stale predicate re-check retained transaction-local graph state" >&2
  cat "$tmpdir/remove_writer.out" >&2
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
DO $$
DECLARE
    source_age integer;
    source_status text;
BEGIN
    SELECT age, status INTO source_age, source_status
    FROM public.graph_gql_write_recheck_remove_nodes
    WHERE id = 'u2';

    IF source_age <> 99 OR source_status <> 'active' THEN
        RAISE EXCEPTION 'GQL REMOVE stale predicate re-check expected 99/active, got %/%',
            source_age, source_status;
    END IF;
END
$$;
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
SELECT graph.reset();
SET graph.mutable_enabled = on;
SET graph.enforce_tenant_scope = off;
RESET graph.tenant_setting;
DROP TABLE IF EXISTS public.graph_gql_write_recheck_detach_edges CASCADE;
DROP TABLE IF EXISTS public.graph_gql_write_recheck_detach_nodes CASCADE;
CREATE TABLE public.graph_gql_write_recheck_detach_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE public.graph_gql_write_recheck_detach_edges (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES public.graph_gql_write_recheck_detach_nodes(id),
    target_id TEXT NOT NULL REFERENCES public.graph_gql_write_recheck_detach_nodes(id)
);
INSERT INTO public.graph_gql_write_recheck_detach_nodes (id, name)
VALUES ('u1', 'Alice'), ('u2', 'Bob');
INSERT INTO public.graph_gql_write_recheck_detach_edges (id, source_id, target_id)
VALUES ('e1', 'u1', 'u2');
SELECT graph.add_table(
    'public.graph_gql_write_recheck_detach_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name']
);
SELECT graph.add_edge(
    'public.graph_gql_write_recheck_detach_edges'::regclass,
    'source_id',
    'public.graph_gql_write_recheck_detach_nodes'::regclass,
    'target_id',
    'friend'
);
SELECT * FROM graph.build(mode := 'mutable_overlay');
SQL

set +e
PGAPPNAME=pggraph-recheck-detach psql -X -v ON_ERROR_STOP=1 -v VERBOSITY=sqlstate \
  -d "$DBNAME" >"$tmpdir/detach_writer.out" 2>"$tmpdir/detach_writer.err" <<'SQL' &
SET graph.mutable_enabled = on;
SET graph.enforce_tenant_scope = off;
RESET graph.tenant_setting;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(774031);
DO $$
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory'
        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
        AND objid = 774032
        AND granted
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for DETACH DELETE locker';
END
$$;
\set ON_ERROR_STOP off
SELECT graph.gql(
    'MATCH (u:graph_gql_write_recheck_detach_nodes {id: ''u1''})
     WHERE u.name = ''Alice''
     DETACH DELETE u
     RETURN u.name'
);
\set ON_ERROR_STOP on
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
detach_writer_pid=$!
ACTIVE_PIDS+=("$detach_writer_pid")
set -e

wait_for_lock 774031 "DETACH DELETE writer"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/detach_locker.out" &
BEGIN;
UPDATE public.graph_gql_write_recheck_detach_nodes
SET name = 'Moved'
WHERE id = 'u1';
SELECT pg_advisory_lock(774032);
DO $$
BEGIN
  FOR attempt IN 1..100 LOOP
    IF EXISTS (
      SELECT 1 FROM pg_stat_activity
      WHERE datname = current_database()
        AND application_name = 'pggraph-recheck-detach'
        AND state = 'active'
        AND wait_event_type = 'Lock'
    ) THEN RETURN; END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;
  RAISE EXCEPTION 'timed out waiting for blocked DETACH DELETE writer';
END
$$;
COMMIT;
SQL
detach_locker_pid=$!
ACTIVE_PIDS+=("$detach_locker_pid")

set +e
wait "$detach_writer_pid"
detach_writer_status=$?
set -e

wait "$detach_locker_pid"

if [[ "$detach_writer_status" -ne 0 ]]; then
  echo "GQL DETACH DELETE stale predicate client could not continue after statement rollback" >&2
  cat "$tmpdir/detach_writer.out" >&2
  cat "$tmpdir/detach_writer.err" >&2
  exit 1
fi

if [[ "$(grep -c '^ERROR:  ' "$tmpdir/detach_writer.err" || true)" != "1" ]] ||
   ! grep -qx "ERROR:  22000" "$tmpdir/detach_writer.err"; then
  echo "GQL DETACH DELETE stale predicate re-check did not expose SQLSTATE 22000" >&2
  cat "$tmpdir/detach_writer.err" >&2
  exit 1
fi
if ! grep -q "tx_dirty=false" "$tmpdir/detach_writer.out"; then
  echo "GQL DETACH DELETE stale predicate re-check retained transaction-local graph state" >&2
  cat "$tmpdir/detach_writer.out" >&2
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
DO $$
DECLARE
    source_name text;
    node_count bigint;
    edge_count bigint;
BEGIN
    SELECT name INTO source_name
    FROM public.graph_gql_write_recheck_detach_nodes
    WHERE id = 'u1';
    SELECT count(*) INTO node_count
    FROM public.graph_gql_write_recheck_detach_nodes;
    SELECT count(*) INTO edge_count
    FROM public.graph_gql_write_recheck_detach_edges;

    IF source_name <> 'Moved' OR node_count <> 2 OR edge_count <> 1 THEN
        RAISE EXCEPTION 'GQL DETACH DELETE stale predicate re-check expected Moved/2/1, got %/%/%',
            source_name, node_count, edge_count;
    END IF;
END
$$;
SQL

echo "GQL write predicate re-check race checks passed on database: $DBNAME"
