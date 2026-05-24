# Known Issues Completion Plan

Source of truth: `docs/known-issues.mdx`
Scope: `Next Update Scope`, `P0`, and `P1` rows only
Planning basis: `rust-planning`

This plan tracks completion of the higher-priority known issues without
duplicating the public issue register. `docs/known-issues.mdx` remains the
authoritative list of issue rows; this file records implementation order,
ownership, test gates, and regression risks for the tracked rows that are still
open.

## Planning Rules

- Keep the current single `graph` crate. The open issues do not justify a
  workspace split; use typed internal boundaries and helper modules instead.
- Treat SQL behavior, persistence format, sync semantics, SQLSTATEs, and
  catalog storage as compatibility surfaces.
- Prefer typed Rust boundaries over string contracts: enums for status and
  options, structs for key specs and table references, and helper types for SQL
  fragments.
- Values in SQL go through SPI parameters. Identifier fragments must come from
  PostgreSQL catalog validation, `regclass`, or a narrow quoted-identifier
  wrapper.
- Start each item with the smallest failing test or benchmark that proves the
  limitation, then implement against that test.
- Keep docs and code together in the same commit that changes behavior.
- Check private benchmark baselines before accepting traversal, persistence,
  sync, or build-path changes that can trade memory for latency or latency for
  memory.

## Validation Ladder

Use the narrowest check while iterating, then climb the ladder before closing a
milestone.

- Format and Rust tests: `cd graph && cargo fmt --check && cargo test --features pg17`
- PostgreSQL SQL tests: `cd graph && cargo pgrx test pg17`
- Docs drift: `scripts/check_docs_drift.sh`
- SQL/privilege heavy tests when SQL plumbing changes:
  `graph/tests/heavy/run_sqlstate_acl_boundary.sh` and
  `graph/tests/heavy/function_metadata_audit.sh`
- Background-job heavy tests when worker status or failure handling changes:
  `graph/tests/heavy/background_job_lock_regression.sh`
- Sync and durability heavy tests when persistence or replay changes:
  `graph/tests/heavy/concurrency_stress.sh`,
  `graph/tests/heavy/crash_recovery.sh`,
  `graph/tests/heavy/backup_restore_validate.sh`, and
  `graph/tests/heavy/pg_upgrade_validate.sh`
- Fuzz targets after parser or file-format changes:
  `graph/fuzz/fuzz_targets/load_graph_file.rs`,
  `graph/fuzz/fuzz_targets/structured_filter.rs`,
  `graph/fuzz/fuzz_targets/traverse_options.rs`, and
  `graph/fuzz/fuzz_targets/sync_properties.rs`

## Milestone 1 - SQL Contract

Goal: finish behavior that is hard to change after users depend on the SQL
surface.

### Traversal Uniqueness

Tracked row: `Traversal uniqueness`
Status: completed in `fix(traversal): honor global uniqueness`

Plan:

- Accepted values were audited at the SQL option parser: `node_global` and
  `node_per_root` are accepted; other values are rejected before traversal.
- Multi-start `graph.traverse()` and `graph.traverse_search()` now apply
  `node_global` after deterministic merged-result sorting and before final
  pagination.
- `node_per_root` remains the mode for returning the same reached node once per
  root.
- pgrx coverage now uses converging roots for both multi-start traversal and
  `traverse_search`.

Regression risk:

- `node_global` allocates a `HashSet` of reached node identities for merged
  multi-start results. This is proportional to the already-materialized result
  page candidates and avoids adding per-root traversal state.
- Default multi-start behavior now removes duplicate reached nodes before final
  pagination because the public default is `node_global`.

Completion criteria:

- Every accepted `uniqueness` value either changes result semantics in a tested
  way or fails with a stable SQLSTATE and documented message.
- Completed with pgrx coverage for `node_global`, `node_per_root`, and the
  default multi-start mode.

### Path Aggregation

Tracked row: `Path aggregation`
Status: completed in `perf(aggregation): share indexed path snapshots`

Plan:

- Existing cap-boundary SQL tests continue to prove `path_count_estimate()`
  exact/capped behavior.
- Path enumeration now records each unique indexed path as a shared
  `Rc<[u32]>` snapshot, so the seen set and output vector do not each own a
  full cloned path.
- `path_count_estimate()` now counts indexed paths directly instead of
  converting every path into table/id coordinates.
- `all_possible_paths` aggregation now hydrates from one coordinate map for the
  unique node indices used by the indexed paths.
