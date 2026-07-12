# 1.0 Production Readiness Plan

## R1: Correctness, Security, Identity, And Transactions

- [x] Finish parallel-aware relationship compaction without collapsing distinct
      source rows that share endpoints, type, and direction.
- [ ] Carry stable relationship identity through every advertised read, write,
      sync, hydration, visibility, and recovery path.
- [ ] Complete relationship-row ACL/RLS checks for nodes, relationships, paths,
      scalar identities, aggregates, existence, and hydration-disabled output.
- [ ] Finish table-qualified filter identity and remove ambiguous internal
      name-only lookup paths.
- [ ] Add subtransaction/savepoint delta frames with rollback and release tests.
- [ ] Complete READ COMMITTED, REPEATABLE READ, SERIALIZABLE, constraint,
      trigger, partition, composite-key, and concurrent-write coverage for the
      advertised write profile.
- [ ] Complete supported-major evidence for mapped safety, guarded PostgreSQL
      errors, definer search paths, relation identity, and exact durable values.
- [x] Create the machine-readable 1.0 capability/conformance registry and make
      documentation drift fail CI.

## R2: Resource Containment And Safe Publication

- [ ] Centralize byte, work, row, disk, and time policy with checked unit types
      and fallible reservations.
- [ ] Enforce preflight plus runtime breakers for build, load, queries, sync,
      compaction, and advertised analytics.
- [ ] Prefer bounded adaptive batches and spill; return a stable typed resource
      error before backend or container OOM.
- [ ] Add production-shaped RSS/PSS gates for stale statistics, many filters,
      high-cardinality values, parallel edges, supernodes, `LIMIT 1`, concurrent
      backends, auto-load, and compaction.
- [ ] Replace process-local publication protection with graph-scoped
      cross-backend locking and generation compare-and-swap.
- [ ] Stage, fsync, validate, catch up, and atomically switch generations while
      retaining the previous serving generation on every failure.
- [ ] Add reader pins, rollback retention, bounded garbage collection, and
      competing-publisher/crash/fault-injection tests.

## R3: Bounded Storage, Build, Sync, And Compaction

- [ ] Build from one coherent PostgreSQL snapshot and publish a declared source
      watermark after bounded catch-up.
- [ ] Use bounded runs and fixed-fanout external merge for nodes,
      relationships, filters, resolution, inbound, and outbound data.
- [ ] Stream validated mmap-ready artifact sections without retaining a second
      complete owned graph.
- [ ] mmap inbound CSR and supported filter/dictionary sections while charging
      private metadata and page-cache-sensitive residency separately.
- [ ] Pin one immutable projection snapshot per query/generation.
- [ ] Keep sync ingestion, normalization, manifest publication, and range
      compaction within enforced byte/row/disk limits.
- [ ] Prove in-memory and spill builds are equivalent and repeated
      build/load/sync/compact cycles do not leak memory or files.

## R4: Release-Risk Refactoring

- [ ] Complete canonical enum/newtype, checked-conversion, pgrx-adapter, exact
      value, and unsafe/raw-FFI allowlist work needed by R1-R3.
- [ ] Split publication, generation, resource policy, artifact validation, and
      compatibility ownership where current coupling prevents isolated tests.
- [ ] Keep mechanical moves separate from behavior changes and preserve SQL,
      SQLSTATE, artifact, and configuration contracts.
- [ ] Execute the playground/scripts overhaul in
      [04-playground-and-scripts.md](./04-playground-and-scripts.md).
- [ ] Re-evaluate broader module or crate extraction after 1.0; do not make
      wholesale decomposition a release gate without concrete risk evidence.

## R5: Supported Query And Write Profile

- [ ] Freeze the exact SQL and GQL profile advertised for 1.0.
- [ ] Ensure every advertised capability has parser/binder/executor,
      diagnostics, ACL/RLS, transaction, resource, positive, negative, and docs
      evidence as applicable.
- [ ] Make unsupported GQL and compatibility syntax fail deterministically with
      guidance; do not imply full ISO GQL or broad openCypher conformance.
- [ ] Bound all operators used by the supported profile, including path search,
      aggregate, distinct, sort, hydration, and write rechecks.
- [ ] Keep PostgreSQL DML authoritative for every graph write and reject
      unmapped writes with actionable mapping guidance.
