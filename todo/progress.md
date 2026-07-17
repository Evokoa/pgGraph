# Todo Program Progress

The active phased program is tracked in
[`full-graph-engine/progress.md`](./full-graph-engine/progress.md). That file is
the authoritative checkpoint handoff, measurement log, and next-action record.

Last synchronized: 2026-07-16

Current phase: R7 release candidate assurance. R1 through R6 are complete.
R6 froze the exact 134-function SQL surface and bounded GQL read/write profile,
made their public inventories and documentation drift-enforced, and passed the
clean PostgreSQL 14-18 package, alpha migration, packaged quickstart, and
packaged CSR/mutable playground gates.

Release planning: [`v1-release/README.md`](./v1-release/README.md) is now the
single source of truth for pgGraph 1.0 scope. The existing full-engine plans
remain technical references; PostgreSQL 19, full ISO GQL, competitive breadth,
and dynamic graphs are post-1.0 roadmap work. R1 through R6 are complete; R7 is
the active release checkpoint.

2026-07-16 R6 documentation and packaging — Public SQL/GQL inventories,
compatibility and migration guidance, rendered documentation, CI tiers, script
ownership, PG14-18 release packages, the real alpha-to-1.0 transition,
archive-built quickstart, and 40 CSR/41 mutable playground queries pass from the
clean `267bd39` source archive with no R6 waiver; the independent review
correction at `57acb58` adds the production discovery-lock regression without
changing runtime source.

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

2026-07-16 R4 release-risk refactoring — A generated 75-entry script inventory,
10-block unsafe allowlist, declarative locked/resumable release tiers, and a
modular bounded playground replace implicit ownership. The packaged PostgreSQL
17 reference passes 40 CSR and 41 mutable shared-catalog examples without
changing expected results.

2026-07-16 R5 supported profile — The exact SQL and GQL 1.0 surfaces are
generated and drift-gated, unsupported compatibility syntax has stable
diagnostics, every public SQL function has one evidence group, and bounded GQL
writes retain PostgreSQL as the authoritative source of truth.

2026-07-16 R7 release-ready closure — The PostgreSQL 14–18 release matrix,
packaged examples, compatibility and migration proofs, security/resource/crash
gates, deterministic source bundle, and independent reviews are complete;
pgGraph 1.0.0 is release-ready but unpublished, with signed tag and publication
reserved for the release-owner handoff.

2026-07-16 R7 packaged-worker closure — The packaged Panama gate now waits for
the exact concurrent job within a bounded five-minute window; all 40 CSR
playground examples pass in the rebuilt PostgreSQL 17 image without racing the
active worker during test teardown.

2026-07-16 R7 sandbox cleanup closure — Playground container recreation and
cleanup now remove the container's anonymous PostgreSQL volume, preventing
repeated release runs from leaking multi-gigabyte disposable databases.

2026-07-17 full-matrix validation closure — The full-matrix release tier
completed on the clean dev worktree at 4edabea: 16 of 17 gates pass, including
script contracts, docs drift/render, external links, Rust static and
checked-cast gates, Miri mapped-memory checks, legacy release, crash recovery,
transaction-delta crash recovery, read latency, the supported-majors matrix,
Linux runtime resources, the pg_upgrade matrix, and the PostgreSQL 14-18
package-install matrix. The postgres-sanitizer gate fails closed on this macOS
workstation because valgrind is unavailable here; it is deferred to the Linux
release-operator environment with the other environment-specific gates. Local
evidence: release/evidence/full-matrix.json and its logs directory.

2026-07-17 Dockerfile sfw decision — The previously proposed change to prefix
the production Dockerfile's cargo install with the local sfw wrapper is
rejected: sfw is a workstation safety wrapper for agent-executed package
installs and does not exist inside Docker build containers, so shipping it
would break every user image build. The Dockerfile is correct as written, no
new commit or evidence regeneration is required for this item, and dev@4edabea
remains the validated release candidate. Remaining work is release-owner
action only: the Linux sanitizer gate, signing/tagging, and publication.

