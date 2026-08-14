#!/usr/bin/env bash
set -euo pipefail
shopt -s nullglob

DBNAME="${DBNAME:-pggraph_open_type_latency}"
if [[ ! "$DBNAME" =~ ^pggraph_[A-Za-z0-9_]+$ ]]; then
  echo "DBNAME must match ^pggraph_[A-Za-z0-9_]+$" >&2
  exit 2
fi
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
if [[ "$PG_VERSION_FEATURE" != "pg17" ]]; then
  echo "P9 latency evidence is declared for pg17" >&2
  exit 2
fi
PG_CONFIG="${PG_CONFIG:-$(command -v pg_config || true)}"
if [[ -z "$PG_CONFIG" || ! -x "$PG_CONFIG" ]]; then
  echo "PG_CONFIG must name an executable PostgreSQL 17 pg_config" >&2
  exit 2
fi
if [[ "$("$PG_CONFIG" --version)" != "PostgreSQL 17"* ]]; then
  echo "PG_CONFIG must report PostgreSQL 17" >&2
  exit 2
fi
PGBENCH="${PGBENCH:-$(dirname "$PG_CONFIG")/pgbench}"
PGHOST="${PGHOST:-localhost}"
PGPORT="${PGPORT:-28817}"
OUTPUT_DIR="${OUTPUT_DIR:-$(pwd)/open-type-latency-output}"
STATEMENT_TIMEOUT_MS="${STATEMENT_TIMEOUT_MS:-300000}"
SKIP_INSTALL="${SKIP_INSTALL:-0}"
MANAGE_PGRX="${MANAGE_PGRX:-1}"
NODE_COUNT=131072
DEGREE=8
EDGE_COUNT=1048576
DEPTH=4
TRANSACTIONS=50
WARMUPS=10
worker_started=0
database_created=0

if [[ ! "$STATEMENT_TIMEOUT_MS" =~ ^[1-9][0-9]*$ ]] || (( STATEMENT_TIMEOUT_MS > 900000 )); then
  echo "STATEMENT_TIMEOUT_MS must be an integer from 1 through 900000" >&2
  exit 2
fi
if [[ "$SKIP_INSTALL" != "0" && "$SKIP_INSTALL" != "1" ]]; then
  echo "SKIP_INSTALL must be 0 or 1" >&2
  exit 2
fi
if [[ "$MANAGE_PGRX" != "0" && "$MANAGE_PGRX" != "1" ]]; then
  echo "MANAGE_PGRX must be 0 or 1" >&2
  exit 2
fi
if [[ ! -x "$PGBENCH" ]]; then
  echo "pgbench is unavailable at $PGBENCH" >&2
  exit 2
fi

