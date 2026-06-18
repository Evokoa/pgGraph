#!/usr/bin/env bash
set -euo pipefail

PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

if [[ -z "$PG_CONFIG" ]]; then
  if [[ -x "/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config"
  elif [[ -x "/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config"
  else
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE"
    exit 2
  fi
fi

cargo pgrx package --pg-config "$PG_CONFIG"

generated_sql="/tmp/graph-generated.sql"
cargo pgrx schema --features "$PG_VERSION_FEATURE" --no-default-features > "$generated_sql"
grep -E "CREATE TABLE IF NOT EXISTS graph\._registered_tables|CREATE TABLE IF NOT EXISTS graph\._sync_log|pg_extension_config_dump" "$generated_sql"

package_version="$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"version":"\([^"]*\)".*/\1/p' | head -n 1)"
package_dir="target/release/graph-${PG_VERSION_FEATURE}"
control_file="$(find "$package_dir" -path "*/extension/graph.control" -type f | head -n 1)"
sql_file="$(find "$package_dir" -path "*/extension/graph--${package_version}.sql" -type f | head -n 1)"
shared_library="$(find "$package_dir" \( -name "graph.so" -o -name "graph.dylib" \) -type f | head -n 1)"

if [[ -z "$control_file" ]]; then
  echo "Missing package artifact: graph.control under $package_dir"
  exit 1
fi
if [[ -z "$sql_file" ]]; then
  echo "Missing package artifact: graph--${package_version}.sql under $package_dir"
  exit 1
fi
if [[ -z "$shared_library" ]]; then
  echo "Missing package shared library under $package_dir"
  exit 1
fi

grep -q "module_pathname = 'graph'" "$control_file"
grep -q "superuser = true" "$control_file"
grep -q "trusted = false" "$control_file"
grep -q "relocatable = false" "$control_file"

repo_required=(
  "$REPO_ROOT/LICENSE"
  "$REPO_ROOT/README.md"
  "$REPO_ROOT/docs/index.mdx"
  "$REPO_ROOT/docs/quickstart.mdx"
  "$REPO_ROOT/docs/roadmap.mdx"
  "$REPO_ROOT/docs/user_guide/installation.mdx"
  "$REPO_ROOT/docs/user_guide/api-reference.mdx"
  "$REPO_ROOT/docs/contributor_guide/testing-release.mdx"
)
for path in "${repo_required[@]}"; do
  if [[ ! -f "$path" ]]; then
    echo "Missing release companion artifact: $path"
    exit 1
  fi
done

echo "Package validation passed for $PG_VERSION_FEATURE at $package_dir"
