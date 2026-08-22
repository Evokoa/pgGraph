#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_phase3_update}"
PG_CONFIG="${PG_CONFIG:-pg_config}"
PSQL="${PSQL:-psql}"
UPDATE_SQL="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/sql/graph--1.0.0--1.1.0.sql"
SHAREDIR="$($PG_CONFIG --sharedir)/extension"

dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

"$PSQL" -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
CREATE EXTENSION graph VERSION '1.0.0';
CREATE TABLE public.phase3_update_nodes (id text PRIMARY KEY, name text NOT NULL);
CREATE TABLE public.phase3_update_expectations AS
SELECT proc.proowner AS traverse_owner
FROM pg_catalog.pg_proc AS proc
JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = proc.pronamespace
WHERE namespace.nspname = 'graph'
  AND proc.oid = 'graph.traverse(oid,text,integer,text[],text,oid[],jsonb,text,text,text,boolean,boolean,integer,integer,integer,integer)'::regprocedure;
SELECT graph.add_table(
    'public.phase3_update_nodes'::regclass,
    'id',
    ARRAY['name']::text[],
    NULL
);
DROP ROLE IF EXISTS phase3_update_reader;
DROP ROLE IF EXISTS phase3_update_ungranted;
CREATE ROLE phase3_update_reader;
CREATE ROLE phase3_update_ungranted;
GRANT USAGE ON SCHEMA graph TO phase3_update_reader;
GRANT USAGE ON SCHEMA graph TO phase3_update_ungranted;
REVOKE EXECUTE ON FUNCTION graph.traverse(
    oid, text, integer, text[], text, oid[], jsonb, text, text, text,
    boolean, boolean, integer, integer, integer, integer
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION graph.traverse(
    oid, text, integer, text[], text, oid[], jsonb, text, text, text,
    boolean, boolean, integer, integer, integer, integer
) TO phase3_update_reader;

ALTER FUNCTION graph.traverse(
    oid, text, integer, text[], text, oid[], jsonb, text, text, text,
    boolean, boolean, integer, integer, integer, integer
) SECURITY DEFINER;
ALTER FUNCTION graph.traverse(
    oid, text, integer, text[], text, oid[], jsonb, text, text, text,
    boolean, boolean, integer, integer, integer, integer
) SET work_mem TO '64kB';
ALTER FUNCTION graph.connected_components() SECURITY DEFINER;
ALTER FUNCTION graph.component_stats() SECURITY DEFINER;
ALTER FUNCTION graph.build_status(text) SECURITY INVOKER;
ALTER FUNCTION graph.build_status(text) RESET ALL;
ALTER FUNCTION graph.build_status_for_graph(text, text, text, integer) SECURITY INVOKER;
ALTER FUNCTION graph.build_status_for_graph(text, text, text, integer) RESET ALL;
ALTER FUNCTION graph.maintenance_status(text) SECURITY INVOKER;
ALTER FUNCTION graph.maintenance_status(text) RESET ALL;
ALTER FUNCTION graph.maintenance_status_for_graph(text, text, text, integer) SECURITY INVOKER;
ALTER FUNCTION graph.maintenance_status_for_graph(text, text, text, integer) RESET ALL;
GRANT SELECT ON TABLE graph._build_jobs TO PUBLIC;
GRANT SELECT ON TABLE graph._maintenance_jobs TO PUBLIC;

DO $$
BEGIN
    IF to_regprocedure('graph._pending_sync_rows_for_current_role(bigint)') IS NULL
       OR to_regprocedure('graph._pending_sync_rows_for_current_role()') IS NOT NULL THEN
        RAISE EXCEPTION 'installed 1.0 package has an unexpected pending-sync helper signature';
    END IF;
END
$$;
SQL

cp "$UPDATE_SQL" "$SHAREDIR/"

"$PSQL" -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
ALTER EXTENSION graph UPDATE TO '1.1.0';

DO $$
DECLARE
    wrong_modes bigint;
    wrong_status_modes bigint;
    missing_helpers bigint;
    traverse_settings text[];
BEGIN
    SELECT count(*) INTO wrong_modes
    FROM pg_catalog.pg_proc AS proc
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = proc.pronamespace
    WHERE namespace.nspname = 'graph'
      AND proc.proname IN ('traverse', 'connected_components', 'component_stats')
      AND (
          proc.prosecdef
          OR EXISTS (
              SELECT 1
              FROM unnest(COALESCE(proc.proconfig, ARRAY[]::text[])) AS setting
              WHERE setting LIKE 'search_path=%'
          )
      );
    IF wrong_modes <> 0 THEN
        RAISE EXCEPTION 'topology functions retained SECURITY DEFINER or search_path';
    END IF;

    SELECT proc.proconfig INTO traverse_settings
    FROM pg_catalog.pg_proc AS proc
    WHERE proc.oid = 'graph.traverse(oid,text,integer,text[],text,oid[],jsonb,text,text,text,boolean,boolean,integer,integer,integer,integer)'::regprocedure;
    IF NOT COALESCE(traverse_settings, ARRAY[]::text[]) @> ARRAY['work_mem=64kB'] THEN
        RAISE EXCEPTION 'topology update removed an unrelated function setting';
    END IF;

    SELECT count(*) INTO wrong_status_modes
    FROM pg_catalog.pg_proc AS proc
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = proc.pronamespace
    WHERE namespace.nspname = 'graph'
      AND proc.proname IN (
          'build_status', 'build_status_for_graph',
          'maintenance_status', 'maintenance_status_for_graph'
      )
      AND (
          NOT proc.prosecdef
          OR NOT COALESCE(proc.proconfig, ARRAY[]::text[])
              @> ARRAY['search_path=pg_catalog, pg_temp']
      );
    IF wrong_status_modes <> 0 THEN
        RAISE EXCEPTION 'job status functions are not pinned catalog mediators';
    END IF;

    SELECT count(*) INTO missing_helpers
    FROM (VALUES
        ('_require_selected_graph_privilege_for_current_role'),
        ('_graph_id_for_current_role_with_privilege'),
        ('_expire_projection_heartbeats_for_current_role'),
        ('_expire_sync_watermarks_for_current_role'),
        ('_record_sync_watermark_for_current_role'),
        ('_record_projection_heartbeat_for_current_role'),
        ('_max_sync_log_id_for_query_state'),
        ('_active_generation_count_for_current_role'),
        ('_enforce_loaded_graph_quota_for_current_role')
    ) AS expected(proname)
    WHERE NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_proc AS proc
        JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = proc.pronamespace
        WHERE namespace.nspname = 'graph'
          AND proc.proname = expected.proname
          AND proc.prosecdef
          AND COALESCE(proc.proconfig, ARRAY[]::text[])
              @> ARRAY['search_path=pg_catalog, pg_temp']
    );
    IF missing_helpers <> 0 THEN
        RAISE EXCEPTION 'one or more pinned catalog mediators are missing';
    END IF;
    IF pg_catalog.to_regprocedure(
        'graph._pending_sync_rows_for_current_role()'
    ) IS NULL THEN
        RAISE EXCEPTION 'no-argument pending-sync mediator is missing';
    END IF;
