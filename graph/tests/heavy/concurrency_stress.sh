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

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
DO $$
DECLARE
    saw_build_job BOOLEAN;
    saw_maintenance_job BOOLEAN;
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
SQL

echo "Concurrency stress passed for $DBNAME"
