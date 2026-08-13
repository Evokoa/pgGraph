#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_rls_large}"
ROLE_NAME="${ROLE_NAME:-${DBNAME}_reader}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
NODE_COUNT="${NODE_COUNT:-1000000}"
COMPOSITE_COUNT="${COMPOSITE_COUNT:-$NODE_COUNT}"
SAMPLES="${SAMPLES:-10}"
WARMUPS="${WARMUPS:-3}"
QUERY_MEMORY_MB="${QUERY_MEMORY_MB:-1024}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd -- "${GRAPH_DIR}/.." && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-${ROOT_DIR}/todo/measurements/$(date +%F)-p0-eager-rls-${NODE_COUNT}}"
RUN_PROFILE="${RUN_PROFILE:-full}"
STATEMENT_TIMEOUT_MS="${STATEMENT_TIMEOUT_MS:-600000}"
DB_READY=0
RUN_STATUS="setup"
ACTIVE_CASE=""

case "$DBNAME" in
  pggraph_*) ;;
  *) echo "DBNAME must start with pggraph_"; exit 2 ;;
esac
case "$ROLE_NAME" in
  pggraph_*) ;;
  *) echo "ROLE_NAME must start with pggraph_"; exit 2 ;;
esac
for value in "$NODE_COUNT" "$COMPOSITE_COUNT" "$SAMPLES" "$WARMUPS" "$QUERY_MEMORY_MB" "$STATEMENT_TIMEOUT_MS"; do
  [[ "$value" =~ ^[0-9]+$ ]] || { echo "counts and sample settings must be non-negative integers"; exit 2; }
done
(( NODE_COUNT >= 1000 && COMPOSITE_COUNT >= 1000 && SAMPLES >= 1 )) || {
  echo "NODE_COUNT and COMPOSITE_COUNT must be at least 1000; SAMPLES must be positive"
  exit 2
}
case "$RUN_PROFILE" in
  full|compact|p3_selective) ;;
  *) echo "RUN_PROFILE must be full, compact, or p3_selective"; exit 2 ;;
esac

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

