# Pre-Planning: Named Subgraphs, Tenanted Graphs, and Hot/Cold Control

Status: this pre-planning note has been reconciled into
`todo/named-graphs-complete-plan.md`. That complete plan is the source of truth
for phase ownership, acceptance criteria, quotas, generic jobs, hosted
scheduling, observability, failure behavior, and release gates.

## Request

Assess the viability of supporting:

- smaller subgraphs inside one PostgreSQL database;
- user-owned and tenant-owned graphs;
- explicit control over hot and cold graph residency.

## Initial Viability

This is viable, but it should be treated as a catalog and lifecycle expansion,
not as a separate graph database inside PostgreSQL.

The current architecture already has useful foundations:

- source tables remain authoritative;
- registration is catalog-driven;
- builds produce derived CSR projections;
- persisted artifacts are mmap-loadable;
- mutable overlays and projection generations already model some lifecycle state;
- tenant filtering already exists through tenant membership bitmaps.

The main missing concept is a first-class graph identity. Current catalogs and
artifact paths effectively describe one active graph per database installation.
To support subgraphs, user graphs, tenant graphs, and hot/cold state, graph
identity must become part of the SQL API, catalog rows, build jobs, sync state,
projection generations, artifact paths, status reporting, and engine selection.

## Current Architecture Fit

### Already aligned

- `graph._registered_tables`, `graph._registered_edges`, and
  `graph._registered_filter_columns` can be extended with a graph identifier.
- `Engine` already carries tenant membership by tenant value and tenanted table
  OIDs.
- Query paths already accept tenant scope in traversal, search, and GQL.
- Persistence already writes derived artifacts under `$PGDATA/<graph.data_dir>`.
- Projection manifests already describe durable generations and can be scoped
  per graph.
- Build and maintenance jobs already have durable job rows.

### Not yet aligned

- Catalog primary keys do not include `graph_id`.
- The active engine is backend-local and singular.
- `graph.build()` has no graph selector.
- Persistence writes to `main.pggraph`, so multiple artifacts would collide.
- Projection generation rows are keyed by generation/backend/database, not graph.
- Sync logs do not distinguish which graph registrations care about a table
  mutation.
- Admin and ACL checks are global to the `graph` schema, not graph ownership.

## Proposed Model

Introduce a `graph._graphs` catalog table:

```sql
graph._graphs (
  graph_id UUID PRIMARY KEY,
  graph_name TEXT NOT NULL,
  owner_role OID NOT NULL,
  created_by OID NOT NULL,
  tenant TEXT,
  namespace TEXT,
  graph_kind TEXT NOT NULL CHECK (graph_kind IN ('global', 'user', 'tenant', 'subgraph')),
  residency TEXT NOT NULL CHECK (residency IN ('hot', 'warm', 'cold')),
  materialization TEXT NOT NULL CHECK (materialization IN ('logical', 'materialized')),
  projection_mode TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (tenant, owner_role, namespace, graph_name)
)
```

Then add `graph_id` to registration, filter, build-job, maintenance-job, sync,
and projection-generation catalogs. Existing installs can be migrated to one
default graph.

## Feature Breakdown

### Smaller subgraphs

Best implemented as named graph registrations over the same source tables. A
subgraph can be:

- a subset of node tables;
- a subset of edge registrations;
- optional row predicates later;
- optional tenant scope;
- independently built and persisted.

The first version should avoid arbitrary SQL predicates in catalog rows. Start
with table/edge/filter subsets because they match the current builder.

### Auto-discovery for named graphs

Auto-discovery must become graph-aware.

Current discovery registers tables and FK edges into the single global graph
catalog and immediately runs `graph.build()`. With named graphs, discovery
should accept a target graph and should not assume one global build.

Target APIs:

```sql
SELECT * FROM graph.auto_discover(
  graph_name := 'customer_360',
  schema_name := 'public',
  build := false
);

SELECT * FROM graph.auto_discover_tables(
  graph_name := 'customer_360',
  tables := ARRAY['public.customers'::regclass, 'public.orders'::regclass],
  tenant_column := 'tenant_id',
  build := true
);
```

Discovery should support:

- discovering into a named graph;
- discovering a selected table set as a subgraph;
- preview/dry-run mode before writing registration rows;
- optional build after registration;
- graph-scoped conflict handling;
- generated edge labels that users can inspect and rename before build.

