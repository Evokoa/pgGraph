#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_concurrency}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
CLIENTS="${CLIENTS:-6}"
ROUNDS="${ROUNDS:-8}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-concurrency.XXXXXX")"
PROBE_ROLE="graph_worker_probe_${$}_${RANDOM}"
PROBE_ROLE_CREATED=0

cleanup_probe_role() {
  psql -X -v ON_ERROR_STOP=1 -v probe_role="$PROBE_ROLE" "$DBNAME" <<'SQL'
SELECT set_config('pggraph.cleanup_probe_role', :'probe_role', false);
DO $$
DECLARE
    attempt INTEGER;
BEGIN
    FOR attempt IN 1..300 LOOP
        EXIT WHEN NOT EXISTS (
            SELECT 1
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND usename = current_setting('pggraph.cleanup_probe_role')
              AND backend_type = 'graph concurrent build'
        );
        PERFORM pg_sleep(0.1);
    END LOOP;

    -- The dynamic worker belongs exclusively to this disposable probe. Stop
    -- it after the bounded wait so failure-path cleanup cannot leak the role.
    PERFORM pg_terminate_backend(pid)
    FROM pg_stat_activity
    WHERE datname = current_database()
      AND usename = current_setting('pggraph.cleanup_probe_role')
      AND backend_type = 'graph concurrent build';

    FOR attempt IN 1..100 LOOP
        EXIT WHEN NOT EXISTS (
            SELECT 1
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND usename = current_setting('pggraph.cleanup_probe_role')
              AND backend_type = 'graph concurrent build'
        );
        PERFORM pg_sleep(0.1);
    END LOOP;
END
$$;

ALTER TABLE public.graph_concurrency_nodes OWNER TO CURRENT_USER;
ALTER TABLE public.graph_concurrency_edges OWNER TO CURRENT_USER;
ALTER TABLE graph._graphs OWNER TO CURRENT_USER;
ALTER TABLE graph._registered_tables OWNER TO CURRENT_USER;
ALTER TABLE graph._registered_edges OWNER TO CURRENT_USER;
ALTER TABLE graph._registered_filter_columns OWNER TO CURRENT_USER;
ALTER TABLE public.graph_concurrency_nodes NO FORCE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS graph_worker_role_probe_nodes
    ON public.graph_concurrency_nodes;
DROP OWNED BY :"probe_role";
DROP ROLE :"probe_role";
SELECT format(
    'ALTER DATABASE %I SET graph.sync_mode = %L',
    current_database(),
    'trigger'
) \gexec
SQL
}

cleanup() {
  status=$?
  trap - EXIT
  set +e
  if [[ "$PROBE_ROLE_CREATED" -eq 1 ]]; then
    cleanup_probe_role
    cleanup_status=$?
    if [[ "$status" -eq 0 && "$cleanup_status" -ne 0 ]]; then
      status=$cleanup_status
    fi
  fi
  rm -rf "$WORKDIR"
  exit "$status"
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
CREATE TABLE public.graph_concurrency_nodes (
    id TEXT PRIMARY KEY,
    tenant TEXT NOT NULL,
    name TEXT NOT NULL
);
CREATE TABLE public.graph_concurrency_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_concurrency_nodes(id),
    to_id TEXT NOT NULL REFERENCES public.graph_concurrency_nodes(id)
);
INSERT INTO public.graph_concurrency_nodes (id, tenant, name)
SELECT i::text, 'tenant-' || (i % 4)::text, 'node-' || i::text
FROM generate_series(1, 2000) AS i;
INSERT INTO public.graph_concurrency_edges (from_id, to_id)
SELECT i::text, (i + 1)::text
FROM generate_series(1, 1999) AS i;
SELECT graph.add_table('public.graph_concurrency_nodes'::regclass, 'id', ARRAY['tenant', 'name']);
SELECT graph.add_edge('public.graph_concurrency_edges'::regclass, 'from_id', 'public.graph_concurrency_nodes'::regclass, 'id', 'linked', true);
SELECT * FROM graph.build();
SELECT graph.enable_sync();
SQL

mutator="$WORKDIR/mutator.sql"
cat > "$mutator" <<'SQL'
\set id random(2001, 50000)
INSERT INTO public.graph_concurrency_nodes (id, tenant, name)
VALUES (:id::text, 'tenant-' || (:id % 4)::text, 'node-' || :id::text)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name;
INSERT INTO public.graph_concurrency_edges (from_id, to_id)
SELECT (:id - 1)::text, :id::text
WHERE EXISTS (SELECT 1 FROM public.graph_concurrency_nodes WHERE id = (:id - 1)::text)
ON CONFLICT DO NOTHING;
SELECT count(*) >= 0
FROM graph.traverse('public.graph_concurrency_nodes'::regclass, '1', 2, edge_types := ARRAY['linked'], direction := 'out', max_rows := 50);
SQL

