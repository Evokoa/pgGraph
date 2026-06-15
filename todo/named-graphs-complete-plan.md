# Complete Plan: Named Graphs, Tenant/User Graphs, Hot/Cold Residency, Automated Sync, and GQL Closure

## Purpose

This plan turns `todo/pre-planning.md` into a complete implementation sequence.
It assigns every feature, risk, and compatibility item from pre-planning to a
phase with acceptance criteria.

The plan is grounded in the current codebase:

- The extension is a single Rust 2021 pgrx crate in `graph/`, with module-level
  boundaries rather than a Cargo workspace.
- PostgreSQL source tables are the source of truth. pgGraph catalogs and
  artifacts are derived graph definitions and projections.
- Current catalog state lives in `graph/sql/bootstrap.sql`:
  `graph._registered_tables`, `graph._registered_edges`,
  `graph._registered_filter_columns`, `graph._build_jobs`,
  `graph._maintenance_jobs`, `graph._sync_log`,
  `graph._projection_generations`, and `graph._sync_buffer`.
- Current catalog access is concentrated in `graph/src/catalog/*`.
- Current SQL entrypoints are split under `graph/src/sql_facade/*`.
- Current build and persistence paths are concentrated in
  `graph/src/sql_build.rs`, `graph/src/builder.rs`, and
  `graph/src/persistence.rs`.
- Current runtime state is a single backend-local `ENGINE:
  RefCell<Engine>` in `graph/src/lib.rs`, with auto-load in
  `graph/src/sql_facade/runtime.rs`.
- Current sync and durable projection work is in `graph/src/sync.rs`,
  `graph/src/sql_sync.rs`, and `graph/src/projection/*`.
- Current GQL/openCypher frontends and planner are in `graph/src/gql/*`,
  `graph/src/cypher/*`, and `graph/src/query/*`.

## Architectural Rules

These rules apply to every phase.

1. PostgreSQL source tables remain authoritative. Graph writes must route through
   PostgreSQL DML before pgGraph projection state is updated.
2. Graph identity is a first-class catalog and runtime input. It must not be
   inferred from artifact paths, backend state, or session globals alone.
3. Default-graph compatibility is explicit. Existing single-graph SQL calls
   continue to target the default graph until a breaking 1.0 API boundary says
   otherwise.
4. Public SQL functions are thin facades. Catalog resolution, validation,
   planning, build, sync, and projection logic stay in their existing modules or
   new focused modules under the same single crate.
5. SQLSTATE behavior is part of the contract. Every phase that adds errors must
   add tests for the intended SQLSTATE and message shape.
6. ACL, RLS, tenant scope, and graph grants are checked before returning source
   data or mutating source tables.
7. Hot state is backend-local. Cross-backend sharing happens through PostgreSQL
   catalogs, durable files, and OS page cache backed mmap, not shared Rust heap.
8. Unsafe changes are avoided unless the persistence/mmap boundary requires
   them. Any unsafe change must stay behind safe wrappers with `// SAFETY:`
   comments and targeted tests.
9. TDD is the default workflow. Each phase starts with failing pgrx, unit,
   property, fuzz, heavy SQL, or benchmark tests that describe the target
   behavior.
10. No new dependency is added without first proving the standard library,
    pgrx, or existing dependency set is insufficient. If a dependency is added,
    installation must use `sfw` per `AGENTS.md`.
11. The crate stays a single pgrx crate unless compile-time, ownership, or
    feature-boundary pressure proves that a workspace split is necessary. New
    graph, job, quota, and policy code should be added as focused modules under
    the existing crate first.
12. Long-running work is durable and observable. Background workers and hosted
    schedulers must call the same SQL-visible job runner, record run history,
    and avoid hidden fire-and-forget execution.
13. Policy constants are single-sourced. Defaults for quotas, scheduler wake
    intervals, run batch sizes, retry limits, and residency limits belong in a
    focused policy/config module and corresponding GUC/catalog defaults, not as
    literals scattered through facades.
14. Retryable jobs are idempotent at the PostgreSQL boundary. Every job run
    records enough state to retry, diagnose, or skip already-completed work
    without duplicating source-table writes or projection publication.

## End State

The complete end state is:

- Multiple named graphs can coexist inside one PostgreSQL database.
- A graph can be global, user-owned, tenant-owned, workspace/project scoped, or
  a subgraph over selected registered tables and relationships.
- Every catalog row that describes graph definition, build state, sync state,
  projection generation state, policy, residency, and grants is graph-scoped.
- The default graph migration path preserves existing installs.
- Build, persistence, projection manifests, sync replay, jobs, status, reset,
  load, unload, and query execution all operate against an explicit graph.
- Tenant graphs support both shared-projection tenant-scoped views and dedicated
  physical tenant projections.
- Graph-level grants coexist with source-table ACL/RLS; graph access never
  bypasses PostgreSQL table privileges.
- Hot/warm/cold residency is explicit, observable, and enforced per backend.
- Automated sync policies keep selected graphs fresh without application code
  manually calling apply/maintenance functions.
- A generic, SQL-visible job framework backs automated sync policies and any
  later scheduled graph maintenance. Internal worker mode and hosted scheduler
  mode execute the same durable job records and write the same run history.
- Quotas limit graph count, physical materialization, jobs, sync lag, storage,
  build memory, loaded backend state, and compaction work by graph, tenant,
  owner, and namespace where applicable.
- Operational views expose graphs, jobs, job runs, sync health, storage usage,
  quota usage, and graph status with stable failure states and recommended
  recovery actions.
- Copy-on-write dirty-range projection segments and replacement chunks provide
  partial update/compaction without in-place CSR mutation.
- Relationship management APIs support add, rename, alter, remove, inspect, and
  metadata export.
- Direct node identity lookup uses the resolution index and is available from
  SQL helper APIs and lowered GQL predicates.
- The GQL surface reaches the full scope listed in pre-planning, including
  relationship hydration/properties, optional matches, bounded wildcard forms,
  PostgreSQL-first relationship writes, multi-row writes, transaction-created
  node indexes, dynamic label write semantics, and SQL/PGQ exposure.
- The openCypher compatibility surface is closed by a documented compatibility
  matrix, tests, and either implemented behavior or intentional permanent
  rejections for forms that cannot obey pgGraph's PostgreSQL-first model.
- GQL-driven PostgreSQL schema creation is closed by a safe source-table-first
  design or by permanent explicit rejection with tests and docs. It is not left
  as an unassigned item.

## Phase 0: Baseline Inventory and Guard Rails

