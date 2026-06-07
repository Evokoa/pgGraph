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

Microphase 7 implemented the layered runtime checkpoint:

- Added `graph/src/projection/layered.rs` with a pure layered read source that
  merges base CSR neighbors, durable edge insert/delete/weight segments,
  durable node visibility and tenant membership segments, and transaction-local
  edge deltas in deterministic order.
- Added a segment-provider boundary for real manifest-backed segment loading
  while keeping public Engine read-path selection deferred to Microphase 8.
- Extended the shared neighbor iterator with an owned variant for merged
  layered results and turned the layered-neighbor contract green. The remaining
  default-red contract now tracks status and diagnostics only.
- Fixed independent-review findings before promotion: transaction-local node
  tombstones now suppress layered sources and targets, transaction-local
  weighted edge inserts preserve weights, manifest-backed segment loading
  verifies manifest CRC32 checksums, and the gate includes real-provider plus
  proptest coverage.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 projection::layered`: passed with
    12 layered-runtime tests covering full-rebuild equivalence, transaction
    delta precedence, inbound direction, duplicate suppression, weighted
    durable edges, tenant filtering, node visibility, provider loading, and
    durable delete/reinsert ordering.
  - `cd graph && cargo test --features pg17 projection::neighbors`: passed
    with 3 neighbor-source tests.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 5 passed, 1 failed for the future status/diagnostics
    contract.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed with 0 doctests.
  - `python3 scripts/check_doc_references.py`: passed.
  - `git diff --check`: passed.
  - `cd graph && cargo test --features pg17`: expected red; 580 passed, 1
    failed future status/diagnostics contract, 1 ignored scale test.
- Regression report: the runtime merge implementation is production-visible but
  not yet selected by SQL traversal, GQL, components, or shortest-path reads.
  Benchmark baseline remains `pre_durable_projection`; read-path regression
  benchmarking is deferred to Microphase 8 when Engine adoption changes query
  behavior.

Microphase 8 routed public read paths through segment-backed layered snapshots:

- `Engine` now stores the full projection manifest and artifact root, builds
  manifest-backed `LayeredNeighbors` for segment-backed mutable-overlay reads,
  and keeps `csr_readonly` plus base-only manifests on the CSR fast path.
- Traversal, DFS, unweighted shortest path, weighted shortest path, connected
  components, and read-only GQL relationship expansion now select layered
  neighbors when a loaded manifest references durable segments. Transaction
  deltas remain the final read-your-own-writes layer.
- Independent-review fixes keep committed `Engine.edge_buffer` overlays visible
  while segment-backed layered snapshots are active and use the reverse CSR
  store for inbound layered base reads instead of scanning the full base graph
  on each inbound lookup. Segment files are still decoded per read; that
  performance follow-up is deferred to the Microphase 12 benchmark/caching pass.
- Segment-backed `.pggraph` reloads now activate mutable-overlay read mode so
  loaded durable segments are not bypassed by the default CSR-only engine mode.
- Added pgrx coverage that builds persisted mutable graphs, publishes real L0
  segments through `graph.ingest_projection()`, reloads the backend engine, and
  verifies traversal, shortest path, weighted shortest path, components, and
  GQL consume the durable snapshot.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 layered_manifest_snapshot`:
    passed with 6 layered read-adoption tests.
  - `cd graph && cargo test --features pg17 projection::layered`: passed
    with 12 layered-runtime tests.
  - `cd graph && cargo test --features pg17
    layered_manifest_preserves_pending_edge_buffer_overlay`: passed with 4
    regression tests covering traversal, shortest path, components, and GQL.
  - `cd graph && cargo test --features pg17
    traversal_in_direction_uses_layered_base_reverse_csr`: passed.
  - `cd graph && cargo test --features pg17 transaction_delta_edge_overlay`:
    passed.
  - `cd graph && cargo test --features pg17
    persistence::tests::engine_loads_segment_backed_projection_manifest`:
    passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17
    traversal_and_shortest_path_use_layered_manifest_snapshot`: passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17
    weighted_shortest_path_uses_layered_manifest_snapshot`: passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17
    connected_components_use_layered_manifest_snapshot`: passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17
    gql_relationship_expansion_uses_layered_manifest_snapshot`: passed.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 5 passed, 1 failed for the future status/diagnostics
    contract.
  - `cd graph && cargo test --features pg17`: expected red; 592 passed, 1
    failed future status/diagnostics contract, 1 ignored scale test.
  - `cd graph && cargo test --features pg17 --doc`: passed with 0 doctests.
  - `python3 scripts/check_doc_references.py`: passed.
- Regression report: Microphase 8 changes traversal, GQL, shortest-path,
  weighted-path, and component read selection. Criterion comparison against
  `pre_durable_projection` was run for this checkpoint and recorded in
  `todo/regression_report.md`.

Microphase 9 added base chunk rewrite and targeted repair:

- Added `graph/src/projection/chunk.rs` as the testable base chunk publication
  boundary. It builds full replacement edge chunks for dirty source-node
  ranges, publishes the next manifest generation, records replaced chunk files
  as obsolete, and repairs corrupted chunk files by publishing a fresh
  replacement generation.
