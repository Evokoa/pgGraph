# Secondary Review — Overview

Date: 2026-07-16 · pgGraph v0.1.8 · review only, no code changed.

This folder answers three questions: are there issues with pgGraph (including
the todo register), is anything seriously wrong with the implementation, and
how should the write→graph syncing story evolve into CSR / live / hybrid
operating modes.

## Documents

| File | Contents |
|---|---|
| `01-sync-architecture-review.md` | Exact trace of what happens on a write in each mode; how complete `mutable_overlay` is; sync weaknesses W1–W7 |
| `02-dual-mode-design-proposal.md` | The CSR / live / hybrid mode design; phased work plan; "what we'd have done differently" |
| `03-implementation-review-findings.md` | Core-engine findings ranked H/M/L; what's well done |
| `04-docs-and-todo-audit.md` | Docs-vs-code drift; assessment of known-issues/roadmap as a todo list |
| `05-suggested-todos.md` | Prioritized staging list to merge into the official registers |
| `06-path-to-full-graph-engine.md` | Strategy: SQL/PGQ (PG19) compatibility, performance vs. native graph DBs, and the path to replacing them |
| `07-completed-plan-build-sequence.md` | **Plan complete (2026-07-16):** all findings merged into `docs/known-issues.mdx` (KI-012…KI-026) and `docs/roadmap.mdx` (Projection Operating Modes); this file is the dependency-ordered build sequence with merge gates |
| `08-plan-delta-for-graph-engine-strategy.md` | What doc 06 changes in the doc 07 sequence: new SQL/PGQ parallel track (P1–P4), three promotions, PG19-GA clock, two register/positioning edits |
| `09-revised-build-sequence.md` | Build sequence v2 (supersedes 07) — doc 07 slices with `[=]`/`[^]`/`[+]` markers; adds Stage 2P (SQL/PGQ), L5 (WAL design), F1/F4/F5. **Written against main/v0.1.8 — read doc 10 for current status** |
| `10-reconciliation-with-dev-1.0.md` | **Authoritative status map (2026-07-17)** — every doc 09 slice reconciled against the dev 1.0 release line; corrected next-actions list; release-takeover record (full-matrix 16/17, sanitizer deferred to Linux, sfw Dockerfile change rejected) |

## Headline Answers

**Is anything seriously wrong?** No architectural rot — the base design
(Postgres authoritative, immutable mmap CSR + layered deltas + compaction,
PostgreSQL-first writes) is sound and the unsafe/persistence code is genuinely
defensive. But five findings deserve immediate attention:

1. **RLS is not enforced on topology reads** — traverse/paths/components leak
   PKs, adjacency, and connectivity of RLS-hidden rows to anyone with
   table-level SELECT (03 · H1).
2. **Durable ingest can race across backends** — process-local mutex only;
   concurrent `apply_sync()` can silently drop a segment generation
   (01 · W1, 03 · H2).
3. **Bidirectional BFS can return a non-minimal shortest path** — breaks on
   first intersection instead of minimal combined distance (03 · M1).
4. **`graph._sync_log` is never pruned** — unbounded growth on write-heavy
   graphs (01 · W2).
5. **Tenant scope is a filter, not isolation** for bitmap-tenanted graphs
   (03 · H3).

**Is the todo register healthy?** Mostly — docs are unusually accurate.
Gaps: no automated PR CI exists despite docs implying it; `mutable_overlay`
production status has no known-issues row; `README_zh.md` is stale at 0.1.5;
resolved KI rows clutter the active section; and none of the five findings
above were tracked anywhere.

**The syncing issue.** The premise "a write requires a rebuild for traversal"
is already false in the default configuration: trigger sync + default
`query_freshness = 'apply_pending_sync'` makes topology reads catch up before
executing, and the `mutable_overlay` mode already implements the
"write goes into the graph" design (durable LSM-style segments, layered reads,
compaction, GC, crash recovery, cross-backend visibility). What's genuinely
missing is not a storage engine but the **policy layer**: automation that
consumes the `sync_health()` recommendation signals, weighted-path support
over overlays, an ingest lock, sync-log retention, and an `auto` mode that
escalates under write pressure. Rebuilds remain only for weighted-path
staleness, edge-buffer overflow, TRUNCATE, schema drift, and periodic
compaction back to a clean base. See `02-dual-mode-design-proposal.md`.