- Existing path-count, cap, ordering, and duplicate-path aggregate semantics are
  preserved.

Regression risk:

- `Rc<[u32]>` adds reference-count bookkeeping per unique path, but removes an
  owned full-path clone between the seen set and output vector.
- Coordinate materialization is skipped for count-only calls, reducing memory
  near caps without changing path enumeration order.

Completion criteria:

- SQL results match current documented semantics at cap boundaries.
- Duplicate-path aggregate occurrence semantics are preserved.
- Avoidable full-path cloning and count-only coordinate materialization are
  removed.

### Filter Model

Tracked rows: `Filter model` and `Legacy numeric filters`
Status: completed in `fix(filters): retire raw traversal filters`

Plan:

- SQL facade entrypoints now construct traversal predicates only from
  structured typed filters.
- `TraverseRequest` no longer carries `filter_condition`, so public traversal,
  multi-start traversal, traversal search, and aggregation traversal cannot
  select the legacy raw filter parser.
- The legacy unsigned condition parser remains compiled only for tests,
  fuzzing, and development helpers.
- Signed typed filters continue to use `FilterOp::*I64` and never convert
  through `UnsignedFilterOp`.
- Regression coverage keeps the legacy parser rejecting signed numeric
  literals instead of wrapping, dropping, or treating them as unsigned values.

Regression risk:

- Public SQL behavior should not regress because raw `filter_condition` was
  already absent from `graph.traverse` signatures.
- Development-only callers of `Engine::traverse(..., filter_condition)` still
  use the legacy unsigned parser; production SQL code should call
  `traverse_with_filter_ops` through structured typed filters.

Completion criteria:

- Public filter behavior has one implementation source of truth.
- Signed values cannot be silently dropped, wrapped, or treated as unsigned
  misses.

## Milestone 2 - SQL Safety

Goal: make dynamic SQL trust boundaries explicit and auditable.

### Dynamic Table SQL

Tracked row: `Dynamic table SQL`
Status: completed in `fix(sql): bind table estimate lookups`

Plan:

- Build, search, hydration, sync trigger generation, and registration surfaces
  were audited for table and value boundaries.
- Dynamic `FROM` and trigger table references remain limited to registered
  `regclass` text, validated catalog lookups, or quoted identifiers.
- Repeated `pg_class.reltuples` estimate queries now resolve registered table
  names through `to_regclass($1)` and query `pg_class` by bound OID instead of
  interpolating table text into a `'...'::regclass` literal.
- The shared `estimated_table_rows()` helper is used by both build preflight
  memory estimation and `graph.estimate()`.
- pgrx regression coverage registers a table whose identifier contains a quote
  and verifies `graph.estimate()` returns its analyzed row estimate.

Regression risk:

- Estimate paths now perform an explicit `to_regclass($1)` lookup before the
  `pg_class` read. That is one extra catalog lookup per registered source/edge
  table in preflight estimate code, not in traversal hot paths.
- Removing literal interpolation fixes quoted identifiers and avoids accidental
  SQL syntax failures for registered table names containing quote characters.

Completion criteria:

- Dynamic build/query paths no longer interpolate data values in the audited
  table-estimate path.
- Remaining table and column SQL fragments are catalog/regclass-derived or
  quoted identifiers, with quoted-table estimate coverage.

### Discovery SQL

Tracked row: `Discovery SQL`
Status: completed in `fix(discovery): parameterize catalog queries`

Plan:

- Schema-wide table and foreign-key discovery now bind `schema_name` through
  SPI parameters.
- Text-column discovery now binds schema, table, and primary-key exclusion
  values; primary-key exclusions use a `text[]` parameter instead of generated
  `column_name != ...` fragments.
- Targeted table identifier discovery now binds the table OID instead of
  formatting it into the query.
- Targeted foreign-key discovery now binds the selected table set as an
  `int8[]` parameter and uses `ANY()` instead of generating an `IN (...)` list.
- Existing schema-wide and targeted discovery pgrx coverage exercises the
  parameterized paths for composite keys, junction tables, and FK edges.

Regression risk:

- Targeted FK discovery now casts relation OIDs to `bigint` for comparison
  against the bound `int8[]` table set. This is metadata-only discovery work,
  not traversal or build hot-path work.
- Parameterized arrays make the query shape stable for PostgreSQL planning and
  remove SQL string growth for larger targeted table sets.

