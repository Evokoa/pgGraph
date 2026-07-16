#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_runtime_resources}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
NODE_COUNT="${NODE_COUNT:-10000}"
EDGE_COUNT="${EDGE_COUNT:-9999}"
LOAD_ROUNDS="${LOAD_ROUNDS:-3}"
SYNC_ROWS="${SYNC_ROWS:-500}"
MAX_RSS_MB="${MAX_RSS_MB:-1024}"
MAX_PSS_MB="${MAX_PSS_MB:-1024}"
QUERY_MEMORY_MB="${QUERY_MEMORY_MB:-256}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-runtime-resources.XXXXXX")"
PID_FILE="$WORKDIR/backend.pid"
OUT_FILE="$WORKDIR/runtime.out"
SAMPLE_FILE="$WORKDIR/memory.tsv"

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
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE" >&2
    exit 2
  fi
fi

cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

if (( SYNC_ROWS * 2 + 2 > NODE_COUNT )); then
  echo "NODE_COUNT must be at least (SYNC_ROWS * 2) + 2" >&2
  exit 2
fi
if (( QUERY_MEMORY_MB <= 0 )); then
  echo "QUERY_MEMORY_MB must be a positive successful-query budget" >&2
  exit 2
fi

psql -v ON_ERROR_STOP=1 "$DBNAME" <<SQL
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
SET graph.mutable_enabled = on;
SET graph.default_projection_mode = 'mutable_overlay';
SET graph.persist_on_build = on;
CREATE TABLE public.graph_runtime_nodes (
    id TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    next_id TEXT REFERENCES public.graph_runtime_nodes(id)
);
INSERT INTO public.graph_runtime_nodes (id, payload, next_id)
SELECT
  i::text,
  repeat('x', 256),
  CASE WHEN i <= $EDGE_COUNT THEN (i + 1)::text ELSE NULL END
FROM generate_series(1, $NODE_COUNT) AS i;
SELECT graph.add_table('public.graph_runtime_nodes'::regclass, 'id', ARRAY['payload']);
SELECT graph.add_edge(
    'public.graph_runtime_nodes'::regclass,
    'next_id',
    'public.graph_runtime_nodes'::regclass,
    'id',
    'linked',
    false
);
SELECT * FROM graph.build();
SQL

(
  {
    cat <<SQL
\t on
\a
\o $PID_FILE
SELECT pg_backend_pid();
\o
SET graph.query_memory_mb = $QUERY_MEMORY_MB;
SET graph.maintenance_memory_mb = 256;
SET graph.spill_disk_limit_mb = 512;
SET graph.query_work_limit = 10000000;
SET graph.operation_timeout_ms = 60000;
SQL
    for _ in $(seq 1 "$LOAD_ROUNDS"); do
      cat <<'SQL'
SELECT * FROM graph.unload_graph('default');
SELECT * FROM graph.load_graph('default');
SELECT * FROM graph.resource_status();
SQL
    done
    cat <<SQL
SELECT count(*)
FROM graph.traverse(
  'public.graph_runtime_nodes'::regclass,
  '1',
  max_depth := 100,
  direction := 'any',
  hydrate := true,
  max_rows := 10000
);
SELECT * FROM graph.resource_status();
SELECT count(*)
FROM graph.gql(
  'MATCH (n:graph_runtime_nodes) RETURN n',
  NULL::jsonb,
  true
);
SELECT * FROM graph.resource_status();
DO \$\$
DECLARE status_row RECORD;
BEGIN
  SELECT * INTO STRICT status_row FROM graph.resource_status();
  IF status_row.operation <> 'query'
     OR status_row.memory_peak_bytes <= 0
     OR status_row.memory_peak_phase NOT LIKE 'query.%'
     OR status_row.work_units <= 0 THEN
    RAISE EXCEPTION 'invalid GQL resource telemetry: %', row_to_json(status_row);
  END IF;
END
\$\$;
SELECT num_components, largest_component FROM graph.component_stats();
SELECT * FROM graph.resource_status();
DO \$\$
DECLARE status_row RECORD;
BEGIN
  SELECT * INTO STRICT status_row FROM graph.resource_status();
  IF status_row.operation <> 'analytics'
     OR status_row.memory_peak_bytes <= 0
     OR status_row.memory_peak_phase <> 'analytics.workspace'
     OR status_row.work_units <= 0 THEN
    RAISE EXCEPTION 'invalid analytics resource telemetry: %', row_to_json(status_row);
  END IF;
END
\$\$;
UPDATE public.graph_runtime_nodes
SET next_id = (id::integer + 2)::text
WHERE id::integer BETWEEN 1 AND $SYNC_ROWS;
SELECT * FROM graph.ingest_projection(max_rows := $((SYNC_ROWS * 8 + 100)), max_bytes := 67108864);
SELECT * FROM graph.resource_status();
DO \$\$
DECLARE status_row RECORD;
BEGIN
  SELECT * INTO STRICT status_row FROM graph.resource_status();
  IF status_row.operation <> 'maintenance'
     OR status_row.memory_peak_bytes <= 0
     OR status_row.memory_peak_phase <> 'sync.ingest'
     OR status_row.disk_peak_bytes <= 0
     OR status_row.rows <> $SYNC_ROWS THEN
    RAISE EXCEPTION 'invalid sync resource telemetry: %', row_to_json(status_row);
  END IF;
END
\$\$;
SELECT * FROM graph.projection_compact(max_rows := 100000, max_bytes := 134217728);
SELECT * FROM graph.resource_status();
UPDATE public.graph_runtime_nodes
SET next_id = (id::integer + 2)::text
WHERE id::integer BETWEEN $((SYNC_ROWS + 1)) AND $((SYNC_ROWS * 2));
SELECT * FROM graph.ingest_projection(max_rows := $((SYNC_ROWS * 8 + 100)), max_bytes := 67108864);
SELECT * FROM graph.resource_status();
SELECT * FROM graph.projection_compact(max_rows := 100000, max_bytes := 134217728);
SELECT * FROM graph.resource_status();
SET graph.query_work_limit = 1;
DO \$\$
DECLARE
  detail_text TEXT;
BEGIN
  PERFORM count(*)
  FROM graph.gql(
    'MATCH (n:graph_runtime_nodes) RETURN n',
    NULL::jsonb,
    false
  );
  RAISE EXCEPTION 'expected resource breaker did not fire';
EXCEPTION
  WHEN SQLSTATE '54000' THEN
    GET STACKED DIAGNOSTICS detail_text = PG_EXCEPTION_DETAIL;
    IF detail_text NOT LIKE '%PG007%' THEN
      RAISE EXCEPTION 'unexpected resource diagnostic: %', detail_text;
    END IF;
END
\$\$;
SELECT * FROM graph.resource_status();
DO \$\$
DECLARE status_row RECORD;
BEGIN
  SELECT * INTO STRICT status_row FROM graph.resource_status();
  IF status_row.operation <> 'query'
     OR status_row.memory_peak_phase <> 'query.candidates'
     OR status_row.work_units <> 1 THEN
    RAISE EXCEPTION 'invalid rejected-query resource telemetry: %', row_to_json(status_row);
  END IF;
END
\$\$;
SQL
  } | psql -v ON_ERROR_STOP=1 "$DBNAME"
) >"$OUT_FILE" 2>&1 &
psql_pid=$!