for idx in $(seq 1 "$CLIENTS"); do
  pgbench -n -c 1 -j 1 -t "$ROUNDS" -f "$mutator" "$DBNAME" >"$WORKDIR/pgbench-$idx.log" 2>&1 &
done

psql "$DBNAME" -v ON_ERROR_STOP=1 -c "SELECT * FROM graph.build(concurrently := true);" >"$WORKDIR/concurrent-build.log" 2>&1 &
build_pid=$!
psql "$DBNAME" -v ON_ERROR_STOP=1 -c "SELECT * FROM graph.maintenance(concurrently := true);" >"$WORKDIR/concurrent-maintenance.log" 2>&1 &
maintenance_pid=$!

wait "$build_pid" || { cat "$WORKDIR/concurrent-build.log"; exit 1; }
wait "$maintenance_pid" || { cat "$WORKDIR/concurrent-maintenance.log"; exit 1; }
wait

if psql -X -A -t -c \
    "SELECT 1 FROM pg_roles WHERE rolname = '$PROBE_ROLE'" postgres | grep -qx 1; then
  echo "refusing to replace pre-existing probe role: $PROBE_ROLE" >&2
  exit 1
fi
psql -X -v ON_ERROR_STOP=1 -v probe_role="$PROBE_ROLE" postgres <<'SQL'
-- A LOGIN attribute is required because PostgreSQL initializes the dynamic
-- worker connection as this effective role. The role is unique to this run.
CREATE ROLE :"probe_role" LOGIN;
SQL
PROBE_ROLE_CREATED=1

psql -X -v ON_ERROR_STOP=1 -v probe_role="$PROBE_ROLE" "$DBNAME" <<'SQL'
DO $$
DECLARE
    saw_build_job BOOLEAN;
    saw_maintenance_job BOOLEAN;
    jobs_terminal BOOLEAN;
    pending_jobs TEXT;
    unexpected_failures TEXT;
    traversed BIGINT;
    attempt INTEGER;
BEGIN
    SELECT count(*) > 0 INTO saw_build_job
    FROM graph._build_jobs
    WHERE status IN ('queued', 'running', 'completed', 'failed');
    IF NOT saw_build_job THEN
        RAISE EXCEPTION 'concurrent build did not leave a durable job row';
    END IF;

    SELECT count(*) > 0 INTO saw_maintenance_job
    FROM graph._maintenance_jobs
    WHERE status IN ('queued', 'running', 'completed', 'failed');
    IF NOT saw_maintenance_job THEN
        RAISE EXCEPTION 'concurrent maintenance did not leave a durable job row';
    END IF;

    -- A worker that fails before decoding its launch metadata leaves the job
    -- queued forever. Require both asynchronous operations to reach a durable
    -- terminal state before exercising the post-stress maintenance paths.
    FOR attempt IN 1..120 LOOP
        SELECT NOT EXISTS (
            SELECT 1 FROM graph._build_jobs WHERE status IN ('queued', 'running')
        ) AND NOT EXISTS (
            SELECT 1 FROM graph._maintenance_jobs WHERE status IN ('queued', 'running')
        ) INTO jobs_terminal;
        EXIT WHEN jobs_terminal;
        IF attempt = 120 THEN
            SELECT string_agg(kind || '=' || status, ', ' ORDER BY kind, status)
              INTO pending_jobs
              FROM (
                  SELECT 'build' AS kind, status FROM graph._build_jobs
                  UNION ALL
                  SELECT 'maintenance' AS kind, status FROM graph._maintenance_jobs
              ) jobs
             WHERE status IN ('queued', 'running');
            RAISE EXCEPTION 'asynchronous graph jobs did not reach a terminal state: %',
                coalesce(pending_jobs, 'unknown');
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;

    SELECT string_agg(kind || '=' || coalesce(error, '<missing>'), '; ' ORDER BY kind)
      INTO unexpected_failures
      FROM (
          SELECT 'build' AS kind, status, error FROM graph._build_jobs
          UNION ALL
          SELECT 'maintenance' AS kind, status, error FROM graph._maintenance_jobs
      ) jobs
     WHERE status = 'failed'
       AND error IS DISTINCT FROM
           'Another graph maintenance operation or registered source transaction is active';
    IF unexpected_failures IS NOT NULL THEN
        RAISE EXCEPTION 'asynchronous graph jobs failed unexpectedly: %', unexpected_failures;
    END IF;

    -- The enqueueing sessions can exit before their dynamic workers acquire
    -- the maintenance lock. Treat only the documented transient contention as
    -- retryable; each operation retains a 12-second hard deadline.
    FOR attempt IN 1..120 LOOP
        BEGIN
            PERFORM * FROM graph.build();
            EXIT;
        EXCEPTION WHEN lock_not_available THEN
            IF attempt = 120 THEN
                RAISE;
            END IF;
            PERFORM pg_sleep(0.1);
        END;
    END LOOP;
    FOR attempt IN 1..120 LOOP
        BEGIN
            PERFORM * FROM graph.apply_sync();
            EXIT;
        EXCEPTION WHEN lock_not_available THEN
            IF attempt = 120 THEN
                RAISE;
            END IF;
            PERFORM pg_sleep(0.1);
        END;
    END LOOP;
    FOR attempt IN 1..120 LOOP
        BEGIN
            PERFORM * FROM graph.vacuum();
            EXIT;
        EXCEPTION WHEN lock_not_available THEN
            IF attempt = 120 THEN
                RAISE;
            END IF;
            PERFORM pg_sleep(0.1);
        END;
    END LOOP;
    SELECT count(*) INTO traversed
    FROM graph.traverse('public.graph_concurrency_nodes'::regclass, '1', 4, edge_types := ARRAY['linked'], direction := 'out', max_rows := 100);
    IF traversed = 0 THEN
        RAISE EXCEPTION 'post-stress traversal returned no rows';
    END IF;
