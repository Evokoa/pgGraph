"""Shared pgGraph playground query catalog."""

DEFAULT_SQL = """-- Core pgGraph status and catalog checks.
SELECT * FROM graph.status();

SELECT * FROM graph.registered_tables();
SELECT * FROM graph.registered_edges();
"""

DEFAULT_QUESTION = "What is the current graph state, and what can I ask next?"
PLAYGROUND_CONTEXT = "Run SQL directly against the Docker-backed PostgreSQL 17 instance with pgGraph installed."

QUERY_QUESTIONS = {
    "Status + Catalog": DEFAULT_QUESTION,
    "Search Mossack": "Which Panama nodes mention Mossack in a registered searchable field?",
    "Find Mossack": "Which Mossack matches does graph.find rank highest?",
    "Traverse Neighborhood": "What is within two hops of this Panama officer node?",
    "Expand Neighborhood": "What readable neighborhood does graph.expand produce from this seed?",
    "Shortest Path": "What path connects this Panama officer seed to the selected target?",
    "GQL One-Hop Relationships": "Which nodes does a GQL pattern match directly from this Panama officer?",
    "GQL Aggregated Neighbors": "How does GQL group this officer's direct neighbors by source label?",
    "GQL Explain": "What physical plan does pgGraph choose for a one-hop GQL pattern?",
    "Component Stats": "How many connected components exist, and what are the first components?",
    "Largest Component": "Which nodes appear in the largest connected component?",
    "Table Sizes": "How large are the loaded Panama node and relationship tables?",
    "Relationship Label Counts": "Which Panama relationship labels are most common?",
    "Top Connected Officers": "Which officers have the most direct Panama relationships?",
    "Top Connected Entities": "Which entities have the most direct Panama relationships?",
    "Entity Direct Relationships": "Who or what is directly connected to this Panama entity?",
    "Officer Context Packet": "What compact investigation packet can I build around this officer?",
    "Search Entity Then Expand": "What neighborhoods appear when I search for Mossack and expand each match?",
    "Relationship Filtered Walk": "What does the neighborhood look like when traversal is limited to intermediary relationships?",
    "Capped 3-Hop Investigation": "What can I inspect within a capped three-hop investigation from this officer?",
    "Build Graph": "What happens when pgGraph builds the registered graph now?",
    "Build Graph Concurrently": "Can pgGraph start a concurrent build for the registered graph?",
    "Build Status": "What does pgGraph report for a build status lookup?",
    "Sync Health": "What does pgGraph recommend for sync and maintenance right now?",
    "Apply Sync": "What pending sync changes can pgGraph apply?",
    "Scheduled Maintenance": "What would the scheduler-safe maintenance entry point do now?",
    "Vacuum Graph": "What graph storage cleanup does pgGraph perform?",
    "Maintenance": "What maintenance work does pgGraph run synchronously?",
    "Maintenance Status": "What maintenance status does pgGraph report?",
}

