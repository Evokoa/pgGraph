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
  local status=$?
  local stop_failed=0
  trap - EXIT INT TERM
  if (( old_started == 1 )); then
    "$OLD_BINDIR/pg_ctl" -D "$OLD_DATADIR" -m immediate -w stop >/dev/null 2>&1 || stop_failed=1
  fi
  if (( new_started == 1 )); then
    "$NEW_BINDIR/pg_ctl" -D "$NEW_DATADIR" -m immediate -w stop >/dev/null 2>&1 || stop_failed=1
  fi
  if (( stop_failed == 1 )); then
    echo "PostgreSQL shutdown failed; retaining upgrade data in $WORKDIR" >&2
    if (( status == 0 )); then status=1; fi
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

old_started=1
"$OLD_BINDIR/pg_ctl" -D "$OLD_DATADIR" -o "-k $SOCKET_DIR -p $OLD_PORT" -w start
"$OLD_BINDIR/createdb" -h "$SOCKET_DIR" -p "$OLD_PORT" -U "$PGUSER" "$DBNAME"
"$OLD_BINDIR/psql" -X -h "$SOCKET_DIR" -p "$OLD_PORT" -U "$PGUSER" -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
DO $$ BEGIN
  IF current_setting('fsync') <> 'on' THEN
    RAISE EXCEPTION 'Gate requires fsync=on';
  END IF;
END $$;
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

new_started=1
"$NEW_BINDIR/pg_ctl" -D "$NEW_DATADIR" -o "-k $SOCKET_DIR -p $NEW_PORT" -w start
"$NEW_BINDIR/psql" -X -h "$SOCKET_DIR" -p "$NEW_PORT" -U "$PGUSER" -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
DO $$ BEGIN
  IF current_setting('fsync') <> 'on' THEN
    RAISE EXCEPTION 'Gate requires fsync=on';
  END IF;
END $$;
-- PostgreSQL source tables and extension catalogs are upgraded in place. The
-- derived pgGraph artifact is rebuilt in the new cluster data directory.
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
DO $$
DECLARE
    source_rows jsonb;
    nodes bigint;
    edges bigint;
    reached text[];
    matches text[];
BEGIN
    SELECT jsonb_agg(jsonb_build_array(id, name, parent_id) ORDER BY id)
    INTO source_rows FROM graph_upgrade_nodes;
    IF source_rows IS DISTINCT FROM
       '[["child", "Child", "root"], ["root", "Root", null]]'::jsonb THEN
        RAISE EXCEPTION 'upgrade changed authoritative source rows: %', source_rows;
    END IF;

    SELECT node_count, edge_count INTO nodes, edges FROM graph.status();
    IF nodes IS DISTINCT FROM 2::bigint OR edges IS DISTINCT FROM 1::bigint THEN
        RAISE EXCEPTION 'expected upgraded graph to contain 2 nodes and 1 edge, got % and %', nodes, edges;
    END IF;

    SELECT array_agg(node_id ORDER BY depth) INTO reached
    FROM graph.traverse('graph_upgrade_nodes'::regclass, 'child', 1,
                        edge_types := ARRAY['parent'], direction := 'out');
    IF reached IS DISTINCT FROM ARRAY['child', 'root']::text[] THEN
        RAISE EXCEPTION 'upgrade lost child-to-parent topology: %', reached;
    END IF;

    SELECT array_agg(node_id ORDER BY depth) INTO reached
    FROM graph.traverse('graph_upgrade_nodes'::regclass, 'root', 1,
                        edge_types := ARRAY['parent'], direction := 'out');
    IF reached IS DISTINCT FROM ARRAY['root']::text[] THEN
        RAISE EXCEPTION 'upgrade changed parent edge direction: %', reached;
    END IF;

    SELECT array_agg(node_id ORDER BY node_id) INTO matches
    FROM graph.search('name', 'Child',
                      table_filter := 'graph_upgrade_nodes'::regclass);
    IF matches IS DISTINCT FROM ARRAY['child']::text[] THEN
        RAISE EXCEPTION 'upgrade changed indexed property search: %', matches;
    END IF;
END
$$;
SQL
"$NEW_BINDIR/pg_ctl" -D "$NEW_DATADIR" -w stop
new_started=0

echo "pg_upgrade validation passed from $OLD_BINDIR to $NEW_BINDIR"
