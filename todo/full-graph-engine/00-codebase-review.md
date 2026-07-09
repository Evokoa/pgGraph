# Codebase Review

## Scope And Method

This static Rust review covered build/load, GQL parse-to-execute, hydration,
relationship representation, filter indexes, projection publication and
compaction, Rust domain types, GQL values, unsafe/mmap and PostgreSQL FFI
boundaries, pgrx usage, public planning docs, and the largest
implementation/test modules.

The pre-existing low-memory patch was validated before commit with:

```text
git diff --check
bash -n on the three changed heavy scripts
cargo fmt --check
cargo clippy --features "pg17 development" --all-targets -- -D warnings
cargo test --features pg17
cargo test --features pg17 sql_build
targeted cargo pgrx tests for low-memory replacement and GUC contracts
```

It removes old-plus-new overlap in the building backend and drops an owned
persisted graph before mmap reload. It does not make build, load, compaction,
or queries bounded-memory.

## P0 Resource Findings

### Memory policy is advisory on important paths

`oom_action = 'readonly'` continues the same allocation after an over-limit
estimate. Read-only changes graph mutation semantics; it is not a smaller
representation. A second check in the builder duplicates memory policy.

The estimate uses `pg_class.reltuples` and fixed per-node/per-edge constants.
It excludes filters, PK width, tenant sets, reverse CSR, serialization buffers,
PostgreSQL executor memory, and allocator slack.

Evidence:

- `graph/src/sql_build.rs:427-500`
- `graph/src/builder.rs:202-231`
- `graph/src/builder.rs:266-300`
- `graph/src/catalog/validate.rs:193-215`

### Build batches do not bound dominant structures

Every filter cell is retained until all nodes are scanned. The real filter
indexes are then allocated. Resolution clones and sorts its complete entry set
and serializes another buffer; persistence encodes the full filter index into
another vector.

Evidence:

- `graph/src/builder.rs:302-429`
- `graph/src/filter_index.rs:416-445`
- `graph/src/resolution_index.rs:110-131`
- `graph/src/persistence.rs:568-609`

`graph.build_batch_size` bounds cursor/spool batches, not total build memory.

### mmap load can OOM

Load constructs a complete owned reverse CSR and deserializes `FilterIndex`
into each backend heap. Runtime auto-load has no budget reservation.

Evidence:

- `graph/src/sql_facade/runtime.rs:452-462`
- `graph/src/engine.rs:415-427`
- `graph/src/edge_store.rs:419-468`
- `graph/src/persistence.rs:854-884`

An artifact may build successfully and later kill a backend on first use.

### Projection maintenance is independently unbounded

Compaction loads eligible segments before checking budgets. One dirty-chunk
path materializes every final edge and constructs another CSR. Current layered
reads also reload files and rebuild maps rather than pinning one immutable
generation snapshot in the engine.

Evidence:

- `graph/src/projection/compact.rs:62-77`
- `graph/src/projection/compact.rs:186-205`
- `graph/src/projection/compact.rs:329-362`
- `graph/src/projection/layered.rs:92-156`
- `graph/src/projection/layered.rs:212-349`

### Query row caps do not bound intermediate memory

`source_nodes()` collects a whole label and filters into another vector, so
`LIMIT 1` can remain O(label size). A supernode expansion builds, sorts, and
deduplicates a degree-sized vector. Recursive variable-length paths clone state
per branch. Projection, hydration, grouping, DISTINCT, sorting, and SQL result
conversion add more materialized vectors. Dijkstra allocates a graph-sized
distance array.

Evidence:

- `graph/src/query/execute.rs:78-218`
- `graph/src/query/execute.rs:385-459`
- `graph/src/query/execute.rs:708-725`
- `graph/src/query/execute.rs:893-952`
- `graph/src/query/physical_plan.rs:10-13`
- `graph/src/query/value.rs:108-249`
- `graph/src/path_finder.rs:408-455`
- `graph/src/sql_facade/gql.rs:142-147`

The 10,000-row/key cap does not limit bytes, expansions, path states, degree,
or JSON width.

## P0 Correctness And Security Findings

### Coordinate-only GQL visibility requires an RLS fix

Static control flow shows table ACL preflight followed by projection
enumeration. Source-row visibility is consulted only when hydration is needed.
With `hydrate := false`, nodes, relationships, paths, IDs, counts, or existence
may therefore reflect RLS-hidden rows. The existing test proves hydrated reads
fail closed, but there is no coordinate-only counterpart.

Evidence:

