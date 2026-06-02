# Measurements

## 2026-06-02 Phase 1 Parser Slice

- `cargo fmt` from `graph/`: passed.
- `cargo test --features pg17 gql::` from `graph/`: passed, 24 tests.

## 2026-06-02 Phase 1 Binder Slice

- `cargo fmt` from `graph/`: passed.
- `cargo test --features pg17 query::tests::binder_` from `graph/`: passed, 48 tests.

## 2026-06-02 Phase 1 Executor and SQL Slice

- `cargo fmt --check` from `graph/`: passed.
- `cargo test --features pg17 query::tests::binder_rejects_unsupported_wildcard_path_shapes` from `graph/`: passed, 1 test.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests` from `graph/`: passed, 99 tests.
- `cargo test --features pg17 gql::` from `graph/`: passed, 24 tests.
- `cargo test --features pg17` from `graph/`: passed, 453 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test.

## 2026-06-02 Phase 2 Named Element Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 102 tests.
- `cargo test --features pg17 query::tests::wildcard_path_` from `graph/`: passed, 5 tests.
- `cargo test --features pg17` from `graph/`: passed, 458 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test.

## 2026-06-02 Phase 3A Fixed Multi-Segment Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::wildcard_path_` from `graph/`: passed, 6 tests.
- `cargo test --features pg17 query::tests::binder_rejects_multi_segment_wildcard_relationship_variables` from `graph/`: passed, 1 test.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 106 tests.
- `cargo test --features pg17` from `graph/`: passed, 460 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test.

## 2026-06-02 Phase 3B Initial Multi-Pattern Join Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 gql::tests::parses_comma_separated_match_patterns` from `graph/`: passed, 1 test.
- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 4 tests. Includes reviewer-requested exact unsupported-shape assertions and `SKIP` coverage.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 110 tests.
- `cargo test --features pg17` from `graph/`: passed, 465 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes unhydrated multi-pattern property projection through `graph.gql()`.

## 2026-06-02 Phase 3B Multi-Pattern Predicate Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 6 tests. Includes joined node-property `WHERE` predicates, JSON parameter evaluation, and a regression that `LIMIT` does not hide later predicate matches.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 112 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 467 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes parameterized multi-pattern `WHERE` projection through `graph.gql()`.

## 2026-06-02 Phase 3B Multi-Pattern Ordering Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 8 tests. Includes `ORDER BY` over returned property aliases, a regression that ordering happens before `LIMIT`, and a review-requested regression that raw row-cap exhaustion errors for ordered joins.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 114 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 469 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test.

## 2026-06-02 Phase 3B Multi-Pattern DISTINCT Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 9 tests. Includes projected-row `RETURN DISTINCT` deduplication and the unsupported `DISTINCT ORDER BY` non-returned-property regression.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 115 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 470 tests, 1 ignored.

## 2026-06-02 Phase 3C Wildcard Property Predicate Slice

- `cargo test --features pg17 query::tests::wildcard_path_` from `graph/`: passed, 8 tests. Includes named unlabeled path-node property filtering and raw row-cap exhaustion for predicate plans.
- `cargo test --features pg17 query::tests::binder_accepts_wildcard_path_common_node_property_predicates` from `graph/`: passed, 1 test.
- `cargo test --features pg17 query::tests::binder_rejects_wildcard_path_partially_available_node_property_predicates` from `graph/`: passed, 1 test.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes wildcard path `WHERE` over an unlabeled target node through `graph.gql()` with `hydrate := false`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 119 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 474 tests, 1 ignored.

## 2026-06-02 Phase 3D Variable-Length Wildcard Path Slice

- `cargo test --features pg17 query::tests::wildcard_path_` from `graph/`: passed, 13 tests. Includes bounded single-segment wildcard variable-length projection, bounded walk cycle behavior, row-cap exhaustion, tenant/overlay hop filtering, and the review-requested regression that endpoint labels filter only emitted endpoints, not intermediate hops.
- `cargo test --features pg17 query::tests::binder_accepts_variable_length_wildcard_path_without_element_variables` from `graph/`: passed, 1 test.
- `cargo test --features pg17 query::tests::binder_rejects_deferred_variable_length_wildcard_bindings` from `graph/`: passed, 1 test.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 126 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes unhydrated `graph.gql()` wildcard variable-length path projection and path-function shape checks.
- `cargo test --features pg17` from `graph/`: passed, 481 tests, 1 ignored.

## 2026-06-02 Phase 3E Wildcard Relationship Delete Slice

- `cargo test --features pg17 query::tests::binder_` from `graph/`: passed, 61 tests. Includes unique wildcard mapped-edge `DELETE`, named-node wildcard `DELETE` with a relationship type filter, inbound wildcard delete binding, ambiguous wildcard delete rejection, review-requested dynamic-label mapping rejection, review-requested ambiguous endpoint-label rejection, and the existing concrete mapped-edge delete cases.
- `cargo pgrx test --features "pg17 development" gql_wildcard_delete_edge_resolves_unique_mapped_row` from `graph/`: passed, 1 pgrx test. Deletes through `MATCH (u)-[r]->(v)` with endpoint predicates after the wildcard resolves to one registered edge-row mapping.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 132 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 487 tests, 1 ignored.

## 2026-06-03 Phase 3 Parser Fuzz Gate

- Added `gql_parser` seed corpus entries for path-variable projection and wildcard relationship delete parser shapes.
- Added the `cypher_parser` fuzz target plus compatible-match and unsupported-call seed corpus entries for the openCypher compatibility parser frontend.
- `cargo fuzz build` from `graph/`: blocked locally because the `cargo-fuzz` subcommand is not installed.
- `cargo build --manifest-path graph/fuzz/Cargo.toml --bins` from repository root: blocked locally by pgrx extension dylib linkage outside the pgrx fuzz build path; linker reported unresolved PostgreSQL backend symbols such as `CurrentMemoryContext`, `SPI_execute`, and `TopMemoryContext`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17 cypher::tests::` from `graph/`: passed, 6 tests.
