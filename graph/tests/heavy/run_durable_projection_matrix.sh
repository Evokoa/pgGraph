#!/usr/bin/env bash
set -euo pipefail

PG_VERSIONS="${PG_VERSIONS:-14 15 16 17 18}"
PROFILE_TIMEOUT_SECONDS="${PROFILE_TIMEOUT_SECONDS:-600}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKDIR="$(mktemp -d "${PGGRAPH_TEST_TMPDIR:-/tmp}/pggraph-durable-matrix.XXXXXX")"
ACTIVE_PG_CTL=""
ACTIVE_DATA_DIR=""
ACTIVE_POSTMASTER_PID=""

if command -v timeout >/dev/null 2>&1; then
  TIMEOUT_BIN="$(command -v timeout)"
elif command -v gtimeout >/dev/null 2>&1; then
  TIMEOUT_BIN="$(command -v gtimeout)"
else
  echo "GNU timeout (or gtimeout) is required for the durable projection matrix" >&2
  exit 2
fi

stop_postgres() {
  if [[ -n "$ACTIVE_PG_CTL" && -n "$ACTIVE_DATA_DIR" ]]; then
    if ! "$ACTIVE_PG_CTL" -D "$ACTIVE_DATA_DIR" -m fast -t 30 -w stop >/dev/null 2>&1; then
      "$ACTIVE_PG_CTL" -D "$ACTIVE_DATA_DIR" -m immediate -t 10 -w stop \
        >/dev/null 2>&1 || true
    fi
  fi
  if [[ -n "$ACTIVE_POSTMASTER_PID" ]] && \
    kill -0 "$ACTIVE_POSTMASTER_PID" >/dev/null 2>&1; then
    kill "$ACTIVE_POSTMASTER_PID" >/dev/null 2>&1 || true
    for _ in $(seq 1 100); do
      if ! kill -0 "$ACTIVE_POSTMASTER_PID" >/dev/null 2>&1; then
        break
      fi
      sleep 0.1
    done
  fi
  if [[ -n "$ACTIVE_POSTMASTER_PID" ]] && \
    kill -0 "$ACTIVE_POSTMASTER_PID" >/dev/null 2>&1; then
    kill -KILL "$ACTIVE_POSTMASTER_PID" >/dev/null 2>&1 || true
    for _ in $(seq 1 50); do
      if ! kill -0 "$ACTIVE_POSTMASTER_PID" >/dev/null 2>&1; then
        break
      fi
      sleep 0.1
    done
  fi
  if [[ -n "$ACTIVE_POSTMASTER_PID" ]] && \
    kill -0 "$ACTIVE_POSTMASTER_PID" >/dev/null 2>&1; then
    echo "PostgreSQL postmaster $ACTIVE_POSTMASTER_PID did not stop; retaining $WORKDIR" >&2
    return 1
  fi
  ACTIVE_PG_CTL=""
  ACTIVE_DATA_DIR=""
  ACTIVE_POSTMASTER_PID=""
}

cleanup() {
  if stop_postgres; then
    rm -rf "$WORKDIR"
  fi
}
trap cleanup EXIT

run_profile() {
  local description="$1"
  shift
  local status

  echo "==> $description"
  set +e
  "$TIMEOUT_BIN" --kill-after=15s \
    "${PROFILE_TIMEOUT_SECONDS}s" "$@"
  status=$?
  set -e
  if [[ "$status" -ne 0 ]]; then
    echo "$description failed or timed out after ${PROFILE_TIMEOUT_SECONDS}s (status $status)" >&2
    if [[ -f "${postgres_log:-}" ]]; then
      sed -n '1,240p' "$postgres_log" >&2
    fi
    return "$status"
  fi
}

resolve_pg_config() {
  local major="$1"
  local variable="PG_CONFIG_${major}"
  local configured="${!variable:-}"

  if [[ -n "$configured" && -x "$configured" ]]; then
    printf '%s\n' "$configured"
  elif [[ -x "/usr/lib/postgresql/${major}/bin/pg_config" ]]; then
    printf '/usr/lib/postgresql/%s/bin/pg_config\n' "$major"
  elif [[ -x "/opt/homebrew/opt/postgresql@${major}/bin/pg_config" ]]; then
    printf '/opt/homebrew/opt/postgresql@%s/bin/pg_config\n' "$major"
  else
    echo "missing pg_config for PostgreSQL $major" >&2
    return 2
  fi
}

cd "$GRAPH_DIR"

for major in $PG_VERSIONS; do
  if [[ ! "$major" =~ ^(14|15|16|17|18)$ ]]; then
    echo "unsupported PostgreSQL major in PG_VERSIONS: $major" >&2
    exit 2
  fi

  pg_config="$(resolve_pg_config "$major")"
  pg_bin="$(dirname "$pg_config")"
  data_dir="$WORKDIR/pg${major}-data"
  socket_dir="$WORKDIR/pg${major}-socket"
  postgres_log="$WORKDIR/pg${major}.log"
  port="$((55500 + major))"
  mkdir -p "$socket_dir"

  "$pg_bin/initdb" --auth=trust --username="${PGUSER:-pggraph}" \
    --no-instructions -D "$data_dir" >/dev/null
  ACTIVE_PG_CTL="$pg_bin/pg_ctl"
  ACTIVE_DATA_DIR="$data_dir"
  if ! "$ACTIVE_PG_CTL" -D "$data_dir" -l "$postgres_log" \
    -o "-F -p $port -k $socket_dir -c listen_addresses=''" \
    -t 30 -w start >/dev/null; then
    echo "PostgreSQL $major failed to start:" >&2
    sed -n '1,200p' "$postgres_log" >&2
    exit 1
  fi
  ACTIVE_POSTMASTER_PID="$(sed -n '1p' "$data_dir/postmaster.pid")"

  export PATH="$pg_bin:$PATH"
  export PGHOST="$socket_dir"
  export PGPORT="$port"
  export PGUSER="${PGUSER:-pggraph}"
  export PGCONNECT_TIMEOUT="${PGCONNECT_TIMEOUT:-10}"
  export PG_CONFIG="$pg_config"
  export PG_VERSION_FEATURE="pg${major}"

  run_profile "PostgreSQL $major cross-backend durable lifecycle" \
    env DBNAME="pggraph_ki026_${major}_lifecycle" \
    "$SCRIPT_DIR/cross_backend_durable_projection.sh"

  run_profile "PostgreSQL $major publication and writer locks" \
    env DBNAME="pggraph_ki026_${major}_locks" \
    "$SCRIPT_DIR/build_lock_regression.sh"

  stop_postgres
  echo "PostgreSQL $major durable projection matrix passed"
done

echo "Durable projection matrix passed for PostgreSQL versions: $PG_VERSIONS"
