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
- Expanded the `gql_parser` seed corpus with malformed path-variable prefixes, inbound and undirected path forms, bounded wildcard syntax, label/type selector syntax, and duplicate-variable parser shapes.
- Added the `cypher_parser` fuzz target plus compatible-match and unsupported-call seed corpus entries for the openCypher compatibility parser frontend.
- Initial `cargo fuzz build` from `graph/`: blocked locally because the `cargo-fuzz` subcommand was not installed.
- Initial `cargo build --manifest-path graph/fuzz/Cargo.toml --bins` from repository root: blocked locally by pgrx extension dylib linkage outside the pgrx fuzz build path; linker reported unresolved PostgreSQL backend symbols such as `CurrentMemoryContext`, `SPI_execute`, and `TopMemoryContext`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17 cypher::tests::` from `graph/`: passed, 6 tests.

## 2026-06-04 Phase 3 Parser Fuzz Gate Follow-Up

- `sfw cargo install cargo-fuzz`: installed `cargo-fuzz` 0.13.1.
- `rustup toolchain install nightly`: installed `nightly-aarch64-apple-darwin`, `rustc 1.98.0-nightly (b354133fb 2026-06-03)`.
- `cargo +nightly fuzz build` from `graph/`: blocked on macOS because `cargo-fuzz` sets `RUSTFLAGS` and drops the pgrx dynamic-lookup linker flag from `graph/.cargo/config.toml`; the linker reports unresolved PostgreSQL backend symbols such as `AllocSetContextCreateInternal`, `CurrentMemoryContext`, and `DataDir`.
- `bash fuzz/build.sh` from `graph/`: passed, all fuzz targets.
- `cargo build --features pg17` from `graph/`: passed.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17 cypher::tests::` from `graph/`: passed, 6 tests.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test.

## 2026-06-03 Phase 3B Multi-Pattern Relationship Variable Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_projects_relationship_variables` from `graph/`: passed, 1 test.
- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 10 tests. Includes fixed single-hop relationship variable returns with aliases, duplicate relationship-variable and node/relationship-variable rejection, and explicit relationship-property deferral for multi-pattern joins.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes relationship variable projection from a fixed single-hop multi-pattern join through `graph.gql()`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 133 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.

## 2026-06-03 Phase 3B Multi-Pattern Path Variable Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_projects_path_variables` from `graph/`: passed, 1 test.
- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 11 tests. Includes fixed single-hop path variable returns with aliases, duplicate path/node variable rejection, and explicit path-property/path-function deferral for multi-pattern joins.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes path variable projection from a fixed single-hop multi-pattern join through `graph.gql()`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 134 tests.

## 2026-06-03 Phase 3B Multi-Pattern Path Function Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_projects_path_functions` from `graph/`: passed, 1 test.
- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 12 tests. Includes fixed single-hop `nodes(p)`, `relationships(p)`, and `length(p)` projection over path variables in multi-pattern joins.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 135 tests.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes path-function projection from a fixed single-hop multi-pattern join through `graph.gql()`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17` from `graph/`: passed, 490 tests, 1 ignored.

## 2026-06-03 Phase 3B Multi-Pattern Aggregate Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 13 tests. Includes grouped `count(*)`, `count(DISTINCT node)`, and node-property numeric aggregates over fixed single-hop multi-pattern joins, plus explicit relationship/path aggregate deferrals.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 136 tests.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes grouped multi-pattern aggregate projection through `graph.gql()`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17` from `graph/`: passed, 491 tests, 1 ignored.

## 2026-06-03 Phase 3B Multi-Pattern WITH Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 15 tests. Includes node and node-property `WITH` aliases, scope hiding, `WITH DISTINCT` deduplication before aggregation, and explicit relationship/path/aggregate/path-function `WITH` deferrals.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 138 tests.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes multi-pattern `WITH DISTINCT` projection through `graph.gql()`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17` from `graph/`: passed, 493 tests, 1 ignored.

## 2026-06-03 Phase 3B Multi-Pattern Relationship/Path Aggregate Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 15 tests. Includes relationship-value and path-value `count`/`collect` over fixed single-hop multi-pattern joins, while numeric aggregates over relationship/path values remain rejected.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 138 tests.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes relationship/path aggregate projection through `graph.gql()`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17` from `graph/`: passed, 493 tests, 1 ignored.

## 2026-06-03 Phase 3B Multi-Pattern Relationship/Path WITH Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 16 tests. Includes relationship and path `WITH` aliases plus `WITH DISTINCT` over a relationship alias.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 139 tests.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes relationship/path `WITH` aliases through `graph.gql()`.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17` from `graph/`: passed, 494 tests, 1 ignored.

## 2026-06-03 Edge Registration Validation Slice

- `cargo pgrx test --features "pg17 development" mixed_mode_junction_registration_fails_before_build` from `graph/`: passed, 1 pgrx test.
- `cargo pgrx test --features "pg17 development" public_add_edge_supports_fk_style_registered_source_tables` from `graph/`: passed, 1 pgrx test.
- `cargo pgrx test --features "pg17 development" public_add_edge_detects_registered_source_by_oid_across_search_path_changes` from `graph/`: passed, 1 pgrx test.
- `cargo pgrx test pg17 junction` from `graph/`: passed, 8 tests.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17` from `graph/`: passed, 489 tests, 1 ignored.
