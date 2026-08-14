#!/usr/bin/env bash
set -euo pipefail
shopt -s nullglob

if [[ "$(uname -s)" != "Linux" || ! -r /proc/self/smaps_rollup ]]; then
  echo "open_type_query_resources.sh requires Linux /proc/<pid>/smaps_rollup" >&2
  exit 2
fi

DBNAME="${DBNAME:-pggraph_open_type_resources}"
if [[ ! "$DBNAME" =~ ^pggraph_[A-Za-z0-9_]+$ ]]; then
  echo "DBNAME must match ^pggraph_[A-Za-z0-9_]+$" >&2
  exit 2
fi

PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
if [[ ! "$PG_VERSION_FEATURE" =~ ^pg(14|15|16|17|18)$ ]]; then
  echo "PG_VERSION_FEATURE must be one of pg14 through pg18" >&2
  exit 2
fi
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config}"
LABEL_COUNT="${LABEL_COUNT:-65536}"
NODE_COUNT="${NODE_COUNT:-131072}"
DEGREE="${DEGREE:-8}"
DEPTH="${DEPTH:-4}"
BACKEND_COUNT="${BACKEND_COUNT:-1}"
QUERY_ROUNDS="${QUERY_ROUNDS:-100}"
MAX_RSS_BYTES="${MAX_RSS_BYTES:-536870912}"
MAX_PSS_BYTES="${MAX_PSS_BYTES:-536870912}"
MAX_ARTIFACT_BYTES_PER_DIRECTED_EDGE="${MAX_ARTIFACT_BYTES_PER_DIRECTED_EDGE:-64}"
OUTPUT_DIR="${OUTPUT_DIR:-$(pwd)/open-type-resource-output}"
SKIP_INSTALL="${SKIP_INSTALL:-0}"
STATEMENT_TIMEOUT_MS="${STATEMENT_TIMEOUT_MS:-300000}"
MAX_DIRECTED_EDGES="${MAX_DIRECTED_EDGES:-2000000}"
WORKDIR=""
RAW_SAMPLES="$OUTPUT_DIR/resource-samples.tsv"
SUMMARY="$OUTPUT_DIR/resources.csv"
worker_pids=()
database_created=0

cleanup() {
  for worker_pid in "${worker_pids[@]:-}"; do
    kill "$worker_pid" >/dev/null 2>&1 || true
  done
  for worker_pid in "${worker_pids[@]:-}"; do
    wait "$worker_pid" >/dev/null 2>&1 || true
  done
  if [[ "$database_created" -eq 1 ]]; then
    dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
  fi
  if [[ -n "$WORKDIR" ]]; then
    rm -rf "$WORKDIR"
  fi
}

for value in "$LABEL_COUNT" "$NODE_COUNT" "$DEGREE" "$DEPTH" "$BACKEND_COUNT" "$QUERY_ROUNDS"; do
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "resource fixture dimensions must be positive integers" >&2
    exit 2
  fi
done
if (( LABEL_COUNT > 1000000 || NODE_COUNT > 1000000 || DEGREE > 1024 || DEPTH > 16 || BACKEND_COUNT > 8 || QUERY_ROUNDS > 100 )); then
  echo "resource fixture dimensions exceed their bounded runner limits" >&2
  exit 2
fi
for value in "$MAX_RSS_BYTES" "$MAX_PSS_BYTES" "$MAX_ARTIFACT_BYTES_PER_DIRECTED_EDGE" "$STATEMENT_TIMEOUT_MS" "$MAX_DIRECTED_EDGES"; do
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "resource budgets and timeouts must be positive integers" >&2
    exit 2
  fi
done
if [[ "$SKIP_INSTALL" != "0" && "$SKIP_INSTALL" != "1" ]]; then
  echo "SKIP_INSTALL must be 0 or 1" >&2
  exit 2
fi
if (( STATEMENT_TIMEOUT_MS > 900000 || MAX_DIRECTED_EDGES > 10000000 )); then
  echo "resource timeout or directed-edge cap exceeds the runner policy" >&2
  exit 2
fi
if (( MAX_RSS_BYTES > 4398046511104 || MAX_PSS_BYTES > 4398046511104 || MAX_ARTIFACT_BYTES_PER_DIRECTED_EDGE > 4096 )); then
  echo "resource byte budgets exceed the runner policy" >&2
  exit 2
fi
if (( NODE_COUNT > MAX_DIRECTED_EDGES / DEGREE )); then
  echo "NODE_COUNT * DEGREE exceeds MAX_DIRECTED_EDGES" >&2
  exit 2
fi

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/pggraph-open-type-resources.XXXXXX")"
trap cleanup EXIT
mkdir -p "$OUTPUT_DIR"
if [[ "$SKIP_INSTALL" != "1" ]]; then
  cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
