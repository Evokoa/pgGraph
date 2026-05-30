#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_boundary}"
ROLE_NAME="${ROLE_NAME:-${DBNAME}_restricted}"
GQL_SQLSTATE_REQUIRED="${GQL_SQLSTATE_REQUIRED:-0}"

run_sql() {
  local sql="$1"
  psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c "$sql" >/dev/null
}

expect_sqlstate() {
  local code="$1"
  local sql="$2"
  local out

  set +e
  out="$(psql -X -v ON_ERROR_STOP=1 -v VERBOSITY=verbose -d "$DBNAME" -c "$sql" 2>&1)"
  local rc=$?
  set -e

  if [[ $rc -eq 0 ]]; then
    echo "Expected SQLSTATE $code but statement succeeded:"
    echo "$sql"
    exit 1
  fi

  if ! grep -Eq "ERROR:[[:space:]]+$code:" <<<"$out"; then
    echo "Expected SQLSTATE $code but got different error output:"
    echo "$out"
    exit 1
  fi
}

expect_sqlstate_as_role() {
  local role="$1"
  local code="$2"
  local sql="$3"
  local out

  set +e
  out="$(psql -X -v ON_ERROR_STOP=1 -v VERBOSITY=verbose -d "$DBNAME" <<SQL 2>&1
SET ROLE $role;
$sql
SQL
)"
  local rc=$?
  set -e

  if [[ $rc -eq 0 ]]; then
    echo "Expected SQLSTATE $code for role $role but statement succeeded:"
    echo "$sql"
    exit 1
  fi

  if ! grep -Eq "ERROR:[[:space:]]+$code:" <<<"$out"; then
    echo "Expected SQLSTATE $code for role $role but got different error output:"
    echo "$out"
    exit 1
  fi
}

expect_value_as_role() {
  local role="$1"
  local expected="$2"
  local sql="$3"
  local out

  out="$(psql -X -q -v ON_ERROR_STOP=1 -tA -d "$DBNAME" <<SQL
SET ROLE $role;
$sql
SQL
)"

  if [[ "$out" != "$expected" ]]; then
    echo "Expected value '$expected' for role $role but got:"
    echo "$out"
    exit 1
  fi
}

has_gql_facade() {
  local out

  out="$(psql -X -q -v ON_ERROR_STOP=1 -tA -d "$DBNAME" -c "SELECT to_regprocedure('graph.gql(text,jsonb,boolean)') IS NOT NULL;")"
  [[ "$out" == "t" ]]
}

dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

run_sql "CREATE EXTENSION IF NOT EXISTS graph;"
run_sql "SELECT graph.reset();"
run_sql "SET graph.auto_load = off;"

run_sql "DROP TABLE IF EXISTS public.graph_boundary_edges CASCADE;"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_nodes CASCADE;"
run_sql "CREATE TABLE public.graph_boundary_nodes (id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, name TEXT NOT NULL, age INT NOT NULL, friend_id TEXT REFERENCES public.graph_boundary_nodes(id));"
run_sql "CREATE TABLE public.graph_boundary_edges (id BIGSERIAL PRIMARY KEY, from_id TEXT NOT NULL REFERENCES public.graph_boundary_nodes(id), to_id TEXT NOT NULL REFERENCES public.graph_boundary_nodes(id));"
run_sql "INSERT INTO public.graph_boundary_nodes VALUES ('b', 't2', 'Bob', 20, NULL), ('a', 't1', 'Alice', 10, 'b');"
run_sql "INSERT INTO public.graph_boundary_edges (from_id, to_id) VALUES ('a', 'b');"

expect_sqlstate "PG003" "SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1);"

run_sql "SELECT graph.add_table('public.graph_boundary_nodes'::regclass, 'id', ARRAY['tenant_id', 'name', 'age']);"
run_sql "SELECT graph.add_edge('public.graph_boundary_nodes'::regclass, 'friend_id', 'public.graph_boundary_nodes'::regclass, 'id', 'boundary', bidirectional := false);"
run_sql "SELECT graph.add_filter_column('public.graph_boundary_nodes'::regclass, 'age');"
run_sql "SELECT * FROM graph.build();"

expect_sqlstate "PG010" "SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'missing', 1);"
expect_sqlstate "PG005" "SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, NULL, '🔥 > 1');"

if has_gql_facade; then
  expect_sqlstate "PG013" "SELECT * FROM graph.gql('MATCH (');"
  expect_sqlstate "PG014" "SELECT * FROM graph.gql('MATCH (u:graph_boundary_nodes)-[:boundary*]->(v:graph_boundary_nodes) RETURN u');"
  expect_sqlstate "PG015" "SELECT * FROM graph.gql('MATCH (u:no_such_label)-[:boundary]->(v:graph_boundary_nodes) RETURN u');"
  expect_sqlstate "PG016" "SELECT * FROM graph.gql('MATCH (u:graph_boundary_nodes {name: \$name})-[:boundary]->(v:graph_boundary_nodes) RETURN u', '[\"Alice\"]'::jsonb);"
  expect_sqlstate "PG017" "SELECT * FROM graph.gql('MATCH (u:graph_boundary_nodes)-[:boundary]->(v:graph_boundary_nodes) WHERE u.age > ''old'' RETURN u');"
elif [[ "$GQL_SQLSTATE_REQUIRED" == "1" ]]; then
  echo "GQL_SQLSTATE_REQUIRED=1 but graph.gql(text,jsonb,boolean) is not installed"
  exit 1
fi

expect_sqlstate "55000" "SET graph.enabled = off; SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1);"

run_sql "DROP ROLE IF EXISTS $ROLE_NAME;"
run_sql "CREATE ROLE $ROLE_NAME;"
run_sql "GRANT USAGE ON SCHEMA graph TO $ROLE_NAME;"
run_sql "GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA graph TO $ROLE_NAME;"
run_sql "GRANT SELECT ON public.graph_boundary_nodes TO $ROLE_NAME;"
run_sql "ALTER TABLE public.graph_boundary_nodes ENABLE ROW LEVEL SECURITY;"
run_sql "CREATE POLICY graph_boundary_tenant_rls ON public.graph_boundary_nodes FOR SELECT TO $ROLE_NAME USING (tenant_id = current_setting('graph.boundary_tenant', true));"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_traversal_coords;"
run_sql "CREATE TABLE public.graph_boundary_traversal_coords AS SELECT node_table, node_id, depth FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, hydrate := false);"
run_sql "GRANT SELECT ON public.graph_boundary_traversal_coords TO $ROLE_NAME;"

expect_value_as_role "$ROLE_NAME" "2" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM public.graph_boundary_traversal_coords;"
expect_value_as_role "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM public.graph_boundary_traversal_coords g JOIN public.graph_boundary_nodes n ON n.id = g.node_id;"

expect_sqlstate_as_role "$ROLE_NAME" "42501" "INSERT INTO graph._registered_tables (table_name, id_column) VALUES ('public.nope', 'id');"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT graph.add_table('public.graph_boundary_nodes'::regclass, 'id');"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT * FROM graph.build();"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT * FROM graph.vacuum();"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT * FROM graph.maintenance();"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT graph.reset();"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT graph.enable_sync();"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT * FROM graph.apply_sync();"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT * FROM graph.connected_components();"
expect_sqlstate_as_role "$ROLE_NAME" "PG002" "SELECT * FROM graph.component_stats();"

echo "SQLSTATE/ACL boundary checks passed on database: $DBNAME"