Goal: make the current global-graph assumptions mechanically visible before
changing catalogs.

Implementation tasks:

- Add a short architecture note in this plan's companion implementation notes
  or existing contributor docs that records the current single-graph assumptions:
  global catalog keys, single `ENGINE`, `main.pggraph`, global build lock, global
  sync log, and unscoped projection generations.
- Add a planning closure note that records the Rust architecture decisions for
  this work:
  - stay in the existing single pgrx crate and use module boundaries first;
  - keep SQL facades thin and put graph/job/quota policy in catalog/runtime
    modules;
  - model worker execution as durable SQL jobs rather than hidden Rust tasks;
  - keep failure state typed and queryable through SQL status APIs.
- Add tests that pin current default-graph behavior before graph scoping:
  registration, build, auto-discover, query, reset, persistence load, sync apply,
  maintenance jobs, GQL read, GQL write, and docs/API drift checks.
- Add `rg`-based audit notes for all global assumptions:
  `read_catalog()`, `graph_file_path()`, `_sync_log`, `_projection_generations`,
  `_build_jobs`, `_maintenance_jobs`, `ENGINE.with`, `graph.reset()`, and
  `graph.auto_discover()`.
- Add `rg`-based audit notes for all policy and observability assumptions:
  build/sync batch sizes, edge-buffer limits, residency/load limits, artifact
  size reporting, job status values, error SQLSTATE mappings, and GUC defaults.
- Decide the internal graph identifier representation:
  PostgreSQL catalog type is `uuid`; Rust internal type is a small newtype or
  `String` wrapper that is passed explicitly through catalog/build/sync APIs.
- Add shared policy constants for default graph name, default namespace, allowed
  graph kinds, residency values, materialization values, and projection modes.
- Add shared policy constants for scheduler defaults, quota defaults, failure
  status values, and job status/progress values.

Acceptance criteria:

- Existing tests pass unchanged.
- New tests prove old single-graph SQL calls still work.
- The implementation checklist names every file that must be touched by
  graph-scoping work.
- Every pre-planning item is either assigned to a phase or explicitly rejected
  with a reason in this plan.

Suggested verification:

- `cargo fmt --check`
- `cargo test --features "pg17 development" query::`
- `cargo pgrx test --features "pg17 development" gql`
- Existing heavy SQL lifecycle scripts for GQL write paths.

## Phase 1: Graph Catalog Foundation

Goal: introduce first-class graph identity and default-graph compatibility.

Implementation tasks:

- Add `graph._graphs` to `graph/sql/bootstrap.sql`.
- Columns:
  - `graph_id UUID PRIMARY KEY`
  - `graph_name TEXT NOT NULL`
  - `owner_role OID NOT NULL`
  - `created_by OID NOT NULL`
  - `tenant TEXT`
  - `namespace TEXT`
  - `graph_kind TEXT NOT NULL`
  - `residency TEXT NOT NULL`
  - `materialization TEXT NOT NULL`
  - `projection_mode TEXT NOT NULL`
  - `created_at TIMESTAMPTZ NOT NULL DEFAULT now()`
  - `updated_at TIMESTAMPTZ NOT NULL DEFAULT now()`
- Add checks for `graph_kind`, `residency`, `materialization`, and
  `projection_mode`, reusing the existing projection mode vocabulary.
- Add uniqueness for `(tenant, owner_role, namespace, graph_name)` with a clear
  NULL policy. If PostgreSQL version support makes `NULLS NOT DISTINCT`
  awkward, add generated/coalesced key columns or expression indexes.
- Register `graph._graphs` with `pg_extension_config_dump`.
- Revoke writes and grant reads consistently with existing catalog tables.
- Insert or backfill one default graph during extension bootstrap/migration.
- Add catalog helpers under `graph/src/catalog/`:
  - resolve by `graph_name`, `tenant`, `namespace`, and current role;
  - resolve default graph;
  - create graph;
  - update graph metadata;
  - validate graph kind/residency/materialization/projection mode;
  - return stable graph metadata rows for status and map APIs.
- Add SQL facade functions:
  - `graph.create_graph(...)`
  - `graph.alter_graph(...)`
  - `graph.drop_graph(...)`
  - `graph.list_graphs(...)`
  - `graph.current_graph()`
  - `graph.set_current_graph(...)`
- Keep `graph.current_graph()` separate from engine load state. Current graph is
  selection; loaded graph is runtime residency.

Acceptance criteria:

- Existing installs get exactly one default graph.
- Creating two graphs with the same name is allowed only when tenant/owner or
  namespace makes them distinct.
- Old APIs can resolve the default graph without requiring users to call
  `create_graph`.
- Non-admin direct writes to graph catalogs remain blocked.

Tests:

- pgrx tests in a new `pg_tests/named_graphs.rs` or split files if the test file
  grows too large.
- SQLSTATE tests for duplicate graph, invalid kind/residency/materialization,
  missing graph, and unauthorized graph mutation.
- pg_dump/restore metadata preservation tests covering `_graphs`.

## Phase 2: Graph-Scoped Registration Catalogs

Goal: make table, edge, and filter registrations belong to a graph.

Implementation tasks:

- Add `graph_id UUID NOT NULL` to:
  - `graph._registered_tables`
  - `graph._registered_edges`
  - `graph._registered_filter_columns`
- Backfill existing rows with the default graph id.
- Replace primary/unique keys:
  - registered tables: `(graph_id, table_name)`
  - registered edges:
    `(graph_id, from_table, from_column, to_table, to_column, label)`
  - filter columns: `(graph_id, table_name, column_name)`
- Add foreign keys to `graph._graphs(graph_id)` with deletion behavior chosen
  deliberately:
  - default graph cannot be dropped while registrations exist;
  - explicit graph drop can cascade graph-owned catalog rows only after source
    table DML and artifact cleanup rules pass.
- Update `graph/src/catalog/read.rs`, `write.rs`, and `validate.rs` to accept
  graph identity.
- Keep compatibility wrappers:
  - `read_catalog()` delegates to `read_catalog(default_graph_id())`;
  - old tests can continue while new graph-aware paths are introduced.
- Update SQL facade registration functions:
  - legacy `graph.add_table(...)` and `graph.add_edge(...)` target default or
    selected graph;
  - new overloads accept `graph_name`, `tenant`, and `namespace` where pgrx
    overloading is practical;
  - when overloading would make SQL ambiguous, add explicit functions such as
    `graph.add_table_to_graph(...)`.
