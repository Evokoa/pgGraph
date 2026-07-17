# Sync Architecture Review — What Actually Happens On A Write

Date: 2026-07-16
Scope: `graph/src/sync.rs`, `graph/src/sql_sync.rs`, `graph/src/projection/`,
`graph/src/engine.rs`, `graph/src/sql_facade/runtime.rs`, `graph/src/config.rs`,
`docs/contributor_guide/sync-internals.mdx`, `docs/user_guide/sync-and-maintenance.mdx`.

This is a review document only. No code was changed.

## TL;DR — The Premise Is Partly Outdated

The concern "a write requires a rebuild of the graph in order for traversal to
happen" is **no longer true for the common cases**, and the two desired modes
already exist in the codebase:

| Desired mode | Existing implementation |
|---|---|
| CSR — fast reads, rebuild for writes | `csr_readonly` (default projection mode, `config.rs:141`) |
| Immediately-updated-on-write graph | `mutable_overlay` (gated behind `graph.mutable_enabled = on`, `sql_build.rs:360-372`) |
| Hybrid | **Mostly unbuilt** — primitives exist, the automation/policy layer does not (see `02-dual-mode-design-proposal.md`) |

What a full rebuild is *actually* still required for:

1. **Weighted shortest path with any pending edge overlay** — hard-rejected at
   `engine.rs:966-980` because overlays don't carry stable weights.
2. **Edge-buffer overflow recovery** — `csr_readonly` edge writes accumulate in a
   capped backend-local buffer (default 100k, `config.rs:94`); overflow flips the
   engine read-only (`engine.rs:786-795`) until `graph.vacuum()`/`maintenance()`.
3. **TRUNCATE replay** and **catalog/schema drift**.
4. **Housekeeping** — folding tombstones/overlays back into a clean base
   (a compaction concern, not a visibility concern).

Everything else — node inserts, updates, deletes, filter/tenant changes, and
unweighted edge traversal visibility — is applied **incrementally without a
rebuild** in both modes.

## The Write Path, Traced

### 1. Trigger capture (identical in both modes)

With the default `graph.sync_mode = 'trigger'` (`config.rs:116`), `graph.build()`
installs AFTER row triggers + a statement-level TRUNCATE trigger on every
registered table (`sql_build.rs:199-205`, trigger SQL generated at
`sync.rs:252-366`). Each DML writes one durable row to `graph._sync_log`
(op, table OID, old/new PK, row images as jsonb, xid). The trigger **only writes
the durable log row** — it never touches any backend-local engine.

### 2. Apply — branches on projection mode

`apply_sync_to_high_watermark` (`sql_sync.rs:445-469`) branches on
`should_apply_sync_via_durable_projection()` (`sql_sync.rs:489-496`):
persisted artifact exists **and** engine is `MutableOverlay`.

**`csr_readonly` apply** (`apply_sync_until`, `sql_sync.rs:665-689`):
- Node insert → append to NodeStore + resolution delta (`sync.rs:67-94`).
- Node delete → tombstone (`sync.rs:183-215`).
- PK change → tombstone old + insert new; filter/tenant refresh.
- **Edge change → backend-local `edge_buffer` overlay** (`sql_sync.rs:1577-1638`,
  `engine.rs:779-784`), which sets `needs_vacuum = true`.
- State is **backend-local and non-durable**. Other backends replay the same log
  independently.

**`mutable_overlay` apply** (`apply_sync_via_durable_projection`,
`sql_sync.rs:498-526`):
- Sync rows → normalized `ProjectionSyncRow`s → durable L0 segment files →
  new manifest generation published atomically beside `main.pggraph`
  (`ingest.rs:149-281`, `manifest.rs:217-254`).
- Engine reloads and picks up the segments. **Cross-backend visibility without
  rebuild** — a fresh backend loads the artifact + manifest and sees the write.

### 3. Read-time visibility

- Default `graph.query_freshness = 'apply_pending_sync'` (`config.rs:121,211`)
  makes every wired topology read (traverse, shortest path, weighted path,
  components, traverse_search) call `ensure_current_graph_for_query`
  (`runtime.rs:527-569`), which replays pending sync rows up to a captured
  high-water mark **before executing the query**. This is why a committed write
  is visible to the next traversal without any operator action.
- Unweighted BFS/DFS/components merge overlays at read time: `csr_readonly`
  builds insert/delete overlay sets from `edge_buffer` + tx deltas
  (`engine.rs:725-771`, `816-871`); `mutable_overlay` uses layered reads —
  base CSR → base chunks → durable segments → legacy overlay → tx delta
  (`engine.rs:383-396`, `projection/layered.rs`).
- GQL/Cypher mapped writes get read-your-own-writes inside a transaction via
  `TxGraphDelta` (`projection/tx_delta.rs`), applied last in the layer order.
- `graph.search()` is deliberately excluded from freshness (it reads source
  tables via SQL, which is always fresh).
- Subtlety: auto-apply is skipped when the backend has a dirty tx delta
  (`runtime.rs:552-554`) to avoid folding uncommitted rows — correct, but means
  freshness quietly degrades mid-write-transaction.

## How Complete Is `mutable_overlay`?

Substantially built — much more than the roadmap's cautious wording implies:

