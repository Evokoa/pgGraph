#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-postgres}"
CLIENTS="${CLIENTS:-16}"
JOBS="${JOBS:-4}"
TIME="${TIME:-120}"
RATE="${RATE:-100}"
MAX_APPLY_MS="${MAX_APPLY_MS:-5000}"
MAX_QUERY_MS="${MAX_QUERY_MS:-250}"
MIN_ROWS_APPLIED="${MIN_ROWS_APPLIED:-0}"
CREATE_DB="${CREATE_DB:-1}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ "$CREATE_DB" == "1" && "$DBNAME" != "postgres" ]]; then
  dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
  createdb "$DBNAME"
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
DROP TABLE IF EXISTS public.graph_pgbench_edges CASCADE;
DROP TABLE IF EXISTS public.graph_pgbench_nodes CASCADE;
DROP SEQUENCE IF EXISTS public.graph_pgbench_node_seq;
CREATE TABLE public.graph_pgbench_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    score INT NOT NULL
);
CREATE SEQUENCE public.graph_pgbench_node_seq START WITH 10000001;
CREATE TABLE public.graph_pgbench_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_pgbench_nodes(id) ON UPDATE CASCADE ON DELETE CASCADE,
    to_id TEXT NOT NULL REFERENCES public.graph_pgbench_nodes(id) ON UPDATE CASCADE ON DELETE CASCADE,
    weight INT NOT NULL DEFAULT 1
);
INSERT INTO public.graph_pgbench_nodes (id, name, score)
SELECT i::text, 'seed-' || i::text, i % 1000
FROM generate_series(1, 10000) AS i;
INSERT INTO public.graph_pgbench_edges (from_id, to_id, weight)
SELECT i::text, (i + 1)::text, 1
FROM generate_series(1, 9999) AS i;
SELECT graph.add_table('public.graph_pgbench_nodes'::regclass, 'id', ARRAY['name', 'score']);
SELECT graph.add_edge('public.graph_pgbench_edges'::regclass, 'from_id', 'public.graph_pgbench_nodes'::regclass, 'to_id', 'pgbench', false, 'weight');
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
DO $$
DECLARE
    reached text[];
BEGIN
    SELECT array_agg(node_id ORDER BY depth) INTO reached
    FROM graph.traverse('public.graph_pgbench_nodes'::regclass, '1', 3,
                        edge_types := ARRAY['pgbench'], direction := 'out');
    IF reached IS DISTINCT FROM ARRAY['1', '2', '3', '4']::text[] THEN
        RAISE EXCEPTION 'pgbench fixture lost its source chain: %', reached;
    END IF;
END
$$;
SELECT graph.enable_sync();
SQL

pgbench "$DBNAME" \
  --client="$CLIENTS" \
  --jobs="$JOBS" \
  --time="$TIME" \
  --rate="$RATE" \
  --file="$SCRIPT_DIR/pgbench_sync.sql"

# Inspect captured operations before any graph read can replay them.
psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
DO $$
DECLARE
    transactions bigint;
    malformed bigint;
BEGIN
    SELECT count(*), count(*) FILTER (WHERE inserts <> 1 OR updates <> 1
        OR renames <> 1 OR deletes <> 1 OR operations <> 4)
    INTO transactions, malformed
    FROM (
        SELECT xid,
            count(*) AS operations,
            count(*) FILTER (WHERE op = 'I') AS inserts,
            count(*) FILTER (WHERE op = 'U' AND old_pk = new_pk) AS updates,
            count(*) FILTER (WHERE op = 'U' AND old_pk <> new_pk) AS renames,
            count(*) FILTER (WHERE op = 'D') AS deletes
        FROM graph._sync_log
        WHERE table_oid = 'public.graph_pgbench_nodes'::regclass
        GROUP BY xid
    ) AS captured;
    IF transactions = 0 OR malformed <> 0 THEN
        RAISE EXCEPTION 'incomplete pgbench mutation capture: transactions %, malformed %', transactions, malformed;
    END IF;
    IF (SELECT count(*) FROM public.graph_pgbench_nodes) <> 10000
       OR EXISTS (SELECT 1 FROM public.graph_pgbench_nodes WHERE id::bigint >= 10000001)
       OR (SELECT count(*) FROM public.graph_pgbench_edges) <> 9999 THEN
        RAISE EXCEPTION 'pgbench workload left residual rows or changed seed counts';
    END IF;
END
$$;
SQL

# Keep replay and query timing ordered, in one backend. No topology read occurs
# before apply_sync, so its counters and timer include the actual replay.
metrics="$(
  psql -X -q -v ON_ERROR_STOP=1 -tA "$DBNAME" <<'SQL'