- Add graph-scoped `remove_table`, `remove_edge`, and filter-column mutation
  behavior.
- Ensure label uniqueness and ambiguous label detection are graph-local.

Acceptance criteria:

- Two graphs can register different table subsets and edge labels over the same
  source schema.
- Duplicate labels are rejected only inside the same graph.
- Removing an edge/table from one graph does not affect another graph.
- Default graph compatibility remains intact.

Tests:

- Same source table registered in two graphs with different property/filter
  columns.
- Same edge label in two graphs succeeds; duplicate in one graph fails.
- Graph-scoped discovery/build/query tests prove isolation.
- Catalog validation tests cover stale source tables per graph.

## Phase 3: Graph-Aware Discovery and Subgraph Definition

Goal: support named subgraphs and graph-aware auto-discovery.

Implementation tasks:

- Update `graph/src/discover.rs` and `graph/src/sql_facade/discovery.rs` to
  accept a target graph.
- Add SQL APIs:
  - `graph.auto_discover(graph_name := ..., schema_name := ..., build := false)`
  - `graph.auto_discover_tables(graph_name := ..., tables := ..., tenant_column := ..., build := true)`
  - `graph.preview_discover(...)`
  - `graph.preview_discover_tables(...)`
- Add graph-scoped conflict handling:
  - existing registration in same graph can update or skip by policy;
  - existing registration in another graph is not a conflict.
- Add generated edge label preview rows that users can rename before build.
- Add table/edge/filter subset subgraph semantics. Subgraph v1 is catalog
  subsets only: selected node tables, selected edge registrations, optional
  tenant scope, and optional materialization policy.
- Add permanent explicit rejection for arbitrary row-predicate subgraphs unless
  predicate semantics are implemented in this plan with build, sync, tenant,
  and write-path tests. The rejection or implementation is the phase contract.

Acceptance criteria:

- Discovery into `graph_name = 'customer_360'` does not mutate the default graph.
- Dry-run/preview returns deterministic rows and writes no catalog state.
- `build := true` builds only the target graph.
- Table-set discovery discovers only FK edges whose endpoints are both in the
  selected table set.

Tests:

- Preview writes no rows.
- Auto-discover selected tables into two graphs from the same schema.
- Discovery conflict policy is graph-local.
- Docs show default graph and named graph flows.

## Phase 4: Graph-Scoped Build, Locks, Jobs, and Status

Goal: build and report graph state per graph.

Implementation tasks:

- Add `graph_id UUID NOT NULL` to `graph._build_jobs` and
  `graph._maintenance_jobs`; backfill default graph id.
- Add graph-scoped indexes:
  - build jobs by `(graph_id, status, created_at)`
  - maintenance jobs by `(graph_id, status, created_at)`
- Update `graph/src/sql_jobs.rs`:
  - `WorkerMetadata` carries `graph_id` and graph display name;
  - create/read/update/run job functions accept graph identity;
  - background workers restore both database/user and graph context.
- Make build advisory locks graph-aware:
  - lock namespace remains pgGraph-owned;
  - lock object includes a stable hash or two-int split of graph id;
  - global operations that mutate shared schema may still take a global lock.
- Update `graph/src/sql_build.rs`:
  - build reads `read_catalog(graph_id)`;
  - `BuildExecutionResult` and job rows expose graph metadata;
  - `execute_maintenance_rebuild` and `execute_vacuum` are graph-scoped.
- Add SQL functions:
  - `graph.build(graph_name := ..., force_persist := ...)`
  - `graph.build_async(graph_name := ..., projection_mode := ...)`
  - `graph.build_status(graph_name := ...)`
  - `graph.maintenance(graph_name := ...)`
  - `graph.maintenance_status(graph_name := ...)`
  - `graph.vacuum(graph_name := ...)`
  - default-graph wrappers remain.
- Expand `graph.status()` or add `graph.graph_status(graph_name)` with:
  graph metadata, build state, sync state, projection state, artifact path,
  loaded/unloaded status, memory estimate, warnings, and recommendation flags.

Acceptance criteria:

- Concurrent builds for different graphs do not block each other unless they
  touch a deliberately global resource.
- Two background build jobs for the same graph serialize.
- Job rows are always attributable to one graph.
- Status APIs distinguish missing build, stale catalog, pending sync, invalid
  schema, cold residency, and artifact absence.

Tests:

- Build graph A and graph B with different table sets and verify counts.
- Background build metadata restores graph context.
- Job filtering by graph id/name works.
- Advisory lock tests prove same graph serializes and different graphs can run.

## Phase 5: Graph-Scoped Persistence and Projection Generations

Goal: make artifacts, manifests, checkpoints, and generation retention safe for
multiple graphs.

Implementation tasks:

- Replace global artifact path assumptions:
  - current path: `$PGDATA/<graph.data_dir>/main.pggraph`
  - new path: `$PGDATA/<graph.data_dir>/<graph_id>/main.pggraph`
- Add path helpers in `graph/src/persistence.rs`:
  - `graph_root_path(graph_id)`
  - `graph_file_path_for(graph_id)`
  - `sync_checkpoint_path_for(graph_id)`
  - `projection_manifest_root_for(graph_id)`
  - compatibility wrappers for default graph.
- Ensure paths are derived only from validated UUID graph ids, not arbitrary
  graph names.
- Add `graph_id UUID NOT NULL` to `graph._projection_generations`; backfill
  default graph id.
- Change projection-generation primary key to include `graph_id`.
- Update indexes for current and active generations to include `graph_id`.
- Update `graph/src/projection/*` to pass graph identity through manifest
  store, recovery, ingest, compaction, GC, repair, and status.
- Ensure generation cleanup never removes files for another graph.
- Add artifact cleanup for `drop_graph` and `reset_graph` with crash-safe,
  graph-root-only deletion.
- Keep artifact format version separate from graph identity. Graph identity
  lives in path/catalog metadata; the `.pggraph` binary format changes only if
  the file itself needs embedded graph metadata for validation.

Acceptance criteria:

- Persisting graph A and graph B produces separate roots.
- Loading graph A cannot accidentally mmap graph B's artifact.
- Projection generation GC is graph-local.
- Reset/drop graph removes only that graph's files and catalog rows.

Tests:

- Build and persist two graphs, verify distinct file roots.
- Corrupt graph A artifact does not affect graph B load/status.
- Projection generation retention and GC with active backends are graph-scoped.
- Crash/recovery tests preserve last valid generation per graph.

## Phase 6: Backend Runtime Selection and Engine Registry