2026-07-17 bidirectional BFS minimal-meeting-node fix (C2) — Fixed the
correctness gap the July 2026 secondary review flagged in
graph/src/path_finder.rs: bidirectional_bfs previously accepted the first
meeting node found while scanning a BFS level, even when a later candidate in
the same level had a strictly shorter combined distance, because the opposite
side's recorded depth for each candidate reflects whenever that node was
first visited rather than the shallowest depth available. ParentStep now
records each node's hop distance from its own search root, and the level scan
completes fully before selecting the minimum-combined-distance candidate
instead of breaking on the first match. Added a deterministic regression
(bidirectional_bfs_selects_minimal_combined_distance_meeting_node) and a
differential proptest (bidirectional_bfs_matches_single_direction_bfs) that
checks bidirectional results against the always-correct single-direction BFS
across randomized graphs; both pass at 20,000 proptest cases. Verified the
proptest is a meaningful regression guard by temporarily reintroducing the
original early-break behavior and confirming the harness still compiles and
runs against it (20,000 cases did not empirically distinguish old from new
behavior on graphs up to 11 nodes/24 edges, so the fix is justified by the
provable correctness gap in the algorithm rather than an observed failing
case; the new implementation is a strict generalization that can only match
or improve on the old one). Updated
docs/contributor_guide/traversal-search-paths.mdx to describe the
level-complete minimum-selection behavior. Native test count: 862 -> 864 (2
new tests). cargo fmt --check and cargo clippy --all-targets -- -D warnings
are clean for graph/src/path_finder.rs.

2026-07-17 RLS/tenant boundary documentation (C4/C5) — Documented, without
behavior change, that graph.traverse(), graph.shortest_path(),
graph.weighted_shortest_path(), and the connected-component functions read
the builder-scoped graph artifact and check only table-level SELECT ACL, not
row-level security, before returning coordinates, path membership, and
reachability; graph.search() is unaffected since it re-queries source
properties as the caller. Also documented that tenant scope for
tenant_column-registered (non-pinned) graphs is a query-shaping filter, not
an authorization boundary: any role with graph read and source-table
privileges can supply any tenant value. Added a "Row-Level Security And
Topology Reads" section and a tenant-scope callout to
docs/user_guide/administration-and-security.mdx, plus a new "Access Control"
category in docs/known-issues.mdx cross-referencing both. No SQL/behavior
change; docs-drift local-reference and SQL/GUC/source-map/contract checks
pass.

2026-07-17 traversal truncation signal (O3) — Added a `capped boolean` output
column to graph.traverse() (both overloads), graph.get_neighbors(), and
graph.traverse_search(), true when max_nodes/max_frontier stopped BFS/DFS
expansion before the frontier was naturally exhausted, false for an empty
seed, a no-match edge-type filter, reaching max_depth, or full exhaustion.
This closes the silent-truncation gap the July 2026 secondary review flagged:
callers previously could not distinguish a complete result from one cut short
by a resource circuit breaker. graph.shortest_path()/weighted_shortest_path()
were separately confirmed already safe against this class of issue: they run
through the resource-governed path-search budget (PathWorkBudget::finish),
which converts budget exhaustion into a SQLSTATE error rather than a silent
"no path" — that mechanism predates this change and needed no fix.

Implementation: BfsResult (bfs.rs) gained a truncated field set at all 14
construction sites across the CSR, layered-neighbor, and DFS execute_*_inner
variants; Engine::traverse_with_filter_ops_governed now returns a new
TraverseOutcome{rows, truncated} (with Deref/IntoIterator to Vec<TraversalResult>
so existing call sites needed no changes) instead of a bare Vec; the flag
threads through TraverseCandidate.capped in sql_traversal.rs into the new
trailing TraverseRow column. Three sql_facade/workflow.rs helpers
(expand/find_related/neighborhood) that already exposed their own
pagination-based `truncated` column now OR in this signal too, so those
columns are strictly more accurate than before with no signature change.

