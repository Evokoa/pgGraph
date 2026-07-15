#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_process_sanitizer}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"
PORT="${PORT:-55470}"
STATEMENT_TIMEOUT_MS="${STATEMENT_TIMEOUT_MS:-300000}"
LOCK_TIMEOUT_MS="${LOCK_TIMEOUT_MS:-30000}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKDIR="$(mktemp -d "${PGGRAPH_TEST_TMPDIR:-/tmp}/pggraph-process-sanitizer.XXXXXX")"
DATA_DIR="$WORKDIR/data"
SOCKET_DIR="$WORKDIR/socket"
SERVER_LOG="$WORKDIR/postgres.log"
VALGRIND_PID=""
PG_BIN=""

stop_postgres() {
  if [[ -n "$VALGRIND_PID" ]]; then
    local server_pid="$VALGRIND_PID"
    VALGRIND_PID=""
    if kill -0 "$server_pid" >/dev/null 2>&1; then
      "$PG_BIN/pg_ctl" -D "$DATA_DIR" -m fast -t 10 -w stop >/dev/null 2>&1 || true
    fi
    if kill -0 "$server_pid" >/dev/null 2>&1; then
      kill "$server_pid" >/dev/null 2>&1 || true
      for _ in $(seq 1 100); do
        if ! kill -0 "$server_pid" >/dev/null 2>&1; then
          break
        fi
        sleep 0.1
      done
    fi
    if kill -0 "$server_pid" >/dev/null 2>&1; then
      kill -KILL "$server_pid" >/dev/null 2>&1 || true
    fi
    if ! wait "$server_pid"; then
      echo "Valgrind-wrapped PostgreSQL exited unsuccessfully" >&2
      return 1
    fi
  fi
}

show_diagnostics() {
  if [[ -f "$SERVER_LOG" ]]; then
    echo "PostgreSQL process-sanitizer log:" >&2
    sed -n '1,240p' "$SERVER_LOG" >&2
  fi
  for log in "$WORKDIR"/valgrind.*.log; do
    if [[ -f "$log" ]]; then
      echo "$(basename "$log"):" >&2
      sed -n '1,240p' "$log" >&2
    fi
  done
}

cleanup() {
  stop_postgres >/dev/null 2>&1 || true
  rm -rf "$WORKDIR"
}

report_error() {
  local status=$?
  trap - ERR
  show_diagnostics
  exit "$status"
}

trap report_error ERR
trap cleanup EXIT

if ! command -v valgrind >/dev/null 2>&1; then
  echo "valgrind is required for the PostgreSQL-process sanitizer gate" >&2
  exit 2
fi

