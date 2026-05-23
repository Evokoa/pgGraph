#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Copy an existing pgGraph package into a running PostgreSQL Docker container.

Usage:
  scripts/copy_pggraph_package_to_docker_postgres.sh CONTAINER PACKAGE_DIR [PG_MAJOR] [DB_NAME] [DB_USER]

Arguments:
  CONTAINER    Running PostgreSQL container name or ID.
  PACKAGE_DIR  Local graph-pg<major>/ package directory created by
               scripts/build_docker_pggraph_package.sh.
  PG_MAJOR     PostgreSQL major version in the target container.
               Default: inferred from PACKAGE_DIR, then 17.
  DB_NAME      Database where CREATE EXTENSION should run. Default: postgres.
  DB_USER      Database user for psql. Default: postgres.

Environment:
  SKIP_CREATE_EXTENSION=1  Copy files only; do not run CREATE EXTENSION.

Example:
  scripts/copy_pggraph_package_to_docker_postgres.sh my-postgres target/docker-packages/graph-pg17 17 appdb postgres
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -lt 2 || $# -gt 5 ]]; then
  usage >&2
  exit 2
fi

target_container="$1"
package_dir="${2%/}"
inferred_major=""
if [[ "$(basename "$package_dir")" =~ ^graph-pg([0-9]+)$ ]]; then
  inferred_major="${BASH_REMATCH[1]}"
fi

pg_major="${3:-${inferred_major:-17}}"
db_name="${4:-postgres}"
db_user="${5:-postgres}"

case "$pg_major" in
  14|15|16|17|18) ;;
  *)
    echo "Unsupported PostgreSQL major: $pg_major" >&2
    echo "Supported versions: 14 15 16 17 18" >&2
    exit 2
    ;;
esac

if [[ ! -d "$package_dir" ]]; then
  echo "Package directory not found: $package_dir" >&2
  exit 1
fi

if ! docker inspect "$target_container" >/dev/null 2>&1; then
  echo "Target container not found: $target_container" >&2
  exit 1
fi

if [[ "$(docker inspect -f '{{.State.Running}}' "$target_container")" != "true" ]]; then
  echo "Target container is not running: $target_container" >&2
  exit 1
fi

extension_dir="$package_dir/usr/share/postgresql/${pg_major}/extension"
library_file="$package_dir/usr/lib/postgresql/${pg_major}/lib/graph.so"

if [[ ! -d "$extension_dir" ]]; then
  echo "Missing packaged extension directory: $extension_dir" >&2
  exit 1
fi

if [[ ! -f "$library_file" ]]; then
  echo "Missing packaged shared library: $library_file" >&2
  exit 1
fi

echo "Copying pgGraph extension files into ${target_container}..."
docker cp "$extension_dir/." "$target_container:/usr/share/postgresql/${pg_major}/extension/"
docker cp "$library_file" "$target_container:/usr/lib/postgresql/${pg_major}/lib/graph.so"

if [[ "${SKIP_CREATE_EXTENSION:-0}" == "1" ]]; then
  echo "Copied pgGraph files. Skipped CREATE EXTENSION because SKIP_CREATE_EXTENSION=1."
  exit 0
fi

echo "Creating extension in database ${db_name}..."
docker exec "$target_container" \
  psql -U "$db_user" -d "$db_name" -v ON_ERROR_STOP=1 \
  -c "CREATE EXTENSION IF NOT EXISTS graph;"

echo "pgGraph installed in ${target_container}/${db_name} for PostgreSQL ${pg_major}."
