# Design Proposal — CSR / Live / Hybrid Operating Modes

Date: 2026-07-16
Status: proposal only, no code written.
Depends on: `01-sync-architecture-review.md`.

## The Ask

> CSR → fast for reads, needs to be built for writes (read-heavy databases)
> ??? → graph is immediately updated on a write, good read performance (hot data)
> ??? → some sort of combo

## Key Finding: You Already Built Most Of This

The two base modes exist today:

| Ask | Exists as | Gap |
|---|---|---|
| CSR mode | `csr_readonly` (default) | Edge writes land in a capped backend-local buffer, not "rebuild only"; overflow wedges the engine read-only |
| Immediate-update mode | `mutable_overlay` + durable projection segments + layered reads | Off by default, requires persistence, zero automation, not benchmark-verified |
| Hybrid | — | The actual missing work: a policy/automation layer over existing primitives |

Writes are *already* "written to the graph too": trigger → `graph._sync_log` →
(default) auto-apply before every topology read. In `mutable_overlay` the apply
publishes durable segments visible to all backends without rebuild. The
perception that "a write requires a rebuild" comes from four real but narrower
facts: weighted paths reject pending overlays, CSR edge buffers overflow into
read-only mode, compaction back to a clean base is manual, and `mutable_overlay`
is gated off so almost everyone runs CSR.

**Recommendation: do not design a third storage engine. Productize the modes
you have, then add the hybrid as a policy layer.** LSM-style
base-plus-overlay-plus-compaction is the correct architecture for this problem
(it is what most mutable graph indexes converge on), and the hard parts —
durable segments, atomic manifests, layered precedence, crash recovery — are
done.

## Proposed Mode Model (user-facing)

Rename/document the modes by workload rather than by internals:

| Mode | Maps to | Semantics |
|---|---|---|
| `read_optimized` | `csr_readonly` | Clean CSR fast path. Writes visible via auto-applied overlays; periodic maintenance folds them in. Best for mostly-static graphs. |
| `live` | `mutable_overlay` + persistence + auto-maintenance | Writes durable in segments at apply time, visible cross-backend, background compaction keeps read amplification bounded. |
| `auto` (hybrid) | policy layer | Starts read_optimized; monitors write pressure via existing `sync_health()` signals; escalates apply/compaction cadence — and optionally the mode — automatically. |

The internal names can stay; this is a docs/API-surface decision.

## Work Plan

### Phase 1 — Prerequisites (correctness/operational, do first)

1. **Advisory-lock durable ingest** (fixes W1 in doc 01). Ingest/manifest
   publication must take the same per-graph PostgreSQL advisory lock that
   build/vacuum already take (`sql_build.rs:315-330`), not just the
   process-local mutex (`ingest.rs:86-111`). Without this, two backends calling
   `apply_sync()` can race manifest generations (last-writer-wins data loss in
   the projection).
2. **Sync-log retention** (fixes W2). Add a checkpointed prune: delete
   `_sync_log` rows below `min(applied watermark across durable manifests,
   retention floor)`, run from maintenance and `run_scheduled_maintenance()`.
   Any always-fresh mode is a time bomb without it.
3. **Poison-row dead-lettering** (fixes W3). Wire the existing-but-unused
   `_sync_log.error_message` column: on repeated apply failure, mark the row,
   skip it, surface a count in `sync_health()`, and document operator recovery.

### Phase 2 — Make `live` mode real

4. **Auto-maintenance driver.** `sync_health()` already computes
   `apply_sync_recommended`, `durable_compaction_recommended`,
   `durable_gc_recommended`, `maintenance_recommended` — nothing consumes them.
   `graph.run_scheduled_maintenance()` exists as the scheduler-safe entry.
   Missing piece: make the Docker/pg_cron default cadence adaptive (act on the
   booleans, tighter cadence under write pressure) and document a supported
   non-Docker setup path prominently. Optionally: a one-shot dynamic background
   worker kick after apply when compaction is recommended
   (`run_due_jobs_async()` already demonstrates the pattern).
