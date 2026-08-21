#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_v1_2_update}"
RESTORE_DB="${RESTORE_DB:-${DBNAME}_rollback}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/pggraph-v1-2-update.XXXXXX")"

cleanup() {
  dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
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
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE" >&2
    exit 2
  fi
fi

install_package() {
  local package_dir="$1"
  cp "$package_dir$($PG_CONFIG --sharedir)/extension/"graph--*.sql "$($PG_CONFIG --sharedir)/extension/"
  cp "$package_dir$($PG_CONFIG --sharedir)/extension/graph.control" "$($PG_CONFIG --sharedir)/extension/"
  local library
  library="$(find "$package_dir$($PG_CONFIG --pkglibdir)" -maxdepth 1 -type f \
    \( -name 'graph.so' -o -name 'graph.dylib' \) -print -quit)"
  if [[ -z "$library" ]]; then
    echo "packaged graph shared library is missing" >&2
    exit 1
  fi
  cp "$library" "$($PG_CONFIG --pkglibdir)/"
}

install_tree() {
  local tree="$1"
  local package_dir="$2"
  CARGO_TARGET_DIR="$package_dir-target" cargo pgrx package \
    --manifest-path "$tree/Cargo.toml" \
    --pg-config "$PG_CONFIG" \
    --out-dir "$package_dir" \
    --no-default-features \
    --features "$PG_VERSION_FEATURE"
  install_package "$package_dir"
}

if [[ -n "${V1_1_SOURCE_ARCHIVE:-}" ]]; then
  tar -xf "$V1_1_SOURCE_ARCHIVE" -C "$WORKDIR"
else
  git -C "$ROOT_DIR/.." archive v1.1.0 graph | tar -x -C "$WORKDIR"
fi
install_tree "$WORKDIR/graph" "$WORKDIR/package-1.1"

createdb "$DBNAME"
psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION graph VERSION '1.1.0';
DROP ROLE IF EXISTS pggraph_v1_2_reader;
DROP ROLE IF EXISTS pggraph_v1_2_owner;
CREATE ROLE pggraph_v1_2_reader;
CREATE ROLE pggraph_v1_2_owner;
CREATE TABLE public.release_nodes (
    id text PRIMARY KEY,
    name text NOT NULL
);
CREATE TABLE public.release_edges (
    id bigint PRIMARY KEY,
    from_id text NOT NULL REFERENCES public.release_nodes(id),
    to_id text NOT NULL REFERENCES public.release_nodes(id),
    relationship_name text NOT NULL
);
INSERT INTO public.release_nodes VALUES
    ('a', 'A'), ('b', 'B'), ('c', 'C'), ('d', 'D');
INSERT INTO public.release_edges VALUES
    (1, 'a', 'b', 'works_at'),
    (2, 'b', 'c', 'founded'),
    (3, 'c', 'd', 'co_authored_with');
SELECT graph.add_table('public.release_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge(
    'public.release_edges'::regclass, 'from_id',
    'public.release_nodes'::regclass, 'to_id', 'relationship', false,
    label_column := 'relationship_name'
);
SELECT * FROM graph.build();
GRANT USAGE ON SCHEMA graph TO pggraph_v1_2_reader;
CREATE TABLE public.release_function_contract AS
SELECT p.oid, p.proowner, p.proacl
FROM pg_proc p
JOIN pg_namespace n ON n.oid = p.pronamespace
WHERE n.nspname = 'graph'
  AND p.oid = 'graph.traverse(oid,text,integer,text[],text,oid[],jsonb,text,text,text,boolean,boolean,integer,integer,integer,integer)'::regprocedure;
ALTER FUNCTION graph.traverse(
    oid, text, integer, text[], text, oid[], jsonb, text, text, text,
    boolean, boolean, integer, integer, integer, integer
) OWNER TO pggraph_v1_2_owner;
REVOKE EXECUTE ON FUNCTION graph.traverse(
    oid, text, integer, text[], text, oid[], jsonb, text, text, text,
    boolean, boolean, integer, integer, integer, integer
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION graph.traverse(
    oid, text, integer, text[], text, oid[], jsonb, text, text, text,
    boolean, boolean, integer, integer, integer, integer
) TO pggraph_v1_2_reader;
UPDATE public.release_function_contract c
SET proowner = p.proowner, proacl = p.proacl
FROM pg_proc p
WHERE p.oid = c.oid;
SQL

pg_dump --format=custom --file="$WORKDIR/pre-upgrade.dump" "$DBNAME"
install_tree "$ROOT_DIR" "$WORKDIR/package-1.2"

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
ALTER EXTENSION graph UPDATE TO '1.2.0';
SELECT 1 / CASE WHEN extversion = '1.2.0' THEN 1 ELSE 0 END
FROM pg_extension WHERE extname = 'graph';
DO $$
DECLARE
    wrong bigint;
BEGIN
    SELECT count(*) INTO wrong
    FROM public.release_function_contract c
    JOIN pg_proc p ON p.oid = c.oid
    WHERE p.proowner <> c.proowner OR p.proacl IS DISTINCT FROM c.proacl;
    IF wrong <> 0 THEN
        RAISE EXCEPTION '1.2 update changed an existing function owner or ACL';
    END IF;

    SELECT count(*) INTO wrong
    FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'graph'
      AND (
        (p.proname = 'edge_types'
         AND pg_get_function_identity_arguments(p.oid) = 'after_type_id bigint, max_rows integer')
        OR
        (p.proname = 'reset'
         AND pg_get_function_identity_arguments(p.oid) = 'clear_registrations boolean')
      )
      AND has_function_privilege('public', p.oid, 'EXECUTE');
    IF wrong <> 2 THEN
        RAISE EXCEPTION '1.2 added functions or PUBLIC EXECUTE grants are missing';
    END IF;

    SELECT count(*) INTO wrong
    FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'graph'
      AND (
        (p.proname = 'edge_types'
         AND pg_get_function_identity_arguments(p.oid) = 'after_type_id bigint, max_rows integer'
         AND (p.prosecdef OR p.proconfig IS NOT NULL))
        OR
        (p.proname = 'reset'
         AND pg_get_function_identity_arguments(p.oid) = 'clear_registrations boolean'
         AND (NOT p.prosecdef
              OR p.proconfig IS DISTINCT FROM ARRAY['search_path=pg_catalog, pg_temp']))
      );
    IF wrong <> 0 THEN
        RAISE EXCEPTION '1.2 added function metadata differs from a fresh install';
    END IF;
END
$$;
SELECT 1 / CASE WHEN count(*) = 3 THEN 1 ELSE 0 END FROM graph.edge_types();
SELECT 1 / CASE WHEN string_agg(node_id, ',' ORDER BY depth, node_id) = 'a,b' THEN 1 ELSE 0 END
FROM graph.traverse(
    'public.release_nodes'::regclass, 'a', 1,
    edge_types := ARRAY['works_at'], hydrate := false
);

SELECT graph.reset(true);
SELECT 1 / CASE WHEN count(*) = 0 THEN 1 ELSE 0 END FROM graph._registered_tables;
SELECT 1 / CASE WHEN count(*) = 0 THEN 1 ELSE 0 END FROM graph._registered_edges;
SELECT 1 / CASE WHEN count(*) = 4 THEN 1 ELSE 0 END FROM public.release_nodes;

INSERT INTO public.release_nodes
SELECT 'n' || value, 'Node ' || value FROM generate_series(0, 256) AS value;
INSERT INTO public.release_edges
SELECT 1000 + value, 'n0', 'n' || value, 'rel_' || lpad(value::text, 3, '0')
FROM generate_series(1, 256) AS value;
SELECT graph.add_table('public.release_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge(
    'public.release_edges'::regclass, 'from_id',
    'public.release_nodes'::regclass, 'to_id', 'relationship', false,
    label_column := 'relationship_name'
);
SELECT * FROM graph.build();
SELECT 1 / CASE WHEN count(*) = 259 THEN 1 ELSE 0 END FROM graph.edge_types();
SELECT 1 / CASE WHEN count(*) = 1 THEN 1 ELSE 0 END
FROM graph.traverse(
    'public.release_nodes'::regclass, 'n0', 1,
    edge_types := ARRAY['rel_256'], hydrate := false
) WHERE depth = 1 AND node_id = 'n256';
SQL

# Rollback restores the pre-upgrade backup with the matching 1.1 package.
# A 1.1 binary must never be installed over 1.2 catalogs or v7 artifacts.
psql -X -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT graph.reset(true)"
dropdb "$DBNAME"
install_package "$WORKDIR/package-1.1"
createdb "$RESTORE_DB"
pg_restore --dbname="$RESTORE_DB" "$WORKDIR/pre-upgrade.dump"
psql -X -v ON_ERROR_STOP=1 "$RESTORE_DB" <<'SQL'
SELECT 1 / CASE WHEN extversion = '1.1.0' THEN 1 ELSE 0 END
FROM pg_extension WHERE extname = 'graph';
SELECT 1 / CASE WHEN count(*) = 4 THEN 1 ELSE 0 END FROM public.release_nodes;
SELECT graph.reset();
SELECT graph.add_table('public.release_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge(
    'public.release_edges'::regclass, 'from_id',
    'public.release_nodes'::regclass, 'to_id', 'relationship', false,
    label_column := 'relationship_name'
);
SELECT * FROM graph.build();
SELECT 1 / CASE WHEN count(*) = 2 THEN 1 ELSE 0 END
FROM graph.traverse(
    'public.release_nodes'::regclass, 'a', 3,
    edge_types := ARRAY['works_at', 'founded'], hydrate := false
) WHERE depth > 0;
SQL

dropdb "$RESTORE_DB"
dropdb --if-exists "$DBNAME"
dropuser --if-exists pggraph_v1_2_reader >/dev/null 2>&1 || true
dropuser --if-exists pggraph_v1_2_owner >/dev/null 2>&1 || true

echo "Packaged 1.1 artifact update, open-type build, and backup-restore rollback passed for PostgreSQL ${PG_MAJOR}"
