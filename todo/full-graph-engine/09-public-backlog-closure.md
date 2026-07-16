# Public Roadmap And Known-Issues Closure Ledger

> **Scope:** This ledger is the long-term public-roadmap accountability map,
> not the pgGraph 1.0 release checklist. The
> [1.0 release program](../v1-release/README.md) owns R0-R7. Rows marked
> `POST-1.0` are intentionally outside that release.

## Purpose

This is the accountability map for clearing the current public Roadmap and
Known Issues. Every public item must have exactly one disposition, one owning
TODO/checkpoint, and objective evidence. A public row may not remain open only
because it says “future”, “consider”, or “measure later”.

Inventory snapshot for 2026-07-09:

- 6 Open Source V1 rows;
- 10 Near-Term Roadmap rows;
- 14 Reserved/Future rows;
- 2 substantive future-work sections outside the tables;
- 21 active Known Issues;
- 4 resolved Known Issues awaiting retirement.

## Status And Disposition

| Value | Meaning |
|---|---|
| `OPEN` | Work or its required evidence is incomplete. |
| `CONDITIONAL` | A dated/evidence-based decision must implement or reject it. |
| `RESOLVED-RETIRE` | Behavior is shipped; verify release evidence and remove it from active Known Issues. |
| `DEFER-TRIGGER` | Remove it from the active roadmap and record an explicit revisit trigger/non-goal. |
| `CLOSED` | Evidence is linked and public docs have been updated. |
| `POST-1.0` | Active roadmap work that does not block the 1.0 release. |

Allowed final dispositions:

1. **Graduate:** move delivered capability into Roadmap Current Baseline and
   release notes, then remove its planned/reserved row.
2. **Retire:** remove a resolved limitation from active Known Issues after its
   regression and release evidence is durable.
3. **Reject:** remove the roadmap row and record the rationale/non-goal.
4. **Defer with trigger:** remove it from active work and record the exact event
   that reopens it. “Someday” is not a trigger.

## Closure Protocol

For each row:

1. Start with the named failing/acceptance tests.
2. Implement through the owning plan/checkpoint.
3. Record exact commands, results, compatibility, migration, and performance
   evidence in `progress.md` or its linked report.
4. Update API/operations docs and release notes.
5. Graduate/retire/reject/defer the public row in the same checkpoint commit.
6. Run a drift check proving every remaining public item appears exactly once
   in this ledger.

Add a repository check that inventories Roadmap table rows and `KI-NNN` IDs,
compares them with stable IDs in this ledger, and fails on missing/duplicate
coverage. Completed IDs remain in a short archive so identifiers are never
reused.

## Version 1.0 Public-Surface Rows

These rows are owned by R6 and R7 in the 1.0 release program.

| ID | Public item | Owner | Closure evidence | Status |
|---|---|---|---|---|
| V1-DOC | Documentation | [Quality and operations](./06-quality-performance-operations.md) | Docs inventory, internal/external link check, no duplicate SSOT pages, current install/query/ops/security examples. | OPEN |
| V1-PKG | Packaging | [Quality and operations](./06-quality-performance-operations.md) | PG14-18 source/package/fresh-install/upgrade matrix green; PG19 joins only through PG19-0. | OPEN |
| V1-CI | CI/CD | [Quality and operations](./06-quality-performance-operations.md) | Required unit/static, pgrx, heavy, fuzz, low-memory, concurrency, and release jobs exist at their documented cadence and retain evidence. | OPEN |
| V1-EXAMPLE | Examples | Quality plan plus this ledger | Every maintained quick/realistic example executes in smoke CI and is linked from docs; stale examples are removed. | OPEN |
| V1-API | API clarity | GQL conformance and quality plans | Generated/drift-gated inventory covers exported SQL functions, GUCs, reserved options, SQLSTATEs, volatility/security context, permissions, and version support. | OPEN |
| V1-OPS | Operations | [Quality and operations](./06-quality-performance-operations.md) | Reproducible backup/restore, crash, concurrency, memory, package, generation rollback, and cleanup procedures pass release gates. | OPEN |

## Engine Roadmap Rows

