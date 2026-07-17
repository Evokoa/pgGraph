# Plan Delta — What The Graph-Engine Strategy Changes In Doc 07

Date: 2026-07-16
Baseline: `07-completed-plan-build-sequence.md` (Stages 0–6).
Driver: `06-path-to-full-graph-engine.md` (PG19 SQL/PGQ + replace-native-DBs).

## Summary

Nothing in Stages 0–2 is removed or reordered — the correctness/security/CI
work is a hard prerequisite for the graph-engine ambition too. The strategy
adds **one new stage** (SQL/PGQ compatibility), **promotes three existing
items**, adds an **external clock** (PG19 GA, ~fall 2026), and requires **two
positioning/register edits**. Everything else stands.

## 1. Unchanged (and why they matter more now)

- **S0 CI, C1 ingest lock, C3 retention, C4 RLS, C5 tenant** — unchanged;
  these are Pillar E ("things that disqualify you in evaluations"). Graph-DB
  refugees evaluating a replacement will hit them in week one.
- **Stages 3–4 (live/hybrid modes)** — unchanged; Pillar C is doc 02 verbatim.
- **Stage 6 R1–R3** — unchanged.

## 2. Promotions (existing items, new urgency or scheduling)

| Item | Was | Becomes | Why |
|---|---|---|---|
| C2 bidirectional BFS minimality | Stage 1, no external deadline | Stage 1 with a hard gate: **must land before any published benchmark** | A wrong shortest path in an LDBC comparison is unrecoverable publicly |
| Per-generation artifact validation cache | Stage 5 "any time, only with a benchmark showing it matters" | Scheduled slice (new Stage 2½ / early Stage 5) | The benchmark already exists: the measured ~4.4 s first-backend query. Native graph DBs have zero per-connection warmup; this is a head-to-head disqualifier, not a nice-to-have |
| R4 WAL/logical-replication sync | Stage 6 "roadmap-owned, last" | Committed roadmap item, design starts during Stage 3 | "Replace a graph database" requires the always-current write story at OLTP rates; trigger overhead caps it. Implementation can stay late, but the design decision (logical decoding consumer shape) gates Stage 3 choices |
| O3 truncation signal | Stage 2 hardening | Unchanged position, add gate: required before benchmark publication | Silent partial results in a bench harness produce false wins that get called out |

## 3. New stage — SQL/PGQ compatibility (insert as Stage 2P, parallel track)

Runs parallel to Stages 1–3 after S0; owns the PG19 GA clock.

| Slice | What | Depends on | Merge gate | Target |
|---|---|---|---|---|
| P1 | `graph.import_property_graph()`: read `pg_propgraph_*` catalogs → pgGraph registration; drift detection via `pg_get_propgraphdef()`; version-gated to PG19+ | S0 only | Round-trip test: CREATE PROPERTY GRAPH → import → build → traverse on PG19 beta; drift flag on ALTER | **Before PG19 GA** |
| P2 | LDBC SNB harness (`sandbox/benchmark` ldbc preset) incl. plain-PG19 `GRAPH_TABLE` comparison on same box | S0; C2+O3 before *publication* | Reproducible harness in-repo; baseline numbers recorded | Harness now; publish after C2/O3 |
| P3 | `graph.graph_table()`: accept the standard SQL/PGQ pattern grammar (incl. variable-length forms core rejects), resolved against imported property graphs; frontend adaptation of the existing GQL parser feeding the existing typed adapter seam | P1 | Compatibility matrix (like `cypher_compatibility()`); every supported pattern also parseable by PG19 where fixed-depth, with matching results (differential test) | GA + 0–6 mo |
| P4 | CustomScan/planner-hook prototype: can GRAPH_TABLE-derived join trees be recognized and substituted? Outcome is a finding either way | P1, P3 | Written finding: feasible (→ slice) or not (→ upstream hook proposal with LDBC evidence) | GA + 6–18 mo |

New Stage 5 additions (benchmark-driven, same rule as existing Stage 5):
direction-optimizing BFS (needs shared reverse CSR), and **shared mmap
sections for reverse CSR / filter index / edge-type registry** — currently an
"open question" in `architecture-tradeoffs.mdx`; the strategy answers it yes,
so it becomes a tracked Stage 5/6 item rather than a question.

## 4. Positioning / register edits required

1. **Roadmap "Reserved Features" SQL/PGQ row** currently says: internal
   adapter seam only, expose "only after PostgreSQL provides stable
   graph-pattern hooks." The strategy splits this in two:
   *catalog import (P1) and the standard-pattern function surface (P3) do not
   wait for hooks*; only transparent GRAPH_TABLE acceleration (P4) does.
   The row (and `sqlpgq_adapter.rs`'s "does not parse SQL/PGQ text" doc
   comment) needs rewording to reflect the deliberate, scoped reversal.
2. **Non-goals wording.** "Replace native graph databases" must stay scoped as
   *workload* replacement (bounded traversal over operational relational
   data), not storage replacement — the existing non-goals table
   ("not a separate graph database hidden inside PostgreSQL") remains true
   and should be cross-referenced from any marketing claim.

## 5. Net sequencing picture

```text
S0 CI ──┬── Stage 1 (C1..C5) ── Stage 3 live ── Stage 4 auto
        ├── Stage 2 (O1..O5)         │
        ├── Stage 2P: P1 import ── P3 graph_table() ── P4 CustomScan/hooks
        │        └── P2 LDBC harness (publish after C2+O3)
        └── Stage 5 perf (validation cache promoted; dir-opt BFS, shared CSR added)
                                     Stage 6 refactors (R4 WAL design pulled into Stage 3 window)
```

External anchor: **P1 lands before PG19 GA**; publishable LDBC + GRAPH_TABLE
comparison lands as close to GA as C2/O3 allow.
