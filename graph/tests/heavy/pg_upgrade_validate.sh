#!/usr/bin/env bash
set -euo pipefail

OLD_BINDIR="${OLD_BINDIR:?Path to old PostgreSQL bin directory is required}"
NEW_BINDIR="${NEW_BINDIR:?Path to new PostgreSQL bin directory is required}"
OLD_DATADIR="${OLD_DATADIR:?Path to old disposable PGDATA is required}"
NEW_DATADIR="${NEW_DATADIR:?Path to new disposable PGDATA is required}"
PGGRAPH_UPGRADE_SENTINEL="${PGGRAPH_UPGRADE_SENTINEL:?Disposable-upgrade sentinel is required}"
DBNAME="${DBNAME:-pggraph_upgrade}"
PGUSER="${PGUSER:-pggraph}"
WORKDIR="$(cd "$(dirname "$PGGRAPH_UPGRADE_SENTINEL")" && pwd -P)"
SOCKET_DIR="$WORKDIR/socket"

if [[ ! -f "$PGGRAPH_UPGRADE_SENTINEL" ]]; then
  echo "Disposable-upgrade sentinel is missing: $PGGRAPH_UPGRADE_SENTINEL" >&2
  exit 2
fi
mkdir -p "$SOCKET_DIR"
old_parent="$(cd "$(dirname "$OLD_DATADIR")" && pwd -P)"
new_parent="$(cd "$(dirname "$NEW_DATADIR")" && pwd -P)"
if [[ "$old_parent" != "$WORKDIR" || "$new_parent" != "$WORKDIR" ]]; then
  echo "Upgrade data directories must be direct children of the sentinel directory" >&2
  exit 2
fi
if [[ ! -f "$OLD_DATADIR/PG_VERSION" ]]; then
  echo "Old disposable cluster is not initialized: $OLD_DATADIR" >&2
  exit 2
fi
if [[ -e "$NEW_DATADIR" ]]; then
  echo "New disposable cluster path must not exist: $NEW_DATADIR" >&2
  exit 2
fi

read -r OLD_PORT NEW_PORT < <(python3 - <<'PY'
import socket

ports = []
for _ in range(2):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        ports.append(sock.getsockname()[1])
print(*ports)
PY
)
old_started=0
new_started=0

cleanup() {
  if (( old_started == 1 )); then
    "$OLD_BINDIR/pg_ctl" -D "$OLD_DATADIR" -m immediate -w stop >/dev/null 2>&1 || true
  fi
  if (( new_started == 1 )); then
    "$NEW_BINDIR/pg_ctl" -D "$NEW_DATADIR" -m immediate -w stop >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

"$OLD_BINDIR/pg_ctl" -D "$OLD_DATADIR" -o "-F -k $SOCKET_DIR -p $OLD_PORT" -w start
old_started=1
"$OLD_BINDIR/createdb" -h "$SOCKET_DIR" -p "$OLD_PORT" -U "$PGUSER" "$DBNAME"
"$OLD_BINDIR/psql" -X -h "$SOCKET_DIR" -p "$OLD_PORT" -U "$PGUSER" -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION graph;
SELECT graph.reset();
CREATE TABLE graph_upgrade_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    parent_id TEXT REFERENCES graph_upgrade_nodes(id)
);
INSERT INTO graph_upgrade_nodes VALUES ('root', 'Root', NULL), ('child', 'Child', 'root');
SELECT graph.add_table('graph_upgrade_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge('graph_upgrade_nodes'::regclass, 'parent_id', 'graph_upgrade_nodes'::regclass, 'id', 'parent', false);
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
SQL
"$OLD_BINDIR/pg_ctl" -D "$OLD_DATADIR" -w stop
old_started=0

old_checksum_version="$("$OLD_BINDIR/pg_controldata" "$OLD_DATADIR" \
  | awk -F: '/Data page checksum version/ { gsub(/[[:space:]]/, "", $2); print $2 }')"
if [[ ! "$old_checksum_version" =~ ^[0-9]+$ ]]; then
  echo "Could not determine old-cluster checksum mode" >&2
  exit 2
fi
initdb_checksum_args=()
if (( old_checksum_version > 0 )); then
  initdb_checksum_args+=(--data-checksums)
elif "$NEW_BINDIR/initdb" --help | grep -q -- '--no-data-checksums'; then
  initdb_checksum_args+=(--no-data-checksums)
fi
"$NEW_BINDIR/initdb" --auth=trust --username="$PGUSER" \
  "${initdb_checksum_args[@]}" -D "$NEW_DATADIR" >/dev/null
(
  cd "$WORKDIR"
  "$NEW_BINDIR/pg_upgrade" \
    --old-bindir="$OLD_BINDIR" \
    --new-bindir="$NEW_BINDIR" \
    --old-datadir="$OLD_DATADIR" \
    --new-datadir="$NEW_DATADIR" \
    --socketdir="$SOCKET_DIR" \
    --old-port="$OLD_PORT" \
    --new-port="$NEW_PORT" \
    --check
  "$NEW_BINDIR/pg_upgrade" \
    --old-bindir="$OLD_BINDIR" \
    --new-bindir="$NEW_BINDIR" \
    --old-datadir="$OLD_DATADIR" \
    --new-datadir="$NEW_DATADIR" \
    --socketdir="$SOCKET_DIR" \
    --old-port="$OLD_PORT" \
    --new-port="$NEW_PORT"
)

"$NEW_BINDIR/pg_ctl" -D "$NEW_DATADIR" -o "-F -k $SOCKET_DIR -p $NEW_PORT" -w start
new_started=1
"$NEW_BINDIR/psql" -X -h "$SOCKET_DIR" -p "$NEW_PORT" -U "$PGUSER" -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
-- PostgreSQL source tables and extension catalogs are upgraded in place. The
-- derived pgGraph artifact is rebuilt in the new cluster data directory.
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
SELECT node_count, edge_count FROM graph.status();
SELECT count(*)
FROM graph.search(
  'name',
  'Child',
  table_filter := 'graph_upgrade_nodes'::regclass
);
SQL
"$NEW_BINDIR/pg_ctl" -D "$NEW_DATADIR" -w stop
new_started=0

echo "pg_upgrade validation passed from $OLD_BINDIR to $NEW_BINDIR"
