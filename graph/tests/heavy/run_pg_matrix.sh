#!/usr/bin/env bash
set -euo pipefail

PG_VERSIONS="${PG_VERSIONS:-14 15 16 17 18}"
RUN_PGRX_SQL="${RUN_PGRX_SQL:-1}"

missing=()
for pg in $PG_VERSIONS; do
  feature="pg${pg}"
  pg_config_var="PG_CONFIG_${pg}"
  pg_config="${!pg_config_var:-}"
  if [[ -z "$pg_config" ]]; then
    if [[ -x "/usr/lib/postgresql/${pg}/bin/pg_config" ]]; then
      pg_config="/usr/lib/postgresql/${pg}/bin/pg_config"
    elif [[ -x "/opt/homebrew/opt/postgresql@${pg}/bin/pg_config" ]]; then
      pg_config="/opt/homebrew/opt/postgresql@${pg}/bin/pg_config"
    fi
  fi

  if [[ -z "$pg_config" || ! -x "$pg_config" ]]; then
    missing+=("$feature")
    continue
  fi

  echo "==> Initializing pgrx for $feature with $pg_config"
  cargo pgrx init "--pg${pg}=${pg_config}"

  echo "==> cargo test --no-default-features --features $feature"
  cargo test --no-default-features --features "$feature"

  if [[ "$RUN_PGRX_SQL" == "1" ]]; then
    echo "==> cargo pgrx test $feature"
    cargo pgrx test "$feature"
  fi
done

if (( ${#missing[@]} > 0 )); then
  echo "Missing pg_config for: ${missing[*]}"
  echo "Install those PostgreSQL versions or set PG_CONFIG_<major> to their pg_config path."
  exit 2
fi
