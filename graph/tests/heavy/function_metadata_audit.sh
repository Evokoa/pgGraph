#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_metadata}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"

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

cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql "$DBNAME" -v ON_ERROR_STOP=1 -c "CREATE EXTENSION IF NOT EXISTS graph"

violations="$(psql "$DBNAME" -qAt <<'SQL'
WITH exported AS (
    SELECT p.oid,
           p.proname,
           pg_get_function_identity_arguments(p.oid) AS args,
           p.provolatile,
           p.proparallel,
           p.prosecdef,
           p.proleakproof,
           p.procost,
           p.prorows
    FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'graph'
),
allowed_security_definer AS (
    SELECT *
    FROM (VALUES
        ('_max_sync_log_id_for_current_role', ''),
        ('_pending_sync_rows_for_current_role', 'applied_sync_id bigint'),
        ('_selected_graph_id_for_current_role', ''),
        ('add_edge', 'from_table oid, from_column text, to_table oid, to_column text, label text, bidirectional boolean, weight_column text, label_column text'),
        ('add_edge_to_graph', 'graph_name text, from_table oid, from_column text, to_table oid, to_column text, label text, bidirectional boolean, weight_column text, label_column text, graph_tenant text, graph_namespace text'),
        ('add_table', 'table_name oid, id_column text, columns text[], tenant_column text'),
        ('add_table', 'table_name oid, id_columns text[], columns text[], tenant_column text'),
        ('add_table_to_graph', 'graph_name text, table_name oid, id_column text, columns text[], tenant_column text, graph_tenant text, graph_namespace text'),
        ('add_table_to_graph', 'graph_name text, table_name oid, id_columns text[], columns text[], tenant_column text, graph_tenant text, graph_namespace text'),
        ('add_sync_policy', 'graph_name text, schedule_interval_secs bigint, max_sync_lag_rows bigint, enabled boolean, graph_tenant text, graph_namespace text'),
        ('apply_sync', ''),
        ('build', ''),
        ('build_graph', 'graph_name text, force_persist boolean, graph_tenant text, graph_namespace text'),
        ('component_stats', ''),
        ('connected_components', ''),
        ('current_graph', ''),
        ('enable_sync', ''),
        ('graph_privileges', 'graph_name text, tenant text, namespace text'),
        ('graph_quota_usage', ''),
        ('graph_quotas', ''),
        ('graph_runtime_status', ''),
        ('job_runs', 'job_id text, graph_name text, graph_tenant text, graph_namespace text, max_rows integer'),
        ('job_stats', 'graph_name text, graph_tenant text, graph_namespace text'),
        ('jobs', 'graph_name text, graph_tenant text, graph_namespace text, max_rows integer'),
        ('list_graphs', ''),
        ('load_graph', 'graph_name text, tenant text, namespace text'),
        ('loaded_graphs', ''),
        ('registered_edges', ''),
        ('registered_edges_for_graph', 'graph_name text, graph_tenant text, graph_namespace text'),
        ('registered_tables', ''),
        ('registered_tables_for_graph', 'graph_name text, graph_tenant text, graph_namespace text'),
        ('reset', ''),
        ('run_due_jobs', 'max_jobs integer'),
        ('run_job', 'job_id text'),
        ('run_sync_policy', 'policy_id text'),
        ('select_graph', 'graph_name text, tenant text, namespace text'),
        ('set_current_graph', 'graph_name text, tenant text, namespace text'),
        ('set_graph_residency', 'graph_name text, residency text, tenant text, namespace text'),
        ('sync_policy_status', 'graph_name text, graph_tenant text, graph_namespace text, max_rows integer'),
        ('traverse', 'seed_table oid, seed_id text, max_depth integer, edge_types text[], direction text, node_tables oid[], filter jsonb, tenant text, strategy text, uniqueness text, include_start boolean, hydrate boolean, max_rows integer, row_offset integer, max_nodes integer, max_frontier integer'),
        ('unload_graph', 'graph_name text, tenant text, namespace text'),
        ('vacuum', ''),
        ('vacuum_graph', 'graph_name text, graph_tenant text, graph_namespace text'),
        ('maintenance', '"concurrently" boolean')
    ) AS allowed(proname, args)
),
violations AS (
    SELECT format('%s(%s): security definer is not expected', proname, args) AS problem
    FROM exported e
    WHERE prosecdef
      AND NOT EXISTS (
          SELECT 1
          FROM allowed_security_definer allowed
          WHERE allowed.proname = e.proname
            AND allowed.args = e.args
      )

    UNION ALL
    SELECT format('%s(%s): leakproof is not expected', proname, args)
    FROM exported
    WHERE proleakproof

    UNION ALL
    SELECT format('%s(%s): traversal set-returning function needs non-default COST', proname, args)
    FROM exported
    WHERE proname = 'traverse'
      AND procost <= 100

    UNION ALL
    SELECT format('%s(%s): traversal set-returning function needs ROWS estimate', proname, args)
    FROM exported
    WHERE proname = 'traverse'
      AND prorows <= 0

    UNION ALL
    SELECT format('%s(%s): mutation/admin function must be volatile', proname, args)
    FROM exported
    WHERE proname IN (
        'add_table', 'add_edge', 'reset', 'build', 'vacuum', 'maintenance',
        'apply_sync', 'enable_sync', 'disable_sync', 'enable', 'disable', 'gql'
    )
      AND provolatile <> 'v'
)
SELECT problem FROM violations ORDER BY problem;
SQL
)"

if [[ -n "$violations" ]]; then
  echo "graph SQL function metadata audit failed:"
  echo "$violations"
  exit 1
fi

echo "Function metadata audit passed for $DBNAME"