- Extended manifest chunk references with dirty source/edge pressure counters
  and tightened chunk validation to require non-empty source ranges.
- Extended manifest-backed `LayeredNeighbors` loading so active base chunks
  replace covered source-node ranges while preserving old-generation chunk
  readability, inbound equivalence, durable segment overlays, committed
  `Engine.edge_buffer` overlays, and transaction-local deltas.
- Partial dirty rewrites now expand across overlapping existing chunks so a
  later targeted rewrite does not discard still-valid portions of a previous
  base replacement.
- Independent-review fixes ensure base chunks preserve unchanged unweighted
  edges inside rewritten ranges and reject non-outbound chunk files before they
  can suppress covered base rows.
- Tests run:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 base_chunk_`: passed with 7 base
    chunk manifest, rewrite, old-generation, overlap, malformed-chunk, and
    repair tests.
  - `cd graph && cargo test --features pg17 projection::manifest`: passed
    with 13 manifest tests.
  - `cd graph && cargo test --features pg17 projection::layered`: passed
    with 12 layered-runtime tests.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    expected red; 5 passed, 1 failed for the future status/diagnostics
    contract.
  - `cd graph && cargo test --features pg17`: expected red; 599 passed, 1
    failed future status/diagnostics contract, 1 ignored scale test.
  - `cd graph && cargo test --features pg17 --doc`: passed with 0 doctests.
  - `python3 scripts/check_doc_references.py`: passed.
  - `git diff --check`: passed.
- Regression report: Microphase 9 adds an inactive write/repair boundary plus
  manifest-backed read semantics for chunked generations. It does not change
  default CSR/base-only reads or add a SQL scheduling path yet, so no new
  benchmark comparison is required until compaction/GC or SQL repair scheduling
  makes chunk rewrite operationally active.

Microphase 10 added compaction publication:

- Added `graph/src/projection/compact.rs` with bounded compaction over active
  manifest edge segments. L0 fanout compacts to L1, L1 fanout compacts to L2,
  and tombstone/delete precedence is preserved by materializing the previous
  layered view and writing a compacted delta against base CSR.
- Added dirty chunk pressure handling: when the configured segment threshold is
  reached, compaction publishes base chunk replacements and drops the compacted
  segment fanout from the new manifest.
- Independent-review fixes ensure compaction preserves non-edge manifest
  segments, carries durable edge weights through segment and chunk compaction,
  and normalizes overlapping dirty source ranges before chunk publication.
- Added row, byte, segment-count, and elapsed-time budget checks that fail
  before manifest publication so the previous generation remains current.
- Tests run:
  - `cd graph && cargo test --features pg17 projection::compact`: passed with
    9 compaction tests.
  - `cd graph && cargo test --features pg17 compaction_`: passed with the 9
    compaction tests plus the existing compaction-lock ingestion test.
  - `cd graph && cargo test --features pg17 base_chunk_`: passed with 7 chunk
    rewrite/repair tests.
  - `cd graph && cargo test --features pg17 projection::layered`: passed with
    12 layered-runtime tests.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed with 0 doctests.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cd graph && cargo test --features pg17`: expected red; 608 passed, 1
    failed future status/diagnostics contract, 1 ignored scale test.
- Regression report: Microphase 10 changes opt-in compacted manifest artifacts
  but does not alter default CSR/base-only reads or SQL scheduling. No benchmark
  comparison is required until scheduled maintenance invokes compaction in a
  production-visible path.

Microphase 11 added active generation heartbeats:

- Extended active-generation heartbeat SQL helpers so backend rows are
  idempotently recorded/refreshed with backend PID, database OID, manifest
  generation, heartbeat timestamp, expiry timestamp, sync watermark, and
  validation status.
- Added stale heartbeat expiry and a generation-active predicate that
  generation-aware GC can use to refuse files still referenced by a live
  backend.
- Installing a manifest snapshot now records the current backend's active
  generation heartbeat immediately. `refreshed_engine_status()` also expires
  stale heartbeat rows and refreshes the installed manifest heartbeat when a
  backend continues using the generation.
- Because pgrx rejects more than 32 tuple fields for `graph.status()`,
  `graph.active_generation_count()` exposes the active generation count without
  changing the existing `graph.status()` return ABI.
- Tests run:
  - `cd graph && cargo test --features pg17 projection::manifest`: passed with
    13 manifest/heartbeat tests.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_generation_heartbeat`:
    passed with backend record, refresh, stale expiry, GC-blocking, and unit
    heartbeat expiry tests.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_mode_build_and_status_contract`:
    passed with the status ABI preserved.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 sync_health_exposes_operator_contract_field_names`:
    passed with the sync-health ABI preserved.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed with 0 doctests.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cd graph && cargo test --features pg17`: expected red; 608 passed, 1
    failed future status/diagnostics contract, 1 ignored scale test.
- Regression report: Microphase 11 changes metadata/status paths only. No BFS
  or read-path benchmark comparison is required; preserve the existing
  `graph.status()` ABI and verify SQL callers before promotion.
