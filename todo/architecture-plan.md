# Architecture Plan: openCypher, GQL, SQL/PGQ, And Mutable Graph Projections

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

## Planning Basis

This plan uses the repository's Rust planning guidance:

- keep the current single `graph` crate unless a real crate-boundary trigger
  appears;
- add architecture in layers, not through a rewrite;
- keep pgrx/PostgreSQL adapter code at the facade edge;
- make internal compiler/planner structures plain Rust and unit-testable;
- prefer direct synchronous calls because PostgreSQL SPI and the current engine
  are synchronous inside a backend process;
- avoid new unsafe code;
- plan tests before implementation.

## Architecture Summary

The target architecture is:

```text
SQL facade
  graph.cypher(...)
  graph.gql(...)
  existing graph.* SQL functions
  future SQL/PGQ adapter
        |
query frontend layer
  openCypher lexer/parser
  GQL lexer/parser
  SQL function request lowering
  SQL/PGQ adapter lowering
        |
semantic binding
  graph catalog snapshot
  label/type/property resolution
  ACL/RLS/tenant planning inputs
        |
logical graph IR
        |
physical graph operators
        |
execution runtime
  immutable CSR base
  mutable overlay deltas
  SQL/SPI lookup and hydration
        |
PostgreSQL source tables remain authoritative
```

The architecture deliberately separates language support from runtime
mutability. Read-only openCypher can run on the current immutable CSR engine.
Mutable overlay support is valuable to existing SQL APIs even without Cypher.
Cypher/GQL writes come after both layers are stable.

## Crate And Module Layout

Keep one `graph` crate initially. The feature is large, but not yet a Cargo
workspace boundary. Splitting into crates is only justified later if parser
reuse, compile-time pressure, dependency surface, or multi-team ownership makes
that worthwhile.

New module groups:

```text
graph/src/query/
  mod.rs
  value.rs
  errors.rs
  catalog_snapshot.rs
  logical_plan.rs
  physical_plan.rs
  operators.rs
  execute.rs
  explain.rs

graph/src/cypher/
  mod.rs
  ast.rs
  lexer.rs
  parser.rs
  semantics.rs
  lower.rs

graph/src/gql/
  mod.rs
  ast.rs
  lexer.rs
  parser.rs
  lower.rs

graph/src/projection/
  mod.rs
  mode.rs
  overlay.rs
  neighbors.rs
  tx_delta.rs
  mutable_adjacency.rs
  compaction.rs

graph/src/sql_facade/cypher.rs
graph/src/sql_facade/gql.rs
```

Existing modules remain owners of existing storage and behavior:

- `engine.rs`: active backend-local engine orchestration;
- `edge_store.rs`: immutable CSR edge store;
- `node_store.rs`: node SoA and active bits;
- `resolution_index.rs`: finalized and delta resolution;
- `filter_index.rs`: typed filter storage;
- `sql_sync.rs`: sync log replay;
- `sql_build.rs`: build/vacuum/maintenance orchestration;
- `persistence.rs`: `.pggraph` artifact format;
- `safety.rs`: SQLSTATE and panic boundary.

## Dependency Direction

Internal dependency direction should be:

```text
sql_facade
  -> cypher/gql/query/projection/engine/catalog/safety

cypher/gql
  -> query

query
  -> catalog snapshot traits/types
  -> projection execution traits/types
  -> domain value/error types

projection
  -> edge_store/node_store/resolution_index/filter_index/types

engine
  -> projection primitives where shared execution requires them
```

The parser and logical planner must not depend on pgrx types. pgrx stays in SQL
facades and PostgreSQL adapters. This keeps parser/planner unit tests fast and
independent of PostgreSQL.

## Public SQL Surface

Initial openCypher API:

```sql
graph.cypher(
  query text,
  params jsonb default '{}',
  hydrate boolean default true
)
RETURNS TABLE (row jsonb)
```

Plan inspection:

```sql
graph.cypher_explain(
  query text,
  params jsonb default '{}'
)
RETURNS TABLE (stage text, detail jsonb)
```

Future GQL API:

```sql
graph.gql(
  query text,
  params jsonb default '{}',
  hydrate boolean default true
)
RETURNS TABLE (row jsonb)
```

