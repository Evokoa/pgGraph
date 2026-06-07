# Mutable Projection Progress

## 2026-06-07

Completed planning documentation, and recorded initial baseline for regression
testing.

Microphase 0 started and completed the test-harness checkpoint:

- Added test-only projection fixture helpers for temporary artifact
  directories, manifest and segment paths, synthetic sync rows, normalized
  mutations, CSR construction, and full-neighbor equivalence checks.
- Added the six ignored contract tests named by `build_order.md`; running them
  with `--ignored` fails because the targeted production modules do not exist
  yet.
- Tests run:
  - `cd graph && cargo fmt --check` before edits: passed.
  - `cd graph && cargo test --features pg17 projection::` before edits:
    passed.
  - `cd graph && cargo test --features pg17 projection::` after edits:
    passed, with 21 passed and 6 ignored.
  - `cd graph && cargo test --features pg17 projection::test_contracts --
    --ignored`: failed as expected with each contract reporting the missing
    production feature.
- Regression report: no runtime or memory-sensitive code changed in this
  checkpoint; existing `pre_durable_projection` baseline remains current.

Microphase 1 implemented the manifest and generation-table checkpoint:

- Added `graph/src/projection/manifest.rs` with the JSON manifest model,
  validation, pretty JSON encoding/decoding, base-only manifest constructor,
  segment/chunk/obsolete-file references, and active-generation heartbeat
  model helpers. Manifest JSON decoding rejects unknown top-level and nested
  fields so schema/version drift fails closed.
- Added extension-build SQL helpers for active-generation heartbeat upsert and
  stale heartbeat expiration. Heartbeat upserts preserve subsecond TTLs and
  refresh diagnostic `updated_at` metadata on conflict.
- Added `graph._projection_generations` to the bootstrap SQL with generation,
  backend PID, database OID, heartbeat, sync watermark, validation, repair,
  current-generation, retention, and timestamp fields.
- Updated public contributor docs to describe projection generation metadata as
  extension-owned operational state while PostgreSQL source tables remain
  authoritative.
- Changed durable projection contract tests to run by default. Implemented
  contracts now pass; future-phase contracts fail normally until their modules
  exist.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 projection::manifest`: passed
    with 7 manifest/heartbeat tests.
  - `cd graph && cargo test --features pg17 projection::`: passed before
    making future contracts default-red.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 1 passed, 5 failed for future production features.
  - `cd graph && cargo test --features pg17`: expected red; 528 passed, 5
    failed future contracts, 1 ignored scale test.
  - `git diff --check`: passed.
- Regression report: no traversal, ingestion, compaction, GC, or runtime
  read-path code changed; benchmark baseline remains `pre_durable_projection`.

Microphase 2 implemented the atomic manifest publish/load checkpoint:

- Added `ProjectionManifestStore` for durable manifest files rooted in the
  projection artifact directory.
- Added generation manifest filename parsing and final-path construction using
  the existing `projection-generation-{generation}.json` convention.
- Added atomic publish with temp-file creation, file fsync, directory fsync,
  rename, final directory fsync, and bounded temp-name collision retry.
- Added latest-generation loading that ignores unrelated and temporary files,
  decodes and validates the selected manifest, and rejects missing active base,
  segment, or chunk references.
- Tightened artifact ownership after review: manifest references must remain
  relative to the artifact directory, generation manifest files are immutable,
  and publish reloads the renamed final manifest before reporting success.
- Updated contributor docs to describe final-manifest selection, temp-file
  ignore policy, atomic publish steps, and active-reference validation.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 projection::manifest`: passed
    with 13 manifest, heartbeat, publish, and load tests.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 1 passed, 5 failed for future production features.
  - `cd graph && cargo test --features pg17`: expected red; 534 passed, 5
    failed future contracts, 1 ignored scale test.
  - `git diff --check`: passed.
- Regression report: no traversal, ingestion, compaction, GC, SQL, or runtime
  read-path code changed; benchmark baseline remains `pre_durable_projection`.