| ID | Public item | Owner | Closure evidence | Status |
|---|---|---|---|---|
| RM-01 | Correctness and security | Checkpoints 0, 1A, 1C and [Rust boundary plan](./10-rust-type-safety-pgrx-boundaries.md) | KI-014 through KI-025 closed; two-role, multigraph, filter/value, mapped-soundness, error-guard, definer-path, relation-identity, transaction, publication, and failure tests green. | OPEN |
| RM-02 | Resource governance | [Memory governance](./01-memory-governance.md), checkpoints 1B, 3, 5 | Build/load/query/sync/compaction/analytics stay within hard envelopes or return typed resource errors. | OPEN |
| RM-03 | Out-of-core build | Memory/storage plans, checkpoint 2 | Forced-spill build completes below heap requirement and is equivalent at a declared source watermark. | OPEN |
| RM-04 | Full GQL conformance | [GQL plan](./02-gql-conformance.md), checkpoint 6 | Every applicable ISO GQL row is green and generated public docs do not overclaim. | POST-1.0 |
| RM-05 | Planner/runtime | [Planner plan](./03-planner-runtime.md), checkpoint 5 | Costed streaming IR, spill, explain estimates/actuals, bounded pathological queries, and differential plan equivalence are green. The bounded subset required by the advertised 1.0 profile remains in R5. | POST-1.0 |
| RM-06 | Storage and sync | [Storage plan](./04-projection-storage-sync.md), checkpoints 1C-3 | Identity-preserving vNext generations, watermark catch-up, mmap residency, bounded compaction, crash/CAS tests green. | OPEN |
| RM-07 | PostgreSQL 19 | [PG19 native property graphs](./08-postgresql-19-property-graphs.md), checkpoint 7 | PG19-0 through PG19-5 complete; native mapping needs no duplicate registration; PG14-18 remain green. | POST-1.0 |
| RM-08 | Refactoring | [Refactor plan](./05-refactor-plan.md), [Rust boundary plan](./10-rust-type-safety-pgrx-boundaries.md), checkpoint 4 | Planned mega-modules split with acyclic dependencies; canonical enums/newtypes and pgrx adapters replace stringly/raw boundaries; unsafe is allowlisted; SQL contracts and tests/benchmarks remain green. Release-risk subsets remain in R4. | POST-1.0 |
| RM-09 | CI/CD rollout | V1-CI and [quality plan](./06-quality-performance-operations.md) | Each documented tier runs automatically at its stated cadence; a release candidate has archived evidence. | OPEN |
| RM-10 | Competitive evidence | Planner/quality plans, checkpoint 8 | Reproducible checked scorecards publish p50/p95/p99, throughput, RSS/PSS, spill, build/load, sync lag, and semantics. | POST-1.0 |

## Reserved And Future Roadmap Rows

