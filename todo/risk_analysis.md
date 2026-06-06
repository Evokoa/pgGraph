# Mutable Projection Risk Analysis

This document lists the benefits, tradeoffs, failure modes, and test gates for
the complete production plan in `todo_overview.md`.

The core risk is adding a durable projection publication layer while PostgreSQL
source tables remain authoritative. The production design is acceptable only
when every projection artifact is derived state, every read observes one stable
manifest generation, and every corrupt or partial artifact enters a typed repair
or rebuild path.

## Main Pros

| Benefit | Why it matters |
|---|---|
| Cross-backend committed delta visibility | Durable segments let all backends observe committed topology changes without waiting for full rebuild. |
| Lower full-rebuild pressure | L0/L1/L2 delta CSR segments and dirty chunk rewrites absorb write bursts while keeping the base CSR fast path. |
| Atomic read publication | Manifest generations give readers stable snapshots and keep half-written files invisible. |
| Complete projection semantics | Production segments cover topology, weights, node active state, resolution, filters, and tenant membership. |
| Incremental repair | Chunk metadata enables targeted repair before full projection rebuild. |
| Safe cleanup | Active-generation heartbeat and retained manifest scanning let GC delete only truly obsolete artifacts. |
| Operator visibility | Watermarks, segment pressure, chunk pressure, compaction backlog, obsolete bytes, active generations, and repair state are visible from SQL. |
| Executable correctness invariant | Layered reads are continuously checked against full CSR rebuild output for the same PostgreSQL state. |

## Main Cons

| Cost | Impact |
|---|---|
| More artifact formats | JSON manifests and binary segment files require strict validation, versioning, fuzzing, and compatibility tests. |
| More read-path complexity | Graph reads must merge base chunks, durable inserts, durable deletes, weights, visibility deltas, tenant/filter deltas, and tx deltas deterministically. |
| More write-path complexity | Ingestion must convert sync rows into multiple segment kinds and publish a single atomic generation. |
| More operational concepts | Operators must understand manifest generation, watermarks, active generation heartbeats, compaction, GC, and repair status. |
| Higher disk footprint | Retained generations and obsolete files remain on disk until heartbeat and retention rules allow deletion. |
| More crash states | Ingestion, manifest publish, compaction, chunk rewrite, repair, and GC all have interruption cases. |
| Migration burden | Existing `Engine.edge_buffer` behavior remains until durable segments prove production equivalence, so both paths coexist during implementation. |

## What Can Go Wrong

### Correctness Risks

| Risk | Expected symptom | Required mitigation and tests |
|---|---|---|
| Manifest watermark advances before all rows are represented | Queries miss committed edges while status reports fresh projection | Publish watermark only after every referenced segment validates; add `projection_manifest_watermark_advances_only_after_publish`. |
| Aborted transaction reaches durable segment | Rolled-back source-table or GQL write appears in graph reads | Ingest only committed `graph._sync_log` rows; add rollback lifecycle heavy tests. |
| Delete precedence is wrong | Deleted base or segment edge still appears | Define generation and sync-log ordering in `normalize.rs`; add delete-precedence proptests. |
| Duplicate suppression is wrong | Traversal returns duplicate neighbors or paths | Normalize segment content and test layered output against full rebuild. |
| Direction handling diverges | Inbound or any-direction traversals differ from rebuilt CSR | Store forward and reverse segment sections; test `Out`, `In`, and `Any` directions. |
| Edge type filtering diverges | GQL relationship expansion returns wrong relationship types | Include edge type coverage in segment metadata and SQL relationship expansion tests. |
| Weight segments lag topology segments | Weighted shortest path uses stale weights | Publish topology and weight mutations in the same manifest generation; weighted path tests compare against full rebuild. |
| Node visibility lags edge topology | Query returns edges to deleted or inactive nodes | Publish node active/tombstone segments with edge segments and apply visibility before neighbor emission. |
| Filter or tenant deltas lag reads | Tenant-scoped or filtered queries return rows outside scope | Publish filter and tenant delta segments and add tenant/filter SQL tests. |
| Resolution deltas lag node inserts | New node cannot be resolved across backends | Publish resolution delta segments with node insert segments and test cross-backend node lookup. |
| Transaction-local precedence is wrong | Same transaction cannot read its own write or delete | Merge `projection::tx_delta` last and test tx-delta-over-durable precedence. |
| Full rebuild invariant is too narrow | Neighbor tests pass but SQL hydration, paths, or GQL results diverge | Add unit, property, pgrx SQL, and heavy lifecycle invariants. |

