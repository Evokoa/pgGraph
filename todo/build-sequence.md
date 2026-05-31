# Build Sequence: All Phases, In Order

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

The single ordered list of every implementation slice across all four phases,
with dependencies and the gate that lets each one merge. Build top to bottom.
Each slice is an independently mergeable, TDD'd PR. Stop anywhere and what you've
merged is still shippable.

Detail per slice lives in the phase design docs:
`phase-1-readonly-gql-design.md` … `phase-4-advanced-writes-opencypher-design.md`.

## Pre-flight (do first, ~30 min, read-only)

- **S0 — Catalog/hydration spike.** Confirm before writing the `CatalogSnapshot`
  trait: (a) `catalog::read::read_catalog()` is reachable from `graph/src/query/`
  (it is `pub(crate)`); (b) the table-name → `TableOid` resolution path the build
  uses; (c) `sql_hydration` can produce a coordinate without `to_jsonb` for
  `hydrate=false`. Output: confirm or adjust the trait shape in
  `phase-1-readonly-gql-design.md §5`.

## Phase 1 — Read-only GQL (immutable CSR; no new infra)

| Slice | Depends on | Merge gate |
|---|---|---|
| 1A Frontend foundation (docs reconcile + lexer + AST + parser + fuzz) | S0 | parser totality fuzz green; diagnostic snapshots |
| 1B Bind + plan + execute single directed MATCH (`#[cfg(development)]` gql fn) | 1A | result matches `graph.traverse()`; ACL denial; unknown-label negatives |
| 1C Predicates + RETURN shapes + hydration + ACL/RLS/tenant (DR-4) | 1B | generated matrix green; null-vs-missing; param errors |
| 1D ORDER BY/SKIP/LIMIT + undirected + bounded var-length | 1C | limit/zero/over-limit; undirected dedup; unbounded-`*` rejection |
| **G1 Public-exposure gate** | 1D + DR-5 | flip `graph.gql()` out of `development` only when 1A–1D **+ SQLSTATE taxonomy (DR-5) + docs positioning + matrix rows** all green; re-run `bfs_bench` vs `pre_gql_mutable_overlay` (zero regression) |

## Phase 2 — Mutable overlay + narrow writes (NEW: overlay + xact callbacks)

| Slice | Depends on | Merge gate |
|---|---|---|
| 2A NeighborSource refactor (route path_finder/components/search or reject dirty) | G1 | clean-overlay ≡ CSR proptest; clean overlay ≈ `csr_readonly` bench |
| 2B Overlay storage + tx/subxact callbacks + `build(mode:=)` + read-your-own-writes | 2A | rollback discards; concurrent isolation; out-of-band sync catch-up; crash/reload rebuilds |
| 2C `CREATE` mapped node (SPI-first + delta) | 2B | read-your-own-writes; ACL/RLS/tenant; edge-type ceiling; unregistered-label reject |
| 2D `SET` mapped property | 2C | typed-only; type-mismatch reject; filter-index delta visible |
| 2E `DELETE` mapped edge | 2C | tombstone reduces neighbors; reverse consistency; no cascade |
| 2F Compaction + observability + memory limits (DR-2) | 2D, 2E | compaction equivalence (CSR+overlay ≡ rebuilt); statement-scoped abort; status row shape |

Status note, 2026-05-31: 2A is implemented in the working branch. Clean
overlay equivalence is covered by a `NeighborSource` property test, dirty
unweighted paths/components route through overlay neighbors, weighted paths
reject dirty edge overlays with `PG018`, and the clean-overlay benchmark is
within noise of the pre-2A baseline.

Status note, 2026-05-31: 2B is closed. Projection-mode GUCs,
`graph.build(mode := ...)`, queued-build mode persistence, status/sync-health
mode columns, transaction-delta callbacks, and internal transaction-local edge
overlay reads are in place. Read-only GQL pattern expansion consumes the same
overlay-aware neighbor path, with SQL-visible coverage for internal transaction
edge inserts and deletes. `tx_delta_lifecycle.sh` proves commit cleanup,
rollback discard, concurrent backend isolation, and trigger-sync catch-up for
the internal transaction edge overlay path. `tx_delta_crash_recovery.sh` proves
uncommitted transaction edge overlays are ignored after postmaster crash/reload
while the persisted base graph reloads. Public mapped `CREATE` now records node
deltas through the public GQL write path.