| ID | Public item | Required task or decision | Closure evidence | Status |
|---|---|---|---|---|
| RF-01 | WAL-driven sync | Implement logical-decoding input behind the shared `SourceChange` boundary, or reject it after measured trigger-volume evidence. Cover commit/subtransaction ordering, slot restart, schema change, watermark recovery, backpressure, retention, permissions, and trigger-vs-WAL equivalence. | Supported-version crash/restart/equivalence/resource tests and operations docs; otherwise a recorded no-go and row removal. | OPEN |
| RF-02 | COPY build scanner | Implement only if it preserves snapshot, validation, ACL/RLS, error, cancellation, and memory semantics of bounded SPI scans; otherwise remove the reserved GUC after a documented benchmark/no-go decision. | Forced-low-memory equivalence and failure tests, or rejected disposition with GUC cleanup. | CONDITIONAL |
| RF-03 | Out-of-core build and maintenance | Same owner as RM-02/RM-03/RM-06. | All memory-plan acceptance gates green; graduate into Current Baseline. | OPEN |
| RF-04 | Relationship registration helper | Run a documented usability/support-ticket gate after relationship identity is fixed. Either ship an unambiguous helper for edge-table keys/endpoints/type or declare `graph.add_edge()` final and improve diagnostics/examples. | API/SQL contract tests if shipped, or evidence-based rejection and roadmap-row removal. | CONDITIONAL |
| RF-05 | Full ISO GQL | Same owner as RM-04. | Full applicable conformance ledger green; graduate. | OPEN |
| RF-06 | PostgreSQL SQL/PGQ | [PG19 plan](./08-postgresql-19-property-graphs.md) and separate SQL/PGQ registry. | Native catalog interop, `GRAPH_TABLE` differential results, security/DDL/version gates, clear acceleration boundary. | OPEN |
| RF-07 | openCypher compatibility | Generate a narrow compatibility matrix from shared GQL capability metadata; deterministic unsupported errors; no independent planner/runtime. Decide whether the compatibility facade is a permanent bounded product surface. | Docs never imply broad conformance; matrix/tests green; then retire limitation from Known Issues without requiring full openCypher. | CONDITIONAL |
| RF-08 | Online mutable overlays | Storage/sync plan and checkpoints 1C/3. | Transaction, cross-backend, crash, upgrade, rollback, bounded-memory, compaction, and source-of-truth gates green. | OPEN |
| RF-09 | Full GQL writes | GQL plan checkpoint 6. | Applicable ISO data-modification rows, set-based DML, savepoints, concurrency, RLS, rollback, and SQLSTATE gates green. | OPEN |
| RF-10 | Non-CSR mutable projection | Storage plan checkpoint 3 plus release gates. | Supported-version concurrency/crash/upgrade/rebuild/rollback suite, bounded maintenance, release docs, and benchmark signoff with no open P0 projection issue. | OPEN |
| RF-11 | pgVector embedding cache | Decide sidecar versus derived artifact section; define stable node-ID/generation keys, invalidation, source/RLS boundary, bounded residency, update/rebuild, and no source lookup in the hot loop. | Correctness/security tests and benchmark showing useful recall/latency or a rejected disposition. | CONDITIONAL |
| RF-12 | Full A* | Implement only after defining an admissible heuristic for an explicit cost model; otherwise keep semantic guidance approximate and remove A* from active roadmap. | Optimality differential tests against Dijkstra plus bound tests, or recorded no-admissible-model rejection. | CONDITIONAL |
| RF-13 | Additional analytics | Select algorithms by demonstrated demand; run them in quota-controlled workers with cancellation, progress, failure isolation, and bounded/checkpointed state. | Correctness oracle and memory/performance evidence per algorithm; never an unbounded OLTP call. | CONDITIONAL |
| RF-14 | Distributed execution | Keep as a non-goal until checkpoint 8 evidence shows a single-database ceiling that cannot be solved locally. | Revisit only on documented workload/scale trigger; otherwise move to deferred archive and remove the active row. | DEFER-TRIGGER |

## Other Roadmap Prose That Needs A Disposition

| ID | Public item | Required task or decision | Closure evidence | Status |
|---|---|---|---|---|
| RP-01 | Semantic-guided search | Define an explicitly approximate `guided_path` contract, signal normalization/weights, degree/type bias, deterministic ties, RLS-safe candidates, beam/work/memory caps, and result diagnostics. | Recall/latency/resource benchmark against structural baselines, or reject until a user workload exists. | CONDITIONAL |
| RP-02 | Arena allocation | Profile bounded parser/query scratch allocation after streaming runtime lands. Adopt only if allocator overhead is material; never present it as graph compaction. | Before/after profile and unchanged budget/failure behavior, or remove the speculative paragraph. | CONDITIONAL |

## Known-Issues Closure

Resolved rows remain here only until their release evidence is durable. They
should not stay indefinitely on the active Known Issues page.

