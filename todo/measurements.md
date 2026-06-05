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

## 2026-06-04 Phase 3B Multi-Pattern Path-Function WITH Slice

- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 17 tests. Includes `WITH length(p)`, `WITH nodes(p)`, `WITH relationships(p)`, and chained path-function aliases over fixed single-hop multi-pattern join paths.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes path-function `WITH` aliases through `graph.gql()` with coordinate-only output.

## 2026-06-04 Phase 3B Multi-Pattern Aggregate WITH Slices

- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 18 tests. Includes grouped aggregate `WITH` aliases, direct aggregate `WITH DISTINCT`, relationship/path aggregate aliases through `WITH`, chained aggregate aliases with hidden grouping keys, and explicit later `WITH DISTINCT` over aggregate aliases plus post-aggregate-`WITH` aggregate-expression deferrals.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes grouped aggregate `WITH DISTINCT` aliases through `graph.gql()` with coordinate-only output.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 141 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 496 tests, 1 ignored.

## 2026-06-04 Phase 3B Multi-Pattern Post-Aggregate Boundary Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::multi_pattern_join_` from `graph/`: passed, 18 tests. Includes later `WITH DISTINCT` over projected aggregate aliases and final aggregate expressions over projected aggregate aliases after aggregate `WITH` boundaries.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 141 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 496 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes later aggregate-alias `WITH DISTINCT` and final aggregate expressions over aggregate `WITH` aliases through `graph.gql()`.

## 2026-06-04 Phase 3B Multi-Pattern Optional Join Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::multi_pattern` from `graph/`: passed, 19 tests. Includes optional multi-pattern joins that null-extend unmatched fixed single-hop relationship/path projections.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 142 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 497 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes optional multi-pattern join null-extension through `graph.gql()`.

## 2026-06-04 Phase 3B Multi-Pattern Variable-Length Join Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::multi_pattern` from `graph/`: passed, 20 tests. Includes bounded variable-length multi-pattern joins that preserve path values and lengths while joining later patterns against the bounded endpoint.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 143 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 498 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes bounded variable-length multi-pattern join path functions through `graph.gql()`.

## 2026-06-04 Phase 3B Multi-Pattern Variable-Length Relationship Variable Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::multi_pattern` from `graph/`: passed, 21 tests. Includes variable-length relationship variables in multi-pattern joins as path values.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 144 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 499 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes variable-length relationship variables as path values through `graph.gql()`.

## 2026-06-04 Phase 3D Named Variable-Length Wildcard Segment Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::wildcard_path_variable_length` from `graph/`: passed, 4 tests. Includes named variable-length wildcard relationship variables as path values and named endpoint variables that bind to the emitted endpoint.
- `cargo test --features pg17 query::tests::wildcard_path_` from `graph/`: passed, 14 tests.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 146 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 501 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes named single-segment variable-length wildcard relationship and endpoint variables through `graph.gql()`.

## 2026-06-04 Phase 3C Wildcard Property Ambiguity Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::binder_rejects_wildcard_path_ambiguous_node_property_predicates` from `graph/`: passed, 1 test. Confirms wildcard property predicates reject common root properties whose registered JSONB subpaths differ across possible concrete node labels.
- `cargo test --features pg17 query::tests::binder_` from `graph/`: passed, 63 tests.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 147 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 502 tests, 1 ignored.

## 2026-06-04 Phase 3D Multi-Segment Variable-Length Wildcard Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::wildcard_path_executor_projects_multi_segment_variable_length_paths` from `graph/`: passed, 1 test. Confirms unnamed multi-segment wildcard path variables can include bounded variable-length relationship segments and still project the full path value.
- `cargo test --features pg17 query::tests::binder_rejects_deferred_variable_length_wildcard_bindings` from `graph/`: passed, 1 test. Confirms named target nodes on multi-segment variable-length wildcard boundaries remain rejected until slot semantics are designed.
- `cargo test --features pg17 query::tests::wildcard_path_` from `graph/`: passed, 15 tests.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 148 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 23 tests.
- `cargo test --features pg17` from `graph/`: passed, 503 tests, 1 ignored.

## 2026-06-04 Phase 3D Relationship Type Alternation Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 gql::tests::parses_relationship_type_alternation` from `graph/`: passed, 1 test.
- `cargo test --features pg17 query::tests::binder_routes_relationship_type_alternation_to_wildcard_paths_only` from `graph/`: passed, 1 test. Confirms wildcard path variables accept relationship type alternation while concrete single-hop reads, multi-pattern joins, and mapped DELETE reject alternation until their slot semantics are designed.
- `cargo test --features pg17 query::tests::wildcard_path_executor_filters_relationship_type_alternation` from `graph/`: passed, 1 test. Confirms alternation includes each selected relationship type and excludes unrelated registered types.
- `cargo test --features pg17 query::tests::wildcard_path_` from `graph/`: passed, 16 tests.
- `cargo test --features pg17 query::tests::` from `graph/`: passed, 150 tests.
- `cargo test --features pg17 gql::tests::` from `graph/`: passed, 24 tests.
- `cargo pgrx test --features "pg17 development" gql_wildcard_path_values_and_functions_have_stable_shape` from `graph/`: passed, 1 pgrx test. Includes `graph.gql()` relationship type alternation for wildcard path variables.
- `cargo test --features pg17` from `graph/`: passed, 506 tests, 1 ignored.

