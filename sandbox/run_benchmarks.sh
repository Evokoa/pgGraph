#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SANDBOX_DIR="${ROOT_DIR}/sandbox"
VENV_DIR="${SANDBOX_DIR}/benchmark/.venv"

# shellcheck source=common/docker.sh
source "${SANDBOX_DIR}/common/docker.sh"

DATASET="${1:-all}"
PG_PORT="${PGGRAPH_PG_PORT:-55432}"
CONTAINER_NAME="${PGGRAPH_CONTAINER_NAME:-pggraph-sandbox}"
IMAGE_NAME="${PGGRAPH_IMAGE_NAME:-pggraph-postgres:17}"
shift || true

if [ "${DATASET}" = "cleanup" ]; then
  CLEANUP_TARGETS=()
  CLEANUP_FLAGS=()
  for arg in "$@"; do
    case "${arg}" in
      datasets|results|venv|docker|all) CLEANUP_TARGETS+=("${arg}") ;;
      *) CLEANUP_FLAGS+=("${arg}") ;;
    esac
  done
  if [ "${#CLEANUP_TARGETS[@]}" -eq 0 ]; then
    CLEANUP_TARGETS=(all)
  fi
  python3 "${SANDBOX_DIR}/common/run_benchmarks.py" \
    --cleanup "${CLEANUP_TARGETS[@]}" \
    --container "${CONTAINER_NAME}" \
    --image "${IMAGE_NAME}" \
    --datasets-dir "${SANDBOX_DIR}/benchmark/datasets" \
    --results-dir "${SANDBOX_DIR}/benchmark/results" \
    --benchmark-venv "${SANDBOX_DIR}/benchmark/.venv" \
    --playground-venv "${SANDBOX_DIR}/playground/.venv" \
    ${CLEANUP_FLAGS[@]+"${CLEANUP_FLAGS[@]}"}
  exit 0
fi

require_docker
ensure_pggraph_image "${ROOT_DIR}" "${IMAGE_NAME}"
ensure_pggraph_container "${CONTAINER_NAME}" "${IMAGE_NAME}" "${PG_PORT}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "Error: python3 is required to run benchmarks." >&2
  exit 1
fi

BENCHMARK_PYTHON=""
for candidate in python3.13 python3.12 python3.11 python3.10 python3; do
  if command -v "${candidate}" >/dev/null 2>&1; then
    if "${candidate}" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' >/dev/null 2>&1; then
      BENCHMARK_PYTHON="$(command -v "${candidate}")"
      break
    fi
  fi
done

if [ -z "${BENCHMARK_PYTHON}" ]; then
  echo "Error: benchmark timing requires Python 3.10 or newer." >&2
  exit 1
fi

if [ -d "${VENV_DIR}" ]; then
  if ! "${VENV_DIR}/bin/python" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' >/dev/null 2>&1; then
    rm -rf "${VENV_DIR}"
  fi
fi

if [ ! -d "${VENV_DIR}" ]; then
  "${BENCHMARK_PYTHON}" -m venv "${VENV_DIR}"
fi

if ! command -v sfw >/dev/null 2>&1; then
  echo "Error: sfw is required before benchmark dependencies can be installed." >&2
  echo "Install sfw, or provision sandbox/benchmark/.venv from requirements.txt ahead of time." >&2
  exit 1
fi

run_venv_pip() {
  PATH="${VENV_DIR}/bin:${PATH}" sfw pip "$@"
}

run_venv_pip install --upgrade pip >/dev/null
run_venv_pip install -r "${SANDBOX_DIR}/benchmark/requirements.txt"

"${VENV_DIR}/bin/python" "${SANDBOX_DIR}/common/run_benchmarks.py" \
  --dataset "${DATASET}" \
  --container "${CONTAINER_NAME}" \
  --image "${IMAGE_NAME}" \
  --host 127.0.0.1 \
  --port "${PG_PORT}" \
  --database postgres \
  --user postgres \
  --password postgres \
  --datasets-dir "${SANDBOX_DIR}/benchmark/datasets" \
  --results-dir "${SANDBOX_DIR}/benchmark/results" \
  --benchmark-venv "${SANDBOX_DIR}/benchmark/.venv" \
  --playground-venv "${SANDBOX_DIR}/playground/.venv" \
  "$@"
