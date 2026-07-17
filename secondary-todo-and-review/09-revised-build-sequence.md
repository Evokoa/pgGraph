# Revised Build Sequence — v2 (Supersedes Doc 07)

Date: 2026-07-16
Status: **authoritative build sequence.** Supersedes
`07-completed-plan-build-sequence.md`, which remains valid as the baseline —
every slice ID from doc 07 keeps its meaning here. The changes come from
`06-path-to-full-graph-engine.md` (strategy) via
`08-plan-delta-for-graph-engine-strategy.md` (delta rationale).

**Note for the agent currently building from doc 07:** nothing you have built
from Stages 0–2 is invalidated. Change markers below:
`[=]` unchanged from doc 07 · `[^]` promoted/gate changed · `[+]` new in v2.
If you are mid-slice on anything marked `[=]`, continue as specified in doc 07.
The only doc 07 content that changed meaning is: C2 and O3 gained a
benchmark-publication gate, the validation cache left Stage 5's
"measure-first" bucket, and WAL sync design moved earlier. Everything else new
is additive (Stage 2P and Stage 5 additions).

External anchor: **PG19 GA (~fall 2026)** — P1 must land before it.

---

## Stage 0 — CI first `[=]`

**S0 (KI-017).** `cargo fmt --check`, `clippy`, `cargo test`, `cargo doc`,
`scripts/check_docs_drift.sh` in a PR workflow; pgrx integration as a second
job. Gate: green on a no-op PR, red on a seeded violation.
*Everything below depends on S0.*

## Stage 1 — Correctness and security

Parallel after S0. All `[=]` except C2's gate.

| Slice | Register | What | Merge gate |
|---|---|---|---|
| C1 `[=]` | KI-012 | Advisory-lock durable ingest (per-graph lock as `sql_build.rs` does, around `projection/ingest.rs` + `manifest.rs` publication) | Two-backend concurrent `apply_sync()` stress: no lost generation; throughput unchanged |
| C2 `[^]` | KI-015 | Bidirectional BFS: finish level, pick minimal combined-distance meeting node | Two-meeting-point regression; existing tests green; bench unregressed. **New gate: blocks any published benchmark (P2 publication)** |
| C3 `[=]` | KI-016 | `_sync_log` retention: checkpointed prune below durable-watermark minimum, from `maintenance()` + `run_scheduled_maintenance()` | Proptest: never deletes unapplied rows; log bounded under write-heavy test |
| C4 `[=]` | KI-013 | RLS topology boundary: security-docs section now; refuse-or-warn GUC for RLS-enabled tables | Docs drift green; pgrx test for warn/refuse |
| C5 `[=]` | KI-014 | Tenant filter-vs-isolation docs + enforcement decision | Docs updated; decision recorded |

## Stage 2 — Operational hardening

All `[=]` except O3's gate.

| Slice | Register | What | Merge gate |
|---|---|---|---|
| O1 `[=]` | KI-018 | Poison-row dead-lettering via `_sync_log.error_message`; count in `sync_health()` | Repeated-failure row skipped + surfaced; recovery documented |
| O2 `[=]` | KI-019 | Atomic artifact+sidecar commit; parent-dir fsync after rename | Crash-window test recovers correctly |
| O3 `[^]` | KI-020 | `truncated` signal on capped traversal/path results | Contract documented; false-"no path" covered. **New gate: blocks P2 publication** |
| O4 `[=]` | KI-021 | `mutable_overlay` production-status row + quickstart caveat | Row live until L4 signoff |
| O5 `[=]` | KI-024 | `README_zh.md` version parity in drift check | Fails on stale translation |

## Stage 2P — SQL/PGQ compatibility track `[+]`

New parallel track after S0. Owns the PG19 GA clock. Source: doc 06 Pillar A,
doc 08 §3.

