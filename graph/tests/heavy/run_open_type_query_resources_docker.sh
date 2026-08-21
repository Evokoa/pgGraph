#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
RUN_ID="${RUN_ID:?RUN_ID is required}"
if [[ ! "$RUN_ID" =~ ^[0-9a-f]{40}$ ]]; then
  echo "RUN_ID must be a full lowercase Git commit" >&2
  exit 2
fi
OUTPUT_DIR="${OUTPUT_DIR:?OUTPUT_DIR is required}"
IMAGE="${IMAGE:-pggraph:p9-open-type-resources-${RUN_ID:0:12}}"
CONTAINER_NAME="pggraph-p9-open-type-resources-${RUN_ID:0:12}"

mkdir -p "$OUTPUT_DIR"
cleanup() {
  docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker build \
  --progress=plain \
  --build-arg PG_VERSIONS=17 \
  --build-arg RUN_RUST_TESTS=0 \
  --build-arg RUN_PGRX_SQL=0 \
  --build-arg RUN_GQL_WRITE_MATRIX=0 \
  --build-arg RUN_PACKAGE_INSTALL_MATRIX=0 \
  --build-arg RUN_CRASH_MATRIX=0 \
  --build-arg RUN_RUNTIME_RESOURCES=0 \
  --build-arg RUN_PG_UPGRADE_MATRIX=0 \
  -f "$ROOT_DIR/graph/tests/heavy/Dockerfile.pg-matrix" \
  -t "$IMAGE" "$ROOT_DIR" 2>&1 | tee "$OUTPUT_DIR/docker-build.log"

docker create --name "$CONTAINER_NAME" --user pggraph \
  "$IMAGE" bash -lc \
  "export PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin CARGO_TARGET_DIR=/tmp/pggraph-target && cd /src/graph && cargo pgrx start pg17 && export PGHOST=localhost PGPORT=28817 && RUN_ID='$RUN_ID' OUTPUT_DIR=/tmp/p9-open-type-resources PG_VERSION_FEATURE=pg17 PG_CONFIG=/usr/lib/postgresql/17/bin/pg_config ./tests/heavy/run_open_type_query_resource_matrix.sh && uname -a > /tmp/p9-open-type-resources/linux-uname.txt && /usr/lib/postgresql/17/bin/pg_config --version > /tmp/p9-open-type-resources/resource-postgres-version.txt && cargo pgrx stop pg17" \
  >/dev/null
docker start -a "$CONTAINER_NAME" 2>&1 | tee "$OUTPUT_DIR/docker-resource.log"
docker cp "$CONTAINER_NAME:/tmp/p9-open-type-resources/." "$OUTPUT_DIR/"
docker image inspect "$IMAGE" >"$OUTPUT_DIR/docker-image-inspect.json"
docker version --format '{{json .}}' >"$OUTPUT_DIR/docker-version.json"

echo "P9 Docker resource evidence copied to $OUTPUT_DIR"
