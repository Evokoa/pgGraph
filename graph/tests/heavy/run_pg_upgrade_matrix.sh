#!/usr/bin/env bash
set -euo pipefail

UPGRADE_PAIRS="${UPGRADE_PAIRS:-14:15 15:16 16:17 17:18}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"

for pair in $UPGRADE_PAIRS; do
  old="${pair%%:*}"
  new="${pair##*:}"
  old_bindir="/usr/lib/postgresql/${old}/bin"
  new_bindir="/usr/lib/postgresql/${new}/bin"
  for command in "$old_bindir/pg_config" "$new_bindir/pg_config"; do
    if [[ ! -x "$command" ]]; then
      echo "Missing PostgreSQL upgrade command: $command" >&2
      exit 2
    fi
  done

  echo "==> Installing pgGraph for PostgreSQL ${old} and ${new}"
  cargo pgrx install --pg-config "$old_bindir/pg_config" \
    --features "pg${old}" --no-default-features
  cargo pgrx install --pg-config "$new_bindir/pg_config" \
    --features "pg${new}" --no-default-features

  workdir="$(mktemp -d "$TMPDIR_ROOT/pggraph-upgrade-${old}-${new}.XXXXXX")"
  sentinel="$workdir/.pggraph-disposable-upgrade"
  touch "$sentinel"
  cleanup() {
    local status=$?
    trap - EXIT INT TERM
    if (( status != 0 )); then
      echo "Upgrade validation failed; retaining work directory: $workdir" >&2
    else
      rm -rf "$workdir" || status=$?
    fi
    exit "$status"
  }
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  "$old_bindir/initdb" --auth=trust --username=pggraph -D "$workdir/old" >/dev/null

  OLD_BINDIR="$old_bindir" \
  NEW_BINDIR="$new_bindir" \
  OLD_DATADIR="$workdir/old" \
  NEW_DATADIR="$workdir/new" \
  PGGRAPH_UPGRADE_SENTINEL="$sentinel" \
  DBNAME="pggraph_upgrade_${old}_${new}" \
  PGUSER=pggraph \
    ./tests/heavy/pg_upgrade_validate.sh

  rm -rf "$workdir"
  trap - EXIT INT TERM
done

echo "pg_upgrade matrix passed for: $UPGRADE_PAIRS"