```sql
graph.gql_explain(
  query text,
  params jsonb default '{}'
)
RETURNS TABLE (stage text, detail jsonb)
```

Projection-mode selection should be exposed through build/registration APIs,
but the exact syntax remains open:

```sql
SELECT graph.build(mode := 'csr_readonly');
SELECT graph.build(mode := 'mutable_overlay');
```

## Query Frontends

### openCypher

The openCypher frontend owns:

- tokenization;
- parsing;
- AST with spans;
- syntax diagnostics;
- lowering to shared logical IR.

It does not execute plans and does not know about CSR, overlays, SPI, or pgrx.

The compatibility target should be phrased as "openCypher-compatible subset"
until a formal compatibility matrix proves broader coverage.

### GQL

The GQL frontend should be added after the shared IR is stable. It owns GQL
syntax and GQL-specific diagnostics, but lowers into the same logical IR where
semantics overlap.

GQL features that cannot map to the PostgreSQL-authoritative property graph
model should be rejected during semantic binding with stable diagnostics.

### SQL/PGQ

SQL/PGQ support should be treated as an adapter target. pgGraph should not fork
PostgreSQL's SQL/PGQ implementation. Instead, eligible graph patterns should be
lowered into the shared IR when PostgreSQL exposes stable extension points or
when SQL/PGQ graph definitions can be mapped safely to pgGraph projections.

## Semantic Binding

Semantic binding resolves query-language names against a catalog snapshot:

- node labels to registered tables or aliases;
- relationship types to registered edge labels;
- properties to validated PostgreSQL columns;
- table OIDs and primary keys;
- tenant columns;
- filter columns;
- searchable/hydratable columns;
- edge table metadata for writes;
- privilege and RLS planning inputs.

Catalog binding should produce typed errors:

- unknown label;
- ambiguous label;
- unknown relationship type;
- unknown property;
- unsupported property type;
- missing parameter;
- wrong parameter type;
- write attempted against read-only projection;
- write attempted against unregistered label/type.

No dynamic SQL should be built from user-provided identifiers without catalog
validation.

## Logical IR

The logical IR is the shared representation for openCypher, GQL, existing SQL
API lowering, and future SQL/PGQ adapters.

Core logical operators:

```text
NodeScan
NodeLookup
Expand
ExpandVariableLength
Filter
Project
Limit
Skip
Sort
Distinct
Aggregate
Optional
Join
CreateNode
CreateEdge
SetProperty
RemoveProperty
DeleteNode
DeleteEdge
DetachDeleteNode
```

Logical plans must carry:

- variable bindings;
- graph coordinates;
- source table/edge metadata;
- estimated row bounds where known;
- memory and traversal limit requirements;
- required privileges;
- tenant-scope requirements;
- supported projection modes;
- write/read-only classification.

## Physical Operators

Physical operators are executable Rust structures chosen by the planner:

```text
IndexNodeLookup
SourceTableSearch
ExpandOutCsr
ExpandInCsr
ExpandOverlayAware
FilterIndexPredicate
HydrationPredicate
ProjectionJson
HashJoin
NestedLoopBounded
AggregateRows
SpiInsertNode
SpiUpdateProperty
SpiDeleteEdge
ApplyTxDelta
```

Physical planning chooses between:

- immutable CSR base execution;
- overlay-aware execution;
- PostgreSQL SPI lookup/search/hydration;
- rejection with a stable unsupported-feature error.

Planner decisions should be visible through `cypher_explain()` / `gql_explain()`.

## Projection Runtime

### Immutable CSR Base

The committed base graph remains immutable CSR:

- forward edge store is compact adjacency;
- reverse CSR remains derived per backend unless later optimized;
- persisted artifacts remain read-only and CRC-validated;
- CSR is never mutated in place.

### Mutable Overlay

The mutable overlay is layered on top of the immutable base:

```text
OverlayNeighbors =
  base CSR neighbors excluding tombstones
  + added delta neighbors
```

Overlay state includes:

- added edge deltas;
- deleted base-edge tombstones;
- added node deltas;
- deleted node tombstones;
- property/filter deltas;
- tenant bitmap deltas;
- resolution index deltas;
- reverse adjacency deltas.

