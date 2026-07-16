"""Shared pgGraph playground query catalog."""

from __future__ import annotations

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
    "GQL Parameterized Match": "How does GQL use parameters to project a known Panama relationship?",
    "GQL Scalar Projection": "Which scalar fields can GQL return from matched Panama nodes?",
    "GQL One-Hop Relationships": "Which nodes does a GQL pattern match directly from this Panama officer?",
    "GQL Relationship Projection": "What relationship value does GQL return for a known Panama edge?",
    "GQL Inbound Relationships": "Which inbound relationships point at this Panama intermediary?",
    "GQL Undirected Relationships": "What neighbors appear when direction does not matter?",
    "GQL Distinct Labels": "Which distinct target labels appear in a GQL relationship match?",
    "GQL Aggregated Neighbors": "How does GQL group this officer's direct neighbors by source label?",
    "GQL Aggregate By Label": "How many direct neighbors does this officer have by target label?",
    "GQL Collect Neighbor Labels": "Which neighbor labels are collected around this relationship type?",
    "GQL Variable-Length Paths": "What bounded path lengths can GQL find over a small relationship type?",
    "GQL Path Functions": "What do nodes(path), relationships(path), and length(path) return?",
    "GQL Hydration Off": "What compact node identifiers does GQL return when hydration is disabled?",
    "GQL Explain": "What physical plan does pgGraph choose for a one-hop GQL pattern?",
    "Mutable GQL Merge Node": "How does mutable GQL merge a playground node into the Panama source table?",
    "Mutable GQL Merge Update": "How does mutable GQL update an existing mapped node through MERGE?",
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
            "Component Stats": """SELECT * FROM graph.component_stats();

SELECT * FROM graph.components(max_rows := 20);""",
            "Largest Component": """SELECT component_id, node_id, node_table::regclass AS node_table, node
FROM graph.largest_component(max_rows := 20, hydrate := false);""",
        },
    ),
    (
        "GQL Examples",
        {
            "GQL Parameterized Match": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   WHERE source.node_id = $seed
     AND target.node_id = $target
   RETURN source.node_id AS source_id,
          target.node_id AS target_id',
  params := '{"seed":"23000018","target":"11012822"}'::jsonb
);""",
            "GQL Scalar Projection": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   RETURN source.node_id AS source_id,
          source.name AS source_name,
          target.node_id AS target_id,
          target.name AS target_name,
          target.country_codes AS target_country_codes
   ORDER BY source_id, target_id
   LIMIT 4',
  params := '{}'::jsonb
);""",
            "GQL One-Hop Relationships": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   WHERE source.node_id = $seed
     AND target.node_id = $target
   RETURN source.node_id AS source_id,
          target.node_id AS target_id,
          target.name AS target_name,
          target.label AS target_label
   ORDER BY target_id
   LIMIT 1',
  params := '{"seed":"23000018","target":"11012822"}'::jsonb
);""",
            "GQL Relationship Projection": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[rel:same_intermediary_as]->(target:nodes)
   WHERE source.node_id = $seed
     AND target.node_id = $target
   RETURN source.node_id AS source_id,
          rel AS relationship,
          target.node_id AS target_id
   LIMIT 1',
  params := '{"seed":"23000018","target":"11012822"}'::jsonb
);""",
            "GQL Inbound Relationships": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)<-[:same_intermediary_as]-(target:nodes)
   WHERE source.node_id = $target
   RETURN target.node_id AS source_id,
          source.node_id AS target_id,
          source.name AS target_name
   ORDER BY source_id
   LIMIT 10',
  params := '{"target":"11012822"}'::jsonb
);""",
            "GQL Undirected Relationships": """SELECT row
