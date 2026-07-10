# Todo Program Progress

The active phased program is tracked in
[`full-graph-engine/progress.md`](./full-graph-engine/progress.md). That file is
the authoritative checkpoint handoff, measurement log, and next-action record.

Last synchronized: 2026-07-10

Current phase: Checkpoint 1A relationship identity persistence, base-CSR GQL
query/path propagation, base relationship hydration, and mapped relationship-row
RLS for single-pattern reads, supported joins, and base wildcard path outputs
are implemented and targeted on PostgreSQL 17 tests; transaction-local
relationship `CREATE` overlays now preserve source identity for same-transaction
visibility and hydration. Continue with sync, compaction, broader writes, and
savepoint use of persisted relationship IDs.