Small overlays use maps, vectors, small vectors, and bitsets. Larger long-lived
mutable regions may use arena/slab blocks for delta edges only.

Constraints:

- keep node identity as dense `u32`;
- no raw pointer handles;
- no new unsafe code;
- do not persist overlay arena/slab state;
- per-backend memory growth scales with churn, not graph size.

### Overlay-Aware Neighbor Abstraction

Introduce one neighbor abstraction used by every algorithm that can run on a
dirty mutable projection:

```rust
trait NeighborSource {
    fn neighbors(&self, node: u32, direction: Direction) -> NeighborIter<'_>;
}
```

The exact trait shape can change during implementation, but the invariant is
that algorithms should not reach directly into `EdgeStore::neighbors()` when
the projection may be dirty.

Algorithms must either consume this abstraction or reject dirty mutable
projections:

- BFS/DFS traversal;
- shortest path;
- weighted shortest path;
- connected components;
- aggregation/path enumeration;
- traversal-search hybrids.

## Transaction Delta Model

Each backend transaction owns local graph deltas:

```text
TxGraphDelta
  added_nodes
  deleted_nodes
  added_edges
  deleted_edges
  property_updates
  filter_updates
  tenant_updates
  resolution_updates
```

Cypher/GQL writes execute PostgreSQL SPI writes first. If PostgreSQL accepts the
write, pgGraph records transaction-local deltas for read-your-own-writes.

Transaction callbacks:

- commit: clear or promote local deltas after PostgreSQL commit, then rely on
  sync-log replay for committed visibility;
- abort: discard local deltas;
- subtransaction handling: either support nested delta stacks or reject write
  clauses inside unsupported subtransaction contexts until explicitly designed.

The overlay must not expose uncommitted changes across backends.

## Sync And Out-Of-Band Writes

Out-of-band SQL writes are handled through existing trigger sync infrastructure:

- source table write;
- trigger writes durable sync log row;
- backend-local graph catches up through replay;
- status/health exposes lag and recommendations.

Mutable projection work should extend the current sync path rather than create
a parallel mutation log.

Logical decoding is a future optimization only if trigger overhead or coverage
becomes unacceptable.

## Persistence

Read-only CSR persistence remains the existing `.pggraph` artifact model.

Mutable overlay state is not durable. On restart:

- load or rebuild immutable CSR base;
- discard any overlay cache snapshots unless validated;
- catch up through PostgreSQL source state and sync log.

Optional fast mutable snapshots are cache-only and must be validated against
PostgreSQL freshness markers before use.

## Locking And Isolation

Cypher/GQL writes must use PostgreSQL's locking and transaction semantics:

- write PostgreSQL source rows first;
- use parameterized SPI;
- acquire row/table locks appropriate for `INSERT`, `UPDATE`, and `DELETE`;
- respect existing build locks, source-table locks, and advisory transaction
  locks;
- reject conflicting maintenance/vacuum states where correctness is not proven.

The mutable overlay does not replace PostgreSQL MVCC.

## Error Strategy

Add typed internal query-language errors and translate them at the SQL facade
boundary through `GraphError`/SQLSTATE policy.

Internal categories:

- syntax;
- unsupported feature;
- semantic binding;
- parameter;
- type mismatch;
- schema violation;
- write-on-read-only projection;
- memory limit;
- execution;
- internal invariant.

Public diagnostics should include stable SQLSTATE, a concise message, clause or
span context where possible, and a hint when useful.

Avoid `Result<T, String>` in public or cross-layer APIs.

## Configuration

Add or extend GUCs for:

- default projection mode;
- mutable projection enablement;
- max query text length;
- max AST nodes;
- max variables/patterns;
- max Cypher/GQL returned rows;
- max hydrated rows;
- max transaction delta nodes;
- max transaction delta edges;
- max overlay memory;
- compaction threshold;
- behavior when mutable overlay limits are exceeded.

Default values should preserve current read-only behavior unless users opt into
new language/runtime features.

## Observability

Extend `graph.status()` and `graph.sync_health()` with:

- projection mode;
- overlay dirty flag;
- added/deleted node delta counts;
- added/deleted edge delta counts;
- tombstone count;
- overlay memory estimate;
- compaction recommended;
- mutable read-only fallback reason;
- unsupported algorithm reason where applicable;
- optional LSN/XID fields if safely captured.

