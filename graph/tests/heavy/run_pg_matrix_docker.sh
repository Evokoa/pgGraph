#!/usr/bin/env bash
set -euo pipefail

IMAGE="${IMAGE:-pggraph:pg-matrix}"
PG_VERSIONS="${PG_VERSIONS:-14 15 16 17 18}"
RUN_PGRX_SQL="${RUN_PGRX_SQL:-1}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DOCKERFILE="$ROOT_DIR/graph/tests/heavy/Dockerfile.pg-matrix"

docker build \
  --build-arg "PG_VERSIONS=${PG_VERSIONS}" \
  --build-arg "RUN_PGRX_SQL=${RUN_PGRX_SQL}" \
  -f "$DOCKERFILE" \
  -t "$IMAGE" \
  "$ROOT_DIR"

echo "Docker PostgreSQL matrix passed for versions: $PG_VERSIONS"
