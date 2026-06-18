#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_named_graphs_heavy}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-named-graphs-heavy.XXXXXX")"

cleanup() {
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

if [[ -z "$PG_CONFIG" ]]; then
  if [[ -x "/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config"
  elif [[ -x "/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config"
  else
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE"
    exit 2
  fi
fi

cargo pgrx install --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE pg_test" \
  --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

run_sql() {
  local sql="$1"
  psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c "$sql" >/dev/null
}

run_sql_query() {
  local sql="$1"
  psql -X -A -t -v ON_ERROR_STOP=1 -d "$DBNAME" -c "$sql"
}

cat >"$WORKDIR/fixture.sql" <<'SQL'
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();

-- 1. Named graphs and quotas
SELECT * FROM graph.set_graph_quota('cluster', 'max_named_graphs', 10, NULL, 'hard');
SELECT * FROM graph.create_graph('heavy_named', namespace := 'app');
CREATE TABLE public.heavy_nodes (id TEXT PRIMARY KEY, name TEXT);
INSERT INTO public.heavy_nodes VALUES ('n1', 'A'), ('n2', 'B');
SELECT * FROM graph.add_table_to_graph('heavy_named', 'public.heavy_nodes'::regclass, 'id', ARRAY['name'], graph_namespace := 'app');
SELECT * FROM graph.build_graph('heavy_named', force_persist := true, graph_namespace := 'app');
SELECT * FROM graph.select_graph('heavy_named', namespace := 'app');

-- 2. Tenant and RLS registration
SET graph.enforce_tenant_scope = on;
CREATE TABLE public.tenant_nodes (id TEXT PRIMARY KEY, tenant_id TEXT, val TEXT);
INSERT INTO public.tenant_nodes VALUES ('t1', 'tenantA', 'V1');
SELECT * FROM graph.create_graph('tenant_graph', tenant := 'tenantA', namespace := 'app', graph_kind := 'tenant');
SELECT * FROM graph.add_table_to_graph(
    'tenant_graph',
    'public.tenant_nodes'::regclass,
    'id',
    ARRAY['val'],
    graph_namespace := 'app',
    graph_tenant := 'tenantA',
    tenant_column := 'tenant_id'
);
SELECT * FROM graph.build_graph('tenant_graph', force_persist := true, graph_namespace := 'app', graph_tenant := 'tenantA');
SELECT * FROM graph.select_graph('heavy_named', namespace := 'app');
SELECT * FROM graph.load_graph('heavy_named', namespace := 'app');

-- 3. Sync and jobs
SELECT graph.enable_sync();
INSERT INTO public.heavy_nodes VALUES ('n3', 'C');
SELECT * FROM graph.set_graph_residency('tenant_graph', 'warm', tenant := 'tenantA', namespace := 'app');

-- 4. Quota enforcement and execution health checks
SELECT * FROM graph.set_graph_quota('cluster', 'max_loaded_graphs_per_backend', 4, NULL, 'warn');
SELECT * FROM graph.set_graph_quota('owner', 'max_graph_jobs', 64, current_user::text, 'warn');

-- 5. Storage and runtime status checks
SELECT artifact_bytes FROM graph.projection_status();
SELECT schema_status, sync_status, sync_lag, invalid_reason FROM graph.status();
SELECT artifact_validation_state FROM graph.projection_status();

-- 6. Query and search smoke
SELECT * FROM graph.search('name', 'A');
SELECT * FROM graph.graph_map('heavy_named', graph_namespace := 'app');
SQL

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -f "$WORKDIR/fixture.sql" >/dev/null

POLICY_ROW="$(run_sql_query "SELECT policy_id::text || '|' || job_id::text FROM graph.add_sync_policy('heavy_named', schedule_interval_secs := 1, graph_namespace := 'app')")"
POLICY_ID="${POLICY_ROW%%|*}"
JOB_ID="${POLICY_ROW##*|}"
if [[ -z "$POLICY_ID" ]]; then
  echo "heavy graph gate: sync policy was not created"
  exit 3
fi

RUN_STATUS="$(run_sql_query "SELECT status FROM graph.run_job('${JOB_ID}')")"
if [[ "$RUN_STATUS" != "completed" ]]; then
  echo "heavy graph gate: run_job status was $RUN_STATUS"
  exit 4
fi

LOAD_ROW="$(run_sql_query "SELECT loaded::text || '|' || node_count::text || '|' || edge_count::text FROM graph.load_graph('heavy_named', namespace := 'app')")"
LOAD_LOADED="${LOAD_ROW%%|*}"
LOAD_REST="${LOAD_ROW#*|}"
LOAD_NODE_COUNT="${LOAD_REST%%|*}"
if [[ "$LOAD_LOADED" != "t" ]] && [[ "$LOAD_LOADED" != "true" ]]; then
  echo "heavy graph gate: load_graph did not report a loaded graph: ${LOAD_LOADED}"
  exit 5
fi
if ! [[ "$LOAD_NODE_COUNT" =~ ^[0-9]+$ ]] || [[ "$LOAD_NODE_COUNT" -le 0 ]]; then
  echo "heavy graph gate: load_graph node count not positive: ${LOAD_NODE_COUNT}"
  exit 5
fi

echo "Named graphs heavy gate passed."
