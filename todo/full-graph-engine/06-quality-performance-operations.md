# Quality, Performance, And Operations

## Continuous Improvement Loop

Every bug follows the same durable path:

1. Minimize and classify the reproduction.
2. Add a failing test at the lowest valid layer.
3. Add cross-backend/heavy coverage when the boundary is PostgreSQL process,
   role, transaction, crash, memory, or filesystem behavior.
4. Fix the owning abstraction, not only the symptom.
5. Run affected correctness, security, memory, and performance gates.
6. Update conformance, Known Issues, release notes, and operational guidance.
7. Preserve the reproducer in the permanent suite.

Track escaped defects by subsystem and feed recurring shapes into property,
fuzz, fault, and stress suites.

## Test Portfolio

### Unit and property tests

- parser spans, diagnostics, and round-trip/generated AST properties;
- binder scopes, type coercion, null/missing, and feature-registry coverage;
- optimizer equivalence across reorder/rewrite alternatives;
- CSR and relationship identity invariants, including parallel edges;
- filter identity across same-named columns and tables;
- artifact layout, checked arithmetic, checksums, corrupt/truncated inputs;
- budget lease conservation, pressure transitions, and adaptive batches;
- delta normalization, subtransaction frames, generation CAS, and idempotency.

### Fuzzing

- GQL and compatibility parser token/AST fuzzing;
- semantic/binder fuzzing with valid and invalid generated catalogs;
- artifact, segment, manifest, and spill decoder fuzzing;
- path automaton and expression evaluator differential fuzzing;
- state-machine fuzzing for sync, publish, crash, retry, and GC.

Fuzz targets for pure logic should not need a live PostgreSQL process.

### PostgreSQL-backed tests

- every supported PostgreSQL major version;
- ACL/RLS matrix across owner, granted, restricted, SECURITY DEFINER/INVOKER
  boundaries, hydration modes, nodes, edges, paths, counts, and errors;
- READ COMMITTED, REPEATABLE READ, SERIALIZABLE, savepoint rollback/release;
- triggers, constraints, generated columns, partitions, composite keys, JSONB,
  temporal/numeric values, and catalog drift;
- two-backend build, ingest, publication, reader pin, and DDL/DML races;
- crash/restart and artifact rollback.

## Memory And Fault Gates

Run in cgroup-limited containers with nonzero enforcement thresholds:

- forced-spill build below the old heap requirement;
- stale/no `ANALYZE`, long/composite PKs, many filters, high-cardinality text,
  tenants, weights, parallel edges, and degree skew;
- mmap auto-load with small private-memory budget;
- huge label with `LIMIT 1`, supernode, cyclic/unreachable paths, wide JSON,
  DISTINCT/group/sort spill, Dijkstra, and analytics;
- bounded projection ingestion and range compaction;
- repeated rebuild/load/compact/query loops for RSS/file leaks.

Inject ENOSPC, quota exhaustion, partial write, fsync/rename failure, checksum
corruption, interrupt, backend termination, stale generation, and catalog
change. The previous serving generation must remain valid.

## Correctness And Conformance Gates

- Machine-readable ISO GQL ledger coverage is complete and drift-checked.
- Each supported row links positive, negative, diagnostics, security, resource,
  and transaction tests as relevant.
- In-memory and spill builders produce equivalent artifacts/results.
- Clean rebuild and incremental projection converge at the same watermark.
- Multiple physical plans for the same logical query produce equivalent typed
  binding tables.
- Where practical, compare GQL results with an independent implementation or a
  PostgreSQL relational oracle on generated small graphs.

## Benchmark Matrix

Use checked datasets and query semantics. Candidate suites include LDBC SNB-
style interactive workloads, Graphalytics-style analytics, generated skew and
supernode workloads, and PostgreSQL-native transactional cases.

| Dimension | Required report |
|---|---|
| Query | p50/p95/p99, throughput, rows, expansions, correctness checksum |
| Memory | peak RSS/PSS, private heap, mmap, page faults, temp/spill bytes |
| Build | scan/merge/write time, peak memory, spill, output size |
| Load | cold/warm time, private heap, mapped bytes, first-query latency |
| Sync | ingest throughput, source-to-serving lag, compaction amplification |
| Concurrency | latency and memory by graph-using backend count |
| Recovery | interrupted build/sync recovery time and retained generations |

Publish hardware, OS, PostgreSQL settings, pgGraph settings, dataset checksums,
warmup, cache state, compiler profile, and confidence intervals. Compare
competitors only with equivalent semantics and tuned documented configurations.

Performance gates should detect meaningful regressions but allow an explicitly
reviewed trade for correctness, bounded memory, or better tail latency.

## Operational Surface

Status and explain should expose:

- active graph/generation/source watermark and freshness;
- artifact compatibility, reader pins, retained generations, and GC blockers;
- private/mapped memory, budget source, pressure, and spill;
- build/merge/validation/catch-up progress and ETA inputs;
- query operator estimates/actuals and resource breaker reason;
- sync backlog, ingestion/compaction rates, and last error SQLSTATE;
- RLS/visibility mode without leaking protected row values.

Add structured PostgreSQL logs with graph, operation, generation, phase, and
stable error identifiers. Keep sensitive PK/property values out of default
logs and spill filenames.

## Release Gates

### Every change

- formatting, warnings-denied clippy, unit/property tests;
- affected pgrx tests and conformance drift;
- docs links/drift and artifact compatibility checks.

### Scheduled/nightly

- supported-version pgrx matrix;
- fuzz smoke budgets and sanitizer/Miri-eligible pure modules;
- two-backend concurrency/security suites;
- low-memory and failure-injection profiles;
- performance smoke comparisons.

### Release candidate

- fresh install/upgrade/rebuild/rollback;
- full conformance/security/transaction suite;
- synthetic and representative large workload;
- bounded build/load/query/sync/compaction under cgroup;
- crash recovery, backup/restore, generation cleanup;
- benchmark report and reviewed regressions.

## Acceptance Gates

- No P0 known issue is hidden behind a green aggregate test count.
- Memory tests enforce thresholds rather than only record metrics.
- Security tests include coordinate-only output and topology-derived aggregates.
- Cross-backend tests cover publication because in-process mutex tests cannot.
- Every fixed production bug leaves a permanent regression.
- Release evidence is archived with exact commands, versions, and results.
