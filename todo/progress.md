# pgGraph Named Graphs Progress

This file is the cross-session handoff for completing `todo/` in phase order.

## Current Checkpoint

- Active phase: Phase 1, Graph Catalog Foundation.
- Status: ready to start after Phase 0 commit.
- Started: 2026-06-15.

## Phase Updates

- Phase 0: complete - pinned default-graph compatibility, recorded the single-graph audit, added named-graph policy vocabulary, and updated contributor docs.

## Verification Log

- 2026-06-15: `cargo fmt --check` passed.
- 2026-06-15: `cargo test --features "pg17 development" graph_policy` passed, 4 tests.
- 2026-06-15: `cargo test --features "pg17 development" query::` passed, 164 tests.
- 2026-06-15: `cargo pgrx test --features "pg17 development" default_graph_compatibility_workflow_still_uses_legacy_sql_surface` passed, 1 test.
- 2026-06-15: `cargo pgrx test --features "pg17 development" named_graph_policy_defaults_are_single_sourced` passed, 1 test.
- 2026-06-15: `scripts/check_docs_drift.sh` passed.
- 2026-06-15: `cargo doc --features pg17 --no-deps` passed.

## Working Notes

- Repository started clean on `dev`, already ahead of `origin/dev` by one commit.
- No package installation has been run. Use `sfw` only for dependency-adding package-manager commands.
- Do not push.
- Next checkpoint: Phase 1 creates `graph._graphs`, default graph bootstrap/backfill, graph metadata catalog helpers, SQL facades, SQLSTATE/ACL tests, and docs for the default/named graph lifecycle.
