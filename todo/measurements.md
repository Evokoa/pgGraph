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
