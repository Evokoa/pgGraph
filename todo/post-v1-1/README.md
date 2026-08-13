# Post-1.1 Delivery Program

> **Active planning snapshot:** 2026-08-12 on `dev` after the pgGraph 1.1
> candidate at `0cbb03c`.

This directory is the executable work queue for the first post-1.1 engine
program. The completed [`../v1-1-release/`](../v1-1-release/README.md) program
remains the source of truth for the 1.1 release. The archived
[`../full-graph-engine/`](../full-graph-engine/README.md) documents remain
technical references and are not active checklists.

## Outcome

Deliver three independently testable improvements without weakening the 1.1
security, source-of-truth, resource, transaction, artifact, or compatibility
contracts:

1. scale caller-scoped topology RLS for selective queries by resolving only
   bounded candidate batches while retaining eager evaluation for global work;
2. replace the 254-user-label ceiling with checked, resource-governed,
   open-vocabulary relationship type identifiers; and
3. add bounded set-based graph mutations whose PostgreSQL DML statement count
   scales with mapping groups and operation phases rather than input rows.

## Non-negotiable invariants

- PostgreSQL source tables are the only durable source of truth.
- `graph.rls_mode = enforce` remains the default. Unknown visibility is never
  admissible, PostgreSQL remains the only policy oracle, and SPI or policy
  errors abort the query rather than becoming empty results.
- Hidden nodes and relationship rows are filtered before reachability, path
  selection, caps, counts, aggregates, hydration, or write target selection.
- Missing relationship identity on an RLS-governed mapping remains a fail-closed
  rebuild error.
- Visibility verdicts are scoped to one top-level query invocation. No
  transaction- or cross-statement verdict cache is permitted.
- Algorithms never invoke SPI while holding an `ENGINE` borrow. Lazy execution
  materializes a bounded ordered candidate batch, releases the borrow, resolves
  through PostgreSQL, and resumes in the original order.
- After an input coordinate resolves in the projection, caller-visible
  topology results and documented diagnostics do not distinguish a
  policy-hidden source row from the same source row absent at the PostgreSQL
  visibility snapshot. A coordinate absent from the projection retains the
  existing unresolved-coordinate diagnostic. Timing and physical work are not
  claimed to be noninterfering; the governor may charge examined projection
  entries.
- New allocations and inputs are checked and charged before allocation or
  authoritative DML. Less memory produces smaller batches or a typed resource
  error, never an ungoverned allocation.
- Mapped graph writes execute PostgreSQL DML first. Constraints, triggers,
  ACLs, RLS, MVCC, partitions, and stable source identities remain authoritative.
- Publication remains validated and atomic. A failed candidate cannot replace
  or delete the last published generation.

## Locked architecture decisions

### Scalable RLS

- Retain the 1.1 eager implementation as the semantic oracle and the initial
  implementation behind the new coordinator.
- Use a query-scoped tri-state verdict cache: `Unknown`, `Visible`, `Hidden`.
  A successful probe negative-caches requested identities not returned by
  PostgreSQL. An unsuccessful probe does not convert unknown to hidden; it
  aborts the query.
- Keep policy evaluation in `sql_visibility.rs`. Core graph algorithms consume
  resolved verdicts and remain PostgreSQL-free.
- Introduce resumable candidate production only where required. Do not place
  SPI behind `VisibilityScope::allows_*` or hold a resolver `RefMut` across SPI.
- Start with deterministic strategy selection: targeted identity/traversal
  operations use lazy resolution; whole-graph analytics and wide source scans
  remain eager. Adaptive switching is allowed only after retained benchmarks
  demonstrate a win and the switch reuses already resolved verdicts.
- Preserve write-side locking and `lock_and_recheck_*` boundaries. Remove only
  measured redundant read-result checks after complete differential coverage.
- Do not add a public paranoid-recheck GUC, a transaction verdict cache, or a
  general RLS-policy equivalence shortcut.

The detailed architecture and acceptance matrix are in
[`scalable-rls.md`](./scalable-rls.md).

### Open relationship types

- Promote the existing `EdgeTypeId` domain type into the production path and
  widen it before changing storage. Do not introduce a second label-ID type.
- Use a `u32` logical ID with checked reserved values and explicit count,
  dictionary-byte, and label-length limits.
- Use adaptive 1/2/4-byte encoding for immutable base CSR type IDs when the
  benchmark and validation gates support it; do not blindly add three bytes to
  every directed edge.
- Mutable segments and transaction deltas may use fixed-width `u32` when the
  simpler representation remains inside the measured budgets.
