#!/usr/bin/env bash
set -euo pipefail

GRAPH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REPO_ROOT="$(cd "$GRAPH_DIR/.." && pwd)"
EVIDENCE_DIR="${EVIDENCE_DIR:-$REPO_ROOT/todo/measurements/2026-08-13-p9-open-type-query}"
CRITERION_ROOT="$GRAPH_DIR/target/criterion"

mkdir -p "$EVIDENCE_DIR"
EVIDENCE_DIR="$(cd "$EVIDENCE_DIR" && pwd)"

rm -f -- \
  "$EVIDENCE_DIR/criterion-estimates.csv" \
  "$EVIDENCE_DIR/criterion-results.csv" \
  "$EVIDENCE_DIR/criterion-raw-hashes.csv" \
  "$EVIDENCE_DIR/criterion-run.log"
rm -rf -- "$EVIDENCE_DIR/raw/criterion"

for group in open_type_registry_lookup open_type_filter_resolution open_type_bfs; do
  rm -rf -- "$CRITERION_ROOT/$group"
done

mkdir -p "$EVIDENCE_DIR/raw/criterion"
(
  cd "$GRAPH_DIR"
  cargo +1.96.0 bench --features "pg17 benchmarks" --bench open_type_query_bench
) 2>&1 | tee "$EVIDENCE_DIR/criterion-run.log"

python3 "$REPO_ROOT/scripts/extract_p9_open_type_criterion.py" \
  --criterion-root "$CRITERION_ROOT" \
  --evidence-dir "$EVIDENCE_DIR"

echo "P9 Criterion evidence retained in $EVIDENCE_DIR"
