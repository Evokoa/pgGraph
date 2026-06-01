# Consolidated Review Remediation Plan

Date: 2026-06-01

Status: planning. This combines the two review reports produced for
`2afb40321fbecac602ee0912c2183adc6cd0873c..HEAD` into one implementation
backlog. The plan is ordered to fix observable correctness risks first, then
close weak-path testing gaps, then pay down scaffolding and file-size pressure.

Commands in this plan intentionally use direct Cargo commands because the
current repository instruction is to run without `sfw`.

## Goals

1. Restore GQL write and read semantics where the current implementation can
   mutate the wrong row, expose the wrong tenant data, or change row cardinality
   based on projection shape.
2. Turn the review's weak-path concerns into regression tests before changing
   production code.
3. Separate supported, gated, and future-only language surfaces so scaffolded
   code cannot be mistaken for complete public behavior.
4. Reduce maintenance risk from mega files and duplicated validation rules
   without a broad rewrite.
5. Keep all fixes inside the current crate/module structure unless a module is
   already crossing a practical boundary.

## Non-Goals

- Do not redesign the graph storage engine.
- Do not promote the project to a wider Cargo workspace only to address file
  size. Split modules inside the existing crate first.
- Do not expand openCypher or SQL/PGQ compatibility while fixing review
  findings. Gate, document, or remove partial surfaces instead.
- Do not make performance changes that alter query semantics without paired
  before/after tests.

## Phase 0 - Fact Check And Lock The Test Surface

1. Reproduce each confirmed behavior with a failing unit, pgrx, or heavy test:
   - Dynamic-label GQL DELETE must not count or delete a different relationship
     with the same endpoints.
   - `SET`, `REMOVE`, `DELETE`, and `DETACH DELETE` must not update/delete a row
     that no longer satisfies the matched predicate at write time.
   - Tenant-scoped scans must not see transaction-local unscoped inserted nodes
     unless a documented global-row policy says otherwise.
   - The same graph pattern must produce the same match cardinality regardless
     of whether the caller returns a path, scalar, or aggregate.
   - `collect()` over optional/null-extended rows must have explicit semantics.
     If following SQL/GQL aggregate behavior, null values are skipped.
   - The MERGE race test must coordinate deterministically and assert both
     session outputs.
2. Audit speculative findings before coding:
   - Verify whether transaction delta cleanup can be stranded after a mid-path
     PostgreSQL error. If yes, add an RAII cleanup guard; if no, document the
     infallibility boundary in code and tests.
   - Confirm whether `query/explain.rs` is an intentional minimal explain
     surface or a stub that should be gated/removed.
   - Re-check all remaining `#[allow(dead_code)]` instances and classify each
     as test-only, feature-gated, or removable.
3. Establish the regression command set:
   - `git diff --check`
   - `cargo fmt --check`
   - `cargo test --features "pg17 development" query::`
   - `cargo pgrx test --features "pg17 development" gql`
   - targeted heavy scripts for transaction lifecycle, MERGE race, ACL, and
     install smoke paths when PostgreSQL mutation is approved.

## Phase 1 - Correctness Blockers

1. **Dynamic edge-label DELETE**
   - Files: `graph/src/sql_facade/gql.rs`.
   - Carry `label_column` and bound relationship type into the count and delete
     helpers.
   - Predicate dynamic edge tables on source, target, and label.
   - Tests: pgrx case with one edge table, shared endpoints, two labels, and a
     DELETE targeting only one label.

2. **Write-time stale snapshot protection**
   - Files: `graph/src/sql_facade/gql.rs`, query predicate/hydration helpers as
     needed.
   - Re-lock and re-check the PostgreSQL row before final `UPDATE`/`DELETE`, or
     fold the matched predicate plus tenant scope into the final SQL statement.
   - Preserve typed GQL execution errors for zero-row and multi-row outcomes.
   - Tests: concurrent update/delete drift for `SET`, `REMOVE`, `DELETE`, and
     `DETACH DELETE`.

3. **Tenant visibility for transaction-local inserts**
   - Files: `graph/src/projection/tx_delta.rs` and scan callers.
   - Replace implicit `(Some(active_tenant), None) => true` visibility with an
     exact-match policy unless global rows are intentionally modeled.
   - Tests: tenant-scoped GQL scan with unscoped tx insert, plus same-tenant and
     different-tenant positive/negative cases.