fi
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"
database_created=1
export PGOPTIONS="${PGOPTIONS:-} -c statement_timeout=${STATEMENT_TIMEOUT_MS}"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -v label_count="$LABEL_COUNT" -v node_count="$NODE_COUNT" -v degree="$DEGREE" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SET graph.persist_on_build = on;
SET graph.sync_mode = 'manual';
SELECT graph.reset();
CREATE TABLE public.open_type_resource_nodes (id integer PRIMARY KEY);
INSERT INTO public.open_type_resource_nodes SELECT value FROM generate_series(1, :node_count) value;
CREATE TABLE public.open_type_resource_edges (
  id bigint PRIMARY KEY,
  source_id integer NOT NULL REFERENCES public.open_type_resource_nodes(id),
  target_id integer NOT NULL REFERENCES public.open_type_resource_nodes(id),
  rel_type text NOT NULL
);
CREATE UNLOGGED TABLE public.open_type_resource_control (
  worker integer NOT NULL,
  phase text NOT NULL,
  PRIMARY KEY (worker, phase)
);
INSERT INTO public.open_type_resource_edges
SELECT (source_id::bigint - 1) * :degree + edge_offset,
       source_id,
       ((source_id + edge_offset - 1) % :node_count) + 1,
       'type_' || ((((source_id::bigint - 1) * :degree + edge_offset - 1) % :label_count) + 1)
FROM generate_series(1, :node_count) source_id
CROSS JOIN generate_series(1, :degree) edge_offset;
SELECT graph.add_table('public.open_type_resource_nodes'::regclass, 'id');
SELECT graph.add_edge(
  'public.open_type_resource_edges'::regclass,
  'source_id', 'public.open_type_resource_nodes'::regclass, 'target_id',
  'fallback', false, label_column := 'rel_type');
SELECT * FROM graph.build();
SQL

EDGE_COUNT=$((NODE_COUNT * DEGREE))
if (( EDGE_COUNT < LABEL_COUNT )); then
  echo "NODE_COUNT * DEGREE does not place every label in the fixture" >&2
  exit 2
fi
ARTIFACT_BYTES="$(psql -X -v ON_ERROR_STOP=1 -At -d "$DBNAME" \
  -c 'SELECT artifact_bytes FROM graph.projection_status()')"
if [[ ! "$ARTIFACT_BYTES" =~ ^[0-9]+$ ]]; then
  echo "projection status returned invalid ARTIFACT_BYTES=$ARTIFACT_BYTES" >&2
  exit 1
fi
if (( ARTIFACT_BYTES > EDGE_COUNT * MAX_ARTIFACT_BYTES_PER_DIRECTED_EDGE )); then
  echo "artifact bytes $ARTIFACT_BYTES exceed the per-directed-edge budget" >&2
  exit 1
fi

for backend in $(seq 1 "$BACKEND_COUNT"); do
  pid_file="$WORKDIR/backend-${backend}.pid"
  sql_file="$WORKDIR/backend-${backend}.sql"
  {
    printf '\\pset tuples_only on\n\\pset format unaligned\n'
    printf "SET application_name = 'pggraph_open_type_resource_worker_%s';\n" "$backend"
    printf '\\o %s\nSELECT pg_backend_pid();\n\\o\n' "$pid_file"
    printf "DO \$wait\$ BEGIN WHILE NOT EXISTS (SELECT 1 FROM public.open_type_resource_control WHERE worker = 0 AND phase = 'load') LOOP PERFORM pg_sleep(0.05); END LOOP; END \$wait\$;\n"
    printf "SELECT * FROM graph.load_graph('default');\n"
    printf "INSERT INTO public.open_type_resource_control VALUES (%s, 'ready');\n" "$backend"
    printf "DO \$wait\$ BEGIN WHILE NOT EXISTS (SELECT 1 FROM public.open_type_resource_control WHERE worker = 0 AND phase = 'query') LOOP PERFORM pg_sleep(0.05); END LOOP; END \$wait\$;\n"
    printf "INSERT INTO public.open_type_resource_control VALUES (%s, 'query-started');\n" "$backend"
    printf "DO \$query\$ DECLARE round_no integer; BEGIN FOR round_no IN 1..%s LOOP PERFORM count(*) FROM graph.traverse('public.open_type_resource_nodes'::regclass, '1', %s, hydrate := false); END LOOP; END \$query\$;\n" "$QUERY_ROUNDS" "$DEPTH"
    printf "INSERT INTO public.open_type_resource_control VALUES (%s, 'query-done');\n" "$backend"
    printf 'SELECT pg_sleep(2);\n'
  } >"$sql_file"
  psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -f "$sql_file" \
    >"$WORKDIR/backend-${backend}.out" 2>&1 &
  worker_pids+=("$!")