- Preserve text SQL/GQL contracts. Artifact and manifest versions, not public
  query signatures, own the compatibility change.
- Persist the cumulative relationship-type dictionary and publish it atomically
  with segments so incremental sync can introduce unseen labels.

The detailed design is in
[`open-relationship-types.md`](./open-relationship-types.md). The archived
[`../full-graph-engine/11-schema-flexible-dynamic-graphs.md`](../full-graph-engine/11-schema-flexible-dynamic-graphs.md)
is background, not an active queue.

### Bounded batch mutations

The public contract and execution design remain owned by
[`docs/contributor_guide/batched-graph-mutations.mdx`](../../docs/contributor_guide/batched-graph-mutations.mdx).
The initial SQL target remains one static mapped GQL write plus an ordered JSONB
array of parameter objects. A Rust loop issuing one SPI DML statement per input
row does not satisfy the feature.

Open relationship identifiers precede relationship batching so that slice is
built once against the final transaction-delta, segment, and dictionary
representation. The private typed batch relation and node `CREATE`/`MERGE`
track may proceed independently after P0. The SQL function remains private
until its complete first public contract is ready.

## Dependency map

```text
                              ┌─ P1 -> P2 -> P3 -> P4 -> P5  scalable RLS
P0 contracts and baselines ───┼─ P6 -> P7 -> P8 -> P9        open types
                              └─ P10 -> P11                   batch core/nodes

P9 open-type lifecycle ───────┐
                              ├─ P12 -> P13  relationship and remaining batches
P11 batch core/nodes ─────────┘

P5 + P9 + P13 ──────────────────> P14 release and matrix closure
```

The numbered ledger is the recommended low-conflict implementation order, not
a requirement to delay independent work. After P0, the three branches may make
progress in parallel. P12 is the first hard join: relationship batches require
both the typed batch core and the completed open-type lifecycle.

## Phase ledger

A phase becomes complete only after its red tests, implementation, relevant
public documentation, retained evidence, and independent Rust review are green.

| Phase | Status | Exit gate |
|---|---|---|
| P0 | Complete | Contracts, surface inventory, semantic corpus, and representative large-table baselines are frozen before production behavior changes. |
| P1 | Complete | Every topology-producing internal execution path requires the coordinator; the eager oracle produces byte-for-byte equivalent results and no SPI can run under an engine borrow. |
| P2 | Complete | Direct identity resolution uses bounded tri-state probes with caller identity, cancellation cleanup, scalar/composite key plans, and eager differential parity. |
| P3 | Complete | `get_neighbors`, depth-bounded BFS, multi-seed traversal, ordering, caps, parents, and truncation are lazy/eager equivalent on clean CSR. |
| P4 | In progress (P4.6 complete) | DFS, reverse, bidirectional and weighted paths, workflows, overlays, and eligible targeted GQL/Cypher expansions preserve exact result ordering and semantics; P4.7 retained evidence and closure remain. |
| P5 | In progress (P5.1 complete) | Targeted queries select lazy and global analytics select eager; relationship-identity completeness uses fixed projection summaries; retained 1M/10M evidence and read-recheck measurement remain. |
| P6 | Not started | The existing `EdgeTypeId` becomes the one production checked authority for reserved values and conversions; behavior and artifact bytes remain unchanged while width candidates are measured. |
| P7 | Not started | Runtime topology and a validated versioned base artifact support more than 254 exact relationship types without regressing normal-graph hot paths beyond the accepted budget. |
| P8 | Not started | Persistent dictionaries, mutable segments, compaction, reload, and transaction-local/savepoint state support unseen labels atomically and within resource limits. |
| P9 | Not started | SQL traversal, paths, and GQL preserve exact filtering beyond 254 labels; migration, diagnostics, docs, fuzz/property, and performance evidence are complete. |
| P10 | Not started | The private batch contract validates and types bounded ordered input once, rejects duplicate identities, and proves no per-input DML loop. |
| P11 | Not started | Set-based node `CREATE`/`MERGE` preserve RLS, constraints, triggers, partitions, ordinality, atomicity, savepoints, and idempotent replay. |
| P12 | Not started | Set-based relationship `CREATE`/`MERGE` resolve endpoints and identities set-wise, support open labels and parallel edges, and expose same-transaction node-to-edge ingestion. |
| P13 | Not started | `SET`, `REMOVE`, relationship `DELETE`, and `DETACH DELETE` reuse the bounded relation; sync, reload, compaction, cancellation, and concurrency gates are green. |
| P14 | Not started | Contracts, update/install paths, supported-feature docs, benchmarks, fuzz/property suites, and PostgreSQL 14-18 package matrices pass on one reviewed commit. |

