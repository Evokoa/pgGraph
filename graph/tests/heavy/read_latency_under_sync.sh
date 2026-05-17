#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_read_latency}"
CREATE_DB="${CREATE_DB:-1}"
NODE_COUNT="${NODE_COUNT:-10000}"
SMALL_BACKLOG="${SMALL_BACKLOG:-100}"
LARGE_BACKLOG="${LARGE_BACKLOG:-5000}"
SAMPLES="${SAMPLES:-25}"
CONCURRENT_SAMPLES="${CONCURRENT_SAMPLES:-25}"
CLIENTS="${CLIENTS:-4}"
JOBS="${JOBS:-2}"
TIME="${TIME:-30}"
RATE="${RATE:-100}"
QUERY_FRESHNESS="${QUERY_FRESHNESS:-off}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
KEEP_WORKDIR="${KEEP_WORKDIR:-0}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-read-latency.XXXXXX")"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SAMPLES_CSV="$WORKDIR/read-latency-samples.csv"
SUMMARY_CSV="$WORKDIR/read-latency-summary.csv"

cleanup() {
  if [[ "$KEEP_WORKDIR" == "1" ]]; then
    echo "Keeping workdir: $WORKDIR"
  else
    rm -rf "$WORKDIR"
  fi
}
trap cleanup EXIT

if ! command -v psql >/dev/null 2>&1; then
  echo "psql is required"
  exit 2
fi

if ! command -v pgbench >/dev/null 2>&1; then
  echo "pgbench is required"
  exit 2
fi

case "$QUERY_FRESHNESS" in
  off | apply_pending_sync | error_on_pending) ;;
  *)
    echo "QUERY_FRESHNESS must be off, apply_pending_sync, or error_on_pending"
    exit 2
    ;;
esac

if [[ "$CREATE_DB" == "1" && "$DBNAME" != "postgres" ]]; then
  dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
  createdb "$DBNAME"
fi

echo "label,query_kind,query_freshness,sample,latency_ms,rows_seen,backlog_rows" >"$SAMPLES_CSV"
echo "label,query_kind,query_freshness,samples,p50_ms,p95_ms,p99_ms,min_ms,max_ms,backlog_rows" >"$SUMMARY_CSV"

psql -X -v ON_ERROR_STOP=1 \
  -v node_count="$NODE_COUNT" \
  "$DBNAME" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
DROP TABLE IF EXISTS public.graph_read_latency_edges CASCADE;
DROP TABLE IF EXISTS public.graph_read_latency_nodes CASCADE;
DROP SEQUENCE IF EXISTS public.graph_read_latency_node_seq;

CREATE TABLE public.graph_read_latency_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    score INT NOT NULL
);

CREATE SEQUENCE public.graph_read_latency_node_seq START WITH 10000001;

CREATE TABLE public.graph_read_latency_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_read_latency_nodes(id) ON UPDATE CASCADE ON DELETE CASCADE,
    to_id TEXT NOT NULL REFERENCES public.graph_read_latency_nodes(id) ON UPDATE CASCADE ON DELETE CASCADE,
    weight INT NOT NULL DEFAULT 1
);

INSERT INTO public.graph_read_latency_nodes (id, name, score)
SELECT i::text, 'seed-' || i::text, i % 1000
FROM generate_series(1, :node_count) AS i;

INSERT INTO public.graph_read_latency_edges (from_id, to_id, weight)
SELECT i::text, (i + 1)::text, 1
FROM generate_series(1, :node_count - 1) AS i;

SELECT graph.add_table('public.graph_read_latency_nodes'::regclass, 'id', ARRAY['name', 'score']);
SELECT graph.add_edge(
    'public.graph_read_latency_edges'::regclass,
    'from_id',
    'public.graph_read_latency_nodes'::regclass,
    'id',
    'read_latency',
    false,
    'weight'
);
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
SELECT graph.enable_sync();
SQL

pending_rows() {
  psql -X -q -v ON_ERROR_STOP=1 -tA "$DBNAME" \
    -c "SELECT count(*) FROM graph._sync_log"
}

append_backlog() {
  local rows="$1"

  psql -X -v ON_ERROR_STOP=1 -v rows="$rows" "$DBNAME" <<'SQL'
WITH ids AS (
    SELECT nextval('public.graph_read_latency_node_seq') AS id
    FROM generate_series(1, :rows)
),
inserted_nodes AS (
    INSERT INTO public.graph_read_latency_nodes (id, name, score)
    SELECT id::text, 'pending-' || id::text, (id % 1000)::int
    FROM ids
    RETURNING id::bigint
)
INSERT INTO public.graph_read_latency_edges (from_id, to_id, weight)
SELECT (id - 1)::text, id::text, 1
FROM inserted_nodes
WHERE EXISTS (
    SELECT 1
    FROM public.graph_read_latency_nodes
    WHERE graph_read_latency_nodes.id = (inserted_nodes.id - 1)::text
);
SQL
}

