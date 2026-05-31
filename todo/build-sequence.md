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

Status note, 2026-05-31: 2E is closed. Public `graph.gql()` accepts directed
single-edge mapped relationship `DELETE` on `mutable_overlay` when the
relationship is backed by a registered edge row table. The write deletes
exactly one PostgreSQL edge row first, then records transaction-local edge
tombstones; bidirectional registrations hide both neighbor directions and do
not cascade to endpoint nodes. Directed reverse matches on self-referential
bidirectional mappings delete the registered physical row when exactly one
orientation exists, while opposite physical rows are rejected as ambiguous.
SQL-visible coverage includes source-row delete, `csr_readonly` rejection,
forward and reverse neighbor reduction, no endpoint cascade, ambiguous
self-edge rejection, and rollback cleanup through `gql_delete_tx_lifecycle.sh`.

Status note, 2026-05-31: 2F is closed. `graph.status()` and
`graph.sync_health()` expose overlay tombstone count, estimated overlay memory,
durable compaction recommendation, and transaction-delta counters; overlay
dirty state is visible from existing `edge_buffer_used` plus `tx_delta_dirty`.
Mapped GQL writes enforce `graph.max_tx_delta_nodes`,
`graph.max_tx_delta_edges`, and `graph.max_overlay_memory_mb` before recording
transaction-local deltas; over-limit writes abort with `PG019` and leave source
tables unchanged for the failed statement. `graph.compaction_threshold` drives
operator-facing compaction recommendations for durable overlays, and
maintenance scheduling treats that recommendation as work to run.

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

Status note, 2026-05-31: 3A is closed for projection-stage `WITH`.
`graph.gql()` now parses `WITH` between the base `MATCH` and final `RETURN`,
and the binder carries a downstream scope chain for node variables,
relationship variables, and scalar property aliases. SQL-visible coverage
includes aliases crossing the `WITH` boundary, shadowing of an existing
variable name by a later projection, scalar property aliases through
`ORDER BY`, and negative tests proving hidden pre-`WITH` variables do not leak
into final `RETURN`. Post-`WITH MATCH` joins remain deferred to the later
multi-pattern read work, where the physical planner can add row-stream joins
instead of overloading the current single-pattern executor.

Status note, 2026-05-31: 3B is closed for top-level single-relationship
`OPTIONAL MATCH`. `graph.gql()` now parses `OPTIONAL MATCH`, binds optional
relationship reads into an explicit optional expand plan, and null-extends
unmatched source rows with JSON `null` target nodes, target properties, and
relationship values. Predicate misses on optional targets also null-extend the
source row, matching left-outer join behavior. SQL-visible coverage compares
optional GQL row counts and null target counts against equivalent PostgreSQL
left-outer SQL. Node-only optional matches and `WITH ... OPTIONAL MATCH`
multi-pattern joins remain deferred to later row-stream join planning.

Status note, 2026-05-31: 3C is closed for `RETURN` aggregates over the current
node-only and single-relationship row streams. `graph.gql()` now parses and
binds `count`, `sum`, `avg`, `min`, `max`, and `collect`, treats
non-aggregate `RETURN` items as implicit grouping keys, applies ordering and
pagination after aggregation, returns an empty aggregate group for aggregate-only
empty inputs, and preserves optional-match null-extension semantics for
aggregate counts. SQL-visible coverage compares grouping and numeric aggregate
results against equivalent PostgreSQL aggregation and covers empty-group/null
behavior. Aggregate `WITH` projections and aggregate arguments over future path
values remain deferred to later multi-stage row-stream planner work.

Status note, 2026-05-31: 3D is closed for bounded `DISTINCT` over the current
node-only and single-relationship row streams. `graph.gql()` supports
`RETURN DISTINCT`, `WITH DISTINCT` row-stream deduplication before later
projection/aggregation, and aggregate `DISTINCT` for `count`, `sum`, `avg`,
`min`, `max`, and `collect`. DISTINCT operations use the GQL result cap as the
unique-key working-set cap and abort rather than returning partial results when
that cap is exceeded. SQL-visible coverage compares `RETURN DISTINCT`,
`WITH DISTINCT`, aggregate `DISTINCT`, and `collect(DISTINCT ...)` against
equivalent PostgreSQL results.