if [[ -z "$PG_CONFIG" ]]; then
  if [[ -x "/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config"
  elif [[ -x "/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config"
  else
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE" >&2
    exit 2
  fi
fi

PG_BIN="$(dirname "$PG_CONFIG")"
for command in createdb initdb pg_ctl pg_isready postgres psql; do
  if [[ ! -x "$PG_BIN/$command" ]]; then
    echo "missing PostgreSQL executable: $PG_BIN/$command" >&2
    exit 2
  fi
done

cd "$GRAPH_DIR"
cargo pgrx install --pg-config "$PG_CONFIG" \
  --features "$PG_VERSION_FEATURE development" \
  --no-default-features

mkdir -p "$SOCKET_DIR"
"$PG_BIN/initdb" --auth=trust --username="${PGUSER:-pggraph}" \
  --no-instructions -D "$DATA_DIR" >/dev/null
cat >>"$DATA_DIR/postgresql.conf" <<EOF
shared_preload_libraries = 'graph'
max_worker_processes = 8
listen_addresses = ''
EOF

valgrind \
  --tool=memcheck \
  --leak-check=no \
  --track-origins=yes \
  --trace-children=yes \
  --suppressions="$SCRIPT_DIR/postgres_process_valgrind.supp" \
  --log-file="$WORKDIR/valgrind.%p.log" \
  "$PG_BIN/postgres" -D "$DATA_DIR" -F -p "$PORT" -k "$SOCKET_DIR" \
  >"$SERVER_LOG" 2>&1 &
VALGRIND_PID=$!

export PGHOST="$SOCKET_DIR"
export PGPORT="$PORT"
export PGUSER="${PGUSER:-pggraph}"
export PGCONNECT_TIMEOUT="${PGCONNECT_TIMEOUT:-10}"
export PGOPTIONS="${PGOPTIONS:+$PGOPTIONS }-c statement_timeout=${STATEMENT_TIMEOUT_MS} -c lock_timeout=${LOCK_TIMEOUT_MS}"

ready=0
for _ in $(seq 1 300); do
  if "$PG_BIN/pg_isready" -q -d postgres; then
    ready=1
    break
  fi
  if ! kill -0 "$VALGRIND_PID" >/dev/null 2>&1; then
    echo "Valgrind-wrapped PostgreSQL stopped during startup" >&2
    show_diagnostics
    exit 1
  fi
  sleep 0.1
done
if [[ "$ready" != "1" ]]; then
  echo "Valgrind-wrapped PostgreSQL did not become ready" >&2
  show_diagnostics
  exit 1
fi

"$PG_BIN/createdb" "$DBNAME"
"$PG_BIN/psql" -X -v ON_ERROR_STOP=1 -d postgres -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.persist_on_build = on" >/dev/null
"$PG_BIN/psql" -X -v ON_ERROR_STOP=1 -d postgres -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.default_projection_mode = 'mutable_overlay'" >/dev/null
"$PG_BIN/psql" -X -v ON_ERROR_STOP=1 -d postgres -c \
  "ALTER DATABASE \"$DBNAME\" SET graph.mutable_enabled = on" >/dev/null
"$PG_BIN/psql" -X -v ON_ERROR_STOP=1 -d "$DBNAME" \
  -f "$SCRIPT_DIR/postgres_process_sanitizer.sql" >/dev/null

"$PG_BIN/psql" -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "SELECT * FROM graph.unload_graph('default', namespace := 'public')" >/dev/null
artifact_path="$(find "$DATA_DIR/graph" -name main.pggraph -type f -print -quit)"
if [[ -z "$artifact_path" ]]; then
  echo "persisted sanitizer artifact was not created" >&2
  exit 1
fi
cp "$artifact_path" "$WORKDIR/main.pggraph.valid"
printf '\000' | dd of="$artifact_path" bs=1 seek=0 conv=notrunc status=none
if "$PG_BIN/psql" -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "SELECT * FROM graph.load_graph('default', namespace := 'public')" \
  >"$WORKDIR/corrupt-load.out" 2>&1; then
  echo "corrupt sanitizer artifact unexpectedly loaded" >&2
  exit 1
fi
cp "$WORKDIR/main.pggraph.valid" "$artifact_path"
"$PG_BIN/psql" -X -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "SELECT * FROM graph.load_graph('default', namespace := 'public')" >/dev/null
recovered_traversal="$("$PG_BIN/psql" -X -A -t -v ON_ERROR_STOP=1 -d "$DBNAME" -c \
  "SELECT count(*) FROM graph.traverse(
     'public.graph_process_sanitizer_nodes'::regclass,
     'child', 1, direction := 'out', hydrate := false
   ) WHERE node_id = 'root'")"
if [[ "$recovered_traversal" != "1" ]]; then
  echo "recovered artifact traversal returned $recovered_traversal rows, expected 1" >&2
  exit 1
fi

if ! stop_postgres; then
  show_diagnostics
  exit 1
fi

if ! compgen -G "$WORKDIR/valgrind.*.log" >/dev/null; then
  echo "Valgrind produced no process logs" >&2
  exit 1
fi
for log in "$WORKDIR"/valgrind.*.log; do
  if ! grep -Eq "ERROR SUMMARY: 0 errors" "$log"; then
    echo "Valgrind log does not contain a zero-error summary: $log" >&2
    show_diagnostics
    exit 1
  fi
done

echo "PostgreSQL-process sanitizer passed for $PG_VERSION_FEATURE"