- `graph/src/sql_facade/gql.rs:171-177`
- `graph/src/sql_facade/gql.rs:243-307`
- `graph/src/sql_facade/gql.rs:3015-3059`
- `graph/src/query/value.rs:991-1055`
- `graph/src/pg_tests/gql.rs:471-522`

First reproduce with two roles. Then batch-filter node and relationship
candidates through PostgreSQL under the caller snapshot. Hidden topology must
not influence aggregates or path existence. A topology-bypass privilege, if
ever added, must be explicit and non-default.

### Relationship identity collapses a property multigraph

The sorted CSR builder discards equal endpoint/type/direction tuples. Runtime
steps and hydration keys carry no source edge-row key. Hydration queries by
endpoints/type and takes arbitrary `LIMIT 1`.

Evidence:

- `graph/src/edge_store.rs:227-252`
- `graph/src/query/execute.rs:1020-1035`
- `graph/src/query/value.rs:23-45`
- `graph/src/sql_facade/gql.rs:3062-3122`

Parallel relationships can collapse, hydrate the wrong row, and cannot be
updated or deleted by stable graph identity.

### Filter identity collides across source tables

Filter metadata stores a table OID, but lookup, pending replay, populated
counts, and pushdown resolve by column name alone. Registering the same column
name on two tables can skip the second registration or use the wrong index.

Evidence:

- `graph/src/filter_index.rs:343-348`
- `graph/src/builder.rs:376-429`
- `graph/src/builder.rs:946-977`
- `graph/src/sql_filters.rs:26-31`
- `graph/src/sql_filters.rs:343-373`

Use a stable `FilterColumnId` containing graph/table/attribute identity through
registration, build, pushdown, persistence, and deltas.

### Publication is not cross-backend serialized

Projection ingestion uses a Rust process-local mutex. PostgreSQL backends do
not share that mutex. Manifest publication checks `exists()` before `rename()`,
leaving a cross-process race.

Evidence:

- `graph/src/projection/ingest.rs:27-110`
- `graph/src/sql_sync.rs:565-635`
- `graph/src/projection/manifest.rs:217-253`

Use a graph-scoped PostgreSQL advisory or equivalent cross-process lock, reload
the generation inside it, and compare-and-swap the previous generation and
watermark.

### Replacement validation occurs after artifact rename

The writer renames its temporary file over the serving path before the build
path mmap-loads and validates it. A failed validation has already displaced the
last good path.

Evidence: `graph/src/persistence.rs:611-624` and
`graph/src/sql_build.rs:89-128`.

### Rust soundness and PostgreSQL boundary blockers

Three safe/unsafe boundaries do not currently meet Rust's safety contract:

- `NodeStore::table_oid()` is safe but its mmap branch dereferences
  `oid_ptr.add(node_idx)` without checking `node_idx < node_count`.
- mapped edge constructors require valid pointer ranges but do not encode
  validated CSR offset values; safe weighted/schema neighbor access can reuse
  an invalid offset for pointer arithmetic;
- custom SQLSTATE emission calls PostgreSQL `errfinish()` directly below
  pgrx's top-level guard, allowing PostgreSQL longjmp semantics to cross live
  Rust frames.

Evidence:

- `graph/src/node_store.rs:315-322`
- `graph/src/edge_store.rs:68-141`
- `graph/src/edge_store.rs:522-597`
- `graph/src/safety.rs:238-280`

Two PostgreSQL boundary findings are also P0:

- security-definer entrypoints do not declare an explicit hardened pgrx search
  path;
- registered relations are persisted/re-resolved through names and search
  path rather than stable OID/regclass identity.

Evidence:

- `graph/src/sql_facade/`
- `graph/sql/bootstrap.sql:75-91`
- `graph/src/catalog/validate.rs:132-190`
- `graph/src/builder.rs:341`

### Durable filter values lose their type

The in-memory filter model supports signed integers, booleans, text tokens,
dates, timestamps, and UUIDs. Durable projection ingestion reduces updates to
`Option<u32>`: negative values are clamped, large values narrow, and UUID
updates are dropped. Segment replay therefore cannot be equivalent to a clean
build for the supported value domain.

Evidence:

- `graph/src/filter_index.rs:42-57`
- `graph/src/projection/ingest.rs:32-48`
- `graph/src/sql_sync.rs:1383-1391`
- `graph/src/projection/segment.rs:116-126`

## P1 Architecture Findings

- **Source-order planning:** multi-pattern plans execute recursively from
  pattern zero without graph statistics, costing, join reorder, expand-into,
  or estimated work/memory in explain.
