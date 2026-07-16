#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_install_smoke}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
SKIP_PACKAGE_INSTALL="${SKIP_PACKAGE_INSTALL:-0}"

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

if [[ "$SKIP_PACKAGE_INSTALL" != "1" ]]; then
  cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
fi
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION graph;
DO $$
DECLARE
    missing text;
BEGIN
    WITH expected(relname) AS (
        VALUES
            ('_registered_tables'),
            ('_registered_edges'),
            ('_build_jobs'),
            ('_sync_log'),
            ('_sync_buffer')
    ),
    owned AS (
        SELECT c.relname, e.extname
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        LEFT JOIN pg_depend d
          ON d.objid = c.oid
         AND d.deptype = 'e'
        LEFT JOIN pg_extension e
          ON e.oid = d.refobjid
        WHERE n.nspname = 'graph'
          AND c.relname IN (
              '_registered_tables',
              '_registered_edges',
              '_build_jobs',
              '_sync_log',
              '_sync_buffer'
          )
    )
    SELECT string_agg(expected.relname, ', ' ORDER BY expected.relname)
      INTO missing
    FROM expected
    LEFT JOIN owned USING (relname)
    WHERE owned.extname IS DISTINCT FROM 'graph';

    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'graph catalog tables are not extension-owned: %', missing;
    END IF;
END
$$;
SELECT graph.reset();
DROP TABLE IF EXISTS public.graph_install_edge CASCADE;
DROP TABLE IF EXISTS public.graph_install_node CASCADE;
CREATE TABLE public.graph_install_node (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    parent_id TEXT REFERENCES public.graph_install_node(id)
);
INSERT INTO public.graph_install_node VALUES
    ('root', 'Root', NULL),
    ('child', 'Child', 'root');
SELECT graph.add_table('public.graph_install_node'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge(
    'public.graph_install_node'::regclass,
    'parent_id',
    'public.graph_install_node'::regclass,
    'id',
    'parent',
    bidirectional := false
);
SELECT * FROM graph.build();
SELECT count(*) AS traverse_rows
FROM graph.traverse('public.graph_install_node'::regclass, 'child', 1, direction := 'out');
SELECT count(*) AS search_rows
FROM graph.search('name', 'Child', table_filter := 'public.graph_install_node'::regclass);
SQL

echo "Fresh install smoke passed on database: $DBNAME"
