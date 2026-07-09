# Build Order

## Principle

Correctness, security, and hard resource containment precede syntax breadth.
Independent P0 tracks may run in parallel, but no full-GQL or performance claim
ships before their convergence gates.

## Checkpoint 0: Freeze And Measure The Baseline

- Create the machine-readable GQL conformance registry.
- Capture current SQL/API, SQLSTATE, artifact, and benchmark baselines.
- Add phase-level memory telemetry and production-shaped RSS profiles.
- Add exact reproductions for coordinate-only RLS, duplicate filter names,
  parallel relationships, concurrent publication, and failed replacement load.

**Exit:** each static P0 finding is reproduced or disproved by a permanent test;
baseline commands/results are archived.

## Checkpoint 1A: Security And Identity

- Enforce source-row visibility before all graph observability.
- Add stable relationship source identity and preserve parallel edges.
- Add stable filter graph/table/attribute identity.
- Add subtransaction delta frames and isolation-level contract tests.

**Exit:** security, multigraph, filter, rollback, and two-session gates are
green. Artifact migration/rebuild behavior for new identities is documented.

## Checkpoint 1B: Immediate Memory Containment

- Centralize resource policy and accounting.
- Remove read-only as over-budget construction fallback.
- Add fallible reservations and byte-based adaptive batches.
- Add load/query/compaction preflights and work breakers.
- Enforce RSS/spill thresholds in heavy tests.

**Exit:** known over-budget paths spill or fail deterministically before OOM;
status reports the resolved budget and peak phase.

## Checkpoint 1C: Safe Publication

- Replace process-local publication protection with graph-scoped cross-backend
  locking and generation CAS.
- Stage, fsync, mmap-validate, and catch up before current-manifest switch.
- Add reader pins, rollback retention, and safe garbage collection.

**Exit:** competing publishers, injected failure, crash, and old-reader tests
preserve exactly one valid current generation.

## Checkpoint 2: Artifact vNext And Out-Of-Core Build

- Implement coherent snapshot/watermark coordinator.
- Create bounded node, relationship, filter, resolution, inbound, and outbound
  runs.
- External-sort/merge with bounded fanout.
- Stream mmap-ready sections without a complete owned engine.
- Publish through the validated generation protocol.

**Exit:** a graph larger than the heap envelope builds under cgroup limits;
spill and in-memory results are equivalent; concurrent DML converges at W1.

## Checkpoint 3: Bounded Load, Sync, And Compaction

- mmap inbound CSR and filter/dictionary data.
- Pin one immutable projection snapshot per generation.
- Use immutable base plus node/edge/filter/resolution/tenant deltas.
- Compact by bounded source range.

**Exit:** load and maintenance respect the governor, no one-row mutation copies
the base, and repeated cycles have stable memory/files.

## Checkpoint 4: Refactor Foundations

- Split memory, build, projection, binder, evaluator, SQL facade, engine, and
  test modules in the order in the refactor plan.
- Establish typed values, graph identities, adapter traits, and canonical
  binding-table logical IR.
- Keep old execution behind equivalence tests while migrating.

**Exit:** dependency direction is acyclic, mega-facades are thin, and core
planning tests do not need SPI.

## Checkpoint 5: Streaming Costed Runtime

- Add graph/PostgreSQL statistics and cost/resource estimates.
- Implement lazy scans, streaming expansion, joins, batched visibility and
  hydration, spillable blocking operators, and iterative path state.
- Add actual-versus-estimated explain data and plan alternatives.

**Exit:** huge-label LIMIT, supernode, wide-row sort/group, and unreachable-path
tests stay bounded; differential plans agree.

## Checkpoint 6: Full GQL Vertical Slices

Implement conformance-registry slices in dependency order:

1. typed value, null/missing, coercion, expression, and diagnostic foundation;
2. composable binding tables and read clauses;
3. pattern and path language plus match/search modes;
4. grouping, set/bag, subquery, and ordering semantics;
5. standards-aligned PostgreSQL-first writes;
6. graph/session/catalog/transaction/privilege features or explicit delegation.

Each slice includes parser, binder, optimizer, executor, resource behavior,
security, transactions, tests, explain, and docs before the next slice.

**Exit:** every applicable requirement in the selected ISO GQL edition is green,
with no hand-maintained matrix drift or compatibility wording that overstates
support.

## Checkpoint 7: PostgreSQL 19 SQL/PGQ

- Rebase the typed adapter on canonical IR.
- Investigate native property-graph catalog import/interoperation.
- Add version-gated planner/catalog adapters and a separate conformance matrix.
- Compare native `GRAPH_TABLE` and pgGraph on identical source tables.

**Exit:** supported PG19 behavior is explicit, tested, and does not destabilize
PG14-18 frontends.

## Checkpoint 8: Competitive Engine Program

- Prepared plan caching with safe invalidation.
- Selective residency/eviction and page-cache-aware tuning.
- WAL/logical-decoding sync option.
- Quota-controlled analytics workers and additional algorithms.
- pgvector-guided expansion where semantics and benchmarks justify it.
- Published, repeatable performance scorecards and regression gates.

**Exit:** competitive claims are scoped to measured workloads and include
correctness, tails, memory, build/load, update lag, and operational cost.

## Checkpoint Discipline

At every checkpoint:

- update `progress.md`, conformance metadata, Roadmap, and Known Issues;
- run formatting, clippy, unit, affected pgrx, docs, and relevant heavy gates;
- record exact test and benchmark commands/results;
- make format/SQL compatibility and rollback impact explicit;
- commit coherent reviewable units using repository commit conventions;
- never push unless separately requested.

Do not close a checkpoint on code volume. Close it only when its exit evidence
is green and no required work is deferred under a vague later-phase label.