- **Hydration N+1:** broad candidates are projected before source hydration,
  and hydration performs one SPI lookup per unique coordinate.
- **Representation ceilings:** node/edge indexes use `u32`; relationship types
  use `u8` and reject more than 255 entries. Keep these explicit or widen them
  in an artifact migration.
- **Disabled production type safety:** `NodeIdx` and `EdgeTypeId` exist only in
  test/development builds, while production passes interchangeable `u32`/`u8`
  values and performs unchecked narrowing casts.
- **Stringly PostgreSQL boundaries:** closed GUCs, graph/job/policy/catalog
  states, UUID identities, error reasons, and engine status are parsed or
  stored as strings despite pgrx enum, UUID, and OID support.
- **Incomplete GQL value model:** decimal literals, parameters, hydration, and
  aggregation use `f64`/JSON as semantic storage, which cannot preserve exact
  PostgreSQL numeric and temporal behavior.
- **Background-worker transaction ambiguity:** a plain Rust `Result::Err`
  returned from a pgrx transaction closure can allow the closure to return
  normally and commit earlier job mutations.
- **Transaction completeness:** mutable graph writes reject savepoints instead
  of maintaining per-subtransaction delta frames; isolation-level behavior
  needs two-session gates.
- **mmap mutation fallback:** some node sync mutations materialize the complete
  mmap node store into owned memory.
- **Memory accounting drift:** build guard, `graph.estimate()`, runtime profile,
  filters, and projection ingest use different formulas.

Primary evidence:

- `graph/src/query/lower.rs:50-80`
- `graph/src/query/execute.rs:240-270`
- `graph/src/sql_facade/gql.rs:3015-3122`
- `graph/src/types.rs:20-64`
- `graph/src/config.rs:110-147`
- `graph/src/gql/ast.rs:488-500`
- `graph/src/query/lower.rs:434-443`
- `graph/src/sql_facade/admin.rs:1858-1866`
- `graph/src/engine.rs:436-444`
- `graph/src/projection/tx_delta.rs:650-699`
- `graph/src/node_store.rs:397-413`

## Maintainability

The crate contains about 85,500 Rust source lines. Largest files at review:

| File | Lines | Split around |
|---|---:|---|
| `sql_facade/admin.rs` | 5,963 | lifecycle, registration, projection, jobs, diagnostics |
| `query/tests.rs` | 5,577 | capability-focused test modules |
| `pg_tests/gql.rs` | 4,616 | read, write, security, transaction, hydration |
| `query/semantics.rs` | 3,932 | scope, expression, path, read, write binding |
| `pg_tests/maintenance_admin.rs` | 3,544 | build, load, jobs, maintenance, memory |
| `sql_facade/gql.rs` | 3,275 | API, ACL, hydration, locking, mapped DML |
| `engine.rs` | 2,545 | state, stores, resolution, overlays, adapters |
| `query/value.rs` | 2,508 | scalar evaluation, predicates, aggregate, projection |
| `persistence.rs` | 2,291 | format, writer, validation, loader, sidecars |
| `sql_sync.rs` | 2,096 | log, replay, triggers, checkpoint, ingestion |

Split modules inside the existing pgrx crate first and enforce dependency
direction. Evaluate a pure core crate only after that boundary is real; an
early workspace split would move coupling rather than remove it.

Historical phase comments have drifted from implemented behavior. Replace them
with enduring capability descriptions and generate compatibility tables from
one conformance registry.

## Strengths To Preserve

- PostgreSQL-first mapped DML keeps source constraints, triggers, security, and
  MVCC authoritative.
- Edge endpoint resolution already uses bounded PostgreSQL spool batches.
- Forward CSR, node arrays, PK bytes, and resolution can be mmap-backed.
- Traversal APIs already have useful node/frontier breakers.
- pgrx guards, typed `GraphError`, and artifact validation are useful bases;
  custom SQLSTATE emission and mapped-store construction must be repaired
  before they can be treated as safe boundaries.
- Transaction deltas and projection generations support richer freshness.

## Measurements Still Required

- Phase-by-phase RSS with long PKs, many filters, degree skew, and tenants.
- PostgreSQL temp memory versus Rust heap versus page-cache contribution.
- Cold/warm mmap traversal and per-backend private-memory costs.
- Dense/sparse filter and in-memory/spill break-even points.
- Long-build snapshot contention and sync catch-up lag.

These affect tuning, not the need for hard budgets and spill.
