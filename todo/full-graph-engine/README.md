# Full Graph Engine Program

## Mission

Build pgGraph into a complete ISO GQL graph engine inside PostgreSQL while
keeping PostgreSQL tables authoritative for identity, data, constraints,
triggers, ACLs, RLS, MVCC, WAL, backup, and recovery.

The competitive goal is measurable: lead on PostgreSQL-native graph
workloads, remain competitive on standard graph workloads, and publish
reproducible correctness, latency, throughput, build, sync, and memory results.

## Non-Negotiable Invariants

- PostgreSQL source rows are the only durable source of truth.
- A derived projection can be discarded and rebuilt without data loss.
- ACL and RLS visibility applies before coordinates, topology, counts, or
  hydrated values become observable.
- Memory limits are hard constraints. Less memory means smaller batches, more
  spill, and slower completion, not an operating-system OOM.
- Readers retain the last good generation until a replacement is written,
  validated, synchronized, and atomically published.
- GQL, SQL/PGQ, SQL functions, and compatibility frontends lower into one
  typed logical and physical runtime.
- Closed vocabularies, identities, planner slots, counts, offsets, and resource
  units use canonical enums/newtypes after one checked PostgreSQL boundary.
- Safe Rust APIs cannot reach undefined behavior; raw PostgreSQL and mmap
  operations live only in reviewed adapters when pgrx has no suitable safe API.
- Parallel relationships retain source-row identity.
- Long-running loops are interruptible and resource failures are typed.

## Stop-Ship Findings

| Finding | Evidence | Required outcome |
|---|---|---|
| Read-only mode still performs an over-budget build. | `graph/src/sql_build.rs:463-477`, `graph/src/builder.rs:280-300` | Spill or reject before allocation. |
| Filters, resolution, both CSR directions, and serialization can scale with the whole graph. | `graph/src/builder.rs:302-429`, `graph/src/resolution_index.rs:110-131`, `graph/src/edge_store.rs:419-468` | One byte governor and an out-of-core builder. |
| mmap load and compaction allocate whole-graph heap structures. | `graph/src/engine.rs:415-427`, `graph/src/projection/compact.rs:62-77`, `graph/src/projection/compact.rs:329-362` | mmap immutable structures and compact by bounded range. |
| Coordinate-only GQL may expose RLS-hidden topology. | `graph/src/sql_facade/gql.rs:171-177`, `graph/src/sql_facade/gql.rs:3015-3037`, `graph/src/query/execute.rs:708-725` | Reproduce, fix, and gate both hydration modes. |
| Equal endpoint/type relationships are deduplicated and hydrated with `LIMIT 1`. | `graph/src/edge_store.rs:227-252`, `graph/src/query/value.rs:23-45`, `graph/src/sql_facade/gql.rs:3090-3122` | Persist edge-row identity end to end. |
| Filter lookup ignores table identity. | `graph/src/filter_index.rs:343-348`, `graph/src/builder.rs:946-977`, `graph/src/sql_filters.rs:343-373` | Key filters by table and attribute. |
| Projection publication uses a process-local mutex. | `graph/src/projection/ingest.rs:27-110`, `graph/src/projection/manifest.rs:217-253` | Cross-backend lock and generation CAS. |
| Artifact replacement precedes mmap validation. | `graph/src/persistence.rs:618-622`, `graph/src/sql_build.rs:112-124` | Validate a staged generation before manifest switch. |
| GQL caps rows, not source scans, supernodes, paths, hydration, sort, or grouping bytes. | `graph/src/query/execute.rs:708-725`, `graph/src/query/execute.rs:893-952`, `graph/src/query/value.rs:108-249` | Streaming operators with byte/work budgets. |
| The roadmap targets a compatible GQL subset, not full conformance. | `graph/src/gql/ast.rs:5-24`, `graph/src/sql_facade/gql.rs:123-148` | Normative machine-readable conformance ledger. |
| Safe mmap accessors can use an unchecked node index or persisted CSR offset. | `graph/src/node_store.rs:315-322`, `graph/src/edge_store.rs:522-597` | Validated layouts and locally checked access only. |
| Custom SQLSTATE reporting calls `errfinish()` below the pgrx guard. | `graph/src/safety.rs:238-280` | Unwind through the supported pgrx error boundary. |
| Durable filter deltas narrow signed, temporal, large, and UUID values to `Option<u32>`. | `graph/src/projection/ingest.rs:32-48`, `graph/src/sql_sync.rs:1383-1391` | Versioned tagged filter values across sync and reload. |
| Security-definer functions lack explicit search paths and registered relation identity is name-based. | `graph/src/sql_facade/`, `graph/src/catalog/validate.rs:132-190` | Hardened pgrx search paths and OID-stable registration. |

