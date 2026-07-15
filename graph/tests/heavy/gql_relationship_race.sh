#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_relationship_race}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/pggraph-gql-relationship-race.XXXXXX")"
ACTIVE_PIDS=()

cleanup() {
  local pid
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

wait_for_lock() {
  local lock_key="$1"
  local description="$2"
  local count

  for _ in $(seq 1 100); do
    count="$(psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
      "SELECT count(*) FROM pg_locks
       WHERE locktype = 'advisory'
         AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
         AND objid = $lock_key AND granted")"
    if [[ "$count" != "0" ]]; then
      return 0
    fi
    sleep 0.1
  done

  echo "timed out waiting for $description" >&2
  return 1
}

assert_clean_client() {
  local output="$1"
  local description="$2"

  if ! grep -q 'tx_dirty=false' "$output"; then
    echo "$description retained transaction-local graph state" >&2
    cat "$output" >&2
    return 1
  fi
}

cargo pgrx install --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE" \
  --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
CREATE EXTENSION graph;
SELECT graph.reset();
SET graph.mutable_enabled = on;
CREATE TABLE public.graph_gql_relationship_race_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE public.graph_gql_relationship_race_edges (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES public.graph_gql_relationship_race_nodes(id),
    target_id TEXT NOT NULL REFERENCES public.graph_gql_relationship_race_nodes(id),
    note TEXT NOT NULL
);
INSERT INTO public.graph_gql_relationship_race_nodes VALUES
    ('source', 'Source'), ('target', 'Target');
SELECT graph.add_table(
    'public.graph_gql_relationship_race_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name']
);
SELECT graph.add_edge(
    'public.graph_gql_relationship_race_edges'::regclass,
    'source_id',
    'public.graph_gql_relationship_race_nodes'::regclass,
    'target_id',
    'linked',
    bidirectional := false
);
CREATE FUNCTION public.graph_gql_relationship_create_barrier()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM pg_advisory_lock(773002);
    PERFORM pg_advisory_lock_shared(773001);
    RETURN NEW;
END
$$;
CREATE TRIGGER graph_gql_relationship_create_barrier
BEFORE INSERT ON public.graph_gql_relationship_race_edges
FOR EACH ROW EXECUTE FUNCTION public.graph_gql_relationship_create_barrier();
SELECT * FROM graph.build(mode := 'mutable_overlay');
SQL

psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" >"$WORKDIR/create-coordinator.out" <<'SQL' &
SELECT pg_advisory_lock(773001);
SELECT pg_advisory_lock(773005);
SELECT pg_advisory_lock(773004);
DO $$
DECLARE
    ready_count integer;
BEGIN
    FOR attempt IN 1..100 LOOP
        SELECT count(DISTINCT objid)::integer INTO ready_count
        FROM pg_locks
        WHERE locktype = 'advisory'
          AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
          AND objid IN (773006, 773007) AND granted;
        EXIT WHEN ready_count = 2;
        PERFORM pg_sleep(0.1);
    END LOOP;
    IF ready_count <> 2 THEN
        RAISE EXCEPTION 'timed out waiting for relationship CREATE backends';
    END IF;
    PERFORM pg_advisory_unlock(773005);
    FOR attempt IN 1..100 LOOP
        IF EXISTS (
            SELECT 1 FROM pg_locks
            WHERE locktype = 'advisory'
              AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
              AND objid = 773002 AND granted
        ) AND (
            SELECT count(*) FROM pg_stat_activity
            WHERE datname = current_database()
              AND application_name IN ('pggraph-create-a', 'pggraph-create-b')
              AND state = 'active'
              AND wait_event_type = 'Lock'
        ) = 2 THEN
            RETURN;
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;
    RAISE EXCEPTION 'timed out waiting for the relationship CREATE lock contender';
END
$$;
SELECT pg_advisory_unlock(773001);
SQL
create_coordinator_pid=$!
ACTIVE_PIDS+=("$create_coordinator_pid")
wait_for_lock 773004 "relationship CREATE coordinator"

PGAPPNAME=pggraph-create-a psql -X -q -tA -v ON_ERROR_STOP=0 -v VERBOSITY=sqlstate -d "$DBNAME" \
  >"$WORKDIR/create-a.out" 2>"$WORKDIR/create-a.err" <<'SQL' &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(773006);