Old discovery APIs do not need a permanent compatibility guarantee before 1.0,
but the public API should still have a clear default-graph path for simple
single-graph users.

### User and tenant graphs

User and tenant graphs should support typical agent deployments where one
account can contain many users, each user can work across multiple workspaces
or projects, and each workspace can contain multiple graphs.

Target ownership hierarchy:

```text
tenant/account
  -> user
    -> workspace/project/namespace
      -> graph
```

The graph catalog should therefore model more than just graph name:

- tenant/account identifier;
- owner role;
- creator role;
- optional workspace/project/namespace;
- graph name;
- graph-level grants;
- residency policy;
- materialization mode;
- projection mode;
- quota state.

Tenant graphs have two possible meanings:

- query-time tenant scoped views over a shared physical projection;
- physically separate tenant projections.

The shared projection is cheaper and mostly exists today. Physically separate
tenant projections are useful when tenants are large, need independent
retention, or must avoid loading unrelated topology.

User graphs are mostly an ownership and ACL problem. They require graph-level
permissions and likely an owner/admin split:

- owner can register/build/reset their graph;
- readers can query the graph;
- graph schema admins can manage all graphs.

Most per-user or per-workspace graphs should start cheap. The default should be
metadata/logical graph definitions over shared materialized projections where
possible. Dedicated physical graph artifacts should be reserved for graphs that
are large, hot, isolated, or explicitly materialized by policy.

This keeps agent-style deployments viable when one tenant has many users and
each user owns many small graphs.

### Relationship management and graph map export

Relationship registration should become easier to inspect and edit.

Current behavior:

- `graph.add_edge()` registers a relationship.
- Calling `graph.add_edge()` again with the same `(from_table, from_column,
  to_table, to_column, label)` updates `bidirectional`, `weight_column`, and
  `label_column`.
- `graph.remove_edge(label)` deletes relationships by label.
- Renaming a relationship label or changing endpoint columns is not a dedicated
  operation today; users effectively remove/re-add and rebuild.

Add explicit relationship management APIs:

```sql
SELECT graph.rename_edge(
  graph_name := 'customer_360',
  old_label := 'customer_id',
  new_label := 'placed_by'
);

SELECT graph.alter_edge(
  graph_name := 'customer_360',
  label := 'placed_by',
  bidirectional := false,
  weight_column := 'score',
  label_column := NULL
);

SELECT graph.remove_edge(
  graph_name := 'customer_360',
  label := 'placed_by'
);
```

Add graph map/export APIs:

```sql
SELECT graph.graph_map('customer_360');
SELECT graph.graph_map('customer_360', format := 'json');
```

The JSON map should include:

- graph metadata;
- registered node tables and primary keys;
- registered relationship mappings;
- generated/static labels;
- dynamic label columns;
- tenant columns;
- filter columns;
- build/sync/projection status summary;
- warnings such as missing build, stale schema, ambiguous labels, or rebuild
  required.

This should be a metadata map, not a dump of all nodes and edges.

### High-priority GQL node identity lookup

Add a first-class, fast path for querying a node by registered label and source
primary key.

Current GQL can express common lookup shapes such as
`MATCH (u:users {id: $id}) RETURN u`, but this should become an explicit
identity lookup that uses the graph resolution index instead of table-wide node
scans.

Target SQL helper APIs:

```sql
SELECT graph.get_node(
  graph_name := 'customer_360',
  label := 'users',
  id := 'u1'
);

SELECT graph.get_neighbors(
  graph_name := 'customer_360',
  label := 'users',
  id := 'u1'
);
```

Target GQL syntax:

```sql
SELECT graph.gql(
  'MATCH (u:users) WHERE id(u) = $id RETURN u',
  '{"id": "u1"}'
);
```

Implementation requirements:

- resolve `(graph_name, label, id)` to `(graph_id, table_oid, primary_key)`;
- use the engine resolution index to find `node_idx` directly;
- enforce source-table ACL and tenant scope before returning the node;
- work with composite primary-key string encoding;
- support `hydrate := false` coordinate-only responses;
- expose a predictable not-found result/error policy;
- lower supported GQL identity predicates into the same direct lookup path.

