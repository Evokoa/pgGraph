#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "run_open_type_query_resource_matrix.sh requires Linux" >&2
  exit 2
fi
RUN_ID="${RUN_ID:?RUN_ID is required}"
if [[ ! "$RUN_ID" =~ ^[0-9a-f]{40}$ ]]; then
  echo "RUN_ID must be a full lowercase Git commit" >&2
  exit 2
fi
OUTPUT_DIR="${OUTPUT_DIR:?OUTPUT_DIR is required}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_CONFIG="${PG_CONFIG:-/usr/lib/postgresql/17/bin/pg_config}"
LABEL_COUNT=65536

mkdir -p "$OUTPUT_DIR/raw/resources"
printf 'run_id\tlabel_count\tbackend_count\tphase\tsample_id\tepoch_ms\tbackend\tpid\trss_bytes\tpss_bytes\n' \
  >"$OUTPUT_DIR/resource-samples.tsv"
printf 'run_id,label_count,backend_count,query_surface,filter_shape,degree,depth,projection_artifact_bytes,max_per_backend_rss_bytes,max_per_backend_pss_bytes,max_idle_total_rss_bytes,max_idle_total_pss_bytes,max_loaded_total_rss_bytes,max_loaded_total_pss_bytes,max_query_total_rss_bytes,max_query_total_pss_bytes,query_minus_idle_total_rss_bytes,query_minus_idle_total_pss_bytes\n' \
  >"$OUTPUT_DIR/resource-results.csv"

skip_install=0
for backend_count in 1 4 8; do
  case_run_id="${RUN_ID}-b${backend_count}"
  run_output="$OUTPUT_DIR/raw/resources/backend-${backend_count}"
  mkdir -p "$run_output"
  PG_VERSION_FEATURE="$PG_VERSION_FEATURE" \
  PG_CONFIG="$PG_CONFIG" \
  DBNAME="pggraph_open_type_resource_${backend_count}" \
  OUTPUT_DIR="$run_output" \
  BACKEND_COUNT="$backend_count" \
  LABEL_COUNT="$LABEL_COUNT" \
  NODE_COUNT=131072 \
  DEGREE=8 \
  DEPTH=4 \
  QUERY_ROUNDS=100 \
  MAX_DIRECTED_EDGES=1048576 \
  SKIP_INSTALL="$skip_install" \
  bash ./tests/heavy/open_type_query_resources.sh
  skip_install=1
  awk -F '\t' -v run_id="$case_run_id" -v labels="$LABEL_COUNT" -v backends="$backend_count" \
    'BEGIN {OFS="\t"} NR > 1 {print run_id, labels, backends, $0}' \
    "$run_output/resource-samples.tsv" >>"$OUTPUT_DIR/resource-samples.tsv"
  awk -F ',' -v run_id="$case_run_id" 'BEGIN {OFS=","} NR > 1 {print run_id, $0}' \
    "$run_output/resources.csv" >>"$OUTPUT_DIR/resource-results.csv"
done

echo "P9 Linux resource matrix retained in $OUTPUT_DIR"