done
for _ in $(seq 1 300); do
  valid_pid_count=0
  for backend in $(seq 1 "$BACKEND_COUNT"); do
    pid_file="$WORKDIR/backend-${backend}.pid"
    if [[ -s "$pid_file" ]]; then
      backend_pid="$(tr -d '[:space:]' <"$pid_file")"
      [[ "$backend_pid" =~ ^[0-9]+$ ]] && valid_pid_count=$((valid_pid_count + 1))
    fi
  done
  [[ "$valid_pid_count" -eq "$BACKEND_COUNT" ]] && break
  sleep 0.1
done
if [[ "${valid_pid_count:-0}" -ne "$BACKEND_COUNT" ]]; then
  echo "resource runner timed out capturing valid backend PIDs" >&2
  exit 1
fi

printf 'phase\tsample_id\tepoch_ms\tbackend\tpid\trss_bytes\tpss_bytes\n' >"$RAW_SAMPLES"
sample_id=0
sample_phase() {
  local phase="$1"
  sample_id=$((sample_id + 1))
  local epoch_ms
  epoch_ms="$(date +%s%3N)"
  for backend in $(seq 1 "$BACKEND_COUNT"); do
    local backend_pid
    backend_pid="$(tr -d '[:space:]' <"$WORKDIR/backend-${backend}.pid")"
    if [[ ! "$backend_pid" =~ ^[0-9]+$ || ! -r "/proc/$backend_pid/status" || ! -r "/proc/$backend_pid/smaps_rollup" ]]; then
      echo "missing Linux RSS/PSS source for backend $backend" >&2
      exit 1
    fi
    local rss_kb pss_kb
    rss_kb="$(awk '/^VmRSS:/ {print $2; exit}' "/proc/$backend_pid/status")"
    pss_kb="$(awk '/^Pss:/ {print $2; exit}' "/proc/$backend_pid/smaps_rollup")"
    if [[ ! "$rss_kb" =~ ^[0-9]+$ || ! "$pss_kb" =~ ^[0-9]+$ ]]; then
      echo "invalid Linux RSS/PSS sample for backend $backend" >&2
      exit 1
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$phase" "$sample_id" "$epoch_ms" "$backend" "$backend_pid" \
      "$((rss_kb * 1024))" "$((pss_kb * 1024))" >>"$RAW_SAMPLES"
  done
}

control_count() {
  local phase="$1"
  psql -X -v ON_ERROR_STOP=1 -At -d "$DBNAME" \
    -v phase="$phase" \
    -c "SELECT count(*) FROM public.open_type_resource_control WHERE phase = :'phase'"
}

for _ in $(seq 1 10); do
  sample_phase idle
  sleep 0.1
done
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -c "INSERT INTO public.open_type_resource_control VALUES (0, 'load')" >/dev/null
for _ in $(seq 1 600); do
  ready_count="$(control_count ready)"
  [[ "$ready_count" -eq "$BACKEND_COUNT" ]] && break
  sleep 0.1
done
if [[ "${ready_count:-0}" -ne "$BACKEND_COUNT" ]]; then
  echo "resource runner timed out waiting for every loaded backend" >&2
  exit 1
fi
for _ in $(seq 1 20); do
  sample_phase loaded
  sleep 0.1
done
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -c "INSERT INTO public.open_type_resource_control VALUES (0, 'query')" >/dev/null
for _ in $(seq 1 300); do
  started_count="$(control_count query-started)"
  [[ "$started_count" -eq "$BACKEND_COUNT" ]] && break
  sleep 0.01
done
if [[ "${started_count:-0}" -ne "$BACKEND_COUNT" ]]; then
  echo "resource runner timed out waiting for every query worker" >&2
  exit 1
fi
PID_LIST="$(for backend in $(seq 1 "$BACKEND_COUNT"); do tr -d '[:space:]' <"$WORKDIR/backend-${backend}.pid"; done | paste -sd, -)"
query_samples=0
for _ in $(seq 1 6000); do
  done_count="$(control_count query-done)"
  if [[ "$done_count" -eq "$BACKEND_COUNT" ]]; then
    break
  fi
  active_count="$(psql -X -v ON_ERROR_STOP=1 -At -d "$DBNAME" \
    -c "SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE pid = ANY (ARRAY[$PID_LIST]) AND state = 'active' AND query LIKE '%graph.traverse%'")"
  if [[ "$active_count" -eq "$BACKEND_COUNT" ]]; then
    sample_phase query
    query_samples=$((query_samples + 1))
  fi
  sleep 0.05
