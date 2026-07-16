#!/usr/bin/env bash
# Deterministic synthetic graph release smoke.
#
# Usage:
#   cd graph
#   DBNAME=pggraph_synthetic NODE_COUNT=50000 ./tests/heavy/synthetic_release_smoke.sh
#
# Useful knobs:
#   NODE_COUNT=50000          number of source nodes to generate
#   HUB_FANOUT=1000           direct fanout edges from node 1
#   MAX_BUILD_MS=60000        fail if graph.build() reports a slower build
#   MAX_QUERY_MS=1000         fail if representative query smoke is slower
#   CREATE_DB=1               drop/create DBNAME before running
#
# This is release-gate evidence, not a public benchmark. Public benchmark
# claims should use real or standardized datasets with the benchmark harness.
set -euo pipefail

DBNAME="${DBNAME:-pggraph_synthetic}"
NODE_COUNT="${NODE_COUNT:-50000}"
HUB_FANOUT="${HUB_FANOUT:-1000}"
MAX_BUILD_MS="${MAX_BUILD_MS:-60000}"
MAX_QUERY_MS="${MAX_QUERY_MS:-1000}"
CREATE_DB="${CREATE_DB:-1}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSPECTOR="$SCRIPT_DIR/../../../scripts/inspect_pggraph_artifact.py"

for value_name in NODE_COUNT HUB_FANOUT MAX_BUILD_MS MAX_QUERY_MS; do
  value="${!value_name}"
  if [[ ! "$value" =~ ^[0-9]+$ ]]; then
    echo "$value_name must be a non-negative integer, got: $value"
    exit 2
  fi
done

if (( NODE_COUNT < 100 )); then
  echo "NODE_COUNT must be at least 100 so traversal/search/path checks are meaningful"
  exit 2
fi

if (( HUB_FANOUT < 2 )); then
  echo "HUB_FANOUT must be at least 2"
  exit 2
fi

if (( HUB_FANOUT > NODE_COUNT )); then
  HUB_FANOUT="$NODE_COUNT"
fi

if [[ "$CREATE_DB" == "1" && "$DBNAME" != "postgres" ]]; then
  dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
  createdb "$DBNAME"
fi

echo "[synthetic] loading NODE_COUNT=$NODE_COUNT HUB_FANOUT=$HUB_FANOUT into $DBNAME"

build_result="$(
  psql -X -q -v ON_ERROR_STOP=1 -tA "$DBNAME" <<SQL
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
DROP TABLE IF EXISTS public.graph_synth_edges CASCADE;
DROP TABLE IF EXISTS public.graph_synth_nodes CASCADE;

CREATE TABLE public.graph_synth_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    tenant TEXT NOT NULL,
    score BIGINT NOT NULL,
    active BOOLEAN NOT NULL,
    parent_id TEXT
);

INSERT INTO public.graph_synth_nodes (id, name, tenant, score, active, parent_id)
SELECT
    i::text,
    'node-' || i::text,
    'tenant-' || (i % 10)::text,
    (i % 1000)::bigint,
    (i % 7) <> 0,
    CASE WHEN i > 1 THEN (i / 2)::bigint::text ELSE NULL END
FROM generate_series(1, $NODE_COUNT) AS i;

CREATE TABLE public.graph_synth_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_synth_nodes(id),
    to_id TEXT NOT NULL REFERENCES public.graph_synth_nodes(id),
    weight INT NOT NULL
);

INSERT INTO public.graph_synth_edges (from_id, to_id, weight)
SELECT i::text, (i + 1)::text, 1
FROM generate_series(1, $((NODE_COUNT - 1))) AS i;

INSERT INTO public.graph_synth_edges (from_id, to_id, weight)
SELECT i::text, (i + 10)::text, 2
FROM generate_series(1, $((NODE_COUNT - 10))) AS i;

INSERT INTO public.graph_synth_edges (from_id, to_id, weight)
SELECT '1', i::text, 3
FROM generate_series(100, LEAST($NODE_COUNT, 99 + $HUB_FANOUT)) AS i;

SELECT graph.add_table(
    'public.graph_synth_nodes'::regclass,
    'id',
    ARRAY['name', 'tenant', 'score', 'active']
);
SELECT graph.add_edge(
    'public.graph_synth_nodes'::regclass,
    'parent_id',
    'public.graph_synth_nodes'::regclass,
    'id',
    'parent',
    bidirectional := false
);
SELECT graph.add_edge(
    'public.graph_synth_edges'::regclass,
    'from_id',
    'public.graph_synth_nodes'::regclass,
    'to_id',
    'synthetic',
    bidirectional := false,
    weight_column := 'weight'
);
SELECT graph.add_filter_column('public.graph_synth_nodes'::regclass, 'score', column_type := 'numeric');
SELECT graph.add_filter_column('public.graph_synth_nodes'::regclass, 'tenant', column_type := 'text');
SET graph.persist_on_build = on;
SELECT nodes_loaded::text || '|' || edges_loaded::text || '|' || ceil(build_time_ms)::bigint::text
FROM graph.build();
SQL
)"
build_result="$(printf '%s\n' "$build_result" | awk 'NF { row = $0 } END { print row }')"

IFS='|' read -r nodes_loaded edges_loaded build_ms <<<"$build_result"

