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
trap 'rm -rf "$tmpdir"' EXIT

set +e
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" >"$tmpdir/writer.out" 2>"$tmpdir/writer.err" <<'SQL' &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_sleep(2);
SELECT graph.gql(
    'MATCH (u:graph_gql_write_recheck_nodes {id: ''u2''})
     WHERE u.age = 41
     SET u.age = 101
     RETURN u.age'
);
SQL
writer_pid=$!
set -e

sleep 1

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/locker.out" &
BEGIN;
UPDATE public.graph_gql_write_recheck_nodes
SET age = 99
WHERE id = 'u2';
SELECT pg_sleep(4);
COMMIT;
SQL
locker_pid=$!

set +e
wait "$writer_pid"
writer_status=$?
set -e

wait "$locker_pid"

if [[ "$writer_status" -eq 0 ]]; then
  echo "GQL SET stale predicate re-check unexpectedly succeeded" >&2
  cat "$tmpdir/writer.out" >&2
  exit 1
fi

if ! grep -q "no longer satisfies the matched predicate" "$tmpdir/writer.err"; then
  echo "GQL SET stale predicate re-check failed with an unexpected error" >&2
  cat "$tmpdir/writer.err" >&2
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
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" >"$tmpdir/tenant_writer.out" 2>"$tmpdir/tenant_writer.err" <<'SQL' &
SET graph.mutable_enabled = on;
SET graph.enforce_tenant_scope = on;
SET graph.tenant_setting = 'app.graph_gql_write_recheck_tenant';
SET app.graph_gql_write_recheck_tenant = 'tenant-a';
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_sleep(2);
SELECT graph.gql(
    'MATCH (u:graph_gql_write_recheck_tenant_nodes {id: ''u1''})
     SET u.name = ''Updated''
     RETURN u.name'
);
SQL
tenant_writer_pid=$!
set -e

sleep 1

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/tenant_locker.out" &
BEGIN;
UPDATE public.graph_gql_write_recheck_tenant_nodes
SET tenant_id = 'tenant-b'
WHERE id = 'u1';
SELECT pg_sleep(4);
COMMIT;
SQL
tenant_locker_pid=$!

set +e
wait "$tenant_writer_pid"
tenant_writer_status=$?
set -e

wait "$tenant_locker_pid"

if [[ "$tenant_writer_status" -eq 0 ]]; then
  echo "GQL SET stale tenant re-check unexpectedly succeeded" >&2
  cat "$tmpdir/tenant_writer.out" >&2
  exit 1
fi

if ! grep -q "active tenant scope" "$tmpdir/tenant_writer.err"; then
  echo "GQL SET stale tenant re-check failed with an unexpected error" >&2
  cat "$tmpdir/tenant_writer.err" >&2
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
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" >"$tmpdir/detach_writer.out" 2>"$tmpdir/detach_writer.err" <<'SQL' &
SET graph.mutable_enabled = on;
SET graph.enforce_tenant_scope = off;
RESET graph.tenant_setting;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_sleep(2);
SELECT graph.gql(
    'MATCH (u:graph_gql_write_recheck_detach_nodes {id: ''u1''})
     WHERE u.name = ''Alice''
     DETACH DELETE u
     RETURN u.name'
);
SQL
detach_writer_pid=$!
set -e

sleep 1

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/detach_locker.out" &
BEGIN;
UPDATE public.graph_gql_write_recheck_detach_nodes
SET name = 'Moved'
WHERE id = 'u1';
SELECT pg_sleep(4);
COMMIT;
SQL
detach_locker_pid=$!

set +e
wait "$detach_writer_pid"
detach_writer_status=$?
set -e

wait "$detach_locker_pid"

if [[ "$detach_writer_status" -eq 0 ]]; then
  echo "GQL DETACH DELETE stale predicate re-check unexpectedly succeeded" >&2
  cat "$tmpdir/detach_writer.out" >&2
  exit 1
fi

if ! grep -q "no longer satisfies the matched predicate" "$tmpdir/detach_writer.err"; then
  echo "GQL DETACH DELETE stale predicate re-check failed with an unexpected error" >&2
  cat "$tmpdir/detach_writer.err" >&2
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
