#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-graph_test}"
BACKENDS="${BACKENDS:-10}"
SLEEP_SECONDS="${SLEEP_SECONDS:-60}"
STARTUP_WAIT_SECONDS="${STARTUP_WAIT_SECONDS:-15}"
TMPDIR_ROOT="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "$TMPDIR_ROOT/pggraph-mmap-pss.XXXXXX")"
GRAPH_QUERY="${GRAPH_QUERY:-SELECT count(*) FROM graph.traverse('entities'::regclass, '10000001', 3, hydrate := false);}"

declare -a CLIENT_PIDS=()

cleanup() {
  for pid in "${CLIENT_PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "measure_mmap_pss.sh requires Linux /proc/<pid>/smaps_rollup for PSS accounting." >&2
  echo "macOS RSS cannot substitute for retained Linux PSS capacity evidence." >&2
  exit 2
fi

if [[ ! -r /proc/self/smaps_rollup ]]; then
  echo "/proc/<pid>/smaps_rollup is not readable on this host." >&2
  exit 2
fi

read_smaps_kb() {
  local pid="$1" field="$2"
  awk -v field="$field" '$1 == field ":" { print $2; found=1; exit } END { if (!found) print 0 }' \
    "/proc/$pid/smaps_rollup"
}

echo "Starting $BACKENDS backends against database $DBNAME"
echo "Graph query: $GRAPH_QUERY"

for i in $(seq 1 "$BACKENDS"); do
  out_file="$WORKDIR/backend_${i}.out"
  err_file="$WORKDIR/backend_${i}.err"
  (
    psql -X -qAt -v ON_ERROR_STOP=1 "$DBNAME" <<SQL
SELECT pg_backend_pid();
$GRAPH_QUERY
SELECT pg_sleep($SLEEP_SECONDS);
SQL
  ) >"$out_file" 2>"$err_file" &
  CLIENT_PIDS+=("$!")
done

declare -a BACKEND_PIDS=()
deadline=$((SECONDS + STARTUP_WAIT_SECONDS))
while (( SECONDS < deadline )); do
  BACKEND_PIDS=()
  for i in $(seq 1 "$BACKENDS"); do
    out_file="$WORKDIR/backend_${i}.out"
    if [[ -s "$out_file" ]]; then
      pid="$(head -n 1 "$out_file" | tr -dc '0-9' || true)"
      if [[ -n "$pid" && -r "/proc/$pid/smaps_rollup" ]]; then
        BACKEND_PIDS+=("$pid")
      fi
    fi
  done
  if (( ${#BACKEND_PIDS[@]} == BACKENDS )); then
    break
  fi
  sleep 0.25
done

if (( ${#BACKEND_PIDS[@]} == 0 )); then
  echo "No backend PIDs were captured." >&2
  for err in "$WORKDIR"/*.err; do
    [[ -s "$err" ]] && cat "$err" >&2
  done
  exit 1
fi

echo
printf 'pid\trss_kb\tpss_kb\tshared_clean_kb\tprivate_clean_kb\tprivate_dirty_kb\n'

total_rss=0
total_pss=0
total_shared_clean=0
total_private_clean=0
total_private_dirty=0

for pid in "${BACKEND_PIDS[@]}"; do
  rss="$(read_smaps_kb "$pid" Rss)"
  pss="$(read_smaps_kb "$pid" Pss)"
  shared_clean="$(read_smaps_kb "$pid" Shared_Clean)"
  private_clean="$(read_smaps_kb "$pid" Private_Clean)"
  private_dirty="$(read_smaps_kb "$pid" Private_Dirty)"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$pid" "$rss" "$pss" "$shared_clean" "$private_clean" "$private_dirty"

  total_rss=$((total_rss + rss))
  total_pss=$((total_pss + pss))
  total_shared_clean=$((total_shared_clean + shared_clean))
  total_private_clean=$((total_private_clean + private_clean))
  total_private_dirty=$((total_private_dirty + private_dirty))
done

echo
printf 'totals\t%s\t%s\t%s\t%s\t%s\n' \
  "$total_rss" "$total_pss" "$total_shared_clean" "$total_private_clean" "$total_private_dirty"

if (( total_rss > 0 )); then
  ratio="$(awk -v pss="$total_pss" -v rss="$total_rss" 'BEGIN { printf "%.3f", pss / rss }')"
  echo "total_pss_to_total_rss_ratio=$ratio"
fi

echo
echo "Interpretation:"
echo "- RSS includes private pages and double-counts any shared mappings in each backend."
echo "- PSS apportions shared libraries and mappings, so summed PSS is the multi-backend capacity measure."
echo "- Current graph artifacts are loaded into private anonymous snapshots; do not infer file-page sharing from this output."
