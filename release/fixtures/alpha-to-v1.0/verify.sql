\set ON_ERROR_STOP on

SELECT * FROM graph.load_graph('default');

DO $$
DECLARE
    source_count bigint;
    graph_count bigint;
    graph_edge_count bigint;
    source_checksum text;
BEGIN
    SELECT count(*),
           md5(string_agg(id || '|' || name || '|' || coalesce(manager_id, ''), E'\n' ORDER BY id))
      INTO source_count, source_checksum
      FROM release_fixture_people;
    IF source_count <> 3 THEN
        RAISE EXCEPTION 'expected 3 authoritative source rows, got %', source_count;
    END IF;
    IF source_checksum <> '7597c464628b5c2e0674ea849bd8337a' THEN
        RAISE EXCEPTION 'unexpected authoritative source checksum %', source_checksum;
    END IF;

    SELECT node_count, edge_count INTO graph_count, graph_edge_count
      FROM graph.status();
    IF graph_count <> 3 OR graph_edge_count <> 2 THEN
        RAISE EXCEPTION 'expected graph status 3 nodes/2 edges, got % nodes/% edges',
            graph_count, graph_edge_count;
    END IF;

    SELECT count(*) INTO graph_count
    FROM graph.traverse(
        'release_fixture_people'::regclass,
        'alice',
        1,
        hydrate := false
    );
    IF graph_count <> 2 THEN
        RAISE EXCEPTION 'expected 2 traversal rows after rebuild, got %', graph_count;
    END IF;
END
$$;
