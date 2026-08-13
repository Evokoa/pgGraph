# Open-Vocabulary Relationship Types

This document owns the implementation architecture for removing the historical
254-user-label ceiling. Public direction remains in
[`docs/roadmap.mdx`](../../docs/roadmap.mdx); phase status lives in
[`README.md`](./README.md).

## Current delivery state

P6 introduced one checked logical `EdgeTypeId(u32)` authority and O(1) registry
lookup. P7 now writes adaptive v7 immutable CSR bases with one-, two-, or
four-byte type sections and continues to read v6 artifacts. Rebuilt
`csr_readonly` graphs can therefore exceed 254 user-facing labels under the
documented count and byte policies.

The remaining narrow boundary is the mutable segment codec. Wide mutable bases
are rejected before publication, and sync accepts only labels already present
in the loaded dictionary. P8 owns adaptive segment persistence and governed
incremental dictionary growth. P9 owns the complete query/listing/release
matrix and final public feature closure.

Open vocabulary means a checked, explicitly bounded dictionary large enough for
data-driven relationship types. It does not mean unlimited backend memory or an
unbounded SQL result.

## Logical type

The existing `EdgeTypeId` newtype becomes a production type and remains the one
authority for this domain. It is widened rather than shadowed by a second type:

```text
EdgeTypeId(u32)
  UNTYPED = 0
  user IDs = 1..=MAX_USER_LABEL_ID
  SENTINEL = u32::MAX
```

The maximum is frozen by explicit dictionary count/byte policy and artifact
evidence. Raw `u8`, `u16`, or `u32` conversions occur only in checked
storage adapters. Query and planner code use `EdgeTypeId`.

Logical `UNTYPED` is 0 and logical `SENTINEL` is `u32::MAX`; neither sentinel is
stored as a user edge type. Each physical width reserves its all-ones encoding
as invalid. A one-byte section therefore stores user IDs through 254, a
two-byte section through 65,534, and a four-byte section through the configured
maximum below `u32::MAX`. The writer widens before an ID reaches the reserved
physical code, and the reader rejects that code in an edge record.

The registry owns both:

- ordered `Vec<String>`/equivalent ID-to-label storage; and
- an O(1) label-to-ID map.

Source spelling remains authoritative in PostgreSQL and is preserved exactly.

## Physical representation decision

The preferred immutable CSR format stores one declared width for the type-ID
section:

| Registry range | Width |
|---|---|
| IDs fit the one-byte encoding | 1 byte |
| IDs fit the two-byte encoding | 2 bytes |
| Otherwise within the public bound | 4 bytes |

This preserves current edge density for ordinary graphs instead of adding
three bytes per directed edge and per backend's anonymous artifact mapping.
The loader exposes a typed `EdgeTypeId` view/iterator rather than `&[u8]`.

Mutable segment and transaction-delta records may store fixed `u32` label IDs
to keep mutation logic simple. Their exact record sizes and all memory/disk
accounting must be updated together.

## Persistence and migration

- The v7 base artifact stores variable-width type IDs with checked
  `edge_count * width`, alignment, bounds, endian, and checksum validation.
- Load v6 `u8` IDs through checked conversion. If a particular
  artifact cannot be safely consumed, return a targeted rebuild diagnostic and
  retain the last valid generation.
- Persist the label dictionary in the checksummed generation artifact
  referenced by the manifest, validation, recovery planner, retention graph,
  and garbage collector.
- Publish base/segments/dictionary/manifest atomically under the existing
  per-graph writer lock.
- P8 will decode older segment fixtures through checked widening and emit the
  adaptive segment format after migration.

## Incremental and transaction-local labels

Durable sync carries authoritative label text until publication. Under the
writer lock it loads the cumulative dictionary, interns unseen labels in a
deterministic order, maps rows to `EdgeTypeId`, and publishes the dictionary with
the new segment.

Transaction-local changes cannot mutate the loaded base registry irreversibly.
The transaction delta owns appended label spellings and resolves a combined
base-plus-transaction view. Savepoint snapshots, rollback, release, and top-level
transaction cleanup include this appended dictionary. Committed ingestion may
remap temporary backend-local IDs from the authoritative strings.

## Cross-cutting migration inventory

The implementation audit must cover at least:

- raw/build edges and sort/run codecs;
- outbound and inbound CSR stores and persisted scanners;
- neighbor records and `NeighborSource` implementations;
- BFS, DFS, bidirectional and weighted paths;
- components, aggregation, GQL execution, filters, and edge-path output;
- relationship visibility type sets and missing-identity validation;
- edge buffers, layered segments, normalization, transaction deltas, sync,
  durable ingestion, compaction, repair, reload, and recovery;
- resource size estimates and status/debug output; and
- every narrowing cast and corruption/fuzz fixture.

## Resource contract

Freeze and enforce:

- maximum distinct labels per graph;
- maximum UTF-8 bytes per label;
- maximum cumulative dictionary bytes;
- build/run spill and merge budgets;
- incremental dictionary growth per sync/publication;
- transaction-local label count/bytes;
- type-filter input count/bytes; and
- relationship-type listing/status pagination or output bounds.

Every count/byte product uses checked arithmetic before allocation. Exceeding a
budget returns a stable typed diagnostic; IDs never wrap or alias.

## Query semantics

- SQL traversal and typed shortest paths resolve text labels once and compare
  compact IDs in the hot loop.
- Supported GQL relationship patterns retain exact semantics at high
  cardinality.
- Equality predicates on an eligible registered label column may lower to the
  same compact filter. General relationship-property predicates remain governed
  by the documented GQL profile.
- Unknown labels preserve the existing invalid-input behavior unless the
  operation is an authoritative mapped write allowed to create a new label.
- RLS continues to admit relationship source identities before type filters.

## Evidence

- boundary tests at 254, 255, 65,535, 65,536, and the configured maximum;
- exact traversal/path/GQL results above both historical width boundaries;
- new-label build, sync, transaction, savepoint, reload, compaction, and repair;
- parallel relationships and identical endpoints/types with stable identity;
- ACL, RLS, FORCE/BYPASS, cancellation, concurrency, and failed publication;
- corrupt width/dictionary/count/checksum/segment fuzz cases; and
- benchmarks comparing current `u8`, adaptive 1/2/4-byte, and fixed `u32`
  storage across label cardinality, selectivity, degree, depth, latency,
  throughput, RSS/PSS, artifact size, and backend count.

The adaptive representation becomes the default only if normal low-cardinality
hot traversal remains within the accepted regression budget recorded before
implementation.
