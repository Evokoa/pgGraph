# Sync-Log Retention Plan (C3)

Status: design complete, not yet implemented.
Owner note: this is the next open item from
`secondary-todo-and-review/10-reconciliation-with-dev-1.0.md`. The scoping
decision (full cross-backend heartbeat registry, not a `mutable_overlay`-only
prune) was made explicitly on 2026-07-17.

## Problem

`graph._sync_log` durable rows are never pruned anywhere in the codebase.
`applied_sync_id` — each backend's own replay progress in `csr_readonly`
mode — is a plain in-memory `i64` field on `Engine` (`engine.rs:137`,
`engine.rs:646` `record_applied_sync_id`), never written to durable storage
except into the `.sync` artifact sidecar at build/vacuum time. Backends
running the default trigger-sync + `apply_pending_sync` freshness mode never
publish their progress anywhere another session, or a maintenance job, can
see. A naive `DELETE FROM graph._sync_log WHERE id < X` is therefore unsafe:
it could delete rows a slower or idle backend has not replayed yet, and that
backend has no way to detect the gap — it would silently diverge from
PostgreSQL with no error.

`mutable_overlay` graphs already solve the equivalent problem for durable
projection generations via `graph._projection_generations`
(`projection/manifest.rs:1292-1448`): a per-backend, per-graph heartbeat row
with a TTL, upserted on every meaningful engine-state touch and expired by a
background sweep. This plan extends the same pattern to sync-log replay
progress so a safe prune floor exists for `csr_readonly` graphs too.

## Design

### New durable table

```sql
CREATE TABLE IF NOT EXISTS graph._sync_watermarks (
    graph_id         UUID NOT NULL,
    backend_pid      INTEGER NOT NULL,
    database_oid     OID NOT NULL,
    applied_sync_id  BIGINT NOT NULL,
    heartbeat_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (graph_id, backend_pid, database_oid)
);
```

Same key shape as `graph._projection_generations`
(`sync-internals.mdx:52-59`), so it fits the existing internal-catalog
conventions: add to the revoked-`PUBLIC`-access list in
`administration-and-security.mdx`'s "Internal Catalog Tables" section
alongside `graph._sync_log` and `graph._projection_generations`, and to
`pg_extension_config_dump` handling for backup/restore parity
(`administration-and-security.mdx:244-248`).

### Recording the heartbeat

Hook into `refreshed_engine_status()` (`sql_facade/admin.rs:1781-1798`), the
existing function that already reads `eng.applied_sync_id` and calls
`record_loaded_generation_heartbeat(manifest)` when one is loaded. This
function backs `graph.status()` and `graph.sync_health()`, both of which are
already the routine operator/scheduler touchpoints
(`run_scheduled_maintenance()` calls `sync_health()` first,
`sync-and-maintenance.mdx:151-157`). Add:

```rust
crate::sql_sync::record_sync_watermark_heartbeat(applied_sync_id)?;
```

