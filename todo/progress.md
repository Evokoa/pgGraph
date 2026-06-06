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
