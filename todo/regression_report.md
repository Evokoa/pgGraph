# Mutable Projection Regression Report

This report is appended after each performance-sensitive or memory-sensitive
change. It records the baseline used, regression commands, outcome, and
decision.

## 2026-06-07: Initial Planning Baseline

| Field | Value |
|---|---|
| Scope | Planning documentation and initial benchmark baseline |
| Code changes | None |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo bench --features pg17 --bench bfs_bench -- --save-baseline pre_durable_projection` |
| Result | Baseline captured successfully |
| Decision | Use `pre_durable_projection` as the comparison baseline until a phase-specific baseline is recorded |

## 2026-06-07: Microphase 0 Test Harness

| Field | Value |
|---|---|
| Scope | Test-only fixture helpers and ignored durable-projection contract tests |
| Code changes | `#[cfg(test)]` projection modules only |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::` |
| Result | Passed; ignored contract tests remain out of the default suite |
| Decision | No benchmark comparison required because no runtime, memory, traversal, SQL, or artifact production code changed |

## 2026-06-07: Microphase 1 Manifest And Generation Table

| Field | Value |
|---|---|
| Scope | Manifest JSON model, active-generation heartbeat helpers, generation metadata table, and public sync docs |
| Code changes | Pure manifest metadata plus SQL bootstrap; no traversal/read-path runtime adoption |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::manifest`; `cd graph && cargo check --features pg17`; `python3 scripts/check_doc_references.py` |
| Result | Manifest tests, non-test compile, and doc references passed. Full `cargo test --features pg17` is intentionally red with 528 passed, 5 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required until manifest loading affects engine status, reads, ingestion, or artifact publication |

## 2026-06-07: Microphase 2 Atomic Manifest Publish And Load

| Field | Value |
|---|---|
| Scope | Manifest filesystem store, atomic publish, latest-generation load, temp-file ignore, and active-reference validation |
| Code changes | Projection manifest file I/O only; no traversal/read-path runtime adoption |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::manifest`; `cd graph && cargo check --features pg17`; `python3 scripts/check_doc_references.py` |
| Result | Publish/load manifest tests, non-test compile, and doc references passed. Full `cargo test --features pg17` is intentionally red with 534 passed, 5 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required until manifests are loaded by engine status, reads, ingestion, cleanup, or repair paths |

## 2026-06-07: Microphase 3 Complete Segment Format

| Field | Value |
|---|---|
| Scope | Delta segment writer/loader, corruption validation, segment contract tests, and fuzz seeds |
| Code changes | Projection segment artifact codec under test/fuzz/development gates; no traversal/read-path runtime adoption |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::segment`; `cd graph && cargo test --features pg17 projection::test_contracts`; `cargo check --manifest-path graph/fuzz/Cargo.toml` |
| Result | Segment tests passed, two segment contract tests turned green, fuzz package compiled with existing sync-helper dead-code warnings. Full `cargo test --features pg17` is intentionally red with 542 passed, 3 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required until segments are produced by ingestion or consumed by layered reads, compaction, cleanup, or repair paths |

## 2026-06-07: Microphase 4 Mutation Normalization

| Field | Value |
|---|---|
| Scope | Committed mutation normalization, cancellation/delete precedence, bounded ingestion buffers, and normalized segment construction |
| Code changes | Projection normalization under test/development gates; no traversal/read-path runtime adoption |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::normalize`; `cd graph && cargo test --features pg17 projection::segment`; `cd graph && cargo test --features pg17 projection::test_contracts`; `cargo check --manifest-path graph/fuzz/Cargo.toml` |
| Result | Normalization tests and segment writer integration passed, including node/edge domain separation and deterministic duplicate-sync tie breaks. The fuzz package compiled with existing sync-helper dead-code warnings. Full `cargo test --features pg17` is intentionally red with 553 passed, 3 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required until normalized rows are produced by ingestion or consumed by live segment publication |

## 2026-06-07: Microphase 5 Base-Only Engine Manifest Load