4. **Projection-independent traversal cardinality**
   - Files: `graph/src/query/execute.rs` and traversal result shaping.
   - Move path-return shaping after expansion/dedup semantics so `RETURN p` does
     not change the rows matched by the pattern.
   - Tests: paired queries for the same pattern with `RETURN p`, `RETURN n`,
     `RETURN count(*)`, and grouped aggregates.

5. **Aggregate null semantics and budget**
   - Files: `graph/src/query/value.rs`.
   - Decide and encode `collect()` null behavior. Prefer skipping nulls unless
     the GQL spec path chosen for this project requires preserving them.
   - Add an explicit collection size budget to prevent unbounded Vec growth.
   - Tests: `OPTIONAL MATCH ... collect(...)`, `collect(DISTINCT ...)`, empty
     input, null-bearing input, and budget overflow.

6. **Production panic removal**
   - Files: `graph/src/query/value.rs` first; then audit other production
     `unreachable!()` sites.
   - Replace data-dependent `unreachable!()` with typed internal errors. Keep
     only mechanically impossible assertions with a narrow comment.
   - Tests: malformed aggregate state or comparison paths should return errors,
     not panic across the PostgreSQL boundary.

## Phase 2 - Transaction Overlay And Traversal Robustness

1. **Net-neutral edge delta capacity**
   - Files: `graph/src/projection/tx_delta.rs`.
   - Compute net growth before checking capacity so add-after-delete and
     delete-after-add at the limit do not fail when the resulting delta size is
     unchanged.
   - Tests: exact-limit boundary cases for both operation orders.

2. **Transaction-created node policy**
   - Files: `graph/src/projection/tx_delta.rs`, `graph/src/engine.rs`.
   - Choose one policy:
     - assign temporary node IDs and support tx-created nodes in traversal/path
       entry points, or
     - reject unsupported traversal/path use of tx-created nodes with a typed
       error.
   - Tests: node scan, traversal entry, path expansion, rollback, and commit
     lifecycle.

3. **Overlay neighbor performance**
   - Files: `graph/src/projection/neighbors.rs`, traversal call sites.
   - Replace O(n^2) duplicate checks with a `HashSet` or lazy per-traversal
     overlay index.
   - Add a terminal iterator phase so exhausted phases are not revisited.
   - Tests: duplicate suppression, reverse traversal, empty overlay, and mixed
     insert/delete overlays.

4. **Hot-loop transaction delta snapshots**
   - Files: `graph/src/bfs.rs`, path/traversal modules.
   - Snapshot deleted-node and deleted-edge sets once per traversal instead of
     reading thread-local state per neighbor.
   - Bench: run the BFS benchmark against the existing baseline and accept only
     no-regression or explained improvements.

5. **Tenant bitmap borrowing**
   - Files: traversal config and engine call sites.
   - Avoid deep-cloning tenant bitmaps into every traversal config; borrow or
     share immutable views with clear lifetime ownership.
   - Tests: tenant filtered traversal with read-only, tx overlay, and hydrated
     rows.

## Phase 3 - Language Surface, Parsers, And Public Claims

1. **Cypher preview boundary**
   - Files: `graph/src/cypher/`, `graph/src/sql_facade/cypher.rs`,
     `graph/src/lib.rs`, docs.
   - Decide whether Cypher remains available behind `development`, a preview
     GUC, or is removed until the Phase 4 plan is active.
   - Public behavior must clearly say narrow compatibility surface, not Neo4j
     compatibility.
   - Tests: unsupported features return stable SQLSTATEs and compatibility
     matrix rows remain honest.

2. **Parser fuzz targets**
   - Files: `graph/fuzz`, `graph/src/lib.rs` fuzz support exports.
   - Add fuzz targets for GQL and Cypher parser entry points using the existing
     support functions.
   - Verification: `cargo check --bins` in `graph/fuzz`; run a short local fuzz
     smoke when available.

3. **Identifier and keyword rules**
   - Files: GQL/Cypher lexers and parsers.
   - Add backtick quoted identifier support where the accepted grammar requires
     it.
   - Ensure unsupported keyword scanning does not reject property names or
     quoted identifiers.
   - Tests: keyword-as-property, mixed-case functions, backtick identifiers,
     malformed quoted identifiers.

4. **Path function case normalization**
   - Files: `graph/src/query/semantics.rs`.
   - Normalize path function names or use ASCII-insensitive matching for
     `nodes`, `relationships`, and `length`.
   - Tests: mixed-case variants and unsupported function spelling.