END
$$;

CREATE TEMP TABLE graph_worker_target (
    kind TEXT PRIMARY KEY,
    job_id TEXT NOT NULL
) ON COMMIT PRESERVE ROWS;

INSERT INTO graph_worker_target (kind, job_id)
SELECT 'build', build_id
FROM graph.build(concurrently := true);

DO $$
DECLARE
    job_status TEXT;
    job_error TEXT;
    attempt INTEGER;
BEGIN
    FOR attempt IN 1..300 LOOP
        SELECT jobs.status, jobs.error
          INTO job_status, job_error
          FROM graph._build_jobs jobs
          JOIN graph_worker_target target ON target.job_id = jobs.build_id
         WHERE target.kind = 'build';
        EXIT WHEN job_status = 'completed';
        IF job_status = 'failed' THEN
            RAISE EXCEPTION 'standalone asynchronous build failed: %',
                coalesce(job_error, 'unknown error');
        END IF;
        IF attempt = 300 THEN
            RAISE EXCEPTION 'standalone asynchronous build did not complete: %',
                coalesce(job_status, 'missing');
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;
END
$$;

-- A job's terminal row can become visible immediately before its dynamic
-- worker backend exits. Wait for that backend to release any remaining
-- process-scoped state before starting the next isolated worker probe.
DO $$
DECLARE
    worker_running BOOLEAN;
    attempt INTEGER;
BEGIN
    FOR attempt IN 1..300 LOOP
        SELECT EXISTS (
            SELECT 1
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND backend_type = 'graph concurrent build'
        ) INTO worker_running;
        EXIT WHEN NOT worker_running;
        IF attempt = 300 THEN
            RAISE EXCEPTION 'standalone asynchronous build worker did not exit';
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;
END
$$;

INSERT INTO graph_worker_target (kind, job_id)
SELECT 'maintenance', job_id
FROM graph.maintenance(concurrently := true);

DO $$
DECLARE
    job_status TEXT;
    job_error TEXT;
    attempt INTEGER;
BEGIN
    FOR attempt IN 1..300 LOOP
        SELECT jobs.status, jobs.error
          INTO job_status, job_error
          FROM graph._maintenance_jobs jobs
          JOIN graph_worker_target target ON target.job_id = jobs.job_id
         WHERE target.kind = 'maintenance';
        EXIT WHEN job_status = 'completed';
        IF job_status = 'failed' THEN
            RAISE EXCEPTION 'standalone asynchronous maintenance failed: %',
                coalesce(job_error, 'unknown error');
        END IF;
        IF attempt = 300 THEN
            RAISE EXCEPTION 'standalone asynchronous maintenance did not complete: %',
                coalesce(job_status, 'missing');
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;
END
$$;

INSERT INTO graph_worker_target (kind, job_id)
SELECT 'scheduler', job_id
FROM graph.add_sync_policy('default', schedule_interval_secs := 60);

UPDATE graph._jobs jobs
   SET next_run_at = now() - interval '1 second'
  FROM graph_worker_target target
 WHERE target.kind = 'scheduler'
   AND target.job_id = jobs.job_id::text;

UPDATE graph._sync_policies policies
   SET next_run_at = now() - interval '1 second'
  FROM graph_worker_target target
 WHERE target.kind = 'scheduler'
   AND target.job_id = policies.job_id::text;