for _ in $(seq 1 100); do
  [[ -s "$PID_FILE" ]] && break
  sleep 0.1
done

if [[ ! -s "$PID_FILE" ]]; then
  wait "$psql_pid" || true
  cat "$OUT_FILE"
  echo "Runtime resource profile did not capture a backend PID" >&2
  exit 1
fi

backend_pid="$(tr -dc '0-9' < "$PID_FILE" || true)"
if [[ -z "$backend_pid" ]]; then
  wait "$psql_pid" || true
  cat "$OUT_FILE"
  echo "Runtime resource profile captured an invalid backend PID" >&2
  exit 1
fi
peak_rss_kb=0
peak_pss_kb=0
rss_samples=0
pss_samples=0
printf 'epoch\trss_kb\tpss_kb\n' >"$SAMPLE_FILE"
while kill -0 "$psql_pid" >/dev/null 2>&1; do
  rss_kb=""
  pss_kb=""
  if [[ -n "$backend_pid" ]]; then
    rss_kb="$(ps -o rss= -p "$backend_pid" 2>/dev/null | tr -dc '0-9' || true)"
    if [[ -r "/proc/$backend_pid/smaps_rollup" ]]; then
      pss_kb="$(awk '/^Pss:/ { print $2; exit }' "/proc/$backend_pid/smaps_rollup")"
    fi
  fi
  if [[ -n "$rss_kb" ]]; then
    rss_samples=$((rss_samples + 1))
    (( rss_kb > peak_rss_kb )) && peak_rss_kb="$rss_kb"
  fi
  if [[ -n "$pss_kb" ]]; then
    pss_samples=$((pss_samples + 1))
    (( pss_kb > peak_pss_kb )) && peak_pss_kb="$pss_kb"
  fi
  printf '%s\t%s\t%s\n' "$(date +%s)" "${rss_kb:-}" "${pss_kb:-}" >>"$SAMPLE_FILE"
  sleep 0.1
done
wait "$psql_pid" || { cat "$OUT_FILE"; exit 1; }

if (( rss_samples == 0 )); then
  cat "$OUT_FILE"
  echo "Runtime resource profile captured no RSS samples" >&2
  exit 1
fi
if [[ "$(uname -s)" == "Linux" ]] && (( pss_samples == 0 )); then
  cat "$OUT_FILE"
  echo "Runtime resource profile captured no Linux PSS samples" >&2
  exit 1
fi

peak_rss_mb=$(( (peak_rss_kb + 1023) / 1024 ))
peak_pss_mb=$(( (peak_pss_kb + 1023) / 1024 ))
if (( MAX_RSS_MB <= 0 )); then
  echo "MAX_RSS_MB must be a positive release threshold" >&2
  exit 2
fi
if [[ "$(uname -s)" == "Linux" ]] && (( MAX_PSS_MB <= 0 )); then
  echo "MAX_PSS_MB must be a positive Linux release threshold" >&2
  exit 2
fi
if (( peak_rss_mb > MAX_RSS_MB )); then
  cat "$OUT_FILE"
  echo "Peak RSS ${peak_rss_mb}MB exceeded MAX_RSS_MB=${MAX_RSS_MB}MB" >&2
  exit 1
fi
if (( peak_pss_mb > MAX_PSS_MB )); then
  cat "$OUT_FILE"
  echo "Peak PSS ${peak_pss_mb}MB exceeded MAX_PSS_MB=${MAX_PSS_MB}MB" >&2
  exit 1
fi

cat "$OUT_FILE"
echo "Runtime resource profile passed."
echo "Load rounds: $LOAD_ROUNDS"
echo "Peak backend RSS: ${peak_rss_mb}MB"
if (( peak_pss_kb > 0 )); then
  echo "Peak backend PSS: ${peak_pss_mb}MB"
else
  echo "Peak backend PSS: unavailable (requires Linux /proc/<pid>/smaps_rollup)"
fi
