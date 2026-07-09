#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_rss}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
NODE_COUNT="${NODE_COUNT:-200000}"
EDGE_COUNT="${EDGE_COUNT:-199999}"
BUILD_BATCH_SIZE="${BUILD_BATCH_SIZE:-10000}"
REBUILD_ROUNDS="${REBUILD_ROUNDS:-0}"
LOW_MEMORY_BUILD="${LOW_MEMORY_BUILD:-off}"
MAX_RSS_MB="${MAX_RSS_MB:-0}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-rss.XXXXXX")"
PID_FILE="$WORKDIR/backend.pid"
OUT_FILE="$WORKDIR/build.out"
RSS_FILE="$WORKDIR/rss.tsv"

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

cargo pgrx install --pg-config "$PG_CONFIG" --features "$PG_VERSION_FEATURE" --no-default-features
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql "$DBNAME" <<SQL
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.reset();
CREATE TABLE public.graph_rss_nodes (id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE public.graph_rss_edges (
    id BIGSERIAL PRIMARY KEY,
    from_id TEXT NOT NULL REFERENCES public.graph_rss_nodes(id),
    to_id TEXT NOT NULL REFERENCES public.graph_rss_nodes(id)
);
INSERT INTO public.graph_rss_nodes (id, name)
SELECT i::text, 'node-' || i::text FROM generate_series(1, $NODE_COUNT) AS i;
INSERT INTO public.graph_rss_edges (from_id, to_id)
SELECT i::text, (i + 1)::text FROM generate_series(1, $EDGE_COUNT) AS i;
SELECT graph.add_table('public.graph_rss_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge('public.graph_rss_edges'::regclass, 'from_id', 'public.graph_rss_nodes'::regclass, 'id', 'linked', false);
SQL

(
  {
    cat <<SQL
\t on
\a
\o $PID_FILE
SELECT pg_backend_pid();
\o
SET graph.build_batch_size = $BUILD_BATCH_SIZE;
SET graph.low_memory_build = $LOW_MEMORY_BUILD;
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
SQL
    if (( REBUILD_ROUNDS > 0 )); then
      for _ in $(seq 1 "$REBUILD_ROUNDS"); do
        echo "SELECT * FROM graph.build();"
      done
    fi
  } | psql "$DBNAME"
) >"$OUT_FILE" 2>&1 &
psql_pid=$!

for _ in $(seq 1 100); do
  [[ -s "$PID_FILE" ]] && break
  sleep 0.1
done

backend_pid="$(tr -dc '0-9' < "$PID_FILE" || true)"
peak_kb=0
while kill -0 "$psql_pid" >/dev/null 2>&1; do
  if [[ -n "$backend_pid" ]]; then
    rss_kb="$(ps -o rss= -p "$backend_pid" 2>/dev/null | tr -dc '0-9' || true)"
    if [[ -n "$rss_kb" ]]; then
      printf '%s\t%s\n' "$(date +%s)" "$rss_kb" >> "$RSS_FILE"
      if (( rss_kb > peak_kb )); then
        peak_kb="$rss_kb"
      fi
    fi
  fi
  sleep 0.25
done
wait "$psql_pid" || { cat "$OUT_FILE"; exit 1; }

peak_mb=$(( (peak_kb + 1023) / 1024 ))
graph_file_mb="$(psql "$DBNAME" -Atc "SELECT round(pg_database_size(current_database()) / 1048576.0, 1)")"
temp_bytes="$(psql "$DBNAME" -Atc "SELECT COALESCE(sum(temp_bytes), 0) FROM pg_stat_database WHERE datname = current_database()")"

if (( MAX_RSS_MB > 0 && peak_mb > MAX_RSS_MB )); then
  cat "$OUT_FILE"
  echo "Peak RSS ${peak_mb}MB exceeded MAX_RSS_MB=${MAX_RSS_MB}MB"
  exit 1
fi

cat "$OUT_FILE"
echo "Build executions: $((REBUILD_ROUNDS + 1))"
echo "Peak backend RSS: ${peak_mb}MB"
echo "Database size: ${graph_file_mb}MB"
echo "Temp bytes recorded: ${temp_bytes}"