5. **SQL/PGQ adapter status**
   - Files: `graph/src/query/sqlpgq_adapter.rs`, architecture docs.
   - If it has no production caller, gate it to tests/development or delete it.
   - If kept, add module docs describing it as an internal future hook and
     remove broad `#![allow(dead_code)]`.

## Phase 4 - Error Paths And Heavy-Test Quality

1. Add GQL write error-path pgrx tests:
   - bad label and relationship type;
   - `SET NULL` on NOT NULL;
   - double-delete of the same node;
   - malformed property values;
   - delete with committed and rolled-back transaction lifecycle.
2. Extend heavy scripts:
   - replace sleep-based race coordination with advisory locks or
     `LISTEN/NOTIFY`;
   - add timeout guards;
   - assert SQLSTATEs, not only message text or final state.
3. Add module-specific edge-case tests:
   - connected-components overlay path;
   - filter-index NULL handling, empty arrays, jsonb path miss, list
     containment, and type mismatch;
   - `node_store.rs` owned-mode bounds behavior or a documented caller
     invariant.
4. Keep weak-path tests close to the code they constrain. Use pgrx only when
   SQL behavior, privileges, MVCC, or PostgreSQL error mapping is the behavior
   under test.

## Phase 5 - Code Health And Module Boundaries

Split only after the correctness tests are green so moves are mechanical.

1. `graph/src/sql_facade/gql.rs`:
   - split read execution, write execution, ACL/privilege checks, hydration, and
     SQL construction helpers.
2. `graph/src/engine.rs`:
   - extract traversal setup, path status, lifecycle/build status, and mutable
     overlay resolution helpers.
3. `graph/src/query/value.rs`:
   - extract aggregate state/evaluation, scalar comparison, JSON/path access,
     and value coercion.
4. `graph/src/query/semantics.rs`:
   - extract property validation and function binding. Property rules should
     have one source of truth shared by parser/semantics/catalog checks.
5. `graph/src/query/tests.rs` and `graph/src/pg_tests/gql.rs`:
   - split by feature domain and introduce fixture builders where duplication
     is hiding missing weak-path assertions.
6. Add module-level docs for new modules as they are split. Do not add planning
   citations to source comments; source docs should state invariants and public
   behavior only.

## Acceptance Gates

Each phase is complete only when:

1. The phase's regression tests fail before the production fix or are clearly
   documented as audit-only.
2. `git diff --check` and `cargo fmt --check` pass.
3. Relevant focused tests pass:
   - query unit tests for pure semantics/value/traversal changes;
   - pgrx GQL tests for SQL-facing behavior;
   - heavy scripts for concurrency, lifecycle, install, and ACL boundaries;
   - fuzz target build checks for parser work;
   - BFS benchmark when traversal hot loops change.
4. No public docs claim a feature is complete unless tests cover both happy and
   weak paths.
5. Mega-file splits are behavior-preserving and reviewed separately from
   semantic fixes when possible.

## Current High-Risk File Sizes

These are current counts from the working tree and should be used to prioritize
Phase 5. The missing old `graph/tests/pg_tests/gql.rs` path in the review report
corresponds to `graph/src/pg_tests/gql.rs`.

| File | Lines | Priority |
|---|---:|---|
| `graph/src/pg_tests/gql.rs` | 2802 | first |
| `graph/src/sql_facade/gql.rs` | 2247 | first |
| `graph/src/query/tests.rs` | 2235 | first |
| `graph/src/engine.rs` | 1992 | first |
| `graph/src/query/semantics.rs` | 1651 | second |
| `graph/src/query/value.rs` | 1383 | second |
| `graph/src/bfs.rs` | 1304 | second |
| `graph/src/gql/parser.rs` | 1164 | second |

## Open Decisions

1. Should unscoped transaction-local rows ever be visible inside tenant-scoped
   queries? Default answer: no, unless a formal global-row policy is added.
2. Should `collect()` preserve nulls to match the project's chosen GQL behavior,
   or skip them like other aggregates? Default answer: skip nulls.
3. Should tx-created nodes receive temporary IDs for traversal immediately, or
   should traversal reject them until durable IDs exist? Default answer: reject
   clearly unless Phase 2 requires full temp-ID support.
4. Should Cypher stay compiled in development for testing, be gated by a preview
   GUC, or be deleted until Phase 4? Default answer: keep only if every SQL
   function exposes clear preview wording and unsupported-feature SQLSTATEs.
