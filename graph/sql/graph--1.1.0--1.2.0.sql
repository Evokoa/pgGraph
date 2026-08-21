-- pgGraph 1.1.0 -> 1.2.0
--
-- The 1.2 catalog change is additive. Existing function objects are not
-- replaced, so their owners and explicit EXECUTE grants remain unchanged.

CREATE FUNCTION graph.edge_types(
    after_type_id bigint DEFAULT 0,
    max_rows integer DEFAULT 1000
) RETURNS TABLE (
    type_id bigint,
    label text
)
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'edge_type_page_wrapper';

CREATE FUNCTION graph.reset(
    clear_registrations boolean
) RETURNS void
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
LANGUAGE c
AS 'MODULE_PATHNAME', 'reset_with_registrations_wrapper';

GRANT EXECUTE ON FUNCTION graph.edge_types(bigint, integer) TO PUBLIC;
GRANT EXECUTE ON FUNCTION graph.reset(boolean) TO PUBLIC;