Status note, 2026-05-31: 2C is closed. Public `graph.gql()` accepts
single-node mapped `CREATE` on `mutable_overlay`, performs a PostgreSQL-first
`INSERT ... RETURNING`, returns the inserted row, rejects `csr_readonly`, and
records a transaction-local added-node delta. Node-only `MATCH` reads merge the
active transaction's added-node keys so a transaction can read its own newly
created isolated nodes. SQL-visible coverage includes read-only projection
rejection, session-tenant insertion, source-table RLS preservation, explicit
rollback/commit lifecycle behavior, node-delta visibility, and
unregistered-label rejection.

Status note, 2026-05-31: 2D is closed. Public `graph.gql()` accepts
single-node mapped property `SET` on `mutable_overlay`, performs a
PostgreSQL-first `UPDATE ... RETURNING`, returns the updated row, rejects
`csr_readonly`, and rejects non-writable mapped columns such as primary keys
and tenant columns. SQL-visible coverage includes source-row update,
PostgreSQL type-mismatch rejection, tenant-column rejection, same-backend typed
filter-index visibility after the update, and rollback cleanup of the
transaction-local filter delta.

## Phase 3 — Advanced reads + SQL/PGQ adapter

| Slice | Depends on | Merge gate |
|---|---|---|
| 3A `WITH` + binder scope chain (enabling change — build first) | G1 (reads only; can parallel Phase 2) | cross-stage visibility; shadowing; scope-leak negatives |
| 3B `OPTIONAL MATCH` (null-extension) | 3A | results vs left-outer SQL |
| 3C Aggregates (`sum`/`avg`/`min`/`max`/`collect`; grouping) | 3A | correctness vs SQL aggregation; empty-group; nulls |
| 3D `DISTINCT` (memory-limited) | 3A | dedup correctness; over-limit abort |
| 3E Path functions (`nodes`/`relationships`/`length`) | 3A | path value-shape snapshots |
| 3F jsonb dynamic/list/map properties + missing-vs-null rule | 3C | jsonb predicate + return; type-mapping negatives |
| 3G SQL/PGQ adapter into shared IR | 3A–3F stable IR | success + rejection corpus; own compatibility matrix |

## Phase 4 — Advanced writes + optional openCypher

| Slice | Depends on | Merge gate |
|---|---|---|
| 4A `REMOVE` property/label | 2F + 3F | typed + jsonb cases; idempotency |
| 4B `DETACH DELETE` (cascade policy) | 2E | incident-edge enumeration; ordering; partial-failure rollback |
| 4C `MERGE` (FOR UPDATE / ON CONFLICT) | 2C | two-session race on same key; ON CREATE/ON MATCH |
| 4D openCypher frontend (optional) | 3G stable IR | parser totality fuzz; unmappable rejection corpus; shared-IR equivalence; dual-surface SQLSTATE |

## Cross-cutting, every slice

- TDD: failing test first, then code (rust-planning rule 34a).
- pgrx-free parser/binder/IR; SPI/pgrx only in `execute`, catalog `load()`,
  `sql_facade`. No `Result<_, String>` in cross-layer APIs. No new unsafe.
- Docs contract gate before any public SQL surface; update the compatibility
  matrix row before calling a feature supported.
- Benchmark gate: existing CSR traversal must not regress; record vs
  `pre_gql_mutable_overlay`.

## Realistic note

This is the full dependency graph, not a one-night scope. A focused night
realistically lands **S0 → 1A → 1B**, possibly into **1C**. Phase 2's xact
callbacks and Phase 3's scope chain are each substantial; Phase 4 is optional and
furthest out. Build in this order and every merge point is a shippable
increment.
