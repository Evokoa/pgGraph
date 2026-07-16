# R3 Bounded Storage Execution Plan

This plan executes the R3 release checklist in
[`02-production-readiness.md`](./02-production-readiness.md). PostgreSQL source
tables remain authoritative. Every build artifact and intermediate run is
derived, checksummed, permission-restricted, disposable, and invisible to
readers until the generation publication boundary accepts it.

## R3A: Coherent Source Boundary

- Acquire the graph build lock, then lock pgGraph registration tables in a
  fixed order before reading the catalog.
- Resolve registered source roots by OID, discover partitions while their
  roots are locked, and lock every source relation in deterministic OID order.
- Reject caller-owned catalog or source write locks so an uncommitted source
  mutation cannot escape PostgreSQL rollback through filesystem publication.
- Reconcile sync triggers, acquire the writer barrier, validate schema and
  caller privileges, and capture source watermark `W0` before scanning.
- Immediately before persistence or backend installation, re-read the catalog,
  schema, caller privileges, and watermark `W1`. Require the same catalog
  fingerprint and `W1 = W0`.

The 1.0 strategy deliberately fences writers for the transaction, so bounded
catch-up contains zero rows. Cross-transaction or resumable snapshots require a
durable delta coordinator and are not used by the 1.0 build path.

## R3B: Governed Build Runs

- Add versioned node, relationship-outbound, relationship-inbound, filter,
  filter-dictionary, and resolution run records with manual little-endian
  codecs. Preserve duplicates and stable source identities.
- Use a 128-byte checksummed run header bound to graph ID, build ID, catalog
  fingerprint, run kind, record count, and payload length.
- Bound record size, collector bytes, read/write buffers, open readers, merge
  fanout, spill files, spill bytes, elapsed time, and interrupt cadence through
  the existing resource governor.
- Merge at a fixed fanout, using one decoded record per input and a bounded
  heap. Validate checksums and sorted order before an input can influence an
  output.
- Create build workspaces with mode `0700` and files with mode `0600`. Cleanup
  partial files on failure and abandoned workspaces only while holding the
  graph build lock.

## R3C: Artifact v5 And Bounded Load

- Replace artifact v4 with a 448-byte, 64-byte-aligned v5 header containing 23
  explicitly sized sections: node metadata; forward CSR; inbound CSR;
  resolution; filter catalog/data/dictionaries; edge-type registry; and
  relationship-identity descriptors/key bytes.
- Pre-compute the checked layout from merged-run summaries, preallocate one
  generation-specific staging artifact, and stream each run directly into its
  final section. Do not construct or retain a complete owned `Engine` on the
  persisted path.
- Validate header/body checksums, gaps, alignments, exact cardinalities, both
  CSR orientations, node and resolution ranges, filter layouts, dictionary
  ordering, UTF-8, and relationship identity before creating mapped stores.
- Map both CSR directions and graph-sized relationship/filter/dictionary data.
  Keep only bounded descriptors and the edge-label registry as private
  metadata.
- Represent filters as an owned or mapped immutable base plus a sparse delta.
  Projection generations clone only mapping metadata and the delta, never the
  complete base index.
- Continue using the governed anonymous read-only snapshot for 1.0 so another
  process cannot invalidate Rust references by modifying an inode. Report the
  snapshot as private mapped residency and report expected filesystem
  page-cache pressure separately.

Artifact v4 and future versions fail closed with rebuild guidance. Derived
artifacts are rebuilt from PostgreSQL; bytes are never silently reinterpreted
or migrated in place.

## R3D: Direct Persisted Build And Publication

- Make persisted build and vacuum scan bounded source batches directly into
  the run collectors. The legacy owned build remains only as the nonpersisted
  path and equivalence oracle until the differential gate is green.
- Stream and fsync a generation-specific v5 candidate, validate it through the
  production loader, repeat the R3A final checks, and publish through the R2D
  graph-scoped compare-and-swap pointer.
- Retain the previous generation for rollback and reader pins. Failed scan,
  spill, merge, write, checksum, fsync, validation, catalog, watermark, or CAS
  steps remove only private staging state.

## Acceptance Gates

- In-memory and spill builds are equivalent for node identity, both CSR
  directions, relationship identity/type/weight/direction, filters,
  dictionaries, tenants, resolution, and representative GQL.
- Tiny budgets force multiple runs and at least three merge passes without
  exceeding configured memory, disk, file-count, row, or time limits.
- Concurrent writers, catalog changes, source DDL, partition-leaf writes, and
  trigger bypass either appear at the declared watermark or fail before
  publication.
- Corrupt/truncated/future runs and artifacts fail before typed mapped views are
  constructed.
- ENOSPC, quota, interrupt, write, fsync, checksum, validation, and CAS faults
  preserve the prior serving generation.
- Repeated build/load/sync/compact/drop and failed-build cycles show no upward
  RSS/PSS or staging/artifact file-count trend.
- Formatting, Clippy, rustdoc, Rust/PostgreSQL tests, compatibility drift,
  supported-major publication checks, sanitizer/fuzz checks, playground, and
  the full release gate pass before R3 closes.