| ID | Owner/disposition | Evidence required to remove from active Known Issues | Status |
|---|---|---|---|
| KI-001 | Retire hardened write recheck item | Existing two-session stale-write gates green on release matrix; resolution retained in release notes. | RETIRED |
| KI-002 | Retire transaction-created-node item | Existing read-your-writes and rollback tests green on release matrix; resolution retained in release notes. | RETIRED |
| KI-003 | Move parser fuzzing from issue to permanent quality policy | Fuzz targets build, seed corpus runs in scheduled/release environment, and quality docs own ongoing campaigns. | RESOLVED-RETIRE |
| KI-004 | Scheduler decision under V1-OPS | Either ship a supervised per-database built-in scheduler with preload/auth/crash/quota tests, or declare external scheduling the supported final model with complete setup/health docs. | CONDITIONAL |
| KI-005 | Checkpoints 3, 5, 8 | Representative dirty-overlay benchmarks identify and fix material regressions, or demonstrate current performance within an approved threshold; retain regression gate. | OPEN |
| KI-006 | RF-07 openCypher bounded-surface decision | Generated compatibility matrix, deterministic rejection tests, and explicit non-conformance docs make the limitation a product boundary rather than an issue. | CONDITIONAL |
| KI-007 | RM-07/RF-06 and [PG19 plan](./08-postgresql-19-property-graphs.md) | PG19-0 through PG19-5 and SQL/PGQ registry/differential gates green; release notes preserve the change. | OPEN |
| KI-008 | RM-08, [refactor plan](./05-refactor-plan.md), and [Rust boundary plan](./10-rust-type-safety-pgrx-boundaries.md) | Named mega-modules are split at cohesive boundaries; RUST-2, RUST-3, and RUST-6 close stringly state, raw-ID, pgrx-adapter, and unsafe-allowlist gaps; SQL behavior remains stable. | OPEN |
| KI-009 | Checkpoints 0/6 and [quality plan](./06-quality-performance-operations.md) | Enumerated write SQLSTATE, constraint, RLS, savepoint, concurrency, and rollback weak paths have permanent unit/pgrx/heavy tests. | RETIRED |
| KI-010 | Tooling maintenance task | In a Nix-capable environment update both lock inputs, build the dev shell, run documented Rust/PostgreSQL/pgrx checks, and record versions; then retire. | OPEN |
| KI-011 | Retire serializer migration item | Upgrade/rebuild documentation and corrupt/trailing-byte regression remain green; historical migration stays in release notes. | RESOLVED-RETIRE |
| KI-012 | RM-02/RM-03, memory plan, checkpoints 1B/2 | Read-only no longer masks over-budget build; stale/no stats and wide filters spill or fail before OOM under enforced RSS thresholds. | OPEN |
| KI-013 | RM-02/RM-05/RM-06, checkpoints 1B/3/5 | mmap load, paths, GQL, sync, projection reads, compaction, and analytics all use hard byte/work budgets and pathological stress gates. | OPEN |
| KI-014 | RM-01 checkpoint 1A | Two-role tests prove hidden nodes/relationships cannot affect coordinates, topology, counts, paths, existence, hydration, or error side channels. | RETIRED |
| KI-015 | RM-01/RM-06 checkpoints 1A/2 | Stable source relationship identity preserves parallel edges through artifact, overlays, paths, writes, rollback, sync, hydration, and compaction. | RETIRED |
| KI-016 | RM-01 checkpoint 1A | Same-named filter columns on different graph/table/attributes remain distinct through build, pushdown, persistence, sync, and load. | RETIRED |
| KI-017 | RM-01/RM-06 checkpoint 1C | Two PostgreSQL backends cannot fork/overwrite a generation; lock/reload/CAS/crash tests preserve one current manifest. | RETIRED |
| KI-018 | RM-01/RM-06 checkpoints 1C/2 | Injected write/checksum/mmap validation/catch-up failure leaves the old generation current and queryable. | OPEN |
| KI-019 | RM-01/RF-09 checkpoints 1A/6 and QA-01 | Mutable GQL writes support PostgreSQL savepoint rollback/release through per-subtransaction delta frames, with READ COMMITTED, REPEATABLE READ, and SERIALIZABLE two-session gates. | RETIRED |
| KI-020 | RM-01 and RUST-00A through RUST-00B | Out-of-range/corrupt mapped-layout tests cannot reach unchecked pointer arithmetic; only an owning validated layout constructs stores; Miri/in-memory backing plus PostgreSQL sanitizer matrix is green. | RETIRED |
| KI-021 | RM-01 and RUST-00C | Rust destructors unwind through the supported pgrx guard before PostgreSQL ERROR; SQLSTATE/diagnostic behavior is versioned and exact on every supported major; no deep raw `errfinish()` remains. | RETIRED |
| KI-022 | RM-01/RM-06 and RUST-00D/RUST-5 | Every filter value domain roundtrips through build, sync, persisted segment, restart, and reload; old/new format behavior is documented and differential results match. | RETIRED |
| KI-023 | RM-01 and RUST-00E | Every security-definer function has the vetted `proconfig` search path and shadow-schema attack tests prove catalog/function/operator resolution cannot be redirected. | RETIRED |
| KI-024 | RM-01/RM-06 and RUST-00F | OID-stable relation identity survives rename/search-path change, serializes with concurrent DDL, and fails closed on drop/recreate across every supported major. | RETIRED |
| KI-025 | RM-01/RM-08 and RUST-1 | Failure injection proves background-worker transactions do not commit unintended partial state or leave stuck jobs; intentional progress checkpoints are documented and tested. | OPEN |
| KI-026 | RM-01/RM-06 | Durable post-build node identity, cross-backend lifecycle, writer horizons, and publication locks remain exact across every supported PostgreSQL major. | RETIRED |

