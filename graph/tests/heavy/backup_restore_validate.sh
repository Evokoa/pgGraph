#!/usr/bin/env bash
set -euo pipefail

SOURCE_DB="${SOURCE_DB:-pggraph_backup_src}"
RESTORE_DB="${RESTORE_DB:-pggraph_backup_dst}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-backup-restore.XXXXXX")"

cleanup() {
  dropdb --if-exists "$SOURCE_DB" >/dev/null 2>&1 || true
  dropdb --if-exists "$RESTORE_DB" >/dev/null 2>&1 || true
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

if [[ -z "$PG_CONFIG" ]]; then
  if [[ -x "/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config"
  elif [[ -x "/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config"
  else
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE"
    exit 2
  fi
fi

cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
createdb "$SOURCE_DB"

psql -X -v ON_ERROR_STOP=1 "$SOURCE_DB" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
CREATE TABLE public.graph_backup_nodes (
    id TEXT PRIMARY KEY,
    tenant TEXT NOT NULL,
    name TEXT NOT NULL
);
CREATE TABLE public.graph_backup_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_backup_nodes(id),
    to_id TEXT NOT NULL REFERENCES public.graph_backup_nodes(id),
    label TEXT NOT NULL DEFAULT 'linked'
);
INSERT INTO public.graph_backup_nodes (id, tenant, name)
SELECT i::text, CASE WHEN i % 2 = 0 THEN 'even' ELSE 'odd' END, 'node-' || i::text
FROM generate_series(1, 200) AS i;
INSERT INTO public.graph_backup_edges (from_id, to_id)
SELECT i::text, (i + 1)::text
FROM generate_series(1, 199) AS i;
SELECT graph.add_table('public.graph_backup_nodes'::regclass, 'id', ARRAY['tenant', 'name']);
SELECT graph.add_edge('public.graph_backup_edges'::regclass, 'from_id', 'public.graph_backup_nodes'::regclass, 'id', 'linked', false);
SELECT * FROM graph.build();
SELECT graph.enable_sync();
INSERT INTO public.graph_backup_nodes VALUES ('201', 'odd', 'node-201');
INSERT INTO public.graph_backup_edges (from_id, to_id) VALUES ('200', '201');
SELECT * FROM graph.apply_sync();
SQL

pg_dump --format=custom --file="$WORKDIR/graph.dump" "$SOURCE_DB"
createdb "$RESTORE_DB"
pg_restore --dbname="$RESTORE_DB" --clean --if-exists "$WORKDIR/graph.dump"

psql -X -v ON_ERROR_STOP=1 "$RESTORE_DB" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
-- Relation OIDs are database-local and intentionally fail closed after a
-- logical restore. Re-register the restored source relations before rebuilding.
SELECT graph.reset();
SELECT graph.add_table('public.graph_backup_nodes'::regclass, 'id', ARRAY['tenant', 'name']);
SELECT graph.add_edge('public.graph_backup_edges'::regclass, 'from_id', 'public.graph_backup_nodes'::regclass, 'id', 'linked', false);
SELECT * FROM graph.build();
SELECT graph.enable_sync();
INSERT INTO public.graph_backup_nodes VALUES ('202', 'even', 'node-202');
INSERT INTO public.graph_backup_edges (from_id, to_id) VALUES ('201', '202');
DO $$
DECLARE
    applied BIGINT;
    traversed BIGINT;
    matches BIGINT;
BEGIN
    SELECT inserts_applied + updates_applied + deletes_applied INTO applied FROM graph.apply_sync();
    IF applied <= 0 THEN
        RAISE EXCEPTION 'expected restored sync apply to consume rows, got %', applied;
    END IF;

    SELECT count(*) INTO traversed
    FROM graph.traverse('public.graph_backup_nodes'::regclass, '200', 3, edge_types := ARRAY['linked'], direction := 'out');
    IF traversed < 1 THEN
        RAISE EXCEPTION 'expected restored traversal to see rows, got %', traversed;
    END IF;

    SELECT count(*) INTO matches
    FROM graph.search('tenant', 'even', table_filter := 'public.graph_backup_nodes'::regclass);
    IF matches < 100 THEN
        RAISE EXCEPTION 'expected restored tenant search to see at least 100 even nodes, got %', matches;
    END IF;
END
$$;
SQL

echo "Backup/restore validation passed: $SOURCE_DB -> $RESTORE_DB"
