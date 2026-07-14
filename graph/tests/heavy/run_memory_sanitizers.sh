#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-postgres}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
RUN_ASAN="${RUN_ASAN:-0}"
ASAN_TEST_FILTER="${ASAN_TEST_FILTER:-}"
RUN_PGRX="${RUN_PGRX:-1}"
RUN_PGBENCH="${RUN_PGBENCH:-1}"
RUN_VALGRIND="${RUN_VALGRIND:-0}"

echo "Memory/FFI boundary runs for $PG_VERSION_FEATURE."

if [[ "$RUN_ASAN" == "1" ]]; then
  target_triple="$(rustc +nightly -vV | sed -n 's/^host: //p')"
  target_rustflags="CARGO_TARGET_$(printf '%s' "$target_triple" | tr '[:lower:]-' '[:upper:]_')_RUSTFLAGS"
  export "$target_rustflags=-Zsanitizer=address"
  if [[ "$(uname -s)" == "Darwin" ]]; then
    asan_runtime="$(rustc +nightly --print target-libdir)/librustc-nightly_rt.asan.dylib"
    if [[ ! -f "$asan_runtime" ]]; then
      echo "AddressSanitizer runtime not found: $asan_runtime" >&2
      exit 2
    fi
    export DYLD_INSERT_LIBRARIES="$asan_runtime"
  fi
  if [[ -n "$ASAN_TEST_FILTER" ]]; then
    cargo +nightly test --target "$target_triple" \
      --features "$PG_VERSION_FEATURE" -Zbuild-std "$ASAN_TEST_FILTER"
  else
    cargo +nightly test --target "$target_triple" \
      --features "$PG_VERSION_FEATURE" -Zbuild-std
  fi
else
  echo "Skipping ASan. Set RUN_ASAN=1 to run nightly address-sanitizer tests."
fi

if [[ "$RUN_PGRX" == "1" ]]; then
  cargo pgrx test "$PG_VERSION_FEATURE"
fi

if [[ "$RUN_PGBENCH" == "1" ]]; then
  DBNAME="$DBNAME" CLIENTS="${CLIENTS:-16}" JOBS="${JOBS:-4}" TIME="${TIME:-600}" ./tests/heavy/run_pgbench_sync.sh
fi

if [[ "$RUN_VALGRIND" == "1" ]]; then
  : "${PGDATA:?PGDATA is required when RUN_VALGRIND=1}"
  : "${POSTGRES_BIN:=postgres}"
  valgrind --leak-check=full --trace-children=yes "$POSTGRES_BIN" -D "$PGDATA"
else
  echo "Skipping Valgrind. Set RUN_VALGRIND=1 with PGDATA and POSTGRES_BIN to run it."
fi

echo "Memory/FFI boundary checks completed."