SELECT pg_advisory_lock_shared(773005);
SELECT 'result=' || (row #>> '{source_id}')
FROM graph.gql(
    'MATCH (u:graph_gql_relationship_race_nodes {id: ''source''}),
           (v:graph_gql_relationship_race_nodes {id: ''target''})
     CREATE (u)-[r:linked {id: ''race-edge'', note: ''from-a''}]->(v)
     RETURN u.id AS source_id'
);
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
create_a_pid=$!
ACTIVE_PIDS+=("$create_a_pid")
wait_for_lock 773006 "first relationship CREATE backend"

PGAPPNAME=pggraph-create-b psql -X -q -tA -v ON_ERROR_STOP=0 -v VERBOSITY=sqlstate -d "$DBNAME" \
  >"$WORKDIR/create-b.out" 2>"$WORKDIR/create-b.err" <<'SQL' &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(773007);
SELECT pg_advisory_lock_shared(773005);
SELECT 'result=' || (row #>> '{source_id}')
FROM graph.gql(
    'MATCH (u:graph_gql_relationship_race_nodes {id: ''source''}),
           (v:graph_gql_relationship_race_nodes {id: ''target''})
     CREATE (u)-[r:linked {id: ''race-edge'', note: ''from-b''}]->(v)
     RETURN u.id AS source_id'
);
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
create_b_pid=$!
ACTIVE_PIDS+=("$create_b_pid")

if ! wait "$create_coordinator_pid"; then
  echo "relationship CREATE coordinator failed" >&2
  cat "$WORKDIR/create-a.out" "$WORKDIR/create-a.err" \
      "$WORKDIR/create-b.out" "$WORKDIR/create-b.err" >&2
  kill "$create_a_pid" "$create_b_pid" >/dev/null 2>&1 || true
  wait "$create_a_pid" "$create_b_pid" >/dev/null 2>&1 || true
  exit 1
fi
wait "$create_a_pid"
wait "$create_b_pid"

create_results="$(grep -h '^result=' "$WORKDIR/create-a.out" "$WORKDIR/create-b.out" || true)"
create_errors="$(grep -h '^ERROR:  ' "$WORKDIR/create-a.err" "$WORKDIR/create-b.err" || true)"
if [[ "$(grep -c '^result=source$' <<<"$create_results" || true)" != "1" ]] ||
   [[ "$(grep -c '^ERROR:  23505$' <<<"$create_errors" || true)" != "1" ]]; then
  echo "relationship CREATE race did not produce one winner and one unique-key loser" >&2
  printf '%s\n%s\n' "$create_results" "$create_errors" >&2
  exit 1
fi
assert_clean_client "$WORKDIR/create-a.out" "relationship CREATE client A"
assert_clean_client "$WORKDIR/create-b.out" "relationship CREATE client B"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
DROP TRIGGER graph_gql_relationship_create_barrier
    ON public.graph_gql_relationship_race_edges;
CREATE FUNCTION public.graph_gql_relationship_delete_barrier()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM pg_advisory_lock(773102);
    PERFORM pg_advisory_lock_shared(773101);
    RETURN OLD;
END
$$;
CREATE TRIGGER graph_gql_relationship_delete_barrier
BEFORE DELETE ON public.graph_gql_relationship_race_edges
FOR EACH ROW EXECUTE FUNCTION public.graph_gql_relationship_delete_barrier();
SQL

psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" >"$WORKDIR/delete-coordinator.out" <<'SQL' &
SELECT pg_advisory_lock(773101);
SELECT pg_advisory_lock(773105);
SELECT pg_advisory_lock(773104);
DO $$
DECLARE
    ready_count integer;
    waiting_count integer;
BEGIN
    FOR attempt IN 1..100 LOOP
        SELECT count(DISTINCT objid)::integer INTO ready_count
        FROM pg_locks
        WHERE locktype = 'advisory'
          AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
          AND objid IN (773106, 773107) AND granted;
        EXIT WHEN ready_count = 2;
        PERFORM pg_sleep(0.1);
    END LOOP;
    IF ready_count <> 2 THEN
        RAISE EXCEPTION 'timed out waiting for relationship DELETE backends';
    END IF;
    PERFORM pg_advisory_unlock(773105);
    FOR attempt IN 1..100 LOOP
        IF EXISTS (
            SELECT 1 FROM pg_locks
            WHERE locktype = 'advisory'
              AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
              AND objid = 773102 AND granted
        ) THEN
            SELECT count(*)::integer INTO waiting_count
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND application_name IN ('pggraph-delete-a', 'pggraph-delete-b')
              AND state = 'active'
              AND wait_event_type = 'Lock';
            EXIT WHEN waiting_count = 2;
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;
    IF waiting_count <> 2 THEN
        RAISE EXCEPTION 'timed out waiting for the relationship DELETE row-lock loser';
    END IF;
END
$$;
SELECT pg_advisory_unlock(773101);
SQL
delete_coordinator_pid=$!
ACTIVE_PIDS+=("$delete_coordinator_pid")
wait_for_lock 773104 "relationship DELETE coordinator"

PGAPPNAME=pggraph-delete-a psql -X -q -tA -v ON_ERROR_STOP=0 -v VERBOSITY=sqlstate \
  -d "$DBNAME" >"$WORKDIR/delete-a.out" 2>"$WORKDIR/delete-a.err" <<'SQL' &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(773106);
SELECT pg_advisory_lock_shared(773105);
SELECT 'result=' || (row #>> '{source_id}')
FROM graph.gql(
    'MATCH (u:graph_gql_relationship_race_nodes {id: ''source''})
           -[r:linked]->
           (v:graph_gql_relationship_race_nodes {id: ''target''})
     DELETE r RETURN u.id AS source_id'
);
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
delete_a_pid=$!
ACTIVE_PIDS+=("$delete_a_pid")
wait_for_lock 773106 "first relationship DELETE backend"

PGAPPNAME=pggraph-delete-b psql -X -q -tA -v ON_ERROR_STOP=0 -v VERBOSITY=sqlstate \
  -d "$DBNAME" >"$WORKDIR/delete-b.out" 2>"$WORKDIR/delete-b.err" <<'SQL' &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(773107);
SELECT pg_advisory_lock_shared(773105);
SELECT 'result=' || (row #>> '{source_id}')
FROM graph.gql(
    'MATCH (u:graph_gql_relationship_race_nodes {id: ''source''})
           -[r:linked]->
           (v:graph_gql_relationship_race_nodes {id: ''target''})
     DELETE r RETURN u.id AS source_id'
);
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
delete_b_pid=$!
ACTIVE_PIDS+=("$delete_b_pid")

wait "$delete_coordinator_pid"
wait "$delete_a_pid"
wait "$delete_b_pid"

delete_results="$(grep -h '^result=' "$WORKDIR/delete-a.out" "$WORKDIR/delete-b.out" || true)"
delete_errors="$(grep -h '^ERROR:  ' "$WORKDIR/delete-a.err" "$WORKDIR/delete-b.err" || true)"
if [[ "$(grep -c '^result=source$' <<<"$delete_results" || true)" != "1" ]] ||
   [[ "$(grep -c '^ERROR:  22000$' <<<"$delete_errors" || true)" != "1" ]]; then
  echo "relationship DELETE race did not produce one winner and one stale-row loser" >&2
  printf '%s\n%s\n' "$delete_results" "$delete_errors" >&2
  exit 1
fi
assert_clean_client "$WORKDIR/delete-a.out" "relationship DELETE client A"
assert_clean_client "$WORKDIR/delete-b.out" "relationship DELETE client B"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
DO $$
BEGIN
    IF (SELECT count(*) FROM public.graph_gql_relationship_race_edges) <> 0 THEN
        RAISE EXCEPTION 'relationship race source table retained an edge';
    END IF;
    IF (SELECT count(*) FROM graph.gql(
        'MATCH (u:graph_gql_relationship_race_nodes {id: ''source''})
               -[:linked]->
               (v:graph_gql_relationship_race_nodes {id: ''target''})
         RETURN u, v',
        hydrate := false
    )) <> 0 THEN
        RAISE EXCEPTION 'relationship race graph retained an edge';
    END IF;
    IF (SELECT tx_delta_dirty FROM graph.status()) THEN
        RAISE EXCEPTION 'relationship race verifier has transaction-local graph state';
    END IF;
END
$$;
SQL

echo "GQL relationship CREATE/DELETE race checks passed on database: $DBNAME"
