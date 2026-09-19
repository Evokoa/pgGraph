\set ON_ERROR_STOP on
SELECT 'pggraph_health_reader_' || substr(md5(current_database()), 1, 12) AS reader,
       'pggraph_health_admin_' || substr(md5(current_database()), 1, 12) AS admin,
       'pggraph_health_owner_' || substr(md5(current_database()), 1, 12) AS owner,
       'pggraph_health_builder_' || substr(md5(current_database()), 1, 12) AS builder
\gset health_
CREATE ROLE :"health_reader" NOLOGIN;
CREATE ROLE :"health_admin" NOLOGIN;
CREATE ROLE :"health_owner" NOLOGIN;
CREATE ROLE :"health_builder" NOLOGIN;
GRANT USAGE ON SCHEMA graph, public TO :"health_reader", :"health_admin", :"health_owner", :"health_builder";
CREATE TABLE public.health_nodes(id text PRIMARY KEY);
INSERT INTO public.health_nodes VALUES ('visible'), ('hidden');
ALTER TABLE public.health_nodes ENABLE ROW LEVEL SECURITY;
CREATE POLICY health_visible ON public.health_nodes FOR SELECT
    TO :"health_reader", :"health_admin", :"health_owner" USING (id = 'visible');
GRANT SELECT ON public.health_nodes TO :"health_reader", :"health_admin", :"health_owner";
SELECT * FROM graph.create_graph('health_scope');
SELECT graph.set_current_graph('health_scope');
SELECT graph.add_table('public.health_nodes'::regclass, 'id');
SELECT * FROM graph.build_graph('health_scope', force_persist := true);
SELECT graph.grant_graph('health_scope', :'health_reader', 'read');
SELECT graph.grant_graph('health_scope', :'health_admin', 'admin');
SELECT graph.grant_graph('health_scope', :'health_builder', 'build');
SELECT graph.grant_graph('health_scope', current_user, 'admin');
UPDATE graph._graphs SET owner_role = :'health_owner'::regrole WHERE graph_name = 'health_scope';

DO $$ BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_proc p JOIN pg_extension e ON e.extname = 'graph'
    WHERE p.oid = 'graph._sync_retention_catalog_for_current_role()'::regprocedure
      AND p.prosecdef AND p.proisstrict AND p.provolatile = 'v' AND p.proparallel = 'u'
      AND p.proconfig = ARRAY['search_path=pg_catalog, pg_temp']::text[]
      AND p.proowner = e.extowner AND p.pronargs = 0
      AND pg_get_function_result(p.oid) =
          'TABLE(heartbeat_floor bigint, active_backends integer, has_sources boolean, shared_source boolean, alternate_artifact_root boolean)'
      AND EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass
                  AND d.objid = p.oid AND d.refclassid = 'pg_extension'::regclass
                  AND d.refobjid = e.oid AND d.deptype = 'e')
      AND EXISTS (SELECT 1 FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) a
                  WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE')
  ) OR (SELECT prosecdef FROM pg_proc WHERE oid = 'graph.sync_health()'::regprocedure) THEN
    RAISE EXCEPTION 'sync-health function security contract changed';
  END IF;
END $$;

SET ROLE :"health_reader";
SET search_path = pg_temp, public, graph, pg_catalog;
CREATE TEMP TABLE _sync_watermarks(applied_sync_id bigint);
INSERT INTO _sync_watermarks VALUES (-999);
BEGIN READ ONLY;
DO $$ DECLARE relation text; BEGIN
  IF (SELECT count(*) FROM graph.sync_health()) <> 1
     OR (SELECT count(*) FROM graph.status()) <> 1
     OR NOT (SELECT has_sources AND NOT shared_source AND NOT alternate_artifact_root
             FROM graph._sync_retention_catalog_for_current_role())
     OR (SELECT count(*) FROM graph.traverse('public.health_nodes'::regclass, 'hidden', 0)) <> 0
     OR (SELECT count(*) FROM graph.traverse('public.health_nodes'::regclass, 'visible', 0)) <> 1 THEN
    RAISE EXCEPTION 'graph reader diagnostics changed selected scope or RLS visibility';
  END IF;
  FOREACH relation IN ARRAY ARRAY['_graphs', '_sync_watermarks', '_projection_heads'] LOOP
    BEGIN
      EXECUTE format('SELECT 1 FROM graph.%I LIMIT 1', relation);
      RAISE EXCEPTION 'reader accessed private catalog %', relation;
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
  END LOOP;
  BEGIN
    PERFORM * FROM graph.sync_retention();
    RAISE EXCEPTION 'reader accessed admin-only sync retention';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
