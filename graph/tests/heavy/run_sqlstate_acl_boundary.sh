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

expect_sqlstate_as_login() {
  local role="$1"
  local code="$2"
  local sql="$3"
  local out

  set +e
  out="$(psql -X -v ON_ERROR_STOP=1 -v VERBOSITY=verbose -U "$role" -d "$DBNAME" -c "$sql" 2>&1)"
  local rc=$?
  set -e

  if [[ $rc -eq 0 ]] || ! grep -Eq "ERROR:[[:space:]]+$code:" <<<"$out"; then
    echo "Expected SQLSTATE $code for login $role:"
    echo "$out"
    exit 1
  fi
}

expect_placeholder_rls_bypass_rejected() {
  local role="$1"
  local out

  out="$(psql -X -q -tA -v VERBOSITY=verbose -U "$role" -d "$DBNAME" -c "SET graph.rls_mode = 'legacy_bypass'; SELECT count(*) FROM graph.traverse('public.graph_boundary_identity_nodes'::regclass, 'hidden', 0, hydrate := false);" 2>&1)"
  if ! grep -Eq "(WARNING|ERROR):[[:space:]]+42501:" <<<"$out"; then
    echo "Expected PostgreSQL to reject the pre-load graph.rls_mode placeholder:"
    echo "$out"
    exit 1
  fi
  if [[ "$(tail -n 1 <<<"$out" | tr -d '[:space:]')" != "0" ]]; then
    echo "Pre-load graph.rls_mode placeholder bypassed caller RLS:"
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

expect_value_as_login() {
  local role="$1"
  local expected="$2"
  local sql="$3"
  local out

  out="$(psql -X -q -v ON_ERROR_STOP=1 -tA -U "$role" -d "$DBNAME" -c "$sql")"
  if [[ "$out" != "$expected" ]]; then
    echo "Expected value '$expected' for login $role but got:"
    echo "$out"
    exit 1
  fi
}

expect_visibility_cancel_cleanup_as_login() {
  local role="$1"
  local sql="$2"
  local out

  set +e
  out="$(psql -X -q -tA -U "$role" -d "$DBNAME" <<SQL 2>&1
\set VERBOSITY verbose
SELECT graph._test_arm_visibility_scan_cancel(0);
$sql
SELECT graph._test_visibility_build_slot_empty();
SQL
)"
  local rc=$?
  set -e

  if [[ $rc -ne 0 ]]; then
    echo "Visibility cancellation cleanup session failed unexpectedly:"
    echo "$out"
    exit 1
  fi
  if ! grep -Eq "ERROR:[[:space:]]+57014:" <<<"$out"; then
    echo "Expected SQLSTATE 57014 from injected visibility cancellation:"
    echo "$out"
    exit 1
  fi
  if [[ "$(tail -n 1 <<<"$out" | tr -d '[:space:]')" != "t" ]]; then
    echo "Visibility build slot was not empty after cancellation:"
    echo "$out"
    exit 1
  fi
}

expect_missing_identity_as_login() {
  local role="$1"
  local sql="$2"
  local out

  set +e
  out="$(psql -X -q -tA -v ON_ERROR_STOP=1 -v VERBOSITY=verbose -U "$role" -d "$DBNAME" <<SQL 2>&1
SELECT graph._test_arm_missing_relationship_identity();
$sql
SQL
)"
  local rc=$?
  set -e

  if [[ $rc -eq 0 ]] || ! grep -Eq "ERROR:[[:space:]]+55000:" <<<"$out"; then
    echo "Expected rebuild-required SQLSTATE 55000 for a legacy relationship identity:"
    echo "$out"
    exit 1
  fi
  if ! grep -q "PG023" <<<"$out"; then
    echo "Expected diagnostic PG023 for a legacy relationship identity:"
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
run_sql "SET graph.auto_load = off; SET graph.persist_on_build = off;"