## Phase details

### P0: Freeze contracts, corpus, and baselines

**Red evidence first**

- Add a generated public topology-surface inventory. Direct unrestricted
  constructors are test-only; production no-RLS/bypass execution requires the
  sealed proof produced by policy preparation.
- Add missing real-role policy cases: combined permissive/restrictive policies
  and user-created `SECURITY DEFINER` wrappers.
- Add output-equivalence fixtures for a policy-hidden topology candidate versus
  the same candidate absent from the projected topology, covering rows, exact
  paths, counts, caps, and truncation. Keep the public unresolved-seed
  diagnostic contract separate; do not assert equal timing or physical work.
- Freeze a reproducible large-table RLS runner for 1M and 10M source rows,
  sparse-allow and sparse-deny policies, shallow/deep traversal, scalar and
  composite identities, node-only and relationship RLS, and no-RLS controls.
  Retain a local 1M lower bound even when the eager query is censored by the
  statement timeout; P5 owns completed 1M/10M comparative runs.
- Record available visibility time, total time, p50/p95, result/hidden counts,
  table sizes, and source-query plans. Mark unavailable SPI-call, scanned-key,
  scanned-byte, graph-only-time, and peak-governed-byte fields `NA`; P1 adds
  bounded coordinator instrumentation before P5 makes comparative claims.
- Freeze batch API/resource/diagnostic decisions and EdgeTypeId width/artifact
  benchmark fixtures without changing production behavior.

**Exit:** the safety corpus detects visibility regressions, the runner can
retain complete or censored large-table evidence, and the width/artifact
benchmark candidates are reproducible. This phase makes no optimization claim;
P5 requires completed 1M/10M comparative evidence on a suitable host.

**P0 evidence (2026-08-12):**

- `scripts/check_topology_security_inventory.py` exhaustively partitions the
  public SQL contract, exactly matches topology `#[pg_extern]` entry points and
  overloads, and checks each function's body-scoped route through visibility
  composition. Rust context types and PostgreSQL semantic tests remain the
  execution-boundary proof.
- `graph/tests/heavy/run_sqlstate_acl_boundary.sh` covers PostgreSQL's combined
  permissive/restrictive policy semantics and user-created `SECURITY DEFINER`
  effective-role behavior through real login sessions.
- Focused BFS, unweighted-path, and exact-path enumeration tests compare a
  policy-hidden candidate or relationship with the same topology physically
  absent, including returned coordinates, selected paths, counts, caps, and
  truncation. Timing, physical work, and unresolved-coordinate diagnostics are
  intentionally outside that equivalence contract.
- `graph/tests/heavy/rls_large_table_baseline.sh` owns the parameterized scalar,
  composite, relationship-row, node-only, edge-only, combined, and no-RLS
  baseline. The retained [1M result](../measurements/2026-08-12-p0-eager-rls-1m/README.md)
  records a completed no-RLS control and a cancelled `>600s` eager RLS query.
  The 10M graph run remains a dedicated-host P5 exit gate because the current
  eager 1M query already exceeded ten minutes.
- `graph/benches/edge_type_width_bench.rs` freezes decode and artifact-copy
  fixtures at the one-/two-/four-byte boundaries (254, 255, 65,534, and 65,535
  labels), including a fixed-`u32` low-cardinality comparison, without changing
  production types or artifact bytes.

### P1: Introduce the eager coordinator

- Add a query-scoped coordinator API required by topology execution. Initially
  it delegates only to the eager oracle. Production unrestricted execution
  requires a sealed no-RLS/bypass proof created by policy preparation; direct
  unrestricted construction is test-only.
- Define bounded ordered candidate and verdict types. Candidate materialization
  is pure Rust; PostgreSQL resolution occurs only after `ENGINE` borrows end.
- Make direct unrestricted constructors test-only. Production code can obtain
  unrestricted execution only through the sealed proof returned by the policy
  preparation boundary.
- Preserve all 1.1 output, diagnostic, resource, and performance contracts.

**Exit:** compiler-visible composition prevents a new public topology path from
silently omitting visibility, while eager behavior remains equivalent.

**P1 evidence (2026-08-12):**

- SQL topology entry points receive a `VisibilityCoordinator` from
  `prepare_eager_visibility()` and can no longer construct execution contexts
  directly; the exhaustive topology inventory tracks the renamed preparation
  boundary.
