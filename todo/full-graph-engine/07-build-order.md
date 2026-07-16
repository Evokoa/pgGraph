# Build Order

> **Archived 2026-07-16:** The release-scoped checkpoints are complete. This
> order is retained for historical context; later engine work is owned only by
> the public roadmap.

This is the long-term engine build order. The
[pgGraph 1.0 release program](../v1-release/README.md) owns release scope:
checkpoints 0 through 3 are production blockers, checkpoint 4 and checkpoint 5
are required only where they close a demonstrated 1.0 risk, checkpoint 6 is
limited to the advertised 1.0 GQL profile, and checkpoints 7, 8, and 10 are
post-1.0 roadmap work. Checkpoint 9 supplies the 1.0 documentation, packaging,
operations, and release-evidence closure.

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
- Add exact reproductions for out-of-range mapped access, malformed CSR
  offsets, error-boundary unwinding, durable filter-value type loss,
  security-definer search-path shadowing, and relation rename/search-path
  identity.
- Freeze new unsafe/raw-FFI locations until RUST-00A through RUST-00F in the
  [Rust boundary plan](./10-rust-type-safety-pgrx-boundaries.md) close.

**Exit:** each static P0 finding is reproduced or disproved by a permanent test;
baseline commands/results are archived; safe mapped APIs cannot reach UB and
PostgreSQL errors unwind through the supported pgrx guard.

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

- Split memory, build, projection, binder, evaluator, SQL facade, engine,
  persistence, sync, and test modules in the order in the refactor plan.
- Complete RUST-2, RUST-3, and RUST-6 from the
  [Rust boundary plan](./10-rust-type-safety-pgrx-boundaries.md): canonical
  enums, production identity/unit types, pgrx adapters, and unsafe isolation.
- Establish typed values, graph identities, adapter traits, and canonical
  binding-table logical IR; raw strings/integers convert once at boundaries.
- Keep old execution behind equivalence tests while migrating.

**Exit:** dependency direction is acyclic, mega-facades are thin, and core
planning tests do not need SPI. Closed state is not stringly, unsafe is
allowlisted, and compile-fail tests prevent cross-domain ID/slot mixups.

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
The first slice implements RUST-4's exact `GraphValue`; JSON/`f64` is not the
semantic value layer.

**Exit:** every applicable requirement in the selected ISO GQL edition is green,
with no hand-maintained matrix drift or compatibility wording that overstates
support.

## Checkpoint 7: PostgreSQL 19 SQL/PGQ

- Add the `pg19 = ["pgrx/pg19", "pgrx-tests/pg19"]` crate feature and
  experimental build/install/test lane available in pinned pgrx 0.19.1;
  promote it to supported only after the PG19 release/package matrix is green.
- Complete [PG19-0 through PG19-5](./08-postgresql-19-property-graphs.md):
  toolchain, native catalog adapter, attach/build/invalidation, query
  interoperation, security semantics, and release support.
- Consume native `CREATE PROPERTY GRAPH` mappings without duplicate manual
  table/edge registration.
- Rebase the typed SQL/PGQ adapter on canonical IR and maintain a separate
  conformance registry.
- Differential-test native `GRAPH_TABLE` and pgGraph on identical source data.
- Claim transparent `GRAPH_TABLE` acceleration only if PostgreSQL exposes a
  stable supported integration hook.

**Exit:** the PG19 plan's overall definition of done is green, the two public
PG19/SQL-PGQ rows graduate to Current Baseline, KI-007 retires with release
evidence, and PG14-18 remain green.

## Checkpoint 8: Competitive Engine Program

- Prepared plan caching with safe invalidation.
- Selective residency/eviction and page-cache-aware tuning.
- WAL/logical-decoding sync option.
- Quota-controlled analytics workers and additional algorithms.
- pgvector-guided expansion where semantics and benchmarks justify it.
- Published, repeatable performance scorecards and regression gates.

**Exit:** competitive claims are scoped to measured workloads and include
correctness, tails, memory, build/load, update lag, and operational cost.

## Checkpoint 9: Clear The Current Public Backlog

- Close every 1.0 row in
  [the closure ledger](./09-public-backlog-closure.md) and assign every
  undelivered direction to the post-1.0 roadmap.
- Move `RESOLVED-RETIRE` items out of active Known Issues after release
  regressions and release-note evidence are confirmed.
- Graduate delivered roadmap work into Current Baseline, then remove its
  planned/reserved row.
- Run the Roadmap/KI drift inventory and archive exact evidence.

**Exit:** the ledger's “Public Backlog Clear” gate is green. Every existing
Roadmap item is delivered, rejected, or trigger-deferred outside active work;
Known Issues contains only newly discovered unresolved defects, if any.

## Checkpoint 10: Schema-Flexible Dynamic Graphs (Late)

This checkpoint does not alter checkpoints 0 through 9 or the current public
backlog-closure gate. After checkpoint 9, complete
[DYN-0 through DYN-6](./11-schema-flexible-dynamic-graphs.md):

- data-driven node and relationship labels from typed source columns;
- evolving top-level JSONB properties without duplicating source documents;
- bounded label dictionaries, membership, sync, and selected property indexes;
- exact `GraphValue` conversion, multi-source label scans, batched visibility/
  hydration, PostgreSQL JSONB pushdown, and PostgreSQL-first writes;
- explicit pgGraph-extension semantics where PostgreSQL 19 native property
  graphs do not expose equivalent dynamic clauses.

**Exit:** dynamic label/property mappings satisfy their full security,
transaction, memory/spill, rebuild-versus-sync, PostgreSQL-version, explain,
operations, and performance gates without regressing declared/static graphs.

## Checkpoint Discipline

At every checkpoint:

- update `progress.md`, conformance metadata, Roadmap, and Known Issues;
- update [the public backlog closure ledger](./09-public-backlog-closure.md) and
  graduate, retire, reject, or trigger-defer the completed public row;
- run formatting, clippy, unit, affected pgrx, docs, and relevant heavy gates;
- run the enum/type drift checks, unsafe/raw-FFI allowlist, checked-cast audit,
  and affected Miri/sanitizer/fuzz gates from the Rust boundary plan;
- record exact test and benchmark commands/results;
- make format/SQL compatibility and rollback impact explicit;
- commit coherent reviewable units using repository commit conventions;
- never push unless separately requested.

Do not close a checkpoint on code volume. Close it only when its exit evidence
is green and no required work is deferred under a vague later-phase label.