run_sql "DROP TABLE IF EXISTS public.graph_boundary_edges CASCADE;"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_nodes CASCADE;"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_public_nodes CASCADE;"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_secret_nodes CASCADE;"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_output_nodes CASCADE;"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_identity_nodes CASCADE;"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_sparse_nodes CASCADE;"
run_sql "CREATE TABLE public.graph_boundary_nodes (id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, name TEXT NOT NULL, age INT NOT NULL, friend_id TEXT REFERENCES public.graph_boundary_nodes(id));"
run_sql "CREATE TABLE public.graph_boundary_edges (id BIGSERIAL PRIMARY KEY, from_id TEXT NOT NULL REFERENCES public.graph_boundary_nodes(id), to_id TEXT NOT NULL REFERENCES public.graph_boundary_nodes(id), visible_to NAME NOT NULL, edge_weight INT NOT NULL);"
run_sql "CREATE TABLE public.graph_boundary_output_nodes (id TEXT PRIMARY KEY);"
run_sql "CREATE TABLE public.graph_boundary_identity_nodes (id TEXT PRIMARY KEY, visible_to NAME NOT NULL, name TEXT NOT NULL);"
run_sql "CREATE TABLE public.graph_boundary_sparse_nodes (id TEXT PRIMARY KEY, ordinal INT NOT NULL);"
run_sql "CREATE TABLE public.graph_boundary_secret_nodes (id TEXT PRIMARY KEY, output_id TEXT REFERENCES public.graph_boundary_output_nodes(id), edge_weight INT NOT NULL);"
run_sql "CREATE TABLE public.graph_boundary_public_nodes (id TEXT PRIMARY KEY, secret_id TEXT REFERENCES public.graph_boundary_secret_nodes(id), edge_weight INT NOT NULL);"
run_sql "INSERT INTO public.graph_boundary_nodes VALUES ('c', 't1', 'Carol', 30, NULL), ('b', 't2', 'Bob', 20, 'c'), ('a', 't1', 'Alice', 10, 'b'), ('d', 't1', 'Dana', 40, NULL);"
run_sql "INSERT INTO public.graph_boundary_edges (from_id, to_id, visible_to, edge_weight) VALUES ('a', 'c', 'graph_boundary_other', 1), ('a', 'd', '$ROLE_NAME', 2), ('d', 'c', '$ROLE_NAME', 2);"
run_sql "INSERT INTO public.graph_boundary_output_nodes VALUES ('o1');"
run_sql "INSERT INTO public.graph_boundary_identity_nodes VALUES ('visible', '$ROLE_NAME', 'Visible'), ('hidden', 'graph_boundary_other', 'Hidden');"
run_sql "INSERT INTO public.graph_boundary_sparse_nodes SELECT 's' || value::text, value FROM generate_series(0, 999) AS value;"
run_sql "INSERT INTO public.graph_boundary_secret_nodes VALUES ('s1', 'o1', 1);"
run_sql "INSERT INTO public.graph_boundary_public_nodes VALUES ('p1', 's1', 1);"

expect_sqlstate "55000" "SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1);"

run_sql "SELECT graph.add_table('public.graph_boundary_nodes'::regclass, 'id', ARRAY['tenant_id', 'name', 'age']);"
run_sql "SELECT graph.add_edge('public.graph_boundary_nodes'::regclass, 'friend_id', 'public.graph_boundary_nodes'::regclass, 'id', 'boundary', bidirectional := false);"
run_sql "SELECT graph.add_edge('public.graph_boundary_edges'::regclass, 'from_id', 'public.graph_boundary_nodes'::regclass, 'to_id', 'boundary_row', bidirectional := false, weight_column := 'edge_weight');"
run_sql "SELECT graph.add_filter_column('public.graph_boundary_nodes'::regclass, 'age');"
run_sql "SELECT graph.add_table('public.graph_boundary_public_nodes'::regclass, 'id');"
run_sql "SELECT graph.add_table('public.graph_boundary_secret_nodes'::regclass, 'id');"
run_sql "SELECT graph.add_table('public.graph_boundary_output_nodes'::regclass, 'id');"
run_sql "SELECT graph.add_table('public.graph_boundary_identity_nodes'::regclass, 'id', ARRAY['visible_to', 'name']);"
run_sql "SELECT graph.add_table('public.graph_boundary_sparse_nodes'::regclass, 'id', ARRAY['ordinal']);"
run_sql "SELECT graph.add_edge('public.graph_boundary_public_nodes'::regclass, 'secret_id', 'public.graph_boundary_secret_nodes'::regclass, 'id', 'hidden_link', bidirectional := false, weight_column := 'edge_weight');"
run_sql "SELECT graph.add_edge('public.graph_boundary_secret_nodes'::regclass, 'output_id', 'public.graph_boundary_output_nodes'::regclass, 'id', 'visible_link', bidirectional := false, weight_column := 'edge_weight');"
run_sql "SET graph.mutable_enabled = on; SELECT * FROM graph.build(mode := 'mutable_overlay');"
run_sql "SELECT * FROM graph.create_graph('boundary_private_graph');"

expect_sqlstate "P0002" "SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'missing', 1);"
expect_sqlstate "22023" "SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, NULL, '🔥 > 1');"

