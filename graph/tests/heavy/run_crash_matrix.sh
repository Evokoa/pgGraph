#!/usr/bin/env bash
set -euo pipefail

PG_VERSIONS="${PG_VERSIONS:-14 15 16 17 18}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

for pg in $PG_VERSIONS; do
  pg_config="/usr/lib/postgresql/${pg}/bin/pg_config"
  if [[ ! -x "$pg_config" ]]; then
    echo "Missing pg_config for PostgreSQL ${pg}: $pg_config" >&2
    exit 2
  fi

  echo "==> PostgreSQL ${pg} persisted-generation crash recovery"
  PG_CONFIG="$pg_config" \
  PG_VERSION_FEATURE="pg${pg}" \
  DBNAME="pggraph_crash_pg${pg}" \
    "$ROOT_DIR/scripts/with_disposable_postgres.sh" \
      ./tests/heavy/crash_recovery.sh

  echo "==> PostgreSQL ${pg} transaction-overlay crash recovery"
  PG_CONFIG="$pg_config" \
  PG_VERSION_FEATURE="pg${pg}" \
  DBNAME="pggraph_tx_delta_crash_pg${pg}" \
    "$ROOT_DIR/scripts/with_disposable_postgres.sh" \
      ./tests/heavy/tx_delta_crash_recovery.sh
done

echo "Crash recovery matrix passed for PostgreSQL: $PG_VERSIONS"
