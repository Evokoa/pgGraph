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