Completion criteria:

- Discovery SQL uses parameters for schema values, table names, OIDs, PK
  exclusions, and targeted OID sets.
- Existing targeted and schema-wide metadata tests pass with the parameterized
  query shapes.

## Milestone 3 - Sync, Jobs, and Operator Semantics

Goal: remove avoidable synchronous broad work and make operational failures
durable.

### Truncate Handling

Tracked row: `Truncate handling`
Status: completed in `fix(sync): bound truncate by table membership`

Plan:

- Engine state now maintains a table-to-node `RoaringBitmap` membership index.
- Build ingestion, sync inserts, sync deletes, and persisted graph load keep
  table membership aligned with active nodes.
- Truncate replay clones the affected table membership bitmap and tombstones
  only those nodes, then removes their tenant membership and clears the table
  membership entry.
- A fallback rebuilds table membership if an older/manual test engine reaches
  truncate without membership populated.
- Unit coverage verifies insert, delete, truncate, unrelated-table preservation,
  and tenant-membership cleanup.

Regression risk:

- Table membership adds one compressed bitmap per table in backend memory.
  Artifact format is unchanged; membership is rebuilt after mmap load.
- Truncate latency now scales with affected table membership instead of total
  graph node count when membership is present.

Completion criteria:

- Truncate work is bounded by affected table membership.
- Existing mmap truncate replay and sync property tests continue to pass.

### Tenant Bitmap Mutation

Tracked row: `Tenant bitmap mutation`

Status: completed in `fix(sync): bound tenant bitmap updates`

Plan:

- Durable trigger rows already capture `old_row` and `new_row` payloads for
  update, delete, and primary-key replacement replay.
- Sync replay reads old and new tenant values from row images for tables with a
  registered `tenant_column`, falling back to properties for legacy insert-style
  payloads.
- `sync_update_tenant` and `sync_delete_tenant` mutate only the known old/new
  tenant bitmaps when old tenant data is available.
- Primary-key replacement uses bounded delete plus insert tenant maintenance
  instead of a broad tenant-bitmap scan.
- Legacy or manual sync rows without old tenant data retain the broad-removal
  compatibility fallback.

Regression risk:

- No sync-log schema change is required; the durable trigger row images already
  contain the tenant source data.
- Legacy rows without old tenant data still use the previous broad-removal
  fallback, preserving correctness at the cost of older-path work.
- Bounded update and delete reduce tenant-bitmap work for normal durable replay
  and do not add graph artifact fields.

Completion criteria:

- Unit coverage verifies old/new tenant extraction prefers row images over
  properties.
- Unit coverage verifies update and delete preserve unrelated tenant bitmaps
  when the old tenant is known.

### Internal Scheduler

Tracked row: `Internal scheduler`

Status: completed in `docs(sync): document external scheduler ownership`

Plan:

- Scheduler ownership remains external for this hardening pass.
- `graph.run_scheduled_maintenance()` is documented as the stable admin-only
  target for `pg_cron`, Kubernetes CronJobs, systemd timers, Docker
  initialization SQL, or app-side schedulers.
- The docs explicitly state pgGraph does not run an always-on internal scheduler
  or background loop.
- A future internal scheduler still requires a separate design covering
  PostgreSQL worker lifecycle, crash behavior, privilege boundaries, background
  worker slots, and operator controls.

Regression risk:

- No runtime regression; this is a documentation and operator-contract
  decision.
- External schedulers keep cadence, retry, and lifecycle behavior outside
  backend-local pgGraph state.

Completion criteria:

- User and contributor docs state scheduler ownership clearly enough that this
  is no longer listed as an active alpha limitation.

### Background Job Failures

Tracked row: `Background job failures`

Status: completed in `fix(jobs): persist worker failure status`

Plan:

- Build and maintenance job execution now return work errors without trying to
  update failure status inside the same transaction.
- Background-worker entrypoints run the work transaction first, then record
  failed status and error detail from a fresh worker transaction when the work
  transaction returns an error.
- Failure updates are idempotent for retries: queued, running, and already
  failed rows can receive failure detail, but completed rows are not overwritten.
- Operator docs describe `build_status()` and `maintenance_status()` as the
  source of truth for worker failure detail.

Regression risk:

- Separate transactions can briefly show an in-progress job before the failure
  status transaction commits.
- Failure recording now avoids overwriting completed jobs; a late failure after
  completion leaves the completed status intact.
