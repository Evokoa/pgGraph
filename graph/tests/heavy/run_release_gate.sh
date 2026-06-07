#!/usr/bin/env bash
set -euo pipefail

PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
DEVELOPMENT_FEATURES="$PG_VERSION_FEATURE development"
DB_PREFIX="${DB_PREFIX:-pggraph_release}"
RUN_DOCKER="${RUN_DOCKER:-0}"
RUN_FULL_MATRIX="${RUN_FULL_MATRIX:-0}"
RUN_CRASH="${RUN_CRASH:-0}"
RUN_PGBENCH="${RUN_PGBENCH:-1}"
RUN_PACKAGE="${RUN_PACKAGE:-1}"
RUN_BOUNDARY="${RUN_BOUNDARY:-1}"
RUN_INSTALL="${RUN_INSTALL:-1}"
RUN_BACKUP_RESTORE="${RUN_BACKUP_RESTORE:-1}"
RUN_BACKGROUND_LOCK="${RUN_BACKGROUND_LOCK:-1}"
RUN_BUILD_LOCK="${RUN_BUILD_LOCK:-1}"
RUN_CONCURRENCY="${RUN_CONCURRENCY:-1}"
RUN_METADATA="${RUN_METADATA:-1}"
RUN_RSS="${RUN_RSS:-0}"
RUN_SYNTHETIC="${RUN_SYNTHETIC:-1}"
RUN_PLAYGROUND="${RUN_PLAYGROUND:-1}"
RUN_GQL_CREATE_TX="${RUN_GQL_CREATE_TX:-1}"
RUN_GQL_SET_TX="${RUN_GQL_SET_TX:-1}"
RUN_GQL_DELETE_TX="${RUN_GQL_DELETE_TX:-1}"
RUN_GQL_MERGE_RACE="${RUN_GQL_MERGE_RACE:-1}"
RUN_TX_DELTA_CRASH="${RUN_TX_DELTA_CRASH:-0}"

if [[ "$RUN_FULL_MATRIX" == "1" ]]; then
  ./tests/heavy/run_pg_matrix.sh
fi

cargo fmt --check
cargo clippy --features "$DEVELOPMENT_FEATURES" --all-targets -- -D warnings
cargo doc --features "$PG_VERSION_FEATURE" --no-deps
cargo test --features "$PG_VERSION_FEATURE"
cargo pgrx test --features "$DEVELOPMENT_FEATURES" "$PG_VERSION_FEATURE"
cargo deny check advisories bans licenses sources
(cd fuzz && cargo check --bins)
./fuzz/run_projection_seed_corpora.sh

if [[ "$RUN_PACKAGE" == "1" ]]; then
  PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/package_validate.sh
fi

if [[ "$RUN_INSTALL" == "1" ]]; then
  DBNAME="${DB_PREFIX}_install" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/fresh_install_smoke.sh
fi

if [[ "$RUN_METADATA" == "1" ]]; then
  DBNAME="${DB_PREFIX}_metadata" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/function_metadata_audit.sh
fi

if [[ "$RUN_BOUNDARY" == "1" ]]; then
  DBNAME="${DB_PREFIX}_boundary" ./tests/heavy/run_sqlstate_acl_boundary.sh
fi

if [[ "$RUN_BACKUP_RESTORE" == "1" ]]; then
  SOURCE_DB="${DB_PREFIX}_backup_src" RESTORE_DB="${DB_PREFIX}_backup_dst" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/backup_restore_validate.sh
fi

if [[ "$RUN_BACKGROUND_LOCK" == "1" ]]; then
  DBNAME="${DB_PREFIX}_background_lock" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/background_job_lock_regression.sh
fi

if [[ "$RUN_BUILD_LOCK" == "1" ]]; then
  DBNAME="${DB_PREFIX}_build_lock" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/build_lock_regression.sh
fi

if [[ "$RUN_CONCURRENCY" == "1" ]]; then
  DBNAME="${DB_PREFIX}_concurrency" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/concurrency_stress.sh
fi

if [[ "$RUN_SYNTHETIC" == "1" ]]; then
  DBNAME="${DB_PREFIX}_synthetic" \
    NODE_COUNT="${SYNTHETIC_NODE_COUNT:-50000}" \
    HUB_FANOUT="${SYNTHETIC_HUB_FANOUT:-1000}" \
    MAX_BUILD_MS="${SYNTHETIC_MAX_BUILD_MS:-60000}" \
    MAX_QUERY_MS="${SYNTHETIC_MAX_QUERY_MS:-1000}" \
    ./tests/heavy/synthetic_release_smoke.sh
fi

if [[ "$RUN_PLAYGROUND" == "1" ]]; then
  PGGRAPH_REBUILD_IMAGE=1 PGGRAPH_RECREATE_CONTAINER=1 ./tests/heavy/playground_release_gate.sh
fi

if [[ "$RUN_PGBENCH" == "1" ]]; then
  DBNAME="${DB_PREFIX}_pgbench" CLIENTS="${CLIENTS:-4}" JOBS="${JOBS:-2}" TIME="${TIME:-30}" ./tests/heavy/run_pgbench_sync.sh
fi

if [[ "$RUN_GQL_CREATE_TX" == "1" ]]; then
  DBNAME="${DB_PREFIX}_gql_create_tx" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/gql_create_tx_lifecycle.sh
fi

if [[ "$RUN_GQL_SET_TX" == "1" ]]; then
  DBNAME="${DB_PREFIX}_gql_set_tx" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/gql_set_tx_lifecycle.sh
fi

if [[ "$RUN_GQL_DELETE_TX" == "1" ]]; then
  DBNAME="${DB_PREFIX}_gql_delete_tx" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/gql_delete_tx_lifecycle.sh
fi

if [[ "$RUN_GQL_MERGE_RACE" == "1" ]]; then
  DBNAME="${DB_PREFIX}_gql_merge_race" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/gql_merge_race.sh
fi

if [[ "$RUN_RSS" == "1" ]]; then
  DBNAME="${DB_PREFIX}_rss" PG_VERSION_FEATURE="$PG_VERSION_FEATURE" ./tests/heavy/measure_build_rss.sh
fi

if [[ "$RUN_CRASH" == "1" ]]; then
  : "${PGDATA:?PGDATA must point at a disposable cluster when RUN_CRASH=1}"
  DBNAME="${DB_PREFIX}_crash" PGDATA="$PGDATA" ./tests/heavy/crash_recovery.sh
fi

if [[ "$RUN_TX_DELTA_CRASH" == "1" ]]; then
  : "${PGDATA:?PGDATA must point at a disposable cluster when RUN_TX_DELTA_CRASH=1}"
  DBNAME="${DB_PREFIX}_tx_delta_crash" PGDATA="$PGDATA" ./tests/heavy/tx_delta_crash_recovery.sh
fi

if [[ "$RUN_DOCKER" == "1" ]]; then
  ./tests/heavy/docker_smoke.sh
fi

echo "Release gate passed for $PG_VERSION_FEATURE"