## Program Documents

| Document | Purpose |
|---|---|
| [Codebase review](./00-codebase-review.md) | Evidence-backed findings. |
| [Memory governance](./01-memory-governance.md) | Budgets, chunking, spill, load, compaction, publication. |
| [GQL conformance](./02-gql-conformance.md) | ISO target, identity, visibility, SQL/PGQ. |
| [Planner and runtime](./03-planner-runtime.md) | Binding tables, costing, streaming, spill. |
| [Projection, storage, and sync](./04-projection-storage-sync.md) | Artifact vNext, snapshots, deltas, generations. |
| [Refactor plan](./05-refactor-plan.md) | Ordered module decomposition. |
| [Quality and performance](./06-quality-performance-operations.md) | Security, fuzz, fault, benchmarks, operations. |
| [Build order](./07-build-order.md) | Checkpoints and completion criteria. |
| [PostgreSQL 19 property graphs](./08-postgresql-19-property-graphs.md) | Native catalog, build, query, security, and release support. |
| [Public backlog closure](./09-public-backlog-closure.md) | One disposition and closure gate for every Roadmap and Known Issues item. |
| [Rust type safety and pgrx boundaries](./10-rust-type-safety-pgrx-boundaries.md) | Enums, newtypes, exact values, safe mmap/FFI, and pgrx-first integration. |
| [Progress](./progress.md) | Living status and next checkpoint. |

The earlier [out-of-core build plan](../out-of-core-build-plan.md) is a useful
focused sketch. The memory plan in this folder supersedes it where they differ
and expands the scope to load, query, sync, and compaction.

## Definition Of Done

1. The selected ISO GQL edition and applicable corrections have a green
   parser-to-documentation conformance ledger.
2. Build, load, query, sync, and compaction finish or return typed resource
   errors below assigned memory and disk envelopes.
3. RLS-hidden rows and relationships cannot affect observable topology,
   identities, counts, paths, or errors.
4. Parallel relationships retain identity and properties through build, load,
   traversal, writes, rollback, sync, and compaction.
5. Savepoints and supported isolation levels have two-session tests.
6. A generation can be built, validated, caught up, published, rolled back,
   and garbage-collected without interrupting readers.
7. Public benchmarks report correctness, latency percentiles, throughput,
   RSS/PSS, spill, build/load time, and sync lag.
8. PostgreSQL adapters and the planning/runtime core have an acyclic boundary.
9. Canonical enums/newtypes and exact graph values cross one checked adapter;
   unsafe code is allowlisted, locally proven, and sanitizer/fuzz/matrix gated.
10. Every current Roadmap and Known Issues row has passed the public backlog
   closure gate and has been graduated, retired, rejected, or trigger-deferred.

## Working Rules

- Fix reproduced correctness and security bugs before adjacent syntax.
- Add a failing regression test before or with every bug fix.
- Do not add graph-sized allocation without a budget owner and fallback.
- Do not add an enum-like string, raw cross-domain integer, unchecked narrowing
  cast, raw `pg_sys` call, or safe wrapper over unproven unsafe invariants.
- Treat artifact changes as migrations with rebuild and rollback plans.
- Update progress, conformance, Known Issues, and Roadmap at checkpoints.
- Close public rows through the backlog ledger; do not leave conditional or
  “future” items open indefinitely.
- Preserve the slower, safer spill path when optimizing the fast path.
