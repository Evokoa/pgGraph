# Projection, Storage, And Sync

## Source-Of-Truth Contract

PostgreSQL tables and rows remain authoritative. Projection artifacts,
segments, indexes, dictionaries, caches, and build journals are derived state.
Every graph write executes PostgreSQL DML first so constraints, triggers, ACL,
RLS, locks, MVCC, and WAL decide whether it succeeds.

A query pins one projection generation and one documented PostgreSQL snapshot
contract. Freshness metadata declares the source watermark represented by that
generation plus any visible deltas.

## Artifact vNext

Use a sectioned, checksummed, versioned format designed for direct streaming
writes and mmap reads:

```text
header + feature/compatibility flags
catalog fingerprint and source watermark
node identity and state
outbound CSR offsets + edge records
inbound CSR offsets + edge references
relationship identity table
filter column descriptors + mmap-friendly data/dictionaries
resolution index
tenant membership
statistics
section directory + checksums
```

### Required Representation Changes

- Relationship records carry stable source mapping and row identity.
- Parallel edges are preserved; any duplicate suppression uses full identity.
- Filter descriptors use stable graph/table/attribute identity.
- Inbound CSR is persisted rather than rebuilt per backend.
- Large immutable filter/dictionary sections are directly mapped.
- Node and relationship count/index width is explicit. Decide whether vNext
  widens current `u32` ceilings or documents a hard maximum with early errors.
- Registry/type limits are explicit and no longer accidental `u8` behavior.
- All sizes and offsets use checked arithmetic and validate before pointer use.

Variable metadata should use a pgGraph-owned encoding with explicit evolution,
not serializer-crate behavior as a durable contract.

## Immutable Generations

```text
generation-N/
  graph.pggraph
  projection segments/chunks
  build/compaction metadata
  checksums
current -> small atomically replaced manifest
```

Publication sequence:

1. Acquire graph-scoped cross-backend publication lock.
2. Reload current generation and watermark inside the lock.
3. Write generation-specific files; fsync files and directories.
4. mmap-load and fully validate the staged generation.
5. Catch up through declared final source watermark.
6. Recheck catalog fingerprint and compare-and-swap expected predecessor.
7. Atomically switch the current manifest.
8. Retain the previous generation while any backend pin or rollback window
   references it.
9. Garbage-collect only unpinned, unreachable generations.

A Rust `Mutex` is not cross-backend synchronization. Use a PostgreSQL advisory
lock or equivalent process-shared primitive with crash-safe release.

## Build Snapshot And Catch-Up

Two valid modes:

### Snapshot build

One repeatable-read snapshot scans stable PK keysets and records W0. This is
simple and correct but can hold an old snapshot for a long time. Expose age and
operator cancellation.

### Baseline plus durable delta

Capture a durable trigger/logical-decoding stream at W0, scan baseline chunks,
replay all source changes through W1, then publish W1. This enables resumable
work only when the baseline checkpoint and retained delta stream can reproduce
one coherent state.

Never resume arbitrary cross-transaction chunks without a complete change
stream. Doing so can miss updates or combine states that never existed.

## Sync Architecture

Unify trigger and future WAL/logical-decoding input behind typed source changes:

```text
SourceChange
  graph/mapping identity
  source relation and row identity
  old/new endpoint, label, property, tenant data
  commit/watermark ordering
  operation
```

Translation validates mappings and produces immutable node, relationship,
filter, tenant, and resolution deltas. Normalize last-write-wins state once at
ingest, indexed by source/direction, instead of rebuilding overlays per query.

Requirements:

- retain enough source identity to recheck ACL/RLS and hydrate;
- order and deduplicate by committed source change identity, not topology tuple;
- support transaction and subtransaction rollback;
- expose applied, durable, serving, and source watermarks separately;
- enforce artifact/storage quotas before allocating or writing;
- serialize publication across backends;
- make catch-up idempotent after crash and retry.

## Mutable Projection Shape

Use immutable mmap base plus small, generation-pinned deltas:

- `NodeDeltaStore`: appended nodes, tombstones, state replacements;
- `EdgeDeltaStore`: identity-preserving inserts/deletes by source range;
- `FilterDeltaStore`: stable column IDs and value changes;
- `ResolutionDeltaStore`: source identity to internal ID changes;
- `TenantDeltaStore`: membership changes.

Do not copy the complete mmap node store for one mutation. Reads merge base and
deltas through reusable source-range indexes. Compaction rewrites bounded
ranges into a new generation.

## Relationship Lifecycle

Registration must identify authoritative relationship rows:

- FK-style relationship: source table PK plus mapping ID;
- edge table: declared or discovered PK tuple plus mapping ID;
- label-column relationship: row identity remains stable when type changes;
- bidirectional registration: one source relationship, two traversal
  orientations, one identity;
- generated reverse adjacency is never a second relationship.

Create, update, delete, hydration, traversal, paths, rollback, sync, and
compaction carry the same ID. Source rows without stable identity must be
rejected with mapping guidance rather than assigned durable graph-only IDs.

## Read Residency

Each backend pins at most its configured active graph generations. It maps
immutable sections and retains a decoded/mapped `ProjectionSnapshot` for the
generation lifetime. Neighbor calls do not reread/checksum/decode every segment.

Use bounded caches for genuinely variable metadata, with memory governor leases
and observable eviction. Shared OS page cache is reported separately from
backend-private heap.

## Failure And Recovery Matrix

| Failure | Required behavior |
|---|---|
| Backend interrupt/crash during build | Current generation unchanged; staging is recoverable or cleaned. |
| ENOSPC/quota | Typed error before corrupting current; reserved spill/build files released. |
| Checksum/layout validation failure | Staged generation rejected; current remains loadable. |
| Two concurrent publishers | One CAS winner; loser reloads/retries or exits cleanly. |
| Catalog mapping drift | Build/catch-up aborts before publication. |
| Sync replay crash | Idempotent restart from durable watermark. |
| Reader pins old generation | GC defers deletion until pin expires. |
| New version cannot load | Explicit incompatibility/rebuild error; previous version rollback path retained. |

## Acceptance Gates

- Parallel relationships remain distinct across every lifecycle path.
- Same-named filter columns on different tables return correct results before
  and after mmap load, sync, and compaction.
- Concurrent two-backend ingestion cannot overwrite or fork a generation.
- Failed staged load leaves the last known-good generation byte-for-byte
  current and queryable.
- Snapshot/catch-up tests with concurrent INSERT/UPDATE/DELETE match source
  tables at the published watermark.
- SAVEPOINT rollback/release affects only its delta frame.
- No single-node sync mutation materializes the complete base node store.
- Load maps inbound CSR/filter data without graph-sized private reconstruction.
- Range compaction remains under budget and produces the same graph as a clean
  rebuild.
- Crash, restart, rollback, reader pin, and GC tests run across supported
  PostgreSQL versions.
