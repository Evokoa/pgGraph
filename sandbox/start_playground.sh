#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SANDBOX_DIR="${ROOT_DIR}/sandbox"
VENV_DIR="${SANDBOX_DIR}/playground/.venv"

# shellcheck source=common/docker.sh
source "${SANDBOX_DIR}/common/docker.sh"

PG_PORT="${PGGRAPH_PG_PORT:-55432}"
CONTAINER_NAME="${PGGRAPH_CONTAINER_NAME:-pggraph-sandbox}"
IMAGE_NAME="${PGGRAPH_IMAGE_NAME:-pggraph-postgres:17}"
PLAYGROUND_DATASET="${PGGRAPH_PLAYGROUND_DATASET:-panama}"

require_docker
ensure_pggraph_image "${ROOT_DIR}" "${IMAGE_NAME}"
ensure_pggraph_container "${CONTAINER_NAME}" "${IMAGE_NAME}" "${PG_PORT}"
ACTUAL_PG_PORT="$(pggraph_container_host_port "${CONTAINER_NAME}")"
if [ "${ACTUAL_PG_PORT}" != "${PG_PORT}" ]; then
  echo "Using PostgreSQL host port ${ACTUAL_PG_PORT} from existing container ${CONTAINER_NAME}."
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "Error: python3 is required to start the playground." >&2
  exit 1
fi

PLAYGROUND_PYTHON=""
for candidate in python3.13 python3.12 python3.11 python3.10 python3; do
  if command -v "${candidate}" >/dev/null 2>&1; then
    if "${candidate}" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' >/dev/null 2>&1; then
      PLAYGROUND_PYTHON="$(command -v "${candidate}")"
      break
    fi
  fi
done

if [ -z "${PLAYGROUND_PYTHON}" ]; then
  echo "Error: the Streamlit playground requires Python 3.10 or newer." >&2
  echo "Install Python 3.10+ or set PATH so python3.10, python3.11, python3.12, or python3.13 is available." >&2
  exit 1
fi

port_available() {
  local port="$1"

  "${PLAYGROUND_PYTHON}" - "${port}" <<'PY'
import socket
import sys

port = int(sys.argv[1])
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind(("127.0.0.1", port))
    except OSError:
        raise SystemExit(1)
PY
}

choose_app_port() {
  if [ -n "${PGGRAPH_PLAYGROUND_PORT:-}" ]; then
    printf '%s\n' "${PGGRAPH_PLAYGROUND_PORT}"
    return 0
  fi

  for candidate in 3000 5000 8000 8080; do
    if port_available "${candidate}"; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done

  printf '%s\n' "32123"
}

wait_for_playground_ready() {
  local url="$1"
  local pid="$2"

  for _ in $(seq 1 60); do
    if curl -fsS --max-time 2 "${url}" >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "${pid}" >/dev/null 2>&1; then
      wait "${pid}"
      return 1
    fi
    sleep 1
  done

  echo "Error: Streamlit did not become ready at ${url} within 60 seconds." >&2
  return 1
}

USE_SFW_PIP=0
if command -v sfw >/dev/null 2>&1; then
  USE_SFW_PIP=1
else
  echo "Warning: sfw was not found; falling back to direct pip for playground dependencies." >&2
fi

run_venv_pip() {
  if [ "${USE_SFW_PIP}" = "1" ]; then
    PATH="${VENV_DIR}/bin:${PATH}" sfw pip "$@"
    return
  fi

  "${VENV_DIR}/bin/python" -m pip "$@"
}

APP_PORT="$(choose_app_port)"

"${PLAYGROUND_PYTHON}" "${SANDBOX_DIR}/common/run_benchmarks.py" \
  --dataset "${PLAYGROUND_DATASET}" \
  --container "${CONTAINER_NAME}" \
  --host 127.0.0.1 \
  --port "${ACTUAL_PG_PORT}" \
  --database postgres \
  --user postgres \
  --password postgres \
  --datasets-dir "${SANDBOX_DIR}/benchmark/datasets" \
  --results-dir "${SANDBOX_DIR}/benchmark/results" \
  --prepare-only

if [ -d "${VENV_DIR}" ]; then
  if ! "${VENV_DIR}/bin/python" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' >/dev/null 2>&1; then
    echo "Recreating playground venv with Python 3.10+."
    rm -rf "${VENV_DIR}"
  fi
fi

if [ ! -d "${VENV_DIR}" ]; then
  "${PLAYGROUND_PYTHON}" -m venv "${VENV_DIR}"
fi

run_venv_pip install --upgrade pip >/dev/null
run_venv_pip install -r "${SANDBOX_DIR}/playground/requirements.txt"

export PGGRAPH_DSN="host=127.0.0.1 port=${ACTUAL_PG_PORT} dbname=postgres user=postgres password=postgres"
export PGGRAPH_ASSETS_DIR="${ROOT_DIR}/assets"

URL="http://127.0.0.1:${APP_PORT}"
echo "Starting pgGraph playground at ${URL}"
"${VENV_DIR}/bin/streamlit" run "${SANDBOX_DIR}/playground/app.py" \
  --server.address 127.0.0.1 \
  --server.port "${APP_PORT}" \
  --server.headless true \
  --browser.gatherUsageStats false \
  --theme.base dark &
STREAMLIT_PID="$!"

cleanup_streamlit() {
  if kill -0 "${STREAMLIT_PID}" >/dev/null 2>&1; then
    kill "${STREAMLIT_PID}" >/dev/null 2>&1 || true
  fi
}
trap cleanup_streamlit INT TERM

if ! wait_for_playground_ready "${URL}" "${STREAMLIT_PID}"; then
  cleanup_streamlit
  exit 1
fi
open_browser "${URL}"
wait "${STREAMLIT_PID}"