5. **Weighted paths over overlays.** Segments already carry a weight section.
   Extend Dijkstra's neighbor source to the layered provider so
   `weighted_shortest_path()` stops hard-rejecting pending overlays
   (`engine.rs:966-980`). Until then, "live" mode has an asterisk.
6. **Benchmark + verification signoff** the roadmap already calls for:
   dirty-read amplification vs. segment fanout, apply throughput, compaction
   pause behavior. Publish numbers; flip `mutable_enabled` default or at least
   remove the double gate (`mutable_enabled` + explicit build mode) if results
   hold.
7. **Non-persisted mutable graphs should refuse or warn.** Today they silently
   degrade to backend-local overlays (`sql_sync.rs:489-496`) — surprising
   divergence from the mode's promise. Prefer: `mutable_overlay` build requires
   `persist_on_build = on`.

### Phase 3 — The hybrid (`auto`) mode

A policy layer, not a new engine:

- **Signals** (all already computed): pending sync rows, edge-buffer fill %,
  overlay memory, segment fanout, tombstone ratio, read amplification.
- **Actions**: trigger apply, trigger compaction, schedule maintenance,
  and (the one new decision) *mode escalation* — a `csr_readonly` graph whose
  edge-buffer pressure keeps crossing thresholds gets rebuilt as
  `mutable_overlay` at the next maintenance window (and can demote back when
  write rates fall). Escalation should be opt-in (`graph.projection_mode =
  'auto'`) and logged loudly.
- **Read fast-path**: short-circuit overlay assembly when buffers/deltas are
  empty (`engine.rs:725`, `816-871`) so a compacted `live` graph reads at
  CSR speed. This is what makes "good read performance" true in the hybrid:
  clean generations behave exactly like CSR mode.

### Phase 4 — Nice-to-haves (later)

- Background pre-applier so the first read after a write burst doesn't pay the
  whole catch-up bill synchronously (W5).
- WAL/logical-replication sync (`sync_mode = 'wal'` is already reserved) to
  replace trigger overhead on high-write tables — this is the long-term answer
  for "data moving a lot", since it removes per-row trigger cost from the write
  path entirely.
- Incremental/indexed overlay merge if benchmarks show KI-005 matters after the
  empty-overlay short-circuit.

## What I Would Have Done Differently (honest assessment)

1. **Ship the automation with the mechanism.** The projection subsystem is
   excellent, but shipping compaction/GC/repair as manual admin calls with
   recommendation booleans nobody consumes means the flagship capability
   (mutable graphs) is effectively unusable outside the Docker image's pg_cron
   preset. The LSM design is only as good as its background maintenance.
2. **One overlay system, not two.** The legacy backend-local `edge_buffer` and
   the durable projection segments coexist, with precedence rules gluing them
   together in layered reads. That doubles the read-merge logic, the failure
   modes, and the docs burden. Path out: make durable segments the only edge
   overlay for persisted graphs in both modes (CSR mode = segments exist but
   compaction folds eagerly), and retire `edge_buffer` to
   transaction-local-only duty.
3. **Weighted-path refusal should have been scoped at design time.** Segments
   carry weights; the refusal at `engine.rs:966-980` is a seam that was left
   for later but shows up to users as "writes break weighted queries".
4. **The freshness default is right, but its cost model is invisible.** Reads
   paying arbitrary catch-up synchronously is a surprising latency cliff;
   at minimum `sync_health()`/docs should quantify it, ideally a pre-applier
   bounds it.

None of these are architectural mistakes — the base design (Postgres
authoritative, derived rebuildable artifact, immutable base + layered deltas +
compaction) is sound and matches the AGENTS.md source-of-truth principle. The
misses are sequencing: mechanism before policy, two overlay generations
coexisting, and defaults that hide the good mode.