cleanup() {
  if [[ "$database_created" -eq 1 ]]; then
    dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
  fi
  if [[ "$worker_started" -eq 1 ]]; then
    cargo pgrx stop "$PG_VERSION_FEATURE" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

mkdir -p "$OUTPUT_DIR/raw/postgres"
"$PG_CONFIG" --version >"$OUTPUT_DIR/latency-postgres-version.txt"
"$PGBENCH" --version >"$OUTPUT_DIR/latency-pgbench-version.txt"
if [[ "$MANAGE_PGRX" -eq 1 ]]; then
  cargo pgrx start "$PG_VERSION_FEATURE"
  worker_started=1
fi
if [[ "$SKIP_INSTALL" -eq 0 ]]; then
  cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
fi
export PGHOST PGPORT
export PGOPTIONS="${PGOPTIONS:-} -c statement_timeout=${STATEMENT_TIMEOUT_MS} -c graph.memory_limit_mb=2048 -c graph.query_memory_mb=512"
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"
database_created=1
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c "CREATE EXTENSION graph" >/dev/null
psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -c "ALTER DATABASE \"$DBNAME\" SET graph.auto_load = on" \
  -c "ALTER DATABASE \"$DBNAME\" SET graph.memory_limit_mb = 2048" \
  -c "ALTER DATABASE \"$DBNAME\" SET graph.query_memory_mb = 512" >/dev/null
effective_limits="$(psql -X -v ON_ERROR_STOP=1 -At -F, -d "$DBNAME" \
  -c "SELECT current_setting('graph.memory_limit_mb')::int, current_setting('graph.query_memory_mb')::int")"
if [[ "$effective_limits" != "2048,512" ]]; then
  echo "effective graph memory limits differ from the declared 2048,512 MiB: $effective_limits" >&2
  exit 1
fi
printf '{"memory_limit_mb":2048,"query_memory_mb":512}\n' >"$OUTPUT_DIR/latency-settings.json"

SAMPLES="$OUTPUT_DIR/postgres-samples.csv"
ORACLES="$OUTPUT_DIR/postgres-oracles.csv"
printf 'query_surface,label_count,sample_index,elapsed_ns,result_digest\n' >"$SAMPLES"
printf 'query_surface,label_count,row_count,result_digest\n' >"$ORACLES"

query_sql() {
  case "$1" in
    traverse)
      printf "SELECT count(*) FROM graph.traverse('public.p9_latency_nodes'::regclass, '1', %s, hydrate := false);\n" "$DEPTH"
      ;;
    shortest_path)
      printf "SELECT count(*) FROM graph.shortest_path('public.p9_latency_nodes'::regclass, '1', 'public.p9_latency_nodes'::regclass, '33', %s, hydrate := false);\n" "$DEPTH"
      ;;
    gql)
      printf "SELECT count(*) FROM graph.gql(replace('MATCH (u@p9_latency_nodes {id: 1})-[@type_1]->(v@p9_latency_nodes) RETURN v', '@', chr(58)), hydrate := false);\n"
      ;;
    cypher)
      printf "SELECT count(*) FROM graph.cypher(replace('MATCH (u@p9_latency_nodes {id: 1})-[@type_1]->(v@p9_latency_nodes) RETURN v', '@', chr(58)), hydrate := false);\n"
      ;;
    *)
      echo "unknown query surface: $1" >&2
      return 2
      ;;
  esac
}

oracle_sql() {
  case "$1" in
    traverse)
      printf "SELECT count(*), md5(COALESCE(string_agg(node_id || ':' || depth, ',' ORDER BY depth, node_id), '')) FROM graph.traverse('public.p9_latency_nodes'::regclass, '1', %s, hydrate := false);\n" "$DEPTH"
      ;;
    shortest_path)
      printf "SELECT count(*), md5(COALESCE(string_agg(step || ':' || node_id || ':' || COALESCE(edge_label, ''), ',' ORDER BY step), '')) FROM graph.shortest_path('public.p9_latency_nodes'::regclass, '1', 'public.p9_latency_nodes'::regclass, '33', %s, hydrate := false);\n" "$DEPTH"
      ;;
    gql)
      printf "SELECT count(*), md5(string_agg(row #>> '{v,_id,id}', ',' ORDER BY row #>> '{v,_id,id}')) FROM graph.gql(replace('MATCH (u@p9_latency_nodes {id: 1})-[@type_1]->(v@p9_latency_nodes) RETURN v', '@', chr(58)), hydrate := false) AS exact_rows(row) HAVING count(*) = 1 AND count(row #>> '{v,_id,id}') = 1 AND min(row #>> '{v,_id,id}') = '2';\n"
      ;;
    cypher)
      printf "SELECT count(*), md5(string_agg(row #>> '{v,_id,id}', ',' ORDER BY row #>> '{v,_id,id}')) FROM graph.cypher(replace('MATCH (u@p9_latency_nodes {id: 1})-[@type_1]->(v@p9_latency_nodes) RETURN v', '@', chr(58)), hydrate := false) AS exact_rows(row) HAVING count(*) = 1 AND count(row #>> '{v,_id,id}') = 1 AND min(row #>> '{v,_id,id}') = '2';\n"
      ;;
  esac
}

for label_count in 254 65536; do
  psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" \
    -v label_count="$label_count" -v node_count="$NODE_COUNT" -v degree="$DEGREE" <<'SQL'