| Field | Value |
|---|---|
| Scope | Base-only projection manifest discovery during `.pggraph` load and backend-local status metadata |
| Code changes | Persistence load path and engine status metadata only; CSR remains the active read path |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 persistence::tests::`; `cd graph && cargo check --features pg17`; `cd graph && cargo test --features pg17 --doc`; `python3 scripts/check_doc_references.py` |
| Result | Persistence/load-path tests passed, including base-only manifest load, CSR traversal preservation, base manifest status metadata, wrong-base rejection, stale checksum rejection, wrong-version rejection, and non-base-only rejection. Full `cargo test --features pg17` is intentionally red with 560 passed, 3 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required because traversal, GQL, components, shortest-path, ingestion, compaction, GC, and SQL read-path adoption are unchanged |

## 2026-06-07: Microphase 6 Core Ingestion Publisher

| Field | Value |
|---|---|
| Scope | Testable committed-row ingestion into L0 projection segments and next-manifest publication |
| Code changes | Projection ingestion under test/development gates; no SQL entrypoint, scheduler wiring, traversal read-path adoption, compaction, or GC change yet |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo fmt --check`; `cd graph && cargo test --features pg17 projection::ingest`; `cd graph && cargo test --features pg17 projection::test_contracts`; `cd graph && cargo check --features pg17`; `cd graph && cargo test --features pg17 --doc`; `python3 scripts/check_doc_references.py` |
| Result | Core ingestion tests passed, including committed filtering, aborted-row exclusion, watermark rollback on failed publish, artifact-root publication locking, serialized generations, generation-overflow rejection, node-surface normalization and limits, durable no-overwrite segment publication, edge weights, node state, resolution, filter, tenant, and direction-specific edge segments. Check, doctests, and docs passed. Full `cargo test --features pg17` is intentionally red with 567 passed, 2 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required until ingestion is wired into SQL/scheduled maintenance or durable segments are consumed by runtime read paths |

## 2026-06-07: Microphase 6 SQL Projection Ingestion

| Field | Value |
|---|---|
| Scope | SQL `graph.ingest_projection(...)`, committed sync-log conversion, persisted-base manifest publication, and scheduled-maintenance ingestion call |
| Code changes | Production-visible projection ingestion modules; artifact checksum/version helpers; tombstone-safe resolution lookup; sync-log conversion into edge/node/resolution/filter/tenant projection rows; pgrx SQL wrapper; scheduled maintenance invokes ingestion after `apply_sync_internal()` and ignores missing persisted base artifacts |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo fmt`; `cd graph && cargo test --features pg17 projection::ingest -- --list`; `cd graph && cargo test --features pg17 projection::ingest`; `cd graph && cargo check --features pg17`; `cd graph && cargo pgrx test --features "pg17 development" pg17 ingest_projection`; `cd graph && cargo pgrx test --features "pg17 development" pg17 scheduled_maintenance`; `cd graph && cargo test --features pg17` |
| Result | Unit loader smoke and core ingestion tests passed. The pgrx SQL signature, persisted sync-log ingestion, base-checkpoint watermark, and no-row watermark-advance tests passed, and scheduled maintenance still applies sync/starts maintenance while treating absent persisted projection artifacts as a no-op. Full `cargo test --features pg17` is intentionally red with 567 passed, 2 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required because runtime traversal, GQL, components, shortest-path, compaction, GC, and durable read-path adoption are unchanged; record this as an artifact-write-path checkpoint |

## 2026-06-07: Microphase 7 Layered Runtime

| Field | Value |
|---|---|
| Scope | Pure layered neighbor runtime over base CSR, durable segments, and transaction-local deltas |
| Code changes | Added production-visible `projection/layered.rs`, an owned neighbor iterator variant, durable segment-provider boundary, deterministic durable insert/delete/weight merging, node visibility and tenant-membership filtering, and weighted-neighbor lookup |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo fmt --check`; `cd graph && cargo test --features pg17 projection::layered`; `cd graph && cargo test --features pg17 projection::neighbors`; `cd graph && cargo test --features pg17 projection::test_contracts`; `cd graph && cargo check --features pg17`; `cd graph && cargo test --features pg17 --doc`; `python3 scripts/check_doc_references.py`; `git diff --check`; `cd graph && cargo test --features pg17` |
| Result | Layered, neighbor, compile, doctest, docs, and whitespace gates passed. Feature contracts are intentionally red only for the future status/diagnostics contract: 5 passed, 1 failed. Full unit tests are expected-red with 580 passed, 1 failed future status/diagnostics contract, and 1 ignored scale test. Independent-review findings were fixed before promotion: tx node tombstones, tx weighted inserts, manifest checksum checks, and real-provider/proptest coverage. |
| Decision | No benchmark comparison required in this checkpoint because Engine and SQL read paths still use the existing CSR/overlay selection; run read-latency and BFS comparisons in Microphase 8 when layered reads become query-visible |

## 2026-06-07: Microphase 8 SQL Read-Path Adoption