SET graph.auto_load = on;
CREATE TEMP TABLE pgbench_measurement (
    apply_ms bigint, inserts bigint, updates bigint, deletes bigint,
    query_ms bigint, traversal_rows bigint, search_rows bigint
);
DO $$
DECLARE
    started timestamptz;
    apply_elapsed bigint;
    query_elapsed bigint;
    applied record;
    traversed bigint;
    searched bigint;
BEGIN
    started := clock_timestamp();
    SELECT * INTO STRICT applied FROM graph.apply_sync();
    apply_elapsed := ceil(EXTRACT(EPOCH FROM (clock_timestamp() - started)) * 1000)::bigint;
    started := clock_timestamp();
    SELECT count(*) INTO traversed
    FROM graph.traverse('public.graph_pgbench_nodes'::regclass, '1', 3, hydrate := false);
    SELECT count(*) INTO searched
    FROM graph.search('name', 'renamed', table_filter := 'public.graph_pgbench_nodes'::regclass, max_rows := 20);
    query_elapsed := ceil(EXTRACT(EPOCH FROM (clock_timestamp() - started)) * 1000)::bigint;
    IF apply_elapsed < 0 OR query_elapsed < 0 THEN
        RAISE EXCEPTION 'system clock moved backwards during pgbench measurement';
    END IF;
    INSERT INTO pgbench_measurement VALUES (
        apply_elapsed, applied.inserts_applied, applied.updates_applied, applied.deletes_applied,
        query_elapsed, traversed, searched);
END
$$;
SELECT apply_ms, inserts, updates, deletes, query_ms, traversal_rows, search_rows
FROM pgbench_measurement;
SQL
)"
IFS='|' read -r apply_duration inserts updates deletes query_duration traversal_rows search_rows <<<"$metrics"
rows_applied=$((inserts + updates + deletes))

if (( rows_applied < MIN_ROWS_APPLIED )); then
  echo "Expected at least $MIN_ROWS_APPLIED sync rows applied, got $rows_applied"
  exit 1
fi
if (( inserts < 1 || updates < 1 || deletes < 1 )); then
  echo "Expected replay of every operation: inserts=$inserts updates=$updates deletes=$deletes"
  exit 1
fi
if (( apply_duration > MAX_APPLY_MS )); then
  echo "apply_sync exceeded threshold: ${apply_duration}ms > ${MAX_APPLY_MS}ms"
  exit 1
fi

if (( traversal_rows != 4 || search_rows != 0 )); then
  echo "Expected four seed traversal rows and no deleted search results, got $traversal_rows and $search_rows"
  exit 1
fi
if (( query_duration > MAX_QUERY_MS )); then
  echo "post-sync query smoke exceeded threshold: ${query_duration}ms > ${MAX_QUERY_MS}ms"
  exit 1
fi

sync_log_rows="$(psql -X -q -v ON_ERROR_STOP=1 -tA "$DBNAME" -c "SELECT count(*) FROM graph._sync_log")"
if (( sync_log_rows < 1 )); then
  echo "Expected pgbench workload to create durable sync log rows"
  exit 1
fi

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
SET graph.auto_load = on;
DO $$
DECLARE
    reached text[];
BEGIN
    SELECT array_agg(node_id ORDER BY depth) INTO reached
    FROM graph.traverse('public.graph_pgbench_nodes'::regclass, '1', 3,
                        edge_types := ARRAY['pgbench'], direction := 'out');
    IF reached IS DISTINCT FROM ARRAY['1', '2', '3', '4']::text[] THEN
        RAISE EXCEPTION 'post-stress pgbench traversal lost the seed chain: %', reached;
    END IF;
    IF (SELECT count(*) FROM graph.gql(
        'MATCH (n:graph_pgbench_nodes) RETURN n.id', hydrate := false)) <> 10000
       OR (SELECT count(*) FROM graph.gql(
        'MATCH (a:graph_pgbench_nodes)-[:pgbench]->(b:graph_pgbench_nodes) RETURN a.id, b.id',
        hydrate := false)) <> 9999 THEN
        RAISE EXCEPTION 'post-stress visible graph changed seed node or edge counts';
    END IF;
    IF EXISTS (SELECT 1 FROM graph.gql(
        'MATCH (n:graph_pgbench_nodes) RETURN n.id', hydrate := false) AS nodes(row)
        WHERE (row ->> 'n.id')::bigint >= 10000001) THEN
        RAISE EXCEPTION 'post-stress graph retained a deleted workload node';
    END IF;
END
$$;
SELECT node_count, edge_count, sync_status, pending_sync_rows
FROM graph.status();
SQL

echo "pgbench sync stress passed: sync_log_rows=$sync_log_rows rows_applied=$rows_applied apply_ms=$apply_duration query_ms=$query_duration search_rows=$search_rows"
