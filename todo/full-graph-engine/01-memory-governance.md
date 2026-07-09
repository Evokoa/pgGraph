# Memory Governance And Out-Of-Core Execution

## Target Behavior

When memory is scarce, pgGraph should complete more slowly by shrinking
batches and spilling derived state. It must not ask the operating system to
kill a PostgreSQL backend. The policy covers build, mmap load, query execution,
sync ingestion, projection compaction, analytics, hydration, and persistence.

## One Resource Governor

Replace scattered estimates with one statement-scoped `ResourceGovernor`.
Each operation receives a `ResourceBudget` and leases bytes/work to named
phases. Leases are RAII values; growing Rust collections use `try_reserve` only
after acquiring a lease. PostgreSQL temporary work and spill bytes are tracked
separately.

All accounting uses checked integer domain types from the
[Rust boundary plan](./10-rust-type-safety-pgrx-boundaries.md): `ByteCount`,
`MemoryBudget`, `DiskBudget`, and `WorkUnits`. Allocation decisions never use
rounded `f64` MiB values; unit conversion is display/config boundary work.

```text
ResourceGovernor
  effective private-memory bytes
  PostgreSQL temp-memory allowance
  spill-byte and file-count allowance
  work units / expansions / rows / path states
  elapsed deadline and interrupt cadence
  phase leases
    build.scan / build.sort / build.write
    load.metadata / load.inbound
    query.scan / query.expand / query.sort / query.hydrate
    sync.ingest / compact.merge / analytics.workspace
```

The effective private-memory ceiling is the minimum of:

```text
configured operation cap
graph.memory_limit_mb minus current pgGraph private residency
cgroup limit minus cgroup current usage minus safety reserve
host available memory minus safety reserve
optional cluster/build concurrency reservation
```

Use remaining cgroup memory, not `cgroup_limit * percentage`: PostgreSQL and
other backends may already consume most of the container. Unknown inputs use a
conservative configured fallback. A zero/unknown `reltuples` estimate never
means zero required memory.

PostgreSQL backends do not share normal Rust heap. Local enforcement is always
required. Concurrent heavy operations also need graph-scoped admission control
and operator-configured concurrency; an optional shared reservation counter may
augment this when preload/shared-memory support is available.

## Policy Corrections

1. Deprecate `oom_action = 'readonly'` as a build-memory response. It may remain
   a post-build mutation policy, but over-budget construction must spill or
   return a typed resource error.
2. Remove the builder's second OOM policy. Planning owns all mode decisions.
3. Replace row-only batch size with adaptive byte targets informed by observed
   encoded row width.
4. Separate reports for private heap, shared mmap, expected page-cache working
   set, PostgreSQL temp, spill disk, and peak workspace.
5. Put auto-load, query, sync, compaction, and analytics under the same API.

## Out-Of-Core Build

```text
catalog + source watermark
  -> bounded keyset scans
  -> node / relationship / filter / resolution runs
  -> external sort and bounded-fanout merge
  -> streaming outbound and inbound CSR sections
  -> streaming mmap-friendly filter and resolution sections
  -> generation-specific artifact
  -> fsync + full mmap validation
  -> replay changes to final watermark
  -> catalog/security recheck
  -> atomic current-generation switch
```

### Snapshot And Source-Of-Truth Rules

A slow chunked build must still represent one coherent PostgreSQL state.

- Initial implementation: one read-only repeatable-read snapshot, catalog
  fingerprint, and sync watermark. Keyset-scan by stable primary key rather than
  OFFSET. Document that a very long snapshot may retain dead tuples.
- Scalable implementation: capture a durable change stream at watermark W0,
  scan a baseline in chunks, replay through W1, validate, then publish W1.
- A cross-transaction or resumable scan is invalid unless durable deltas cover
  every source change since its baseline.
- Abort or restart on incompatible catalog mapping drift.
- Staging files are derived, private, checksummed, permission-restricted, and
  disposable. They never become a second source of truth.

### Bounded Runs

- Node runs contain stable table/PK identity, assigned internal ID, active
  state, tenant identity, and required catalog version.
- Relationship runs contain mapping ID, source relationship key, endpoints,
  type, direction marker, and weight.
- Filter runs are keyed by stable `FilterColumnId` plus node ID; build
  dictionaries externally rather than retaining every string.
- Resolution runs are sorted directly into the persisted lookup representation.
- Produce outbound and inbound runs so load never builds a full reverse CSR.
- Use fixed fanout and bounded read/write buffers during merge.
- Set local PostgreSQL sort/work settings deliberately; account for temp files
  and prevent hidden parallel-worker multiplication.

### Adaptive Chunking

Start from a byte target, not a fixed row count:

```text
next_rows = clamp(
  available_phase_bytes / observed_encoded_row_bytes,
  minimum_rows,
  maximum_rows
)
```

After every batch, update observed width, peak lease use, spill throughput, and
PostgreSQL temp usage. Under pressure: shrink scan buffers, reduce merge fanout,
flush dictionaries, and yield. Never rely on catching allocator OOM.

### Persistence Without Whole-Graph Copies

- Stream each section while computing length and checksum.
- Reserve header/section-table space and finalize it after section fsync.
- Do not clone the full resolution array or encode the full filter index into a
  temporary `Vec`.
- Write both CSR orientations and mmap-friendly filter metadata.
- Validate the staged artifact before it can become current.
- Keep the previous generation until reader pins and rollback retention expire.