| Field | Value |
|---|---|
| Scope | Public traversal, shortest path, weighted shortest path, connected components, and GQL relationship expansion select layered neighbors for segment-backed manifests |
| Code changes | Engine read-path selection now builds manifest-backed `LayeredNeighbors`; segment-backed `.pggraph` reloads activate mutable-overlay reads; base-only and `csr_readonly` manifests keep the CSR fast path; committed `Engine.edge_buffer` overlays remain visible inside layered reads; inbound layered base reads use the reverse CSR store; BFS keeps its concrete overlay hot path and exposes a separate generic helper for layered callers |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo bench --features pg17 --bench bfs_bench -- --baseline pre_durable_projection`; targeted follow-ups: `d1_supernode/500k`, `d1_supernode/2M_panama`, and `d3_leaf/2M_panama` |
| Result | Full comparison completed. Existing mutable-overlay guardrail improved: `no_overlay_d3` -4.94%, `sparse_overlay_d3` -6.08%, `dense_overlay_d3` -2.81%. Filter traversal improved: sparse -6.23%, dense -2.67%. Several deeper raw BFS cases improved. A first `d1_supernode/500k` regression was reduced to no-change after restoring the concrete BFS hot path (`+0.95%`, p = 0.23). Residual targeted raw-BFS regressions remain on the 2M Panama fixture: `d1_supernode/2M_panama` +4.07% and `d3_leaf/2M_panama` +6.33%. |
| Decision | Promote the Microphase 8 checkpoint with the residual 2M raw-BFS regressions recorded for follow-up because the directly relevant existing overlay guardrail improved and segment-backed layered SQL reads now pass pgrx correctness gates. Segment files are still decoded per read and should be cached or otherwise benchmarked before release. Do not treat this as a release-ready performance signoff; Microphase 12 must add durable segment-specific BFS, weighted, and GQL benchmarks before production replacement. |

## 2026-06-07: Microphase 9 Base Chunk Rewrite

| Field | Value |
|---|---|
| Scope | Base chunk manifest metadata, checked chunk rewrite publication, targeted corrupt-chunk repair, and layered base-range replacement semantics |
| Code changes | Added `projection::chunk` for source-range chunk publication and repair; added dirty source/edge counters to `ManifestChunkRef`; manifest-backed layered reads now load base chunks, replace covered outgoing base ranges, suppress covered inbound base edges, and merge chunk inbound edges |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo fmt --check`; `cd graph && cargo check --features pg17`; `cd graph && cargo test --features pg17 base_chunk_`; `cd graph && cargo test --features pg17 projection::manifest`; `cd graph && cargo test --features pg17 projection::layered`; `cd graph && cargo test --features pg17 projection::test_contracts` |
| Result | Base chunk manifest, rewrite equivalence, old-generation readability, partial-overlap expansion, unchanged-edge preservation, malformed inbound chunk rejection, and corruption repair tests passed. Manifest and layered tests passed. Feature contracts remain intentionally red only for the future status/diagnostics contract: 5 passed, 1 failed. |
| Decision | No new benchmark comparison required for this checkpoint because chunk rewrite is a publication/repair boundary and only affects manifests that already opt into chunked generations. Default CSR/base-only reads and existing SQL read-path selection are unchanged; benchmark chunked generation reads when compaction or repair scheduling makes chunks operationally active. |

## 2026-06-07: Microphase 10 Compaction

| Field | Value |
|---|---|
| Scope | Durable segment fanout compaction, tombstone-preserving merged segments, dirty chunk pressure rewrite, and bounded publication failure |
| Code changes | Added `projection::compact`; L0 segments compact to L1, L1 segments compact to L2, compacted output is derived from the previous layered view against base CSR, high segment pressure can publish base chunks instead, non-edge segments and durable weights are retained, overlapping dirty ranges are normalized, and budget failures leave the previous manifest current |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::compact`; `cd graph && cargo test --features pg17 compaction_`; `cd graph && cargo test --features pg17 base_chunk_`; `cd graph && cargo test --features pg17 projection::layered`; `cd graph && cargo check --features pg17`; `cd graph && cargo fmt --check`; `cd graph && cargo test --features pg17 --doc`; `python3 scripts/check_doc_references.py`; `cd graph && cargo test --features pg17` |
| Result | L0-to-L1, L1-to-L2, tombstone precedence, non-edge segment retention, weighted edge retention, dirty chunk pressure, dirty chunk weight/non-edge retention, overlapping dirty-range normalization, and interruption tests passed. Chunk, layered, compile, format, doctest, and docs gates passed. Full tests remain intentionally red only for the future status/diagnostics contract: 608 passed, 1 failed, 1 ignored. |
| Decision | No benchmark comparison required for this checkpoint because compaction is not yet invoked by SQL or scheduled maintenance. It creates opt-in compacted artifacts that preserve layered output; measure when compaction is connected to an operational maintenance path. |

## 2026-06-07: Microphase 11 Active Generation Heartbeat

| Field | Value |
|---|---|
| Scope | Active backend generation liveness rows, stale heartbeat expiry, GC-facing active-generation predicate, and SQL active generation count |
| Code changes | SQL heartbeat helpers now record/refresh backend PID, database OID, generation, heartbeat/expiry timestamps, sync watermark, and validation status; manifest installation records the current backend heartbeat immediately; status refresh expires stale rows and refreshes the installed manifest heartbeat; `graph.active_generation_count()` exposes the active count without changing the existing `graph.status()` row ABI |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::manifest`; `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_generation_heartbeat`; `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_mode_build_and_status_contract`; `cd graph && cargo pgrx test --features "pg17 development" pg17 sync_health_exposes_operator_contract_field_names`; `cd graph && cargo check --features pg17`; `cd graph && cargo fmt --check`; `cd graph && cargo test --features pg17 --doc`; `python3 scripts/check_doc_references.py`; `cd graph && cargo test --features pg17` |
| Result | Manifest heartbeat unit tests passed. Pgrx heartbeat tests passed for manifest-install record/status exposure, refresh/upsert, stale expiry, and GC active-generation blocking. Projection-mode status and sync-health signature contracts passed with existing ABIs preserved. Full tests remain intentionally red only for the future status/diagnostics contract: 608 passed, 1 failed, 1 ignored. |
| Decision | No benchmark comparison required because this checkpoint changes SQL metadata and status paths, not graph traversal/read-path execution. Keep the existing `graph.status()` ABI intact and expose active generation count through a new scalar status helper because pgrx rejects a wider status tuple. |

