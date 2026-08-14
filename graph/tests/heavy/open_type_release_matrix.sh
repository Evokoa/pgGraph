#!/usr/bin/env bash
set -euo pipefail

PG_VERSIONS="${PG_VERSIONS:-14 15 16 17 18}"
RUN_PGRX_SQL=1
RUN_PACKAGE_INSTALL_MATRIX=1

# The source matrix includes the real open-type boundary and diagnostic gates:
# open_type_255_traversal_paths_gql_and_cypher_filter_exactly
# adaptive_edge_type_policy_limits_fail_atomically
# durable_sync_dictionary_corruption_fails_closed_without_advancing_generation
# The package matrix additionally runs open_type_package_smoke.sh after install.
if ! env \
  PG_VERSIONS="$PG_VERSIONS" \
  RUN_RUST_TESTS=0 \
  RUN_PGRX_SQL="$RUN_PGRX_SQL" \
  PGRX_TEST_FILTER="open_type_ adaptive_edge_type_policy_limits_fail_atomically durable_sync_dictionary_corruption_fails_closed_without_advancing_generation" \
  RUN_GQL_WRITE_MATRIX=0 \
  RUN_PACKAGE_INSTALL_MATRIX="$RUN_PACKAGE_INSTALL_MATRIX" \
  RUN_OPEN_TYPE_PACKAGE_SMOKE=1 \
  RUN_POSTGRES_SANITIZER=0 \
  RUN_DURABLE_PROJECTION_MATRIX=0 \
  RUN_CRASH_MATRIX=0 \
  RUN_RUNTIME_RESOURCES=0 \
  RUN_PG_UPGRADE_MATRIX=0 \
  RUN_V1_UPDATE_ARTIFACT_ROLLBACK=0 \
  ./tests/heavy/run_pg_matrix_docker.sh; then
  echo "Open-type release matrix failed" >&2
  exit 1
fi

echo "Open-type source and installed-package matrix passed for PostgreSQL: $PG_VERSIONS"