Add explain functions for query-language plans:

- parse output summary;
- semantic binding summary;
- logical plan;
- physical plan;
- selected runtime;
- rejection/fallback reason.

## Security

Query text is untrusted input.

Required controls:

- parser totality;
- hard parser/planner limits;
- no dynamic SQL value interpolation;
- catalog/OID validation for identifiers;
- parameterized SPI for values;
- ACL checks for every touched source table;
- RLS behavior preserved by PostgreSQL execution;
- tenant scoping preserved in graph execution;
- panic-to-PostgreSQL-error boundary;
- fuzzing for parser and planner input.

## Test Architecture

Testing follows the repository's existing ladder and the rust-planning test
strategy.

Unit tests:

- lexer;
- parser;
- AST spans;
- semantic binding with fake catalog snapshots;
- logical lowering;
- physical planning;
- operator behavior;
- JSON projection;
- typed error conversion.

Property tests:

- overlay invariants;
- active-node visibility;
- resolution/filter consistency;
- edge insert/delete reduction;
- compaction equivalence between CSR+overlay and rebuilt CSR.

Fuzz tests:

- openCypher parser totality;
- GQL parser totality when added;
- unsupported-shape diagnostics;
- expression parser edge cases.

pgrx SQL tests:

- read-only openCypher success cases;
- read-only openCypher unsupported writes;
- mutable projection read-your-own-writes;
- rollback discards deltas;
- concurrent sessions do not see uncommitted deltas;
- out-of-band SQL write catch-up;
- ACL/RLS denial;
- tenant scoping;
- SQLSTATE stability.

Heavy tests:

- crash/reload;
- backup/restore;
- maintenance/vacuum interaction;
- overlay memory limits;
- large graph benchmarks;
- function metadata drift;
- docs/API drift.

Benchmark gates:

- existing CSR traversal must not regress materially;
- overlay-aware algorithms must report overhead on clean and dirty graphs;
- openCypher equivalent queries should be compared with existing SQL APIs;
- compaction thresholds should be measured on representative graphs.

## Documentation Plan

Every public behavior change updates:

- `README.md`;
- `docs/roadmap.mdx`;
- `docs/user_guide/querying.mdx`;
- `docs/user_guide/api-reference.mdx`;
- `docs/user_guide/limitations-and-fit.mdx`;
- `docs/user_guide/sync-and-maintenance.mdx`;
- `docs/user_guide/configuration.mdx`;
- `docs/contributor_guide/architecture.mdx`;
- `docs/contributor_guide/engine-internals.mdx`;
- `docs/contributor_guide/memory-model.mdx`;
- `docs/contributor_guide/persistence-format.mdx`;
- `docs/contributor_guide/sync-internals.mdx`;
- `docs/contributor_guide/safety-security.mdx`;
- release notes when appropriate.

Run docs drift checks before calling any public milestone complete.

## Risk Register

| Risk | Mitigation |
|---|---|
| "Full Cypher" implies Neo4j compatibility | Publish an openCypher-compatible subset matrix |
| Mutable overlay returns stale answers | Centralize neighbor abstraction and reject unsupported dirty algorithms |
| Query text introduces SQL injection | Catalog validation plus parameterized SPI only |
| Overlay memory grows without bound | GUC limits, compaction thresholds, read-only fallback |
| Writes bypass PostgreSQL durability | PostgreSQL-first writes only |
| Rollback leaks graph deltas | transaction callback tests and delta stack discipline |
| CSR read path regresses | benchmark gates and clean-graph fast path |
| Persistence contract weakens | never persist overlay as authoritative state |
| Docs contradict feature scope | docs contract gate before public API |

## Implementation Readiness Checklist

- Public compatibility target chosen.
- Current non-goal docs reconciled.
- Critical pre-launch safety/correctness items resolved or explicitly deferred.
- Parser design accepted.
- Query IR accepted.
- Projection-mode API accepted.
- Overlay neighbor abstraction accepted.
- SQLSTATE taxonomy accepted.
- GUC additions accepted.
- Test ladder accepted.
- Benchmark gates accepted.
