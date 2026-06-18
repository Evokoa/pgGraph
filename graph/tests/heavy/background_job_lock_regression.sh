#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_background_lock}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-background-lock.XXXXXX")"

LOCK_PID=""

terminate_lock_holder() {
  local lock_pids
  lock_pids="$(psql -X -qAt -d "$DBNAME" <<'SQL' 2>/dev/null || true
SELECT pid
FROM pg_locks
WHERE locktype = 'advisory'
  AND classid = 1918928211
  AND objid = -272080206
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

cargo pgrx install \
  --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE development" \
  --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
SET graph.auto_load = off;
SET graph.persist_on_build = off;
CREATE TABLE public.graph_background_lock_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
INSERT INTO public.graph_background_lock_nodes (id, name)
VALUES ('a', 'alpha'), ('b', 'beta');
SELECT graph.add_table('public.graph_background_lock_nodes'::regclass, 'id', ARRAY['name']);
INSERT INTO graph._build_jobs (build_id, status, sync_mode)
VALUES ('background-build-lock-test', 'queued', 'manual');
INSERT INTO graph._maintenance_jobs (job_id, status)
VALUES ('background-maintenance-lock-test', 'queued');
SQL

psql -X -v ON_ERROR_STOP=1 "$DBNAME" >"$WORKDIR/lock-holder.log" 2>&1 <<'SQL' &
SELECT pg_advisory_lock(1918928211, -272080206);
SELECT pg_sleep(300);
SELECT pg_advisory_unlock(1918928211, -272080206);
SQL
LOCK_PID=$!

for _ in $(seq 1 100); do
  if psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL' | grep -qx "held"
WITH attempted AS (
    SELECT pg_try_advisory_lock(1918928211, -272080206) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, -272080206)::text
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
    SELECT pg_try_advisory_lock(1918928211, -272080206) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, -272080206)::text
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

build_error="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT graph._test_run_build_job('background-build-lock-test')")"
if [[ "$build_error" != *"Another build() or vacuum() is already running"* ]]; then
  echo "background build job runner did not return the BuildLocked message"
  echo "$build_error"
  exit 1
fi

build_job_row="$(psql -X -qAt -F $'\t' -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT status, COALESCE(error, '') FROM graph.build_status('background-build-lock-test')")"
build_job_status="${build_job_row%%$'\t'*}"
build_job_error="${build_job_row#*$'\t'}"
if [[ "$build_job_status" != "failed" ]]; then
  echo "background build job row was not marked failed"
  echo "$build_job_row"
  exit 1
fi
if [[ "$build_job_error" != *"Another build() or vacuum() is already running"* ]]; then
  echo "background build job row failed without the BuildLocked message"
  echo "$build_job_row"
  exit 1
fi

maintenance_error="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT graph._test_run_maintenance_job('background-maintenance-lock-test')")"
if [[ "$maintenance_error" != *"Another build() or vacuum() is already running"* ]]; then
  echo "background maintenance job runner did not return the BuildLocked message"
  echo "$maintenance_error"
  exit 1
fi

maintenance_job_row="$(psql -X -qAt -F $'\t' -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT status, COALESCE(error, '') FROM graph.maintenance_status('background-maintenance-lock-test')")"
maintenance_job_status="${maintenance_job_row%%$'\t'*}"
maintenance_job_error="${maintenance_job_row#*$'\t'}"
if [[ "$maintenance_job_status" != "failed" ]]; then
  echo "background maintenance job row was not marked failed"
  echo "$maintenance_job_row"
  exit 1
fi
if [[ "$maintenance_job_error" != *"Another build() or vacuum() is already running"* ]]; then
  echo "background maintenance job row failed without the BuildLocked message"
  echo "$maintenance_job_row"
  exit 1
fi

terminate_lock_holder

released="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
WITH attempted AS (
    SELECT pg_try_advisory_lock(1918928211, -272080206) AS acquired
)
SELECT CASE
    WHEN acquired THEN pg_advisory_unlock(1918928211, -272080206)::text
    ELSE 'held'
END
FROM attempted;
SQL
)"
if [[ "$released" == "held" ]]; then
  echo "simulated build/vacuum advisory lock was not released"
  exit 1
fi

echo "Background job advisory lock regression passed for $DBNAME"