### Durability And Crash Risks

| Risk | Expected symptom | Required mitigation and tests |
|---|---|---|
| Reader sees half-written segment | Loader panic, corrupt neighbor output, or backend crash | Manifest references only final validated files; temp files are ignored by loader tests. |
| Manifest publish is interrupted | Startup selects a partial generation | Atomic temp-write, fsync, rename, reload, and PostgreSQL generation metadata update; publish interruption tests keep previous generation current. |
| Segment checksum/header corrupts | Wrong graph output or unsafe slice construction | Strict loader rejection and fuzz targets for every segment kind. |
| Compaction publishes incomplete output | Replacement hides still-needed L0 files | Validate replacement segments before manifest publish and retain old generation; compaction interruption tests. |
| Chunk rewrite publishes incomplete base chunk | Reads mix old base and new segments incorrectly | Chunk replacement validates checksum and full rebuild equivalence before publish. |
| GC deletes active files | Existing backend or new load fails | Active-generation heartbeat plus retained-manifest reference scan; GC refusal tests. |
| Repair corrupts current generation | Recovery makes a bad artifact current | Repair writes a new generation only after validation; failed repair leaves old valid generation current or reports full rebuild required. |
| Full rebuild cannot restore projection | Extension remains unusable after corruption | Full rebuild repair path reads PostgreSQL source tables and publishes a valid base manifest; heavy recovery test required. |

### Concurrency Risks

| Risk | Expected symptom | Required mitigation and tests |
|---|---|---|
| Build, vacuum, ingest, compaction, repair, or GC publish conflicting generations | Manifest references mismatched base and segment files | Single publication lock and generation compare-and-swap; lock conflict SQL tests. |
| Multiple ingesters race | Duplicate segments, skipped sync rows, or watermark regression | Publisher lock plus monotonic watermark validation; concurrent ingester tests. |
| Backend holds old manifest while GC runs | Read failure in long-lived backend | Active-generation heartbeat and expiration; tests with old and new engine instances. |
| Query freshness conflicts with manifest watermark | Backend-local sync state and durable projection status disagree | Status reports backend-local apply watermark and manifest watermark separately. |
| Subtransaction writes leak into tx delta | Savepoint abort remains visible | Preserve current subtransaction rejection/cleanup tests and run them after layered read adoption. |

### Performance Risks

| Risk | Expected symptom | Required mitigation and tests |
|---|---|---|
| L0 fanout is too high | Traversal and GQL relationship expansion regress | Segment fanout diagnostics, compaction thresholds, and BFS/GQL benchmark gates. |
| Merge allocates per node | Hot traversal allocates large temporary vectors | Streaming iterator design and allocation benchmarks. |
| Segment lookup scans too much metadata | Sparse queries become metadata-bound | Source-node range index by direction and edge type; benchmark sparse and high-degree queries. |
| Reverse segments double disk growth | Disk growth exceeds expected budget | Status exposes forward/reverse bytes; release benchmarks include disk footprint. |
| Compaction exceeds maintenance budget | Maintenance stalls or blocks other jobs | Max rows, bytes, segments, and elapsed budget; compaction latency tests. |
| Chunk rewrites are too large | Dirty range repair behaves like full rebuild | Chunk-size config and dirty-range tests over small and large ranges. |
| Status calls are too expensive | Monitoring adds load | Status reads cached manifest diagnostics and PostgreSQL metadata, not full directory scans. |

## Expected Performance And Memory Impact

The production design should reduce rebuild pressure and cross-backend staleness,
but it will add new memory and disk surfaces. These impacts are part of the
plan and must be measured through `todo/measurements.md` and
`todo/regression_report.md`.