done
done_count="$(control_count query-done)"
if [[ "$done_count" -ne "$BACKEND_COUNT" || "$query_samples" -eq 0 ]]; then
  echo "resource runner did not capture a synchronized query-phase sample" >&2
  exit 1
fi

for worker_pid in "${worker_pids[@]}"; do
  wait "$worker_pid" || {
    cat "$WORKDIR"/backend-*.out >&2
    exit 1
  }
done
worker_pids=()

MAX_PER_BACKEND_RSS_BYTES="$(awk -F '\t' 'NR > 1 && $6 > max {max=$6} END {print max+0}' "$RAW_SAMPLES")"
MAX_PER_BACKEND_PSS_BYTES="$(awk -F '\t' 'NR > 1 && $7 > max {max=$7} END {print max+0}' "$RAW_SAMPLES")"
read -r MAX_IDLE_TOTAL_RSS_BYTES MAX_IDLE_TOTAL_PSS_BYTES MAX_LOADED_TOTAL_RSS_BYTES MAX_LOADED_TOTAL_PSS_BYTES MAX_QUERY_TOTAL_RSS_BYTES MAX_QUERY_TOTAL_PSS_BYTES < <(
  awk -F '\t' '
    NR > 1 {rss[$1 SUBSEP $2] += $6; pss[$1 SUBSEP $2] += $7}
    END {
      for (key in rss) {
        split(key, parts, SUBSEP); phase=parts[1]
        if (rss[key] > max_rss[phase]) max_rss[phase]=rss[key]
        if (pss[key] > max_pss[phase]) max_pss[phase]=pss[key]
      }
      print max_rss["idle"]+0, max_pss["idle"]+0,
            max_rss["loaded"]+0, max_pss["loaded"]+0,
            max_rss["query"]+0, max_pss["query"]+0
    }
  ' "$RAW_SAMPLES"
)
QUERY_MINUS_IDLE_TOTAL_RSS_BYTES=$((MAX_QUERY_TOTAL_RSS_BYTES > MAX_IDLE_TOTAL_RSS_BYTES ? MAX_QUERY_TOTAL_RSS_BYTES - MAX_IDLE_TOTAL_RSS_BYTES : 0))
QUERY_MINUS_IDLE_TOTAL_PSS_BYTES=$((MAX_QUERY_TOTAL_PSS_BYTES > MAX_IDLE_TOTAL_PSS_BYTES ? MAX_QUERY_TOTAL_PSS_BYTES - MAX_IDLE_TOTAL_PSS_BYTES : 0))
if (( MAX_PER_BACKEND_RSS_BYTES > MAX_RSS_BYTES )); then
  echo "per-backend RSS $MAX_PER_BACKEND_RSS_BYTES exceeds MAX_RSS_BYTES=$MAX_RSS_BYTES" >&2
  exit 1
fi
if (( MAX_PER_BACKEND_PSS_BYTES > MAX_PSS_BYTES )); then
  echo "per-backend PSS $MAX_PER_BACKEND_PSS_BYTES exceeds MAX_PSS_BYTES=$MAX_PSS_BYTES" >&2
  exit 1
fi

printf 'label_count,backend_count,query_surface,filter_shape,degree,depth,projection_artifact_bytes,max_per_backend_rss_bytes,max_per_backend_pss_bytes,max_idle_total_rss_bytes,max_idle_total_pss_bytes,max_loaded_total_rss_bytes,max_loaded_total_pss_bytes,max_query_total_rss_bytes,max_query_total_pss_bytes,query_minus_idle_total_rss_bytes,query_minus_idle_total_pss_bytes\n' >"$SUMMARY"
printf '%s,%s,traverse,no_filter,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
  "$LABEL_COUNT" "$BACKEND_COUNT" "$DEGREE" "$DEPTH" "$ARTIFACT_BYTES" \
  "$MAX_PER_BACKEND_RSS_BYTES" "$MAX_PER_BACKEND_PSS_BYTES" \
  "$MAX_IDLE_TOTAL_RSS_BYTES" "$MAX_IDLE_TOTAL_PSS_BYTES" \
  "$MAX_LOADED_TOTAL_RSS_BYTES" "$MAX_LOADED_TOTAL_PSS_BYTES" \
  "$MAX_QUERY_TOTAL_RSS_BYTES" "$MAX_QUERY_TOTAL_PSS_BYTES" \
  "$QUERY_MINUS_IDLE_TOTAL_RSS_BYTES" "$QUERY_MINUS_IDLE_TOTAL_PSS_BYTES" >>"$SUMMARY"

echo "Open-type Linux query resource profile passed."
echo "Raw samples: $RAW_SAMPLES"
echo "Summary: $SUMMARY"
