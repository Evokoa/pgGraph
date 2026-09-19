-- Publication authority is transactional and derived from source tables.
-- Existing artifacts require graph.build() before they can be served.
-- Do not dump publication heads: restored databases must rebuild their artifacts.
CREATE TABLE graph._projection_heads (
    graph_id UUID NOT NULL REFERENCES graph._graphs(graph_id) ON DELETE CASCADE,
    artifact_root TEXT NOT NULL,
    generation_id BIGINT NOT NULL CHECK (generation_id > 0),
    manifest_checksum TEXT NOT NULL,
    PRIMARY KEY (graph_id, artifact_root)
);
REVOKE ALL ON TABLE graph._projection_heads FROM PUBLIC;

CREATE FUNCTION graph._published_generation_for_current_role()
RETURNS jsonb STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'published_generation_for_current_role_wrapper';

CREATE FUNCTION graph._publish_generation_for_current_role()
RETURNS boolean STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'publish_generation_for_current_role_wrapper';

GRANT EXECUTE ON FUNCTION graph._published_generation_for_current_role() TO PUBLIC;
GRANT EXECUTE ON FUNCTION graph._publish_generation_for_current_role() TO PUBLIC;

CREATE FUNCTION graph._sync_retention_catalog_for_current_role()
RETURNS TABLE (
    heartbeat_floor bigint,
    active_backends integer,
    has_sources boolean,
    shared_source boolean,
    alternate_artifact_root boolean
)
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'sync_retention_catalog_for_current_role_wrapper';

GRANT EXECUTE ON FUNCTION graph._sync_retention_catalog_for_current_role() TO PUBLIC;

CREATE FUNCTION graph.sync_retention()
RETURNS TABLE (
    eligible_prune_floor bigint,
    prune_blocker text,
    retained_graph_rows bigint,
    database_sync_log_bytes bigint,
    active_sync_watermark_backends integer
)
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'sync_retention_wrapper';
