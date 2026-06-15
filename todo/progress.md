# pgGraph Named Graphs Progress

This file is the cross-session handoff for completing `todo/` in phase order.

## Current Checkpoint

- Active phase: Phase 2, Graph-Scoped Registration Catalogs.
- Status: Phase 1 complete and committed; Phase 2 ready to start.
- Started: 2026-06-15.

## Phase Updates

- Phase 0: complete - pinned default-graph compatibility, recorded the single-graph audit, added named-graph policy vocabulary, and updated contributor docs.
- Phase 1: complete - added `graph._graphs`, bootstrapped the compatibility default graph, exposed graph metadata SQL functions, documented the public metadata surface, and verified catalog ACL/SQLSTATE behavior.

## Verification Log

- 2026-06-15: `cargo fmt --check` passed.
- 2026-06-15: `cargo test --features "pg17 development" graph_policy` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" query::` passed, 164 tests.
- 2026-06-15: `cargo pgrx test --features "pg17 development" default_graph_compatibility_workflow_still_uses_legacy_sql_surface` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" named_graph_policy_defaults_are_single_sourced` passed, 1 test.
- 2026-06-15: `scripts/check_docs_drift.sh` passed.
- 2026-06-15: `cargo doc --features pg17 --no-deps` passed.
- 2026-06-15: `cargo fmt --check` passed from `graph/`.
- 2026-06-15: `cargo test --features "pg17 development" graph_policy` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" query::` passed, 164 tests.
- 2026-06-15: `cargo pgrx test --features "pg17 development" default_graph_catalog_row_is_bootstrapped_once` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" create_graph_enforces_identity_and_policy_values` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" current_graph_selection_is_separate_from_engine_load_state` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" graph_catalog_mutation_requires_admin_privileges` passed, 1 test.
- 2026-06-15: `scripts/check_docs_drift.sh` passed.
- 2026-06-15: `cargo doc --features pg17 --no-deps` passed from `graph/`.
- 2026-06-15: `git diff --check` passed.

## Working Notes

- Repository started clean on `dev`, already ahead of `origin/dev` by one commit.
- No package installation has been run. Use `sfw` only for dependency-adding package-manager commands.
- Do not push.
- Phase 1 review found no blocking issues. The one review adjustment made global/default graph selection visible to non-owner roles without loosening create, alter, drop, or direct catalog-write permissions.
- Next checkpoint: Phase 2 scopes registered tables, edges, and filter columns by graph id while keeping default-graph compatibility wrappers.