Status note, 2026-05-31: 3E is closed for the current named-relationship path
model. Bounded relationship variables now project stable path objects with
`_path.nodes` and `_path.relationships`, and `nodes(path)`,
`relationships(path)`, and `length(path)` are supported in final `RETURN`
projections. SQL-visible coverage snapshots the raw path value and path
functions over a two-hop relationship pattern. Path-function `WITH`
projections, full standalone path-variable syntax, and aggregate path arguments
remain later multi-stage row-stream work.

Status note, 2026-05-31: 3F is closed for read-time JSONB property paths.
Registered table properties may include dotted paths rooted at a `jsonb` source
column, such as `profile.plan`, and `graph.gql()` can predicate, order, group,
aggregate, and return those values through hydration-time evaluation. JSONB
arrays and objects keep their JSON shape. Missing JSONB keys project as JSON
`null` but do not satisfy `IS NULL`; explicit JSON `null` does. Registration
rejects dotted paths whose base column is not `jsonb`. JSONB path writes remain
Phase 4 work.

Status note, 2026-05-31: 3G is closed for the internal typed SQL/PGQ adapter
seam. The adapter lives inside `graph/src/query/`, does not parse SQL text, and
lowers PostgreSQL-owned typed pattern shapes through the shared GQL AST and
binder path. The success corpus covers node-only reads, optional
single-relationship reads, projections, aliases, path functions, aggregates,
ordering, pagination, and `DISTINCT`. The rejection corpus covers out-of-matrix
optional node-only patterns and invalid relationship ranges. Public SQL/PGQ API
exposure remains deferred until PostgreSQL provides stable graph-pattern hooks.

## Phase 4 — Advanced writes + optional openCypher

| Slice | Depends on | Merge gate |
|---|---|---|
| 4A `REMOVE` property/label | 2F + 3F | typed + jsonb cases; idempotency |
| 4B `DETACH DELETE` (cascade policy) | 2E | incident-edge enumeration; ordering; partial-failure rollback |
| 4C `MERGE` (FOR UPDATE / ON CONFLICT) | 2C | two-session race on same key; ON CREATE/ON MATCH |
| 4D openCypher frontend (optional) | 3G stable IR | parser totality fuzz; unmappable rejection corpus; shared-IR equivalence; dual-surface SQLSTATE |

Status note, 2026-05-31: 4A is closed for single-node mapped property
`REMOVE`. `graph.gql()` parses `REMOVE n.property`, binds it through the same
single-node write path as `SET`, and executes it PostgreSQL-first against the
registered source row. Scalar mapped columns are set to SQL `NULL`; registered
dotted JSONB property paths remove the addressed key and missing-key removal is
idempotent. `REMOVE n:Label` is parsed and rejected because labels map to
registered source tables rather than mutable node tags.

Status note, 2026-05-31: 4B is closed for single-node `DETACH DELETE` over
registered edge-row relationships. `graph.gql()` parses
`DETACH DELETE n`, binds a node-only match, enumerates registered incident
edge-row tables, deletes matching forward and reverse edge rows first, then
deletes the matched PostgreSQL node row. The current backend records
transaction-local edge and node tombstones so subsequent reads in the same
transaction hide the deleted topology. Incident relationships not backed by
registered edge row tables are rejected until a safe generic policy exists for
endpoint-table foreign-key relationships.

Status note, 2026-05-31: 4C is closed for single-node mapped `MERGE`.
`graph.gql()` parses one node pattern with optional `ON CREATE SET` and
`ON MATCH SET` branches, binds the registered primary-key columns as the match
identity, locks existing rows with `FOR UPDATE`, inserts with
`ON CONFLICT DO NOTHING`, and retries the lock path after an insert race.
Branch expressions are lazy: `ON CREATE` parameters are required only when an
insert is attempted, and `ON MATCH` parameters are required only when an
existing row is matched. Coverage includes parser/binder tests, pgrx insert,
match, lazy-branch, mutable-overlay, identity, and delta-limit cases, plus
`gql_merge_race.sh` for a two-session same-key race.

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
