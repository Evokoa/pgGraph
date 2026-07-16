#!/usr/bin/env bash
set -euo pipefail

PG_VERSIONS="${PG_VERSIONS:-14 15 16 17 18}"
ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/pggraph-package-matrix.XXXXXX")"
running_pg=""

cleanup() {
  if [[ -n "$running_pg" ]]; then
    cargo pgrx stop "pg${running_pg}" >/dev/null 2>&1 || true
  fi
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

for pg in $PG_VERSIONS; do
  pg_config="${PG_CONFIG_PREFIX:-/usr/lib/postgresql}/${pg}/bin/pg_config"
  if [[ ! -x "$pg_config" ]]; then
    echo "PostgreSQL ${pg} pg_config is unavailable at ${pg_config}" >&2
    exit 2
  fi

  out_dir="$WORKDIR/pg${pg}"
  echo "==> package PostgreSQL ${pg}"
  cargo pgrx package \
    --manifest-path "$ROOT_DIR/Cargo.toml" \
    --pg-config "$pg_config" \
    --out-dir "$out_dir" \
    --no-default-features \
    --features "pg${pg}"

  control="$out_dir$("$pg_config" --sharedir)/extension/graph.control"
  sql_dir="$out_dir$("$pg_config" --sharedir)/extension"
  library="$out_dir$("$pg_config" --pkglibdir)/graph.so"
  [[ -s "$control" ]] || { echo "missing graph.control for PostgreSQL ${pg}" >&2; exit 1; }
  compgen -G "$sql_dir/graph--*.sql" >/dev/null || {
    echo "missing extension SQL for PostgreSQL ${pg}" >&2
    exit 1
  }
  [[ -s "$library" ]] || { echo "missing graph.so for PostgreSQL ${pg}" >&2; exit 1; }

  cp "$control" "$("$pg_config" --sharedir)/extension/"
  cp "$sql_dir"/graph--*.sql "$("$pg_config" --sharedir)/extension/"
  cp "$library" "$("$pg_config" --pkglibdir)/"
  cargo pgrx start "pg${pg}"
  running_pg="$pg"
  PGHOST=localhost \
    PGPORT="288${pg}" \
    DBNAME="pggraph_package_pg${pg}" \
    PG_VERSION_FEATURE="pg${pg}" \
    PG_CONFIG="$pg_config" \
    SKIP_PACKAGE_INSTALL=1 \
    "$ROOT_DIR/tests/heavy/fresh_install_smoke.sh"
  cargo pgrx stop "pg${pg}"
  running_pg=""
done

echo "Package install trees passed for PostgreSQL majors: $PG_VERSIONS"
