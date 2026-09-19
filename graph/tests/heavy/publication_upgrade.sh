#!/usr/bin/env bash
set -euo pipefail

# Requires the released 1.2.0 install SQL and candidate 1.2.1 update SQL.
# The package-backed wrapper uses separate backends around library replacement.
mode="${1:-all-current}"
case "$mode" in
  all-current|prepare-1.2.0|verify-1.2.1) ;;
  *) echo "Usage: $0 [all-current|prepare-1.2.0|verify-1.2.1]" >&2; exit 2 ;;
esac
PGGRAPH_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$PGGRAPH_ROOT/scripts/lib/pggraph-common.sh"
database="${DB_PREFIX:-pggraph_publication_upgrade_$$}"
pggraph_validate_database_name "$database"
if [[ "$mode" != "verify-1.2.1" ]]; then
createdb "$database"
psql -X -v ON_ERROR_STOP=1 -d "$database" <<'SQL'
CREATE EXTENSION graph VERSION '1.2.0';
DO $$ BEGIN
  IF to_regclass('graph._projection_heads') IS NOT NULL THEN
    RAISE EXCEPTION 'upgrade test requires the released 1.2.0 schema';
  END IF;
END $$;
SELECT 'pggraph_upgrade_owner_' || substr(md5(current_database()), 1, 12) AS role
\gset upgrade_
CREATE ROLE :"upgrade_role" NOLOGIN;
ALTER FUNCTION graph.test_enabled() OWNER TO :"upgrade_role";
REVOKE EXECUTE ON FUNCTION graph.test_enabled() FROM PUBLIC;
CREATE TABLE public.pggraph_upgrade_original_functions AS
SELECT oid, proowner, proacl FROM pg_proc WHERE pronamespace = 'graph'::regnamespace;
CREATE TABLE n(id text PRIMARY KEY, name text NOT NULL);
CREATE TABLE edges (
    id bigint PRIMARY KEY,
    from_id text NOT NULL REFERENCES n(id),
    to_id text NOT NULL REFERENCES n(id),
    relationship_name text NOT NULL
);
INSERT INTO n VALUES ('a', 'Alpha'), ('b', 'Beta');
INSERT INTO edges VALUES (1, 'a', 'b', 'original_label');
SELECT graph.add_table('n'::regclass, 'id', ARRAY['name']);
-- Edges are not registered as nodes, so both endpoint columns belong to edges.
SELECT graph.add_edge('edges'::regclass, 'from_id', 'n'::regclass,
                     to_column := 'to_id', label := 'relationship',
                     bidirectional := false, label_column := 'relationship_name');
SQL
if [[ "$mode" == "prepare-1.2.0" ]]; then
  psql -X -v ON_ERROR_STOP=1 -d "$database" <<'SQL'
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
DO $$ BEGIN
  IF (SELECT extversion FROM pg_extension WHERE extname = 'graph') IS DISTINCT FROM '1.2.0'
     OR (SELECT array_agg(node_id ORDER BY depth) FROM graph.traverse(
       'n'::regclass, 'a', 2, edge_types := ARRAY['original_label'], direction := 'out'))
        IS DISTINCT FROM ARRAY['a', 'b']::text[]
     OR (SELECT edge_path FROM graph.traverse(
       'n'::regclass, 'a', 2, edge_types := ARRAY['original_label'], direction := 'out')
       WHERE node_id = 'b' AND depth = 1) IS DISTINCT FROM '["original_label"]'::jsonb THEN
    RAISE EXCEPTION 'released 1.2.0 package did not build the upgrade fixture';
  END IF;
END $$;
SQL
  printf 'Released 1.2.0 publication fixture prepared (%s)\n' "$database"
  exit 0
fi
fi
psql -X -v ON_ERROR_STOP=1 -d "$database" <<'SQL'
DO $$ BEGIN
  IF (SELECT extversion FROM pg_extension WHERE extname = 'graph') IS DISTINCT FROM '1.2.0'
     OR to_regclass('graph._projection_heads') IS NOT NULL
     OR to_regclass('public.pggraph_upgrade_original_functions') IS NULL THEN
    RAISE EXCEPTION 'verification requires the prepared 1.2.0 catalog';
  END IF;