if has_gql_facade; then
  expect_sqlstate "42601" "SELECT * FROM graph.gql('MATCH (');"
  expect_sqlstate "0A000" "SELECT * FROM graph.gql('MATCH (u:graph_boundary_nodes)-[:boundary*]->(v:graph_boundary_nodes) RETURN u');"
  expect_sqlstate "22023" "SELECT * FROM graph.gql('MATCH (u:no_such_label)-[:boundary]->(v:graph_boundary_nodes) RETURN u');"
  expect_sqlstate "22023" "SELECT * FROM graph.gql('MATCH (u:graph_boundary_nodes {name: \$name})-[:boundary]->(v:graph_boundary_nodes) RETURN u', '[\"Alice\"]'::jsonb);"
  expect_sqlstate "22000" "SELECT * FROM graph.gql('MATCH (u:graph_boundary_nodes)-[:boundary]->(v:graph_boundary_nodes) WHERE u.age > ''old'' RETURN u');"
elif [[ "$GQL_SQLSTATE_REQUIRED" == "1" ]]; then
  echo "GQL_SQLSTATE_REQUIRED=1 but graph.gql(text,jsonb,boolean) is not installed"
  exit 1
fi

expect_sqlstate "55000" "SET graph.enabled = off; SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1);"

run_sql "DROP ROLE IF EXISTS $ROLE_NAME;"
run_sql "CREATE ROLE $ROLE_NAME LOGIN;"
run_sql "GRANT USAGE ON SCHEMA graph TO $ROLE_NAME;"
run_sql "GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA graph TO $ROLE_NAME;"
run_sql "GRANT SELECT ON public.graph_boundary_nodes TO $ROLE_NAME;"
run_sql "GRANT SELECT ON public.graph_boundary_edges TO $ROLE_NAME;"
run_sql "GRANT SELECT ON public.graph_boundary_public_nodes, public.graph_boundary_output_nodes TO $ROLE_NAME;"
run_sql "ALTER TABLE public.graph_boundary_identity_nodes ENABLE ROW LEVEL SECURITY;"
run_sql "CREATE POLICY graph_boundary_identity_rls ON public.graph_boundary_identity_nodes FOR SELECT TO $ROLE_NAME USING (visible_to = current_user);"
run_sql "GRANT SELECT ON public.graph_boundary_identity_nodes TO $ROLE_NAME;"
run_sql "GRANT SELECT ON public.graph_boundary_sparse_nodes TO $ROLE_NAME;"
run_sql "ALTER TABLE public.graph_boundary_sparse_nodes ENABLE ROW LEVEL SECURITY;"
run_sql "CREATE POLICY graph_boundary_sparse_rls ON public.graph_boundary_sparse_nodes FOR SELECT TO $ROLE_NAME USING (CASE current_setting('graph.boundary_policy', true) WHEN 'sparse_allow' THEN ordinal % 100 = 0 WHEN 'sparse_deny' THEN ordinal % 100 <> 0 ELSE false END);"

expect_value_as_login "$ROLE_NAME" "1" "SELECT count(*) FROM graph.traverse('public.graph_boundary_identity_nodes'::regclass, 'visible', 0, hydrate := true) WHERE node->>'name' = 'Visible';"
expect_value_as_login "$ROLE_NAME" "0" "SELECT count(*) FROM graph.traverse('public.graph_boundary_identity_nodes'::regclass, 'hidden', 0, hydrate := true);"
expect_value_as_login "$ROLE_NAME" "0" "SELECT count(*) FROM graph.traverse('public.graph_boundary_identity_nodes'::regclass, 'hidden', 0, hydrate := false);"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_policy = 'sparse_allow'; SELECT count(*) FROM graph.traverse('public.graph_boundary_sparse_nodes'::regclass, 's0', 0, hydrate := false);"
expect_value_as_login "$ROLE_NAME" "0" "SET graph.boundary_policy = 'sparse_allow'; SELECT count(*) FROM graph.traverse('public.graph_boundary_sparse_nodes'::regclass, 's1', 0, hydrate := false);"
expect_value_as_login "$ROLE_NAME" "0" "SET graph.boundary_policy = 'sparse_deny'; SELECT count(*) FROM graph.traverse('public.graph_boundary_sparse_nodes'::regclass, 's0', 0, hydrate := false);"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_policy = 'sparse_deny'; SELECT count(*) FROM graph.traverse('public.graph_boundary_sparse_nodes'::regclass, 's1', 0, hydrate := false);"

expect_value_as_login "$ROLE_NAME" "3" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 2, edge_types := ARRAY['boundary'], hydrate := false);"

