# Phase 0 Audit: Current Single-Graph Baseline

Phase 0 records the current global graph assumptions before catalog and runtime
APIs become graph-scoped. This note is a handoff artifact for implementation;
public behavior changes belong in `docs/`.

## Architecture Decision Closure

- Keep the existing single Rust pgrx crate and use focused module boundaries.
- Keep SQL functions as thin facades over catalog, build, sync, runtime, and
  projection modules.
- Model future automated work as durable SQL-visible jobs, not hidden
  fire-and-forget Rust tasks.
- Keep PostgreSQL source tables authoritative for graph writes.
- Keep graph failure states typed, SQLSTATE-mapped, and queryable through
  status APIs.
- Use PostgreSQL `uuid` in catalogs. Rust uses an explicit `GraphId` newtype
  around canonical UUID text until the schema exists and proves whether a UUID
  dependency is justified.

## Global Assumption Inventory

| Assumption | Current location | Phase that owns change |
|---|---|---|
| Registered table, edge, and filter catalogs have no `graph_id`. | `graph/sql/bootstrap.sql`, `graph/src/catalog/read.rs`, `graph/src/catalog/write.rs`, `graph/src/catalog/validate.rs` | Phase 2 |
| `read_catalog()` reads one global catalog. | `graph/src/catalog/read.rs`, callers in build, sync, status, and query setup | Phase 2 |
| One backend-local engine represents the active graph. | `graph/src/lib.rs`, `graph/src/sql_facade/runtime.rs`, `ENGINE.with` call sites | Phase 6 |
| Runtime `ensure_current_graph()` means built/loaded schema state, not named graph selection. | `graph/src/sql_facade/runtime.rs` | Phase 6 |
| Build jobs and maintenance jobs are global queues. | `graph/sql/bootstrap.sql`, `graph/src/sql_jobs.rs`, `graph/src/sql_build.rs` | Phase 4 |
| Build advisory locks serialize the global graph. | `graph/src/sql_build.rs`, heavy build-lock tests | Phase 4 |
| Artifact path is `$PGDATA/<graph.data_dir>/main.pggraph`. | `graph/src/persistence.rs`, load/reset/status tests | Phase 5 |
| Sync log and sync buffer are global. | `graph/sql/bootstrap.sql`, `graph/src/sync.rs`, `graph/src/sql_sync.rs` | Phase 9 |
| Projection generations are global. | `graph/sql/bootstrap.sql`, `graph/src/projection/*`, maintenance/admin tests | Phase 5 |
| `graph.reset()` clears global engine state and global artifact files. | `graph/src/sql_facade/runtime.rs` | Phase 5 and Phase 6 |
| `graph.auto_discover()` mutates the global registration catalog. | `graph/src/discover.rs`, `graph/src/sql_facade/discovery.rs` | Phase 3 |

## Policy And Observability Inventory

| Policy or status fact | Current location | Phase that owns change |
|---|---|---|
| Default graph name, namespace, graph kinds, residency, materialization, projection modes | `graph/src/graph_policy.rs` | Phase 1 |
| Build batch size | `graph/src/config.rs` (`graph.build_batch_size`) | Phase 0, later Phase 10 quota checks |
| Sync replay batch size | `graph/src/config.rs` (`graph.sync_batch_size`) | Phase 9 |
| Edge buffer size and read-only threshold | `graph/src/config.rs`, `graph/src/engine.rs` | Phase 9 and Phase 10 |
| Residency/load limits | `graph/src/graph_policy.rs` starts `DEFAULT_BACKEND_LOADED_GRAPH_LIMIT` | Phase 6 and Phase 8 |
| Scheduler wake interval, batch size, retry attempts | `graph/src/graph_policy.rs` | Phase 7 and Phase 9 |
| Job status values | `graph/sql/bootstrap.sql`, `graph/src/graph_policy.rs`, `graph/src/sql_jobs.rs` | Phase 4 and Phase 7 |
| Projection validation status values | `graph/sql/bootstrap.sql`, `graph/src/projection/manifest.rs`, `graph/src/projection/status.rs` | Phase 5 |
| SQLSTATE mapping | `graph/src/safety.rs`, heavy SQLSTATE/ACL tests | Every phase that adds errors |
| GUC defaults and ranges | `graph/src/config.rs`, `docs/user_guide/configuration.mdx` | Phase-specific updates |

## Regression Samples

- `graph/src/pg_tests/named_graphs.rs` pins the current legacy/default SQL
  workflow: register table and edge, build, traverse, inspect status, reset,
  and receive `PG003` after reset.
- `graph/src/pg_tests/named_graphs.rs` also checks the named-graph policy
  constants that Phase 1 will use for catalog checks.

## Phase Assignment Closure

The remaining named-graph plan assigns all pre-planning items through Phase 16.
No pre-planning item is intentionally rejected in Phase 0. Rejections are
phase-local where the plan calls for an implement-or-reject decision, such as
arbitrary row-predicate subgraphs in Phase 3 and unsafe GQL-driven schema
creation in Phase 16.