| Surface | Expected impact | Measurement requirement |
|---|---|---|
| Clean `csr_readonly` reads | No material regression. Base-only manifests must bypass segment lookup. | Criterion `bfs_overlay_paths/no_overlay_d3` and traversal cases compare against `pre_durable_projection`. |
| Layered reads with L0 segments | Additional segment metadata lookup and merge work. Sparse L0 should be close to current sparse overlay behavior; many L0 segments may regress until compaction. | Add L0, many-L0, and compacted L1/L2 Criterion cases before replacing `Engine.edge_buffer`. |
| Weighted shortest path | Durable weight lookup adds another layer lookup compared with clean CSR. | Add weighted layered benchmark and SQL correctness tests. |
| GQL relationship expansion | Relationship expansion inherits layered neighbor lookup cost. | Add GQL layered expansion benchmark and pgrx SQL tests. |
| Per-backend memory | Each backend holds a manifest snapshot, segment metadata indexes, active-generation heartbeat state, and current tx delta. Segment payloads should be mmap-backed or borrowed where possible rather than copied per backend. | Add per-backend metadata byte accounting to status and measure with `measure_mmap_pss.sh` on Linux. |
| Shared memory/page cache | Segment and base chunk files increase mapped/read file footprint but should share OS page cache across backends. | Use Linux PSS evidence, not summed RSS, for multi-backend memory claims. |
| Disk footprint | Forward and reverse segments, retained generations, obsolete segments, and chunk rewrites increase disk use until compaction and GC. | Status must report segment bytes, obsolete bytes, and retained generation bytes. |
| Ingestion memory | Bounded mutation buffers consume memory during admin/maintenance ingestion. | Enforce row/byte limits and record ingestion peak memory for large batches. |
| Compaction memory | Merging L0/L1/L2 and rewriting chunks requires bounded working memory. | Compaction benchmarks must record rows, bytes, elapsed time, and peak memory where available. |
| GC memory | Reference scanning loads manifest metadata, not segment payloads. | GC tests must assert metadata-only scanning and status must expose obsolete file counts/bytes. |
| PostgreSQL connections | More concurrent connections mean more backend-local manifest metadata and tx-delta state. Large segment payloads should not be duplicated per connection. | Add a multi-backend PSS/RSS measurement before release readiness. |

### Operator And Compatibility Risks

| Risk | Expected symptom | Required mitigation and tests |
|---|---|---|
| Roadmap and TODO diverge | Contributors build against stale assumptions | TODO points to roadmap; completed behavior moves into production docs before release. |
| SQL status fields churn | Dashboards break | Add fields backward-compatibly and run SQL API drift checks. |
| Artifact versioning is unclear | Upgrades fail or load incompatible files | Independent manifest and segment versions with typed incompatible-version errors. |
| Users treat projection as source of truth | Backup or recovery guidance becomes wrong | Docs state PostgreSQL tables remain authoritative and projection artifacts are derived. |
| `csr_readonly` slows down | Read-mostly users pay for mutable features | Clean fast path bypasses segment lookup for base-only manifests; benchmark gate. |

## Tests To Write First

These tests are written before production code for their phase. They fail until
the phase implements the required behavior.

