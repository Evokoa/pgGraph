#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_build_lock}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-build-lock.XXXXXX")"

LOCK_PID=""
PUBLISH_PID=""
WRITER_PID=""
GATE_PID=""

terminate_lock_holder() {
  local lock_pids
  lock_pids="$(psql -X -qAt -d "$DBNAME" <<'SQL' 2>/dev/null || true
SELECT pid
FROM pg_locks
WHERE locktype = 'advisory'
  AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
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
  if [[ -n "$PUBLISH_PID" ]]; then
    kill "$PUBLISH_PID" >/dev/null 2>&1 || true
    wait "$PUBLISH_PID" >/dev/null 2>&1 || true
    PUBLISH_PID=""
  fi
  if [[ -n "$WRITER_PID" ]]; then
    kill "$WRITER_PID" >/dev/null 2>&1 || true
    wait "$WRITER_PID" >/dev/null 2>&1 || true
    WRITER_PID=""
  fi
  if [[ -n "$GATE_PID" ]]; then
    kill "$GATE_PID" >/dev/null 2>&1 || true
    wait "$GATE_PID" >/dev/null 2>&1 || true
    GATE_PID=""
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

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  >"$WORKDIR/current-source-writer.log" 2>&1 <<'SQL'
BEGIN;
SET LOCAL graph.sync_mode = manual;
UPDATE public.graph_lock_nodes SET name = 'uncommitted' WHERE id = 'a';
SELECT * FROM graph.build();
COMMIT;
SQL
current_source_writer_status=$?
set -e
if [[ "$current_source_writer_status" -eq 0 ]]; then
  echo "graph.build() accepted caller-owned writes on a registered source"
  cat "$WORKDIR/current-source-writer.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/current-source-writer.log" ||
  ! grep -q "Another graph maintenance operation or registered source transaction is active" "$WORKDIR/current-source-writer.log"; then
  echo "caller-owned source write did not fail with the build-lock contract"
  cat "$WORKDIR/current-source-writer.log"
  exit 1
fi
if [[ "$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT name FROM public.graph_lock_nodes WHERE id = 'a'")" != "alpha" ]]; then
  echo "failed caller-owned source build did not roll back its source write"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.sync_mode = manual; SELECT * FROM graph.build();" >/dev/null

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE TABLE public.graph_lock_partitioned_nodes (
    id INTEGER NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (id)
) PARTITION BY RANGE (id);
CREATE TABLE public.graph_lock_partitioned_nodes_p0
PARTITION OF public.graph_lock_partitioned_nodes
FOR VALUES FROM (0) TO (100);
INSERT INTO public.graph_lock_partitioned_nodes VALUES (1, 'partition node');
SELECT graph.add_table(
    'public.graph_lock_partitioned_nodes'::regclass,
    'id',
    ARRAY['name']
);
SQL

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  >"$WORKDIR/current-partition-lock.log" 2>&1 <<'SQL'
BEGIN;
SET LOCAL graph.sync_mode = manual;
LOCK TABLE public.graph_lock_partitioned_nodes_p0 IN ROW EXCLUSIVE MODE;
SELECT * FROM graph.build();
COMMIT;
SQL
current_partition_lock_status=$?
set -e
if [[ "$current_partition_lock_status" -eq 0 ]] ||
  ! grep -q "55P03" "$WORKDIR/current-partition-lock.log"; then
  echo "caller-owned partition-leaf write lock did not fail with 55P03"
  cat "$WORKDIR/current-partition-lock.log"
  exit 1
fi

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  >"$WORKDIR/current-catalog-writer.log" 2>&1 <<'SQL'
BEGIN;
UPDATE graph._registered_tables SET columns = columns
WHERE table_oid = 'public.graph_lock_nodes'::regclass::oid;
SELECT * FROM graph.build();
COMMIT;
SQL
current_catalog_writer_status=$?
set -e
if [[ "$current_catalog_writer_status" -eq 0 ]] ||
  ! grep -q "55P03" "$WORKDIR/current-catalog-writer.log"; then
  echo "caller-owned mapping-catalog write did not fail with 55P03"
  cat "$WORKDIR/current-catalog-writer.log"
  exit 1
fi

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

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET statement_timeout = '5s'; SELECT * FROM graph.build();" \
  >"$WORKDIR/build-while-locked.log" 2>&1
build_status=$?
set -e

if [[ "$build_status" -eq 0 ]]; then
  echo "graph.build() succeeded while the build/vacuum advisory lock was held"
  cat "$WORKDIR/build-while-locked.log"
  exit 1
fi

if ! grep -q "55P03" "$WORKDIR/build-while-locked.log"; then
  echo "graph.build() did not report 55P03 while the advisory lock was held"
  cat "$WORKDIR/build-while-locked.log"
  exit 1
fi

if ! grep -q "Another graph maintenance operation or registered source transaction is active" "$WORKDIR/build-while-locked.log"; then
  echo "graph.build() did not report the BuildLocked message"
  cat "$WORKDIR/build-while-locked.log"
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

psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.persist_on_build = on; SELECT * FROM graph.build();" >/dev/null
psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
SET graph.sync_mode = trigger;
SELECT graph.enable_sync();
INSERT INTO public.graph_lock_nodes (id, name) VALUES ('c', 'gamma');
SQL
data_directory="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT current_setting('data_directory')")"
graph_data_dir="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT COALESCE(NULLIF(current_setting('graph.data_dir', true), ''), 'graph')")"
graph_id="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT graph_id FROM graph.current_graph()")"
if [[ "$graph_data_dir" = /* ]]; then
  graph_path="$graph_data_dir/$graph_id/main.pggraph"
else
  graph_path="$data_directory/$graph_data_dir/$graph_id/main.pggraph"
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

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET statement_timeout = '5s'; SELECT * FROM graph.ingest_projection();" \
  >"$WORKDIR/ingest-while-locked.log" 2>&1
ingest_status=$?
set -e
if [[ "$ingest_status" -eq 0 ]]; then
  echo "graph.ingest_projection() succeeded while the graph publication lock was held"
  cat "$WORKDIR/ingest-while-locked.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/ingest-while-locked.log"; then
  echo "graph.ingest_projection() did not report 55P03 while the graph publication lock was held"
  cat "$WORKDIR/ingest-while-locked.log"
  exit 1
fi

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.persist_on_build = on; SET statement_timeout = '5s'; SELECT * FROM graph.build();" \
  >"$WORKDIR/artifact-build-while-locked.log" 2>&1
artifact_build_status=$?
set -e
if [[ "$artifact_build_status" -eq 0 ]]; then
  echo "graph.build() succeeded during artifact preservation lock test"
  cat "$WORKDIR/artifact-build-while-locked.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/artifact-build-while-locked.log"; then
  echo "artifact preservation lock test did not report 55P03"
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
INSERT INTO public.graph_lock_nodes (id, name)
SELECT 'publisher-' || i::text, 'publisher node ' || i::text
FROM generate_series(1, 20000) AS i;
SQL
psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.maintenance_memory_mb = 2048; SELECT * FROM graph.ingest_projection(50000, 1073741824);" \
  >"$WORKDIR/publisher-owner.log" 2>&1 &
PUBLISH_PID=$!

for _ in $(seq 1 200); do
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
  sleep 0.05
done

publisher_holds_lock="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
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
if [[ "$publisher_holds_lock" != "held" ]]; then
  echo "owner graph.ingest_projection() did not hold the publication lock"
  cat "$WORKDIR/publisher-owner.log"
  exit 1
fi

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.maintenance_memory_mb = 2048; SELECT * FROM graph.ingest_projection(50000, 1073741824);" \
  >"$WORKDIR/publisher-contender.log" 2>&1
publisher_contender_status=$?
set -e
if [[ "$publisher_contender_status" -eq 0 ]]; then
  echo "concurrent graph.ingest_projection() contender succeeded"
  cat "$WORKDIR/publisher-contender.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/publisher-contender.log"; then
  echo "concurrent graph.ingest_projection() contender did not report 55P03"
  cat "$WORKDIR/publisher-contender.log"
  exit 1
fi

set +e
wait "$PUBLISH_PID"
publisher_owner_status=$?
set -e
PUBLISH_PID=""
if [[ "$publisher_owner_status" -ne 0 ]]; then
  echo "owner graph.ingest_projection() failed"
  cat "$WORKDIR/publisher-owner.log"
  exit 1
fi
retry_rows="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.maintenance_memory_mb = 2048; SELECT rows_ingested FROM graph.ingest_projection(50000, 1073741824)" | tail -n 1)"
if [[ "$retry_rows" != "0" ]]; then
  echo "publisher retry unexpectedly found $retry_rows rows after owner completion"
  exit 1
fi
published_watermark="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT manifest_watermark FROM graph.projection_status()")"
max_sync_id="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT max(id) FROM graph._sync_log")"
if [[ "$published_watermark" != "$max_sync_id" ]]; then
  echo "publication watermark $published_watermark did not reach sync log $max_sync_id"
  exit 1
fi

node_table_oid="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT 'public.graph_lock_nodes'::regclass::oid::integer")"
psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<SQL
DO \$\$
DECLARE
    definition text;
BEGIN
    SELECT pg_get_functiondef(oid)
      INTO definition
      FROM pg_proc
     WHERE pronamespace = 'graph'::regnamespace
       AND proname = '_sync_${node_table_oid}';
    EXECUTE replace(
        definition,
        '    PERFORM pg_advisory_xact_lock_shared(1918928211, 1735552877);' || chr(10),
        ''
    );
END
\$\$;
SQL
set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT * FROM graph.ingest_projection(100, 1048576)" \
  >"$WORKDIR/legacy-trigger-preflight.log" 2>&1
legacy_trigger_status=$?
set -e
if [[ "$legacy_trigger_status" -eq 0 ]]; then
  echo "durable ingestion accepted a legacy trigger without the writer barrier"
  cat "$WORKDIR/legacy-trigger-preflight.log"
  exit 1
fi
if ! grep -q "0A000" "$WORKDIR/legacy-trigger-preflight.log" ||
  ! grep -q "do not have the current transaction writer barrier" "$WORKDIR/legacy-trigger-preflight.log"; then
  echo "legacy trigger preflight did not fail with upgrade guidance"
  cat "$WORKDIR/legacy-trigger-preflight.log"
  exit 1
fi
psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT graph.enable_sync()" >/dev/null
legacy_trigger_retry="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT rows_ingested FROM graph.ingest_projection(100, 1048576)")"
if [[ "$legacy_trigger_retry" != "0" ]]; then
  echo "refreshed trigger retry unexpectedly ingested $legacy_trigger_retry rows"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" >"$WORKDIR/horizon-commit-gate.log" 2>&1 <<'SQL' &
SELECT pg_advisory_lock(1918928211, 1735552875);
SELECT pg_sleep(300);
SQL
GATE_PID=$!

for _ in $(seq 1 100); do
  gate_ready="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND classid = 1918928211 AND objid = 1735552875 AND granted")"
  if [[ "$gate_ready" == "1" ]]; then
    break
  fi
  sleep 0.05
done
if [[ "${gate_ready:-0}" != "1" ]]; then
  echo "out-of-order commit gate did not acquire its synchronization lock"
  cat "$WORKDIR/horizon-commit-gate.log"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" >"$WORKDIR/horizon-commit-writer.log" 2>&1 <<'SQL' &
BEGIN;
INSERT INTO public.graph_lock_nodes (id, name) VALUES ('horizon-low-commit', 'low commit');
SELECT pg_advisory_lock(1918928211, 1735552873);
SELECT pg_advisory_lock(1918928211, 1735552875);
COMMIT;
SQL
WRITER_PID=$!

for _ in $(seq 1 100); do
  writer_ready="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND classid = 1918928211 AND objid = 1735552873 AND granted")"
  if [[ "$writer_ready" == "1" ]]; then
    break
  fi
  sleep 0.05
done
if [[ "${writer_ready:-0}" != "1" ]]; then
  echo "out-of-order commit writer did not reach its synchronization point"
  cat "$WORKDIR/horizon-commit-writer.log"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "INSERT INTO public.graph_lock_nodes (id, name) VALUES ('horizon-high-commit', 'high commit');" >/dev/null
set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.persist_on_build = on; SELECT * FROM graph.build()" \
  >"$WORKDIR/writer-barrier-build.log" 2>&1
writer_barrier_build_status=$?
set -e
if [[ "$writer_barrier_build_status" -eq 0 ]]; then
  echo "persisted build succeeded while a registered source writer was active"
  cat "$WORKDIR/writer-barrier-build.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/writer-barrier-build.log" ||
  ! grep -q "Another graph maintenance operation or registered source transaction is active" "$WORKDIR/writer-barrier-build.log"; then
  echo "persisted build did not report the registered source writer barrier"
  cat "$WORKDIR/writer-barrier-build.log"
  exit 1
fi
set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.persist_on_build = on; SELECT * FROM graph.load_graph('default'); SELECT * FROM graph.vacuum()" \
  >"$WORKDIR/writer-barrier-vacuum.log" 2>&1
writer_barrier_vacuum_status=$?
set -e
if [[ "$writer_barrier_vacuum_status" -eq 0 ]]; then
  echo "persisted vacuum succeeded while a registered source writer was active"
  cat "$WORKDIR/writer-barrier-vacuum.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/writer-barrier-vacuum.log" ||
  ! grep -q "Another graph maintenance operation or registered source transaction is active" "$WORKDIR/writer-barrier-vacuum.log"; then
  echo "persisted vacuum did not report the registered source writer barrier"
  cat "$WORKDIR/writer-barrier-vacuum.log"
  exit 1
fi
set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT rows_ingested FROM graph.ingest_projection(100, 1048576)" \
  >"$WORKDIR/writer-barrier-commit.log" 2>&1
writer_barrier_commit_status=$?
set -e
if [[ "$writer_barrier_commit_status" -eq 0 ]]; then
  echo "durable ingestion succeeded while a registered source writer was active"
  cat "$WORKDIR/writer-barrier-commit.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/writer-barrier-commit.log" ||
  ! grep -q "Another graph maintenance operation or registered source transaction is active" "$WORKDIR/writer-barrier-commit.log"; then
  echo "durable ingestion did not report the registered source writer barrier"
  cat "$WORKDIR/writer-barrier-commit.log"
  exit 1
fi
gate_backend_pid="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND classid = 1918928211 AND objid = 1735552875 AND granted")"
psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT pg_terminate_backend($gate_backend_pid)" >/dev/null
wait "$GATE_PID" >/dev/null 2>&1 || true
GATE_PID=""
wait "$WRITER_PID"
WRITER_PID=""
horizon_rows_after_commit="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT rows_ingested FROM graph.ingest_projection(100, 1048576)")"
if [[ "$horizon_rows_after_commit" != "2" ]]; then
  echo "expected both out-of-order committed rows after the horizon cleared, got $horizon_rows_after_commit"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" >"$WORKDIR/horizon-rollback-gate.log" 2>&1 <<'SQL' &
SELECT pg_advisory_lock(1918928211, 1735552876);
SELECT pg_sleep(300);
SQL
GATE_PID=$!

for _ in $(seq 1 100); do
  gate_ready="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND classid = 1918928211 AND objid = 1735552876 AND granted")"
  if [[ "$gate_ready" == "1" ]]; then
    break
  fi
  sleep 0.05
done
if [[ "${gate_ready:-0}" != "1" ]]; then
  echo "rollback gate did not acquire its synchronization lock"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" >"$WORKDIR/horizon-rollback-writer.log" 2>&1 <<'SQL' &
BEGIN;
INSERT INTO public.graph_lock_nodes (id, name) VALUES ('horizon-low-rollback', 'low rollback');
SELECT pg_advisory_lock(1918928211, 1735552874);
SELECT pg_advisory_lock(1918928211, 1735552876);
ROLLBACK;
SQL
WRITER_PID=$!

for _ in $(seq 1 100); do
  writer_ready="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND classid = 1918928211 AND objid = 1735552874 AND granted")"
  if [[ "$writer_ready" == "1" ]]; then
    break
  fi
  sleep 0.05
done
if [[ "${writer_ready:-0}" != "1" ]]; then
  echo "rollback writer did not reach its synchronization point"
  cat "$WORKDIR/horizon-rollback-writer.log"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "INSERT INTO public.graph_lock_nodes (id, name) VALUES ('horizon-high-after-rollback', 'high after rollback');" >/dev/null
set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT rows_ingested FROM graph.ingest_projection(100, 1048576)" \
  >"$WORKDIR/writer-barrier-rollback.log" 2>&1
writer_barrier_rollback_status=$?
set -e
if [[ "$writer_barrier_rollback_status" -eq 0 ]]; then
  echo "durable ingestion succeeded while a registered source writer could roll back"
  cat "$WORKDIR/writer-barrier-rollback.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/writer-barrier-rollback.log" ||
  ! grep -q "Another graph maintenance operation or registered source transaction is active" "$WORKDIR/writer-barrier-rollback.log"; then
  echo "durable ingestion did not report the rollback writer barrier"
  cat "$WORKDIR/writer-barrier-rollback.log"
  exit 1
fi
gate_backend_pid="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND classid = 1918928211 AND objid = 1735552876 AND granted")"
psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT pg_terminate_backend($gate_backend_pid)" >/dev/null
wait "$GATE_PID" >/dev/null 2>&1 || true
GATE_PID=""
wait "$WRITER_PID"
WRITER_PID=""
rollback_rows_after="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT rows_ingested FROM graph.ingest_projection(100, 1048576)")"
if [[ "$rollback_rows_after" != "1" ]]; then
  echo "expected only the committed row after rollback, got $rollback_rows_after"
  exit 1
fi
rolled_back_source_count="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT count(*) FROM public.graph_lock_nodes WHERE id = 'horizon-low-rollback'")"
if [[ "$rolled_back_source_count" != "0" ]]; then
  echo "rollback fixture unexpectedly retained its source row"
  exit 1
fi

same_transaction_watermark_before="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT manifest_watermark FROM graph.projection_status()")"
psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE OR REPLACE FUNCTION public.graph_lock_same_transaction_ingest()
RETURNS text
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO public.graph_lock_nodes (id, name)
    VALUES ('same-transaction-apply', 'same transaction apply');
    BEGIN
        PERFORM * FROM graph.ingest_projection(100, 1048576);
        RETURN 'unexpected-success';
    EXCEPTION WHEN others THEN
        RETURN SQLSTATE || '|' || SQLERRM;
    END;
END
$$;
SQL
same_transaction_error="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT public.graph_lock_same_transaction_ingest()")"
if [[ "$same_transaction_error" != 55P03\|* ]]; then
  echo "same-transaction durable ingestion did not fail with 55P03: $same_transaction_error"
  exit 1
fi
same_transaction_watermark_after="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT manifest_watermark FROM graph.projection_status()")"
if [[ "$same_transaction_watermark_after" != "$same_transaction_watermark_before" ]]; then
  echo "same-transaction failed ingestion advanced the projection watermark"
  exit 1
fi
apply_counts="$(psql -X -qAt -F '|' -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT inserts_applied, updates_applied, deletes_applied FROM graph.apply_sync()")"
if [[ "$apply_counts" != "1|0|0" ]]; then
  echo "durable graph.apply_sync() returned counts that differ from its published batch: $apply_counts"
  exit 1
fi
same_transaction_max_sync="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT max(id) FROM graph._sync_log")"
same_transaction_published="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SELECT manifest_watermark FROM graph.projection_status()")"
if [[ "$same_transaction_published" != "$same_transaction_max_sync" ]]; then
  echo "durable graph.apply_sync() did not publish the exact counted batch"
  exit 1
fi

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
FROM generate_series(1, 200000) AS i;
SELECT graph.add_table('public.graph_lock_slow_nodes'::regclass, 'id', ARRAY['name']);
SQL

psql -X -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET graph.sync_mode = 'manual'; SET graph.persist_on_build = on; SET statement_timeout = '30s'; SELECT * FROM graph.build();" \
  >"$WORKDIR/concurrent-owner.log" 2>&1 &
OWNER_PID=$!

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

owner_holds_lock="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
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
if [[ "$owner_holds_lock" != "held" ]]; then
  echo "owner graph.build() did not acquire the build/vacuum advisory lock"
  cat "$WORKDIR/concurrent-owner.log"
  exit 1
fi

failure_count=0
for idx in 1 2 3; do
  set +e
  psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
    -c "SET statement_timeout = '5s'; SELECT * FROM graph.build();" \
    >"$WORKDIR/concurrent-contender-$idx.log" 2>&1
  contender_status=$?
  set -e

  if [[ "$contender_status" -eq 0 ]]; then
    echo "concurrent graph.build() contender $idx succeeded while owner held the lock"
    cat "$WORKDIR/concurrent-contender-$idx.log"
    exit 1
  fi
  if ! grep -q "55P03" "$WORKDIR/concurrent-contender-$idx.log"; then
    echo "concurrent graph.build() contender $idx did not report 55P03"
    cat "$WORKDIR/concurrent-contender-$idx.log"
    exit 1
  fi
  if ! grep -q "Another graph maintenance operation or registered source transaction is active" "$WORKDIR/concurrent-contender-$idx.log"; then
    echo "concurrent graph.build() contender $idx did not report the BuildLocked message"
    cat "$WORKDIR/concurrent-contender-$idx.log"
    exit 1
  fi
  failure_count=$((failure_count + 1))
done

set +e
psql -X --set=VERBOSITY=verbose -v ON_ERROR_STOP=1 "$DBNAME" \
  -c "SET statement_timeout = '5s'; SELECT * FROM graph.vacuum();" \
  >"$WORKDIR/concurrent-vacuum.log" 2>&1
vacuum_status=$?
set -e
if [[ "$vacuum_status" -eq 0 ]]; then
  echo "concurrent graph.vacuum() succeeded while graph.build() held the lock"
  cat "$WORKDIR/concurrent-vacuum.log"
  exit 1
fi
if ! grep -q "55P03" "$WORKDIR/concurrent-vacuum.log"; then
  echo "concurrent graph.vacuum() did not report 55P03"
  cat "$WORKDIR/concurrent-vacuum.log"
  exit 1
fi
if ! grep -q "Another graph maintenance operation or registered source transaction is active" "$WORKDIR/concurrent-vacuum.log"; then
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
  echo "expected 3 concurrent graph.build() 55P03 failures, saw $failure_count"
  exit 1
fi
if [[ -e "$graph_tmp_path" ]]; then
  echo "temporary graph artifact remained after concurrent owner build: $graph_tmp_path"
  exit 1
fi

echo "Build and projection publication advisory lock regression passed for $DBNAME"
