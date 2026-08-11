-- pgGraph 1.0.0 -> 1.1.0
--
-- This script is extended by later 1.1 phases. Existing functions are altered
-- in place so their owner and explicit EXECUTE grants remain unchanged.

ALTER FUNCTION graph.traverse(
    oid, text, integer, text[], text, oid[], jsonb, text, text, text,
    boolean, boolean, integer, integer, integer, integer
) SECURITY INVOKER;

ALTER FUNCTION graph.connected_components() SECURITY INVOKER;
ALTER FUNCTION graph.component_stats() SECURITY INVOKER;

-- Job status remains callable by readers, but executes through a pinned
-- catalog-only boundary after raw job-table access is revoked.
ALTER FUNCTION graph.build_status(text) SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp;
ALTER FUNCTION graph.build_status_for_graph(text, text, text, integer) SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp;
ALTER FUNCTION graph.maintenance_status(text) SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp;
ALTER FUNCTION graph.maintenance_status_for_graph(text, text, text, integer) SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp;

REVOKE ALL ON TABLE graph._build_jobs FROM PUBLIC;
REVOKE ALL ON TABLE graph._maintenance_jobs FROM PUBLIC;

-- Pinned catalog mediators let invoker entry points authorize the outer role
-- without switching PostgreSQL user IDs or exposing internal catalog tables.
CREATE FUNCTION graph._require_selected_graph_privilege_for_current_role(
    privilege text
) RETURNS boolean
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'require_selected_graph_privilege_for_current_role_wrapper';

CREATE FUNCTION graph._graph_id_for_current_role_with_privilege(
    graph_name text,
    graph_tenant text DEFAULT NULL,
    graph_namespace text DEFAULT NULL,
    privilege text DEFAULT 'read'
) RETURNS text
SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'graph_id_for_current_role_with_privilege_wrapper';

CREATE FUNCTION graph._expire_projection_heartbeats_for_current_role()
RETURNS boolean
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'expire_projection_heartbeats_for_current_role_wrapper';

CREATE FUNCTION graph._expire_sync_watermarks_for_current_role()
RETURNS boolean
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'expire_sync_watermarks_for_current_role_wrapper';

CREATE FUNCTION graph._record_sync_watermark_for_current_role()
RETURNS boolean
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'record_sync_watermark_for_current_role_wrapper';

CREATE FUNCTION graph._record_projection_heartbeat_for_current_role()
RETURNS boolean
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'record_projection_heartbeat_for_current_role_wrapper';

CREATE FUNCTION graph._active_generation_count_for_current_role()
RETURNS integer
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'active_generation_count_for_current_role_wrapper';

CREATE FUNCTION graph._enforce_loaded_graph_quota_for_current_role(
    projected_loaded_graphs bigint
) RETURNS boolean
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'enforce_loaded_graph_quota_for_current_role_wrapper';

GRANT EXECUTE ON FUNCTION
    graph._require_selected_graph_privilege_for_current_role(text)
TO PUBLIC;
GRANT EXECUTE ON FUNCTION
    graph._graph_id_for_current_role_with_privilege(text, text, text, text)
TO PUBLIC;
GRANT EXECUTE ON FUNCTION
    graph._expire_projection_heartbeats_for_current_role()
TO PUBLIC;
GRANT EXECUTE ON FUNCTION
    graph._expire_sync_watermarks_for_current_role()
TO PUBLIC;
GRANT EXECUTE ON FUNCTION
    graph._record_sync_watermark_for_current_role()
TO PUBLIC;
GRANT EXECUTE ON FUNCTION
    graph._record_projection_heartbeat_for_current_role()
TO PUBLIC;
GRANT EXECUTE ON FUNCTION
    graph._active_generation_count_for_current_role()
TO PUBLIC;
GRANT EXECUTE ON FUNCTION
    graph._enforce_loaded_graph_quota_for_current_role(bigint)
TO PUBLIC;