- Test-only and explicit `benchmarks`-feature APIs route unrestricted execution
  through the same coordinator. Normal production builds do not compile those
  escape hatches.
- Bounded owned candidate and resolved-verdict batches enforce stable sequence,
  candidate/key-byte limits, and fail-closed alignment before P2 adds SPI
  probes.
- Architectural tests reject raw production unrestricted constructors, direct
  context construction, and SPI calls inside `ENGINE` closures. A
  BFS oracle compares coordinator and direct eager output byte for byte.

### P2: Build the lazy oracle and direct-identity slice

- Implement checked row-and-byte bounded candidate batches and tri-state
  statement-local caches, charged to `QueryVisibility` with fallible reserves.
- Deduplicate probe keys while preserving candidate order.
- Remove the eager `max(octet_length(...))` preflight from lazy mode.
- For the one-identity P2 slice, use one typed scalar/composite equality probe
  and retain `EXPLAIN` evidence that PostgreSQL selects the source primary-key
  index. P3 benchmarks batched `ANY` versus `VALUES`/`unnest` when frontier
  batches contain more than one identity.
- Add a narrow recursive-visibility guard cleared with
  `PgTryBuilder::finally`; never keep Rust borrows or ordinary stack-owned
  graph-sized state across PostgreSQL ERROR/longjmp.
- Use lazy probes for `get_node` and true depth-zero seeds. At the P2
  checkpoint, positive-depth shortest paths remained eager; unweighted paths
  moved to resumable execution in P4.4 and weighted paths in P4.5.
- Add negative-cache, cancellation, policy error, recursion, memory exhaustion,
  current-setting, role, snapshot, and transaction-delta tests.

**Exit:** direct identities are lazy/eager equivalent and source work scales
with requested identities rather than table membership.

**P2 evidence (2026-08-12):**

- `get_node()` and true single-root `traverse(max_depth := 0)` use a bounded,
  statement-local tri-state resolver. At the P2 checkpoint, positive-depth
  traversal and paths remained on the eager oracle; BFS and DFS moved to their
  resumable engines in P3 and P4.3, unweighted paths moved in P4.4, and
  weighted paths moved in P4.5.
- Candidate count, key bytes, probe-plan scratch, cache growth, copied payloads,
  returned work, elapsed time, and PostgreSQL interrupts are governed under
  `query.visibility`. The lazy path has no table-wide maximum-key preflight.
- Scalar and composite probes compare registered primary-key columns to typed
  requested components. A retained PostgreSQL `EXPLAIN` regression requires an
  index or index-only scan. Date/time, array, domain, extension, and custom key
  types fail closed under RLS because the v1.1 artifact stores only text and
  cannot safely reconstruct values across changed session I/O settings.
- The shared recursion guard covers eager and lazy policy evaluation. Resolver
  state is TLS-owned across PostgreSQL longjmp and cleared in
  `PgTryBuilder::finally`; synthetic and policy-originated cancellation tests
  prove cleanup and successful retry.
- PostgreSQL tests cover effective `SECURITY DEFINER` identity, current-setting
  policies, visible/hidden/absent eager parity, stale no-RLS and legacy-bypass
  source rows, composite keys, GUC-sensitive date identity rejection, resource
  exhaustion before probing, transaction-local nodes with subtransaction
  rollback, same-statement command changes, concurrent REPEATABLE READ snapshot
  isolation, arbitrary policy-error propagation and cleanup,
  relationship-identity failure, and depth-zero diagnostics.

### P3: Add resumable one-hop and BFS traversal

- Introduce a resumable BFS machine that owns frontier, visited, parents,
  outputs, and adjacency cursor state.
- Materialize bounded adjacency candidates while the engine is borrowed,
  resolve outside the borrow, and admit sequentially in original order.
- Preserve duplicate-parent selection, multi-seed order, max-node/frontier
  behavior, work charging, truncation, pagination, and hydration.
- Share one resolver within top-level workflow calls that perform several
  internal targeted traversals.

**Exit:** one-hop and BFS results, paths, caps, and diagnostics match eager mode
exactly while selective-table benchmarks show bounded source work.

**P3 evidence (2026-08-13):**

- Clean CSR BFS uses an owned resumable machine; committed overlays, durable
  segments, and transaction-local topology deliberately retain eager visibility
  until P4 provides their owned identity cursors.
- Candidate materialization owns node and relationship source identities before
  releasing the engine borrow. Set-based typed PostgreSQL probes execute after
  that boundary and fan node/relationship verdicts back to adjacency order.
