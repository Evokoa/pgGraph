# Todo Program Progress

The active phased program is tracked in
[`full-graph-engine/progress.md`](./full-graph-engine/progress.md). That file is
the authoritative checkpoint handoff, measurement log, and next-action record.

Last synchronized: 2026-07-12

Current phase: Checkpoint 1A relationship identity persistence, base-CSR GQL
query/path propagation, base relationship hydration, and mapped relationship-row
RLS for single-pattern reads, supported joins, and base wildcard path outputs
are implemented and targeted on PostgreSQL 17 tests; transaction-local
relationship `CREATE` overlays now preserve source identity for same-transaction
visibility and hydration; trigger-sync inserted relationship edges now carry
source identity through durable segments, layered mutable-overlay reads, and
representable compacted segment rows. Parallel-aware compaction, dirty-range
chunk replacement, and savepoint rollback/release are complete. Continue with
broader write identity, visibility, filter, and isolation coverage.

Release planning: [`v1-release/README.md`](./v1-release/README.md) is now the
single source of truth for pgGraph 1.0 scope. The existing full-engine plans
remain technical references; PostgreSQL 19, full ISO GQL, competitive breadth,
and dynamic graphs are post-1.0 roadmap work. The next implementation work is
still R1/Checkpoint 1A broader write identity, visibility, filter, and isolation
closure; parallel-aware compaction, dirty-range identity preservation, and
savepoint delta handling are complete.

2026-07-11 R1 savepoints — Transaction-local graph overlays now follow nested
PostgreSQL savepoint and PL subtransaction release/rollback semantics, including
when the extension is first loaded from inside an existing savepoint.

2026-07-11 R1 filter identity — Build, pushdown, sync, and transaction updates
use table OID plus column identity; ambiguous unqualified public filters fail
with guidance instead of selecting the first same-named column.

2026-07-11 R1 relationship visibility — PostgreSQL 17 RLS regressions now prove
fail-closed behavior before aggregate, existence, and relationship-list output;
mutable-overlay role coverage remains.

2026-07-12 R1 relationship authorization — Join and wildcard plans preflight
every mapped edge-table ACL even for empty results, and mapped wildcard rows
fail closed when stable source identity is missing or belongs to another plan.

2026-07-12 R1 durable relationship identity — Post-build relationship rows now
retain canonical identities through manifest publication, backend reload, and
standalone mapped relationship-table trigger replay; delete specificity remains.

2026-07-12 R1 relationship deletion — Transaction overlays, immediate sync,
durable ingestion, and GQL row deletion now tombstone canonical relationship
identities without hiding parallel siblings.

2026-07-11 R1 compaction — Segment format v5 and identity-aware layered keys
preserve equal-endpoint parallel relationship rows, weights, and specific
tombstones through normal compaction and dirty-range base-chunk replacement.

2026-07-11 release-gate maintenance: added an enabled-by-default gitleaks gate
for full Git history and pending tracked changes, with redacted output and a
standalone `scripts/check_secrets.sh` entry point.

2026-07-11 R0 — Froze the PostgreSQL 14-18 and documented GQL 1.x contract,
published compatibility/deprecation guidance, added a drift-checked API/GUC/
diagnostic inventory, release-note template, and versioned migration fixtures.