right after the existing `record_loaded_generation_heartbeat` call, guarded
the same way (only when the graph participates in trigger sync — check
`sync_mode` first so `manual`-mode graphs, which never advance
`applied_sync_id`, don't register a meaningless heartbeat at 0). Use the
same TTL as projection heartbeats
(`graph.projection_heartbeat_ttl_secs`-equivalent GUC, or introduce
`graph.sync_watermark_heartbeat_ttl_secs` if the semantics should be
independent — recommend independent, since sync-log retention safety and
projection-generation retention are different failure domains with
potentially different acceptable staleness windows).

This piggybacks on an already-called path instead of adding a write to the
hot apply-sync loop (`apply_sync_until`, `sql_sync.rs:1090`), keeping the
per-row apply cost unchanged. The tradeoff: a backend that never calls
`status()`/`sync_health()` between applying sync and going idle won't refresh
its heartbeat until it does, which is conservative in the safe direction
(pruning waits, never proceeds early) but means very ordinary usage patterns
must be checked — confirm `ensure_current_graph_for_query` (the
`apply_pending_sync` freshness auto-apply path, `sql_facade/runtime.rs:527-569`)
also refreshes the heartbeat, since a purely-reading, never-`status()`-calling
long-lived backend must still be counted. Recommend adding the same
heartbeat call there, immediately after `apply_sync_to_high_watermark`
succeeds.

### Expiry

Add `expire_stale_sync_watermarks()` mirroring
`expire_stale_generation_heartbeats()` (`manifest.rs:1427-1438`):

```sql
DELETE FROM graph._sync_watermarks
WHERE graph_id = $1::uuid
  AND expires_at <= now()
```

Call it from the same sites that already call
`expire_stale_generation_heartbeats()` (`admin.rs:1194,1401,1782,1802,1816`) —
these are exactly the routine status/maintenance touchpoints, so no new call
sites are needed, only additions alongside the existing ones.

### Safe-floor computation and pruning

New function `sync_log_retention_floor(graph_id) -> Option<i64>`:

```text
if graph is mutable_overlay and persisted:
    durable_floor = durable projection sync_watermark (existing, from
        _projection_generations, already the authority for that path)
else:
    durable_floor = None

heartbeat_floor =
    SELECT min(applied_sync_id) FROM graph._sync_watermarks
    WHERE graph_id = $1 AND expires_at > now()
    -- NULL (no active heartbeat) if the graph has zero currently-registered
    -- backends

floor = match (durable_floor, heartbeat_floor):
    (None, None)       => None   -- prune nothing; no evidence anything is safe
    (Some(d), None)     => Some(d)
    (None, Some(h))     => Some(h)
    (Some(d), Some(h))  => Some(min(d, h))
```

**Bootstrap safety rule**: if `heartbeat_floor` is `NULL` (no backend has
registered — e.g. a freshly built graph nobody has queried since, or every
backend that ever touched it has expired), pruning must not proceed on the
strength of `durable_floor` alone being absent, and must not default to
"prune everything" — the function returns `None` and the prune step is
skipped entirely for that call. This trades availability (log grows a little
longer in the idle-graph case) for the correctness invariant this whole
feature exists to protect.

`prune_sync_log(graph_id)`:

```sql
DELETE FROM graph._sync_log
WHERE table_oid = ANY(<registered table oids for graph_id>)
  AND id < $floor
```

matching the existing graph-scoped filtering convention already used by
`read_sync_log_entries_after` (`sql_sync.rs:744-751`) and
`pending_sync_rows`. Skip entirely when `sync_log_retention_floor` returns
`None`.

Call from `graph.maintenance()`'s durable-state-folding step
(`sql_build.rs`, alongside the existing "Maintenance Rebuild" folding table
in `sync-internals.mdx:323-335`) and from `graph.run_scheduled_maintenance()`.
Do **not** call it from `apply_sync()` itself — pruning belongs with the
already-established "maintenance folds accumulated state into a clean base"
operation, not the interactive apply path.

### Diagnostics

Add to `graph.sync_health()` (`sync-and-maintenance.mdx:116-149`):

| Column | Meaning |
|---|---|
| `sync_log_retention_floor` | Computed safe-prune floor, or `NULL` if unknown |
| `sync_log_prune_recommended` | `true` when `applied_sync_id` for this backend is far enough below `max_sync_log_id` that pruning would meaningfully shrink the log, and a non-`NULL` floor exists |
| `active_sync_watermark_backends` | Count of currently-registered, unexpired heartbeat rows for this graph — operators can see whether a "stuck" backend is blocking pruning |

This mirrors the existing `durable_ingest_recommended`/`durable_gc_recommended`
boolean pattern already in `sync_health()` and gives operators a way to
diagnose "why isn't my sync log shrinking" (a lagging or crashed-without-cleanup
backend still holding an unexpired heartbeat).

## Interaction with the ingest advisory-lock fix (C1)

`_projection_generations` heartbeats and the proposed `_sync_watermarks`
heartbeats are independent tables serving independent floors and can be
implemented without ordering dependency on C1 (durable ingest locking).
Verify at implementation time whether C1 shipped in the interim
(`projection/manifest.rs`'s `create_new`/CAS publish, confirmed present on
dev per `10-reconciliation-with-dev-1.0.md`) changes anything about the
durable-projection half of the floor computation above — it should not, since
this plan reads the existing `sync_watermark` column, not the publish path.

## Test plan

- Unit: `sync_log_retention_floor` returns `None` with zero heartbeats and no
  durable projection; returns the correct `min()` across multiple active
  heartbeats; ignores expired heartbeats; correctly prefers the lower of
  durable vs. heartbeat floors.
- Unit: `prune_sync_log` never deletes a row with `id >= floor`
  (proptest over watermark orderings, matching the gate proposed in
  `09-revised-build-sequence.md` C3's original merge gate).
- pgrx integration: two-backend scenario — backend A applies sync and
  advances far ahead, backend B stays behind (simulate by not calling
  `apply_sync`/`status` on B); prune must not remove rows B still needs;
  after B calls `status()` (registering its heartbeat) and then
  `apply_sync()`, prune may proceed past B's now-current watermark.
- pgrx integration: heartbeat expiry — a backend that disconnects without
  cleanup must stop blocking pruning once its TTL elapses.
- Backup/restore: confirm `_sync_watermarks` is included/excluded from
  `pg_extension_config_dump` consistently with `_projection_generations`
  (heartbeats are backend-local runtime state and arguably should NOT survive
  a restore — verify against how `_projection_generations` is currently
  handled and match it).
- Write-heavy soak: log size stays bounded over a long run with periodic
  `run_scheduled_maintenance()` calls, matching the C3 merge gate from
  `09-revised-build-sequence.md`.

## Scope boundary

This plan does not change `apply_sync()`'s public behavior or return
columns — no SQL contract change, no release-contract regeneration needed
beyond the new internal table in `bootstrap.sql` and its documentation. It
adds one new internal catalog table, three new internal functions
(`record_sync_watermark_heartbeat`, `expire_stale_sync_watermarks`,
`sync_log_retention_floor`/`prune_sync_log`), three new `sync_health()`
columns, and wires pruning into the existing maintenance path.
