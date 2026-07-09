#!/usr/bin/env bash
# Run opt-in build memory stress profiles.
#
# Usage:
#   cd graph
#   PG_VERSION_FEATURE=pg17 ./tests/heavy/build_memory_stress.sh
#
# Profiles:
# - baseline: large persisted build with the default batch size.
# - small_batch: same shape with smaller SPI/spool batches.
# - rebuild: repeated persisted graph.build() calls in one backend to catch
#   rebuild-time memory spikes.
# - low_memory_rebuild: repeated persisted rebuilds with graph.low_memory_build
#   enabled so the current backend graph may be unloaded before replacement.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
DB_PREFIX="${DB_PREFIX:-pggraph_memstress}"
NODE_COUNT="${NODE_COUNT:-200000}"
EDGE_COUNT="${EDGE_COUNT:-199999}"
BASELINE_BATCH_SIZE="${BASELINE_BATCH_SIZE:-10000}"
SMALL_BATCH_SIZE="${SMALL_BATCH_SIZE:-1000}"
REBUILD_ROUNDS="${REBUILD_ROUNDS:-2}"
MAX_RSS_MB="${MAX_RSS_MB:-0}"
RUN_BASELINE="${RUN_BASELINE:-1}"
RUN_SMALL_BATCH="${RUN_SMALL_BATCH:-1}"
RUN_REBUILD="${RUN_REBUILD:-1}"
RUN_LOW_MEMORY_REBUILD="${RUN_LOW_MEMORY_REBUILD:-1}"

run_profile() {
  local profile="$1"
  local batch_size="$2"
  local rebuild_rounds="$3"
  local low_memory_build="${4:-off}"

  echo "build memory stress profile: ${profile}"
  DBNAME="${DB_PREFIX}_${profile}" \
    PG_VERSION_FEATURE="$PG_VERSION_FEATURE" \
    NODE_COUNT="$NODE_COUNT" \
    EDGE_COUNT="$EDGE_COUNT" \
    BUILD_BATCH_SIZE="$batch_size" \
    REBUILD_ROUNDS="$rebuild_rounds" \
    LOW_MEMORY_BUILD="$low_memory_build" \
    MAX_RSS_MB="$MAX_RSS_MB" \
    "$SCRIPT_DIR/measure_build_rss.sh"
}

if [[ "$RUN_BASELINE" == "1" ]]; then
  run_profile "baseline" "$BASELINE_BATCH_SIZE" 0
fi

if [[ "$RUN_SMALL_BATCH" == "1" ]]; then
  run_profile "small_batch" "$SMALL_BATCH_SIZE" 0
fi

if [[ "$RUN_REBUILD" == "1" ]]; then
  run_profile "rebuild" "$BASELINE_BATCH_SIZE" "$REBUILD_ROUNDS"
fi

if [[ "$RUN_LOW_MEMORY_REBUILD" == "1" ]]; then
  run_profile "low_memory_rebuild" "$BASELINE_BATCH_SIZE" "$REBUILD_ROUNDS" "on"
fi

echo "Build memory stress passed for $PG_VERSION_FEATURE"