This is high priority because application flows often start from a known
business identifier and then expand the graph neighborhood.

### GQL feature backlog

Keep GQL as the primary graph-language surface for this planning cycle. Push
broader Cypher compatibility later.

Guiding principle: pgGraph should feel like a full graph layer while
PostgreSQL remains the source of truth. Graph-style reads and writes are in
scope when labels, relationship types, properties, and identities can map
cleanly to registered PostgreSQL tables and columns. Mutating GQL operations
must run PostgreSQL-first DML before projection updates, so constraints,
triggers, ACLs, RLS, MVCC, and source indexes stay authoritative.

Priority additions after direct node-id lookup:

1. Relationship source-row hydration and relationship property projection.
   Registered edge-row relationships should be able to return useful
   properties, not only coordinate identity.
2. Node-only optional matches.
3. Broader wildcard path support for currently rejected combinations where they
   can remain bounded: `ORDER BY`, `WITH`, `RETURN DISTINCT`, aggregates, and
   optional matches.
4. Relationship creation through PostgreSQL-first DML for registered edge-row
   mappings.
5. Multi-row writes, with strict transaction, rollback, ACL, tenant, and
   SQLSTATE tests.
6. Temporary graph node indexes for transaction-created nodes so newly created
   nodes can participate in traversal, joins, and path reads inside the same
   transaction.
7. Dynamic label write semantics only if they can map cleanly to registered
   table or label-column metadata.
8. Public SQL/PGQ exposure only after PostgreSQL provides stable graph-pattern
   hooks and the compatibility matrix is green.

Explicitly defer:

- broad openCypher compatibility;
- GQL-driven PostgreSQL schema creation;
- unbounded or planner-hostile path forms;
- unmapped graph writes that cannot be routed through PostgreSQL as the source
  of truth.

### Hot and cold control

Suggested semantics:

- `hot`: eagerly load or keep loaded in the current backend when selected;
- `warm`: persisted and auto-loadable on demand;
- `cold`: persisted but not auto-loaded unless explicitly loaded.

Current PostgreSQL backend isolation means hot state is per backend. There is
no shared Rust heap across connections. Persisted mmap artifacts let multiple
backends share OS page-cache pages, but each backend still owns its `Engine`.

The first implementation should expose explicit SQL controls:

```sql
SELECT graph.load_graph('name');
SELECT graph.unload_graph('name');
SELECT graph.set_graph_residency('name', 'cold');
```

Automatic cluster-wide hot graph management would require background worker
policy work and should be later.

## Implementation Phases

### Phase 1: Named graph catalog

- Add `graph._graphs`.
- Add `graph_id` to registration tables.
- Add a default graph migration path.
- Add SQL API overloads accepting `graph_name`.
- Keep old APIs mapped to the default graph.

### Phase 2: Per-graph build and artifact paths

- Make build/read catalog functions accept `graph_id`.
- Write artifacts to graph-scoped paths such as
  `$PGDATA/<graph.data_dir>/<graph_id>/main.pggraph`.
- Add `graph_id` to build and maintenance jobs.
- Add status APIs that report per graph.

### Phase 3: Backend engine selection

- Replace the single backend-local active engine with a small per-backend engine
  registry keyed by `graph_id`, or keep one active selected graph plus explicit
  load/switch semantics.
- The conservative first choice is one active selected graph per backend.
- A multi-engine registry is more ergonomic but raises memory-pressure risks.

### Phase 4: Tenant and ownership policy

- Add graph ownership and reader grants.
- Decide whether tenant graphs are shared-projection views, separate physical
  projections, or both.
- Enforce tenant scope consistently for traversal, search, GQL reads, and GQL
  writes.
- Decide and document the PostgreSQL RLS stance before claiming tenant-grade
  isolation.

### Phase 5: Hot/cold lifecycle

- Add residency metadata.
- Implement explicit load/unload.
- Add optional eager load for hot graphs at first query or backend startup path.
- Add observability for loaded engines, artifact size, mmap status, and last
  access.

## Risks

- Catalog migration needs care because registration primary keys currently
  assume one global graph.
- Sync fanout can get expensive if many graph registrations depend on the same
  source table.