FROM graph.gql(
  'MATCH (seed:nodes)-[:same_intermediary_as]-(neighbor:nodes)
   WHERE seed.node_id = $seed
   RETURN neighbor.node_id AS neighbor_id,
          neighbor.name AS neighbor_name,
          neighbor.label AS neighbor_label
   ORDER BY neighbor_id',
  params := '{"seed":"23000018"}'::jsonb
);""",
            "GQL Distinct Labels": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   RETURN DISTINCT target.label AS target_label
   ORDER BY target_label',
  params := '{}'::jsonb
);""",
            "GQL Aggregated Neighbors": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   RETURN count(*) AS direct_links',
  params := '{}'::jsonb
);""",
            "GQL Aggregate By Label": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   WHERE source.node_id = $seed
   RETURN target.label AS target_label,
          count(*) AS links
   ORDER BY links DESC, target_label',
  params := '{"seed":"23000018"}'::jsonb
);""",
            "GQL Collect Neighbor Labels": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   RETURN source.label AS source_label,
          collect(DISTINCT target.label) AS target_labels,
          count(*) AS links',
  params := '{}'::jsonb
);""",
            "GQL Variable-Length Paths": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[path:same_intermediary_as*1..1]->(target:nodes)
   RETURN source.node_id AS source_id,
          target.node_id AS target_id,
          length(path) AS hops
   ORDER BY source_id, target_id
   LIMIT 4',
  params := '{}'::jsonb
);""",
            "GQL Path Functions": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[path:same_intermediary_as*1..1]->(target:nodes)
   RETURN length(path) AS hops,
          nodes(path) AS path_nodes,
          relationships(path) AS path_relationships
   ORDER BY source.node_id, target.node_id
   LIMIT 3',
  params := '{}'::jsonb
);""",
            "GQL Hydration Off": """SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   WHERE source.node_id = $seed
   RETURN source,
          target
   ORDER BY target.node_id
   LIMIT 2',
  params := '{"seed":"23000018"}'::jsonb,
  hydrate := false
);""",
            "GQL Explain": """SELECT graph.gql_explain(
  'MATCH (source:nodes)-[:same_intermediary_as]->(target:nodes)
   WHERE source.node_id = $seed
   RETURN source.node_id AS source_id, target.node_id AS target_id
   ORDER BY target_id
   LIMIT 20'
);""",
        },
    ),
    (
        "Mutable GQL Writes",
        {
            "Mutable GQL Merge Node": """SELECT row
FROM graph.gql(
  'MERGE (n:nodes {node_id: $id, label: ''others'', name: $name})
   ON CREATE SET n.countries = $countries
   ON MATCH SET n.name = $name
   RETURN n.node_id AS id,
          n.name AS name,
          n.label AS label,
          n.countries AS countries',
  params := '{"id":"pggraph-playground-merge","name":"pgGraph playground merge","countries":"Playground"}'::jsonb
);""",
            "Mutable GQL Merge Update": """SELECT row
FROM graph.gql(
  'MERGE (n:nodes {node_id: $id, label: ''others'', name: $name})
   ON CREATE SET n.countries = $created_country
   ON MATCH SET n.countries = $matched_country
   RETURN n.node_id AS id,
          n.name AS name,
          n.label AS label,
          n.countries AS countries',
  params := '{"id":"pggraph-playground-merge","name":"pgGraph playground merge","created_country":"Playground","matched_country":"Mutable Playground"}'::jsonb
);""",
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
ORDER BY depth, node_table_name, node_id, readable_path;""",
        },
    ),
    (
        "Admin",
        {
            "Build Graph": "SELECT * FROM graph.build('csr_readonly');",
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


def normalize_playground_mode(mode: str | None = None) -> str:
    raw = (mode or "csr").strip().lower()
    if raw in {"csr", "csr_readonly"}:
        return "csr"
    if raw in {"mutable", "mutable_overlay"}:
        return "mutable"
    raise ValueError(f"unsupported playground mode: {mode}")


def query_sections(mode: str | None = None) -> list[tuple[str, dict[str, str]]]:
    normalized = normalize_playground_mode(mode)
    sections: list[tuple[str, dict[str, str]]] = []
    for section, queries in QUERY_SECTIONS:
        if normalized == "csr" and section == "Mutable GQL Writes":
            continue
        section_queries = dict(queries)
        if section == "Admin" and normalized == "mutable":
            section_queries["Build Graph"] = "SELECT * FROM graph.build('mutable_overlay');"
            section_queries.pop("Build Graph Concurrently", None)
        sections.append((section, section_queries))
    return sections


def query_catalog(mode: str | None = None) -> dict[str, str]:
    """Return all sidebar query labels mapped to their SQL text."""
    return {label: sql for _, queries in query_sections(mode) for label, sql in queries.items()}