- Pure Rust tests cover exact eager result bytes, duplicate-parent selection,
  count/key-byte limits, cap/truncation parity, relationship-identity folding,
  projection-epoch validation, cursor paging, and one expansion charge per
  emitted adjacency across yields. Empty or filtered raw pages yield between
  bounded engine borrows, and PostgreSQL errors are converted to Rust unwinds
  so traversal/resolver allocations are dropped before the original SQLSTATE
  is rethrown. PostgreSQL policy cancellation and cleanup are covered by the
  real-login boundary suite.
- The PG17 real-login SQLSTATE/ACL boundary suite passes eager/lazy outbound,
  inbound, depth-two, multi-seed, cap, `get_neighbors`, policy
  cancellation/retry, node policy, and relationship policy cases. Hydration and
  pagination remain on their shared post-traversal implementation and are not
  claimed as separate P3 evidence.
- Automatic selection uses lazy BFS only when RLS applies; development forcing
  supports differential evidence. No-RLS and authorized bypass calls remain on
  the established eager path.
- Retained PG17 evidence lives in
  `todo/measurements/2026-08-13-p3-selective-rls-10k/` and
  `todo/measurements/2026-08-13-p3-selective-rls-1m/`. The completed 10k paired
  run measured 35–47 ms lazy versus 146–430 ms eager with identical results;
  at 1M rows eager exceeded the 600-second timeout while the equivalent lazy
  query completed in 3.25 seconds with two returned source rows.

### P4: Complete targeted lazy topology coverage

P4 is implemented as reviewed checkpoints because every later algorithm
depends on the same borrow-free representation cursor:

1. **P4.1 — owned classic adjacency cursors:** land replay-free clean-CSR and
   classic transaction edge-overlay cursors; both directions page without
   whole-degree materialization.
2. **P4.2 — durable cursor, mutable BFS, and workflows:** add the k-way
   committed durable-segment cursor, generalize candidate identity
   materialization, remove mutable-topology BFS fallbacks, and reuse one
   statement resolver across targeted BFS workflows.
3. **P4.3 — DFS:** own stack, reversed cursors, visited timing, parents, caps,
   and outputs across policy probes.
4. **P4.4 — unweighted paths:** migrate single-direction and bidirectional BFS
   separately; bidirectional traversal completes the chosen level before
   accepting its meeting node.
5. **P4.5 — weighted paths:** page one popped Dijkstra node at a time and
   preserve heap, tie, and target-pop semantics.
6. **P4.6 — targeted GQL/Cypher:** migrate only identity-bounded projection
   expansions and write MATCH plans; whole-source scans stay eager.
7. **P4.7 — evidence and closure:** exact eager/lazy differential coverage,
   retained large-table evidence, docs, and the final Phase 4 review.

P4.1 completed on 2026-08-13. The owned cursor pages clean CSR and classic
edge overlays by examined work, including tombstones and duplicate checks, so
every page releases the engine borrow for cancellation. Machine-retained page
capacity is reserved up front, overlay inserts are deterministic, and mixed
transaction-local node/filter changes still select eager. Property tests cover
cursor/iterator parity; the PG17 real-login gate covers forced eager/lazy
outbound and inbound BFS over visible and hidden relationship inserts, a
relationship delete, stable identities, source-work bounds, rollback, and
retry. Committed durable segments remain assigned to the next cursor
checkpoint.

The durable-cursor portion of P4.2 completed on 2026-08-13. Segment-backed
`out` and `in` BFS now merge base/base-chunk, cumulative durable, and the
query-frozen classic overlay through an owned bounded cursor. Equal keys
advance atomically, newer inserts retain the established tombstone precedence,
parallel relationship identities remain distinct, and empty pages yield
between engine borrows. Monotonic committed-overlay and transaction topology
revisions reject same-cardinality substitutions across a policy probe. Pure
cursor and engine differentials cover both directions, wildcard and identified
deletes, later inserts, parallel IDs, raw work bounds, and exact eager rows. A
PostgreSQL RLS regression covers visible and hidden durable relationship
identities with forced eager/lazy parity and the `PG023` fail-closed diagnostic
for a missing durable identity.
The segment-backed `any` cursor now merges both directional sources through the
same precedence key, including cross-direction tombstones and equal-key inbound
payload precedence, while retaining bounded progress pages. `expand`,
`find_related`, and `neighborhood` now reuse one query-local resolver across
all roots and optional count passes while preserving their established
ordering, pagination, hydration, grouping, and truncation. PostgreSQL tests
cover exact eager/lazy results, cancellation cleanup and retry, and the no-RLS
zero-SPI fast path. P4.2 is complete.

