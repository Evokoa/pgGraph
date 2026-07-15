#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_relation_identity_ddl}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-relation-ddl.XXXXXX")"

HOLDER_PID=""
DDL_PID=""
ACTIVE_HOLDER_KEY_ONE=""
ACTIVE_HOLDER_KEY_TWO=""

terminate_client_process() {
  local pid="$1"
  local state

  kill "$pid" >/dev/null 2>&1 || true
  for _ in $(seq 1 50); do
    state="$(ps -o stat= -p "$pid" 2>/dev/null || true)"
    if [[ -z "$state" || "$state" == Z* ]]; then
      break
    fi
    sleep 0.1
  done
  state="$(ps -o stat= -p "$pid" 2>/dev/null || true)"
  if [[ -n "$state" && "$state" != Z* ]]; then
    kill -KILL "$pid" >/dev/null 2>&1 || true
  fi
  wait "$pid" >/dev/null 2>&1 || true
}

terminate_advisory_holder() {
  local key_one="$1"
  local key_two="$2"
  local backend_pids

  backend_pids="$(psql -X -qAt -d "$DBNAME" -v key_one="$key_one" -v key_two="$key_two" <<'SQL' 2>/dev/null || true
SELECT pid
FROM pg_locks
WHERE locktype = 'advisory'
  AND classid = :'key_one'::integer
  AND objid = :'key_two'::integer
  AND objsubid = 2
  AND granted
  AND pid <> pg_backend_pid();
SQL
)"
  while IFS= read -r backend_pid; do
    if [[ -n "$backend_pid" ]]; then
      psql -X -qAt -d "$DBNAME" \
        -c "SELECT pg_terminate_backend($backend_pid)" >/dev/null 2>&1 || true
    fi
  done <<<"$backend_pids"

  if [[ -n "$HOLDER_PID" && "$ACTIVE_HOLDER_KEY_ONE" == "$key_one" && \
    "$ACTIVE_HOLDER_KEY_TWO" == "$key_two" ]]; then
    terminate_client_process "$HOLDER_PID"
    HOLDER_PID=""
    ACTIVE_HOLDER_KEY_ONE=""
    ACTIVE_HOLDER_KEY_TWO=""
  fi
}

