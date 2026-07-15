#!/usr/bin/env bash
set -euo pipefail

PG_VERSIONS="${PG_VERSIONS:-14 15 16 17 18}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKDIR="$(mktemp -d "${PGGRAPH_TEST_TMPDIR:-/tmp}/pggraph-gql-write-matrix.XXXXXX")"
ACTIVE_PG_CTL=""
ACTIVE_DATA_DIR=""

stop_postgres() {
  if [[ -n "$ACTIVE_PG_CTL" && -n "$ACTIVE_DATA_DIR" ]]; then
    "$ACTIVE_PG_CTL" -D "$ACTIVE_DATA_DIR" -m fast -w stop >/dev/null 2>&1 || true
  fi
  ACTIVE_PG_CTL=""
  ACTIVE_DATA_DIR=""
}

cleanup() {
  stop_postgres
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

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

run_gate() {
  local description="$1"
  shift
  echo "==> $description"
  "$@"
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
  port="$((55400 + major))"
  mkdir -p "$socket_dir"

  "$pg_bin/initdb" --auth=trust --username="${PGUSER:-pggraph}" \
    --no-instructions -D "$data_dir" >/dev/null
  ACTIVE_PG_CTL="$pg_bin/pg_ctl"
  ACTIVE_DATA_DIR="$data_dir"
  if ! "$ACTIVE_PG_CTL" -D "$data_dir" -l "$postgres_log" \
    -o "-F -p $port -k $socket_dir" -w start >/dev/null; then
    echo "PostgreSQL $major failed to start:" >&2
    sed -n '1,200p' "$postgres_log" >&2
    exit 1
  fi

  export PGHOST="$socket_dir"
  export PGPORT="$port"
  export PGUSER="${PGUSER:-pggraph}"
  export PG_CONFIG="$pg_config"
  export PG_VERSION_FEATURE="pg${major}"

  run_gate "PostgreSQL $major full GQL isolation profile" \
    env DBNAME="pggraph_qa01d_${major}_isolation" \
      "$SCRIPT_DIR/gql_isolation_matrix.sh"
  run_gate "PostgreSQL $major persisted GQL isolation profile" \
    env PERSIST_ON_BUILD=on DBNAME="pggraph_qa01d_${major}_persisted" \
      "$SCRIPT_DIR/gql_isolation_matrix.sh"
  run_gate "PostgreSQL $major same-key MERGE race" \
    env DBNAME="pggraph_qa01d_${major}_merge" \
      "$SCRIPT_DIR/gql_merge_race.sh"
  run_gate "PostgreSQL $major relationship race" \
    env DBNAME="pggraph_qa01d_${major}_relationship" \
      "$SCRIPT_DIR/gql_relationship_race.sh"
  run_gate "PostgreSQL $major stale-write recheck race" \
    env DBNAME="pggraph_qa01d_${major}_recheck" \
      "$SCRIPT_DIR/gql_write_recheck_race.sh"

  stop_postgres
  echo "PostgreSQL $major GQL write matrix passed"
done

echo "GQL write matrix passed for PostgreSQL versions: $PG_VERSIONS"