P4.3 completed on 2026-08-13. DFS now owns a LIFO frontier, reverse adjacency
cursor, visited-on-push parent state, bounded pending page, and projection epoch
across caller-policy probes. Reverse cursors page clean CSR, classic overlays,
and durable directional/`any` layers without prefix replay or whole-degree
materialization. Pure and engine differentials preserve reversed push order,
duplicate/cycle/parallel-edge parent choice, caps and truncation, and exact
Out/In/Any results across committed, durable, and edge-only transaction state.
PostgreSQL tests cover forced eager/lazy node and relationship RLS, hidden
intermediates, cancellation and policy-error cleanup with retry, and the no-RLS
zero-SPI fast path.

P4.4 completed on 2026-08-13. Unweighted shortest paths now own bounded
single-direction or bidirectional search state across caller-policy probes.
Single-direction search preserves the eager first-visible-target rule;
bidirectional search freezes and completes the selected level before choosing
the strictly shortest first-encountered meeting node. Endpoint probes,
relationship identities, edge-type filters, mutable edge overlays, durable
segments, transaction-local edge deltas, work caps, cancellation cleanup, and
projection-epoch rejection have eager/lazy differential coverage. The no-RLS
route remains eager with zero visibility SPI. P4.5 weighted paths is the active
checkpoint. The retained
[`10k PG17 checkpoint`](../measurements/2026-08-13-p4-unweighted-path-10k/README.md)
records one functional/performance sample; it is not a statistical benchmark.

P4.5 completed on 2026-08-13. Weighted shortest paths now page one popped
Dijkstra node at a time while preserving strict-lower relaxation, heap ties,
stale-entry handling, target-pop termination, edge weights, and exact step
metadata. Clean CSR and segment-backed durable projections use bounded weighted
cursors; pending classic overlays remain the established `PG018` error, and
transaction-local node/filter state retains the eager oracle. Endpoint order,
typed edge filters, node and relationship RLS, dynamic relationship labels,
resource caps, `PG023`, cancellation, policy-error cleanup, and no-RLS zero-SPI
behavior have PostgreSQL differential coverage. The retained
[`10k PG17 checkpoint`](../measurements/2026-08-13-p4-weighted-path-10k/README.md)
records one functional/performance sample; it is not a statistical benchmark.

P4.6 completed on 2026-08-13. Scalar-identity node scans, fixed one-hop
directed GQL/Cypher reads, optional matches, and identity-bounded mapped `SET`
matches now select bounded caller-policy probes. The implementation preserves
PostgreSQL ACL and RLS ordering, optional null extension, relationship-row
ordering, result caps, cancellation and policy-error cleanup, and the mapped
write DML/recheck boundary. Whole-source scans, joins, wildcard and variable
paths, undirected expansion, and other write shapes remain on the eager oracle.
The PostgreSQL corpus covers real-role node and relationship RLS, hidden nodes
and edges, GQL/Cypher parity, mapped writes, cancellation/retry, row-cap eager
fallback, and the unrestricted resolver fast path.

- Keep connected components, component statistics, whole-table GQL scans,
  all-possible-path enumeration, and path-count analytics eager until they have
  purpose-built resumable executors.

**Exit:** every targeted topology family has eager/lazy differential parity;
global work retains the eager oracle.

### P5: Select strategy and publish scalable-RLS evidence

P5.1 completed on 2026-08-13. Base CSR publication now records a fixed
per-edge-type relationship-identity completeness summary. Durable snapshots
derive the same summary from their effective changed-source topology, while
committed and transaction-local overlays maintain bounded summaries alongside
their revisioned state. Eager and targeted preparation intersect the active
mapping types with those summaries instead of scanning projected relationship
arrays at query start. Candidate validation remains fail-closed with `PG023`,
and static/dynamic mapping selection preserves the established ACL and error
ordering. P5.2 owns runner telemetry and read-recheck measurement; P5.3 owns
the retained 1M/10M release matrix and public closure.

P5.2 completed on 2026-08-13. The release runner now records the selected
physical strategy separately from the workload class, exact ordered result
digests, visibility time and source work, governed peak memory/work, and
relationship-completeness checks. Development telemetry also measures the
remaining eager GQL read-result rechecks without adding production timing
overhead. The focused release profile proves bounded identity-seeded GQL
selection against its eager PostgreSQL oracle, retains whole-source eager
selection, and checks the unrestricted zero-resolver-SPI path. P5.3 remains
open for clean statistical 1M and dedicated-host 10M evidence.