run_sql "ALTER TABLE public.graph_boundary_edges ENABLE ROW LEVEL SECURITY;"
run_sql "CREATE POLICY graph_boundary_edge_rls ON public.graph_boundary_edges FOR SELECT TO $ROLE_NAME USING (visible_to = current_user OR visible_to = NULLIF(current_setting('graph.boundary_edge_override', true), '')::name);"
run_sql "CREATE POLICY graph_boundary_edge_insert_rls ON public.graph_boundary_edges FOR INSERT TO $ROLE_NAME WITH CHECK (true);"
run_sql "GRANT INSERT ON public.graph_boundary_edges TO $ROLE_NAME;"
run_sql "GRANT UPDATE ON public.graph_boundary_nodes TO $ROLE_NAME;"
run_sql "GRANT USAGE, SELECT ON SEQUENCE public.graph_boundary_edges_id_seq TO $ROLE_NAME;"
expect_value_as_login "$ROLE_NAME" "2" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
expect_value_as_login "$ROLE_NAME" "2" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'c', 1, edge_types := ARRAY['boundary_row'], direction := 'in', strategy := 'dfs', hydrate := false);"
run_sql "ALTER ROLE $ROLE_NAME BYPASSRLS;"
expect_value_as_login "$ROLE_NAME" "3" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
run_sql "ALTER ROLE $ROLE_NAME NOBYPASSRLS;"

expect_value_as_login "$ROLE_NAME" $'1\n1\n1\n2\na,c' "BEGIN;
SAVEPOINT graph_boundary_hidden_tx_edge;
SET LOCAL graph.boundary_edge_override = 'graph_boundary_other';
SELECT count(*) FROM graph.gql('MATCH (u:graph_boundary_nodes {id: ''c''}), (v:graph_boundary_nodes {id: ''d''}) CREATE (u)-[r:boundary_row {visible_to: ''graph_boundary_other'', edge_weight: 3}]->(v) RETURN r', hydrate := false);
SET LOCAL graph.boundary_edge_override = '';
SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'c', 1, edge_types := ARRAY['boundary_row'], strategy := 'dfs', hydrate := false);
ROLLBACK TO SAVEPOINT graph_boundary_hidden_tx_edge;
RELEASE SAVEPOINT graph_boundary_hidden_tx_edge;
SELECT count(*) FROM graph.gql('MATCH (u:graph_boundary_nodes {id: ''c''}), (v:graph_boundary_nodes {id: ''a''}) CREATE (u)-[r:boundary_row {visible_to: ''$ROLE_NAME'', edge_weight: 3}]->(v) RETURN r', hydrate := false);
SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], direction := 'in', strategy := 'dfs', hydrate := false);
SELECT string_agg(node_id, ',' ORDER BY depth, node_id) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], direction := 'in', strategy := 'dfs', hydrate := false);
ROLLBACK;"

if psql -X -q -tA -d "$DBNAME" -c "SELECT to_regprocedure('graph._test_arm_visibility_scan_cancel(bigint)') IS NOT NULL;" | grep -qx t; then
  expect_visibility_cancel_cleanup_as_login "$ROLE_NAME" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
  expect_value_as_login "$ROLE_NAME" "2" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
fi

if psql -X -q -tA -d "$DBNAME" -c "SELECT to_regprocedure('graph._test_arm_missing_relationship_identity()') IS NOT NULL;" | grep -qx t; then
  expect_missing_identity_as_login "$ROLE_NAME" "SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
fi

expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM public.graph_boundary_secret_nodes;"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.traverse('public.graph_boundary_public_nodes'::regclass, 'p1', 2, hydrate := false);"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.traverse('public.graph_boundary_public_nodes'::regclass, 'p1', 2, node_tables := ARRAY['public.graph_boundary_public_nodes'::regclass, 'public.graph_boundary_output_nodes'::regclass], hydrate := false);"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.shortest_path('public.graph_boundary_public_nodes'::regclass, 'p1', 'public.graph_boundary_output_nodes'::regclass, 'o1', hydrate := false);"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.weighted_shortest_path('public.graph_boundary_public_nodes'::regclass, 'p1', 'public.graph_boundary_output_nodes'::regclass, 'o1');"

run_sql "ALTER TABLE public.graph_boundary_nodes ENABLE ROW LEVEL SECURITY;"
run_sql "CREATE POLICY graph_boundary_tenant_rls ON public.graph_boundary_nodes FOR SELECT TO $ROLE_NAME USING (tenant_id = current_setting('graph.boundary_tenant', true));"
run_sql "DROP TABLE IF EXISTS public.graph_boundary_traversal_coords;"
run_sql "CREATE TABLE public.graph_boundary_traversal_coords AS SELECT node_table, node_id, depth FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, hydrate := false);"
run_sql "GRANT SELECT ON public.graph_boundary_traversal_coords TO $ROLE_NAME;"