Goal: replace the single active engine assumption with graph-aware runtime
selection while bounding backend memory use.

Implementation tasks:

- Introduce a runtime module that owns backend-local graph runtime state:
  - selected graph id;
  - selected graph metadata snapshot;
  - loaded engine map or single loaded engine slot;
  - per-engine last access and memory estimate.
- Choose the conservative implementation first: one active selected graph per
  backend, with explicit load/switch semantics and optional eviction. This keeps
  memory predictable.
- Add a bounded registry mode before phase completion:
  - configurable max loaded graphs per backend;
  - LRU or explicit unload policy;
  - clear error when loading would exceed the configured limit.
- Update all `ENGINE.with` call sites to go through graph runtime helpers:
  - `with_selected_engine`
  - `with_graph_engine(graph_id)`
  - `replace_graph_engine(graph_id, engine)`
  - `unload_graph_engine(graph_id)`
- Update auto-load to target selected graph and respect residency.
- Ensure transaction-local graph deltas are keyed by graph id or rejected when
  switching graph inside a transaction would be unsafe.
- Add SQL APIs:
  - `graph.select_graph(...)`
  - `graph.load_graph(...)`
  - `graph.unload_graph(...)`
  - `graph.loaded_graphs()`
- Keep default calls selecting/loading the default graph.

Acceptance criteria:

- Querying graph A after graph B does not reuse graph B's engine.
- Switching selected graph has deterministic semantics inside and outside
  transactions.
- Memory pressure is bounded by configuration.
- Loaded-graph observability reports graph id/name, residency, mmap status,
  node/edge count, projection mode, last access, and estimated memory.

Tests:

- Load A, query A, switch to B, query B, switch back to A.
- Cold graph does not auto-load unless explicit policy allows it.
- Transaction-local GQL writes cannot leak across graph selection.
- Registry capacity tests cover eviction/error behavior.

## Phase 7: Graph Ownership, Grants, Tenant Scope, and RLS Semantics

Goal: support user-owned and tenant-owned graphs without bypassing PostgreSQL
security.

Implementation tasks:

- Add graph grants catalog, for example `graph._graph_grants`:
  - `graph_id UUID NOT NULL`
  - `grantee OID NOT NULL`
  - privileges such as `read`, `write`, `build`, `admin`
  - timestamps and grantor role.
- Add SQL APIs:
  - `graph.grant_graph(...)`
  - `graph.revoke_graph(...)`
  - `graph.graph_privileges(...)`
  - `graph.transfer_graph_ownership(...)`
- Define privilege matrix:
  - owner/admin can register, build, reset, drop, grant, set policy;
  - builder can build/rebuild/maintenance;
  - writer can perform mapped graph writes if source table privileges allow;
  - reader can query if source table privileges allow.
- Update `graph/src/acl.rs` and SQL facades to check graph-level privileges
  before source-table ACL checks.
- Enforce source-table ACL/RLS on every query and write path:
  traversal, search, components, hydration, GQL read, GQL write, get_node, and
  get_neighbors.
- Define tenant graph modes:
  - shared projection with query-time tenant scope;
  - dedicated physical projection filtered by tenant;
  - logical graph definitions over shared materialized projections.
- Add graph catalog fields or policy table rows to record tenant mode and
  allowed tenant value(s).
- Add quota policy catalogs, for example `graph._graph_quotas` and a
  SQL-computable usage view:
  - scope: cluster default, tenant, owner role, namespace, graph;
  - dimensions: maximum named graphs, physical/materialized graphs, graph jobs,
    sync lag rows, sync lag age, artifact storage, build memory, loaded graphs
    per backend, concurrent jobs, and compaction work per run;
  - enforcement mode: hard error, warning/status only, or inherited default;
  - stable SQLSTATE and actionable message for hard-limit failures.
- Define quota ownership and override privileges:
  - schema admin can set cluster and tenant defaults;
  - graph owner can inspect effective quota and usage;
  - graph admin can request or apply per-graph overrides when authorized.
- Make tenant scope resolution graph-aware:
  - explicit tenant argument;
  - graph tenant;
  - session tenant setting;
  - conflict policy when these disagree.
- Add docs that state the RLS position precisely: pgGraph enforces source-table
  ACL/RLS at source data access boundaries, and graph artifacts must not be
  treated as a substitute for PostgreSQL row visibility.

Acceptance criteria:

- A role with graph read but no source table SELECT cannot read source row data.
- A role with source table SELECT but no graph read cannot query that graph.
- Tenant scope is applied consistently across traversal/search/GQL/helper APIs.
- Dedicated tenant projections never include nodes outside the graph tenant.
- Shared-projection tenant views filter topology by tenant membership.
- Quota checks fail before creating graph/job/artifact state that would exceed a
  hard limit.
- Effective quota and usage are visible without requiring direct catalog writes.

Tests:

- Owner/admin/reader/writer/build privilege matrix.
- RLS SELECT/INSERT/UPDATE/DELETE tests for GQL and helper APIs.
- Tenant conflict tests.
- Shared vs dedicated tenant projection behavior.
- Security tests for hydration disabled and coordinate-only responses.
- Quota inheritance, override, hard-limit, and warning-mode tests.
- SQLSTATE tests for graph count, job count, storage, memory, sync lag, and
  compaction-work quota failures.

## Phase 8: Hot/Warm/Cold Residency and Lifecycle Policy

Goal: make graph residency explicit, observable, and enforced per backend.

Implementation tasks:

- Finalize residency semantics:
  - `hot`: eligible for eager load or keep-loaded in the current backend;
  - `warm`: persisted and auto-loadable on demand;
  - `cold`: persisted but not auto-loaded unless explicitly loaded.
- Add `graph.set_graph_residency(...)`.
- Update auto-load logic in `graph/src/sql_facade/runtime.rs`:
  - selected hot/warm graph can auto-load according to GUC policy;
  - cold graph returns a clear error or unloaded status unless explicitly
    loaded.
- Add lifecycle policy GUCs:
  - max loaded graphs per backend;
  - max backend graph memory;
  - hot eager-load behavior;
  - idle unload threshold if supported.
- Wire residency and runtime quota checks to the quota policy from Phase 7:
  - backend load limits;
  - estimated memory limits;
  - cold graph explicit-load requirements;
  - clear status when a graph is eligible to load but blocked by quota.
- Add status rows for residency, loaded state, last access, artifact size,
  mmap-backed fields, projection generation, and memory estimate.