- No hot-path traversal or build-memory regression; changes are limited to
  background-worker status updates and docs.

Completion criteria:

- Worker failures have durable final status and error detail after the work
  transaction returns an error.
- Regression tests verify failure detail is stored and late failure updates do
  not corrupt completed job history.

## Milestone 4 - Persistence

Goal: reduce artifact write memory pressure while preserving strict load
validation.

### Artifact Writes

Tracked row: `Artifact writes`

Status: completed in `fix(persistence): stream graph artifact writes`

Plan:

- `GraphArtifactWriter` owns header reservation, section offsets, fixed-width
  section alignment, body writes, length-prefixed metadata payloads, and
  incremental CRC calculation.
- Fixed graph sections stream directly to the temporary file, with the header
  and CRC backpatched before `sync_all()` and atomic rename.
- Primary-key offsets and primary-key bytes are written in separate passes so
  the writer no longer stages a combined primary-key byte buffer or full
  artifact body.
- Existing `FilterIndex` and edge type registry bincode metadata remain
  length-prefixed payloads, preserving the file format and loader contract.
- Temp-write, fsync, atomic rename, sync-checkpoint write, and immediate reload
  validation behavior are unchanged.

Regression risk:

- Streaming writes lower peak writer memory for large artifacts. `BufWriter`
  keeps small writes batched, but very small graph persistence may trade a small
  amount of CPU/seek overhead for lower peak memory.
- Backpatching offsets or CRC incorrectly can corrupt artifacts; existing
  roundtrip, section-layout, CRC, bounds, and corrupt-file tests cover the file
  contract.

Completion criteria:

- Writer no longer allocates one full artifact body buffer.
- Existing artifact load validation remains strict and non-panicking.
- Large roundtrip, section-layout, CRC, and corrupt-artifact tests pass.

### Persistence I/O

Tracked row: `Persistence I/O`

Status: completed in `fix(persistence): expose persistence progress phases`

Plan:

- Background build jobs report `building`, `persisting`, and
  `validating_persistence` phases through durable job status rows.
- Background maintenance jobs report `rebuilding`, `persisting`, and
  `validating_persistence` phases through durable job status rows.
- Artifact writes keep synchronous durability; `persisting` covers the
  temp-file write and `sync_all()`, and `validating_persistence` covers
  immediate mmap reload validation.
- Long fixed-section and primary-key write loops check PostgreSQL interrupts at
  bounded intervals when running inside PostgreSQL.
- Failure status remains durable through the separate worker failure transaction
  completed earlier in this milestone.

Regression risk:

- Background jobs add a small number of progress-row updates per build or
  maintenance run.
- Interrupt checks add bounded branch checks during large artifact section
  loops.
- Synchronous durability, temp-write, fsync, atomic rename, and reload
  validation semantics are unchanged.

Completion criteria:

- Operators can observe persistence write/fsync and reload-validation phases
  through SQL status rows.
- Durability semantics are unchanged and documented.
- Tests verify progress updates are visible for build and maintenance job rows.

## Milestone 5 - Query Performance Follow-Ups

Goal: reduce latency or memory only where measurement justifies the change.

Apply this workflow to each P1 row:

- Capture a pre-change benchmark or SQL fixture that shows the target cost.
- Implement the smallest typed change that addresses that cost.
- Compare memory, allocations, and latency against the private baseline.
- Keep the change only if result parity holds and common paths do not regress.

Tracked P1 rows and specific plans:

- `Traversal internals`: benchmark reusable BFS/DFS scratch buffers and sparse
  result metadata; keep dense traversal result vectors for cases where path
  reconstruction across many returned rows remains faster.
- `Source search recheck`: reduce per-candidate allocation while preserving
  SQL/Rust predicate parity.
- `Aggregation hydration`: use borrowed lookup shapes internally where pgrx row
  lifetimes allow it.

Completed P1 rows:

- `DFS neighbor expansion`: completed in
  `perf(traversal): stream dfs neighbor expansion`. DFS now pushes base and
  overlay neighbors in the same reverse stack order without allocating a full
  per-node neighbor vector. Overlay insertions still skip base duplicates and
  later duplicate overlay entries. Regression note: the change removes the
  eager vector allocation; it adds small reverse-slice scans for overlay
  duplicate checks only when inserted overlay edges exist. Pre/post timing was
  recorded in `/private/tmp/pggraph-dfs-neighbor-expansion-pre-benchmark.md`
  and `/private/tmp/pggraph-dfs-neighbor-expansion-post-benchmark.md`.