Microphase 3 implemented the complete segment-format checkpoint:

- Added `graph/src/projection/segment.rs` with a fixed little-endian delta
  segment header carrying magic bytes, version, kind, level, direction,
  source-node range, row counts, tombstone-capable sections, sync watermark,
  payload offsets, CRC32 checksum, and zeroed reserved bytes.
- Implemented writer/loader support for edge topology inserts, edge deletes,
  edge weights, node active/tombstone deltas, resolution deltas, filter deltas,
  and tenant membership deltas.
- Added total loader validation for magic, version, checksum, contiguous
  offsets, reserved flags, section ownership, source-range row bounds, and
  boolean encodings.
- Corrected segment source-range semantics after review: edge sections shard by
  source only, so targets may point outside the segment source range.
- Turned the two segment contract tests green; the remaining default-red
  contracts now track ingestion, layered reads, and status/diagnostics.
- Added projection segment and manifest fuzz targets plus seed corpus entries
  for edge segments, node segments, and a base-only manifest.
- Updated public contributor docs with the segment module and binary validation
  contract.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 projection::segment`: passed
    with 6 segment tests.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 3 passed, 3 failed for future production features.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cargo check --manifest-path graph/fuzz/Cargo.toml`: passed
    with existing fuzz-build dead-code warnings in sync helpers.
  - `cd graph && cargo test --features pg17`: expected red; 542 passed, 3
    failed future contracts, 1 ignored scale test.
  - `git diff --check`: passed.
- Regression report: no traversal, ingestion, compaction, GC, SQL, or runtime
  read-path code changed; benchmark baseline remains `pre_durable_projection`.

Microphase 4 implemented the mutation-normalization checkpoint:

- Added `graph/src/projection/normalize.rs` with committed mutation rows,
  normalized mutation rows, deterministic grouping, insert/delete cancellation,
  delete precedence, and bounded row/byte ingestion-buffer checks.
- Grouping is by generation, direction, source, target, and edge type so
  interleaved sync-log ids still normalize deterministically; output rows are
  sorted by generation, sync-log id, source, direction, edge type, target, and
  tombstone state.
- Added `DeltaSegment::from_normalized_edges` so edge segment construction can
  consume normalized edge batches directly and reject normalized node rows.
- Added unit tests and a proptest covering deterministic output, cancellation,
  delete precedence, grouping by direction/type, duplicate sync-id tie breaks,
  node/edge domain separation, node operations, and oversized buffer rejection.
- Updated public contributor docs with the normalization boundary and buffer
  rejection policy.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 projection::normalize`: passed
    with 9 normalization tests including a proptest.
  - `cd graph && cargo test --features pg17 projection::segment`: passed
    with 8 segment tests, including normalized edge segment construction and
    node-row rejection.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 3 passed, 3 failed for future production features.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cargo check --manifest-path graph/fuzz/Cargo.toml`: passed
    with existing fuzz-build dead-code warnings in sync helpers.
  - `cd graph && cargo test --features pg17`: expected red; 553 passed, 3
    failed future contracts, 1 ignored scale test.
- Regression report: no traversal, ingestion, compaction, GC, SQL, or runtime
  read-path code changed; benchmark baseline remains `pre_durable_projection`.

Microphase 5 implemented the base-only engine manifest-load checkpoint:

- Added backend-local projection manifest snapshot state to `Engine` with base
  manifest generation and sync watermark metadata.
- Added base-only manifest discovery during `.pggraph` artifact load. The
  loader scans the artifact directory, validates the latest manifest through
  `ProjectionManifestStore`, requires a base-only manifest, checks the manifest
  base artifact version and checksum, and rejects manifests that do not
  reference the loaded `.pggraph` file.
- Kept CSR as the active read path for base-only manifests and added a
  traversal regression proving loaded base-only manifests preserve neighbor
  results.
