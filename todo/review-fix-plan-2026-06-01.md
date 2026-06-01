# Review Fix Plan: GQL Hydration, MERGE ACL, SQL/PGQ Seam

Date: 2026-06-01

Status, 2026-06-01: implementation, focused unit coverage, GQL pgrx coverage,
docs drift, and non-installing release gates are complete. The aggregate
`run_release_gate.sh` and install/database-heavy smoke scripts require explicit
approval because they mutate local PostgreSQL installation paths and disposable
databases.

This plan closes the high-signal findings from the Rust review pass over
`2afb40321fbecac602ee0912c2183adc6cd0873c..HEAD`.

## Goals

1. **Hydration consistency**
   - `hydrate := true` must not fabricate an empty node object when a graph
     coordinate cannot be hydrated from PostgreSQL.
   - Required node hydration should fail with a typed GQL execution error when
     a backing source row is missing.
   - Optional unmatched relationship targets still project as JSON `null`.
   - Missing JSONB path keys keep the existing documented behavior: projection
     returns JSON `null`, but `IS NULL` does not match a missing path.

2. **MERGE ACL precision**
   - `MERGE` requires `SELECT` and `INSERT` on the mapped source table.
   - `UPDATE` is required only when the plan can update an existing matched row
     through `ON MATCH SET`.
   - Existing behavior for `SET`, `REMOVE`, `DELETE`, and `DETACH DELETE`
     remains unchanged.

3. **SQL/PGQ adapter seam**
   - Keep the internal typed adapter out of public API claims.
   - Remove or narrow production dead-code allowances where possible.
   - Document that SQL/PGQ remains an internal future hook, not an exposed
     compatibility surface.

4. **Tests and docs**
   - Add regression tests before production fixes where local unit seams exist.
   - Add pgrx tests for public ACL behavior.
   - Update user and contributor docs for hydration failure semantics and MERGE
     privilege requirements.

## Implementation Sequence

1. Add failing unit tests for missing required hydration:
   - Relationship read returning a node with a missing hydrated source row.
   - Node-only read returning a node with a missing hydrated source row.
   - Predicate `IS NULL` over a missing hydrated source row must not silently
     match as if a real SQL `NULL` existed.

2. Change hydration projection/evaluation to distinguish:
   - **optional unmatched coordinate**: `None` at the row level, projects as
     JSON `null`.
   - **missing required backing row**: coordinate exists, hydration map has no
     object for it, returns `GraphError::GqlExecution`.

3. Add pgrx ACL coverage for `MERGE`:
   - A role with table `SELECT` and `INSERT`, but no `UPDATE`, can run `MERGE`
     without `ON MATCH SET`.
   - The same role is denied for `MERGE ... ON MATCH SET ...`.

4. Update `check_merge_acl` to require `UPDATE` only when `on_match.is_some()`.
   Also keep the execution path branch-sensitive: `MERGE` uses `FOR UPDATE`
   only when an `ON MATCH SET` branch can update the matched row.

5. Reclassify the SQL/PGQ seam:
   - Keep it internal and documented as not public.
   - If the adapter remains test-only, gate it to test builds; otherwise expose
     a real internal caller that justifies compilation.

6. Update docs:
   - `docs/user_guide/querying.mdx`
   - `docs/user_guide/api-reference.mdx`
   - `docs/user_guide/administration-and-security.mdx` if SQLSTATE text needs
     clarification
   - contributor docs if release-gate or SQL/PGQ wording changes

## Verification Plan

Use the repository safety wrapper for Cargo commands.

1. `git diff --check`
2. `sfw cargo fmt --check`
3. `sfw cargo test --features "pg17 development" query::`
4. `sfw cargo pgrx test --features "pg17 development" gql`
5. Release gate:
   - `sfw cargo test --features "pg17 development"`
   - `sfw cargo pgrx test --features "pg17 development"`
   - `graph/tests/heavy/run_release_gate.sh` with the repository's documented
     environment for pg17.

Completed verification:

- `git diff --check`
- `scripts/check_docs_drift.sh`
- `sfw cargo fmt --check`
- `sfw cargo clippy --features pg17 --all-targets -- -D warnings`
- `sfw cargo doc --features pg17 --no-deps`
- `sfw cargo test --features "pg17 development"`
- `sfw cargo pgrx test --features "pg17 development" gql`
- `sfw cargo deny check advisories bans licenses sources`
- `sfw cargo check --bins` in `graph/fuzz`
- `DBNAME=pggraph_boundary_gql_acl graph/tests/heavy/run_sqlstate_acl_boundary.sh`

Blocked pending explicit approval:

- `PG_VERSION_FEATURE=pg17 ./tests/heavy/run_release_gate.sh`
- `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_install_gql_acl ./tests/heavy/fresh_install_smoke.sh`

If `sfw` blocks local Cargo execution, record that explicitly and request the
minimum approval needed for the blocked verification command.

## Commit Checkpoints

1. Commit implementation + focused tests once the targeted Rust and pgrx tests
   pass.
2. Commit docs updates separately if they are substantial.
3. After release gates pass, spawn a Rust reviewing sub-agent for a final
   read-only diff review and fix any confirmed blockers before the final
   checkpoint commit.