- Add unload behavior that is safe with active query/transaction state.
- Add backend startup/first-query hot-load behavior without claiming
  cluster-wide shared heap.

Acceptance criteria:

- `cold` prevents accidental auto-load.
- `load_graph` can explicitly load a cold graph when privileges allow.
- `unload_graph` frees backend-local engine state and does not delete artifacts.
- Residency changes do not mutate source tables or projection artifacts.

Tests:

- Hot, warm, and cold behavior in fresh backend sessions.
- Explicit load/unload with status assertions.
- Memory limit and capacity behavior.
- Runtime quota and quota-status behavior.
- Docs state backend-local semantics and OS page-cache sharing clearly.

## Phase 9: Graph-Scoped Sync Replay and Automated Sync Policies

Goal: make trigger sync, query freshness, maintenance, and durable projection
ingest operate per graph and support autonomous policy execution.

Implementation tasks:

- Decide sync log shape:
  - keep `_sync_log` source-table oriented and compute graph fanout from
    registrations; or
  - add graph-scoped applicability rows/materialized fanout.
- Add graph identity to per-graph sync checkpoints, applied watermarks, and
  projection ingest state.
- Update `pending_sync_rows`, `max_sync_log_id`, `read_sync_log_entries_after`,
  `apply_sync_internal`, and `apply_sync_to_high_watermark` to accept graph id.
- Ensure trigger installation remains per source table but records enough data
  for all dependent graphs.
- Add efficient graph fanout:
  - source table OID to graph ids index;
  - registered edge source table to graph ids index;
  - tenant-aware applicability filtering.
- Add generic job catalogs:
  - `graph._jobs`
    - `job_id UUID PRIMARY KEY`
    - `graph_id UUID NOT NULL`
    - `policy_kind TEXT NOT NULL`
    - `schedule_interval INTERVAL`
    - `enabled BOOLEAN NOT NULL`
    - `max_runtime INTERVAL`
    - `max_retries INTEGER`
    - `next_run_at TIMESTAMPTZ`
    - `last_run_at TIMESTAMPTZ`
    - `last_status TEXT`
    - `last_error TEXT`
    - `last_sqlstate TEXT`
    - policy payload columns or JSONB for bounded kind-specific settings
  - `graph._job_runs`
    - `run_id UUID PRIMARY KEY`
    - `job_id UUID NOT NULL`
    - `graph_id UUID NOT NULL`
    - `started_at`, `finished_at`
    - `status`, `sqlstate`, `error`
    - rows applied, segments published, chunks rewritten, maintenance job ids,
      retry count, worker identity, and hosted/internal execution mode.
- Add a sync policy layer backed by `graph._jobs` rather than a second scheduler:
  - `graph._sync_policies`
  - `policy_id UUID PRIMARY KEY`
  - `graph_id UUID NOT NULL`
  - `job_id UUID NOT NULL`
  - `schedule_interval INTERVAL NOT NULL`
  - `max_sync_lag_rows BIGINT`
  - `max_sync_lag INTERVAL`
  - `compact_after_segments INTEGER`
  - `enabled BOOLEAN NOT NULL`
  - `last_run_at`, `next_run_at`, `last_status`, `last_error`
- Add SQL APIs:
  - `graph.add_sync_policy(...)`
  - `graph.alter_sync_policy(...)`
  - `graph.drop_sync_policy(...)`
  - `graph.run_sync_policy(...)`
  - `graph.sync_policy_status(...)`
- Add generic job SQL APIs:
  - `graph.jobs(...)`
  - `graph.job_runs(...)`
  - `graph.job_stats(...)`
  - `graph.run_job(job_id)`
  - `graph.alter_job(job_id, ...)`
  - `graph.remove_job(job_id)`
- Implement a Timescale-style internal jobs/policies model:
  - SQL-visible durable policy rows;
  - worker execution path that calls existing maintenance decision logic;
  - no hidden untracked fire-and-forget tasks.
- Support two scheduler modes with the same execution primitive:
  - internal worker mode, where an optional pgGraph launcher wakes on a GUC
    interval, claims due jobs, and records `_job_runs`;
  - hosted mode, where `pg_cron`, provider schedulers, system cron, or
    application orchestration call `graph.run_job()` or a due-job runner.
- Add policy worker metadata carrying graph id, policy id, database, and user.
- Add GUCs for worker enablement, wake interval, maximum concurrent jobs, per-run
  work limits, retry delay, and hosted-mode safety.
- Enforce job and sync quotas before claiming or executing work.
- Make every job operation idempotent:
  - acquiring a per-graph/per-job advisory lock is required before work;
  - each run processes a bounded batch;
  - retry sees previously applied watermarks/manifests and skips completed work.
- Ensure query freshness can apply pending sync for the selected graph only.

Acceptance criteria:

- A source table change can be applied to every dependent graph without
  applying to unrelated graphs.
- Sync policy keeps a graph fresh without application code calling
  `graph.apply_sync()`.
- Policy execution is visible, retryable, and inspectable.
- Failed policy runs do not corrupt graph runtime or projection state.
- Internal worker mode and hosted mode produce equivalent catalog state and run
  history for the same due job.
- Re-running a completed or partially completed job is idempotent.
- Job status distinguishes queued, running, succeeded, retryable failure,
  permanent failure, disabled, quota blocked, and lock skipped.

Tests:

- One source table used by multiple graphs with different table/edge subsets.
- Pending row counts and high-water marks per graph.
- Policy run success/failure/status.
- Generic job creation, alteration, deletion, run history, and statistics tests.
- Hosted-mode runner tests that do not require an internal background worker.
- Idempotent retry tests for apply-sync, projection ingest, compaction, and
  maintenance enqueue steps.
- Quota-blocked job tests.
- Concurrent writes plus policy apply.
- Heavy SQL sync lifecycle scripts updated for named graphs.

## Phase 10: Dirty-Range Durable Projection and Copy-on-Write Compaction

Goal: support partial graph updates through manifest-level copy-on-write, not
in-place mutation.

Implementation tasks:

- Extend sync replay to compute dirty source-node ranges from sync rows.
- Teach projection ingestion to emit dirty-range segments instead of broad
  `0..u32::MAX` segments when the source node range is known.
- Add segment fanout/read-amplification metrics per graph.
- Implement replacement base chunk generation:
  - detect dirty source-node ranges;
  - materialize final neighbors for those ranges by merging base plus durable
    deltas;
  - write new chunk files for only those ranges;
  - publish a new manifest generation that points to replacement chunks.