## 2026-06-07: Microphase 12 Generation-Aware GC

| Field | Value |
|---|---|
| Scope | Metadata-only projection artifact cleanup, retained-generation protection, active-heartbeat protection, idempotent obsolete-file deletion, and SQL GC summary exposure |
| Code changes | Added `projection::gc`; GC scans valid manifest metadata, protects references from the newest retained generations and active heartbeat generations, fails closed when an active generation has no valid manifest, deletes only manifest-declared obsolete files, ignores already-missing candidates, adds `graph.projection_retention_generations`, and exposes `graph.projection_gc()` |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::gc`; `cd graph && cargo check --features pg17`; `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_gc`; `cd graph && cargo pgrx test --features "pg17 development" pg17 guc_contract_defaults_ranges_and_contexts_are_registered`; `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_generation_heartbeat`; `cd graph && cargo fmt --check`; `cd graph && cargo test --features pg17 --doc`; `python3 scripts/check_doc_references.py`; `cd graph && cargo test --features pg17` |
| Result | GC refusal, active-generation protection, unmatched-active fail-closed behavior, retention-based deletion, idempotence, crash-shape, compile, SQL deletion/idempotence/signature, GUC range/default, heartbeat, format, doctest, and docs-reference tests passed. Full tests remain intentionally red only for the future status/diagnostics contract: 613 passed, 1 failed, 1 ignored. |
| Decision | No benchmark comparison required at this checkpoint because GC does not alter traversal/read-path execution; it scans manifest metadata and removes only obsolete files. Revisit latency benchmarks when scheduled maintenance invokes GC automatically. |

## 2026-06-07: Microphase 13 Recovery And Repair

| Field | Value |
|---|---|
| Scope | Active projection validation, corrupt segment/manifest full-rebuild planning, base chunk targeted repair, corrupt-manifest quarantine, and SQL repair summary exposure |
| Code changes | Added `projection::recovery`; active validation loads manifest-referenced segments and chunks, corrupt segment or manifest state plans full rebuild, corrupt base chunks can publish a replacement generation from a base graph source, full rebuild repair quarantines the latest manifest before persisted maintenance rebuild, publishes a new base-only generation, reloads it, and exposes `graph.projection_repair()` |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::recovery`; `cd graph && cargo check --features pg17`; `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_repair`; `cd graph && cargo pgrx test --features "pg17 development" pg17 full_rebuild_restores_valid_projection_generation`; `PG_VERSION_FEATURE=pg17 graph/tests/heavy/projection_recovery_gate.sh` |
| Result | Recovery unit tests passed for corrupt active segments, missing referenced segment rejection, unreferenced temp segment ignore, corrupt and missing chunk repair, stale base metadata rebuild planning, corrupt manifest rebuild planning, and rebuilt-generation publication. SQL repair signature, targeted chunk repair, and full-rebuild restoration tests passed. The projection recovery gate passed outside the sandbox after the sandboxed pgrx run could not bind an ephemeral local test port. Format, doctest, doc-reference, and whitespace gates passed. Full tests remain intentionally red only for the future status/diagnostics contract: 621 passed, 1 failed, 1 ignored. |
| Decision | No benchmark comparison required at this checkpoint because repair is an admin/error path and normal traversal/read execution is unchanged. Measure repair latency before production release when status diagnostics and scheduled maintenance invoke repair automatically. |

