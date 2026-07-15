# KI-026 Durable Node Identity Plan

This plan closes the correctness gap where durable mutable-overlay ingestion
can consume a sync watermark without representing a node first created after
the persisted base build.

## Invariants

- Every consumed sync row is represented durably or the operation fails
  without advancing the manifest watermark.
- Node identity is the exact registered table OID plus canonical primary-key
  text. Hashes are indexes only and are always verified against exact bytes.
- New durable node slots are contiguous and deterministic. A standalone delete
  never allocates a slot; a node inserted and deleted in one batch retains its
  already allocated slot as inactive. An existing slot never changes identity.
- Publication does not mutate the serving engine before a fully validated
  manifest commits.
- A fresh backend can reconstruct node, resolution, table, filter, exact tenant,
  and relationship endpoint state without consulting process-local history.
- Concurrent publishers serialize the read-current, allocate, validate, and
  publish boundary by graph and cannot assign the same slot independently.
- Registered source writers hold a shared transaction lock, while durable
  ingestion and persisted builds require the corresponding exclusive lock, so
  sequence allocation cannot skip an earlier commit or rollback.
- Durable ingestion verifies that every applicable installed trigger contains
  the current writer barrier. Older definitions fail with upgrade guidance;
  trigger-mode builds refresh them before reading source tables.
- Exact target-table identities never fall through to another registered table.
  Standalone endpoint identity changes fail with rebuild guidance
  until surviving relationship rows can be retargeted incrementally.

## Ordered Checkpoints

1. Define segment v6 exact primary-key and tenant payloads with checked lengths,
   UTF-8 validation, hash validation, byte accounting, fuzz seeds, and safe v5
   compatibility rules.
2. Add a non-serving `ProjectionNodePlanner` that processes sync rows in order,
   resolves exact existing identities, assigns contiguous slots, and supplies
   node/filter/tenant/edge rows without mutating the active engine.
3. Stage manifest replay into owned node, resolution, filter, table-membership,
   and tenant-membership state. Validate all node metadata first, then validate
   edges against the final node boundary, and atomically install staged state.
4. Serialize cross-backend allocation/publication, reload after successful
   direct ingestion, and retain the previous serving generation on every
   validation or publication failure.
5. Make `TRUNCATE` either produce bounded table-node tombstones or fail with
   rebuild guidance; it must never publish an empty consumed watermark.
6. Gate Unicode/composite identities, typed filters, tenants, same-batch edge
   endpoints, PK change, delete/reinsert, multiple batches, reload, publication
   failure, concurrent publishers, corruption, v5 compatibility, and the
   persisted READ COMMITTED/REPEATABLE READ/SERIALIZABLE matrix.

## Exit Evidence

- [x] Direct ingestion followed by a fresh-backend reload returns the same exact
  coordinates, hydrated values, tenant visibility, filters, and topology as
  PostgreSQL source tables on PostgreSQL 17, including a second allocation
  batch.
- [x] No unsupported `TRUNCATE` advances the durable watermark.
- [x] Graph-scoped PostgreSQL locking rejects a competing publisher with
  SQLSTATE `55P03` while preserving the active artifact. The two-publisher gate
  also proves the owner reaches the maximum sync watermark and a retry is empty.
  The same gate proves active writers fail ingestion closed, committed rows are
  ingested after commit, and rolled-back rows are never published.
- [x] Run the lifecycle and lock evidence on PostgreSQL 14-16 and 18 before
  retiring KI-026 from the supported-major release matrix.
- [x] The persisted isolation gate passes durable new-node ingestion and
  repeated builds after KI-027 manifest rebasing on PostgreSQL 14 through 18.

Evidence: [PostgreSQL 14-18 durable projection matrix](../measurements/2026-07-14-durable-projection-matrix.md).