- Ensure older generations remain readable until no active backend references
  them.
- Add graph-aware GC thresholds and policy settings.
- Enforce compaction and storage quotas:
  - maximum bytes written per run;
  - maximum obsolete bytes before GC recommendation;
  - maximum segments/chunks compacted per run;
  - quota-blocked status when publication would exceed effective limits.
- Update storage observability:
  - artifact bytes;
  - manifest generation bytes;
  - durable segment bytes;
  - replacement chunk bytes;
  - obsolete bytes retained for active backends.
- Add crash recovery for partially written chunks/manifests.

Acceptance criteria:

- No code path overwrites bytes inside an existing mmap'd base CSR artifact.
- Dirty-range ingestion reduces segment scope when source-node ranges are known.
- Replacement chunk publication is atomic at the manifest level.
- Active old generations are retained until heartbeat/expiry rules allow GC.
- Storage and compaction quota states are visible and deterministic.

Tests:

- Unit tests for dirty-range normalization.
- pgrx tests for segment publication and replacement chunk visibility.
- Crash/recovery tests for temp files and invalid manifests.
- Storage usage and quota enforcement tests.
- Benchmarks comparing base-only, broad segment, dirty-range segment, and
  replacement chunk scenarios.

## Phase 11: Relationship Management and Graph Map Export

Goal: make relationship definitions inspectable, editable, and exportable per
graph.

Implementation tasks:

- Add SQL APIs:
  - `graph.rename_edge(graph_name := ..., old_label := ..., new_label := ...)`
  - `graph.alter_edge(graph_name := ..., label := ..., bidirectional := ..., weight_column := ..., label_column := ...)`
  - `graph.remove_edge(graph_name := ..., label := ...)`
  - `graph.list_edges(graph_name := ...)`
  - `graph.graph_map(graph_name := ...)`
  - `graph.graph_map(graph_name := ..., format := 'json')`
- Ensure rename/alter operations are graph-scoped and validate endpoint/table
  metadata.
- Record rebuild-required or sync-required state when relationship definitions
  change.
- Return graph-map metadata:
  - graph id/name/tenant/namespace/kind/residency/materialization;
  - node tables and primary keys;
  - relationship mappings;
  - static labels and dynamic label columns;
  - weight columns;
  - tenant columns;
  - filter columns;
  - build/sync/projection status;
  - warnings for stale builds, missing artifacts, ambiguous labels, and rebuild
    requirements.
- Keep graph map as metadata only. It does not dump all nodes or relationships.

Acceptance criteria:

- Relationship label rename does not require remove/re-add.
- Altering a relationship updates the mapping and clearly marks rebuild/sync
  impact.
- Graph map output is deterministic and graph-scoped.
- Ambiguous labels are reported with actionable metadata.

Tests:

- Rename, alter, remove, and list edge flows.
- Graph map JSON snapshot tests.
- Rebuild-required status after relationship edits.
- ACL tests for relationship management APIs.

## Phase 12: Direct Node Identity Lookup and Neighbor Helpers

Goal: make known-business-id lookups fast and explicit.

Implementation tasks:

- Add SQL APIs:
  - `graph.get_node(graph_name := ..., label := ..., id := ..., hydrate := true)`
  - `graph.get_neighbors(graph_name := ..., label := ..., id := ..., direction := 'any', edge_types := NULL, tenant := NULL, hydrate := true)`
- Resolve `(graph_name, label, id)` to `(graph_id, table_oid, primary_key)`.
- Use `Engine` resolution index to find `node_idx` directly.
- Support composite primary-key string encoding exactly as registration/build
  paths encode it.
- Apply graph grants, source-table ACL, RLS/hydration policy, and tenant scope.
- Define not-found policy:
  - coordinate lookup returns zero rows for table-style APIs;
  - scalar helper returns NULL if added;
  - invalid graph/label/id encoding returns typed SQLSTATE errors.
- Add hydrate false coordinate-only responses.
- Lower supported GQL identity predicates into the same path:
  - `MATCH (u:users) WHERE id(u) = $id RETURN u`
  - property form that is exactly the registered primary-key expression when
    unambiguous.
- Add explain output proving direct lookup rather than node scan.

Acceptance criteria:

- Direct lookup does not scan all nodes for known graph/table/id.
- GQL identity predicate explain output shows the identity lookup plan.
- Not-found and unauthorized cases are deterministic.
- Tenant scope is honored before neighbor expansion.

Tests:

- Simple PK and composite PK lookup.
- Hydrated and hydrate false.
- ACL/RLS/tenant not-found distinctions.
- GQL explain and execution tests.
- Microbench or criterion scenario showing no regression in resolution lookup.

## Phase 13: GQL Relationship Hydration, Properties, Optional Matches, and Bounded Wildcards

Goal: complete the read-side GQL scope from pre-planning.

Implementation tasks:

- Add relationship source-row hydration for registered edge-row relationships.
- Project relationship properties from registered edge table columns.
- Define relationship coordinate JSON shape for hydrate false.
- Add node-only optional matches.
- Add optional path-variable projection semantics.
- Broaden wildcard path support while staying bounded:
  - `ORDER BY`
  - `WITH`
  - `RETURN DISTINCT`
  - aggregates
  - optional matches
  - broader wildcard property predicates
- Keep planner-hostile unbounded forms out of execution by converting them to
  explicit bounded plans or permanent typed rejections with SQLSTATE tests.
- Update `graph/src/query/explain.rs` to show relationship hydration/property
  plans.
- Add docs and compatibility matrix rows for every accepted and rejected form.

Acceptance criteria:

- Relationship variables can return useful properties for registered edge-row
  mappings.
- Optional node/path reads behave predictably with missing matches.
- Bounded wildcard combinations are covered by planner tests and row-count
  limits.
- Unbounded forms have permanent documented rejection behavior.

Tests:

- GQL pgrx tests for relationship property projection.
- Query planner unit tests for optional and wildcard forms.
- Snapshot/explain tests.
- Fuzz seeds for new GQL grammar shapes.
- SQLSTATE tests for permanent rejections.

## Phase 14: PostgreSQL-First GQL Writes Beyond Single-Row Node Mutation

Goal: complete write-side GQL scope while preserving PostgreSQL as source of
truth.

Implementation tasks:

- Add relationship creation through PostgreSQL-first DML for registered
  edge-row mappings.
- Add multi-row writes with strict transaction behavior:
  - source DML in one PostgreSQL transaction;
  - rollback clears transaction-local deltas;
  - commit creates durable sync/projection work;
  - write predicates are rechecked under row locks.