## 2026-06-07: Microphase 14 Status And Diagnostics

| Field | Value |
|---|---|
| Scope | Durable projection status row, sync-health durable recommendation fields, active heartbeat diagnostics, and ingest/compaction/GC/repair recommendations |
| Code changes | Added `projection::status`; added `graph.projection_status()`; extended `graph.sync_health()` with durable recommendation booleans; kept `graph.status()` ABI unchanged because pgrx rejects more than 32 tuple fields; persisted operation timestamps in manifest metadata plus a GC status sidecar; kept `sync_health()` on metadata-only durable recommendations |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo check --features pg17`; `cd graph && cargo test --features pg17 projection::status`; `cd graph && cargo test --features pg17 projection::test_contracts`; `cd graph && cargo test --features pg17 projection::manifest`; `cd graph && cargo pgrx test --features "pg17 development" pg17 projection_status`; `cd graph && cargo pgrx test --features "pg17 development" pg17 sync_health_exposes_operator_contract_field_names`; `cd graph && cargo pgrx test --features "pg17 development" pg17 sync_health_distinguishes_tx_delta_edge_buffer_and_durable_projection_pressure`; `cd graph && cargo pgrx test --features "pg17 development" pg17 status_reports_active_generation_heartbeat_count`; `cd graph && cargo pgrx test --features "pg17 development" pg17 status_recommends_ingest_compaction_gc_or_repair_by_threshold`; `cd graph && cargo fmt --check`; `cd graph && cargo test --features pg17 --doc`; `git diff --check`; `scripts/check_docs_drift.sh`; `python3 scripts/check_doc_references.py`; `cd graph && cargo test --features pg17`; `cd graph && cargo pgrx test --features "pg17 development" pg17 status`; `cd graph && cargo pgrx test --features "pg17 development" pg17 sync_health` |
| Result | Focused Rust status/manifest tests, all durable projection contract tests, pgrx projection-status/sync-health/status recommendation tests, format, doctest, whitespace, docs drift, and docs reference checks passed. Full Rust suite passed with 623 tests and 1 ignored scale test. Broad pgrx status filter passed 13 tests; broad pgrx sync-health filter passed 3 tests. Independent review blockers around misleading timestamps and heavyweight sync-health validation were fixed before commit. |
| Decision | No benchmark comparison required at this checkpoint because traversal/read-path execution is unchanged and scheduler status polling uses metadata-only projection recommendations. Revisit if production monitoring makes full `graph.projection_status()` validation a hot polling path. |

## 2026-06-07: Microphase 15 Durable Committed Edge Overlay Replacement

| Field | Value |
|---|---|
| Scope | `graph.apply_sync()` routes persisted `mutable_overlay` committed edge changes into durable projection segments, reloads segment-backed manifests, and supports fresh-backend cross-backend visibility without full rebuild |
| Code changes | Added fresh-backend persisted graph load for `apply_sync_internal()`; persisted and reloaded projection mode through a sidecar so fresh backends do not use the session default GUC as build-mode truth; added a durable high-watermark apply path that reads pending committed sync rows, publishes L0 projection segments, reloads the latest manifest, preserves segment-backed mutable mode instead of downgrading to `csr_readonly`, and leaves legacy sync-buffer replay for compatibility surfaces; routed query-time `apply_pending_sync` through the same durable-aware helper; added pgrx and heavy cross-backend coverage |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo fmt --check`; `cd graph && cargo check --features pg17`; `cd graph && cargo test --features pg17 persisted_load_preserves_projection_mode_sidecar`; `cd graph && cargo pgrx test --features "pg17 development" pg17 cross_backend_committed_write_visible_without_full_rebuild`; `cd graph && cargo pgrx test --features "pg17 development" pg17 topology_auto_sync_uses_durable_segments_for_mutable_overlay`; `cd graph && cargo pgrx test --features "pg17 development" pg17 csr_readonly_apply_sync_ignores_later_mutable_default_guc`; `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_cross_backend_durable ./tests/heavy/cross_backend_durable_projection.sh`; `cd graph && cargo pgrx test --features "pg17 development" pg17 traverse_auto_sync_opt_in_applies_pending_edge_insert`; `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_create_node`; `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_delete_edge`; `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_tx_delta ./tests/heavy/tx_delta_lifecycle.sh`; `cd graph && cargo test --features pg17 csr_readonly`; `cd graph && cargo test --features pg17`; `cd graph && cargo bench --features pg17 --bench bfs_bench -- --baseline pre_durable_projection`; `cd graph && cargo bench --features pg17 --bench bfs_bench -- bfs_overlay_paths --baseline pre_durable_projection`; same-environment follow-up with temporary HEAD worktree and shared target: `CARGO_TARGET_DIR=/private/tmp/pggraph-micro15-bench-target cargo bench --features pg17 --bench bfs_bench -- bfs_overlay_paths --save-baseline micro15_head_overlay`; `CARGO_TARGET_DIR=/private/tmp/pggraph-micro15-bench-target cargo bench --features pg17 --bench bfs_bench -- bfs_overlay_paths --baseline micro15_head_overlay`; `CARGO_TARGET_DIR=/private/tmp/pggraph-micro15-bench-target cargo bench --features pg17 --bench bfs_bench -- bfs_traverse/d1_supernode/500k --save-baseline micro15_head_d1_500k`; `CARGO_TARGET_DIR=/private/tmp/pggraph-micro15-bench-target cargo bench --features pg17 --bench bfs_bench -- bfs_traverse/d1_supernode/500k --baseline micro15_head_d1_500k` |
| Result | Format, check, projection-mode sidecar, targeted pgrx durable visibility, query-time durable auto-sync, csr-readonly GUC-drift guard, true cross-backend durable visibility, existing auto-sync, GQL create/delete, tx-delta lifecycle, csr-readonly guard, docs, whitespace, and full Rust tests passed. Independent review blockers around high-watermark bounding, query-time catch-up, and projection-mode source of truth were fixed. Full Criterion comparison against the old `pre_durable_projection` baseline was red: examples include `bfs_traverse/d1_supernode/100k` +7.81%, `bfs_traverse/d1_supernode/500k` +7.87%, `bfs_traverse/d1_supernode/2M_panama` +10.68%, `graph_construction/build/500k` +4.03%, `bfs_overlay_paths/no_overlay_d3` +2.10%, `bfs_overlay_paths/dense_overlay_d3` +5.43%, and `bfs_filter_index_paths/score_gte_50_d3/dense_100pct` +4.51%. Targeted overlay rerun against the old baseline was also red: `no_overlay_d3` +1.53%, `sparse_overlay_d3` +5.36%, and `dense_overlay_d3` +2.08%. Same-environment follow-up against a temporary `HEAD` baseline showed no Microphase 15 overlay regression: `no_overlay_d3` was within noise threshold at +1.19%, `sparse_overlay_d3` was within noise threshold at +0.82%, and `dense_overlay_d3` improved by -18.71% after the fresh HEAD dense run showed high variance. A representative raw BFS red case, `bfs_traverse/d1_supernode/500k`, showed no change versus fresh HEAD at +0.81% (p = 0.58). |
| Decision | Promote Microphase 15 correctness with the old `pre_durable_projection` comparison recorded as stale/environment-sensitive evidence rather than a code regression. The same-environment HEAD comparison covers the directly relevant overlay guardrail and a representative raw BFS red case; refresh the long-lived baseline in Microphase 16 production verification before release signoff. |