## Bounded Load, Query, And Maintenance

### Load

- Preflight private metadata before mmap.
- mmap inbound CSR and large immutable filter/dictionary sections.
- Lazily map/pin one immutable projection snapshot per generation.
- If metadata cannot fit, fail before replacing the backend's old engine.
- Report private versus shared residency for capacity planning.

### Query

- Replace eager source vectors with lazy bitmap/iterator scans and push LIMIT
  down when semantics permit.
- Merge base, durable segments, sync overlays, and transaction deltas as
  iterators; do not build a full degree-sized vector per node.
- Bound bytes, source candidates, expansions, frontier, path states, hydration
  keys, aggregate groups, distinct keys, sort runs, output rows, and output
  bytes.
- Use bounded external sort/distinct/group/join operators. Arbitrary path
  exploration should stop at a work budget rather than attempt unsafe spill.
- Batch source visibility and hydration by table and key.
- Call PostgreSQL interrupt checks at predictable work intervals.
- Make weighted paths choose a sparse distance map when a dense graph-sized
  array is outside budget.

### Sync And Compaction

- Validate segment headers/sizes and reserve budget before reading payloads.
- Pin a decoded/mapped projection snapshot instead of rebuilding it per query.
- Compact source-ID ranges independently with a bounded k-way merge.
- Replace mmap-to-owned whole-node materialization with immutable base plus
  node/filter/edge delta stores.
- Cross-backend publication must lock, reload current state, compare-and-swap
  generation/watermark, validate, and atomically publish.

## Proposed Configuration

| Setting | Purpose |
|---|---|
| `graph.resource_memory_mb` | Optional hard private-memory ceiling for pgGraph work in one backend. |
| `graph.build_memory_target` | Absolute or percentage build workspace below the hard ceiling. |
| `graph.query_memory_mb` | Per-statement graph workspace cap. |
| `graph.maintenance_memory_mb` | Sync/compaction/analytics workspace cap. |
| `graph.memory_safety_reserve_mb` | Headroom never assigned to pgGraph. |
| `graph.build_spill_mode` | `auto`, `force`, or `off`; `off` fails when in-memory work cannot fit. |
| `graph.spill_dir` | Secure PostgreSQL-owned staging location. |
| `graph.spill_disk_limit_mb` | Per-operation disk ceiling. |
| `graph.max_heavy_operations` | Admission limit for concurrent build/compaction/analytics work. |
| `graph.query_work_limit` | Expansion/work-unit breaker independent of result rows. |

Defaults must be conservative and expose their resolved values in status.
`temp_file_limit`, filesystem free space, and PostgreSQL temp behavior remain
upper constraints even when pgGraph settings are larger.

## Delivery Phases

1. **Immediate containment:** centralize policy; remove unsafe read-only
   fallback; fix estimates; add fallible reservations; preflight load and
   compaction; enforce nonzero RSS test thresholds.
2. **Governor and telemetry:** cgroup v1/v2 plus host fallbacks, remaining-memory
   calculation, phase leases, adaptive batches, admission control, and status.
3. **Bounded source runs:** snapshot/watermark coordinator and node, edge,
   filter, resolution, inbound, and outbound staging formats.
4. **External merge and artifact vNext:** fixed-fanout merges and streaming
   mmap-ready sections without a complete owned `Engine`.
5. **Validated generation publication:** catch-up, full staged validation,
   cross-backend CAS, atomic current pointer, reader pins, rollback, and GC.
6. **Bounded runtime:** mmap both CSR directions/filter data, streaming query
   operators, and bounded hydration.
7. **Bounded maintenance:** range compaction, immutable base plus deltas,
   crash-safe checkpoints, and automatic pressure response.

## Observability

Every operation should expose:

- resolved hard and phase budgets with their source;
- current/peak private bytes, mmap bytes, PostgreSQL temp, spill bytes/files;
- rows/edges scanned, runs, merge fanout, phase progress, and throughput;
- query candidates, expansions, frontier/path/group/distinct/sort counts;
- current and target generation/watermark;
- pressure events, batch-size reductions, spill reason, and final SQLSTATE;
- previous generation retained and cleanup state.

## Acceptance Gates

- Peak backend high-water RSS stays below the cgroup limit minus safety reserve;
  builder workspace stays within budget plus `max(32 MiB, 5%)` measurement
  tolerance.
- A dataset that OOMs the heap builder completes in forced-spill mode.
- Heap profiles show no spill-mode allocation proportional to total nodes or
  relationships beyond bounded metadata.
- In-memory and spill builds are equivalent for both directions, identities,
  types, weights, filters, tenants, resolution, and representative GQL.
- Concurrent source changes publish a graph equivalent to PostgreSQL at the
  declared final watermark.
- ENOSPC, quota exhaustion, interrupt, write/checksum/load failure, and catalog
  drift preserve the previous generation and return typed errors.
- Repeated rebuild/load/compact cycles show no upward RSS or file-count trend.
- Auto-load and compaction obey the same governor and do not create a full
  reverse CSR or full layered-edge vector.
- cgroup v1/v2, unlimited cgroup, host fallback, and low-remaining-memory
  parsing have unit tests.
- Stress profiles include stale/no statistics, long PKs, many same-named
  filters across tables, supernodes, weights, tenants, and repeated rebuilds.
