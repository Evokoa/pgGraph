# Todo Program Progress

The active phased program is tracked in
[`full-graph-engine/progress.md`](./full-graph-engine/progress.md). That file is
the authoritative checkpoint handoff, measurement log, and next-action record.

Last synchronized: 2026-07-16

Current phase: R4 release-risk refactoring. R1 through R3 are complete: the
bounded persisted build streams a fenced PostgreSQL snapshot through governed
runs into a validated generation-specific artifact, then publishes it with
generation compare-and-swap without exposing an unvalidated serving base.

Release planning: [`v1-release/README.md`](./v1-release/README.md) is now the
single source of truth for pgGraph 1.0 scope. The existing full-engine plans
remain technical references; PostgreSQL 19, full ISO GQL, competitive breadth,
and dynamic graphs are post-1.0 roadmap work. R1 through R3 are complete; R4 is
the active release checkpoint.

2026-07-16 R2D manifest publication — Projection publishers now stage and
validate immutable manifests, compare-and-swap a bounded checksummed current
pointer under graph-scoped cross-process exclusion, retain reader-protected
ancestors, and collect only unprotected artifacts/manifests/temp files. The
generation-specific base switch remains paired with R3 artifact v6. The full
release gate is green except for the unchanged cold auto-load latency gate;
the complete post-pgbench tail passes and the measured blocker is owned by R3.

2026-07-16 R3 planning — The ordered execution contract is source/catalog
fencing and W0/W1 verification, governed checksummed fixed-fanout runs, streamed
artifact v6 with both CSR directions and mapped filters/dictionaries, then the
direct persisted spill path and equivalence/leak/fault gates.

2026-07-16 R3A source boundary — Build and vacuum now lock mapping catalogs,
partition roots, and descendants in stable order, reject production callers
that already own source/catalog write locks, capture the sync watermark behind
the exclusive writer barrier, and recheck catalog, schema, ACL, and watermark
before persistence and serving-state installation. The production PostgreSQL
17 gate passes caller-owned catalog/source locks, partition-leaf locks,
concurrent writers, rollback/commit horizons, and competing publication.

2026-07-16 R3B governed runs — Versioned checksummed run files now cover nodes,
both relationship orientations, filters and dictionaries, and resolution.
Collectors and fixed-fanout merge enforce memory, disk, row, work, file, time,
and record bounds; private permissions, corruption, quota, duplicate, abandoned
cleanup, and four-pass merge tests pass. Production integration remains R3D.

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

2026-07-12 R1 mutable relationship visibility — Two-role PostgreSQL 17 tests
prove durable post-build relationship segments remain fail-closed under RLS for
coordinate, hydrated, aggregate, and existence outputs.

2026-07-12 R1 PostgreSQL write boundaries — Partition routing, CHECK failures,
and user-trigger failures now have PostgreSQL-backed GQL CREATE coverage with
rejected writes proven absent from transaction-local graph state.

2026-07-13 R1 transaction isolation — A two-session PostgreSQL 17 gate now
proves READ COMMITTED statement visibility and REPEATABLE READ/SERIALIZABLE
snapshot retention with matching source and graph results. The gate exposed
KI-026: durable ingestion can consume a newly created node's watermark without
persisting its identity; this is now an explicit P0 blocker for R3.

2026-07-13 R1 durable node identity — Segment v6 now retains exact primary-key
and tenant bytes, allocates contiguous post-build node slots without mutating
the serving engine, validates and installs staged node state atomically, reloads
after direct ingestion, serializes publication across PostgreSQL backends, and
fails incremental TRUNCATE without advancing the watermark. PostgreSQL 17
lifecycle, second-batch, filter, tenant, topology, and watermark tests pass.
Independent review findings for batch ordering, tenant-only identity stability,
transient lifecycles, pre-publication validation, cold-backend loading, and
serving-state atomicity are fixed. Allocation now reads a clean persisted
snapshot instead of transaction-local serving slots. Unicode composite keys,
standalone relationship tables, later endpoints, tenant-only moves, direct
reload, and multiple allocation batches pass the PostgreSQL lifecycle; staged
replay unit tests cover historical PK and transient-slot state. The real
two-publisher PostgreSQL 17 gate passes with an empty retry and no watermark
loss. A shared-writer/exclusive-ingest PostgreSQL transaction barrier prevents
out-of-order commits and rollbacks from being skipped, including DML and apply
in one transaction. Older trigger definitions fail closed until refreshed,
exact target endpoints cannot resolve against a different table with the same
key, and standalone endpoint identity changes fail closed with rebuild
guidance. Durable `graph.apply_sync()` statistics are verified against the
exact published batch.
The persisted isolation matrix now passes durable new-node ingestion and
repeated persisted builds after KI-027 manifest rebasing.