- Documented that SQL `graph.status()` is already at pgrx's tuple-return arity
  limit, so the new Rust `EngineStatus` base-manifest fields are not exposed as
  SQL columns until the later status/diagnostics SQL-shape refactor.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 persistence::tests::`: passed
    with 34 persistence/load-path tests, including base-only manifest load, CSR
    traversal preservation, status metadata, wrong-base rejection, stale
    checksum rejection, wrong-version rejection, and non-base-only rejection.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 3 passed, 3 failed for future production features.
  - `cd graph && cargo test --features pg17`: expected red; 560 passed, 3
    failed future contracts, 1 ignored scale test.
- Regression report: no traversal algorithm, GQL, components, shortest-path,
  ingestion, compaction, GC, or SQL read-path adoption changed; benchmark
  baseline remains `pre_durable_projection`.

Microphase 6 core ingestion checkpoint implemented testable L0 publication:

- Added `graph/src/projection/ingest.rs` with a core projection ingester that
  filters committed rows above the current manifest watermark, ignores aborted
  rows, normalizes edge and node-surface mutations, writes L0 edge segments by
  direction, writes node/resolution/filter/tenant deltas into node segments,
  durably publishes no-overwrite segment files, validates segment reloads, and
  publishes the next manifest generation under an artifact-root ingestion lock.
- Turned the committed-edge ingestion contract green while leaving layered
  neighbor reads and status/diagnostics contracts intentionally failing by
  default for their later phases.
- Kept SQL `graph.ingest_projection(...)`, scheduled maintenance wiring, source
  table/GQL sync-log extraction, and rollback-heavy pgrx tests for the next
  Microphase 6 slice.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 projection::ingest`: passed with
    6 core ingestion tests.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 4 passed, 2 failed for future production features.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cd graph && cargo test --features pg17`: expected red; 567 passed, 2
    failed future contracts, 1 ignored scale test.
- Regression report: the checkpoint adds test/development-gated core artifact
  publication logic but does not yet change SQL, scheduled maintenance,
  traversal reads, GQL, components, shortest-path, compaction, GC, or runtime
  read-path adoption; benchmark baseline remains `pre_durable_projection`.

Microphase 6 SQL ingestion checkpoint wired committed sync-log publication:

- Added production visibility for projection ingestion, normalization, and
  segment modules, plus artifact checksum/version helpers and read-only
  resolution helpers needed to resolve tombstoned nodes after `apply_sync()`.
- Added `graph.ingest_projection(max_rows bigint DEFAULT NULL, max_bytes bigint
  DEFAULT NULL)`, committed `graph._sync_log` conversion into edge, node,
  resolution, filter, and tenant `ProjectionSyncRow` values, persisted-base
  manifest publication, and scheduled-maintenance ingestion after sync apply.
- Kept `graph.apply_sync()` and backend-local `Engine.edge_buffer` behavior
  active; durable segments are published but not yet consumed by runtime reads.
- Preserved the default-red feature-contract policy: the two remaining future
  contracts still fail normally rather than being ignored.
- Tests run:
  - `cd graph && cargo fmt`: passed.
  - `cd graph && cargo test --features pg17 projection::ingest -- --list`:
    passed and confirmed the unit-test binary no longer aborts on pgrx symbols.
  - `cd graph && cargo test --features pg17 projection::ingest`: passed with
    6 core ingestion tests.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17
    ingest_projection`: passed with 3 pgrx ingestion tests.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17
    scheduled_maintenance`: passed with 6 tests.
  - `cd graph && cargo test --features pg17`: expected red; 567 passed, 2
    failed future contracts, 1 ignored scale test.
- Regression report: SQL ingestion now writes durable projection artifacts on
  explicit calls and scheduled maintenance when a persisted base artifact
  exists. Runtime traversal, GQL, components, shortest-path, compaction, GC, and
  durable read-path adoption remain unchanged; benchmark baseline remains
  `pre_durable_projection`.