## 2026-06-07: Microphase 16 Layered Projection Release Benchmarks

| Field | Value |
|---|---|
| Scope | Release-specific durable layered projection benchmark coverage and visible threshold contract tests |
| Code changes | Added `bench_support` helpers that construct real decoded durable projection segments internally, route BFS and weighted path algorithms through `LayeredNeighbors`, expose Criterion scenarios without widening the SQL/API surface, and exercise real ingestion, compaction, GC, and repair publication paths in the named threshold tests; extended `graph/benches/bfs_bench.rs` with `layered_projection_release_paths` |
| Baseline | New Microphase 16 baseline in `todo/measurements.md` |
| Command | `cd graph && cargo test --features pg17 bench_support::tests`; `cd graph && cargo bench --features pg17 --bench bfs_bench -- --list`; `cd graph && cargo bench --features pg17 --bench bfs_bench -- layered_projection_release_paths` |
| Result | The seven named Microphase 16 release-contract tests passed and are not ignored: `bfs_layered_projection_no_unbounded_regression`, `gql_layered_relationship_expansion_no_unbounded_regression`, `weighted_path_layered_projection_no_unbounded_regression`, `projection_ingest_publish_latency_under_threshold`, `projection_compaction_latency_under_threshold`, `projection_gc_latency_under_threshold`, and `projection_repair_latency_under_threshold`. The latency tests now call real `ProjectionIngester::ingest_committed_rows`, `compact_generation`, `collect_projection_garbage_with_config`, and `repair_active_base_chunks` paths against temporary projection artifact roots. Criterion registered and measured base-only, small L0, many L0, compacted L1, compacted L2, dirty chunk rewrite via the base-chunk provider path, committed overlay-on-top, GQL relationship expansion, and weighted-path scenarios. Mean timings on the 100k-node fixture were: base-only `161.55 us`, small L0 `312.32 us`, many L0 `1.3041 ms`, compacted L1 `299.60 us`, compacted L2 `298.87 us`, dirty chunk rewrite `455.46 us`, tx-delta overlay `318.14 us`, GQL expansion `184.89 us`, and weighted path `72.410 us`. Criterion compared against earlier same-working-tree local artifacts and flagged several cases as slower, but those are baseline-refresh comparisons from before the review fixes rather than failures against a release threshold. |
| Decision | Treat this as the initial Microphase 16 release benchmark baseline, not final release signoff. The coverage closes the previously missing durable-segment benchmark target and provides measured thresholds for subsequent production-verification passes. Final promotion still requires full Rust/pgrx gates, fuzz seeds, heavy release matrix, docs drift checks, docs merge, and release-owner approval for any benchmark regressions against the recorded release baseline. |

