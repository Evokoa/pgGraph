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

Microphase 12 added generation-aware projection GC:

- Added `graph/src/projection/gc.rs` with a metadata-only scanner for valid
  projection manifests. It protects references from the newest retained valid
  generations and any generation with an unexpired active-backend heartbeat.
- GC candidates are limited to manifest-declared `obsolete_files`; missing
  obsolete files are ignored so repeated cleanup is idempotent, and current
  manifests are not rewritten during deletion.
- Added `graph.projection_retention_generations` as the GUC-backed retention
  floor and `graph.projection_gc()` as the admin-facing cleanup entry point.
- Added GC tests for referenced-file refusal, active-generation refusal,
  unmatched active-generation fail-closed behavior, obsolete unreferenced
  segment deletion after retention, and crash shape that preserves the current
  generation.
- Independent-review fixes ensure active heartbeat generations are collected
  before deletion and GC refuses to proceed if an active generation no longer
  has a valid manifest to supply protected references. SQL-level tests now
  cover the `graph.projection_gc()` behavior path, and the pgrx GUC contract
  includes the new retention setting and range.
- Tests run:
  - `cd graph && cargo test --features pg17 projection::gc`: passed with 5 GC
    unit/crash-shape tests.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_gc`:
    passed with 5 GC unit tests plus SQL deletion/idempotence and signature
    coverage.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 guc_contract_defaults_ranges_and_contexts_are_registered`:
    passed with the retention GUC registered at default 2 and range 1-1000.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_generation_heartbeat`:
    passed with heartbeat record, refresh, stale expiry, and active-generation
    predicate coverage after GC began scanning active generation IDs.
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed with 0 doctests.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cd graph && cargo test --features pg17`: expected red; 613 passed, 1
    failed future status/diagnostics contract, 1 ignored scale test.
- Regression report: Microphase 12 changes projection artifact metadata
  cleanup and a new admin SQL function. No traversal benchmark comparison is
  required for the checkpoint because read-path code is unchanged; preserve the
  expected-red future status/diagnostics contract before commit.

Microphase 13 added recovery and repair orchestration:

- Added `graph/src/projection/recovery.rs` to validate the active manifest plus
  referenced segments/chunks and classify recovery as `healthy`,
  `targeted_chunk_repair`, `full_rebuild`, or `no_projection`.
- Missing referenced segment files remain rejected through manifest publication
  validation, while unreferenced temp segment files are ignored by the active
  manifest loader.
- Corrupt active segments and corrupt manifests plan full rebuild. Full rebuild
  repair quarantines the latest final manifest before calling the persisted
  maintenance rebuild path, publishes a higher base-only projection generation,
  reloads it, and records the active generation heartbeat.
- Corrupt base chunks use the existing chunk rewrite repair path from a base
  graph source and publish a replacement generation.
- Added `graph.projection_repair()` as the admin-facing repair entry point.
- Tests run so far:
  - `cd graph && cargo test --features pg17 projection::recovery`: passed with
    8 recovery/repair tests, including stale base metadata and missing chunk
    targeted repair coverage.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_repair`:
    passed with SQL signature and targeted chunk repair coverage.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 full_rebuild_restores_valid_projection_generation`:
    passed with SQL full-rebuild repair behavior and recovery unit coverage.
  - `PG_VERSION_FEATURE=pg17 graph/tests/heavy/projection_recovery_gate.sh`:
    passed outside the sandbox after the sandboxed pgrx run could not bind an
    ephemeral local test port; the script covers recovery manifest/segment/chunk
    cases, GC crash-shape/idempotence unit cases, SQL repair signature and
    targeted chunk repair coverage, and SQL full-rebuild restoration.
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed with 0 doctests.
  - `python3 scripts/check_doc_references.py`: passed.
  - `git diff --check`: passed.
  - `cd graph && cargo test --features pg17`: expected red; 621 passed, 1
    failed future status/diagnostics contract, 1 ignored scale test.
- Regression report: Microphase 13 changes admin repair and projection artifact
  metadata paths. No traversal benchmark comparison is required because normal
  read execution is unchanged; preserve the expected-red future
  status/diagnostics contract before commit.

Microphase 14 added durable projection status and diagnostics:

- Added `graph/src/projection/status.rs` to summarize the latest durable
  projection manifest, watermark, pending durable rows, segment counts/bytes by
  level and kind, dirty chunk counts/bytes, tombstone ratio, compaction backlog,
  obsolete files/bytes, active generation count, artifact validation state, and
  ingest/compaction/GC/repair recommendations.
