#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-all-current}"
DBNAME="${DBNAME:-pggraph_alpha_to_v1}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/../../.." && pwd)"
FIXTURE_DIR="${ROOT_DIR}/release/fixtures/alpha-to-v1.0"
PSQL=(psql -X -v ON_ERROR_STOP=1 -d "${DBNAME}")

if [[ ! "${DBNAME}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
  echo "DBNAME must be a simple PostgreSQL identifier" >&2
  exit 2
fi

cleanup_database() {
  dropdb --if-exists "${DBNAME}" >/dev/null 2>&1 || true
}

case "${MODE}" in
  setup-alpha)
    "${PSQL[@]}" -f "${FIXTURE_DIR}/source.sql"
    "${PSQL[@]}" -f "${FIXTURE_DIR}/register.sql"
    ;;
  verify-v1)
    "${PSQL[@]}" -c "CREATE EXTENSION IF NOT EXISTS graph;"
    "${PSQL[@]}" -f "${FIXTURE_DIR}/register.sql"
    "${PSQL[@]}" -f "${FIXTURE_DIR}/verify.sql"
    ;;
  all-current)
    trap cleanup_database EXIT
    cleanup_database
    createdb "${DBNAME}"
    "${PSQL[@]}" -c "CREATE EXTENSION IF NOT EXISTS graph;"
    "${PSQL[@]}" -f "${FIXTURE_DIR}/source.sql"
    "${PSQL[@]}" -f "${FIXTURE_DIR}/register.sql"
    "${PSQL[@]}" -f "${FIXTURE_DIR}/verify.sql"
    ;;
  *)
    echo "Usage: $0 [setup-alpha|verify-v1|all-current]" >&2
    exit 2
    ;;
esac

echo "Alpha-to-1.0 fixture passed (${MODE})."