## 2026-06-08: Microphase 16 pgrx And Projection Fuzz Seed Gate

| Field | Value |
|---|---|
| Scope | Release pgrx SQL gate and durable projection manifest/segment seed corpus execution |
| Code changes | Added `graph/fuzz/run_projection_seed_corpora.sh`, wired it into `graph/tests/heavy/run_release_gate.sh`, documented projection fuzz seed usage, and added fuzz-target PostgreSQL symbol stubs so standalone projection loader fuzz binaries can satisfy pgrx/PostgreSQL symbols outside a live backend |
| Command | `cd graph && cargo pgrx test --features "pg17 development" pg17`; `CARGO_NET_OFFLINE=true graph/fuzz/run_projection_seed_corpora.sh` |
| Result | pgrx SQL tests passed with 821 tests and 1 ignored scale test. The manifest seed corpus ran 1 seed file and completed 2 libFuzzer executions. The segment seed corpus ran 2 seed files and completed 3 libFuzzer executions. Both seed corpora completed without crashes after the fuzz-only symbol stubs made the pgrx-free loader wrappers runnable outside a live PostgreSQL backend. |
| Decision | Mark the Microphase 16 pgrx and manifest/segment fuzz seed gates complete for pg17 local release evidence. No runtime benchmark comparison is required because this checkpoint changes only fuzz harness/release-gate wiring and docs. Final release promotion still requires the heavy release matrix, stable docs merge, and release-owner decision on deleting `todo/`. |

## 2026-06-08: Microphase 16 Local Release Gate

| Field | Value |
|---|---|
| Scope | Local pg17 release-gate script with external crash, Docker, playground, and pgbench gates disabled |
| Code changes | Updated `run_release_gate.sh` to invoke clippy and pgrx tests with `pg17 development`, matching the maintained pgrx evidence commands. Cleared clippy release-gate failures by removing panic-prone fixed-slice `expect()` calls, replacing an empty-group normalization `expect()` with `?`, using iterator style for active heartbeat rows, documenting narrow pgrx/benchmark/development helper lint exceptions, and simplifying a default string path. |
| Command | `PG_VERSION_FEATURE=pg17 DB_PREFIX=pggraph_release_m16 RUN_PLAYGROUND=0 RUN_PGBENCH=0 RUN_DOCKER=0 RUN_CRASH=0 RUN_TX_DELTA_CRASH=0 ./tests/heavy/run_release_gate.sh` |
| Result | Passed outside the sandbox. The gate covered `pg17 development` clippy, docs, full Rust tests, pgrx tests, cargo-deny, fuzz compile, projection fuzz seed corpora, package validation, fresh install smoke, metadata audit, SQLSTATE/ACL boundary, backup/restore, background lock, build lock, concurrency stress, synthetic release smoke, and GQL create/set/delete/merge lifecycle gates. The sandboxed attempt failed at pgrx port binding with `Operation not permitted`, which is an environment restriction rather than a code failure. |
| Decision | Treat this as strong local pg17 release evidence for non-crash, non-Docker, non-playground, non-pgbench gates. No runtime benchmark comparison is required because the code changes are release-gate lint cleanups and script feature alignment. Final release promotion still requires disposable-`PGDATA` crash/tx-delta-crash proof, playground/pgbench/Docker execution or release-owner waiver, stable docs merge, and final `todo/` deletion decision. |

## 2026-06-08: Microphase 16 Disposable PGDATA Crash Gates

