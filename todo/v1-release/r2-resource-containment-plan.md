# R2 Resource Containment And Safe Publication Plan

This plan executes full-engine checkpoints 1B and 1C for the pgGraph 1.0
release. Each checkpoint must keep PostgreSQL tables authoritative, preserve
the last usable projection on failure, update public documentation, and retain
repeatable evidence under `todo/measurements/`.

## R2A: Typed Resource Policy And Build Rejection — Complete

- Introduce checked byte, memory-budget, and elapsed-time domain types at one
  resource-policy boundary. Add disk, row, and work units with their first
  enforced R2B/R2C consumers rather than as dormant abstractions.
- Add a statement-local governor with named phases, fallible reservations,
  peak accounting, and RAII release.
- Make build orchestration the only owner of build-memory preflight and express
  estimates in integer bytes rather than rounded MiB.
- Treat unknown source statistics conservatively and reject arithmetic
  overflow. R2B adds runtime enforcement for stale positive estimates.
- Keep legacy `graph.oom_action` spellings readable for 1.x compatibility, but
  reject over-budget construction for every setting. Read-only remains a
  serving/mutation state, not permission to perform an unsafe build.

Exit evidence: governor unit/property tests, PostgreSQL build rejection tests,
formatting, Clippy, rustdoc, documentation/contract drift, and focused review.

## R2B: Enforced Build Reservations And Adaptive Batches — Complete

- Resolve the effective build budget from the configured cap minus current
  backend-private graph residency and a documented safety reserve.
- Reserve phase workspace before growing node, relationship, filter,
  resolution, and persistence buffers; use `try_reserve` after the lease.
- Replace fixed row-only build batches with bounded byte-adaptive batches.
- Report resolved budget, current/peak bytes, peak phase, and pressure events.

Exit evidence: stale/no-statistics, long-key, wide-filter, and low-memory build
profiles fail before allocation or stay within the declared envelope.

## R2C: Load, Query, Sync, Compaction, And Analytics Breakers — Complete

- Apply the same governor to mapped-load metadata, source candidates,
  expansions, frontiers, paths, hydration, blocking operators, sync
  normalization, compaction, and analytics workspaces.
- Add byte, row, work, disk, and elapsed breakers with interrupt checks.
- Preserve the existing backend engine and current generation on every
  reservation, validation, cancellation, or disk failure.

Exit evidence: production-shaped RSS/PSS and pathological workload gates cover
all advertised operations and return stable typed resource diagnostics.

## R2D: Generation-Safe Publication — Manifest Subphase Complete

- Publish generation-specific staged artifacts under graph-scoped
  cross-backend exclusion and compare-and-swap the current generation.
- Fsync, mmap-validate, catch up, and recheck catalog/security state before the
  current-manifest switch.
- Retain the previous serving generation through reader pins and rollback
  retention; garbage collection removes only unpinned obsolete generations.

The durable manifest pointer, cross-backend compare-and-swap, reader pins, and
bounded generation garbage collection are complete. The generation-specific
base-artifact switch remains coupled to the direct v5 artifact build in R3C/R3D
and must land before R2D and KI-018 close.

Exit evidence: competing publisher, injected write/checksum/load/catch-up
failure, crash, old-reader, rollback, and bounded-GC tests preserve exactly one
valid current generation.

## R2 Completion

Run the full PostgreSQL 14-18 release matrix, process sanitizer, documentation
and contract drift, secrets, package/fresh-install smoke, and resource/crash
profiles. Retire KI-012, KI-013, and KI-018 only when their complete acceptance
evidence is green; partial subsystem work remains explicit in the closure
ledger.