expect_value_as_role "$ROLE_NAME" "4" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM public.graph_boundary_traversal_coords;"
expect_value_as_role "$ROLE_NAME" "3" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM public.graph_boundary_traversal_coords g JOIN public.graph_boundary_nodes n ON n.id = g.node_id;"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 2, edge_types := ARRAY['boundary'], hydrate := false);"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 2, edge_types := ARRAY['boundary'], hydrate := true);"
expect_value_as_login "$ROLE_NAME" "0" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'b', 2, edge_types := ARRAY['boundary'], hydrate := false);"
expect_value_as_login "$ROLE_NAME" "2" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse(ARRAY['public.graph_boundary_nodes'::regclass::oid, 'public.graph_boundary_nodes'::regclass::oid, 'public.graph_boundary_nodes'::regclass::oid], ARRAY['a', 'b', 'c'], max_depth := 0, strategy := 'dfs', hydrate := false);"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse_search('name', 'Alice', table_filter := 'public.graph_boundary_nodes'::regclass, max_depth := 2, edge_types := ARRAY['boundary'], strategy := 'dfs', hydrate := false);"
expect_value_as_login "$ROLE_NAME" "d" "SET graph.boundary_tenant = 't1'; SELECT string_agg(node_id, ',' ORDER BY rank) FROM graph.expand('public.graph_boundary_nodes'::regclass, 'a', max_depth := 1, edge_types := ARRAY['boundary_row'], include_start := false);"
expect_value_as_login "$ROLE_NAME" "d" "SET graph.boundary_tenant = 't1'; SELECT string_agg(node_id, ',' ORDER BY rank) FROM graph.find_related('name', 'Alice', source_table := 'public.graph_boundary_nodes'::regclass, max_depth := 1, edge_types := ARRAY['boundary_row'], include_start := false);"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT COALESCE(sum(node_count), 0) FROM graph.neighborhood('name', 'Alice', source_table := 'public.graph_boundary_nodes'::regclass, max_depth := 1, edge_types := ARRAY['boundary_row']);"
expect_sqlstate_as_login "$ROLE_NAME" "22023" "SELECT count(*) FROM graph.get_neighbors('boundary_private_graph', 'graph_boundary_nodes', 'a', hydrate := false);"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 2, edge_types := ARRAY['boundary'], strategy := 'dfs', hydrate := false);"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'c', 2, edge_types := ARRAY['boundary'], direction := 'in', strategy := 'dfs', hydrate := false);"
expect_value_as_login "$ROLE_NAME" "a,d,c" "SET graph.boundary_tenant = 't1'; SELECT string_agg(node_id, ',' ORDER BY step) FROM graph.shortest_path('public.graph_boundary_nodes'::regclass, 'a', 'public.graph_boundary_nodes'::regclass, 'c', hydrate := false);"
expect_value_as_login "$ROLE_NAME" "0" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.shortest_path('public.graph_boundary_nodes'::regclass, 'a', 'public.graph_boundary_nodes'::regclass, 'b', hydrate := false);"
expect_value_as_login "$ROLE_NAME" "0" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.shortest_path('public.graph_boundary_nodes'::regclass, 'b', 'public.graph_boundary_nodes'::regclass, 'b', hydrate := false);"
expect_value_as_login "$ROLE_NAME" "a,d,c" "SET graph.boundary_tenant = 't1'; SELECT string_agg(node_id, ',' ORDER BY step) FROM graph.weighted_shortest_path('public.graph_boundary_nodes'::regclass, 'a', 'public.graph_boundary_nodes'::regclass, 'c');"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.get_neighbors('default', 'graph_boundary_nodes', 'a', direction := 'out', hydrate := false);"

expect_sqlstate_as_role "$ROLE_NAME" "42501" "INSERT INTO graph._registered_tables (table_name, id_column) VALUES ('public.nope', 'id');"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT graph.add_table('public.graph_boundary_nodes'::regclass, 'id');"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.build();"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.vacuum();"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.maintenance();"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT graph.reset();"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT graph.enable_sync();"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.apply_sync();"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.connected_components();"
expect_sqlstate_as_role "$ROLE_NAME" "42501" "SELECT * FROM graph.component_stats();"
expect_placeholder_rls_bypass_rejected "$ROLE_NAME"
expect_sqlstate_as_login "$ROLE_NAME" "42501" "SELECT count(*) FROM graph.status(); SET graph.rls_mode = 'legacy_bypass';"

echo "SQLSTATE/ACL boundary checks passed on database: $DBNAME"
