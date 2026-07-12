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
visibility and hydration; trigger-sync inserted relationship edges now carry
source identity through durable segments, layered mutable-overlay reads, and
representable compacted segment rows. Continue with parallel-aware compaction,
broader writes, and savepoint use of persisted relationship IDs.

Release planning: [`v1-release/README.md`](./v1-release/README.md) is now the
single source of truth for pgGraph 1.0 scope. The existing full-engine plans
remain technical references; PostgreSQL 19, full ISO GQL, competitive breadth,
and dynamic graphs are post-1.0 roadmap work. The next implementation work is
still R1/Checkpoint 1A parallel-aware compaction, followed by broader identity,
visibility, filter, and savepoint closure.

2026-07-11 release-gate maintenance: added an enabled-by-default gitleaks gate
for full Git history and pending tracked changes, with redacted output and a
standalone `scripts/check_secrets.sh` entry point.