- Add temporary graph node indexes for transaction-created nodes so new nodes
  can participate in traversal, joins, path reads, relationship creation, and
  MERGE in the same transaction.
- Add dynamic label write semantics only where they map cleanly to registered
  table or label-column metadata.
- Reject unmapped graph writes permanently with mapping guidance and tests.
- Add GQL-driven PostgreSQL schema creation closure:
  - if implemented, it must create ordinary PostgreSQL source tables first,
    register mappings second, and then build/sync projections;
  - if not implemented, add a permanent explicit rejection with docs explaining
    that pgGraph does not create durable graph-only schema state.
- Expand heavy SQL lifecycle scripts:
  - relationship create rollback/commit;
  - multi-row write rollback/commit;
  - transaction-created node traversal;
  - tenant/RLS/ACL race cases.

Acceptance criteria:

- Every successful graph write mutates PostgreSQL source tables before graph
  projection state.
- No graph-only durable write state exists.
- Multi-row writes are atomic.
- Dynamic label writes are accepted only for registered mappings.
- Unmapped writes and schema-creation decisions are closed by implementation or
  permanent rejection, not left open.

Tests:

- pgrx GQL write tests.
- Heavy race scripts for concurrent MERGE/create/relationship writes.
- SQLSTATE boundary tests.
- Source-table row count and projection visibility assertions after rollback
  and commit.

## Phase 15: SQL/PGQ and openCypher Compatibility Closure

Goal: close language compatibility work with tests, docs, and stable policy.

Implementation tasks:

- Public SQL/PGQ exposure:
  - expose only after PostgreSQL graph-pattern hooks are stable enough for the
    supported PostgreSQL versions;
  - until then, add permanent documented rejection for public SQL/PGQ entrypoints
    while keeping internal adapter tests.
- openCypher compatibility:
  - create a compatibility matrix for every grammar and semantic category the
    project claims, rejects, or aliases through GQL planner behavior;
  - implement accepted broad openCypher forms that map safely to existing GQL
    and PostgreSQL-first semantics;
  - permanently reject forms that require graph-only state, unbounded planner
    behavior, or source-table bypass.
- Ensure `graph.cypher()` remains honest: either implemented compatibility rows
  are green or unsupported rows have deterministic SQLSTATE errors.
- Add fuzz seeds for accepted/rejected openCypher forms.
- Update roadmap, limitations, API reference, and release notes.

Acceptance criteria:

- There is no vague broad-Cypher placeholder statement in docs.
- Every language form is accepted with tests or rejected with tests.
- SQL/PGQ public exposure has an explicit stable support decision.
- GQL remains the primary standards-oriented surface unless docs state a
  changed priority.

Tests:

- Parser, semantic binding, planner, pgrx execution, and SQLSTATE tests.
- Fuzz corpus additions for GQL and Cypher.
- Docs drift checks for API/reference claims.

## Phase 16: Documentation, Migration, Operational Tooling, and Release Gates

Goal: finish the user/operator experience for the complete feature set.

Implementation tasks:

- Update user docs:
  - schema registration;
  - build and persistence;
  - sync and maintenance;
  - querying;
  - administration and security;
  - API reference;
  - troubleshooting;
  - limitations and fit.
- Update contributor docs:
  - architecture;
  - repository map;
  - engine internals;
  - persistence format;
  - sync internals;
  - SQL tests;
  - testing/release;
  - safety/security.
- Add migration notes:
  - global catalog to default graph;
  - artifact path migration;
  - projection generation migration;
  - rollback/repair guidance for failed migration.
- Add admin SQL examples:
  - create graph;
  - register graph;
  - build async;
  - add sync policy;
  - inspect and run jobs;
  - set residency;
  - grant graph;
  - set and inspect quotas;
  - inspect storage usage;
  - graph map export;
  - direct node lookup.
- Add an operational failure matrix covering:
  - sync falls behind;
  - artifacts are missing;
  - artifact validation fails;
  - disk is full;
  - compaction fails;
  - projection ingest fails;
  - a job repeatedly errors;
  - a graph exceeds quota;
  - background workers are unavailable;
  - schema/catalog drift requires rebuild.
- For each failure matrix row, document:
  - stable status value;
  - SQLSTATE where appropriate;
  - visible location in status/job history;
  - recommended recovery action;
  - whether reads fail, serve last valid generation, or require explicit
    operator action.
- Update scripts:
  - package build validation;
  - docs drift checks;
  - SQL API drift checks;
  - artifact inspection script to accept graph id/name.
- Add release gates:
  - named graph tests;
  - tenant/RLS tests;
  - persistence and projection tests;
  - automated sync tests;
  - job framework and hosted scheduler tests;
  - quota enforcement tests;
  - storage usage and failure-state tests;
  - GQL write/read closure tests;
  - benchmark gates for single default graph no-regression and multi-graph
    overhead.

Acceptance criteria:

- A user can follow docs from `CREATE EXTENSION` to two named graphs with
  different residency and sync policies.
- Existing single-graph quickstart still works.
- API reference includes every new function and default compatibility wrapper.
- Jobs, quotas, hosted scheduling, storage usage, and failure behavior are
  documented as first-class operational surfaces.
- Release checklist includes migration and rollback verification.

Suggested final verification:

- `git diff --check`
- `cargo fmt --check`
- `cargo test --features "pg17 development" query::`
- `cargo pgrx test --features "pg17 development" gql`
- `cargo pgrx test --features "pg17 development" named_graphs`
- Heavy SQL scripts for install, sync, GQL writes, ACL/RLS, and concurrency.
- Heavy SQL scripts for jobs, hosted scheduling, quota enforcement, storage
  usage, and failure recovery.
- `cargo bench --features pg17 --bench bfs_bench` with named-graph benchmark
  scenarios.
- Fuzz targets for projection manifest/segment, GQL parser, and Cypher parser
  with updated corpora.

## Planning Closure: Pre-Planning Item Assignment

This section closes the loose items from `todo/pre-planning.md` so the
implementation sequence has one source of truth.

