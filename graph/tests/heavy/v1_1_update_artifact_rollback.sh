#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_v1_1_update}"
RESTORE_DB="${RESTORE_DB:-${DBNAME}_rollback}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/pggraph-v1-1-update.XXXXXX")"

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
PG_BIN_DIR="$(dirname "$PG_CONFIG")"

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

if [[ -n "${V1_SOURCE_ARCHIVE:-}" ]]; then
  tar -xf "$V1_SOURCE_ARCHIVE" -C "$WORKDIR"
else
  git -C "$ROOT_DIR/.." archive v1.0.0 graph | tar -x -C "$WORKDIR"
fi
install_tree "$WORKDIR/graph" "$WORKDIR/package-1.0"

createdb "$DBNAME"
psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
CREATE EXTENSION graph VERSION '1.0.0';
DROP ROLE IF EXISTS pggraph_v1_1_reader;
DROP ROLE IF EXISTS pggraph_v1_1_control;
DROP ROLE IF EXISTS pggraph_v1_1_owner;
CREATE ROLE pggraph_v1_1_reader;
CREATE ROLE pggraph_v1_1_control;
CREATE ROLE pggraph_v1_1_owner;
CREATE TABLE public.release_nodes (
    id text PRIMARY KEY,
    tenant text NOT NULL,
    name text NOT NULL
);
CREATE TABLE public.release_edges (
    id bigint PRIMARY KEY,
    from_id text NOT NULL REFERENCES public.release_nodes(id),
    to_id text NOT NULL REFERENCES public.release_nodes(id)
);
INSERT INTO public.release_nodes VALUES
    ('a', 'visible', 'A'), ('b', 'visible', 'B'), ('c', 'visible', 'C'),
    ('secret', 'hidden', 'Secret');
INSERT INTO public.release_edges VALUES
    (1, 'a', 'b'), (2, 'b', 'c'), (3, 'b', 'secret');
SELECT graph.add_table('public.release_nodes'::regclass, 'id', ARRAY['tenant', 'name']);
SELECT graph.add_edge(
    'public.release_edges'::regclass, 'from_id',
    'public.release_nodes'::regclass, 'to_id', 'linked', false
);
SET graph.allow_rls_tables = on;
SELECT * FROM graph.build();
GRANT USAGE ON SCHEMA graph TO pggraph_v1_1_reader;
GRANT SELECT ON public.release_nodes, public.release_edges TO pggraph_v1_1_reader;
ALTER TABLE public.release_nodes ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.release_edges ENABLE ROW LEVEL SECURITY;
CREATE POLICY release_nodes_visible ON public.release_nodes
    FOR SELECT TO pggraph_v1_1_reader USING (tenant = 'visible');
CREATE POLICY release_edges_visible ON public.release_edges
    FOR SELECT TO pggraph_v1_1_reader USING (id = 1);
CREATE TABLE public.release_function_contract AS
SELECT p.oid, p.proname, p.proowner
FROM pg_proc p
JOIN pg_namespace n ON n.oid = p.pronamespace
WHERE n.nspname = 'graph'
  AND p.proname IN ('traverse', 'connected_components', 'component_stats');
DO $$
DECLARE
    function_oid oid;
BEGIN
    FOR function_oid IN SELECT oid FROM public.release_function_contract LOOP
        EXECUTE format('ALTER FUNCTION %s OWNER TO pggraph_v1_1_owner', function_oid::regprocedure);
        EXECUTE format('REVOKE EXECUTE ON FUNCTION %s FROM PUBLIC', function_oid::regprocedure);
        EXECUTE format(
            'GRANT EXECUTE ON FUNCTION %s TO pggraph_v1_1_reader',
            function_oid::regprocedure
        );
    END LOOP;
    UPDATE public.release_function_contract c
       SET proowner = p.proowner
      FROM pg_proc p
     WHERE p.oid = c.oid;
END
$$;
SQL

"$PG_BIN_DIR/pg_dump" --format=custom --file="$WORKDIR/pre-upgrade.dump" "$DBNAME"
install_tree "$ROOT_DIR" "$WORKDIR/package-1.1"