2026-07-13 R1 persisted rebuild rebasing — Persisted mutable build and vacuum
now publish a monotonic base-only manifest after replacing the base artifact,
carry forward operation timestamps, and record superseded segments, chunks,
and identity dictionaries for generation-aware garbage collection. The
PostgreSQL 17 persisted isolation matrix passes repeated builds. KI-018 remains
the separate crash-atomic generation-specific base-switch blocker.

2026-07-13 R1 definer search-path reassessment — Static review found that
pgrx-exported definer functions are pinned, but dynamically generated trigger
sync row and truncate functions still inherit the caller search path. KI-023
remains open until those functions and the release metadata audit are hardened.

2026-07-13 R1 definer search-path hardening — Extension and generated trigger
definers now place `pg_catalog` before explicit `pg_temp`; shared caller-role
checks use PostgreSQL's outer-user identity and qualify authorization-critical
catalog, function, type, and operator references. PostgreSQL 17 temporary-table,
persistent function/operator, trigger-function, and metadata attacks pass.

2026-07-14 R1 definer search-path checkpoint — KI-023 is closed on PostgreSQL
17: the 970-test pgrx suite, generated-function metadata audit, strict
publication/writer-barrier gate, release-contract drift checks, Clippy,
rustdoc, and independent review pass. PostgreSQL 14-16 and 18 matrix evidence
remains before the known issue can be retired.

2026-07-14 R1 mapped-layout baseline — KI-020 begins from 14 passing mapped
tests (0.10s test time, 0.55s wall time). The implementation will replace
borrowed raw-pointer lifetime contracts with an owning validated artifact and
explicit little-endian native-load policy before sanitizer and PG14-18 gates.

2026-07-14 R1 mapped-layout checkpoint — KI-020 now uses a private validated
artifact capability and Arc-owned aligned ranges over a backend-local anonymous
read-only snapshot, rejects non-little-endian native loads, accounts the
snapshot as private backend memory, and passes source-inode truncation,
malformed-layout, Miri, full Rust ASan, and the 975-test PostgreSQL 17 suite.
The sanitizer gate now runs correctly on macOS; its full run also drove a
stack-safe 128-level public boolean predicate bound. Independent review findings
for source-inode mutability, lifecycle docs, 32-bit offset conversion, and
status accounting are fixed. PostgreSQL-process sanitizer and PG14-16/18 matrix
evidence remain pending.

2026-07-14 R1 trigger identity — Node and relationship writes now re-read their
source rows after statement triggers. Relationship `CREATE` rejects rewritten
source keys, endpoints, or dynamic labels; node `CREATE` and `MERGE` insert
reject moved or removed returned identities; `SET`, `REMOVE`, and `MERGE ON
MATCH` reject rewritten primary-key or tenant identity. PostgreSQL 17 covers
`BEFORE` and side-effecting `AFTER` triggers plus authoritative ordinary
property rewrites. QA-01A passes formatting, Clippy, rustdoc, release-contract
and documentation drift, secret scanning, 718 Rust tests (1 ignored), and 977
PostgreSQL 17 pgrx tests (1 ignored). An independent raw-diff review found no
remaining block or request-change findings.

2026-07-14 R1 source-shape matrix — Partitioned node and relationship sources,
composite relationship identities, PostgreSQL constraints, rejecting and
value-mutating triggers, authoritative returned/filter values, and rollback of
transaction-local projection state are covered for the advertised write
families. Binder and durable replay now share partition-root-aware foreign-key
resolution, including a duplicate-key decoy regression. QA-01B passes Clippy,
rustdoc, release/docs drift, 718 Rust tests (1 ignored), and 981 PostgreSQL 17
pgrx tests (1 ignored).

2026-07-14 R1 write isolation and concurrency — The PostgreSQL 17 two-session
matrix now executes every advertised write family under READ COMMITTED,
REPEATABLE READ, and SERIALIZABLE and verifies writer returns, source rows,
transaction-local graph state, reader snapshots, and post-commit agreement.
Advisory/trigger handshakes make same-key MERGE and relationship CREATE/DELETE
races deterministic; stale SET, REMOVE, tenant, relationship DELETE, and DETACH
losers expose exactly one expected SQLSTATE and retain no graph delta. All lock
readiness checks are database-scoped and abort safely on failed orchestration.
The full and persisted new-node isolation profiles plus all focused race gates
pass, and the required third-phase independent raw-diff review reports PASS.

