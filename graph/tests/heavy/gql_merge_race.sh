#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_gql_merge_race}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"

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
DROP TABLE IF EXISTS public.graph_gql_merge_race_nodes CASCADE;
CREATE TABLE public.graph_gql_merge_race_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    age INT NOT NULL DEFAULT 0
);
CREATE FUNCTION public.graph_gql_merge_race_barrier()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    ready_key integer;
BEGIN
    ready_key := CASE NEW.name
        WHEN 'from-a' THEN 772002
        WHEN 'from-b' THEN 772003
        ELSE NULL
    END;
    IF ready_key IS NOT NULL THEN
        PERFORM pg_advisory_lock(ready_key);
        PERFORM pg_advisory_lock_shared(772001);
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER graph_gql_merge_race_barrier
BEFORE INSERT ON public.graph_gql_merge_race_nodes
FOR EACH ROW EXECUTE FUNCTION public.graph_gql_merge_race_barrier();
SELECT graph.add_table(
    'public.graph_gql_merge_race_nodes'::regclass,
    id_column := 'id',
    columns := ARRAY['name', 'age']
);
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
         AND objid = $lock_key AND granted")"
    if [[ "$count" != "0" ]]; then
      return 0
    fi
    sleep 0.1
  done

  echo "timed out waiting for $description" >&2
  return 1
}

psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" >"$tmpdir/coordinator.out" <<'SQL' &
SELECT pg_advisory_lock(772001);
SELECT pg_advisory_lock(772005);
SELECT pg_advisory_lock(772004);
DO $$
DECLARE
    ready_count integer;
BEGIN
    FOR attempt IN 1..100 LOOP
        SELECT count(DISTINCT objid)::integer INTO ready_count
        FROM pg_locks
        WHERE locktype = 'advisory'
          AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
          AND objid IN (772006, 772007)
          AND granted;
        IF ready_count = 2 THEN
            EXIT;
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;
    IF ready_count <> 2 THEN
        RAISE EXCEPTION 'timed out waiting for both MERGE backends';
    END IF;
    PERFORM pg_advisory_unlock(772005);
    FOR attempt IN 1..100 LOOP
        SELECT count(DISTINCT objid)::integer INTO ready_count
        FROM pg_locks
        WHERE locktype = 'advisory'
          AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
          AND objid IN (772002, 772003)
          AND granted;
        IF ready_count = 2 THEN
            RETURN;
        END IF;
        PERFORM pg_sleep(0.1);
    END LOOP;
    RAISE EXCEPTION 'timed out waiting for both MERGE trigger barriers';
END
$$;
SELECT pg_advisory_unlock(772001);
SQL
coordinator_pid=$!
ACTIVE_PIDS+=("$coordinator_pid")

wait_for_lock 772004 "MERGE coordinator"

psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/a.out" &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(772006);
SELECT pg_advisory_lock_shared(772005);
SELECT 'result=' || (row #>> '{name}') || ':' || (row #>> '{age}')
FROM graph.gql(
    'MERGE (u:graph_gql_merge_race_nodes {id: ''race-node'', name: ''from-a''})
     ON CREATE SET u.age = 1
     ON MATCH SET u.name = ''matched-a''
     RETURN u.name AS name, u.age AS age'
);
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
pid_a=$!
ACTIVE_PIDS+=("$pid_a")

wait_for_lock 772006 "first MERGE backend"

psql -X -q -tA -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >"$tmpdir/b.out" &
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT pg_advisory_lock(772007);
SELECT pg_advisory_lock_shared(772005);
SELECT 'result=' || (row #>> '{name}') || ':' || (row #>> '{age}')
FROM graph.gql(
    'MERGE (u:graph_gql_merge_race_nodes {id: ''race-node'', name: ''from-b''})
     ON CREATE SET u.age = 2
     ON MATCH SET u.name = ''matched-b''
     RETURN u.name AS name, u.age AS age'
);
SELECT 'tx_dirty=' || tx_delta_dirty FROM graph.status();
SQL
pid_b=$!
ACTIVE_PIDS+=("$pid_b")

wait "$coordinator_pid"
wait "$pid_a"
wait "$pid_b"

results="$(grep -h '^result=' "$tmpdir/a.out" "$tmpdir/b.out")"
if [[ "$(grep -c '^result=from-' <<<"$results")" != "1" ]] ||
   [[ "$(grep -c '^result=matched-' <<<"$results")" != "1" ]]; then
  echo "GQL MERGE race did not return one insert winner and one match retry" >&2
  printf '%s\n' "$results" >&2
  exit 1
fi

for client in a b; do
  if ! grep -qx 'tx_dirty=false' "$tmpdir/$client.out"; then
    echo "GQL MERGE race client $client retained transaction-local graph state" >&2
    cat "$tmpdir/$client.out" >&2
    exit 1
  fi
done

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL' >/dev/null
SET graph.mutable_enabled = on;
DO $$
DECLARE
    source_count bigint;
    inserted_age int;
    final_name text;
BEGIN
    PERFORM graph.build(mode := 'mutable_overlay');
    SELECT count(*), min(age), max(name)
    INTO source_count, inserted_age, final_name
    FROM public.graph_gql_merge_race_nodes
    WHERE id = 'race-node';

    IF source_count <> 1 THEN
        RAISE EXCEPTION 'GQL MERGE race expected one source row, got %',
            source_count;
    END IF;

    IF inserted_age NOT IN (1, 2) THEN
        RAISE EXCEPTION 'GQL MERGE race expected inserted ON CREATE age 1 or 2, got %',
            inserted_age;
    END IF;

    IF final_name NOT IN ('matched-a', 'matched-b') THEN
        RAISE EXCEPTION 'GQL MERGE race expected a matched final name, got %',
            final_name;
    END IF;

    IF (SELECT count(*) FROM graph.gql(
        'MATCH (u:graph_gql_merge_race_nodes {id: ''race-node''}) RETURN u',
        hydrate := false
    )) <> 1 THEN
        RAISE EXCEPTION 'GQL MERGE race graph state does not contain exactly one winner';
    END IF;

    IF (SELECT tx_delta_dirty FROM graph.status()) THEN
        RAISE EXCEPTION 'GQL MERGE race left transaction-local graph state after commit';
    END IF;
END
$$;
SQL

echo "GQL MERGE race checks passed on database: $DBNAME"