END
$$;

SELECT 1 / CASE WHEN extversion = '1.1.0' THEN 1 ELSE 0 END
FROM pg_catalog.pg_extension
WHERE extname = 'graph';
SELECT 1 / CASE WHEN count(*) = 1 THEN 1 ELSE 0 END
FROM graph.registered_tables()
WHERE table_name = 'phase3_update_nodes';
SELECT 1 / CASE WHEN count(*) = 1 THEN 1 ELSE 0 END
FROM graph.status();
DO $$
BEGIN
    PERFORM graph._max_sync_log_id_for_query_state();
    RAISE EXCEPTION 'query-state max-sync mediator accepted a direct SQL call';
EXCEPTION
    WHEN insufficient_privilege THEN NULL;
END
$$;
SELECT 1 / CASE WHEN has_function_privilege(
    'phase3_update_reader',
    'graph.traverse(oid,text,integer,text[],text,oid[],jsonb,text,text,text,boolean,boolean,integer,integer,integer,integer)',
    'EXECUTE'
) THEN 1 ELSE 0 END;
SELECT 1 / CASE WHEN EXISTS (
    SELECT 1
    FROM pg_catalog.pg_proc AS proc
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = proc.pronamespace
    CROSS JOIN LATERAL pg_catalog.aclexplode(proc.proacl) AS acl
    JOIN pg_catalog.pg_roles AS role ON role.oid = acl.grantee
    WHERE namespace.nspname = 'graph'
      AND proc.oid = 'graph.traverse(oid,text,integer,text[],text,oid[],jsonb,text,text,text,boolean,boolean,integer,integer,integer,integer)'::regprocedure
      AND role.rolname = 'phase3_update_reader'
      AND acl.privilege_type = 'EXECUTE'
) THEN 1 ELSE 0 END;
SELECT 1 / CASE WHEN NOT has_function_privilege(
    'phase3_update_ungranted',
    'graph.traverse(oid,text,integer,text[],text,oid[],jsonb,text,text,text,boolean,boolean,integer,integer,integer,integer)',
    'EXECUTE'
) THEN 1 ELSE 0 END;
SELECT 1 / CASE WHEN NOT has_table_privilege(
    'phase3_update_reader', 'graph._build_jobs', 'SELECT'
) THEN 1 ELSE 0 END;
SELECT 1 / CASE WHEN NOT has_table_privilege(
    'phase3_update_reader', 'graph._maintenance_jobs', 'SELECT'
) THEN 1 ELSE 0 END;
SELECT 1 / CASE WHEN proc.proowner = expected.traverse_owner THEN 1 ELSE 0 END
FROM pg_catalog.pg_proc AS proc
JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = proc.pronamespace
CROSS JOIN public.phase3_update_expectations AS expected
WHERE namespace.nspname = 'graph'
  AND proc.oid = 'graph.traverse(oid,text,integer,text[],text,oid[],jsonb,text,text,text,boolean,boolean,integer,integer,integer,integer)'::regprocedure;
DROP TABLE public.phase3_update_expectations;
SQL

echo "Phase 3 extension update smoke passed on database: $DBNAME"
