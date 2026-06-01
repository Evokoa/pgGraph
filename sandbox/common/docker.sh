#!/usr/bin/env bash

docker_install_help() {
  cat <<'EOF'
If you need to install Docker, see:
  Mac:     https://docs.docker.com/desktop/setup/install/mac-install/
  Windows: https://docs.docker.com/desktop/setup/install/windows-install/
  Linux:   https://docs.docker.com/desktop/setup/install/linux/
EOF
}

require_docker() {
  if ! command -v docker >/dev/null 2>&1; then
    echo "Error: Docker is not installed or not available on PATH." >&2
    docker_install_help >&2
    exit 1
  fi

  if ! docker info >/dev/null 2>&1; then
    echo "Error: Cannot connect to the Docker daemon. Is Docker Desktop running?" >&2
    docker_install_help >&2
    exit 1
  fi
}

ensure_pggraph_image() {
  local root_dir="$1"
  local image_name="$2"

  if [[ "${PGGRAPH_REBUILD_IMAGE:-0}" != "1" ]] && docker image inspect "${image_name}" >/dev/null 2>&1; then
    return 0
  fi

  echo "Building ${image_name} from the repo Dockerfile. This can take several minutes."
  docker build --build-arg PG_MAJOR=17 -t "${image_name}" "${root_dir}"
}

ensure_pggraph_container() {
  local container_name="$1"
  local image_name="$2"
  local pg_port="$3"

  if [[ "${PGGRAPH_RECREATE_CONTAINER:-0}" == "1" ]] \
    && docker ps -a --format '{{.Names}}' | grep -Fxq "${container_name}"; then
    docker rm -f "${container_name}" >/dev/null
  fi

  if docker ps --format '{{.Names}}' | grep -Fxq "${container_name}"; then
    wait_for_postgres "${container_name}"
    ensure_graph_extension "${container_name}"
    return 0
  fi

  if docker ps -a --format '{{.Names}}' | grep -Fxq "${container_name}"; then
    if ! docker start "${container_name}" >/dev/null; then
      echo "Error: Failed to start Docker container ${container_name}." >&2
      echo "If the configured PostgreSQL port is already in use, choose another port:" >&2
      echo "  PGGRAPH_PG_PORT=55433 sandbox/run_benchmarks.sh panama --yes" >&2
      exit 1
    fi
  else
    if ! docker run \
      --name "${container_name}" \
      -e POSTGRES_PASSWORD=postgres \
      -p "${pg_port}:5432" \
      -d "${image_name}" >/dev/null; then
      echo "Error: Failed to start Docker container ${container_name} on host port ${pg_port}." >&2
      echo "If the port is already allocated, choose another port:" >&2
      echo "  PGGRAPH_PG_PORT=55433 sandbox/run_benchmarks.sh panama --yes" >&2
      exit 1
    fi
  fi

  wait_for_postgres "${container_name}"
  ensure_graph_extension "${container_name}"
}

pggraph_container_host_port() {
  local container_name="$1"
  local published

  published="$(docker port "${container_name}" 5432/tcp 2>/dev/null | awk 'NR == 1 { print $0 }')"
  if [ -z "${published}" ]; then
    echo "Error: Docker container ${container_name} does not publish PostgreSQL port 5432." >&2
    return 1
  fi

  printf '%s\n' "${published##*:}"
}

wait_for_postgres() {
  local container_name="$1"

  echo "Waiting for PostgreSQL in ${container_name}..."
  for _ in $(seq 1 60); do
    if docker exec "${container_name}" pg_isready -U postgres >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done

  echo "Error: PostgreSQL did not become ready in container ${container_name}." >&2
  docker logs "${container_name}" >&2 || true
  exit 1
}

ensure_graph_extension() {
  local container_name="$1"

  docker exec "${container_name}" psql -U postgres -d postgres -v ON_ERROR_STOP=1 \
    -c 'CREATE EXTENSION IF NOT EXISTS graph;' >/dev/null
}

open_browser() {
  local url="$1"

  if command -v open >/dev/null 2>&1; then
    open "${url}" >/dev/null 2>&1 || true
  elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "${url}" >/dev/null 2>&1 || true
  else
    echo "Open ${url} in your browser."
  fi
}