END $$;
BEGIN;
ALTER EXTENSION graph UPDATE TO '1.2.1';
ROLLBACK;
DO $$ BEGIN
  IF (SELECT extversion FROM pg_extension WHERE extname = 'graph') IS DISTINCT FROM '1.2.0'
     OR to_regclass('graph._projection_heads') IS NOT NULL
     OR to_regprocedure('graph._sync_retention_catalog_for_current_role()') IS NOT NULL THEN
    RAISE EXCEPTION 'rolled-back update changed the 1.2.0 catalog';
  END IF;
END $$;
ALTER EXTENSION graph UPDATE TO '1.2.1';
DO $$ BEGIN
  IF (SELECT extversion FROM pg_extension WHERE extname = 'graph') IS DISTINCT FROM '1.2.1' THEN
    RAISE EXCEPTION 'publication update did not install version 1.2.1';
  END IF;
  IF EXISTS (
    SELECT 1 FROM public.pggraph_upgrade_original_functions original
    LEFT JOIN pg_proc current ON current.oid = original.oid
    WHERE current.oid IS NULL OR current.proowner <> original.proowner
       OR current.proacl IS DISTINCT FROM original.proacl
  ) THEN
    RAISE EXCEPTION 'upgrade changed an existing function owner, grant or identity';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'graph'
             AND 'graph._projection_heads'::regclass = ANY(extconfig)) THEN
    RAISE EXCEPTION 'derived publication metadata must not be dumped';
  END IF;
  IF EXISTS (
    SELECT 1 FROM pg_class, LATERAL aclexplode(relacl) AS grant_entry
    WHERE oid = 'graph._projection_heads'::regclass AND grant_entry.grantee = 0
  ) THEN
    RAISE EXCEPTION 'publication heads grant direct access to PUBLIC';
  END IF;
  IF EXISTS (SELECT 1 FROM graph._projection_heads) THEN
    RAISE EXCEPTION 'upgrade must not adopt filesystem artifacts';
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_proc
    WHERE oid = 'graph.sync_retention()'::regprocedure
      AND pg_catalog.pg_get_function_result(oid) =
          'TABLE(eligible_prune_floor bigint, prune_blocker text, retained_graph_rows bigint, database_sync_log_bytes bigint, active_sync_watermark_backends integer)'
      AND prosecdef
      AND proconfig = ARRAY['search_path=pg_catalog, pg_temp']::text[]
  ) THEN
    RAISE EXCEPTION 'upgraded sync_retention has an incorrect result shape or security boundary';
  END IF;
  IF (SELECT count(*) FROM graph.sync_retention()) <> 1 THEN
    RAISE EXCEPTION 'upgraded sync_retention did not return one diagnostic row';
  END IF;
END $$;
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
DO $$ BEGIN
  IF (SELECT array_agg(node_id ORDER BY depth) FROM graph.traverse(
      'n'::regclass, 'a', 2, edge_types := ARRAY['original_label'], direction := 'out'))
       IS DISTINCT FROM ARRAY['a', 'b']::text[]
     OR (SELECT edge_path FROM graph.traverse(
       'n'::regclass, 'a', 2, edge_types := ARRAY['original_label'], direction := 'out')
       WHERE node_id = 'b' AND depth = 1) IS DISTINCT FROM '["original_label"]'::jsonb THEN
    RAISE EXCEPTION 'upgraded graph did not rebuild';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM graph._projection_heads) THEN
    RAISE EXCEPTION 'upgraded rebuild did not publish a generation';
  END IF;
  IF (SELECT jsonb_object_agg(id, name) FROM public.n)
       IS DISTINCT FROM '{"a":"Alpha","b":"Beta"}'::jsonb
     OR (SELECT count(*) FROM public.edges WHERE id = 1 AND from_id = 'a'
         AND to_id = 'b' AND relationship_name = 'original_label') <> 1
     OR (SELECT count(*) FROM public.edges) <> 1 THEN
    RAISE EXCEPTION 'upgrade changed source rows or properties';
  END IF;
