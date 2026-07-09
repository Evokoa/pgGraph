# Refactor Plan

## Strategy

Refactor by responsibility inside the current pgrx crate first. Preserve SQL
signatures, SQLSTATEs, artifacts, and observable behavior at each step. Only
extract a pure core crate after imports and tests prove a clean boundary.

Do not combine broad file moves with semantic feature work. Each split should
have a mechanical commit, unchanged tests, and a small follow-up commit for
new behavior.

## Dependency Direction

```text
gql grammar + typed values
          v
binder / logical IR
          v
optimizer / physical operators
          v
storage traits + execution runtime
          ^
PostgreSQL catalog / visibility / hydration / DML adapters
          ^
pgrx SQL facade
```

Core modules do not call pgrx/SPI directly. PostgreSQL adapters implement
narrow traits and own snapshots, roles, RLS checks, DML, catalog invalidation,
and SQLSTATE translation. Persistence and projection code depend on typed
storage identities, not the SQL facade.

## Ordered Module Splits

### 1. Memory

```text
memory/
  accounting.rs
  budget.rs
  governor.rs
  pressure.rs
  telemetry.rs
```

Move all estimates and runtime accounting behind one API before changing
algorithms. Add `MemoryAccounted` implementations for owned stores, mapped
metadata, overlays, projection snapshots, and query workspaces.

### 2. Build

```text
build/
  catalog.rs
  estimate.rs
  coordinator.rs
  snapshot.rs
  checkpoint.rs
  nodes.rs
  relationships.rs
  resolution.rs
  filters.rs
  runs.rs
  merge.rs
  artifact.rs
  publish.rs
```

Keep the existing in-memory path as one strategy behind the coordinator while
adding the spill strategy. Remove OOM decisions from low-level builders.

### 3. Projection

```text
projection/
  generation.rs
  publisher.rs
  snapshot.rs
  node_delta.rs
  edge_delta.rs
  filter_delta.rs
  ingest/
  compact/
```

Separate cross-backend publication, immutable reader snapshot, ingestion, and
range compaction. Make generation CAS testable without pgrx where possible.

### 4. GQL grammar and binder

```text
gql/
  grammar/
  ast/
  diagnostics.rs
  conformance.rs
query/bind/
  scope.rs
  pattern.rs
  expression.rs
  read.rs
  write.rs
  types.rs
```

Split `query/semantics.rs` without changing errors first. Replace historical
phase comments with capability language. The conformance registry owns feature
status.

### 5. Planner and executor

```text
query/plan/
  logical.rs
  stats.rs
  optimize.rs
  physical.rs
query/execute/
  context.rs
  operator.rs
  scan.rs
  expand.rs
  join.rs
  path.rs
  aggregate.rs
  sort.rs
  spill.rs
  write.rs
```

Convert recursive execution to resumable operators in vertical slices. Keep
old and new engines comparable behind tests until each slice is equivalent.

### 6. Value evaluation

```text
query/eval/
  scalar.rs
  predicate.rs
  aggregate.rs
  project.rs
  graph_value.rs
  path.rs
```

Move SQL hydration out of `query/value.rs`; evaluation consumes typed rows and
a hydration/visibility adapter. Delay JSONB construction to the outer boundary.

### 7. SQL facades

`sql_facade/admin.rs` becomes:

```text
sql_facade/admin/
  graph_lifecycle.rs
  registration.rs
  build.rs
  projection.rs
  maintenance.rs
  jobs.rs
  diagnostics.rs
  memory.rs
```

`sql_facade/gql.rs` becomes:

```text
sql_facade/gql/
  api.rs
  acl.rs
  visibility.rs
  hydration.rs
  locking.rs
  writes/create.rs
  writes/merge.rs
  writes/update.rs
  writes/delete.rs
  projection_delta.rs
```

`sql_sync.rs` becomes `sync/{log,replay,translate,triggers,checkpoint,ingest,legacy}.rs`.

SQL entrypoints should be thin: validate args, establish execution context,
call core/adapters, translate errors, and return rows.

### 8. Engine state

Split `engine.rs` around:

- immutable/mapped base stores and generation pin;
- resolution and graph identity;
- overlays/deltas;
- lifecycle/status/accounting;
- algorithm adapter methods.

Algorithms remain in focused modules and accept neighbor/source traits rather
than reaching into all engine state.

### 9. Persistence

Split `persistence.rs` without changing the artifact contract:

```text
persistence/
  format.rs
  writer.rs
  reader.rs
  validate.rs
  mmap.rs
  compatibility.rs
  sidecar.rs
```

`format` owns versioned constants and checked section descriptors; writer and
reader share those types but not control flow. `validate` is pure where
possible and runs before mapped stores are installed. `mmap` contains the
minimal documented unsafe pointer boundary. Compatibility/rebuild policy lives
in one module rather than branching throughout load/write code.

## Test Decomposition

Mirror production boundaries:

```text
query/tests/{bind,plan,execute,path,aggregate,write,conformance}.rs
pg_tests/gql/{read,write,rls,acl,tx,hydration,diagnostics}.rs
pg_tests/admin/{build,load,jobs,memory,maintenance,projection}.rs
```

Move tests mechanically and preserve names first. New regressions live beside
the smallest module they constrain. Cross-backend, crash, and SQLSTATE tests
remain heavy scripts when an in-process pgrx test cannot model the boundary.

## Core Crate Decision Gate

Consider `pggraph-core` only when:

- parser/binder/planner/storage modules compile without pgrx imports;
- adapter traits are stable and narrow;
- fuzz targets no longer require PostgreSQL symbol stubs for pure logic;
- moving the modules removes dependency edges rather than adding facade crates;
- artifact format/version ownership is clear.

Until then, one crate with enforced module boundaries is lower risk.

## Refactor Checkpoints

1. Add dependency/lint rules and a module ownership map.
2. Centralize memory accounting and policy.
3. Split publication/generation code and add two-backend tests.
4. Split build coordinator and strategies.
5. Split GQL binder and test modules without semantic change.
6. Introduce canonical typed IR and adapter traits.
7. Migrate read operators one vertical slice at a time.
8. Migrate hydration/visibility and then write operators.
9. Thin admin/GQL/sync SQL facades.
10. Re-evaluate crate extraction with compile-time evidence.

## Acceptance Gates

- No refactor checkpoint changes exported SQL signatures or SQLSTATEs unless its
  change is separately documented and tested.
- `cargo fmt`, clippy with warnings denied, unit tests, pgrx tests, docs drift,
  and affected heavy gates stay green after every mechanical split.
- Production modules have a single responsibility and tests can import the
  narrow unit without the mega-module.
- No core planner/executor/storage module depends on `sql_facade`.
- No circular module ownership is hidden behind broad `use super::*` imports.
- Historical phase names are removed from durable errors/docs.
- Public compatibility tables are generated from conformance metadata.
- File size is a signal, not a target; do not split cohesive code merely to
  satisfy a line count.
