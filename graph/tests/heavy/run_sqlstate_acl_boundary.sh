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
run_sql "CREATE TABLE public.graph_boundary_edges (id BIGSERIAL PRIMARY KEY, from_id TEXT NOT NULL REFERENCES public.graph_boundary_nodes(id), to_id TEXT NOT NULL REFERENCES public.graph_boundary_nodes(id), visible_to NAME NOT NULL);"
run_sql "CREATE TABLE public.graph_boundary_output_nodes (id TEXT PRIMARY KEY);"
run_sql "CREATE TABLE public.graph_boundary_identity_nodes (id TEXT PRIMARY KEY, visible_to NAME NOT NULL, name TEXT NOT NULL);"
run_sql "CREATE TABLE public.graph_boundary_sparse_nodes (id TEXT PRIMARY KEY, ordinal INT NOT NULL);"
run_sql "CREATE TABLE public.graph_boundary_secret_nodes (id TEXT PRIMARY KEY, output_id TEXT REFERENCES public.graph_boundary_output_nodes(id), edge_weight INT NOT NULL);"
run_sql "CREATE TABLE public.graph_boundary_public_nodes (id TEXT PRIMARY KEY, secret_id TEXT REFERENCES public.graph_boundary_secret_nodes(id), edge_weight INT NOT NULL);"
run_sql "INSERT INTO public.graph_boundary_nodes VALUES ('c', 't1', 'Carol', 30, NULL), ('b', 't2', 'Bob', 20, 'c'), ('a', 't1', 'Alice', 10, 'b');"
run_sql "INSERT INTO public.graph_boundary_edges (from_id, to_id, visible_to) VALUES ('a', 'b', 'graph_boundary_other');"
run_sql "INSERT INTO public.graph_boundary_output_nodes VALUES ('o1');"
run_sql "INSERT INTO public.graph_boundary_identity_nodes VALUES ('visible', '$ROLE_NAME', 'Visible'), ('hidden', 'graph_boundary_other', 'Hidden');"
run_sql "INSERT INTO public.graph_boundary_sparse_nodes SELECT 's' || value::text, value FROM generate_series(0, 999) AS value;"
run_sql "INSERT INTO public.graph_boundary_secret_nodes VALUES ('s1', 'o1', 1);"
run_sql "INSERT INTO public.graph_boundary_public_nodes VALUES ('p1', 's1', 1);"

expect_sqlstate "55000" "SELECT * FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1);"

run_sql "SELECT graph.add_table('public.graph_boundary_nodes'::regclass, 'id', ARRAY['tenant_id', 'name', 'age']);"
run_sql "SELECT graph.add_edge('public.graph_boundary_nodes'::regclass, 'friend_id', 'public.graph_boundary_nodes'::regclass, 'id', 'boundary', bidirectional := false);"
run_sql "SELECT graph.add_edge('public.graph_boundary_edges'::regclass, 'from_id', 'public.graph_boundary_nodes'::regclass, 'to_id', 'boundary_row', bidirectional := false);"
run_sql "SELECT graph.add_filter_column('public.graph_boundary_nodes'::regclass, 'age');"
run_sql "SELECT graph.add_table('public.graph_boundary_public_nodes'::regclass, 'id');"
run_sql "SELECT graph.add_table('public.graph_boundary_secret_nodes'::regclass, 'id');"
run_sql "SELECT graph.add_table('public.graph_boundary_output_nodes'::regclass, 'id');"
run_sql "SELECT graph.add_table('public.graph_boundary_identity_nodes'::regclass, 'id', ARRAY['visible_to', 'name']);"
run_sql "SELECT graph.add_table('public.graph_boundary_sparse_nodes'::regclass, 'id', ARRAY['ordinal']);"
run_sql "SELECT graph.add_edge('public.graph_boundary_public_nodes'::regclass, 'secret_id', 'public.graph_boundary_secret_nodes'::regclass, 'id', 'hidden_link', bidirectional := false, weight_column := 'edge_weight');"
run_sql "SELECT graph.add_edge('public.graph_boundary_secret_nodes'::regclass, 'output_id', 'public.graph_boundary_output_nodes'::regclass, 'id', 'visible_link', bidirectional := false, weight_column := 'edge_weight');"
run_sql "SELECT * FROM graph.build();"

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
run_sql "CREATE POLICY graph_boundary_edge_rls ON public.graph_boundary_edges FOR SELECT TO $ROLE_NAME USING (visible_to = current_user);"
expect_value_as_login "$ROLE_NAME" "1" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
run_sql "ALTER ROLE $ROLE_NAME BYPASSRLS;"
expect_value_as_login "$ROLE_NAME" "2" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
run_sql "ALTER ROLE $ROLE_NAME NOBYPASSRLS;"

if psql -X -q -tA -d "$DBNAME" -c "SELECT to_regprocedure('graph._test_arm_visibility_scan_cancel(bigint)') IS NOT NULL;" | grep -qx t; then
  expect_visibility_cancel_cleanup_as_login "$ROLE_NAME" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
  expect_value_as_login "$ROLE_NAME" "1" "SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 1, edge_types := ARRAY['boundary_row'], hydrate := false);"
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

expect_value_as_role "$ROLE_NAME" "2" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM public.graph_boundary_traversal_coords;"
expect_value_as_role "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM public.graph_boundary_traversal_coords g JOIN public.graph_boundary_nodes n ON n.id = g.node_id;"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 2, edge_types := ARRAY['boundary'], hydrate := false);"
expect_value_as_login "$ROLE_NAME" "1" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'a', 2, edge_types := ARRAY['boundary'], hydrate := true);"
expect_value_as_login "$ROLE_NAME" "0" "SET graph.boundary_tenant = 't1'; SELECT count(*) FROM graph.traverse('public.graph_boundary_nodes'::regclass, 'b', 2, edge_types := ARRAY['boundary'], hydrate := false);"

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