END $$;
SELECT graph.enable_sync();
INSERT INTO n VALUES ('c', 'Gamma');
INSERT INTO edges VALUES (2, 'b', 'c', 'new_after_upgrade');
SELECT * FROM graph.apply_sync();
DO $$ BEGIN
  IF (SELECT array_agg(node_id ORDER BY depth) FROM graph.traverse(
      'n'::regclass, 'a', 3, edge_types := ARRAY['original_label', 'new_after_upgrade'],
      direction := 'out')) IS DISTINCT FROM ARRAY['a', 'b', 'c']::text[]
     OR (SELECT array_agg(node_id ORDER BY depth) FROM graph.traverse(
      'n'::regclass, 'b', 1, edge_types := ARRAY['new_after_upgrade'],
      direction := 'out')) IS DISTINCT FROM ARRAY['b', 'c']::text[]
     OR (SELECT edge_path FROM graph.traverse(
       'n'::regclass, 'a', 3, edge_types := ARRAY['original_label', 'new_after_upgrade'],
       direction := 'out') WHERE node_id = 'c' AND depth = 2)
        IS DISTINCT FROM '["original_label", "new_after_upgrade"]'::jsonb THEN
    RAISE EXCEPTION 'upgraded sync lost the new node, edge or unseen relationship label';
  END IF;
  IF (SELECT jsonb_object_agg(id, name) FROM public.n)
       IS DISTINCT FROM '{"a":"Alpha","b":"Beta","c":"Gamma"}'::jsonb THEN
    RAISE EXCEPTION 'upgraded sync changed source properties';
  END IF;
END $$;
SQL
psql -X -v ON_ERROR_STOP=1 -d "$database" -f "$PGGRAPH_ROOT/graph/tests/heavy/sync_health_authorization.sql"
fresh_database="${database:0:57}_fresh"
pggraph_validate_database_name "$fresh_database"
createdb "$fresh_database"
psql -X -v ON_ERROR_STOP=1 -d "$fresh_database" -c 'CREATE EXTENSION graph;'
psql -X -v ON_ERROR_STOP=1 -d "$fresh_database" -f "$PGGRAPH_ROOT/graph/tests/heavy/sync_health_authorization.sql"
helper_metadata() {
  psql -X -qAt -v ON_ERROR_STOP=1 -d "$1" <<'SQL'
SELECT jsonb_build_object(
    'arguments', pg_get_function_identity_arguments(p.oid),
    'result', pg_get_function_result(p.oid),
    'owner', pg_get_userbyid(p.proowner),
    'definer', p.prosecdef, 'strict', p.proisstrict,
    'volatility', p.provolatile, 'parallel', p.proparallel,
    'settings', p.proconfig, 'library', p.probin, 'symbol', p.prosrc,
    'extension', (SELECT e.extname FROM pg_depend d JOIN pg_extension e ON e.oid = d.refobjid
                  WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid
                    AND d.refclassid = 'pg_extension'::regclass AND d.deptype = 'e'),
    'acl', (SELECT jsonb_agg(jsonb_build_array(
                 pg_get_userbyid(a.grantor), COALESCE(r.rolname, 'PUBLIC'),
                 a.privilege_type, a.is_grantable) ORDER BY a.grantee, a.privilege_type)
            FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) a
            LEFT JOIN pg_roles r ON r.oid = a.grantee))
FROM pg_proc p WHERE p.oid = 'graph._sync_retention_catalog_for_current_role()'::regprocedure;
SQL
}
upgraded_metadata="$(helper_metadata "$database")"
fresh_metadata="$(helper_metadata "$fresh_database")"
[[ -n "$upgraded_metadata" && "$upgraded_metadata" == "$fresh_metadata" ]] || {
  echo 'Fresh and upgraded diagnostic helper metadata differ' >&2
  exit 1
}
printf 'Fresh/upgraded helper metadata: %s\n' "$upgraded_metadata"
printf 'Transactional publication upgrade passed (%s)\n' "$database"
