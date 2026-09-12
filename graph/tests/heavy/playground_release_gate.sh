#!/usr/bin/env bash
# Validate that Streamlit playground SQL examples still produce expected results.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
SANDBOX_DIR="${ROOT_DIR}/sandbox"

PREPARE_PLAYGROUND="${PREPARE_PLAYGROUND:-1}"
PG_PORT="${PGGRAPH_PG_PORT:-55432}"
CONTAINER_NAME="${PGGRAPH_CONTAINER_NAME:-pggraph-sandbox}"
IMAGE_NAME="${PGGRAPH_IMAGE_NAME:-pggraph-postgres:17}"
PLAYGROUND_DATASET="${PGGRAPH_PLAYGROUND_DATASET:-panama}"
PLAYGROUND_MODE="${PGGRAPH_PLAYGROUND_MODE:-csr}"

if [[ "${1:-}" == "--all-modes" ]]; then
  shift
  # Prepare each catalog's matching build mode. The second pass reuses the
  # image and container created by the first pass.
  PREPARE_PLAYGROUND=1 PGGRAPH_PLAYGROUND_MODE=csr \
    bash "$0" "$@"
  PREPARE_PLAYGROUND=1 PGGRAPH_PLAYGROUND_MODE=mutable \
    PGGRAPH_REBUILD_IMAGE=0 PGGRAPH_RECREATE_CONTAINER=0 \
    bash "$0" "$@"
  exit 0
fi

case "${PLAYGROUND_MODE}" in
  csr|csr_readonly)
    PLAYGROUND_MODE="csr"
    BUILD_MODE="csr_readonly"
    ;;
  mutable|mutable_overlay)
    PLAYGROUND_MODE="mutable"
    BUILD_MODE="mutable_overlay"
    ;;
  *)
    echo "Error: unsupported playground mode '${PLAYGROUND_MODE}'. Use csr or mutable." >&2
    exit 2
    ;;
esac

if [[ "${PREPARE_PLAYGROUND}" == "1" ]]; then
  # shellcheck source=../../../sandbox/common/docker.sh
  source "${SANDBOX_DIR}/common/docker.sh"

  require_docker
  ensure_pggraph_image "${ROOT_DIR}" "${IMAGE_NAME}"
  ensure_pggraph_container "${CONTAINER_NAME}" "${IMAGE_NAME}" "${PG_PORT}"
  ACTUAL_PG_PORT="$(pggraph_container_host_port "${CONTAINER_NAME}")"

  prepare_args=(
    "${SANDBOX_DIR}/common/run_benchmarks.py"
    --dataset "${PLAYGROUND_DATASET}"
    --container "${CONTAINER_NAME}"
    --host 127.0.0.1
    --port "${ACTUAL_PG_PORT}"
    --database postgres
    --user postgres
    --password postgres
    --datasets-dir "${SANDBOX_DIR}/benchmark/datasets"
    --results-dir "${SANDBOX_DIR}/benchmark/results"
    --build-mode "${BUILD_MODE}"
    --prepare-only
  )
  if [[ "${PGGRAPH_PLAYGROUND_YES:-0}" == "1" ]]; then
    prepare_args+=(--yes)
  fi

  python3 "${prepare_args[@]}"

  if [[ -z "${PGGRAPH_DSN:-}" && -z "${PGGRAPH_PLAYGROUND_DSN:-}" ]]; then
    export PGGRAPH_DSN="postgresql://postgres:postgres@localhost:${ACTUAL_PG_PORT}/postgres"
  fi
fi

python3 "${SCRIPT_DIR}/playground_release_gate.py" --mode "${PLAYGROUND_MODE}" "$@"
