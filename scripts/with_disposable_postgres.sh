#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Run a command against a new disposable PostgreSQL cluster.

Usage:
  scripts/with_disposable_postgres.sh COMMAND [ARG ...]

The wrapper derives PostgreSQL binaries from PG_CONFIG (or the selected
PG_VERSION_FEATURE), creates a temporary trust-authenticated cluster and Unix
socket, exports PGDATA/PGHOST/PGPORT/PGUSER/POSTGRES_CTL/POSTGRES_OPTS, and
removes the cluster after the command exits. It is intended for destructive
crash-recovery release gates; never point it at an existing data directory.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi
if (( $# == 0 )); then
  usage >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib/pggraph-common.sh
source "$repo_root/scripts/lib/pggraph-common.sh"

pg_feature="${PG_VERSION_FEATURE:-pg17}"
pg_major="${pg_feature#pg}"
pg_config="${PG_CONFIG:-}"
if [[ -z "$pg_config" ]]; then
  if [[ -x "/usr/lib/postgresql/${pg_major}/bin/pg_config" ]]; then
    pg_config="/usr/lib/postgresql/${pg_major}/bin/pg_config"
  elif [[ -x "/opt/homebrew/opt/postgresql@${pg_major}/bin/pg_config" ]]; then
    pg_config="/opt/homebrew/opt/postgresql@${pg_major}/bin/pg_config"
  else
    pg_config="$(command -v pg_config || true)"
  fi
fi
if [[ -z "$pg_config" || ! -x "$pg_config" ]]; then
  pggraph_die "PG_CONFIG is required for ${pg_feature}"
  exit 2
fi

pg_bin="$($pg_config --bindir)"
for command in initdb pg_ctl pg_isready; do
  if [[ ! -x "$pg_bin/$command" ]]; then
    pggraph_die "required PostgreSQL command not found: $pg_bin/$command"
    exit 2
  fi
done

workdir="$(pggraph_make_temp_dir release-postgres)"
pgdata="$workdir/data"
socket_dir="$workdir/socket"
mkdir -p "$socket_dir"
port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
pguser="${PGUSER:-pggraph}"
postgres_opts="-F -k $socket_dir -p $port"
token="$(python3 - <<'PY'
import uuid
print(uuid.uuid4())
PY
)"
started=0

cleanup() {
  if (( started == 1 )); then
    "$pg_bin/pg_ctl" -D "$pgdata" -m immediate -t 20 -w stop >/dev/null 2>&1 || true
  fi
  rm -rf "$workdir"
}
trap cleanup EXIT INT TERM

"$pg_bin/initdb" --auth=trust --username="$pguser" -D "$pgdata" >/dev/null
sentinel="$pgdata/.pggraph-disposable-cluster"
printf '%s\n' "$token" >"$sentinel"
"$pg_bin/pg_ctl" -D "$pgdata" -o "$postgres_opts" -t 60 -w start >/dev/null
started=1

unset PGDATABASE PGPASSWORD PGSERVICE PGSERVICEFILE PGTARGETSESSIONATTRS
export PG_CONFIG="$pg_config"
export PGDATA="$pgdata"
export PGHOST="$socket_dir"
export PGPORT="$port"
export PGUSER="$pguser"
export POSTGRES_CTL="$pg_bin/pg_ctl"
export POSTGRES_OPTS="$postgres_opts"
export PGGRAPH_DISPOSABLE_CLUSTER_TOKEN="$token"
export PGGRAPH_DISPOSABLE_CLUSTER_SENTINEL="$sentinel"

"$@"