| Test | Layer | Starts failing because | Passes after |
|---|---|---|---|
| `projection_manifest_roundtrips_base_only_generation` | Rust unit | No manifest type or loader exists | Phase 1 |
| `projection_manifest_rejects_partial_or_unknown_generation` | Rust unit | No manifest validation exists | Phase 1 |
| `projection_generation_heartbeat_expires_stale_backend` | pgrx SQL | No generation table exists | Phase 1 |
| `delta_segment_roundtrips_edge_topology_weight_and_delete_sections` | Rust unit | No complete segment writer/loader exists | Phase 2 |
| `delta_segment_roundtrips_node_resolution_filter_tenant_sections` | Rust unit | No non-edge segment support exists | Phase 2 |
| `delta_segment_rejects_corrupt_offsets_checksum_and_reserved_flags` | Rust unit/fuzz seed | No segment validation exists | Phase 2 |
| `delta_segment_normalization_is_deterministic` | Proptest | No mutation normalization exists | Phase 3 |
| `delta_segment_normalization_preserves_delete_precedence` | Proptest | No operation-order rules exist | Phase 3 |
| `projection_ingest_committed_edge_insert_publishes_l0_manifest` | pgrx SQL | No ingester or L0 publish path exists | Phase 4 |
| `projection_ingest_publishes_weight_node_resolution_filter_tenant_deltas` | pgrx SQL | No complete ingestion surface exists | Phase 4 |
| `projection_ingest_aborted_gql_write_is_not_published` | Heavy SQL | Ingest cannot prove commit-only behavior | Phase 4 |
| `layered_neighbors_equal_full_rebuild_for_insert_delete_sequence` | Proptest/Rust unit | No layered durable reader exists | Phase 5 |
| `weighted_shortest_path_uses_durable_weight_segments` | pgrx SQL | Weighted read path has no durable weights | Phase 5 |
| `layered_reads_apply_tenant_filter_and_node_visibility_segments` | pgrx SQL | Visibility/filter/tenant segments are not applied | Phase 5 |
| `base_chunk_rewrite_preserves_full_rebuild_equivalence` | Rust integration | No base chunk rewrite exists | Phase 6 |
| `compaction_l0_l1_l2_preserves_layered_neighbors` | Rust unit/proptest | No compaction planner exists | Phase 7 |
| `projection_gc_refuses_referenced_or_active_generation_files` | Rust unit/pgrx SQL | No GC reference scanner or heartbeat use exists | Phase 8 |
| `status_reports_manifest_watermark_segments_chunks_gc_and_repair` | pgrx SQL | Status has no durable projection fields | Phase 9 |
| `load_corrupt_active_segment_repairs_or_rebuilds` | Heavy SQL | No recovery path exists | Phase 10 |
| `cross_backend_committed_write_visible_without_full_rebuild` | Heavy SQL | `Engine.edge_buffer` is still backend-local | Phase 11 |
| `bfs_layered_projection_no_unbounded_regression` | Benchmark gate | No benchmark target for durable segments exists | Phase 12 |

## Passing Gates By Phase

| Phase | Required passing tests before moving on |
|---|---|
| Phase 1: Manifest and generation table | Manifest roundtrip, validation rejection, temp-file ignore, heartbeat, atomic publish, base-only status. |
| Phase 2: Segment format | Roundtrip and corruption rejection for edge, weight, node, resolution, filter, tenant, tombstone segments; fuzz seeds. |
| Phase 3: Normalization | Determinism, cancellation, delete precedence, direction/type grouping, and buffer limit proptests. |
| Phase 4: Ingester | Committed-only publication, all segment kinds, watermark ordering, concurrent publisher locking, build/vacuum conflict tests. |
| Phase 5: Layered runtime | Full rebuild equivalence, tx-delta precedence, weighted path, tenant/filter/node visibility, GQL/traversal/components tests. |
| Phase 6: Base chunking | Dirty chunk rewrite, chunk repair, old-generation readability, full rebuild equivalence. |
| Phase 7: Compaction | L0/L1/L2 merge preservation, tombstone preservation, bounded execution, interruption safety. |
| Phase 8: GC | Referenced-file refusal, active-heartbeat refusal, retention deletion, crash-safe current generation. |
| Phase 9: Diagnostics | Status and sync-health SQL tests for build, ingest, compaction, chunk rewrite, GC, repair, and tx-delta-only pressure. |
| Phase 10: Recovery | Corrupt active segment, missing referenced file, corrupt chunk, corrupt manifest, full rebuild repair. |
| Phase 11: Replacement | Cross-backend committed visibility, existing mutable-overlay tests, tx-delta lifecycle tests, `csr_readonly` compatibility. |
| Phase 12: Production readiness | Unit, property, fuzz, pgrx, heavy crash/concurrency, docs drift, release evidence, and benchmark gates. |

## Production Stop Conditions

Stop the implementation and revise the architecture when any condition is hit:

- Layered neighbor reads require large per-node materialization to remain
  deterministic.
- Segment metadata lookup dominates traversal latency below the configured
  compaction thresholds.
- Active-generation heartbeat cannot protect long-lived PostgreSQL backends from
  GC without blocking cleanup indefinitely.
- Weighted shortest path cannot use durable weight segments with the same
  correctness invariant as unweighted traversal.
- Node/filter/tenant segment visibility cannot be made equivalent to full
  rebuild output.
- SQL status calls require filesystem scans on routine monitoring reads.
- Full rebuild equivalence cannot be expressed as property and SQL tests.