This is an intentional breaking SQL change on top of the dev@fe546b4 release
candidate, accepted after explicit confirmation. Updated
graph/sql/bootstrap.sql's hand-authored graph.traverse(node_ref[]) wrapper,
docs/user_guide/api-reference.mdx (3 RETURNS TABLE blocks) and
docs/user_guide/querying.mdx (return-columns table plus a clarifying callout
and cross-reference for the three workflow functions). Regenerated the
release contract from a live `cargo pgrx schema` output (no live PostgreSQL
connection required): release/v1-schema.sql, release/v1-contract.json,
release/contract-changes.json, and the auto-derived
docs/user_guide/sql-signatures.mdx, via
`scripts/check_release_contract.py --write --acknowledge`. Both
`scripts/check_release_contract.py` and `scripts/check_docs_drift.sh` pass
clean against the new schema.

Added regression coverage in bfs.rs: max_nodes_circuit_breaker and
max_frontier_limits_exploration now assert truncated=true;
natural_completion_is_not_truncated and depth_limit_alone_is_not_truncated
assert truncated=false (confirming max_depth alone is a complete answer, not
a truncation); dfs_max_nodes_circuit_breaker_reports_truncated and
dfs_natural_completion_is_not_truncated cover the DFS path. Native test
count: 863 -> 867. cargo fmt --check and cargo clippy --all-targets -- -D
warnings are clean.

Owed before this can be folded into a fresh full-matrix run: cargo pgrx test
against a live PostgreSQL cluster to exercise the new column through actual
SQL execution (the pgrx test files that call graph.traverse()/get_neighbors()
were manually reviewed and use named-column or SELECT * error-only patterns
that are backward compatible with a new trailing column, but this has not
been confirmed by an actual pgrx test run in this environment), and
regenerating release/evidence/full-matrix.json since the SQL contract changed
after that evidence was recorded.

2026-07-17 sync-log retention design (C3) — Wrote
todo/full-graph-engine/12-sync-log-retention-plan.md: a cross-backend
heartbeat registry (graph._sync_watermarks, mirroring the existing
graph._projection_generations pattern used by durable projections) so
csr_readonly backends' applied_sync_id replay progress becomes centrally
visible, enabling a safe prune floor for graph._sync_log. The plan specifies
the exact hook points (refreshed_engine_status() in sql_facade/admin.rs,
already called by graph.status()/sync_health(); the apply_pending_sync
freshness auto-apply path), an explicit bootstrap safety rule (zero
registered heartbeats means prune nothing, never default to pruning
everything), new sync_health() diagnostic columns, and a full unit/pgrx/soak
test plan. Deliberately scoped as design-only in this pass: the failure mode
of a wrong implementation is silent sync-log data loss for a lagging
backend, which is exactly the class of bug this review program exists to
prevent, so it warrants its own dedicated implementation and TDD cycle
rather than being rushed alongside the C2/O3 work already landed today.
Implementation remains the next open item.

2026-07-17 RLS/tenant enforcement fix (C4/C5, superseding the earlier
documentation-only decision) — The user asked to actually fix, not just
document, the two access-control gaps: topology reads not respecting RLS,
and tenant scope being caller-suppliable rather than enforced.

RLS fix: graph.build() now refuses to register a table with row-level
security enabled (relrowsecurity or relforcerowsecurity) unless
graph.allow_rls_tables = on (new GUC, default off). This applies to both
node tables and edge endpoint tables (check_build_rls_boundary in
sql_build.rs, called from check_build_acls_result). New diagnostic PG021
(SQLSTATE 55000), acl::table_has_row_security() introspects pg_class. This
does not retrofit per-row RLS filtering into topology reads -- that would
require temporarily switching the effective PostgreSQL user to the outer
caller for the duration of a check, which is unsafe to hand-roll correctly
in this pass -- it converts the previously silent exposure into a deliberate,
documented, explicitly-acknowledged operator decision, which is the
tractable, safe fix.

