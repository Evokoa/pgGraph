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

Plan:

- Split job execution from failure-status recording.
- Record failure state in a separate transaction or independent SPI context
  after the work transaction aborts.
- Add a test-only failing job path that proves final failure status survives an
  aborted worker transaction.

Regression risk:

- Separate transactions can briefly show an in-progress job before the failure
  status is recorded.
- Failure recording must be idempotent so retries do not corrupt job history.

Completion criteria:

- Worker failures have durable final status and error detail even when the work
  transaction aborts.

## Milestone 4 - Persistence

Goal: reduce artifact write memory pressure while preserving strict load
validation.

### Artifact Writes

Tracked row: `Artifact writes`

Plan:

- Introduce a `GraphArtifactWriter`-style boundary for header, section offsets,
  fixed-width arrays, length-prefixed payloads, padding, and CRC data.
- Stream sections directly to the temporary file where offsets can be reserved
  or backpatched safely.
- Preserve temp-write, fsync, atomic rename, immediate reload validation, and
  checksum behavior.
- Add file roundtrip, corrupt-file, crash-recovery, and memory measurement
  coverage before removing the limitation.

Regression risk:

- Streaming writes lower peak memory but can add syscalls and slower small
  graph persistence.
- Backpatching offsets incorrectly can corrupt artifacts, so loader fuzz and
  crash tests are mandatory.

Completion criteria:

- Peak writer memory is lower on a large fixture.
- Existing artifact load validation remains strict and non-panicking.

### Persistence I/O

Tracked row: `Persistence I/O`

Plan:

- Add progress phases for build, write, fsync, reload validation, and failure.
- Check PostgreSQL interrupts during long loops where pgrx exposes safe
  cancellation points.
- Keep synchronous durability unless a later design explicitly moves writes to
  a background workflow with crash consistency guarantees.
- Improve job status text or typed status phases so operators can identify where
  persistence time is spent.

Regression risk:

- More progress writes can add catalog churn during large operations.
- Cancellation checks improve operator control but add branch checks in hot
  loops.

Completion criteria:

- Operators can observe persistence phase and failure state through SQL status
  rows.
- Durability semantics are unchanged or explicitly documented with migration
  notes.

## Milestone 5 - Query Performance Follow-Ups

Goal: reduce latency or memory only where measurement justifies the change.

Apply this workflow to each P1 row:

- Capture a pre-change benchmark or SQL fixture that shows the target cost.
- Implement the smallest typed change that addresses that cost.
- Compare memory, allocations, and latency against the private baseline.
- Keep the change only if result parity holds and common paths do not regress.

Tracked P1 rows and specific plans:

- `Traversal internals`: benchmark sparse visited metadata and reusable scratch
  buffers; keep the current dense path for cases where it wins.
- `DFS neighbor expansion`: replace eager neighbor vectors with iterator-based
  expansion while preserving deterministic traversal order.
- `Edge overlays`: benchmark cache-friendly overlay sets or oriented overlays
  once overlay buffers grow past a measured threshold.
- `Connected components`: reuse component-size data and delay row materializing
  until after filtering, sorting, and pagination.
- `Reverse graph build`: add a linear two-pass reverse CSR builder only if
  build-time memory measurements justify the additional code path.
- `Build-time SPI setup`: batch metadata reads and reuse SPI contexts where
  query shape stays simple and error reporting remains clear.
- `Source search recheck`: reduce per-candidate allocation while preserving
  SQL/Rust predicate parity.
- `Hydration setup`: resolve only table OIDs needed by the requested page and
  reduce SPI context churn.
- `Aggregation hydration`: use borrowed lookup shapes internally where pgrx row
  lifetimes allow it.
- `Resolution delta lookups`: add an indexed delta map only if sync-delta
  growth benchmarks show material lookup cost.

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
