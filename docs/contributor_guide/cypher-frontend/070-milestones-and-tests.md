# Milestones and tests

Six milestones; each shippable on its own, each guarded by tests.
"Done" means: passing tests, doc page updated, all new public SQL
listed in `graph.control`-style migration.

## M0 — Skeleton & catalog (week 1)

**Goal:** the new module compiles, the catalog tables exist, the
`SchemaProvider` impl is wired but nothing executes yet.

Scope:

- Add cyrs deps to `graph/Cargo.toml`. Pin a single git revision /
  crates.io minor.
- `cypher_facade/mod.rs` with stub `execute()` returning "not yet
  implemented".
- `cypher_facade/schema_provider.rs` implementing `SchemaProvider`
  over a fixed catalog snapshot. Methods that depend on upstream
  asks (`label_unique_props`, `labels_compatible`) return default
  values until cyrs ships them.
- `sql/cypher_catalog.sql` migration creating the eight catalog
  tables from `020-catalog-extensions.md`.
- Five new pgrx SQL functions: `register_label`,
  `register_label_property`, `register_rel_type`,
  `register_unique`, `allow_label_set`.
- `register_unique` validates that a matching Postgres unique
  constraint actually exists on the underlying table.

Tests:

- Unit tests on the `SchemaProvider` impl with a hand-built
  `CatalogSnapshot`.
- pg_test: register a label, query the catalog, assert presence.
- Negative pg_test: `register_unique` rejects when no real
  constraint exists.

## M1 — Read MVP (weeks 2–3)

**Goal:** `MATCH (a:L) WHERE a.x = $1 RETURN a.y, a.z` works for a
real registered table, with parameter binding.

Scope:

- `cypher_facade/plan_translator/read.rs` implementing `Source`,
  `Filter` (push-only, scalar equalities and conjunctions), `Project`
  (push-only).
- `Skip`, `Limit`, `Distinct` (SQL push).
- `param_bind.rs` for scalar param types (`String`, `Int`, `Float`,
  `Bool`).
- `diag_to_pg.rs` for parse/sema/embedder errors.
- New SQL function: `graph.cypher(text, jsonb) RETURNS TABLE(row jsonb)`.

Tests:

- TCK subset: every scenario tagged `@MATCH` + `@WHERE` + `@RETURN`
  that doesn't reference variable-length patterns or aggregations.
  Skip / xfail anything that does.
- UI tests for diagnostics: unregistered label, unregistered property,
  bad param type.
- pg_test: round-trip insert via SQL → query via `graph.cypher` →
  assert rows.

## M2 — Traversal & path ops (week 4)

**Goal:** multi-hop and variable-length patterns work.

Scope:

- `Expand` (single-hop) via `engine::Engine::adjacent`.
- `Expand` (varlen) via existing `execute_traverse_rows` +
  `TraverseRequest` builder.
- `OptionalJoin` (lateral form when SQL-pushed; explicit form when
  row-evaled).
- `OrderBy` (push for homogeneous keys; row-eval otherwise).
- `Aggregate` (push for `count/sum/avg/min/max`; row-eval `collect`).
- `Unwind` (push for arrays; row-eval for JSONB lists).
- `Union` (both kinds, with the `Distinct` null-handling caveat).
- `With` (scope barrier; same emission as `Project` + optional
  `Filter`).

Tests:

- TCK subset adds `@OPTIONAL-MATCH`, `@UNWIND`, `@AGGREGATIONS`,
  `@WITH`, `@PATTERNS` (fixed-length), `@LISTS` (non-comprehension),
  `@MAPS`.
- Engine integration tests using existing pgGraph fixtures (the demo
  `people` ↔ `orders` schema in `demo/`).

## M3 — Write MVP (weeks 5–6)

**Goal:** `CREATE`, `SET`, `REMOVE`, `DELETE` all work against
registered tables.

Scope:

