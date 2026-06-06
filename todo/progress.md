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