- `Connected components`: completed in
  `perf(components): reuse component size results`. Component computation now
  stores active-node counts by component once. `component_stats()` and
  `components()` reuse those sizes, while `component()` and `isolated_nodes()`
  filter, sort, and page node indices before constructing component rows for
  hydration. Regression note: component results retain one size map that used
  to be recomputed by multiple helpers; paged helper calls allocate fewer row
  objects before hydration. Pre/post timing was recorded in
  `/private/tmp/pggraph-connected-components-pre-benchmark.md` and
  `/private/tmp/pggraph-connected-components-post-benchmark.md`.
- `Reverse graph build`: completed in
  `perf(edges): build reverse csr linearly`. Reverse CSR construction now
  counts inbound degrees, prefix-sums offsets, and fills reverse arrays in a
  second pass over the forward CSR. Regression note: the change removes the
  intermediate reversed `RawEdge` vector and sort, replacing it with a cloned
  offset cursor and fixed-size output arrays. Pre/post timing was recorded in
  `/private/tmp/pggraph-reverse-graph-build-pre-benchmark.md` and
  `/private/tmp/pggraph-reverse-graph-build-post-benchmark.md`.
- `Hydration setup`: completed in
  `perf(hydration): resolve only requested table oids`. Batched hydration now
  groups the requested rows by table before reading registered table metadata,
  then resolves and stores only table OIDs that appear in the hydration page.
  Regression note: empty hydration pages skip catalog reads, and normal pages
  add one small needed-OID set while avoiding OID resolution for unrelated
  registered tables. Pre/post timing was recorded in
  `/private/tmp/pggraph-hydration-setup-pre-benchmark.md` and
  `/private/tmp/pggraph-hydration-setup-post-benchmark.md`.
- `Resolution delta lookups`: completed in
  `perf(resolution): index sync delta lookups`. Post-build sync inserts now use
  a keyed `(table_oid, pk_hash)` delta map before falling back to finalized or
  mmap-backed resolution indexes. Candidate node indexes are still verified
  against `NodeStore`, so tombstones and hash collisions keep the prior safety
  behavior. Regression note: lookup time now scales with matching hash
  candidates instead of total sync-delta size, at the cost of one hash-map entry
  and candidate vector storage per delta key. Pre/post timing was recorded in
  `/private/tmp/pggraph-resolution-delta-pre-benchmark.md` and
  `/private/tmp/pggraph-resolution-delta-post-benchmark.md`.
- `Path query allocations`: completed in
  `perf(paths): store parent metadata sparsely`. Unweighted shortest-path
  searches now keep only visited parent links and queue depth tuples instead of
  allocating full-graph parent, edge-type, and depth vectors. Weighted
  shortest-path searches still use a dense distance vector for fast Dijkstra
  relaxation, but parent edge type and edge weight metadata are sparse and only
  stored for relaxed nodes. Regression note: sparse maps reduce per-query
  memory on large graphs and shallow paths, with extra hash lookups during path
  reconstruction and parent updates compared with dense vector indexing.
  Pre/post timing was recorded in
  `/private/tmp/pggraph-traversal-allocations-pre-benchmark.md` and
  `/private/tmp/pggraph-traversal-allocations-post-benchmark.md`.
- `Edge overlays`: completed in
  `perf(traversal): key deleted overlays by source`. Traversal and aggregation
  overlay reduction now keeps delete overlays as
  `HashMap<u32, HashSet<(target, type_id)>>`, matching the insert overlay's
  source-keyed shape. Base-edge scans now skip delete lookups entirely when the
  current source has no deletes, and delete hits hash only `(target, type_id)`
  rather than a full `(source, target, type_id)` triple. Regression note: delete
  overlay construction adds a grouping pass after conflict resolution, but the
  traversal hot loop performs fewer and smaller hash lookups. Pre/post timing
  was recorded in `/private/tmp/pggraph-edge-overlays-pre-benchmark.md` and
  `/private/tmp/pggraph-edge-overlays-post-benchmark.md`.
