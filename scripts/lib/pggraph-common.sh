#!/usr/bin/env bash
# Shared, side-effect-free foundations for pgGraph maintainer scripts.

pggraph_log() {
  printf '==> %s\n' "$*"
}

pggraph_die() {
  printf 'Error: %s\n' "$*" >&2
  return 1
}

pggraph_require_command() {
  command -v "$1" >/dev/null 2>&1 || pggraph_die "required command not found: $1"
}

pggraph_validate_database_name() {
  case "$1" in
    ""|postgres|template0|template1)
      printf 'Error: refusing unsafe database target: %s\n' "${1:-<empty>}" >&2
      return 2
      ;;
    *[!a-zA-Z0-9_]* )
      printf 'Error: database names may contain only letters, digits, and underscores: %s\n' "$1" >&2
      return 2
      ;;
  esac
}

pggraph_make_temp_dir() {
  local label="${1:-run}"
  mktemp -d "${TMPDIR:-/tmp}/pggraph-${label}.XXXXXX"
}

pggraph_wait_until() {
  local timeout_seconds="$1"
  local interval_seconds="$2"
  shift 2
  local deadline=$((SECONDS + timeout_seconds))
  while ! "$@"; do
    if (( SECONDS >= deadline )); then
      pggraph_die "timed out after ${timeout_seconds}s: $*"
      return 1
    fi
    sleep "${interval_seconds}"
  done
}

pggraph_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

pggraph_validate_disposable_cluster() {
  local database_name="$1"
  local token="${PGGRAPH_DISPOSABLE_CLUSTER_TOKEN:-}"
  local sentinel="${PGGRAPH_DISPOSABLE_CLUSTER_SENTINEL:-}"
  local pgdata_logical pgdata_real sentinel_real connected_real postgres_pid postgres_command

  pggraph_validate_database_name "$database_name" || return
  if [[ -z "${PGDATA:-}" || -z "$token" || -z "$sentinel" ]]; then
    pggraph_die "destructive cluster test requires the disposable-cluster wrapper"
    return 2
  fi
  if [[ ! -f "$sentinel" || "$(<"$sentinel")" != "$token" ]]; then
    pggraph_die "disposable-cluster sentinel is missing or does not match this run"
    return 2
  fi
  pgdata_logical="$(cd "$PGDATA" && pwd -L)"
  pgdata_real="$(cd "$PGDATA" && pwd -P)"
  sentinel_real="$(cd "$(dirname "$sentinel")" && pwd -P)/$(basename "$sentinel")"
  if [[ "$sentinel_real" != "$pgdata_real/.pggraph-disposable-cluster" ]]; then
    pggraph_die "disposable-cluster sentinel is outside PGDATA"
    return 2
  fi
  case "$pgdata_real" in
    /|"$HOME"|/var/lib/postgresql|/var/lib/postgresql/*|/usr/local/var/postgres|/usr/local/var/postgres/*|/opt/homebrew/var/postgresql*|/Library/PostgreSQL/*)
      pggraph_die "refusing destructive test against unsafe PGDATA: $pgdata_real"
      return 2
      ;;
  esac

  connected_real="$(psql -X -qAt -v ON_ERROR_STOP=1 -d "$database_name" -c "SHOW data_directory")"
  connected_real="$(cd "$connected_real" && pwd -P)"
  if [[ "$connected_real" != "$pgdata_real" ]]; then
    pggraph_die "connected PostgreSQL data directory does not match PGDATA"
    return 2
  fi
  if [[ ! -s "$pgdata_real/postmaster.pid" ]]; then
    pggraph_die "disposable cluster has no postmaster.pid"
    return 2
  fi
  postgres_pid="$(head -n 1 "$pgdata_real/postmaster.pid")"
  if [[ ! "$postgres_pid" =~ ^[0-9]+$ ]] || ! kill -0 "$postgres_pid" 2>/dev/null; then
    pggraph_die "disposable cluster postmaster PID is not live"
    return 2
  fi
  postgres_command="$(ps -o command= -p "$postgres_pid")"
  if [[ "$postgres_command" != *postgres* ]] ||
    { [[ "$postgres_command" != *"$pgdata_logical"* ]] && [[ "$postgres_command" != *"$pgdata_real"* ]]; }; then
    pggraph_die "postmaster PID does not belong to the disposable PGDATA"
    return 2
  fi
}