if [[ "$OUTPUT_DIR" != /* ]]; then
  OUTPUT_DIR="${ROOT_DIR}/${OUTPUT_DIR}"
fi
mkdir -p "$OUTPUT_DIR/plans" "$OUTPUT_DIR/logs"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

cat >"$OUTPUT_DIR/run-metadata.txt" <<EOF
commit=$(git -C "$ROOT_DIR" rev-parse HEAD)
dirty=$(if [[ -n "$(git -C "$ROOT_DIR" status --short)" ]]; then echo true; else echo false; fi)
started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
invocation_cwd=$PWD
repository_root=$ROOT_DIR
postgres=$($PG_CONFIG --version)
platform=$(uname -a)
database=$DBNAME
role=$ROLE_NAME
node_count=$NODE_COUNT
composite_count=$COMPOSITE_COUNT
samples=$SAMPLES
warmups=$WARMUPS
run_profile=$RUN_PROFILE
query_memory_mb=$QUERY_MEMORY_MB
statement_timeout_ms=$STATEMENT_TIMEOUT_MS
EOF
git -C "$ROOT_DIR" rev-parse HEAD >"$OUTPUT_DIR/commit.txt"
git -C "$ROOT_DIR" status --short >"$OUTPUT_DIR/git-status.txt"
printf 'case_name,status,completed_samples,elapsed_seconds,log\n' >"$OUTPUT_DIR/attempts.csv"

export_evidence() {
  (( DB_READY == 1 )) || return 0
  psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v samples_csv="$OUTPUT_DIR/samples.csv" \
    -v summary_csv="$OUTPUT_DIR/summary.csv" \
    -v metadata_csv="$OUTPUT_DIR/database-metadata.csv" \
    -v database_name="$DBNAME" \
    -v node_count="$NODE_COUNT" \
    -v composite_count="$COMPOSITE_COUNT" <<'SQL'
COPY (SELECT * FROM public.rls_bench_samples ORDER BY case_name, sample) TO :'samples_csv' CSV HEADER;
COPY (SELECT case_name, strategy, key_shape, rls_scope, policy_shape, query_kind, count(*) AS samples, percentile_cont(0.5) WITHIN GROUP (ORDER BY total_ms) AS p50_ms, percentile_cont(0.95) WITHIN GROUP (ORDER BY total_ms) AS p95_ms, percentile_cont(0.5) WITHIN GROUP (ORDER BY visibility_us) / 1000.0 AS p50_visibility_ms, percentile_cont(0.95) WITHIN GROUP (ORDER BY visibility_us) / 1000.0 AS p95_visibility_ms, min(result_rows) AS result_rows, min(hidden_nodes) AS hidden_nodes, min(hidden_relationships) AS hidden_relationships, min(spi_calls) AS min_spi_calls, max(spi_calls) AS max_spi_calls, min(requested_keys) AS min_requested_keys, max(requested_keys) AS max_requested_keys, min(returned_keys) AS min_returned_keys, max(returned_keys) AS max_returned_keys, min(requested_key_bytes) AS min_requested_key_bytes, max(requested_key_bytes) AS max_requested_key_bytes, min(returned_key_bytes) AS min_returned_key_bytes, max(returned_key_bytes) AS max_returned_key_bytes, min(source_rows) AS min_source_rows, max(source_rows) AS max_source_rows FROM public.rls_bench_samples GROUP BY case_name, strategy, key_shape, rls_scope, policy_shape, query_kind ORDER BY case_name) TO :'summary_csv' CSV HEADER;
COPY (SELECT current_setting('server_version') AS postgres_version, :'database_name' AS database_name, :'node_count' AS node_count, :'composite_count' AS composite_count, pg_total_relation_size('public.rls_bench_nodes') AS node_table_bytes, pg_total_relation_size('public.rls_bench_composite') AS composite_table_bytes, pg_total_relation_size('public.rls_bench_edges') AS edge_table_bytes) TO :'metadata_csv' CSV HEADER;
SQL
}

finish() {
  local exit_code=$?
  trap - EXIT
  export_evidence || true
  {
    printf 'finished_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'status=%s\n' "$RUN_STATUS"
    printf 'active_case=%s\n' "$ACTIVE_CASE"
    printf 'exit_code=%s\n' "$exit_code"
  } >>"$OUTPUT_DIR/run-metadata.txt"
  exit "$exit_code"
}
trap finish EXIT

if [[ "${SKIP_INSTALL:-0}" != "1" ]]; then
  cd "$GRAPH_DIR"
  cargo pgrx install \
    --pg-config "$PG_CONFIG" \
    --features "$PG_VERSION_FEATURE development" \
    --no-default-features
fi

dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"
DB_READY=1

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -v node_count="$NODE_COUNT" \
  -v composite_count="$COMPOSITE_COUNT" \
  -v samples="$SAMPLES" \
  -v warmups="$WARMUPS" \
  -v role_name="$ROLE_NAME" <<'SQL'
CREATE EXTENSION graph;
SET graph.persist_on_build = on;
SET graph.auto_load = on;
SET graph.sync_mode = 'manual';

CREATE TABLE public.rls_bench_nodes (
    id bigint PRIMARY KEY,
    next_id bigint REFERENCES public.rls_bench_nodes(id),
    tenant_id text NOT NULL,
    payload text NOT NULL
);
INSERT INTO public.rls_bench_nodes
SELECT id,
       CASE WHEN id < :node_count THEN id + 1 END,
       CASE WHEN id % 100 = 0 THEN 'sparse' ELSE 'other' END,
       repeat('x', 32)
FROM generate_series(1, :node_count) AS id;

CREATE TABLE public.rls_bench_composite (
    org_id integer NOT NULL,
    local_id bigint NOT NULL,
    tenant_id text NOT NULL,
    payload text NOT NULL,
    PRIMARY KEY (org_id, local_id)
);
INSERT INTO public.rls_bench_composite
SELECT 7,
       id,
       CASE WHEN id % 100 = 0 THEN 'sparse' ELSE 'other' END,
       repeat('y', 32)
FROM generate_series(1, :composite_count) AS id;

CREATE TABLE public.rls_bench_edges (
    id bigint PRIMARY KEY,
    from_id bigint NOT NULL REFERENCES public.rls_bench_nodes(id),
    to_id bigint NOT NULL REFERENCES public.rls_bench_nodes(id),
    tenant_id text NOT NULL,
    weight integer NOT NULL DEFAULT 1
);
INSERT INTO public.rls_bench_edges
SELECT id,
       id,
       id + 1,
       CASE WHEN id % 100 = 0 THEN 'sparse' ELSE 'other' END
FROM generate_series(1, :node_count - 1) AS id;

SELECT graph.add_table(
    'public.rls_bench_nodes'::regclass,
    'id',
    ARRAY['tenant_id', 'payload']::text[]
);
SELECT graph.add_table(
    'public.rls_bench_composite'::regclass,
    ARRAY['org_id', 'local_id']::text[],
    ARRAY['tenant_id', 'payload']::text[]
);
SELECT graph.add_edge(
    'public.rls_bench_nodes'::regclass,
    'next_id',
    'public.rls_bench_nodes'::regclass,
    'id',
    'next',
    false
);
SELECT graph.add_edge(
    'public.rls_bench_edges'::regclass,
    'from_id',
    'public.rls_bench_nodes'::regclass,
    'to_id',
    'edge_row',
    false,
    weight_column := 'weight'
);
SELECT * FROM graph.build();

DROP ROLE IF EXISTS :role_name;
CREATE ROLE :role_name LOGIN;
GRANT USAGE ON SCHEMA graph TO :role_name;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA graph TO :role_name;
GRANT SELECT ON public.rls_bench_nodes, public.rls_bench_composite, public.rls_bench_edges TO :role_name;

ALTER TABLE public.rls_bench_nodes ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.rls_bench_composite ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.rls_bench_edges ENABLE ROW LEVEL SECURITY;
CREATE POLICY rls_bench_nodes_select ON public.rls_bench_nodes FOR SELECT TO :role_name
USING (
    current_setting('graph.rls_benchmark_policy', true) = 'broad_allow'
    OR (current_setting('graph.rls_benchmark_policy', true) = 'sparse_allow' AND id % 100 = 0)
    OR (current_setting('graph.rls_benchmark_policy', true) = 'sparse_deny' AND id % 100 <> 0)
);
CREATE POLICY rls_bench_composite_select ON public.rls_bench_composite FOR SELECT TO :role_name
USING (
    current_setting('graph.rls_benchmark_policy', true) = 'broad_allow'
    OR (current_setting('graph.rls_benchmark_policy', true) = 'sparse_allow' AND local_id % 100 = 0)
    OR (current_setting('graph.rls_benchmark_policy', true) = 'sparse_deny' AND local_id % 100 <> 0)
);
CREATE POLICY rls_bench_edges_select ON public.rls_bench_edges FOR SELECT TO :role_name
USING (
    current_setting('graph.rls_benchmark_policy', true) = 'broad_allow'
    OR (current_setting('graph.rls_benchmark_policy', true) = 'sparse_allow' AND id % 100 = 0)
    OR (current_setting('graph.rls_benchmark_policy', true) = 'sparse_deny' AND id % 100 <> 0)
);
ALTER TABLE public.rls_bench_nodes DISABLE ROW LEVEL SECURITY;
ALTER TABLE public.rls_bench_composite DISABLE ROW LEVEL SECURITY;
ALTER TABLE public.rls_bench_edges DISABLE ROW LEVEL SECURITY;

CREATE TABLE public.rls_bench_samples (
    case_name text NOT NULL,
    strategy text NOT NULL,
    key_shape text NOT NULL,
    rls_scope text NOT NULL,
    policy_shape text NOT NULL,
    query_kind text NOT NULL,
    sample integer NOT NULL,
    total_ms double precision NOT NULL,
    result_rows bigint NOT NULL,
    visibility_us bigint,
    hidden_nodes bigint,
    hidden_relationships bigint,
    spi_calls bigint NOT NULL,
    requested_keys bigint NOT NULL,
    returned_keys bigint NOT NULL,
    requested_key_bytes bigint NOT NULL,
    returned_key_bytes bigint NOT NULL,
    source_rows bigint NOT NULL
);
GRANT INSERT ON public.rls_bench_samples TO :role_name;

CREATE FUNCTION public.rls_bench_measure(
    case_name text,
    visibility_strategy text,
    key_shape text,
    rls_scope text,
    policy_shape text,
    query_kind text,
    query_sql text,
    expected_rows bigint,
    sample_number integer
) RETURNS void
LANGUAGE plpgsql
SECURITY INVOKER
SET search_path TO pg_catalog, pg_temp
AS $benchmark$
DECLARE
    observed bigint;
    started_at timestamptz;
    finished_at timestamptz;
    metrics jsonb;
BEGIN
    PERFORM graph._test_set_visibility_strategy(visibility_strategy);
    started_at := clock_timestamp();
    EXECUTE query_sql INTO observed;
    finished_at := clock_timestamp();
    metrics := graph._test_visibility_metrics();
    IF observed IS DISTINCT FROM expected_rows THEN
        RAISE EXCEPTION 'benchmark % returned %, expected %', case_name, observed, expected_rows;
    END IF;
    IF sample_number > 0 THEN
        INSERT INTO public.rls_bench_samples (
            case_name, strategy, key_shape, rls_scope, policy_shape, query_kind,
            sample, total_ms, result_rows, visibility_us,
            hidden_nodes, hidden_relationships, spi_calls, requested_keys,
            returned_keys, requested_key_bytes, returned_key_bytes, source_rows
        ) VALUES (
            case_name, metrics->>'strategy', key_shape, rls_scope, policy_shape, query_kind,
            sample_number,
            extract(epoch FROM finished_at - started_at) * 1000.0,
            observed,
            (metrics->>'elapsed_micros')::bigint,
            (metrics->>'hidden_nodes')::bigint,
            (metrics->>'hidden_relationships')::bigint,
            (metrics->>'spi_calls')::bigint,
            (metrics->>'requested_keys')::bigint,
            (metrics->>'returned_keys')::bigint,
            (metrics->>'requested_key_bytes')::bigint,
            (metrics->>'returned_key_bytes')::bigint,
            (metrics->>'source_rows')::bigint
        );
    END IF;
END
$benchmark$;
GRANT EXECUTE ON FUNCTION public.rls_bench_measure(text,text,text,text,text,text,text,bigint,integer) TO :role_name;
SQL

run_cases() {
  local policy="$1"
  local scope="$2"
  local node_depth0=1 shallow=5 deep=21 shortest=5 composite_depth0=1 edge_one_hop=2
  if [[ "$scope" == "node" || "$scope" == "combined" ]]; then
    if [[ "$policy" == "sparse_allow" ]]; then
      shallow=1
      deep=1
      shortest=0
      edge_one_hop=1
    elif [[ "$policy" == "sparse_deny" ]]; then
      node_depth0=0
      shallow=0
      deep=0
      shortest=0
      composite_depth0=0
      edge_one_hop=0
    fi
  elif [[ "$scope" == "edge" && "$policy" == "sparse_deny" ]]; then
    edge_one_hop=1
  fi

  run_case "$policy" "$scope" "${scope}_${policy}_scalar_depth0" "scalar" "depth0" "$node_depth0" \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 0, hydrate := false)"
  run_case "$policy" "$scope" "${scope}_${policy}_scalar_shallow" "scalar" "traverse_depth_4" "$shallow" \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 4, edge_types := ARRAY['next'], hydrate := false)"
  run_case "$policy" "$scope" "${scope}_${policy}_scalar_deep" "scalar" "traverse_depth_20" "$deep" \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 20, edge_types := ARRAY['next'], hydrate := false)"
  run_case "$policy" "$scope" "${scope}_${policy}_scalar_shortest" "scalar" "shortest_path" "$shortest" \
    "SELECT count(*) FROM graph.shortest_path('public.rls_bench_nodes'::regclass, '100', 'public.rls_bench_nodes'::regclass, '104', max_depth := 20, hydrate := false)"
  run_case "$policy" "$scope" "${scope}_${policy}_composite_depth0" "composite" "depth0" "$composite_depth0" \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_composite'::regclass, jsonb_build_array('7'::text, '100'::text)::text, 0, hydrate := false)"
  run_case "$policy" "$scope" "${scope}_${policy}_edge_one_hop" "scalar" "edge_one_hop" "$edge_one_hop" \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['edge_row'], hydrate := false)"
}

run_case() {
  local policy="$1"
  local scope="$2"
  local case_name="$3"
  local key_shape="$4"
  local query_kind="$5"
  local expected_rows="$6"
  local query_sql="$7"
  local visibility_strategy="${8:-eager}"
  local log_file="$OUTPUT_DIR/logs/${case_name}.log"
  local started_seconds=$SECONDS
  local iteration
  ACTIVE_CASE="$case_name"
  RUN_STATUS="running"

  set +e
  {
    cat <<'SQL'
\set VERBOSITY verbose
SET graph.query_memory_mb = :query_memory_mb;
SET statement_timeout = :statement_timeout_ms;
SET graph.rls_benchmark_policy = :'policy';
SQL
    for ((iteration = 1; iteration <= WARMUPS; iteration++)); do
      printf "SELECT public.rls_bench_measure(:'case_name', :'visibility_strategy', :'key_shape', :'scope', :'policy', :'query_kind', :'query_sql', :expected_rows, 0);\n"
    done
    for ((iteration = 1; iteration <= SAMPLES; iteration++)); do
      printf "SELECT public.rls_bench_measure(:'case_name', :'visibility_strategy', :'key_shape', :'scope', :'policy', :'query_kind', :'query_sql', :expected_rows, %d);\n" "$iteration"
    done
  } | psql -X -q -v ON_ERROR_STOP=1 -U "$ROLE_NAME" -d "$DBNAME" \
    -v policy="$policy" -v scope="$scope" -v case_name="$case_name" \
    -v visibility_strategy="$visibility_strategy" \
    -v key_shape="$key_shape" -v query_kind="$query_kind" \
    -v expected_rows="$expected_rows" -v query_sql="$query_sql" \
    -v query_memory_mb="$QUERY_MEMORY_MB" -v statement_timeout_ms="$STATEMENT_TIMEOUT_MS" \
    >"$log_file" 2>&1
  local rc=$?
  set -e

  local completed_samples
  completed_samples="$(psql -X -q -tA -d "$DBNAME" -v case_name="$case_name" <<'SQL'
SELECT count(*) FROM public.rls_bench_samples WHERE case_name = :'case_name';
SQL
)"
  local elapsed_seconds=$((SECONDS - started_seconds))
  if (( rc == 0 )); then
    printf '%s,complete,%s,%s,%s\n' "$case_name" "$completed_samples" "$elapsed_seconds" "logs/${case_name}.log" >>"$OUTPUT_DIR/attempts.csv"
    ACTIVE_CASE=""
    return 0
  fi

  local status="error"
  if grep -Eq 'ERROR:[[:space:]]+57014:|canceling statement due to statement timeout' "$log_file"; then
    status="censored_statement_timeout"
  fi
  RUN_STATUS="$status"
  printf '%s,%s,%s,%s,%s\n' "$case_name" "$status" "$completed_samples" "$elapsed_seconds" "logs/${case_name}.log" >>"$OUTPUT_DIR/attempts.csv"
  cat "$log_file" >&2
  return "$rc"
}

capture_source_plans() {
  local policy="$1"
  local scope="$2"
  local table="$3"
  local key_expr="$4"
  local prefix="$OUTPUT_DIR/plans/${scope}-${policy}-${table}"
  psql -X -q -tA -v ON_ERROR_STOP=1 -U "$ROLE_NAME" -d "$DBNAME" \
    -v policy="$policy" >"${prefix}-scan.json" <<SQL
SET graph.rls_benchmark_policy = :'policy';
-- Keep this retained plan focused on whether the exact production predicate is
-- indexable. Runtime timings above retain PostgreSQL's normal cost choices.
EXPLAIN (ANALYZE, BUFFERS, SETTINGS, FORMAT JSON)
SELECT ${key_expr} AS graph_source_key FROM public.${table} AS src;
SQL
  psql -X -q -tA -v ON_ERROR_STOP=1 -U "$ROLE_NAME" -d "$DBNAME" \
    -v policy="$policy" >"${prefix}-max-key.json" <<SQL
SET graph.rls_benchmark_policy = :'policy';
EXPLAIN (ANALYZE, BUFFERS, SETTINGS, FORMAT JSON)
SELECT COALESCE(max(octet_length(${key_expr})), 0)::bigint
FROM public.${table} AS src;
SQL
}

capture_lazy_probe_plan() {
  local policy="$1"
  local scope="$2"
  local table="$3"
  local key_column="$4"
  local candidate_json="$5"
  local output="$OUTPUT_DIR/plans/${scope}-${policy}-${table}-lazy-candidates.json"
  psql -X -q -tA -v ON_ERROR_STOP=1 -U "$ROLE_NAME" -d "$DBNAME" \
    -v policy="$policy" >"$output" <<SQL
SET graph.rls_benchmark_policy = :'policy';
-- Keep this retained plan focused on whether the exact production predicate is
-- indexable. Runtime timings above retain PostgreSQL's normal cost choices.
SET enable_seqscan = off;
EXPLAIN (ANALYZE, BUFFERS, SETTINGS, FORMAT JSON)
WITH requested(source_key, ordinal) AS (
    SELECT value, ordinality
    FROM pg_catalog.jsonb_array_elements_text('${candidate_json}'::jsonb)
         WITH ORDINALITY AS input(value, ordinality)
)
SELECT requested.source_key
FROM requested
JOIN public.${table} AS src
  ON src.${key_column} = requested.source_key::bigint
ORDER BY requested.ordinal;
SQL
}

capture_source_plans "no_rls" "none" "rls_bench_nodes" "src.id::text"
capture_source_plans "no_rls" "none" "rls_bench_composite" \
  "jsonb_build_array(src.org_id::text, src.local_id::text)::text"
capture_source_plans "no_rls" "none" "rls_bench_edges" "src.id::text"

if [[ "$RUN_PROFILE" == "compact" ]]; then
  run_case "no_rls" "none" "none_no_rls_scalar_depth0" "scalar" "depth0" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 0, hydrate := false)"
elif [[ "$RUN_PROFILE" == "full" ]]; then
  run_cases "no_rls" "none"
elif [[ "$RUN_PROFILE" == "p3_selective" ]]; then
  run_case "no_rls" "none" "p3_no_rls_auto" "scalar" "traverse_depth_4" 5 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 4, edge_types := ARRAY['next'], direction := 'out', strategy := 'bfs', hydrate := false)" "auto"
  run_case "no_rls" "none" "p4_dfs_no_rls_auto" "scalar" "dfs_depth_4" 5 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 4, edge_types := ARRAY['next'], direction := 'out', strategy := 'dfs', hydrate := false)" "auto"
  run_case "no_rls" "none" "p4_path_no_rls_auto" "scalar" "shortest_path" 5 \
    "SELECT count(*) FROM graph.shortest_path('public.rls_bench_nodes'::regclass, '100', 'public.rls_bench_nodes'::regclass, '104', max_depth := 20, hydrate := false)" "auto"
  run_case "no_rls" "none" "p4_weighted_path_no_rls_auto" "scalar" "weighted_shortest_path" 5 \
    "SELECT count(*) FROM graph.weighted_shortest_path('public.rls_bench_nodes'::regclass, '100', 'public.rls_bench_nodes'::regclass, '104', ARRAY['edge_row'])" "auto"
fi

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER TABLE public.rls_bench_nodes ENABLE ROW LEVEL SECURITY; ALTER TABLE public.rls_bench_composite ENABLE ROW LEVEL SECURITY;"
capture_source_plans "broad_allow" "node" "rls_bench_nodes" "src.id::text"
capture_source_plans "sparse_allow" "node" "rls_bench_nodes" "src.id::text"
capture_source_plans "sparse_deny" "node" "rls_bench_nodes" "src.id::text"
capture_source_plans "sparse_allow" "node" "rls_bench_composite" \
  "jsonb_build_array(src.org_id::text, src.local_id::text)::text"
if [[ "$RUN_PROFILE" == "p3_selective" ]]; then
  capture_lazy_probe_plan "sparse_allow" "node" "rls_bench_nodes" "id" '["99","100","101","199","200","201"]'
  run_case "sparse_allow" "node" "p3_node_bfs_eager" "scalar" "traverse_depth_4" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 4, edge_types := ARRAY['next'], direction := 'out', strategy := 'bfs', hydrate := false)" "eager"
  run_case "sparse_allow" "node" "p3_node_bfs_lazy" "scalar" "traverse_depth_4" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 4, edge_types := ARRAY['next'], direction := 'out', strategy := 'bfs', hydrate := false)" "lazy"
  run_case "sparse_allow" "node" "p3_node_inbound_eager" "scalar" "inbound_one_hop" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['next'], direction := 'in', strategy := 'bfs', hydrate := false)" "eager"
  run_case "sparse_allow" "node" "p3_node_inbound_lazy" "scalar" "inbound_one_hop" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['next'], direction := 'in', strategy := 'bfs', hydrate := false)" "lazy"
  run_case "sparse_allow" "node" "p3_node_multiseed_eager" "scalar" "multi_seed_one_hop" 2 \
    "SELECT count(*) FROM graph.traverse(ARRAY['public.rls_bench_nodes'::regclass::oid, 'public.rls_bench_nodes'::regclass::oid], ARRAY['100', '200'], max_depth := 1, edge_types := ARRAY['next'], direction := 'out', strategy := 'bfs', hydrate := false)" "eager"
  run_case "sparse_allow" "node" "p3_node_multiseed_lazy" "scalar" "multi_seed_one_hop" 2 \
    "SELECT count(*) FROM graph.traverse(ARRAY['public.rls_bench_nodes'::regclass::oid, 'public.rls_bench_nodes'::regclass::oid], ARRAY['100', '200'], max_depth := 1, edge_types := ARRAY['next'], direction := 'out', strategy := 'bfs', hydrate := false)" "lazy"
  for direction in out in any; do
    run_case "sparse_allow" "node" "p4_node_dfs_${direction}_eager" "scalar" "dfs_${direction}_depth_4" 1 \
      "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 4, edge_types := ARRAY['next'], direction := '${direction}', strategy := 'dfs', hydrate := false)" "eager"
    run_case "sparse_allow" "node" "p4_node_dfs_${direction}_lazy" "scalar" "dfs_${direction}_depth_4" 1 \
      "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 4, edge_types := ARRAY['next'], direction := '${direction}', strategy := 'dfs', hydrate := false)" "lazy"
  done
  run_case "broad_allow" "node" "p4_node_path_eager" "scalar" "shortest_path" 5 \
    "SELECT count(*) FROM graph.shortest_path('public.rls_bench_nodes'::regclass, '100', 'public.rls_bench_nodes'::regclass, '104', max_depth := 20, hydrate := false)" "eager"
  run_case "broad_allow" "node" "p4_node_path_lazy" "scalar" "shortest_path" 5 \
    "SELECT count(*) FROM graph.shortest_path('public.rls_bench_nodes'::regclass, '100', 'public.rls_bench_nodes'::regclass, '104', max_depth := 20, hydrate := false)" "lazy"
  run_case "broad_allow" "node" "p4_node_weighted_path_eager" "scalar" "weighted_shortest_path" 5 \
    "SELECT count(*) FROM graph.weighted_shortest_path('public.rls_bench_nodes'::regclass, '100', 'public.rls_bench_nodes'::regclass, '104', ARRAY['edge_row'])" "eager"
  run_case "broad_allow" "node" "p4_node_weighted_path_lazy" "scalar" "weighted_shortest_path" 5 \
    "SELECT count(*) FROM graph.weighted_shortest_path('public.rls_bench_nodes'::regclass, '100', 'public.rls_bench_nodes'::regclass, '104', ARRAY['edge_row'])" "lazy"
elif [[ "$RUN_PROFILE" == "compact" ]]; then
  run_case "broad_allow" "node" "node_broad_allow_scalar_depth0" "scalar" "depth0" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 0, hydrate := false)"
  run_case "sparse_allow" "node" "node_sparse_allow_composite_depth0" "composite" "depth0" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_composite'::regclass, jsonb_build_array('7'::text, '100'::text)::text, 0, hydrate := false)"
  run_case "sparse_deny" "node" "node_sparse_deny_scalar_depth0" "scalar" "depth0" 0 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 0, hydrate := false)"
else
  run_cases "broad_allow" "node"
  run_cases "sparse_allow" "node"
  run_cases "sparse_deny" "node"
fi

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER TABLE public.rls_bench_nodes DISABLE ROW LEVEL SECURITY; ALTER TABLE public.rls_bench_composite DISABLE ROW LEVEL SECURITY; ALTER TABLE public.rls_bench_edges ENABLE ROW LEVEL SECURITY;"
capture_source_plans "broad_allow" "edge" "rls_bench_edges" "src.id::text"
capture_source_plans "sparse_allow" "edge" "rls_bench_edges" "src.id::text"
capture_source_plans "sparse_deny" "edge" "rls_bench_edges" "src.id::text"
if [[ "$RUN_PROFILE" == "p3_selective" ]]; then
  capture_lazy_probe_plan "sparse_allow" "edge" "rls_bench_edges" "id" '["99","100","101","199","200","201"]'
  run_case "sparse_allow" "edge" "p3_edge_visible_eager" "scalar" "edge_one_hop" 2 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['edge_row'], direction := 'out', strategy := 'bfs', hydrate := false)" "eager"
  run_case "sparse_allow" "edge" "p3_edge_visible_lazy" "scalar" "edge_one_hop" 2 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['edge_row'], direction := 'out', strategy := 'bfs', hydrate := false)" "lazy"
  run_case "sparse_allow" "edge" "p3_edge_hidden_eager" "scalar" "edge_one_hop" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '101', 1, edge_types := ARRAY['edge_row'], direction := 'out', strategy := 'bfs', hydrate := false)" "eager"
  run_case "sparse_allow" "edge" "p3_edge_hidden_lazy" "scalar" "edge_one_hop" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '101', 1, edge_types := ARRAY['edge_row'], direction := 'out', strategy := 'bfs', hydrate := false)" "lazy"
elif [[ "$RUN_PROFILE" == "compact" ]]; then
  run_case "sparse_allow" "edge" "edge_sparse_allow_one_hop" "scalar" "edge_one_hop" 2 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['edge_row'], hydrate := false)"
  run_case "sparse_deny" "edge" "edge_sparse_deny_one_hop" "scalar" "edge_one_hop" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['edge_row'], hydrate := false)"
else
  run_cases "broad_allow" "edge"
  run_cases "sparse_allow" "edge"
  run_cases "sparse_deny" "edge"
fi

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "ALTER TABLE public.rls_bench_nodes ENABLE ROW LEVEL SECURITY; ALTER TABLE public.rls_bench_composite ENABLE ROW LEVEL SECURITY;"
if [[ "$RUN_PROFILE" == "p3_selective" ]]; then
  run_case "sparse_allow" "combined" "p3_combined_eager" "scalar" "edge_one_hop" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['edge_row'], direction := 'out', strategy := 'bfs', hydrate := false)" "eager"
  run_case "sparse_allow" "combined" "p3_combined_lazy" "scalar" "edge_one_hop" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['edge_row'], direction := 'out', strategy := 'bfs', hydrate := false)" "lazy"
elif [[ "$RUN_PROFILE" == "compact" ]]; then
  run_case "sparse_allow" "combined" "combined_sparse_allow_one_hop" "scalar" "edge_one_hop" 1 \
    "SELECT count(*) FROM graph.traverse('public.rls_bench_nodes'::regclass, '100', 1, edge_types := ARRAY['edge_row'], hydrate := false)"
else
  run_cases "broad_allow" "combined"
  run_cases "sparse_allow" "combined"
  run_cases "sparse_deny" "combined"
fi

if [[ "$RUN_PROFILE" == "p3_selective" ]]; then
  psql -X -q -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
DO $p3_gate$
DECLARE
    mismatches bigint;
    eager_samples bigint;
    lazy_samples bigint;
    paired_samples bigint;
    lazy_max_source_rows bigint;
    eager_min_source_rows bigint;
BEGIN
    SELECT count(*) FILTER (WHERE strategy = 'eager'),
           count(*) FILTER (WHERE strategy = 'lazy')
    INTO eager_samples, lazy_samples
    FROM public.rls_bench_samples;
    IF eager_samples = 0 OR lazy_samples = 0 OR eager_samples <> lazy_samples THEN
        RAISE EXCEPTION 'P3 selective benchmark did not retain balanced strategies: eager=%, lazy=%',
            eager_samples, lazy_samples;
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name = 'p3_no_rls_auto'
          AND strategy = 'auto'
          AND spi_calls = 0
          AND source_rows = 0
    ) THEN
        RAISE EXCEPTION 'P3 no-RLS auto route did not retain the zero-probe eager fast path';
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name = 'p4_weighted_path_no_rls_auto'
          AND strategy = 'auto'
          AND spi_calls = 0
          AND source_rows = 0
    ) THEN
        RAISE EXCEPTION 'P4 weighted-path no-RLS auto route did not retain the zero-probe eager fast path';
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name = 'p4_dfs_no_rls_auto'
          AND strategy = 'auto'
          AND spi_calls = 0
          AND source_rows = 0
    ) THEN
        RAISE EXCEPTION 'P4 DFS no-RLS auto route did not retain the zero-probe eager fast path';
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM public.rls_bench_samples
        WHERE case_name = 'p4_path_no_rls_auto'
          AND strategy = 'auto'
          AND spi_calls = 0
          AND source_rows = 0
    ) THEN
        RAISE EXCEPTION 'P4 path no-RLS auto route did not retain the zero-probe eager fast path';
    END IF;

    SELECT count(*)
    INTO paired_samples
    FROM public.rls_bench_samples AS eager
    JOIN public.rls_bench_samples AS lazy
      ON replace(eager.case_name, '_eager', '') = replace(lazy.case_name, '_lazy', '')
     AND eager.sample = lazy.sample
    WHERE eager.strategy = 'eager'
      AND lazy.strategy = 'lazy';
    IF paired_samples <> eager_samples THEN
        RAISE EXCEPTION 'P3 selective benchmark has unpaired samples: eager=%, paired=%',
            eager_samples, paired_samples;
    END IF;

    SELECT count(*)
    INTO mismatches
    FROM public.rls_bench_samples AS eager
    JOIN public.rls_bench_samples AS lazy
      ON replace(eager.case_name, '_eager', '') = replace(lazy.case_name, '_lazy', '')
     AND eager.sample = lazy.sample
    WHERE eager.strategy = 'eager'
      AND lazy.strategy = 'lazy'
      AND eager.result_rows IS DISTINCT FROM lazy.result_rows;
    IF mismatches <> 0 THEN
        RAISE EXCEPTION 'P3 selective eager/lazy row-count mismatches: %', mismatches;
    END IF;

    SELECT max(source_rows) FILTER (WHERE strategy = 'lazy'),
           min(source_rows) FILTER (WHERE strategy = 'eager')
    INTO lazy_max_source_rows, eager_min_source_rows
    FROM public.rls_bench_samples;
    IF lazy_max_source_rows IS NULL OR eager_min_source_rows IS NULL THEN
        RAISE EXCEPTION 'P3 visibility source-work metrics were NULL';
    END IF;
    IF lazy_max_source_rows > 64 THEN
        RAISE EXCEPTION 'P3 lazy source work is not bounded: max source_rows=%', lazy_max_source_rows;
    END IF;
    IF eager_min_source_rows <= lazy_max_source_rows THEN
        RAISE EXCEPTION 'P3 benchmark did not distinguish eager and lazy source work: eager min=%, lazy max=%',
            eager_min_source_rows, lazy_max_source_rows;
    END IF;
END
$p3_gate$;
SQL
fi

RUN_STATUS="complete"
ACTIVE_CASE=""
export_evidence
echo "Retained eager-RLS baseline: $OUTPUT_DIR"