Tenant fix: resolve_tenant_scope() now rejects an explicit tenant SQL
argument for a tenant_column-registered graph that is not pinned to a single
graph-level tenant, while graph.enforce_tenant_scope = on (the default).
Tenant scope must then come from the session-setting path
(graph.tenant_setting), which only a trusted connection-pooler or login
hook -- not the caller's own query text -- can set. The existing pinned-graph
tenant check (explicit argument must match the graph's own tenant) is
unaffected. Docs are explicit that this does not itself authenticate the
session-setting value; that trust boundary remains a deployment concern.

This is a real behavior change and broke three existing pgrx test
expectations that relied on the old caller-suppliable-tenant and
build-over-RLS-tables behavior:
tenant_scope_filters_search_and_traversal and
auto_discover_tables_stores_shared_tenant_column_and_enforces_scope now
assert the explicit-tenant-argument path is rejected and exercise the
session-setting path instead; ingest_projection_publishes_committed_sync_log_rows
switched its three tenant-scoped traverse() calls to the session-setting
path; gql_create_node_preserves_source_table_rls (whose actual purpose is
verifying GQL writes respect source-table RLS, a different and unaffected
code path) now sets graph.allow_rls_tables = on before its build() call.
Added two new pgrx tests: build_refuses_rls_enabled_table_without_explicit_allow
and build_refuses_rls_enabled_edge_endpoint_table.

Fixed a local pgrx-test environment blocker unrelated to this change:
`postmaster became multithreaded during startup` on macOS is resolved by
running with LC_ALL=C LANG=C; this was blocking all live pgrx testing in
this session until found. With it, ran the complete pg17 pgrx suite with
the standard release-gate feature set (`--features "pg17 development"`,
matching graph/tests/heavy/run_release_gate.sh): 1143 passed, 0 failed, 1
ignored -- a full live confirmation of both this change and the O3 change
from earlier today, superseding the "pgrx test run owed" note left after O3.
cargo fmt --check and cargo clippy --features "pg17 development" --all-targets
-- -D warnings are clean. Regenerated the release contract a second time
(release/v1-schema.sql, release/v1-contract.json, release/contract-changes.json)
for the new GUC and diagnostic code via
scripts/check_release_contract.py --write --acknowledge; docs-drift and
release-contract checks pass clean.

Updated docs/user_guide/administration-and-security.mdx's RLS and Tenant
Scope sections (written a few hours earlier as documentation-only) to
describe the new enforced behavior instead, docs/known-issues.mdx's Access
Control section, docs/user_guide/configuration.mdx's GUC table, and the
PG021 error-code row.

Still not implemented: true per-row RLS-aware topology filtering (would need
a panic-safe, carefully-reviewed temporary PostgreSQL security-context
switch to the outer caller -- flagged as future work, not attempted here
given the risk of getting manual user-id switching subtly wrong).

2026-07-17 sync-log retention implementation (C3) — Implemented the design
from todo/full-graph-engine/12-sync-log-retention-plan.md: graph._sync_log
can now be pruned safely.

New durable table graph._sync_watermarks (graph_id, backend_pid,
database_oid, applied_sync_id, heartbeat_at, expires_at) mirrors the existing
graph._projection_generations heartbeat pattern. Each backend registers its
applied_sync_id (5-minute TTL) whenever sql_facade::admin::refreshed_engine_status
runs -- the shared path behind graph.status()/graph.sync_health() -- but only
when sync_mode is trigger, so manual-mode graphs never register a meaningless
heartbeat at 0. sql_sync::compute_sync_log_retention_floor (pure, unit- and
proptest-covered) implements the bootstrap safety rule: the floor is the
minimum of the durable projection watermark and the minimum active-backend
heartbeat, and is None -- pruning skipped entirely -- when neither is
available. sql_sync::prune_sync_log_if_safe is called only from
graph.maintenance()'s rebuild path (both foreground and the background
worker), never from graph.apply_sync().