if (( nodes_loaded != NODE_COUNT )); then
  echo "Expected $NODE_COUNT nodes_loaded, got $nodes_loaded"
  exit 1
fi

if (( edges_loaded < NODE_COUNT - 1 )); then
  echo "Expected at least $((NODE_COUNT - 1)) edges_loaded, got $edges_loaded"
  exit 1
fi

if (( build_ms > MAX_BUILD_MS )); then
  echo "Synthetic build exceeded threshold: ${build_ms}ms > ${MAX_BUILD_MS}ms"
  exit 1
fi

data_directory="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT current_setting('data_directory')")"
graph_data_dir="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT COALESCE(NULLIF(current_setting('graph.data_dir', true), ''), 'graph')")"
graph_id="$(psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" -c "SELECT graph_id FROM graph.current_graph()")"
if [[ "$graph_data_dir" = /* ]]; then
  graph_logical_path="$graph_data_dir/$graph_id/main.pggraph"
else
  graph_logical_path="$data_directory/$graph_data_dir/$graph_id/main.pggraph"
fi
artifact_path="$(python3 "$INSPECTOR" --resolve-only "$graph_logical_path")"

target_id="$(( NODE_COUNT < 250 ? NODE_COUNT : 250 ))"

query_result="$(
  psql -X -q -v ON_ERROR_STOP=1 -tA "$DBNAME" <<SQL
SET graph.auto_load = on;
WITH started AS (SELECT clock_timestamp() AS ts),
     status AS (
       SELECT node_count, edge_count
       FROM graph.status()
     ),
     traversed AS (
       SELECT count(*) AS rows_seen
       FROM graph.traverse(
           'public.graph_synth_nodes'::regclass,
           '1',
           3,
           direction := 'out',
           hydrate := false,
           max_rows := 10000
       )
     ),
     searched AS (
       SELECT count(*) AS rows_seen
       FROM graph.search(
           'name',
           'node-$target_id',
           table_filter := 'public.graph_synth_nodes'::regclass,
           mode := 'exact',
           max_rows := 10,
           hydrate := false
       )
     ),
     filtered AS (
       SELECT count(*) AS rows_seen
       FROM graph.traverse(
           'public.graph_synth_nodes'::regclass,
           '1',
           2,
           direction := 'out',
           filter := graph.gte('score', 500::bigint),
           hydrate := false,
           max_rows := 10000
       )
     ),
     path AS (
       SELECT count(*) AS rows_seen
       FROM graph.shortest_path(
           'public.graph_synth_nodes'::regclass,
           '1',
           'public.graph_synth_nodes'::regclass,
           '$target_id',
           20
       )
     ),
     weighted AS (
       SELECT count(*) AS rows_seen
       FROM graph.weighted_shortest_path(
           'public.graph_synth_nodes'::regclass,
           '1',
           'public.graph_synth_nodes'::regclass,
           '$target_id'
       )
     ),
     components AS (
       SELECT num_components, largest_component
       FROM graph.component_stats()
     ),
     finished AS (SELECT clock_timestamp() AS ts)
SELECT ((EXTRACT(EPOCH FROM (finished.ts - started.ts)) * 1000)::bigint)::text
       || '|' || status.node_count::text
       || '|' || status.edge_count::text
       || '|' || traversed.rows_seen::text
       || '|' || searched.rows_seen::text
       || '|' || filtered.rows_seen::text
       || '|' || path.rows_seen::text
       || '|' || weighted.rows_seen::text
       || '|' || components.num_components::text
       || '|' || components.largest_component::text
FROM started, status, traversed, searched, filtered, path, weighted, components, finished;
SQL
)"
query_result="$(printf '%s\n' "$query_result" | awk 'NF { row = $0 } END { print row }')"

IFS='|' read -r query_ms status_nodes status_edges traversal_rows search_rows filtered_rows path_rows weighted_rows component_count largest_component <<<"$query_result"

if (( status_nodes != NODE_COUNT )); then
  echo "Expected status node_count=$NODE_COUNT, got $status_nodes"
  exit 1
fi
if (( status_edges < NODE_COUNT - 1 )); then
  echo "Expected status edge_count >= $((NODE_COUNT - 1)), got $status_edges"
  exit 1
fi
if (( traversal_rows < 2 )); then
  echo "Expected traversal to return multiple rows, got $traversal_rows"
  exit 1
fi
if (( search_rows < 1 )); then
  echo "Expected exact search to find node-$target_id"
  exit 1
fi
if (( filtered_rows < 1 )); then
  echo "Expected filtered traversal to return at least one row"
  exit 1
fi
if (( path_rows < 1 )); then
  echo "Expected shortest_path to find node $target_id"
  exit 1
fi
if (( weighted_rows < 1 )); then
  echo "Expected weighted_shortest_path to find node $target_id"
  exit 1
fi
if (( component_count < 1 || largest_component < NODE_COUNT / 2 )); then
  echo "Unexpected component shape: components=$component_count largest=$largest_component"
  exit 1
fi
if (( query_ms > MAX_QUERY_MS )); then
  echo "Synthetic query smoke exceeded threshold: ${query_ms}ms > ${MAX_QUERY_MS}ms"
  exit 1
fi

echo "Synthetic release smoke passed: nodes=$nodes_loaded edges=$edges_loaded build_ms=$build_ms query_ms=$query_ms artifact=$artifact_path"