END $$;
COMMIT;
BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
DO $$ BEGIN
  IF NOT (SELECT sync_log_retention_floor IS NULL AND NOT sync_log_prune_recommended
          FROM graph.sync_health()) THEN
    RAISE EXCEPTION 'fixed snapshot allowed pruning';
  END IF;
END $$;
COMMIT;
SELECT * FROM graph.sync_health();
RESET search_path;
RESET ROLE;

SET ROLE :"health_owner";
SELECT * FROM graph.sync_health();
SELECT * FROM graph._sync_retention_catalog_for_current_role();
RESET ROLE;
SET ROLE :"health_admin";
SELECT * FROM graph.sync_health();
SELECT * FROM graph._sync_retention_catalog_for_current_role();
RESET ROLE;

-- A build grant makes the graph visible but does not authorize read diagnostics.
SET ROLE :"health_builder";
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph._sync_retention_catalog_for_current_role();
    RAISE EXCEPTION 'helper accepted a caller without graph read';
  EXCEPTION WHEN insufficient_privilege THEN
    IF SQLERRM NOT LIKE 'Permission denied%' THEN RAISE; END IF;
  END;
  BEGIN
    PERFORM * FROM graph.sync_health();
    RAISE EXCEPTION 'sync-health accepted a caller without graph read';
  EXCEPTION WHEN insufficient_privilege THEN
    IF SQLERRM NOT LIKE 'Permission denied%' THEN RAISE; END IF;
  END;
END $$;
RESET ROLE;

REVOKE SELECT ON public.health_nodes FROM :"health_reader";
SET ROLE :"health_reader";
SELECT * FROM graph.sync_health();
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph.traverse('public.health_nodes'::regclass, 'visible', 0);
    RAISE EXCEPTION 'diagnostics elevated source-table privileges';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
END $$;
RESET ROLE;
GRANT SELECT ON public.health_nodes TO :"health_reader";

-- Retain graph visibility through build so a revoked read is an explicit denial.
SELECT graph.grant_graph('health_scope', :'health_reader', 'build');
SELECT graph.revoke_graph('health_scope', :'health_reader', 'read');
SET ROLE :"health_reader";
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph._sync_retention_catalog_for_current_role();
    RAISE EXCEPTION 'revoked reader retained diagnostic access';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
END $$;
RESET ROLE;
SELECT graph.grant_graph('health_scope', :'health_reader', 'read');

SELECT * FROM graph.create_graph('health_empty');
SELECT graph.grant_graph('health_empty', :'health_reader', 'read');
SELECT graph.set_current_graph('health_empty');
SET ROLE :"health_reader";
DO $$ BEGIN
  IF NOT (SELECT NOT has_sources AND heartbeat_floor IS NULL AND active_backends = 0
          FROM graph._sync_retention_catalog_for_current_role()) THEN
    RAISE EXCEPTION 'diagnostic helper leaked the previous graph scope';
  END IF;
END $$;
RESET ROLE;
INSERT INTO graph._sync_watermarks (graph_id, backend_pid, database_oid, applied_sync_id, expires_at)
SELECT graph_id, -5432, (SELECT oid FROM pg_database WHERE datname = current_database()),
       42, now() + interval '1 hour'
FROM graph._graphs WHERE graph_name = 'health_empty';
INSERT INTO graph._sync_watermarks (graph_id, backend_pid, database_oid, applied_sync_id, expires_at)
SELECT graph_id, -5433, (SELECT oid FROM pg_database WHERE datname = current_database()),
       0, now() - interval '1 hour'
FROM graph._graphs WHERE graph_name = 'health_empty';
SET ROLE :"health_reader";
DO $$ BEGIN
  IF NOT (SELECT NOT has_sources AND heartbeat_floor = 42 AND active_backends = 1
          FROM graph._sync_retention_catalog_for_current_role()) THEN
    RAISE EXCEPTION 'helper included expired or other-graph heartbeats';
  END IF;
END $$;
RESET ROLE;
SELECT * FROM graph.create_graph('health_hidden');
SELECT graph.set_current_graph('health_hidden');
SET ROLE :"health_reader";
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph._sync_retention_catalog_for_current_role();
    RAISE EXCEPTION 'helper exposed an inaccessible graph';
  EXCEPTION WHEN invalid_parameter_value THEN
    IF SQLERRM NOT LIKE '%selected graph metadata is missing%' THEN RAISE; END IF;
  END;
END $$;
RESET ROLE;
SELECT graph.set_current_graph('default');
\echo Sync-health reader authorization passed