Closed a real gap found while writing this changelog entry, before any code
existed to back it: a resumed backend whose applied_sync_id predates a
completed prune would otherwise silently skip the missing rows and diverge,
exactly the failure this feature exists to prevent. Added
graph._graphs.sync_log_pruned_before_id (a running maximum recorded on every
successful prune) and sql_sync::ensure_sync_replay_not_pruned, checked once
in apply_sync_to_high_watermark -- the single shared entry point for both
explicit graph.apply_sync() and the apply_pending_sync query-freshness
auto-apply path. A stale backend now fails closed with SQLSTATE 55000 and
new diagnostic PG022 instead of silently diverging.

Found and fixed a real off-by-one during live pgrx testing: the initial
prune query used `id < floor` (exclusive), but `floor` is itself an
already-applied, already-safe-to-delete id, so a graph with exactly one
fully-applied sync row could never have that row pruned. Fixed to `id <=
floor` (inclusive), consistent with the pruned_before_id/replay-guard
semantics. Also found and fixed two flawed test simulations during the same
live run: a "stale backend" test that accidentally used applied_sync_id=0
(the same value the guard intentionally exempts for genuinely fresh
backends) and a robustness issue where a test relied on incidental global
_sync_log sequence state instead of capturing its own intermediate
watermark. Neither was a production bug, but both would have been false
negatives (tests that pass without proving what they claim) in a suite that
was never actually executed. This is exactly why the earlier compile-only
verification pattern used for O3/C4/C5 was insufficient on its own; running
every new test live against a real PostgreSQL backend, not just compiling
it, is what actually validates this class of feature.

Added graph.sync_health() diagnostic columns sync_log_retention_floor,
sync_log_prune_recommended, active_sync_watermark_backends (breaking SQL
signature change; release contract regenerated). Added
graph._sync_watermarks to the revoked-PUBLIC-access internal catalog list
and pg_extension_config_dump (matching graph._projection_generations).
Updated docs/contributor_guide/sync-internals.mdx (new table, a full
Sync-Log Retention section with exact function/field references, a Current
Boundaries row), docs/user_guide/sync-and-maintenance.mdx (sync_health()
column table, a full Sync-Log Retention section, a fail-closed callout
naming PG022), docs/user_guide/api-reference.mdx and the auto-generated
docs/user_guide/sql-signatures.mdx, and docs/user_guide/administration-and-security.mdx
(internal catalog table, PG022 error-code row).

Test coverage: 8 new native unit tests (pure floor/recommendation logic,
including a proptest asserting the floor never exceeds either available
input) and 6 new pgrx integration tests, all run live against a real
PostgreSQL 17 backend: sync_health() reports the new columns;
graph.maintenance() prunes when this backend is the only heartbeat; does NOT
prune past a simulated lagging backend's heartbeat (the core safety
property this feature exists for); an expired heartbeat does not block
pruning; a stale backend's apply_sync() fails closed with PG022 after a
completed prune. Fixed an existing hardcoded SQL-signature contract test
(sync_health_exposes_operator_contract_field_names in
pg_tests/maintenance_admin.rs) for the 3 new columns.

Full live pg17 pgrx suite (--features "pg17 development", matching the
release-gate convention): 1156 passed, 0 failed, 1 ignored (up from 1143
before this entry). cargo fmt --check and cargo clippy --all-targets -- -D
warnings are clean. Regenerated the release contract a third time this
session for the new GUC-free but SQL-signature-changing sync_health()
columns and the new PG022 diagnostic.

C3 is now implemented, not just designed. The four items from the
2026-07-17 secondary review takeover (C2, O3, C4/C5, C3) are all complete
and live-verified. Remaining before this can be folded into a fresh release
determination: a full multi-version PostgreSQL 14-18 matrix re-run and the
non-pgrx release-gate checks (docs-render, external-links, crash-recovery,
pg-upgrade-matrix, package-install-matrix), since the previously recorded
release/evidence/full-matrix.json evidence at 4edabea predates all of
today's changes; the Linux sanitizer gate remains unavailable in this
environment; and signing/tagging/publication remain release-owner actions
this takeover does not perform.