cleanup() {
  if [[ -n "$ACTIVE_HOLDER_KEY_ONE" && -n "$ACTIVE_HOLDER_KEY_TWO" ]]; then
    terminate_advisory_holder "$ACTIVE_HOLDER_KEY_ONE" "$ACTIVE_HOLDER_KEY_TWO"
  elif [[ -n "$HOLDER_PID" ]]; then
    terminate_client_process "$HOLDER_PID"
  fi
  if [[ -n "$DDL_PID" ]]; then
    terminate_client_process "$DDL_PID"
  fi
  dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

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

wait_for_advisory_holder() {
  local key_one="$1"
  local key_two="$2"

  for _ in $(seq 1 100); do
    if psql -X -qAt -v ON_ERROR_STOP=1 -d "$DBNAME" \
      -v key_one="$key_one" -v key_two="$key_two" <<'SQL' | grep -qx held
WITH attempted AS (
    SELECT pg_try_advisory_lock(:'key_one'::integer, :'key_two'::integer) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(:'key_one'::integer, :'key_two'::integer)::text
    ELSE 'held'
END
FROM attempted;
SQL
    then
      return 0
    fi
    sleep 0.1
  done

  echo "advisory coordination holder did not become ready" >&2
  return 1
}

start_advisory_holder() {
  local key_one="$1"
  local key_two="$2"
  local log_path="$3"

  psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v key_one="$key_one" -v key_two="$key_two" >"$log_path" 2>&1 <<'SQL' &
SELECT pg_advisory_lock(:'key_one'::integer, :'key_two'::integer);
SELECT pg_sleep(300);
SQL
  HOLDER_PID=$!
  ACTIVE_HOLDER_KEY_ONE="$key_one"
  ACTIVE_HOLDER_KEY_TWO="$key_two"
  wait_for_advisory_holder "$key_one" "$key_two"
}

wait_for_relation_lock() {
  local relation_oid="$1"

  for _ in $(seq 1 100); do
    if [[ "$(psql -X -qAt -v ON_ERROR_STOP=1 -d "$DBNAME" \
      -v relation_oid="$relation_oid" <<'SQL'
SELECT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_locks
    WHERE locktype = 'relation'
      AND relation = :'relation_oid'::oid
      AND mode = 'AccessExclusiveLock'
      AND granted
      AND pid <> pg_backend_pid()
);
SQL
)" == "t" ]]; then
      return 0
    fi
    sleep 0.1
  done

  echo "concurrent DDL did not acquire AccessExclusiveLock on OID $relation_oid" >&2
  return 1
}

expect_build_lock_timeout() {
  local description="$1"
  local log_path="$2"
  local status

  set +e
  psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -c "SET lock_timeout = '1s'; SET statement_timeout = '5s'; SELECT * FROM graph.build();" \
    >"$log_path" 2>&1
  status=$?
  set -e

  if [[ "$status" -eq 0 ]]; then
    echo "graph.build() succeeded during $description" >&2
    cat "$log_path" >&2
    return 1
  fi
  if ! grep -q '55P03' "$log_path"; then
    echo "graph.build() did not report lock_not_available during $description" >&2
    cat "$log_path" >&2
    return 1
  fi
}

cargo pgrx install --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE" --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
CREATE EXTENSION graph;
SELECT graph.reset();
SET graph.auto_load = off;
SET graph.persist_on_build = off;
CREATE SCHEMA relation_identity_gate;
CREATE TABLE relation_identity_gate.nodes (
    id text PRIMARY KEY,
    name text NOT NULL
);
INSERT INTO relation_identity_gate.nodes (id, name)
VALUES ('n1', 'original row');
SELECT graph.add_table(
    'relation_identity_gate.nodes'::regclass,
    'id',
    ARRAY['name']
);
SELECT * FROM graph.build();
SQL

original_oid="$(psql -X -qAt -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -c "SELECT 'relation_identity_gate.nodes'::regclass::oid")"

start_advisory_holder 1918928211 -272080204 "$WORKDIR/rename-holder.log"
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" >"$WORKDIR/rename-ddl.log" 2>&1 <<'SQL' &
BEGIN;
ALTER TABLE relation_identity_gate.nodes RENAME TO renamed_nodes;
SELECT pg_advisory_lock(1918928211, -272080204);
COMMIT;
SQL
DDL_PID=$!
wait_for_relation_lock "$original_oid"
expect_build_lock_timeout "a concurrent relation rename" "$WORKDIR/build-during-rename.log"
terminate_advisory_holder 1918928211 -272080204
if ! wait "$DDL_PID"; then
  echo "concurrent rename transaction failed" >&2
  cat "$WORKDIR/rename-ddl.log" >&2
  exit 1
fi
DDL_PID=""

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
SET search_path = pg_catalog;
SELECT * FROM graph.build();
DO $$
DECLARE
    registered_oid oid;
    renamed_oid oid;
BEGIN
    SELECT table_oid
      INTO STRICT registered_oid
      FROM graph._registered_tables
     WHERE graph_id = '00000000-0000-0000-0000-000000000001'::uuid;
    SELECT 'relation_identity_gate.renamed_nodes'::regclass::oid
      INTO renamed_oid;
    IF registered_oid <> renamed_oid THEN
        RAISE EXCEPTION 'registered OID changed after rename: % <> %',
            registered_oid, renamed_oid;
    END IF;
END
$$;
SQL

renamed_oid="$(psql -X -qAt -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -c "SELECT 'relation_identity_gate.renamed_nodes'::regclass::oid")"
start_advisory_holder 1918928211 -272080203 "$WORKDIR/recreate-holder.log"
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" >"$WORKDIR/recreate-ddl.log" 2>&1 <<'SQL' &
BEGIN;
DROP TABLE relation_identity_gate.renamed_nodes;
CREATE TABLE relation_identity_gate.renamed_nodes (
    id text PRIMARY KEY,
    name text NOT NULL
);
INSERT INTO relation_identity_gate.renamed_nodes (id, name)
VALUES ('n1', 'replacement row');
SELECT pg_advisory_lock(1918928211, -272080203);
COMMIT;
SQL
DDL_PID=$!
wait_for_relation_lock "$renamed_oid"
expect_build_lock_timeout "a concurrent drop and recreation" \
  "$WORKDIR/build-during-recreate.log"
terminate_advisory_holder 1918928211 -272080203
if ! wait "$DDL_PID"; then
  echo "concurrent drop/recreate transaction failed" >&2
  cat "$WORKDIR/recreate-ddl.log" >&2
  exit 1
fi
DDL_PID=""

replacement_oid="$(psql -X -qAt -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -c "SELECT 'relation_identity_gate.renamed_nodes'::regclass::oid")"
if [[ "$replacement_oid" == "$renamed_oid" ]]; then
  echo "drop/recreate unexpectedly reused relation OID $renamed_oid" >&2
  exit 1
fi

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -c "SET statement_timeout = '5s'; SELECT * FROM graph.build();" \
  >"$WORKDIR/build-after-recreate.log" 2>&1
recreate_status=$?
set -e
if [[ "$recreate_status" -eq 0 ]]; then
  echo "graph.build() retargeted a drop/recreated relation by name" >&2
  cat "$WORKDIR/build-after-recreate.log" >&2
  exit 1
fi
if ! grep -q 'registered table relation no longer exists' \
  "$WORKDIR/build-after-recreate.log"; then
  echo "drop/recreate did not fail with relation identity guidance" >&2
  cat "$WORKDIR/build-after-recreate.log" >&2
  exit 1
fi

echo "Concurrent relation identity DDL gate passed for PostgreSQL $PG_MAJOR"