- Start with deterministic targeted-lazy/global-eager selection.
- Add adaptive fallback only if retained benchmarks beat the deterministic
  strategy; reuse known verdicts and never restart policy work from zero.
- Replace repeated per-query relationship-identity completeness scans with a
  load-time/per-mapping summary covering base, segments, and transaction deltas
  while retaining fail-closed `PG023` behavior.
- Measure the remaining GQL read-result rechecks. Remove only checks proven
  redundant by complete differential coverage; preserve every write-side lock
  and recheck.
- Publish representative RLS/no-RLS latency, source work, memory, and plan
  evidence. Update the supported-feature ledger and RLS documentation only when
  the behavior becomes the supported default.

**Exit:** large selective RLS queries no longer scan all visible source rows,
global queries retain the efficient eager path, and no security guarantee is
weaker than 1.1.

### P6: Freeze and promote `EdgeTypeId`

P6.1 completed on 2026-08-13. `EdgeTypeId` is now an always-built private-field
logical `u32` newtype with distinct untyped and all-ones sentinel values. Named
v6 storage conversions exhaustively round-trip bytes 0 through 254, reject the
physical 255 sentinel, and reject logical IDs that require a wider artifact.
The current v6 artifact remains byte-for-byte stable under a full-file golden
checksum and reload test. Registry migration, logical consumer migration, and
multidimensional width evidence remain P6 work; physical CSR widening remains
P7.

- Benchmark logical `u32` with adaptive 1/2/4-byte base storage against the
  current `u8` representation across cardinality, degree, direction, and depth.
- Freeze reserved values, maximum label count, individual label length,
  cumulative dictionary bytes, diagnostics, and artifact migration policy.
- Promote the existing `EdgeTypeId` from its development/test boundary, widen
  it to `u32`, and replace raw `u8` conversions across registry, edges,
  neighbors, filters, visibility, paths, overlays, and sync while adapters
  still encode/decode v6 `u8` bytes.
- Keep logical `UNTYPED = 0` and `SENTINEL = u32::MAX`. Sentinels are never
  serialized as edge type IDs. Each physical width reserves its all-ones code
  as invalid (`255`, `65,535`, or `u32::MAX`), so a section widens before a
  logical ID would equal that code. Consequently one-byte sections encode user
  IDs through 254, two-byte sections through 65,534, and four-byte sections
  through the configured maximum below `u32::MAX`.
- Add O(1) label-to-ID lookup rather than repeated `Vec::position` scans.

**Exit:** behavior and v6 compatibility remain unchanged, checked types prevent
narrowing, and the chosen physical representation has retained evidence.

### P7: Widen runtime and immutable persistence

- Widen raw edges, both CSR directions, neighbor sources, traversal, paths,
  components, aggregation, filters, and visibility edge-type sets.
- Add a validated versioned base format whose type-ID section declares 1/2/4
  byte width, with checked size/alignment and a typed view/iterator.
- Stage persisted build/run codecs in logical `u32`, and preserve v6 reading or
  return the documented rebuild diagnostic without risking the last generation.
- Run corruption, endian/width, truncation, checksum, fuzz, recovery, and hot
  traversal regression tests.

**Exit:** a rebuilt graph can represent and exactly traverse more than 254
types; ordinary low-cardinality graphs retain the approved memory/artifact and
latency envelope.

### P8: Persist dictionaries and incremental labels

- Add a cumulative checksummed relationship-type dictionary artifact referenced
  by the generation manifest and garbage collector.
- Carry authoritative label text through sync until publication; intern unseen
  values deterministically under the writer lock and publish dictionary plus
  segments atomically.
- Widen mutable segment codecs and accept older fixtures through checked decode.
- Extend compaction, repair, reload, rollback retention, and recovery planning.
- Give transaction deltas a savepoint-aware appended-label dictionary without
  irreversibly mutating the loaded base registry before commit.

**Exit:** inserts and label changes can introduce unseen valid types without a
full rebuild, survive reload/compaction, and roll back cleanly.

### P9: Complete open-type query and release behavior

- Preserve exact `edge_types` filtering and GQL relationship patterns above
  254 and 65,535 labels.
- Lower eligible registered label-column equality predicates to the same exact
  type-ID filter rather than hydrating every relationship.
- Bound label listing/status output or make it explicitly paginated.
- Complete high-cardinality build/sync/query resource tests, ACL/RLS and
  transaction matrices, fuzz/property evidence, migration/rollback docs,
  PostgreSQL-version tests, and performance evidence.
