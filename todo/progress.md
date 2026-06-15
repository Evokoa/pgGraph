# pgGraph Named Graphs Progress

This file is the cross-session handoff for completing `todo/` in phase order.

## Current Checkpoint

- Active phase: Phase 5, Graph-Scoped Persistence and Projection Generations.
- Status: Phase 5 reviewed; checkpoint commit pending.
- Started: 2026-06-15.

## Phase Updates

- Phase 0: complete - pinned default-graph compatibility, recorded the single-graph audit, added named-graph policy vocabulary, and updated contributor docs.
- Phase 1: complete - added `graph._graphs`, bootstrapped the compatibility default graph, exposed graph metadata SQL functions, documented the public metadata surface, and verified catalog ACL/SQLSTATE behavior.
- Phase 2: complete - scoped registration catalogs by `graph_id`, kept selected/default graph compatibility wrappers, added explicit named-graph registration APIs, updated filter resolution, and documented graph-scoped registration.
- Phase 3: complete - made auto-discovery graph-aware, added write-free preview APIs, supported targeted named-graph discovery/build flows, and explicitly rejected arbitrary row-predicate subgraphs.
- Phase 4: complete - scoped build and maintenance jobs by `graph_id`, restored graph context in workers, made build/vacuum advisory locks graph-specific, added named graph build/maintenance/status helpers, and documented graph-scoped job behavior.
- Phase 5: complete - moved artifacts under per-graph UUID roots, scoped projection generation heartbeats by `graph_id`, made reset/drop cleanup graph-root-local, kept `_PG_init()` catalog-free for fresh installs, and documented the new persistence layout.

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
- 2026-06-15: `cargo test --features "pg17 development" graph_file_path` passed, 4 tests.
- 2026-06-15: `git diff --check` passed.
- 2026-06-15: `cargo fmt --check` passed from `graph/`.
- 2026-06-15: `cargo test --features "pg17 development" build_lock_query` passed, 2 tests.
- 2026-06-15: `cargo test --features "pg17 development" worker_metadata_round_trips_json_with_delimiters` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" build_graph_uses_named_graph_catalog` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" durable_jobs_are_attributed_to_selected_graph` passed, 1 test.
- 2026-06-15: `cargo test --features "pg17 development" graph_policy` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" query::` passed, 164 tests.
- 2026-06-15: `scripts/check_docs_drift.sh` passed.
- 2026-06-15: `git diff --check` passed.
- 2026-06-15: `cargo doc --features pg17 --no-deps` passed from `graph/`.
- 2026-06-15: `cargo clippy --features "pg17 development" -- -D warnings` remains red on existing SQL facade `type_complexity` warnings; the unrelated `graph_policy` `manual_contains` lint was fixed.
- 2026-06-15: `cargo fmt --check` passed from `graph/`.
- 2026-06-15: `cargo test --features "pg17 development" graph_policy` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" build_lock_query` passed, 2 tests.
- 2026-06-15: `git diff --check` passed.
- 2026-06-15: `cargo fmt --check` passed from `graph/`.
- 2026-06-15: `cargo test --features "pg17 development" graph_file_path` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" projection_manifest` passed, 16 tests.
- 2026-06-15: `cargo pgrx test --features "pg17 development" persisted_named_graphs_use_distinct_artifact_roots` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" projection_generation_heartbeats_are_graph_scoped` passed, 1 test.
- 2026-06-15: `cargo test --features "pg17 development" graph_policy` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" query::` passed, 164 tests.
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
- 2026-06-15: `cargo fmt --check` passed from `graph/`.
- 2026-06-15: `cargo pgrx test --features "pg17 development" preview_discover_tables_writes_no_registration_rows` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" auto_discover_tables_into_named_graph_does_not_mutate_default_graph` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" auto_discover_tables_builds_target_named_graph` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" row_predicate_subgraphs_are_explicitly_rejected` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" auto_discover_tables_registers_only_selected_tables_and_edges` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" auto_discover_tables_discovers_fk_edges_inside_selected_set` passed, 1 test.
- 2026-06-15: `scripts/check_docs_drift.sh` passed.
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
- Phase 3 review found no blocking issues. Discovery writes now route through explicit graph ids; preview APIs return discovery rows without registration writes; row-predicate subgraphs return `PG018`.
- Phase 4 local review found no blocking issue in graph context restoration, graph-scoped locks, job migration, SQL overload compatibility, or docs/API drift. Strict clippy still reports existing SQL ABI row type-complexity warnings outside the Phase 4 scope.
- Phase 5 local review found no blocking issue in fresh-install safety, selected graph path resolution, graph-local reset/drop cleanup, or projection heartbeat scoping. One path comparison hardening fix was made and covered by focused path tests.
- Next checkpoint: Phase 6 introduces backend runtime selection and an engine registry.