- Added `graph.projection_status()` as the full projection-specific SQL
  diagnostic row because `graph.status()` remains at pgrx's 32-field tuple
  limit. `graph.sync_health()` now exposes compact durable recommendation
  booleans for scheduler decisions through a metadata-only collector, so
  scheduler polling does not decode projection artifacts.
- Persisted operation timestamps for ingestion, compaction, GC, and repair
  diagnostics. Ingestion/compaction/repair timestamps are carried forward in
  manifest metadata, and successful GC records a small durable status sidecar.
- Turned the final durable projection contract green:
  `status_reports_manifest_watermark_segments_chunks_gc_and_repair`.
- Added pgrx coverage for projection status signature, active generation
  heartbeat counts, durable pressure in `sync_health()`, and recommendation
  thresholds for ingest, compaction, GC, and repair.
- Tests run so far:
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo test --features pg17 projection::status`: passed.
  - `cd graph && cargo test --features pg17 projection::test_contracts`:
    passed with all 6 durable projection contract tests green.
  - `cd graph && cargo test --features pg17 projection::manifest`: passed with
    13 manifest tests after adding defaulted operation timestamp metadata.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_status`:
    passed with SQL signature coverage.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 sync_health_exposes_operator_contract_field_names`:
    passed with the extended sync-health ABI.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 sync_health_distinguishes_tx_delta_edge_buffer_and_durable_projection_pressure`:
    passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 status_reports_active_generation_heartbeat_count`:
    passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 status_recommends_ingest_compaction_gc_or_repair_by_threshold`:
    passed.
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo test --features pg17 --doc`: passed with 0 doctests.
  - `git diff --check`: passed.
  - `scripts/check_docs_drift.sh`: passed after documenting
    `graph.ingest_projection()`.
  - `python3 scripts/check_doc_references.py`: passed.
  - `cd graph && cargo test --features pg17`: passed with 623 tests, 1 ignored
    scale test.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 status`:
    passed with 13 status-family tests.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 sync_health`:
    passed with 3 sync-health tests.
- Independent review found and the implementation fixed misleading `last_*`
  timestamp derivation and heavyweight `sync_health()` polling before commit.
- Regression report: Microphase 14 changes status/diagnostic metadata and
  scheduler recommendation surfaces. It does not alter traversal read
  execution; no benchmark comparison is required because `sync_health()` now
  uses metadata-only projection status; revisit if full
  `graph.projection_status()` polling becomes a measured hot path.

Microphase 15 is in progress with correctness blockers fixed and regression
signoff resolved by same-environment benchmark follow-up:

- Routed `graph.apply_sync()` for persisted `mutable_overlay` graphs through
  durable projection ingestion instead of committed backend-local
  `Engine.edge_buffer` overlays. A fresh backend now loads the persisted graph
  artifact before applying sync, uses the persisted projection-mode sidecar
  rather than the session default GUC as build-mode truth, publishes committed
  edge mutations into L0 durable segments, reloads the latest manifest, and
  avoids downgrading segment-backed reloads to `csr_readonly`.
- Routed topology read-time `graph.query_freshness = 'apply_pending_sync'`
  catch-up through the same durable-aware high-watermark helper for persisted
  `mutable_overlay` graphs while keeping the transaction-local dirty skip for
  rollback safety.
- Bounded durable ingestion by the captured `_sync_log` high-water mark so
  `graph.apply_sync()` does not publish rows committed after the call started.
- Added a pgrx contract test named
  `cross_backend_committed_write_visible_without_full_rebuild` and a true
  cross-backend heavy script at
  `graph/tests/heavy/cross_backend_durable_projection.sh`.
- Kept transaction-local `projection::tx_delta` behavior separate from
  committed durable projection segments; the tx-delta lifecycle heavy gate
  still passes.
- Updated user and contributor docs for the new committed-edge durable segment
  behavior and the remaining legacy/backend-local edge-buffer semantics.
- Tests run so far:
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo check --features pg17`: passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 cross_backend_committed_write_visible_without_full_rebuild`:
    passed after fixing segment-backed reload mode preservation and persisted
    projection-mode loading.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 topology_auto_sync_uses_durable_segments_for_mutable_overlay`:
    passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 csr_readonly_apply_sync_ignores_later_mutable_default_guc`:
    passed.
  - `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_cross_backend_durable ./tests/heavy/cross_backend_durable_projection.sh`:
    passed outside the sandbox after `cargo pgrx install` required PostgreSQL
    extension-directory write access.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 traverse_auto_sync_opt_in_applies_pending_edge_insert`:
    passed.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_create_node`:
    passed with 6 tests.
  - `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_delete_edge`:
    passed with 6 tests.
  - `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_tx_delta ./tests/heavy/tx_delta_lifecycle.sh`:
    passed.
  - `cd graph && cargo test --features pg17 csr_readonly`: passed with 2
    csr-readonly guard tests.
  - `cd graph && cargo test --features pg17`: passed with 623 tests and 1
    ignored scale test.
  - `cd graph && cargo bench --features pg17 --bench bfs_bench -- --baseline pre_durable_projection`:
    red. Multiple raw BFS, construction, overlay, and filter cases regressed
    against the saved baseline.
  - `cd graph && cargo bench --features pg17 --bench bfs_bench -- bfs_overlay_paths --baseline pre_durable_projection`:
    red on `no_overlay_d3`, `sparse_overlay_d3`, and `dense_overlay_d3`.
  - Same-environment temporary-HEAD overlay comparison using
    `CARGO_TARGET_DIR=/private/tmp/pggraph-micro15-bench-target`: current
    `bfs_overlay_paths/no_overlay_d3` was within noise threshold at +1.19%,
    `sparse_overlay_d3` was within noise threshold at +0.82%, and
    `dense_overlay_d3` improved by -18.71% against the fresh HEAD baseline.
  - Same-environment temporary-HEAD representative raw BFS comparison:
    `bfs_traverse/d1_supernode/500k` showed no change at +0.81% (p = 0.58).
  - `scripts/check_docs_drift.sh`: passed.
  - `python3 scripts/check_doc_references.py`: passed.
  - `git diff --check`: passed.
- Independent review found three blockers: durable ingest ignored the captured
  high-water mark, query-time auto catch-up still used `Engine.edge_buffer`, and
  fresh-backend durable routing used the session default projection-mode GUC
  instead of persisted build-mode state. All three are fixed in the current
  working tree and covered by the tests above.
- Regression report: the old `pre_durable_projection` Criterion comparison was
  red, but a same-environment temporary-HEAD comparison showed no Microphase 15
  regression on the directly relevant overlay group or a representative raw
  BFS red case. Treat the old baseline as stale/environment-sensitive evidence
  and refresh the long-lived baseline in Microphase 16 production verification.

Microphase 16 is in progress with release benchmark coverage started:

- Added visible, non-ignored release-contract tests for the seven Microphase 16
  threshold names:
  `bfs_layered_projection_no_unbounded_regression`,
  `gql_layered_relationship_expansion_no_unbounded_regression`,
  `weighted_path_layered_projection_no_unbounded_regression`,
  `projection_ingest_publish_latency_under_threshold`,
  `projection_compaction_latency_under_threshold`,
  `projection_gc_latency_under_threshold`, and
  `projection_repair_latency_under_threshold`.
- Updated the latency contract tests after independent review so they exercise
  real projection operation boundaries: ingestion publication, compaction
  publication, generation-aware GC deletion, and corrupt base-chunk repair.
- Added benchmark-only support helpers that construct real decoded durable
  projection segments internally and route BFS, GQL-shaped relationship
  expansion, and weighted path checks through the existing `LayeredNeighbors`
  runtime without exposing projection internals through the SQL/API surface.
- Extended `graph/benches/bfs_bench.rs` with
  `layered_projection_release_paths`, covering base-only, small L0, many L0,
  compacted L1, compacted L2, dirty chunk rewrite through base chunks,
  committed overlay on top of durable segments, GQL relationship expansion, and
  weighted path Dijkstra.
- Recorded the new layered projection release baseline in
  `todo/measurements.md` and the regression decision in
  `todo/regression_report.md`.
- Tests and measurements run so far:
  - `cd graph && cargo test --features pg17 bench_support::tests`: passed with
    7 tests.
  - `cd graph && cargo bench --features pg17 --bench bfs_bench -- --list`:
    passed and listed all `layered_projection_release_paths` cases.
  - `cd graph && cargo bench --features pg17 --bench bfs_bench -- layered_projection_release_paths`:
    passed; mean timings were base-only `161.55 us`, small L0 `312.32 us`,
    many L0 `1.3041 ms`, compacted L1 `299.60 us`, compacted L2
    `298.87 us`, dirty chunk rewrite `455.46 us`, tx-delta overlay
    `318.14 us`, GQL expansion `184.89 us`, and weighted path `72.410 us`.
  - `cd graph && cargo fmt --check`: passed.
  - `cd graph && cargo check --features pg17`: passed.
  - `python3 scripts/check_doc_references.py`: passed.
  - `scripts/check_docs_drift.sh`: passed.
  - `git diff --check`: passed.
  - `cd graph && cargo test --features pg17`: passed with 631 tests and 1
    ignored scale test.
- Remaining Microphase 16 blockers: pgrx gate, manifest/segment fuzz seeds,
  heavy release matrix, stable docs merge from `todo/` into `docs/`, and
  `todo/` deletion before release.