- Update `supported_features.md` only when the feature and migration contract
  are complete.

**Exit:** the 254-label limitation is removed from the supported product with
exact semantics and explicit physical resource limits.

### P10: Build the private batch contract and typed relation

- Add failing SQL/API tests for arrays, empty input, ordinality, row/byte caps,
  missing/extra parameters, PostgreSQL type conversion, duplicate effective
  identities, diagnostics, and atomic failure.
- Parse and bind one supported static mapped write once.
- Lower the JSONB array through `jsonb_array_elements ... WITH ORDINALITY` and
  typed PostgreSQL casts without interpolating user values into SQL.
- Preflight worst-case transaction-delta rows/bytes before DML.
- Instrument source DML counts so tests prove statements scale with mapping
  groups and operation phases, not input rows.

**Exit:** the private executor owns one bounded typed input relation and cannot
devolve into per-row SPI DML.

### P11: Add set-based node `CREATE` and `MERGE`

- Implement `INSERT ... SELECT` and conflict-arbitrated set-based `MERGE` using
  registered source identity.
- Lock existing rows in stable table/key order. Do not use `xmax` as the
  create-versus-match oracle.
- Re-read authoritative trigger-adjusted rows set-wise, reconstruct input
  ordinality, and record bounded transaction-local deltas.
- Cover defaults, generated columns, partitions, composite keys, constraints,
  statement/row triggers, ACL/RLS/FORCE RLS, duplicate conflicts, idempotent
  replay, concurrent overlap, rollback, and savepoints.

**Exit:** node ingestion is set-based, atomic, retry-safe for `MERGE`, and
source/DML behavior matches single-row GQL semantics.

### P12: Add set-based relationship `CREATE` and `MERGE`

- Resolve and lock all endpoints set-wise under caller visibility and RLS.
- Insert/upsert relationship source rows set-wise using stable registered
  relationship identity, never endpoints alone.
- Re-read authoritative endpoints, type text, and identity after triggers;
  allocate exact forward/reverse deltas and preserve parallel relationships,
  bidirectional mappings, composite keys, and open labels.
- Add same-transaction node-batch-to-relationship-batch ingestion tests.

**Exit:** relationship batches use the final `EdgeTypeId` lifecycle and preserve
identity, RLS, atomicity, and immediate transaction-local topology.

### P13: Finish mutation shapes and projection lifecycle

- Reuse the typed relation for node/relationship `SET` and `REMOVE`, exact
  relationship `DELETE`, and incident-edge-first `DETACH DELETE` with stable
  locking and write-boundary predicate rechecks.
- Cover trigger sync, durable segments, cross-backend reload, compaction,
  cancellation, timeouts, capacity failures, concurrent `MERGE`, create/delete
  races, and crash recovery.
- Prove any post-DML graph-capacity failure aborts the containing PostgreSQL
  statement and leaves no partial source or projection effect.

**Exit:** every promised batch mutation is atomic, bounded, savepoint-safe, and
durably convergent without per-batch rebuild.

### P14: Public contract and release closure

- Freeze SQL signatures, GUCs, diagnostics, grants, update scripts, and release
  contract records.
- Complete parser/JSON fuzzing, identity/ordinal property tests, full Rust and
  pgrx suites, real-role ACL/RLS gates, concurrency/fault tests, and PostgreSQL
  14-18 source/package/install/update matrices.
- Benchmark repeated `graph.gql()` against batch sizes and mapping groups,
  reporting DML statements, throughput, latency, WAL, RSS/PSS, sync lag, and
  compaction state.
- Update API/querying/security/operations/migration docs and
  `docs/user_guide/supported_features.md` in the same checkpoint.

**Exit:** all three tracks have reviewed retained evidence on one exact commit,
and this ledger contains no actionable incomplete phase.

## Per-phase operating discipline

For every phase:

1. write or identify the red test and retain the failure reason;
2. implement the smallest coherent vertical slice;
3. run targeted tests in parallel where they do not share PostgreSQL state;
4. update public docs and `supported_features.md` when supported behavior or an
   existing supported contract changes;
5. run formatting, strict clippy, production check, affected pgrx/heavy tests,
   docs drift, release contract, and `git diff --check` in proportion to risk;
6. obtain an unbiased Rust review without supplying the expected conclusion;
7. resolve every blocker and request-change, rerun affected gates, then commit
   the phase with the repository commit-message convention; and
8. update this ledger and evidence before starting the next phase.

Do not push unless separately requested.