- `cypher_facade/plan_translator/write.rs`.
- `CreateNode` (single label only; multi-label requires the catalog
  hook from cyrs §2.3).
- `CreateRel` (both FK-column and junction-table cases).
- `SetProperty`, `RemoveProperty`.
- `Delete` (no detach).
- `Delete { detach: true }` walking incident edges via the catalog.
- Param binding extended for date/datetime, list, map.

Tests:

- TCK subset adds `@CREATE`, `@SET`, `@REMOVE`, `@DELETE`.
- pg_test verifies sync triggers fire after each write op so the
  index sees the change on a subsequent statement.
- Negative test for `E4520` (read-after-write within a single
  statement).
- Negative test for FK-violation when deleting without `DETACH`.

## M4 — MERGE & uniqueness (week 7)

**Goal:** `MERGE` works atomically.

Scope:

- `MergeNode` via `INSERT ... ON CONFLICT (cols) DO UPDATE …
  RETURNING …, (xmax = 0)`.
- `MergeRel` same shape on junction tables.
- `on_create` / `on_match` dispatch over the `xmax = 0` flag.
- Sema gate: `MergeNode` whose key props don't match a registered
  uniqueness tuple → `E4504`.
- Requires cyrs feat-request §2.1 / §2.2 to be in place; until then
  use a temporary embedder-side analysis on `MergeNode.props` to
  extract the candidate key.

Tests:

- TCK subset adds `@MERGE`.
- Concurrent MERGE test using two backends; asserts no duplicates
  and exactly one `on_create` branch fires.

## M5 — Shortest path, hardening, TCK gate (week 8+)

**Goal:** `shortestPath` works (blocked on cyrs §1.1); golden TCK
subset becomes a CI gate.

Scope:

- `ShortestPath` op → `path_finder.rs`. Lands when cyrs ships the
  operator.
- All "deferred-to-M5" items from earlier milestones.
- Function-coverage table cross-checked against
  `cyrs_schema::StandardLibrary` (CI test).
- Perf budget: a representative MATCH query compiles + executes in
  < N ms; document N once we measure baseline.
- Wire the openCypher TCK subset (the green-tag list from spec §17.5)
  as a CI gate in pgGraph. Failures block merge.

Tests:

- Full TCK subset run on every PR.
- Mutation-testing run on the facade module weekly (matches cyrs's
  own §17.8 cadence).

## What goes in `graph/tests/`

```
graph/tests/
├── ui/cypher/                       # diagnostic fixtures (see 060)
├── integration/cypher/              # pg_test cases
│   ├── basic_match.rs
│   ├── varlen_traverse.rs
│   ├── merge_atomicity.rs
│   ├── detach_delete.rs
│   └── ...
└── tck/                             # openCypher TCK subset
    ├── README.md
    ├── corpus -> ../../cyrs-tck-corpus/   # vendored or submodule
    └── runner.rs
```

The TCK runner re-uses cyrs's parser/sema for input validation but
runs the query through `graph.cypher()` in a real Postgres backend
spun up via pgrx-tests. We don't depend on `cyrs-tck` the crate; we
consume the corpus directly.

## Performance baselines

Establish before M5:

- Median compile time (parse → HIR → sema → plan) on a 200-character
  Cypher query: target < 5 ms.
- Median execute time for "2-hop pattern, equality WHERE on indexed
  column, RETURN 10 rows" on the demo schema: target < 10 ms.
- Allocation budget: < 200 KB per invocation steady-state.

These are sanity caps, not products. If we miss them by 2×, dig in.
If we miss by 10×, we have a design problem.

## Out of scope for v1 (tracked elsewhere)

- `CALL { }` subqueries, `EXISTS { }` subqueries.
- Temporal beyond `date`/`datetime`.
- Spatial.
- Read-after-write within a single statement (M3 ships rejection,
  not support).
- Streaming results larger than `work_mem` — TableIterator
  materialises.
- Per-tenant catalog overlays.
