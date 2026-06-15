# pgGraph Named Graphs Progress

This file is the cross-session handoff for completing `todo/` in phase order.

## Current Checkpoint

- Active phase: Phase 2, Graph-Scoped Registration Catalogs.
- Status: Phase 2 complete and ready to commit after independent review fixes.
- Started: 2026-06-15.

## Phase Updates

- Phase 0: complete - pinned default-graph compatibility, recorded the single-graph audit, added named-graph policy vocabulary, and updated contributor docs.
- Phase 1: complete - added `graph._graphs`, bootstrapped the compatibility default graph, exposed graph metadata SQL functions, documented the public metadata surface, and verified catalog ACL/SQLSTATE behavior.
- Phase 2: complete - scoped registration catalogs by `graph_id`, kept selected/default graph compatibility wrappers, added explicit named-graph registration APIs, updated filter resolution, and documented graph-scoped registration.

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
- 2026-06-15: `cargo fmt --check` passed from `graph/`.
- 2026-06-15: `cargo test --features "pg17 development" graph_policy` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" query::` passed, 164 tests.
- 2026-06-15: `cargo pgrx test --features "pg17 development" graph_scoped_registrations_isolate_tables_edges_and_filters` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" selected_graph_legacy_registration_builds_and_queries_named_graph` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" registered_tables_and_edges_reflect_public_registration_apis` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" add_filter_column_rejects_non_numeric_columns` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" admin_remove_apis_update_catalog_side_effects` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" selected_graph_guc_cannot_expose_another_roles_graph` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" drop_graph_rejects_non_empty_graph_with_pggraph_sqlstate` passed, 1 test.
- 2026-06-15: `scripts/check_docs_drift.sh` passed.
- 2026-06-15: `cargo doc --features pg17 --no-deps` passed from `graph/`.
- 2026-06-15: `git diff --check` passed.
- 2026-06-15: `cargo test --features "pg17 development" graph_policy` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" query::` passed, 164 tests.
- 2026-06-15: `cargo doc --features pg17 --no-deps` passed from `graph/`.
- 2026-06-15: `git diff --check` passed.

## Working Notes

- Repository started clean on `dev`, already ahead of `origin/dev` by one commit.
- No package installation has been run. Use `sfw` only for dependency-adding package-manager commands.
- Do not push.
- Phase 1 review found no blocking issues. The one review adjustment made global/default graph selection visible to non-owner roles without loosening create, alter, drop, or direct catalog-write permissions.
- Phase 2 local review found no blocking issues before the required independent review.
- Independent review after three completed phases ran in subagent `019ecb1a-df0c-7a00-8257-ae3385df7a21` and found three issues: selected graph GUC spoofing, raw FK behavior for dropping non-empty graphs, and stale `set_current_graph()` docs. All three were fixed and covered by follow-up tests/docs.
- Next checkpoint: Phase 3 adds graph-aware discovery and subgraph definition APIs.
