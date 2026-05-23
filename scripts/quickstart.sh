#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'USAGE'
Run pgGraph quickstart and install/demo workflows.

Usage:
  scripts/quickstart.sh [quickstart|docker|pgrx|playground|clean]

Platforms:
  macOS/Linux terminal, or Windows via WSL2/Git Bash with Docker Desktop.
  This is not a native PowerShell or Command Prompt script.

Commands:
  quickstart             Build/start disposable PostgreSQL, create and load the
                         people/companies demo, build pgGraph, and run example
                         queries. Default.
  docker [CONTAINER]     Install pgGraph into an existing running PostgreSQL
                         Docker container. Example:
                         scripts/quickstart.sh docker my-postgres 17 appdb postgres
  pgrx [PG_MAJOR]       Source build and install pgGraph with pgrx into local
                         PostgreSQL (defaults to pg17). Optionally pass major only;
                         DB target flags are env-driven (see docs below).
  playground [DATASET]   Start the Streamlit playground preloaded with a preset
                         dataset. Supported dataset values: panama, ldbc.
                         Example:
                         scripts/quickstart.sh playground panama
  clean                  Stop the Compose database and remove its volume.

Legacy:
  demo  Alias for quickstart.
  setup Keep previous behavior (start PostgreSQL with pgGraph installed, but do not
        load the sample graph tables).
  psql  Build/start disposable PostgreSQL, create the demo graph, then open psql.
USAGE
}

docker_install_help() {
  cat <<'EOF'
If you need to install Docker, see:
  Mac:     https://docs.docker.com/desktop/setup/install/mac-install/
  Windows: https://docs.docker.com/desktop/setup/install/windows-install/
  Linux:   https://docs.docker.com/desktop/setup/install/linux/
EOF
}

require_docker_compose() {
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

  if ! docker compose version >/dev/null 2>&1; then
    echo "Error: Docker Compose v2 is required. Install Docker Desktop or the docker compose plugin." >&2
    docker_install_help >&2
    exit 1
  fi
}

compose() {
  docker compose --project-directory "${ROOT_DIR}" "$@"
}

wait_for_postgres() {
  echo "Waiting for PostgreSQL..."
  for _ in $(seq 1 60); do
    if compose exec -T postgres pg_isready -U postgres >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done

  echo "Error: PostgreSQL did not become ready." >&2
  compose logs postgres >&2 || true
  exit 1
}

start_postgres() {
  require_docker_compose
  compose up --build -d postgres
  wait_for_postgres
  compose exec -T postgres psql -U postgres -d graph -v ON_ERROR_STOP=1 \
    -c 'CREATE EXTENSION IF NOT EXISTS graph;' >/dev/null
}

prepare_demo_graph() {
  compose exec -T postgres psql -U postgres -d graph -v ON_ERROR_STOP=1 >/dev/null <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();

DROP TABLE IF EXISTS people;
DROP TABLE IF EXISTS companies;

CREATE TABLE companies (
    id text PRIMARY KEY,
    name text NOT NULL
);

CREATE TABLE people (
    id text PRIMARY KEY,
    name text NOT NULL,
    company_id text REFERENCES companies(id)
);

INSERT INTO companies VALUES
    ('c1', 'Acme Bank'),
    ('c2', 'Northwind Trading');

INSERT INTO people VALUES
    ('p1', 'Alice', 'c1'),
    ('p2', 'Bob', 'c1'),
    ('p3', 'Carol', 'c2');

SELECT * FROM graph.auto_discover('public');
SQL
}

run_demo_sql() {
  compose exec -T postgres psql -U postgres -d graph -v ON_ERROR_STOP=1 <<'SQL'
\pset pager off

CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();

DROP TABLE IF EXISTS people;
DROP TABLE IF EXISTS companies;

CREATE TABLE companies (
    id text PRIMARY KEY,
    name text NOT NULL
);

CREATE TABLE people (
    id text PRIMARY KEY,
    name text NOT NULL,
    company_id text REFERENCES companies(id)
);

INSERT INTO companies VALUES
    ('c1', 'Acme Bank'),
    ('c2', 'Northwind Trading');

INSERT INTO people VALUES
    ('p1', 'Alice', 'c1'),
    ('p2', 'Bob', 'c1'),
    ('p3', 'Carol', 'c2');

\echo ''
\echo 'pgGraph discovery and build'
SELECT * FROM graph.auto_discover('public');

\echo ''
\echo 'Example data: ordinary PostgreSQL tables'
SELECT
    p.name AS person,
    c.name AS company
FROM people p
JOIN companies c ON c.id = p.company_id
ORDER BY p.name;

\echo ''
\echo 'Graph search: name = Alice'
SELECT node_table_name, node_id, node->>'name' AS name
FROM graph.search('name', 'Alice', mode := 'exact');

\echo ''
\echo 'Graph traversal: Alice, 1 hop'
SELECT depth, node_table_name, node_id, node->>'name' AS name, edge_path
FROM graph.traverse('people'::regclass, 'p1', 1)
ORDER BY depth, node_table_name, node_id;

\echo ''
\echo 'Graph shortest path: Alice -> Acme Bank'
SELECT step, node_table_name, node_id, node->>'name' AS name, edge_label
FROM graph.shortest_path(
    'people'::regclass, 'p1',
    'companies'::regclass, 'c1'
)
ORDER BY step;
SQL
}