- Multiple loaded engines per backend can exceed memory expectations.
- Projection generation cleanup must become graph-aware to avoid deleting files
  still needed by another graph.
- User/tenant graph ACLs must not allow graph queries to bypass source-table
  privileges.
- Arbitrary row-predicate subgraphs would complicate build validation, sync,
  and GQL writes; defer them until named table/edge subgraphs are stable.
- RLS expectations can become a security footgun if pgGraph exposes topology
  from graph artifacts without clearly honoring row-level visibility.

## Initial Recommendation

Proceed, but in a narrow order:

1. Implement named graph catalogs and default-graph compatibility.
2. Scope build, status, persistence, and projection generation by graph.
3. Add explicit graph load/unload and one active selected graph per backend.
4. Treat tenant graphs first as tenant-scoped views over shared projections.
5. Add physically separate tenant projections only after the named graph path is
   stable and benchmarked.

## Automated Sync Follow-Up

Users also want graph sync to become hands-off: once sync is enabled, the graph
should stay fresh without application code calling `graph.apply_sync()` or
maintenance functions directly.

### Current state

- Trigger sync records source-table changes into `graph._sync_log`.
- Query freshness can apply pending sync on reads up to a captured high-water
  mark.
- Mutable-overlay projections can ingest committed sync rows into durable
  projection segments and reload the latest manifest.
- `graph.run_scheduled_maintenance()` already combines apply-sync,
  projection-ingest, and background maintenance decisions, but it is still an
  explicit SQL entry point rather than a permanently autonomous scheduler.

### Chunk and delta model

There are three update layers today:

- transaction-local deltas: backend-local maps for read-your-own-writes during
  the current transaction;
- durable delta segments: immutable segment files referenced by projection
  manifests;
- replacement base chunks: full source-node ranges published as chunk files and
  selected by a newer manifest generation.

The durable delta segment format has `source_start` and `source_end`, so it can
represent a source-node range. Current ingestion uses broad `0..u32::MAX`
segments for edge directions, so the format is range-capable but ingestion is
not yet producing fine-grained dirty-range delta chunks by default.

Replacement base chunks are the stronger answer to partial rewrite. A new
manifest can point at replacement chunks for dirty source-node ranges while the
original base artifact remains immutable and older generations stay readable.
That is safer than overwriting bytes inside `main.pggraph`.

### Can we overwrite only changed graph parts?

Yes, but the viable design is copy-on-write replacement, not in-place mutation:

1. detect dirty source-node ranges from sync rows;
2. materialize final neighbors for those ranges by merging base plus durable
   deltas;
3. write new chunk files for only those ranges;
4. publish a new manifest generation that points to the new chunks;
5. garbage collect obsolete chunks/segments only after no active backend still
   references the old generation.

Directly overwriting part of the base CSR file is not recommended. CSR offsets
make in-place updates hard unless the edge count and byte layout stay exactly
the same, and PostgreSQL backends may already have the old artifact mmap'd.
Manifest-level copy-on-write avoids torn reads and keeps crash recovery simple.

### Better automated sync path

The safest incremental path is:

1. make `mutable_overlay` plus durable projection ingest the default automated
   sync path for write-heavy users;
2. add a background worker or scheduled worker policy that periodically runs the
   same decision logic as `graph.run_scheduled_maintenance()`;
3. teach projection ingestion to emit dirty-range segments instead of broad
   full-range segments when sync rows identify affected source nodes;
4. compact dirty ranges into replacement base chunks when segment fanout or
   read amplification crosses thresholds;
5. expose sync SLA settings such as max lag rows, max lag age, compaction
   threshold, and cold/hot graph residency policy.

This should be designed together with named graphs, because automated sync
policy will need to run per graph, with per-graph build locks, watermarks,
projection generations, and retention.

### Preferred scheduler model

Use a Timescale-style internal jobs and policies model rather than only exposing
a low-level background worker.

The user-facing goal should feel like:

```sql
SELECT graph.add_sync_policy(
  graph_name := 'customer_360',
  schedule_interval := '10 seconds',
  max_sync_lag_rows := 1000,
  max_sync_lag := '30 seconds',
  compact_after_segments := 32,
  enabled := true
);

SELECT * FROM graph.jobs();
SELECT * FROM graph.job_stats();
SELECT graph.run_job(job_id);
SELECT graph.alter_job(job_id, schedule_interval := '1 minute');
SELECT graph.remove_job(job_id);
```