| Capability | Where | State |
|---|---|---|
| Durable L0 segments (edges + node/resolution/filter/tenant), CRC32, fixed LE header | `projection/segment.rs` | Done, checksum-validated |
| Deterministic normalization (net-neutral cancellation, delete-wins) | `projection/normalize.rs` | Done, bounded buffer |
| Atomic manifest publish (tmp + fsync + dir-fsync + rename + verify) | `manifest.rs:217-254` | Done |
| Generation heartbeats + retention-aware GC | `manifest.rs`, `gc.rs` | Done, manual trigger only |
| Layered neighbor reads, fixed precedence | `layered.rs`, `engine.rs:383-396` | Done, correctness-gated |
| Compaction L0→L1→L2 + base-chunk rewrite | `compact.rs`, `graph.projection_compact()` | Done, manual trigger only |
| Crash recovery (healthy / chunk repair / full rebuild, manifest quarantine) | `recovery.rs:89-248` | Done |
| Tx deltas incl. subtransaction abort handling | `tx_delta.rs:155-675` | Done |
| Cross-backend visibility via artifact reload | `persistence.rs:894-904` | Done |

Gaps:

- Off by default (`graph.mutable_enabled`, `config.rs:146`); default build is
  `csr_readonly`.
- Durable projection only engages when the graph is **persisted**; a
  non-persisted mutable graph silently falls back to backend-local overlays
  (`sql_sync.rs:489-496`).
- **No automation**: ingest, compaction, GC, repair are all admin SQL calls.
  `sync_health()` computes `*_recommended` booleans but nothing consumes them.
  `VACUUM_INTERVAL_SECS` is registered but inert (`config.rs:100-103`).
- `projection/mod.rs:3-42` still carries `#[allow(dead_code)]` microphase
  annotations — not every path has a live caller yet.
- No production `todo!`/`unimplemented!`/`FIXME` in the sync/projection code
  (grep clean).

## Weaknesses Found (Ranked)

### W1 — Cross-backend manifest publication race (serious)

Durable ingest is guarded only by a **process-local**
`static Mutex<HashSet<PathBuf>>` (`ingest.rs:86-111`), not a PostgreSQL advisory
lock. Two *different backends* running `graph.apply_sync()` on the same
persisted mutable graph can both compute the same next `generation_id`, both
pass the `final_path.exists()` TOCTOU check (`manifest.rs:225-230`), and both
`rename(2)` into place — last-writer-wins can silently drop one backend's
segment generation, diverging graph state from the sync log. Build/vacuum *do*
take a per-graph advisory lock (`sql_build.rs:315-330`); ingest should too.
The durable-jobs path has row locking + advisory locks, but direct
`apply_sync()` from two sessions bypasses it.

### W2 — `graph._sync_log` is never pruned (serious, operational)

No code anywhere deletes applied `_sync_log` rows (grep across `.rs`/`.sql`
finds zero `DELETE FROM graph._sync_log`; rebuild only records
`max_sync_log_id`, `sql_build.rs:343`). The legacy `_sync_buffer` *is* cleaned
after apply (`sql_sync.rs:1828-1837`). On a long-lived write-heavy graph the
log grows without bound and every `read_sync_log_entries_after` scan
(`sql_sync.rs:744-751`) degrades. This is a prerequisite fix before promoting
any "always fresh" mode.

### W3 — No dead-lettering for poison sync rows

`apply_sync_until` advances `applied_sync_id` per row, but a row that
persistently fails to parse (e.g. malformed image in `parse_sync_row_image`,
`sql_sync.rs:328-341`) has no dead-letter path on the durable-log side. The
`_sync_log.error_message` column exists but **no code writes or reads it**.
A poison row can wedge or repeatedly re-error the apply path.

### W4 — Overlay merge cost on every read (KI-005, tracked)

`traversal_edge_overlay` allocates fresh hash sets and walks the entire
`edge_buffer` + tx delta **on every traversal call** (`engine.rs:816-871`),
even when both are empty — the fast-path short-circuit
(`has_edge_overlay()`-style) is missing at `engine.rs:725`. Layered providers
are likewise rebuilt per read. Tracked as KI-005; correctness-gated; needs
benchmarks before optimizing, but the empty-overlay short-circuit is nearly
free.

### W5 — Read-path pays the catch-up bill

With default freshness, the first topology read after a write burst
synchronously replays the whole backlog inside the querying backend
(`runtime.rs:556-557`). There is no background pre-applier, so p99 latency on
read-after-write spikes is workload-coupled. This is the operational argument
for the hybrid mode.

### W6 — Edge buffer is a hard wall in CSR mode

Overflow → engine read-only → operator must vacuum. A write-heavy CSR graph
with no scheduled maintenance wedges itself. There's no auto-promotion to
mutable mode, no auto-vacuum, and no early-warning escalation beyond the
`sync_health()` booleans nothing consumes.

### W7 — Per-backend replay duplication (by design, but costly)

In `csr_readonly`, every backend independently replays the same log into its
own heap overlays. N backends × M pending rows of duplicated work, plus
divergent staleness per backend. `mutable_overlay` solves this via shared
durable segments — another argument for making it the recommended write-heavy
mode once verified.

## Crash-Safety Assessment

The durable path is well-engineered: manifest publish is
fsync+rename+reload-verify; `.pggraph` writes use tmp+fsync+rename with a
`.sync` sidecar; `recovery.rs` quarantines corrupt manifests before rebuild;
aborted transactions' rows are excluded from durable ingest
(trigger rows roll back with their transaction, so the log only ever contains
committed intent).

Residual risk is confined to `csr_readonly` backend-local overlays: pure heap
state, lost on crash, recovered by replaying from the last persisted
`applied_sync_id` checkpoint — correct but potentially expensive after a
long-lived backend dies with a large applied backlog.