2026-07-14 R1 supported-major write matrix — QA-01D passes on PostgreSQL
14.23, 15.18, 16.14, 17.10, and 18.4: each major passed 718 release Rust tests
(1 ignored), 981 pgrx tests (1 ignored), and the full, persisted, MERGE,
relationship, and stale-write concurrency profiles. KI-001, KI-002, KI-009,
and KI-019 are retired; the worktree was revalidated with no accidental source
deletions, and the independent raw-diff review passes after its cache-resume and
KI-020 status-consistency findings were fixed.

2026-07-14 R1 PostgreSQL-process sanitizer — KI-020 is retired after PostgreSQL
17.10 persisted mmap, corruption, build-job, callback, and guarded-error paths
completed under Valgrind with zero unsuppressed errors; the reusable local and
Docker release gates and public contributor documentation now describe the
exact process-level contract.

2026-07-14 R1 durable projection matrix — KI-026 is retired after the
cross-backend lifecycle and publication/writer-lock profiles passed unchanged
on PostgreSQL 14.23, 15.18, 16.14, 17.10, and 18.4; the reusable Docker matrix
also corrected the stale pre-named-graph artifact-path assumption in the lock
gate.

2026-07-14 R1 supported-major safety closure — Concurrent rename and
drop/recreate gates pass on PostgreSQL 14.23 through 18.4, proving relation-lock
serialization, OID-stable rename, and fail-closed replacement behavior. With
the existing five-major Rust and pgrx evidence, KI-014 through KI-017 and
KI-021 through KI-024 are retired and the R1 supported-major evidence row is
complete. Independent review passed after active-key tracking and bounded
client cleanup removed a failure-path wait risk from the reusable DDL gate.

2026-07-15 R2A checked build policy — Build estimates now use checked integer
bytes and exact source counts when PostgreSQL statistics are unknown. A
statement-local governor accounts for serving and replacement reservations,
and every legacy `graph.oom_action` spelling rejects an over-budget build
without replacing the serving graph. The 723-test Rust suite and 987-test
PostgreSQL 17 pgrx suite pass, with formatting, Clippy, rustdoc, doctests, and
documentation/contract drift green.

2026-07-15 R2B live build budget — Build, CSR reversal, and optional
persistence now hold phase-specific reservations under the effective
replacement budget; node, edge, and endpoint spools flush on adaptive byte
pressure and major vectors grow fallibly. Long keys, stale positive statistics,
low-memory replacement, and persisted typed filters pass with 728 Rust tests
and 994 PostgreSQL 17 tests (one intentional ignore in each suite), plus all
static, documentation, contract, and secret gates.

2026-07-15 R2C runtime resource breakers — One statement/maintenance governor
now spans persisted load, direct search, traversal, GQL, hydration, workflow
postprocessing, durable sync, range compaction, and analytics. Dense-token and
Unicode search, all public workflow wrappers, and publication-preservation
boundaries are covered. The checkpoint passes 778 Rust tests and 1,049
PostgreSQL 17 tests (one intentional ignore in each suite), a three-load/two-sync
runtime profile at 87 MiB peak RSS, strict normal and pg-test Clippy, rustdoc,
schema/contract/docs drift, gitleaks, and independent terminal review.

2026-07-11 R1 compaction — Segment format v5 and identity-aware layered keys
preserve equal-endpoint parallel relationship rows, weights, and specific
tombstones through normal compaction and dirty-range base-chunk replacement.

2026-07-11 release-gate maintenance: added an enabled-by-default gitleaks gate
for full Git history and pending tracked changes, with redacted output and a
standalone `scripts/check_secrets.sh` entry point.

2026-07-11 R0 — Froze the PostgreSQL 14-18 and documented GQL 1.x contract,
published compatibility/deprecation guidance, added a drift-checked API/GUC/
diagnostic inventory, release-note template, and versioned migration fixtures.

2026-07-16 R3C artifact v6 follow-up — The 26-section format now also persists
sorted tenanted-table OIDs, a lexical tenant dictionary, dense per-node tenant
tokens, and the unidirectional-edge capability flag. Current-manifest readers
pin and resolve generation-specific bases; checksum verification is bounded,
and focused persistence, recovery, tenant, run, and resource suites pass.
Direct source-to-run artifact construction remains the R3D checkpoint.

2026-07-16 R3 bounded persisted build — Persisted build and vacuum now stream a
coherent fenced PostgreSQL snapshot through governed external runs into a
validated generation-specific artifact v6, publish by generation CAS, retain
the prior serving generation on every tested fault, and pass the complete
release gate plus independent review. The final PostgreSQL 14–18 Docker matrix
passes 860 Rust and 1,132 serialized pgrx tests per major, the GQL write and
isolation profiles, and every durable projection profile.