QUERY_SECTIONS = [
    (
        "Core Functions",
        {
            "Status + Catalog": DEFAULT_SQL,
            "Search Mossack": """SELECT node_table_name, node_id, match_type, score, verified, node
FROM graph.search(
  'name',
  'Mossack',
  table_filter := 'panama.nodes'::regclass,
  mode := 'contains',
  max_rows := 20,
  hydrate := true
)
ORDER BY score DESC, node_id;""",
            "Find Mossack": """SELECT node_table_name, node_id, match_type, score, verified, rank, node
FROM graph.find(
  'name',
  'Mossack',
  'panama.nodes'::regclass,
  max_rows := 20
)
ORDER BY rank;""",
            "Traverse Neighborhood": """SELECT depth, node_table_name, node_id, edge_path, node
FROM graph.traverse(
  'panama.nodes'::regclass,
  '54662',
  2,
  hydrate := true,
  max_rows := 100
)
ORDER BY depth, node_table_name, node_id;""",
            "Expand Neighborhood": """SELECT depth, node_table_name, node_id, rank, readable_path, node, truncated
FROM graph.expand(
  'panama.nodes'::regclass,
  '54662',
  2,
  max_rows := 100
)
ORDER BY depth, rank;""",
            "Shortest Path": """SELECT step, node_table_name, node_id, edge_label, node
FROM graph.shortest_path(
  'panama.nodes'::regclass,
  '54662',
  'panama.nodes'::regclass,
  '147079',
  4,
  hydrate := true
)
ORDER BY step;""",
            "GQL One-Hop Relationships": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:related_to]->(target:nodes)
   WHERE source.node_id = $seed
     AND target.node_id = $target
   RETURN source.node_id AS source_id,
          target.node_id AS target_id,
          target.name AS target_name,
          target.label AS target_label
   ORDER BY target_id
   LIMIT 1',
  params := '{"seed":"54662","target":"147079"}'::jsonb
);""",
            "GQL Aggregated Neighbors": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:related_to]->(target:nodes)
   WHERE source.node_id = $seed
   RETURN count(*) AS direct_links',
  params := '{"seed":"54662"}'::jsonb
);""",
            "GQL Explain": """SELECT graph.gql_explain(
  'MATCH (source:nodes)-[:related_to]->(target:nodes)
   WHERE source.node_id = $seed
   RETURN source.node_id AS source_id, target.node_id AS target_id
   ORDER BY target_id
   LIMIT 20'
);""",
            "Component Stats": """SELECT * FROM graph.component_stats();

SELECT * FROM graph.components(max_rows := 20);""",
            "Largest Component": """SELECT component_id, node_id, node_table::regclass AS node_table, node
FROM graph.largest_component(max_rows := 20, hydrate := false);""",
        },
    ),
    (
        "Sample Workflows",
        {
            "Table Sizes": """SELECT label AS node_type, count(*)::bigint AS rows
FROM panama.nodes
GROUP BY label
UNION ALL
SELECT 'relationships' AS node_type, count(*)::bigint AS rows
FROM panama.edges
ORDER BY rows DESC;""",
            "Relationship Label Counts": """SELECT rel_type AS relationship_label, count(*)::bigint AS rows
FROM panama.edges
GROUP BY rel_type
ORDER BY rows DESC, relationship_label
LIMIT 20;""",
            "Top Connected Officers": """SELECT n.node_id, n.name, n.countries, count(*)::bigint AS total_links
FROM panama.nodes n
JOIN panama.edges e ON e.start_id = n.node_id OR e.end_id = n.node_id
WHERE n.label = 'officers'
GROUP BY n.node_id, n.name, n.countries
ORDER BY total_links DESC, n.name
LIMIT 25;""",
            "Top Connected Entities": """SELECT n.node_id, n.name, n.countries, count(*)::bigint AS total_links
FROM panama.nodes n
JOIN panama.edges e ON e.start_id = n.node_id OR e.end_id = n.node_id
WHERE n.label = 'entities'
GROUP BY n.node_id, n.name, n.countries
ORDER BY total_links DESC, n.name
LIMIT 25;""",
            "Entity Direct Relationships": """SELECT e.rel_type, e.start_id, e.end_id,
       CASE WHEN e.start_id = '10000266' THEN e.end_id ELSE e.start_id END AS other_node_id,
       other.label AS other_type,
       other.name AS other_name,
       other.countries AS other_countries
FROM panama.edges e
JOIN panama.nodes other
  ON other.node_id = CASE WHEN e.start_id = '10000266' THEN e.end_id ELSE e.start_id END
WHERE e.start_id = '10000266' OR e.end_id = '10000266'
ORDER BY e.rel_type, other_name NULLS LAST
LIMIT 100;""",
            "Officer Context Packet": """WITH seed AS (
  SELECT node_id, label, name, countries
  FROM panama.nodes
  WHERE node_id = '54662'
), walk AS (
  SELECT depth, node_table_name, node_id, edge_path, graph.format_path(path, edge_path) AS readable_path, node
  FROM graph.traverse(
    'panama.nodes'::regclass,
    '54662',
    2,
    hydrate := true,
    max_rows := 250
  )
), by_type AS (
  SELECT node->>'label' AS node_type, count(*) AS node_count
  FROM walk
  GROUP BY node->>'label'
)
SELECT '54662' AS seed_id,
       (SELECT name FROM seed) AS seed_label,
       jsonb_build_object(
         'seed', (SELECT to_jsonb(seed) FROM seed),
         'counts_by_type', coalesce((SELECT jsonb_object_agg(node_type, node_count) FROM by_type), '{}'::jsonb),
         'nearby_nodes', coalesce((
           SELECT jsonb_agg(jsonb_build_object('depth', depth, 'id', node_id, 'path', readable_path, 'node', node) ORDER BY depth, node_id)
           FROM (SELECT * FROM walk ORDER BY depth, node_id LIMIT 50) s
         ), '[]'::jsonb)
       ) AS packet;""",
            "Search Entity Then Expand": """WITH seed AS (
  SELECT node_table, node_id
  FROM graph.search(
    'name',
    'Mossack',
    table_filter := 'panama.nodes'::regclass,
    mode := 'contains',
    max_rows := 3,
    hydrate := false
  )
)
SELECT t.root_id, t.depth, t.node_table_name, t.node_id, graph.format_path(t.path, t.edge_path) AS readable_path, t.node
FROM seed s
CROSS JOIN LATERAL graph.traverse(
  s.node_table,
  s.node_id,
  2,
  hydrate := true,
  max_rows := 150
) t
ORDER BY t.root_id, t.depth, t.node_table_name, t.node_id;""",
            "Relationship Filtered Walk": """SELECT depth, node_table_name, node_id, edge_path, node
FROM graph.traverse(
  'panama.nodes'::regclass,
  '54662',
  2,
  edge_types := ARRAY['intermediary_of'],
  hydrate := true,
  max_rows := 100
)
ORDER BY depth, node_table_name, node_id;""",
            "Capped 3-Hop Investigation": """SELECT depth, node_table_name, node_id, graph.format_path(path, edge_path) AS readable_path, node
FROM graph.traverse(
  'panama.nodes'::regclass,
  '54662',
  3,
  hydrate := true,
  max_rows := 300,
  max_nodes := 10000,
  max_frontier := 5000
)
ORDER BY depth, node_table_name, node_id;""",
        },
    ),
    (
        "Admin",
        {
            "Build Graph": "SELECT * FROM graph.build();",
            "Build Graph Concurrently": "SELECT * FROM graph.build(concurrently := true);",
            "Build Status": "SELECT * FROM graph.build_status('00000000-0000-0000-0000-000000000000');",
            "Sync Health": "SELECT * FROM graph.sync_health();",
            "Apply Sync": "SELECT * FROM graph.apply_sync();",
            "Scheduled Maintenance": "SELECT * FROM graph.run_scheduled_maintenance();",
            "Vacuum Graph": "SELECT * FROM graph.vacuum();",
            "Maintenance": "SELECT * FROM graph.maintenance(concurrently := false);",
            "Maintenance Status": "SELECT * FROM graph.maintenance_status(NULL);",
        },
    ),
]


def query_catalog() -> dict[str, str]:
    """Return all sidebar query labels mapped to their SQL text."""
    return {label: sql for _, queries in QUERY_SECTIONS for label, sql in queries.items()}