| Slice | What | Depends on | Merge gate | Target |
|---|---|---|---|---|
| P1 `[+]` | `graph.import_property_graph()`: read `pg_propgraph_*` → pgGraph registration; drift detection via `pg_get_propgraphdef()`; version-gated PG19+ | S0 | Round-trip on PG19 beta: CREATE PROPERTY GRAPH → import → build → traverse; drift flag on ALTER | **Before PG19 GA** |
| P2 `[+]` | LDBC SNB harness (`sandbox/benchmark` ldbc preset) incl. plain-PG19 `GRAPH_TABLE` same-box comparison | S0 (build now); C2 + O3 (publish) | Reproducible in-repo harness; baseline recorded | Harness now; publish post-C2/O3 |
| P3 `[+]` | `graph.graph_table()`: standard SQL/PGQ pattern grammar incl. variable-length forms core rejects, resolved against imported property graphs; GQL-parser frontend adaptation feeding the typed adapter seam (`query/sqlpgq_adapter.rs`) | P1 | Compatibility matrix function; differential test vs PG19 on fixed-depth patterns | GA + 0–6 mo |
| P4 `[+]` | CustomScan/planner-hook feasibility prototype on GRAPH_TABLE join trees; outcome is a written finding either way (feasible → slice; not → upstream hook proposal with LDBC evidence) | P1, P3 | Finding documented + decision | GA + 6–18 mo |

**Register edits required with P1/P3** (doc 08 §4): reword the roadmap
"Reserved Features" SQL/PGQ row and the `sqlpgq_adapter.rs` doc comment —
catalog import and the pattern-text surface no longer wait for PostgreSQL
hooks; only transparent acceleration (P4) does. Scope all "replace native
graph databases" language as *workload* replacement, cross-referenced to the
existing non-goals table.

## Stage 3 — Make `live` mode real

Depends on Stage 1 (C1, C3 hard prerequisites). All `[=]` plus one addition.

| Slice | Register | What | Merge gate |
|---|---|---|---|
| L1 `[=]` | roadmap | Auto-maintenance consumes `sync_health()` booleans; adaptive cadence; non-Docker setup documented | Soak test: recommendations acted on; cadence tightens under pressure |
| L2 `[=]` | roadmap | `mutable_overlay` requires `persist_on_build = on` (no silent degrade) | pgrx refusal test; docs |
| L3 `[=]` | KI-023 | Weighted shortest path over layered overlays | Proptest: dirty-overlay results ≡ post-compaction; rejection path removed |
| L4 `[=]` | KI-021 | Benchmark + verification signoff; then relax `mutable_enabled` double gate | Published numbers; KI-021 cleared |
| L5 `[+]` | roadmap | **WAL/logical-decoding sync design** (implementation stays Stage 6 R4): consumer shape, slot management, trigger-mode coexistence | Design doc reviewed; Stage 3 choices don't preclude it |

## Stage 4 — Hybrid `auto` mode `[=]`

Depends on Stage 3. H1 (empty-overlay fast path, part of KI-005) and H2
(`projection_mode = 'auto'` policy layer) exactly as doc 07.

## Stage 5 — Performance (benchmark-driven)

| Slice | What | Status |
|---|---|---|
| F1 `[^]` | **Per-generation artifact validation cache** (kills the ~4.4 s first-backend cost; validate once per generation cluster-wide, not per backend) | Promoted out of "measure first" — the measurement exists. Schedule after Stage 2; required before head-to-head marketing claims |
| F2 `[=]` | Tenant-state borrow in `BfsConfig` | Measure-first rule applies |
| F3 `[=]` | Frontier pre-allocation clamp | Measure-first rule applies |
| F4 `[+]` | Shared mmap sections for reverse CSR / filter index / edge-type registry (answers the `architecture-tradeoffs.mdx` open question: yes) | Prerequisite for F5; benchmark backend-count scaling |
| F5 `[+]` | Direction-optimizing BFS (top-down/bottom-up switching) | After F4; LDBC-verified win required |

## Stage 6 — Larger refactors `[=]`

R1 (KI-022 overlay unification), R2 (KI-025 builder regclass parity),
R3 (KI-026 endianness guard), R4 (WAL sync **implementation** — design done in
L5; background pre-applier).

## Cross-cutting rules `[=]`

Unchanged from doc 07: TDD every slice; pgrx-free internals where possible; no
`Result<_, String>` across layers; no new unsafe; docs-contract gate before
public surface changes; benchmark against current baselines before claiming
completion.

## Dependency picture

```text
S0 CI ──┬── Stage 1 (C1..C5) ─────────── Stage 3 (L1..L5) ── Stage 4 (H1,H2)
        ├── Stage 2 (O1..O5)                   │
        ├── Stage 2P: P1 import ── P3 graph_table() ── P4 hooks finding
        │        └── P2 harness ──(publish gated on C2+O3)
        ├── Stage 5: F1 now; F4 ── F5; F2/F3 measure-first
        └── Stage 6 last (R4 impl after L5 design)
```