install_into_existing_docker_container() {
  local container="$1"
  local pg_major="${2:-17}"
  local db_name="${3:-postgres}"
  local db_user="${4:-postgres}"

  if [[ -z "${container}" ]]; then
    echo "Error: CONTAINER is required for docker mode." >&2
    echo "Usage: scripts/quickstart.sh docker CONTAINER [PG_MAJOR] [DB_NAME] [DB_USER]" >&2
    exit 2
  fi

  "${ROOT_DIR}/scripts/install_into_docker_postgres.sh" \
    "${container}" \
    "${pg_major}" \
    "${db_name}" \
    "${db_user}"
}

install_with_local_pgrx() {
  local pg_major="${1:-17}"
  local db_name="${PGDATABASE:-postgres}"
  local db_user="${PGUSER:-postgres}"
  local db_host="${PGHOST:-localhost}"
  local db_port="${PGPORT:-5432}"
  local pg_config="${2:-${LOCAL_PG_CONFIG:-${PG_CONFIG:-}}}"
  local db_password="${PGPASSWORD:-}"
  local -a feature_args

  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo is required for local pgrx install." >&2
    exit 1
  fi

  if ! command -v psql >/dev/null 2>&1; then
    echo "Error: psql is required for local pgrx install." >&2
    exit 1
  fi

  if [[ ! "${pg_major}" =~ ^(14|15|16|17|18)$ ]]; then
    echo "Error: Unsupported PostgreSQL major ${pg_major}. Supported: 14 15 16 17 18." >&2
    exit 2
  fi

  if [[ -z "${pg_config}" ]]; then
    if ! command -v pg_config >/dev/null 2>&1; then
      echo "Error: pg_config not found. Provide LOCAL_PG_CONFIG or PG_CONFIG." >&2
      exit 1
    fi
    pg_config="$(command -v pg_config)"
  fi

  if [[ ! -x "${pg_config}" ]]; then
    echo "Error: pg_config is not executable: ${pg_config}" >&2
    exit 1
  fi

  if [[ "${db_port}" =~ ^[0-9]+$ ]] && (( db_port < 1 || db_port > 65535 )); then
    echo "Error: DB port must be between 1 and 65535." >&2
    exit 2
  fi

  feature_args=("--features" "pg${pg_major}" "--no-default-features" "--pg-config=${pg_config}")

  echo "Building and installing pgGraph for PostgreSQL ${pg_major} using pgrx..."
  (cd "${ROOT_DIR}/graph" && cargo pgrx install "${feature_args[@]}")

  if [[ -n "${db_password}" ]]; then
    PGPASSWORD="${db_password}" \
      psql -h "${db_host}" -p "${db_port}" -U "${db_user}" -d "${db_name}" -v ON_ERROR_STOP=1 \
        -c 'CREATE EXTENSION IF NOT EXISTS graph;'
    return
  fi

  psql -h "${db_host}" -p "${db_port}" -U "${db_user}" -d "${db_name}" -v ON_ERROR_STOP=1 \
    -c 'CREATE EXTENSION IF NOT EXISTS graph;'
}

start_playground() {
  local dataset="${1:-panama}"

  case "${dataset}" in
    panama|ldbc)
      ;;
    *)
      echo "Error: unsupported dataset '${dataset}'. Use panama or ldbc." >&2
      exit 2
      ;;
  esac

  PGGRAPH_PLAYGROUND_DATASET="${dataset}" \
    "${ROOT_DIR}/sandbox/start_playground.sh"
}

main() {
  local command="${1:-quickstart}"
  local container=""
  local pg_major=""
  local db_name=""
  local db_user=""

  case "${command}" in
    quickstart|demo)
      start_postgres
      run_demo_sql
      echo ""
      echo "Done. Re-run queries with: scripts/quickstart.sh psql"
      ;;
    setup)
      start_postgres
      echo "PostgreSQL is running with pgGraph installed."
      echo "Open psql with: scripts/quickstart.sh psql"
      ;;
    docker)
      container="${2:-}"
      pg_major="${3:-17}"
      db_name="${4:-postgres}"
      db_user="${5:-postgres}"
      install_into_existing_docker_container "${container}" "${pg_major}" "${db_name}" "${db_user}"
      ;;
    pgrx)
      install_with_local_pgrx "${2:-17}" "${3:-}"
      ;;
    playground)
      start_playground "${2:-panama}"
      ;;
    psql)
      start_postgres
      prepare_demo_graph
      echo "Demo graph is ready: people, companies, and pgGraph metadata are built."
      exec docker compose --project-directory "${ROOT_DIR}" exec postgres psql -U postgres -d graph
      ;;
    clean)
      require_docker_compose
      compose down -v
      ;;
    -h|--help|help)
      usage
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
}

main "$@"