psql -X -v ON_ERROR_STOP=1 "$DBNAME" <<'SQL'
ALTER EXTENSION graph UPDATE TO '1.1.0';
DO $$
DECLARE
    wrong bigint;
BEGIN
    SELECT count(*) INTO wrong
    FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'graph'
      AND p.proname IN ('traverse', 'connected_components', 'component_stats')
      AND (p.prosecdef OR p.proconfig IS NOT NULL);
    IF wrong <> 0 THEN
        RAISE EXCEPTION 'updated invoker metadata differs from fresh 1.1';
    END IF;
    SELECT count(*) INTO wrong
    FROM public.release_function_contract c
    JOIN pg_proc p ON p.oid = c.oid
    WHERE p.proowner <> c.proowner
       OR EXISTS (
              SELECT 1
              FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) acl
              WHERE acl.grantee = 0 AND acl.privilege_type = 'EXECUTE'
          )
       OR NOT EXISTS (
              SELECT 1
              FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) acl
              WHERE acl.grantee = 'pggraph_v1_1_reader'::regrole
                AND acl.privilege_type = 'EXECUTE'
          )
       OR EXISTS (
              SELECT 1
              FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) acl
              WHERE acl.grantee = 'pggraph_v1_1_control'::regrole
                AND acl.privilege_type = 'EXECUTE'
          );
    IF wrong <> 0 THEN
        RAISE EXCEPTION 'updated ownership or EXECUTE grants differ from the 1.0 contract';
    END IF;
END
$$;
SET ROLE pggraph_v1_1_reader;
SELECT 1 / CASE WHEN string_agg(node_id, ',' ORDER BY depth, node_id) = 'a,b' THEN 1 ELSE 0 END
FROM graph.traverse('public.release_nodes'::regclass, 'a', 3, hydrate := false);
SELECT 1 / CASE WHEN count(*) = 0 THEN 1 ELSE 0 END
FROM graph.traverse('public.release_nodes'::regclass, 'secret', 1, hydrate := false);
RESET ROLE;
SELECT 1 / CASE WHEN extversion = '1.1.0' THEN 1 ELSE 0 END
FROM pg_extension WHERE extname = 'graph';
SQL

# Rollback is backup restoration with the matching 1.0 package, never an
# in-place extension downgrade or old shared library over 1.1 catalogs.
psql -X -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT graph.reset()"
dropdb "$DBNAME"
install_package "$WORKDIR/package-1.0"
createdb "$RESTORE_DB"
"$PG_BIN_DIR/pg_restore" --dbname="$RESTORE_DB" "$WORKDIR/pre-upgrade.dump"
psql -X -v ON_ERROR_STOP=1 "$RESTORE_DB" <<'SQL'
SELECT 1 / CASE WHEN extversion = '1.0.0' THEN 1 ELSE 0 END
FROM pg_extension WHERE extname = 'graph';
SELECT 1 / CASE WHEN count(*) = 4 THEN 1 ELSE 0 END FROM public.release_nodes;
-- Logical restore assigns database-local relation OIDs. The projection is
-- derived state, so restore source tables, reset registration, and rebuild.
SELECT graph.reset();
SELECT graph.add_table('public.release_nodes'::regclass, 'id', ARRAY['tenant', 'name']);
SELECT graph.add_edge(
    'public.release_edges'::regclass, 'from_id',
    'public.release_nodes'::regclass, 'to_id', 'linked', false
);
SET graph.allow_rls_tables = on;
SELECT * FROM graph.build();
SELECT 1 / CASE WHEN count(*) = 4 THEN 1 ELSE 0 END
FROM graph.traverse('public.release_nodes'::regclass, 'a', 3, hydrate := false);
SQL

dropdb "$RESTORE_DB"
dropdb --if-exists "$DBNAME"
dropuser --if-exists pggraph_v1_1_reader >/dev/null 2>&1 || true
dropuser --if-exists pggraph_v1_1_control >/dev/null 2>&1 || true
dropuser --if-exists pggraph_v1_1_owner >/dev/null 2>&1 || true

echo "Packaged 1.0 artifact update and backup-restore rollback passed for PostgreSQL ${PG_MAJOR}"