SELECT graph.reset();
DROP TABLE IF EXISTS public.p9_latency_edges;
DROP TABLE IF EXISTS public.p9_latency_nodes;
CREATE TABLE public.p9_latency_nodes (id integer PRIMARY KEY);
INSERT INTO public.p9_latency_nodes SELECT value FROM generate_series(1, :node_count) value;
CREATE TABLE public.p9_latency_edges (
  id bigint PRIMARY KEY,
  source_id integer NOT NULL REFERENCES public.p9_latency_nodes(id),
  target_id integer NOT NULL REFERENCES public.p9_latency_nodes(id),
  rel_type text NOT NULL
);
INSERT INTO public.p9_latency_edges
SELECT (source_id::bigint - 1) * :degree + edge_offset,
       source_id,
       ((source_id + edge_offset - 1) % :node_count) + 1,
       'type_' || ((((source_id::bigint - 1) * :degree + edge_offset - 1) % :label_count) + 1)
FROM generate_series(1, :node_count) source_id
CROSS JOIN generate_series(1, :degree) edge_offset;
SELECT graph.add_table('public.p9_latency_nodes'::regclass, 'id');
SELECT graph.add_edge(
  'public.p9_latency_edges'::regclass,
  'source_id', 'public.p9_latency_nodes'::regclass, 'target_id',
  'fallback', false, label_column := 'rel_type');
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
SELECT graph.unload_graph('default');
SELECT * FROM graph.load_graph('default');
SQL

  for surface in traverse shortest_path gql cypher; do
    oracle="$(oracle_sql "$surface" | psql -X -v ON_ERROR_STOP=1 -At -F, -d "$DBNAME")"
    if [[ ! "$oracle" =~ ^[0-9]+,[0-9a-f]{32}$ ]]; then
      echo "invalid $surface oracle for $label_count labels: $oracle" >&2
      exit 1
    fi
    row_count="${oracle%%,*}"
    digest="${oracle#*,}"
    printf '%s,%s,%s,%s\n' "$surface" "$label_count" "$row_count" "$digest" >>"$ORACLES"

    script="$OUTPUT_DIR/raw/postgres/${surface}-${label_count}.sql"
    query_sql "$surface" >"$script"
    log_prefix="$OUTPUT_DIR/raw/postgres/${surface}-${label_count}"
    for stale_log in "${log_prefix}".[0-9]*; do
      rm -f -- "$stale_log"
    done
    "$PGBENCH" -n -M extended -c 1 -j 1 -t "$TRANSACTIONS" \
      --log --log-prefix="$log_prefix" -f "$script" "$DBNAME" \
      >"${log_prefix}.stdout" 2>"${log_prefix}.stderr"
    logs=("${log_prefix}".*)
    latency_log=""
    for candidate in "${logs[@]}"; do
      if [[ "$candidate" =~ \.[0-9]+$ ]]; then
        latency_log="$candidate"
      fi
    done
    if [[ -z "$latency_log" ]]; then
      echo "pgbench did not retain a latency log for $surface/$label_count" >&2
      exit 1
    fi
    transaction_count="$(awk 'NF >= 3 {count++} END {print count+0}' "$latency_log")"
    if [[ "$transaction_count" -ne "$TRANSACTIONS" ]]; then
      echo "pgbench retained $transaction_count transactions, expected $TRANSACTIONS" >&2
      exit 1
    fi
    awk -v surface="$surface" -v labels="$label_count" -v warmups="$WARMUPS" -v digest="$digest" \
      'NF >= 3 {seen++; if (seen > warmups) printf "%s,%s,%d,%.0f,%s\n", surface, labels, seen-warmups, $3*1000, digest}' \
      "$latency_log" >>"$SAMPLES"
  done
done

python3 "$(dirname "$0")/../../../scripts/summarize_p9_open_type_postgres.py" \
  --samples "$SAMPLES" \
  --raw-log-dir "$OUTPUT_DIR/raw/postgres" \
  --hashes-output "$OUTPUT_DIR/postgres-log-hashes.csv" \
  --output "$OUTPUT_DIR/postgres-results.csv"
echo "P9 PostgreSQL latency evidence retained in $OUTPUT_DIR"
