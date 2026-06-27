# Release Gate ACL/GUC Failure Plan

## 1. Cause

Using `rust-reviewing` debug workflow, the failures point to one primary regression family and one separate test/build configuration mismatch.

The primary cause is that internal catalog hardening revoked direct public access to `graph._graphs`, but several invoker-security SQL API paths still resolve selected graph metadata by querying that catalog table under the caller role.

Evidence:

- `graph/sql/bootstrap.sql:856` revokes all privileges on `graph._graphs` from `PUBLIC`.
- `graph/sql/bootstrap.sql:869-873` grants read access back to selected internal tables, but not to `graph._graphs`.
- `graph/src/sql_facade/gql.rs:124-149` exposes `graph.gql` without `security_definer`; the RLS/ACL tests intentionally call it after `SET ROLE` to a role that has only schema and source-table privileges.
- `graph/src/catalog/graphs.rs:773-781` resolves the selected graph through `selected_or_default_graph_metadata()`, which eventually reads `graph._graphs`. Under restricted roles, PostgreSQL raises SQLSTATE `42501` before pgGraph can apply graph visibility rules or return the expected pgGraph SQLSTATE `PG005`.
- The selected-graph spoof test expected `PG005`, but got `42501`, which confirms the failure is happening at PostgreSQL catalog ACL evaluation rather than at pgGraph's visible-graph validation boundary.

That explains the shared `_graphs` permission-denied failures:

- `pg_gql_create_node_preserves_source_table_rls`
- `pg_gql_hydration_fails_closed_when_source_row_is_not_visible`
- `pg_gql_merge_node_without_on_match_does_not_require_update_acl`
- `pg_selected_graph_guc_cannot_expose_another_roles_graph`
- `pg_sync_policies_run_through_visible_durable_jobs`

The secondary cause is feature/install skew for a development-only helper:

- `graph/src/sql_facade/admin.rs:5860-5862` defines `graph._test_run_job_internal` only when the Rust `development` feature is enabled.
- The non-development pgrx/local matrix gate still runs `pg_development_worker_entrypoints_restore_job_graph_context`, which calls that SQL function. If the extension was built or installed without `development`, PostgreSQL correctly reports that `graph._test_run_job_internal(unknown)` does not exist.

The dependency freshness audit is a separate release hygiene blocker, not the cause of the ACL/GUC failures. It must still be tracked because the release gate requires freshness:

- `cargo:bitvec` `1.0.1 -> 1.1.1`
- `github:nixpkgs` `f83fc3c307e74bc5fd5adb7eb6b8b13ffd2a36e1 -> 567a49d1913ce81ac6e9582e3553dd90a955875f`
- `github:rust-overlay` `d9973e2ab49747fada06ebbe26cda27eb0220cf1 -> 8534567325bd8a8d2928e6afd81e0a87d19efd3c`

## 2. Fix Needed To Be Implemented

Using `rust-planning`, the fix should preserve the PostgreSQL source-of-truth and ACL model:

- Keep internal durable catalog tables locked down. Do not restore broad `SELECT` on `graph._graphs` to `PUBLIC`.
- Route graph metadata reads for user-facing SQL APIs through a controlled definer boundary that captures the original caller role and applies pgGraph visibility checks explicitly.
- Keep source-table DML and hydration under the caller role so PostgreSQL ACLs, RLS, triggers, constraints, MVCC, and indexes remain authoritative.
- Convert invisible or spoofed selected graph ids into pgGraph `InvalidFilter` (`PG005`) instead of leaking raw PostgreSQL `42501` from internal catalog reads.
- Split public API execution context from source-table execution context. Catalog metadata lookup may require definer privileges; source-table reads/writes must remain invoker semantics.
- Gate or conditionally compile development-only tests so `_test_run_job_internal` is only called when the installed extension includes the `development` feature.
- Apply dependency freshness updates in their own focused change after the ACL/GUC behavior is green.

Architectural shape:

- The catalog module remains the single owner of graph metadata access.
- SQL facade functions should capture `current_role_oid()` at the boundary before entering any definer-only metadata helper.
- Helpers that enforce visibility should accept an explicit role OID rather than implicitly using `current_user` after privilege elevation.
- Mutable GQL writes should use a two-phase model: resolve graph/table mapping through controlled catalog access, then execute DML as the caller.

## 3. Proper Code Plan

1. Add targeted regression coverage first.
   - Keep the existing failing pg tests as the behavioral spec.
   - Add a focused SQLSTATE assertion around selected graph resolution under a role with `USAGE ON SCHEMA graph` but no direct `_graphs` privilege.
   - Add an assertion that GQL `CREATE`, `MERGE`, and hydrated reads still enforce source-table ACL/RLS after the metadata lookup succeeds.

2. Introduce an explicit caller-role metadata resolution path.
   - Add or standardize helpers such as `selected_or_default_graph_metadata_for_role(role_oid)` for every user-facing path that can run under restricted roles.
   - Audit all calls to `selected_or_default_graph_metadata()` from SQL facades, sync, jobs, GQL, hydration, runtime, and persistence.
   - Replace user-facing call sites with explicit-role variants where the function can be reached by non-admin users.

3. Add a controlled catalog access boundary.
   - Prefer `security_definer` only on small SQL-facing wrapper functions that need internal catalog reads.
   - Capture the caller role before privileged lookup and pass it into catalog visibility and graph privilege checks.
   - Do not mark source-table DML helpers as definer. If a definer wrapper is needed for metadata, hand off to invoker-context DML for source tables.

4. Normalize error mapping.
   - Ensure spoofed, missing, or invisible selected graph ids report `safety::GraphError::InvalidFilter` (`PG005`).
   - Reserve raw `42501` for actual source-table ACL/RLS failures when that is the PostgreSQL source-of-truth behavior under test.
   - Add regression assertions for both cases so the boundary stays clear.

5. Fix development helper gating.
   - Guard `pg_development_worker_entrypoints_restore_job_graph_context` with the same `development` feature condition as `_test_run_job_internal`, or move the helper call behind a runtime skip that detects the SQL function.
   - Verify the plain pg17 gate no longer expects a development-only function.
   - Verify the `pg17 development` gate still exercises the internal worker-entrypoint restoration path.

6. Run focused verification.
   - `cargo fmt --check`
   - `cargo clippy --features "pg17 development" --all-targets -- -D warnings`
   - `cargo test --features pg17`
   - `cargo pgrx test --features "pg17 development" named_graphs`
   - `cargo pgrx test --features "pg17 development" gql`
   - Plain pg17 pgrx/local matrix gate that previously failed.
   - `./tests/heavy/run_release_gate.sh`

7. Handle dependency freshness separately.
   - Update `bitvec`, `nixpkgs`, and `rust-overlay` in a separate dependency-refresh commit.
   - Re-run `python3 scripts/check_dependency_updates.py`.
   - Re-run the same release gates after the dependency update, because lockfile/toolchain changes can change compile or pgrx behavior.