percentile() {
  local file="$1"
  local pct="$2"

  awk -v pct="$pct" '
    { values[++count] = $1 }
    END {
      if (count == 0) {
        print "0";
        exit;
      }
      rank = int((pct * count + 99) / 100);
      if (rank < 1) {
        rank = 1;
      }
      if (rank > count) {
        rank = count;
      }
      print values[rank];
    }
  ' "$file"
}

measure_query() {
  local label="$1"
  local query_kind="$2"
  local samples="$3"
  local sql="$4"
  local backlog="$5"
  local values_file="$WORKDIR/${label}-${query_kind}.latencies"

  : >"$values_file"
  for sample in $(seq 1 "$samples"); do
    local result
    result="$(
      psql -X -q -v ON_ERROR_STOP=1 -tA "$DBNAME" <<SQL
SET graph.auto_load = on;
SET graph.query_freshness = '$QUERY_FRESHNESS';
WITH started AS (SELECT clock_timestamp() AS ts),
     measured AS (
       $sql
     ),
     finished AS (SELECT clock_timestamp() AS ts)
SELECT ((EXTRACT(EPOCH FROM (finished.ts - started.ts)) * 1000)::bigint)::text
       || '|' || measured.rows_seen::text
FROM started, measured, finished;
SQL
    )"

    local latency_ms rows_seen
    IFS='|' read -r latency_ms rows_seen <<<"$result"
    echo "$latency_ms" >>"$values_file"
    echo "$label,$query_kind,$QUERY_FRESHNESS,$sample,$latency_ms,$rows_seen,$backlog" >>"$SAMPLES_CSV"
  done

  sort -n "$values_file" -o "$values_file"

  local p50 p95 p99 min max
  p50="$(percentile "$values_file" 50)"
  p95="$(percentile "$values_file" 95)"
  p99="$(percentile "$values_file" 99)"
  min="$(head -n 1 "$values_file")"
  max="$(tail -n 1 "$values_file")"

  echo "$label,$query_kind,$QUERY_FRESHNESS,$samples,$p50,$p95,$p99,$min,$max,$backlog" >>"$SUMMARY_CSV"
  printf '%-22s %-24s freshness=%s samples=%s backlog=%s p50=%sms p95=%sms p99=%sms min=%sms max=%sms\n' \
    "$label" "$query_kind" "$QUERY_FRESHNESS" "$samples" "$backlog" "$p50" "$p95" "$p99" "$min" "$max"
}

measure_suite() {
  local label="$1"
  local samples="$2"

  measure_query "$label" "traverse" "$samples" \
    "SELECT count(*) AS rows_seen FROM graph.traverse('public.graph_read_latency_nodes'::regclass, '1', 3, hydrate := false)" \
    "$(pending_rows)"
  measure_query "$label" "shortest_path" "$samples" \
    "SELECT count(*) AS rows_seen FROM graph.shortest_path('public.graph_read_latency_nodes'::regclass, '1', 'public.graph_read_latency_nodes'::regclass, '$NODE_COUNT', 20, hydrate := false)" \
    "$(pending_rows)"
  measure_query "$label" "weighted_shortest_path" "$samples" \
    "SELECT count(*) AS rows_seen FROM graph.weighted_shortest_path('public.graph_read_latency_nodes'::regclass, '1', 'public.graph_read_latency_nodes'::regclass, '$NODE_COUNT')" \
    "$(pending_rows)"
}

measure_suite "no_pending_sync" "$SAMPLES"

append_backlog "$SMALL_BACKLOG"
measure_suite "small_pending_backlog" "$SAMPLES"

append_backlog "$LARGE_BACKLOG"
measure_suite "large_pending_backlog" "$SAMPLES"

writer_sql="$WORKDIR/read-latency-writer.sql"
cp "$SCRIPT_DIR/pgbench_sync.sql" "$writer_sql"
perl -pi -e 's/graph_pgbench/graph_read_latency/g' "$writer_sql"

pgbench "$DBNAME" \
  --client="$CLIENTS" \
  --jobs="$JOBS" \
  --time="$TIME" \
  --rate="$RATE" \
  --file="$writer_sql" \
  >"$WORKDIR/concurrent-writers.log" 2>&1 &
writer_pid=$!

measure_suite "concurrent_writers" "$CONCURRENT_SAMPLES"

wait "$writer_pid" || {
  cat "$WORKDIR/concurrent-writers.log"
  exit 1
}

final_backlog="$(pending_rows)"

cp "$SAMPLES_CSV" "$PWD/read-latency-samples.csv"
cp "$SUMMARY_CSV" "$PWD/read-latency-summary.csv"

echo "Read-latency samples: $PWD/read-latency-samples.csv"
echo "Read-latency summary: $PWD/read-latency-summary.csv"
echo "Final sync backlog rows: $final_backlog"