## 2026-06-04 Phase 3 Path-Pattern Completion Boundary

- `git diff --check` from repository root: passed.
- Documentation-only TODO update. Records that the Phase 3 path-pattern plan is complete for topology/path expansion, wildcard node-property predicates, bounded variable-length wildcard paths, and exact wildcard relationship deletes. Relationship source-row hydration and relationship property projection remain future work outside this plan.

## 2026-06-03 Edge Registration Validation Slice

- `cargo pgrx test --features "pg17 development" mixed_mode_junction_registration_fails_before_build` from `graph/`: passed, 1 pgrx test.
- `cargo pgrx test --features "pg17 development" public_add_edge_supports_fk_style_registered_source_tables` from `graph/`: passed, 1 pgrx test.
- `cargo pgrx test --features "pg17 development" public_add_edge_detects_registered_source_by_oid_across_search_path_changes` from `graph/`: passed, 1 pgrx test.
- `cargo pgrx test pg17 junction` from `graph/`: passed, 8 tests.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17` from `graph/`: passed, 489 tests, 1 ignored.

## 2026-06-04 GQL Write Predicate Re-Check Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 query::tests::binder_` from `graph/`: passed, 64 tests.
- `cargo test --features pg17` from `graph/`: passed, 506 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_set_property_updates_source_row_and_filter_index` from `graph/`: passed, 1 pgrx test.
- `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_write_recheck ./graph/tests/heavy/gql_write_recheck_race.sh` from repository root: passed after running with sandbox escalation so `cargo pgrx install` could copy the extension into the Homebrew PostgreSQL extension directory. The two-session races confirm stale `SET` predicates, tenant-scope drift, and stale `DETACH DELETE` predicates fail after the final row-lock re-check instead of overwriting or deleting concurrent committed source-row changes.

## 2026-06-04 GQL Transaction-Created Traversal Policy Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17 transaction_created_node` from `graph/`: passed, 5 tests. Confirms node-only reads still see transaction-created nodes while concrete traversal, multi-pattern joins, and wildcard paths reject matching transaction-created entry points.
- `cargo test --features pg17 excluded_by_source_id` from `graph/`: passed, 3 tests. Confirms literal source-id predicates do not reject unrelated transaction-created rows in the same source table, including unlabeled wildcard source scans.
- `cargo test --features pg17 source_id` from `graph/`: passed, 5 tests. Confirms contradictory source-id predicates do not over-reject transaction-created rows, while source-id disjunctions still reject matching transaction-created rows.
- `cargo test --features pg17 bound_later_source_slots` from `graph/`: passed, 1 test. Confirms non-optional multi-pattern joins do not reject transaction-created rows for later source slots already bound by an earlier materialized hop.
- `cargo pgrx test --features "pg17 development" gql_rejects_transaction_created_node_traversal_entry_points` from `graph/`: passed, 1 pgrx test. Confirms the typed rejection reaches `graph.gql()` after a GQL `CREATE` records a transaction-local node delta, while a materialized source-id traversal remains allowed.
- `cargo test --features pg17` from `graph/`: passed, 516 tests, 1 ignored.

## 2026-06-05 GQL Write Re-Check Follow-Up Slice

- `bash -n graph/tests/heavy/gql_write_recheck_race.sh` from repository root: passed.
- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo test --features pg17` from `graph/`: passed, 516 tests, 1 ignored.
- `cargo pgrx test --features "pg17 development" gql_delete_edge_write_recheck_rejects_stale_endpoint_predicate` from `graph/`: passed, 1 pgrx test. Confirms mapped edge `DELETE` final write-boundary rechecks fail with `PG017` when a locked endpoint row no longer satisfies the bound target predicate.
- `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_write_recheck ./tests/heavy/gql_write_recheck_race.sh` from `graph/`: passed after running with sandbox escalation so `cargo pgrx install` could copy the extension into the Homebrew PostgreSQL extension directory. The two-session races now include stale `REMOVE` predicates in addition to stale `SET`, tenant-scope drift, and stale `DETACH DELETE`.

## 2026-06-05 Relationship Registration Ergonomics Boundary

- Documentation-only checkpoint. Recorded the current decision to keep
  `graph.add_edge()` as the public relationship registration API and defer a
  dedicated junction-table helper until repeated post-fix user reports justify
  another SQL function.
- Updated public docs to clarify that many-to-many junction tables use
  edge-table registration and that `to_column` names the target endpoint column
  on the junction table, while `to_table` is the preferred registered target
  table before fallback endpoint resolution.
- `git diff --check` from repository root: passed.
- `cargo fmt --check` from `graph/`: passed.

## 2026-06-04 KI-009 GQL SET SQLSTATE Hardening Slice

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo pgrx test --features "pg17 development" gql_set_property_readonly_projection_reports_sqlstate` from `graph/`: passed, 1 pgrx test. Confirms readonly mapped `SET` rejects through the client-visible unsupported-operation SQLSTATE `PG018`.
- `cargo pgrx test --features "pg17 development" gql_set_property_type_mismatch_does_not_mutate_source_row` from `graph/`: passed, 1 pgrx test. Confirms mapped `SET` type mismatches still leave the source row unchanged.
- `cargo test --features pg17` from `graph/`: passed, 516 tests, 1 ignored.