SELECT graph.run_due_jobs_async(1);

DO $$
DECLARE
    run_status TEXT;
    run_error TEXT;
    attempt INTEGER;
BEGIN
    FOR attempt IN 1..300 LOOP
        SELECT runs.status, runs.error
          INTO run_status, run_error
          FROM graph._job_runs runs
          JOIN graph_worker_target target ON target.job_id = runs.job_id::text
         WHERE target.kind = 'scheduler'
           AND runs.execution_mode = 'internal'
         ORDER BY runs.started_at DESC
         LIMIT 1;
        EXIT WHEN run_status = 'completed';
        IF run_status IS NOT NULL AND run_status <> 'running' THEN
            RAISE EXCEPTION 'asynchronous due-job scheduler failed with status %: %',
                run_status, coalesce(run_error, 'unknown error');
        END IF;
        IF attempt = 300 THEN
            RAISE EXCEPTION 'asynchronous due-job scheduler did not complete: %',
                coalesce(run_status, 'missing');
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;
END
$$;

-- Exercise the OID-based worker connection as a non-superuser. Internal
-- catalog DML is granted only in this disposable test database so the probe
-- isolates worker identity from the public catalog-mutation boundary. RLS
-- then makes an elevated or missing worker role observable in nodes_loaded.
GRANT USAGE, CREATE ON SCHEMA graph TO :"probe_role";
GRANT ALL PRIVILEGES ON public.graph_concurrency_nodes, public.graph_concurrency_edges
    TO :"probe_role";
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA graph TO :"probe_role";
GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA graph
    TO :"probe_role";
ALTER TABLE graph._graphs OWNER TO :"probe_role";
ALTER TABLE graph._registered_tables OWNER TO :"probe_role";
ALTER TABLE graph._registered_edges OWNER TO :"probe_role";
ALTER TABLE graph._registered_filter_columns OWNER TO :"probe_role";
ALTER TABLE public.graph_concurrency_nodes OWNER TO :"probe_role";
ALTER TABLE public.graph_concurrency_edges OWNER TO :"probe_role";
ALTER TABLE public.graph_concurrency_nodes ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.graph_concurrency_nodes FORCE ROW LEVEL SECURITY;
CREATE POLICY graph_worker_role_probe_nodes
    ON public.graph_concurrency_nodes
    FOR SELECT TO :"probe_role"
    USING (tenant = 'tenant-1');

-- Dynamic workers start a fresh session, so make the test's manual-sync mode
-- a database default as well as a setting on the enqueueing session. This
-- keeps the role-identity probe focused on source reads instead of requiring
-- the disposable role to own the existing trigger functions.
SELECT format(
    'ALTER DATABASE %I SET graph.sync_mode = %L',
    current_database(),
    'manual'
) \gexec
SET graph.sync_mode = 'manual';
SET ROLE :"probe_role";
CREATE TEMP TABLE graph_role_worker_target ON COMMIT PRESERVE ROWS AS
SELECT build_job.build_id,
       visible.expected_nodes
FROM graph.build(concurrently := true) build_job
CROSS JOIN (
    SELECT count(*)::bigint AS expected_nodes
    FROM public.graph_concurrency_nodes
) visible;
RESET ROLE;

DO $$
DECLARE
    job_status TEXT;
    job_error TEXT;
    loaded_nodes BIGINT;
    expected_nodes BIGINT;
    total_nodes BIGINT;
    attempt INTEGER;
BEGIN
    FOR attempt IN 1..300 LOOP
        SELECT jobs.status, jobs.error, jobs.nodes_loaded, target.expected_nodes
          INTO job_status, job_error, loaded_nodes, expected_nodes
          FROM graph._build_jobs jobs
          JOIN graph_role_worker_target target ON target.build_id = jobs.build_id;
        EXIT WHEN job_status = 'completed';
        IF job_status = 'failed' THEN
            RAISE EXCEPTION 'non-superuser asynchronous build failed: %',
                coalesce(job_error, 'unknown error');
        END IF;
        IF attempt = 300 THEN
            RAISE EXCEPTION 'non-superuser asynchronous build did not complete: %',
                coalesce(job_status, 'missing');
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;

    SELECT count(*) INTO total_nodes FROM public.graph_concurrency_nodes;
    IF loaded_nodes <> expected_nodes OR loaded_nodes >= total_nodes THEN
        RAISE EXCEPTION
            'background worker did not preserve effective-role RLS (loaded %, expected %, total %)',
            loaded_nodes, expected_nodes, total_nodes;
    END IF;
END
$$;
SQL

cleanup_probe_role
PROBE_ROLE_CREATED=0
echo "Concurrency stress passed for $DBNAME"