| Pre-planning item | Assigned phase | Planning decision |
|---|---:|---|
| First-class graph identity | 1 | `graph._graphs` is the identity root. Rust uses an explicit graph id type passed through catalog, runtime, build, sync, and projection APIs. |
| Named subgraphs and graph-aware discovery | 2-3 | Subgraph v1 is graph-scoped table/edge/filter registrations plus preview/dry-run discovery. Arbitrary row predicates are rejected until separately designed. |
| Per-graph build, artifacts, jobs, and status | 4-5 | Build jobs, maintenance jobs, artifact paths, and projection generations all carry graph id. |
| Backend engine selection and hot/cold controls | 6, 8 | Start with one selected graph plus bounded loaded-engine registry; residency controls auto-load eligibility. |
| Tenant/user ownership and RLS stance | 7 | Graph grants are necessary but never sufficient; source-table ACL/RLS and tenant scope remain authoritative at source-data boundaries. |
| Quotas | 7-10, 16 | Quota policy starts in Phase 7 and is enforced by graph creation, runtime loading, jobs, sync lag, projection storage, and compaction work. Docs and release gates close in Phase 16. |
| Automated sync policies | 9 | Sync policies are policy rows backed by the generic durable job framework. |
| Generic jobs, job runs, job stats | 9 | `_jobs` and `_job_runs` are the scheduler source of truth; `graph.jobs()`, `graph.job_runs()`, `graph.job_stats()`, `graph.run_job()`, `graph.alter_job()`, and `graph.remove_job()` are public operational APIs. |
| Hosted scheduler mode | 9, 16 | Hosted mode calls the same job runner as internal worker mode. Documentation and release gates prove both modes. |
| Dirty-range deltas and replacement chunks | 10 | Copy-on-write manifests and replacement chunks are the only supported partial rewrite path; in-place mmap mutation is rejected. |
| Relationship management and graph map export | 11 | Relationship rename/alter/remove/list and deterministic metadata export are graph-scoped APIs. |
| Direct node identity lookup | 12 | SQL helpers and supported GQL predicates use the resolution index rather than graph scans. |
| GQL read/write backlog | 13-14 | Read-side hydration/optional/wildcard work and PostgreSQL-first write work are separate phases with SQLSTATE tests. |
| SQL/PGQ and openCypher closure | 15 | Every language form is implemented or permanently rejected with docs and tests. |
| Observability views | 4, 7-10, 16 | `graph.graphs()`, `graph.graph_status()`, `graph.sync_health()`, `graph.jobs()`, `graph.job_stats()`, `graph.job_runs()`, `graph.storage_usage()`, and quota usage APIs are required before completion. |
| Failure behavior | 4, 5, 9, 10, 16 | Each phase that introduces a failure mode owns status values and SQLSTATE tests; Phase 16 publishes the complete operator matrix. |
| Testing matrix | Every phase | TDD remains required: failing unit, pgrx, heavy SQL, property, fuzz, benchmark, or docs-drift tests must precede implementation for each phase. |

## Cross-Phase Data Model Summary

Minimum catalog additions and changes:

- `graph._graphs`: first-class graph metadata and ownership root.
- `graph._graph_grants`: graph-level privileges.
- `graph._sync_policies`: automated graph sync/maintenance policy.
- `graph._jobs`: generic durable scheduled and manually runnable graph jobs.
- `graph._job_runs`: durable job execution history, retry state, run metrics,
  errors, and hosted/internal execution mode.
- `graph._graph_quotas` or equivalent quota policy catalog: inherited hard and
  warning limits for graph, tenant, owner, namespace, and cluster scopes.
- `graph_id` added to:
  - `_registered_tables`
  - `_registered_edges`
  - `_registered_filter_columns`
  - `_build_jobs`
  - `_maintenance_jobs`
  - `_projection_generations`
- Per-graph sync checkpoint state, either as graph-scoped columns/tables or as
  graph-scoped durable checkpoint files.
- Optional applicability/fanout indexes for source table OID to graph ids.
- SQL-computable usage views for graph counts, job counts, loaded runtime state,
  artifact storage, obsolete projection bytes, sync lag, and compaction debt.

Minimum Rust API direction:

- Introduce an explicit graph identity type in catalog/runtime modules.
- Prefer graph-aware functions first, with default-graph wrappers second.
- Replace direct `ENGINE.with` usage outside the runtime module with graph
  runtime helper functions.
- Keep pgrx SQL facade modules thin and continue using existing internal
  modules for build, sync, query, persistence, and projection work.
- Add focused modules for graph policy, jobs, quotas, and failure/status
  translation rather than expanding SQL facades with business logic.
- Keep scheduler work synchronous from PostgreSQL's perspective: internal
  workers and hosted callers invoke durable SQL-visible execution functions that
  claim bounded work and return typed status.

## Test Strategy by Layer

Unit tests:

- graph id parsing/formatting;
- catalog key construction;
- graph map JSON construction;
- effective quota resolution and usage calculations;
- job claim/run state transitions;
- failure-state to SQLSTATE/status translation;
- dirty-range normalization;
- query planner identity lookup and optional/wildcard lowering;
- Cypher/GQL accepted/rejected grammar forms.

pgrx tests:

- catalog migration and default compatibility;
- graph-scoped registration/build/query;
- graph grants and source-table ACL/RLS;
- quota inheritance and enforcement;
- hot/warm/cold runtime behavior;
- generic job framework, hosted runner, and sync policy behavior;
- storage usage and failure-state reporting;
- direct lookup helpers;
- GQL read/write behavior.

Heavy SQL tests:

- extension install/upgrade migration;
- concurrent builds and jobs;
- sync policy under writes;
- job retry, lock skipping, hosted mode, and repeated failure behavior;
- quota enforcement across graph/job/storage/runtime dimensions;
- GQL write rollback/commit/race behavior;
- tenant isolation and RLS.

Property/fuzz tests:

- graph map JSON stability where useful;
- quota policy inheritance and effective-limit invariants;
- job state-machine invariants;
- projection manifest and segment parsing;
- GQL and Cypher parser corpora;
- identity/composite-key encoding invariants.

Benchmarks:

- default graph no-regression;
- graph selection overhead;
- two loaded graphs in one backend;
- job runner overhead and scheduler wake cost;
- direct identity lookup vs node scan;
- dirty-range projection segment and replacement chunk reads.

## Completion Definition

The plan is complete only when:

- Every phase's acceptance criteria pass.
- Existing single-graph workflows still work through default graph resolution.
- Multi-graph workflows are documented and tested.
- Security behavior is tested at graph grant, source ACL, RLS, tenant, and GQL
  write boundaries.
- Quota, job, hosted scheduling, storage usage, and failure-state behavior are
  documented, tested, and exposed through stable SQL APIs.
- No catalog, artifact, sync, projection, or job path remains accidentally
  global unless the docs and tests call it out as intentionally global.
- No pre-planning item remains unassigned.