### KI-013 Subsystem Gates

KI-013 remains open until every child is closed:

- [ ] **KI-013A load:** preflight plus mmap inbound/filter metadata; failure
  keeps the old backend engine; repeated loads stay within private-memory cap.
- [x] **KI-013B query/path:** candidates, degree, expansions, frontier, paths,
  hydration, sort, group, DISTINCT, result bytes, and interrupts are bounded.
- [ ] **KI-013C sync ingest:** input and normalized deltas obey byte/row/disk
  budgets and preserve watermark/idempotency under crash.
- [x] **KI-013D projection reads:** one pinned mapped snapshot per generation;
  no per-query full-file decode/map rebuild.
- [x] **KI-013E compaction:** source-range merge stays bounded and equivalent to
  clean rebuild across repeated cycles.
- [ ] **KI-013F analytics:** whole-graph work is worker-isolated, quota-bound,
  cancellable, observable, and cannot OOM an OLTP query backend.

### Explicit Orphan Work Packages

These tasks had no sufficiently explicit owner in the original plans:

#### OPS-01 Maintenance Scheduler Decision

- Either implement a supervised per-database scheduler with enable/disable,
  cadence, admission, no-duplicate two-backend behavior, crash/restart
  idempotency, role/database context, and PG14-19 tests;
- or declare `pg_cron`/external scheduling the final supported contract, remove
  the built-in promise, and prove setup/health/failure docs are sufficient.

Closes KI-004.

#### PERF-01 Dirty Projection Benchmark

Benchmark base degree, segment depth, tx inserts/deletes/tombstones, directional
reads, weighted/unweighted reads, and concurrency. Record correctness checksum,
allocations, p95/p99, and an approved regression threshold.

Closes KI-005 when optimizations land or evidence shows no material problem.

#### QA-01 GQL Write Failure Matrix

For every supported write cover readonly/unmapped/type failures, constraints,
triggers, ACL/RLS, stale/disappeared matches, cancellation, rollback, and
savepoints. Verify both PostgreSQL rows and projection/transaction deltas after
failure, with stable SQLSTATE assertions.

Closes KI-009 and contributes to KI-019.

QA-01 is complete. The permanent PG14-18 matrix covers the supported write
families, SQLSTATE and rollback behavior, transaction isolation, durable
projection visibility, and deterministic concurrent-writer races.

#### TOOL-01 Nix Input Refresh

In a Nix-capable environment update `nixpkgs` and `rust-overlay`, run
`nix flake check`/`show`, enter and verify default plus PG14-18 shells, check
Rust/cargo-pgrx/`PG_CONFIG`, run a representative build/test, and archive exact
revisions/commands. Assign recurring freshness ownership.

Closes KI-010.

#### META-01 Public Backlog Drift Check

Add a CI script that extracts active Roadmap rows and every `KI-NNN` ID, checks
for exactly one corresponding ledger ID/status, rejects duplicate/open orphan
rows, and requires closed IDs to appear in the archive/release evidence.

Closes the traceability gap that allowed public work to exist without an
executable TODO.

## “Public Backlog Clear” Gate

The current Roadmap and Known Issues are clear only when:

- every `OPEN` item above is `CLOSED`, `RESOLVED-RETIRE`, or has an approved
  conditional/defer disposition with its row removed from active roadmap work;
- every delivered roadmap row has graduated into Current Baseline and release
  notes;
- every resolved KI row has left active Known Issues and its regression remains
  in CI;
- no roadmap row, reserved feature, KI ID, or substantive future-work section
  is missing from the drift inventory;
- `progress.md` points to the next remaining stable ID rather than duplicating
  its task text;
- all checkpoint compatibility, docs, package, test, memory, security, and
  benchmark evidence is archived.

This gate clears the existing public backlog; new discoveries receive new
stable IDs rather than silently extending a closed item.