Under the hood:

- `graph._jobs` stores configured automation jobs and policy metadata.
- `graph._job_runs` stores execution history, duration, status, error, rows
  applied, segments published, chunks rewritten, and maintenance job IDs.
- One optional launcher background worker per database wakes on a configurable
  interval and looks for due jobs.
- Each due job acquires a per-graph advisory lock before doing work.
- Work is bounded and transactional: apply one sync batch, ingest one projection
  batch, compact one bounded segment set, or enqueue one maintenance rebuild.
- The SQL function `graph.run_scheduled_maintenance()` remains the canonical
  execution primitive, but the job framework calls it per graph according to
  policy.

This should behave like Timescale policies:

- explicit SQL policy creation and deletion;
- visible job catalog;
- visible run history and last error;
- manual `run_job()` for debugging;
- GUCs for global worker enablement, max concurrent jobs, and wake interval;
- no requirement for application code to call apply-sync in normal operation.

Implementation should still support external scheduling. If the background
worker is unavailable in a hosted PostgreSQL environment, users can call the
same job runner through `pg_cron`, system cron, or application orchestration.

### Hosted mode

Document and support two automation modes:

- internal worker mode: pgGraph launches a background worker and runs due jobs
  itself;
- hosted mode: the same SQL job runner is invoked by `pg_cron`, a provider
  scheduler, system cron, or application orchestration.

Both modes should use the same job catalogs, locks, run history, and failure
behavior. Hosted mode should not be a second-class code path.

### Quotas

Add a quota model so one graph, tenant, or job cannot consume the whole
database.

Initial quota dimensions:

- maximum named graphs;
- maximum materialized/physical graphs;
- maximum named graphs per tenant/account;
- maximum named graphs per user;
- maximum named graphs per workspace/namespace;
- maximum artifact storage;
- maximum configured jobs;
- maximum sync lag rows;
- maximum sync lag age;
- maximum build memory;
- maximum loaded graphs per backend;
- maximum concurrent jobs;
- maximum compaction work per run.

Quotas should be visible through SQL and should fail with actionable errors.

### Observability views

Add SQL views/functions for operators and application dashboards:

```sql
SELECT * FROM graph.graphs();
SELECT * FROM graph.jobs();
SELECT * FROM graph.job_stats();
SELECT * FROM graph.sync_health();
SELECT * FROM graph.storage_usage();
SELECT * FROM graph.graph_status('customer_360');
```

These views should expose graph identity, owner, residency, materialization
state, build state, sync lag, last job run, last error, artifact storage,
projection generation, and maintenance recommendations.

### Failure behavior

Define public behavior for common failure modes:

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

Each failure should have:

- a stable status value;
- a clear SQLSTATE where appropriate;
- a recommended recovery action;
- visibility in job history or graph status;
- no silent fallback that hides stale graph reads.

### RLS stance

Decide the row-level security stance explicitly.

The conservative default is:

- pgGraph always enforces source-table ACL checks;
- pgGraph does not claim full PostgreSQL RLS equivalence unless every query,
  build, sync, hydration, and graph-artifact path can prove row visibility under
  the caller or graph owner policy;
- tenant isolation should use explicit tenant columns and graph tenant scope,
  not implicit assumptions about RLS.

If RLS support is added later, it needs a dedicated design:

- whether builds run as graph owner, table owner, or invoker;
- whether artifacts may contain rows hidden from some readers;
- whether query-time filters must recheck RLS through SQL before returning
  coordinates;
- how GQL writes interact with RLS and source-table policies;
- leak tests for topology-only results, hydrated results, search, traversal,
  GQL, sync, and background jobs.

### Testing matrix

Add release gates for:

- multi-graph ACL behavior;
- tenant leak prevention;
- graph-scoped auto-discovery;
- relationship rename/alter/remove behavior;
- graph map JSON output;
- extension upgrade/catalog migration;
- backup and restore;
- job scheduler correctness;
- hosted-mode external scheduler path;
- crash recovery during build, ingest, compaction, and chunk publication;
- artifact garbage collection with active backend heartbeats;
- quota enforcement;
- sync lag and repeated job failure behavior.