- `Build-time SPI setup`: completed in
  `perf(build): cache table row estimates`. Build memory preflight and
  `graph.estimate()` now cache row estimates by registered table name across
  node and edge accounting. A source table that is both a node table and the
  source for multiple registered edges now performs one `pg_class.reltuples`
  estimate lookup for that accounting pass instead of repeating the same SPI
  helper call per use. Regression note: the cache retains one table-name/count
  entry per unique registered source table during estimation and removes
  repeated catalog estimate reads. Pre/post timing was recorded in
  `/private/tmp/pggraph-build-spi-setup-pre-benchmark.md` and
  `/private/tmp/pggraph-build-spi-setup-post-benchmark.md`.

Completion criteria:

- Each retained optimization has benchmark evidence and parity tests.
- Any memory-vs-latency tradeoff is recorded before the row is removed from
  `docs/known-issues.mdx`.

## Completion Definition

The high-priority plan is complete when:

- Every `Next Update Scope`, `P0`, and `P1` row in `docs/known-issues.mdx` is
  implemented, explicitly documented as intended behavior, or moved to a lower
  priority with rationale.
- SQL signatures, SQLSTATEs, examples, operator docs, and release notes match
  implemented behavior.
- The validation ladder has passed for each affected milestone.
- Regression measurements exist for changes that trade memory for speed or
  speed for memory.

## Refactoring Notes - Large Rust Source Files

These notes are planning guidance for future cleanup, not completion blockers
for the known-issues milestones above. Per the Rust planning rules, file size by
itself is not enough reason to introduce new crates or broad architecture
changes; prefer module-level splits where a file has mixed responsibilities or
where tests would become clearer through smaller public surfaces.

### Priority 1 - Split `graph/src/sql_facade/admin.rs`

This is the clearest large-file maintainability issue. The file currently mixes
several SQL-facing concerns:

- admin enablement and privilege checks
- status and sync health entrypoints
- scheduled maintenance decisions
- build and maintenance background-worker entrypoints
- table, edge, and filter registration APIs
- structured filter helper SQL exports
- remove, estimate, apply sync, vacuum, and maintenance APIs
- development-only test hooks

Recommended direction:

- Keep SQL extern wrappers thin and grouped by operator-facing surface.
- Move filter-constructor exports near the traversal/filter facade rather than
  admin operations.
- Move build and maintenance worker entrypoints near job orchestration code, or
  into a dedicated SQL job facade.
- Preserve the current SQL ABI and pgrx `include!` constraints while splitting;
  this is a module organization refactor, not a behavior change.

Completion criteria:

- Each resulting module has one primary SQL surface or orchestration concern.
- Existing `graph.*` SQL function names, overloads, result columns, and
  SQLSTATE behavior are unchanged.
- Existing pg tests continue to cover the moved entrypoints.

### Priority 2 - Extract Build Pipeline Phases From `graph/src/builder.rs`

`builder.rs` is conceptually cohesive, but `build_graph()` owns the whole
pipeline: memory preflight, node ingestion, filter indexing, edge resolution,
spool management, CSR construction, and final engine wiring. This is not urgent,
but it is a good candidate for smaller testable phases.

Recommended direction:

- Keep the builder in the same crate and module family; do not split a workspace
  or new crate just because the file is large.
- Extract phase-level helpers around node ingestion, filter population, edge
  spool resolution, and CSR loading.
- Keep the data ownership clear: the builder produces an `Engine`; stores remain
  owned by their existing modules.
- Add focused tests around extracted pure or near-pure helpers where possible;
  keep SPI-heavy behavior in pg tests.

Completion criteria:

- `build_graph()` reads as a phase coordinator.
- Extracted helpers have narrow inputs and avoid hidden global state except
  where pgrx/SPI requires it.
- Build behavior, memory checks, and generated graph contents are unchanged.

### Files Reviewed But Not Prioritized

The following large files are acceptable as-is unless future work changes their
responsibilities:

- `graph/src/persistence.rs`: cohesive `.pggraph` file format, validation,
  mmap loading, atomic writes, sync checkpoint I/O, and hardening tests.
- `graph/src/engine.rs`: borderline large, but mostly an engine orchestrator
  around owned stores, traversal, sync overlay state, and status calculation.
- `graph/src/edge_store.rs`: cohesive CSR storage and mmap safety boundary.
- `graph/src/bfs.rs`: cohesive traversal hot loop, DFS variant, neighbor
  iteration, path reconstruction, and traversal result formatting.
- `graph/src/filter_index.rs`: cohesive traversal filter index data structure.
- `graph/src/pg_tests/maintenance_admin.rs`: large integration-test file; split
  only if test navigation becomes a practical problem.