| Field | Value |
|---|---|
| Scope | Standard crash recovery and transaction-delta crash recovery on a disposable PostgreSQL 17 cluster |
| Code changes | Updated `crash_recovery.sh` so explicit post-restart sync replay runs before default query-time auto catch-up can consume the recovered row, and changed the corrupt-artifact mutation from a no-op all-active bitmap byte to the file magic byte. |
| Command | `PGHOST=/private/tmp/pggraph-crash-pg17.hFQNpW PGPORT=55437 PGDATA=/private/tmp/pggraph-crash-pg17.hFQNpW/data POSTGRES_CTL=/opt/homebrew/opt/postgresql@17/bin/pg_ctl POSTGRES_OPTS="-p 55437 -k /private/tmp/pggraph-crash-pg17.hFQNpW" DBNAME=pggraph_release_m16_crash ./tests/heavy/crash_recovery.sh`; `PGHOST=/private/tmp/pggraph-crash-pg17.hFQNpW PGPORT=55437 PGDATA=/private/tmp/pggraph-crash-pg17.hFQNpW/data POSTGRES_CTL=/opt/homebrew/opt/postgresql@17/bin/pg_ctl POSTGRES_OPTS="-p 55437 -k /private/tmp/pggraph-crash-pg17.hFQNpW" PG_CONFIG=/opt/homebrew/opt/postgresql@17/bin/pg_config PG_VERSION_FEATURE=pg17 DBNAME=pggraph_release_m16_tx_delta_crash ./tests/heavy/tx_delta_crash_recovery.sh` |
| Result | Both crash gates passed outside the sandbox after initializing a disposable PostgreSQL 17.9 cluster under `/private/tmp/pggraph-crash-pg17.hFQNpW/data`. The standard crash gate proved committed trigger rows survived postmaster death, explicit `graph.apply_sync()` could replay them after restart, maintenance rebuilt/persisted successfully, and corrupted persisted artifact magic was rejected. The tx-delta crash gate proved an uncommitted transaction-local edge overlay was visible before postmaster death, did not survive restart, and the persisted base graph reloaded. |
| Decision | Mark disposable-`PGDATA` crash and tx-delta-crash proof complete for local pg17 Microphase 16 evidence. No runtime benchmark comparison is required because the code change only fixes crash-gate assertions and deterministic corruption setup. Remaining release blockers are playground/pgbench/Docker execution or waiver, stable docs merge, and final `todo/` deletion decision. |

## 2026-06-08: Microphase 16 External Gate Progress

| Field | Value |
|---|---|
| Scope | pgbench sync stress and Docker packaging smoke for local pg17 release evidence |
| Code changes | Documentation/evidence only |
| Command | `DBNAME=pggraph_release_m16_pgbench CLIENTS=4 JOBS=2 TIME=30 ./tests/heavy/run_pgbench_sync.sh`; `IMAGE=pggraph:m16-smoke CONTAINER=pggraph-m16-smoke PG_MAJOR=17 ./tests/heavy/docker_smoke.sh` |
| Result | pgbench sync stress passed with 2,997 transactions, 0 failed transactions, 2,997 sync-log rows, `apply_ms=107`, and `query_ms=61`. Docker smoke passed after building `pggraph:m16-smoke`, starting a disposable PostgreSQL 17 container, creating the extension, building a two-node graph, and finding the expected search row. |
| Decision | Mark pgbench and Docker smoke complete for local pg17 Microphase 16 evidence. The playground release gate was started with `PGGRAPH_PLAYGROUND_YES=1 PGGRAPH_CONTAINER_NAME=pggraph-m16-playground PGGRAPH_IMAGE_NAME=pggraph-postgres:m16-playground PGGRAPH_PG_PORT=55438 ./tests/heavy/playground_release_gate.sh`, but the run was interrupted before completion and the disposable container was removed. Heavy production release gates are deferred for a later pass; stable docs merge and final `todo/` deletion still remained open at this checkpoint. |

## 2026-06-08: Microphase 16 Stable Docs Merge

| Field | Value |
|---|---|
| Scope | Public contributor documentation for durable projection architecture and release-gate operation |
| Code changes | Documentation-only updates to `docs/contributor_guide/architecture.mdx` and `docs/contributor_guide/testing-release.mdx` |
| Command | `python3 scripts/check_doc_references.py`; `scripts/check_docs_drift.sh`; `git diff --check` |
| Result | Documentation references, SQL/GUC/source-map drift checks, and whitespace checks passed. The architecture docs now describe durable projection manifests/segments, transaction-local deltas, and cross-backend committed mutable-overlay visibility. The release docs now describe `run_release_gate.sh`, staged `RUN_*` evidence knobs, disposable crash gates, external pgbench/Docker/playground gates, and the rule that skipped or interrupted gates need rerun or explicit release-owner waiver. |
| Decision | Mark the stable docs merge complete for Microphase 16. No runtime benchmark comparison is required because this checkpoint changes docs only. Heavy production release gates are deferred for a later pass, and the final `todo/` deletion decision remains open until after the user's additional fixes. |
