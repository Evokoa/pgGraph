# Completed Plan — Build Sequence For The Post-v0.1.8 Work

Date: 2026-07-16
Status: plan complete; findings merged into the authoritative registers.

This closes the loop on this review folder: every finding from docs 01–05 is now
either a tracked row in `docs/known-issues.mdx` (KI-012 … KI-026), a sequenced
item in `docs/roadmap.mdx` → "Projection Operating Modes", or intentionally
dropped (nothing was dropped). This file is the single dependency-ordered build
sequence. Each slice is an independently mergeable, TDD'd PR; stop anywhere and
what's merged is shippable.

## Stage 0 — CI first (KI-017)

**S0. Automated PR CI.** Wire `cargo fmt --check`, `clippy`, `cargo test`,
`cargo doc`, and `scripts/check_docs_drift.sh` into a PR workflow; pgrx
integration as a second job. *Why first:* every slice below is riskier without
it, and all the scripts already exist. Gate: CI green on a no-op PR and red on a
seeded fmt/clippy violation.

## Stage 1 — Correctness and security (before promoting mutable mode)

Independent of each other; can proceed in parallel after S0.

| Slice | Register | What | Merge gate |
|---|---|---|---|
| C1 | KI-012 | Advisory-lock durable ingest: take the per-graph advisory lock (as `sql_build.rs` does) around `projection/ingest.rs` + `manifest.rs` publication | Two-backend concurrent `apply_sync()` stress test proves no lost generation; single-backend throughput unchanged |
| C2 | KI-015 | Bidirectional BFS: finish the level, select minimal combined-distance meeting node | Two-meeting-point regression test; existing path tests green; `bfs_bench` unregressed |
| C3 | KI-016 | `_sync_log` retention: checkpointed prune below the durable-watermark minimum, from `maintenance()` + `run_scheduled_maintenance()` | Prune never deletes unapplied rows (proptest over watermark orderings); log size bounded in a write-heavy heavy test |
| C4 | KI-013 | RLS topology boundary: loud security-docs section now; then refuse-or-warn GUC for registering RLS-enabled tables | Docs drift check green; pgrx test for the warn/refuse path |
| C5 | KI-014 | Tenant-scope docs: state filter-vs-isolation plainly; evaluate enforcement option | Docs updated; decision recorded (enforce or explicitly not) |

## Stage 2 — Operational hardening

| Slice | Register | What | Merge gate |
|---|---|---|---|
| O1 | KI-018 | Poison-row dead-lettering via `_sync_log.error_message`; count in `sync_health()` | Repeated-failure row is skipped + surfaced; recovery documented |
| O2 | KI-019 | Atomic artifact+sidecar commit; parent-dir fsync after rename | Crash-window test (kill between rename and sidecar) recovers correctly |
| O3 | KI-020 | `truncated` signal on capped traversal/path results | Contract documented; false-"no path" case covered |
| O4 | KI-021 | `mutable_overlay` production-status row + quickstart caveat | Row live until Stage 3 benchmark signoff clears it |
| O5 | KI-024 | `README_zh.md` version parity in `check_docs_drift.sh` | Drift check fails on stale translation version |

## Stage 3 — Make `live` mode real (roadmap Projection Modes phase 2)

Depends on Stage 1 (C1, C3 are hard prerequisites).

| Slice | Register | What | Merge gate |
|---|---|---|---|
| L1 | roadmap | Auto-maintenance consumes `sync_health()` recommendation booleans; adaptive cadence; non-Docker setup documented | Recommendations acted on in a soak test; cadence tightens under write pressure |
| L2 | roadmap | `mutable_overlay` build requires `persist_on_build = on` (refuse or warn, no silent degrade) | pgrx test for refusal path; docs updated |
| L3 | KI-023 | Weighted shortest path over layered overlays (segments already carry weights) | Weighted results on dirty overlay ≡ post-compaction results (proptest); rejection path removed |
| L4 | KI-021 | Benchmark + verification signoff: dirty-read amplification vs segment fanout, apply throughput, compaction pauses | Published numbers; then relax the `mutable_enabled` double gate and clear KI-021 |

## Stage 4 — Hybrid `auto` mode (roadmap phase 3)

Depends on Stage 3.

| Slice | What | Merge gate |
|---|---|---|
| H1 | Empty-overlay read fast path (part of KI-005): short-circuit overlay assembly when buffer + tx delta are empty | Clean-generation reads benchmark at CSR speed |
| H2 | `projection_mode = 'auto'` policy layer: act on pressure signals; opt-in mode escalation/demotion at maintenance windows, loudly logged | Policy soak test: escalates under write pressure, demotes when idle, never flaps |

## Stage 5 — Performance follow-ups (benchmark-driven, any time after S0)

All under KI-005's measure-before-optimizing rule: tenant-state borrow,
per-generation artifact validation cache, frontier pre-allocation clamp. Each
lands only with a benchmark showing it matters.

## Stage 6 — Larger refactors (roadmap-owned, last)

| Slice | Register | What |
|---|---|---|
| R1 | KI-022 | Unify overlays: durable segments become the only edge overlay for persisted graphs; `edge_buffer` retires to transaction-local duty |
| R2 | KI-025 | Builder regclass/OID-stable table resolution parity with hydration |
| R3 | KI-026 | Little-endian format doc + `cfg!(target_endian)` load guard |
| R4 | roadmap | Background pre-applier; WAL/logical-replication sync (`sync_mode = 'wal'`) |

## Cross-cutting rules (unchanged from the shipped program)

TDD every slice; pgrx-free internals where possible; no `Result<_, String>`
across layers; no new unsafe; docs-contract gate before public surface changes;
benchmark against the current baselines before claiming completion.
