# Architecture Plan: GQL, SQL/PGQ, And Mutable Graph Projections

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
  graph.gql(...)
  existing graph.* SQL functions
  future SQL/PGQ adapter
  graph.cypher(...)
        |
query frontend layer
  GQL lexer/parser
  SQL function request lowering
  SQL/PGQ adapter lowering
  openCypher compatibility lowering
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
mutability. Read-only GQL can run on the current immutable CSR engine. Mutable
overlay support is valuable to existing SQL APIs even without GQL. GQL writes
come after both layers are stable. openCypher compatibility remains a separate
surface over the shared planner rather than a Neo4j compatibility claim.

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

graph/src/gql/
  mod.rs
  ast.rs
  lexer.rs
  parser.rs
  semantics.rs
  lower.rs

graph/src/projection/
  mod.rs
  mode.rs
  overlay.rs
  neighbors.rs
  tx_delta.rs
  mutable_adjacency.rs
  compaction.rs

graph/src/sql_facade/gql.rs
```

**Phase-owned module groups:**

```text
graph/src/cypher/            # Phase 4 only — optional openCypher compatibility
  mod.rs
  ast.rs
  lexer.rs
  parser.rs
  semantics.rs
  lower.rs

graph/src/sql_facade/cypher.rs   # Phase 4 only
```

`graph/src/projection/` was created in Phase 2, not Phase 1. The `cypher/`
modules and `sql_facade/cypher.rs` were deferred until Phase 4 so their presence
matches the explicit compatibility commitment in the public contract (see Risk
Register and `phase-4-advanced-writes-opencypher-design.md`).
SQL/PGQ does not get its own frontend module at all — it lowers into the shared
IR through an adapter once the IR is stable (Phase 3), so it reuses
`graph/src/query/` rather than adding a parser directory.

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

gql/cypher
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

Initial GQL API:

```sql
graph.gql(
  query text,
  params jsonb default '{}',
  hydrate boolean default true
)
RETURNS TABLE (row jsonb)
```

Plan inspection:

```sql
graph.gql_explain(
  query text,
  params jsonb default '{}'
)
RETURNS TABLE (stage text, detail jsonb)
```

SQLSTATE additions for GQL syntax, semantic, parameter, unsupported-feature,
write-on-read-only, and execution errors are designed when this SQL facade is
implemented, not during parser-only work.

> **Pre-facade gate task (own decision artifact):** *Define the GQL SQLSTATE
> mapping before exposing `graph.gql()`.* SQLSTATE design is deferred but is a
> hard release gate, tracked as its own item in
> `mutable-graph-projections-todo.md` → "Decision Records". The public
> `graph.gql()` function must not ship until each internal error category has a
> stable, documented SQLSTATE.

### Canonical JSONB Result Shape (decided)

`graph.gql()` returns one `jsonb` column named `row` per result row. Each row is
a JSON object whose keys are the `RETURN` aliases (explicit `AS`, else the
source expression text). Values are encoded by kind:

| GQL value | `hydrate := true` | `hydrate := false` |
|---|---|---|
| Node | `{"_id": {"table": "users", "id": "u1"}, "_labels": ["users"], "<col>": <value>, ...}` | `{"_id": {"table": "users", "id": "u1"}, "_labels": ["users"]}` |
| Relationship | `{"_type": "follows", "_start": {"table":"users","id":"u1"}, "_end": {"table":"users","id":"u2"}, "<col>": <value>, ...}` | `{"_type": "follows", "_start": {...}, "_end": {...}}` |
| Path | `{"_path": {"nodes": [<node>...], "relationships": [<rel>...]}}` (node/rel encoded per their `hydrate` rule) | same, coordinate-only nodes/rels |
| Scalar property (`v.name`) | the JSON scalar of the PG value | same (hydration is a node/path concern, scalars are always materialized) |
| `count`/aggregate | JSON number/array/object | same |

Rules:

- **Node/relationship identity** is always the stable graph coordinate
  (`{"table", "id"}` using source table name + primary-key text), never the
  internal dense `u32` index. Multi-column primary keys serialize `id` as a JSON
  array in key order.
- **Null vs missing:** a property that exists in the source but is SQL `NULL`
  serializes as JSON `null`. A property the catalog does not model for that
  label is a bind-time `UnknownProperty` error, not a silent omission — so
  "missing property" cannot occur in a successfully-bound query. (When
  jsonb-backed open property bags arrive in Phase 3, missing-key vs explicit
  `null` semantics get their own documented rule.)
- **`hydrate := false`** returns coordinate-only nodes/relationships (no source
  columns), avoiding the per-row `to_jsonb(src.*)` SPI lookup. Scalars and
  aggregates are unaffected.
- **Required hydration is fail-closed:** if a coordinate in the graph cannot be
  hydrated from its backing PostgreSQL source row, GQL raises `PG017` instead of
  manufacturing an empty hydrated node object or matching `IS NULL` predicates.
- The `_id` / `_labels` / `_type` / `_start` / `_end` / `_path` reserved keys
  are stable contract and snapshot-tested. Source columns named with a leading
  underscore collide with reserved keys and are a documented bind-time
  rejection.

See `phase-1-readonly-gql-design.md` for worked examples and the snapshot-test
corpus.

openCypher compatibility API:

```sql
graph.cypher(
  query text,
  params jsonb default '{}',
  hydrate boolean default true
)
RETURNS TABLE (row jsonb)
```

```sql
graph.cypher_explain(query text) RETURNS text

graph.cypher_compatibility()
RETURNS TABLE (feature text, status text, notes text)
```

Projection-mode selection should expose the accepted mode names:

```sql
SELECT graph.build(mode := 'csr_readonly');
SELECT graph.build(mode := 'mutable_overlay');
```

## Query Frontends

### GQL

The GQL frontend owns:

- tokenization;
- parsing;
- AST with spans;
- syntax diagnostics;
- lowering to shared logical IR.

It does not execute plans and does not know about CSR, overlays, SPI, or pgrx.

The compatibility target should be phrased as "GQL-compatible subset aligned
with SQL/PGQ graph pattern matching" until a formal compatibility matrix proves
broader coverage.

#### Parser implementation strategy (decided)

The GQL frontend is a **handwritten lexer + recursive-descent parser** with a
precedence-climbing (Pratt) sub-parser for `WHERE` expressions. No
parser-generator (lalrpop/pest/peg) and no external grammar crate.

Justification:

- **Diagnostics and spans are first-class requirements.** Hand-rolled descent
  gives exact control over byte spans, clause context, and hint text. Generated
  parsers make stable, friendly diagnostics harder and their output opaque.
- **Dependency surface must stay minimal and pgrx-free.** Query text is
  untrusted input; a hand-rolled parser keeps the audited code in-repo with no
  third-party parsing dependency in the attack surface.
- **Totality and fuzzing are simpler.** A hand-rolled parser is
  straightforward to drive to "every input returns a typed error, never
  panics," and the fuzz target is the parser entry function with no generated
  intermediate to reason about.
- **The grammar grows slice by slice.** Phase 1A–1D and later phases each add
  productions incrementally; recursive descent extends cleanly without
  regenerating a grammar.
- **House style.** The engine already favors zero-dependency, auditable code.

The lexer is a separate pass producing tokens with spans; the parser consumes
tokens and never re-reads raw bytes. See
`phase-1-readonly-gql-design.md` for the concrete grammar productions, token
set, and AST.

### SQL/PGQ

SQL/PGQ support should be treated as a close standards adapter target. pgGraph
should not fork PostgreSQL's SQL/PGQ implementation. Instead, eligible graph
patterns should be lowered into the shared IR when PostgreSQL exposes stable
extension points or when SQL/PGQ graph definitions can be mapped safely to
pgGraph projections.

### openCypher Compatibility

openCypher support is a separate compatibility surface. It owns openCypher
syntax and diagnostics, but lowers into the same logical IR where semantics
overlap.

openCypher features that cannot map to the PostgreSQL-authoritative property
graph model should be rejected during semantic binding with stable diagnostics.

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

### Catalog Snapshot Interface (decided first shape)

The snapshot is an **immutable, pgrx-free value** built once per query from the
existing catalog read path (`catalog::read::read_catalog`, which is the only
SPI-touching part). The binder and planner consume the snapshot through a trait
so they unit-test against a fake. The concrete struct wraps the existing
`RegisteredTable` / `RegisteredEdge` / `RegisteredFilterColumn` rows plus
resolved `TableOid`s.

```rust
/// Immutable, pgrx-free view of registered graph metadata for one query.
pub trait CatalogSnapshot {
    /// Resolve a GQL node label to a registered table (and its OID).
    /// Errors: UnknownLabel, AmbiguousLabel.
    fn resolve_node_label(&self, label: &str) -> Result<NodeLabelInfo, BindError>;

    /// Resolve a GQL relationship type to a registered edge label.
    /// Errors: UnknownRelType.
    fn resolve_rel_type(&self, rel_type: &str) -> Result<RelTypeInfo, BindError>;

    /// Resolve a property name on a bound label to a source column + type.
    /// Errors: UnknownProperty, UnsupportedPropertyType.
    fn resolve_property(
        &self,
        label: &NodeLabelInfo,
        property: &str,
    ) -> Result<PropertyInfo, BindError>;

    /// Tenant column for a label, if the table is tenant-scoped.
    fn tenant_column(&self, label: &NodeLabelInfo) -> Option<&PropertyInfo>;

    /// Filter-index columns available for fast predicate pushdown on a label.
    fn filter_columns(&self, label: &NodeLabelInfo) -> &[PropertyInfo];

    /// Catalog fingerprint for plan-cache keying and schema-drift detection.
    fn fingerprint(&self) -> u64;
}

pub struct NodeLabelInfo {
    pub table_name: String,
    pub table_oid: TableOid,
    pub primary_key: PrimaryKeySpec, // existing builder type
    pub tenant_column: Option<PropertyInfo>,
}

pub struct RelTypeInfo {
    pub label: String,
    pub edge_type_id: u8,          // matches edge_store u8 ceiling
    pub from_table: String,
    pub to_table: String,
    pub bidirectional: bool,       // drives undirected-relationship semantics
    pub weight_column: Option<PropertyInfo>,
}

pub struct PropertyInfo {
    pub column_name: String,
    pub pg_type: PropertyType,     // closed set; mirrors FilterCondition variants
    pub filter_indexed: bool,
}
```

`PropertyType` is a closed enum mirroring the typed-filter set the engine
already supports (integer/bigint, numeric, boolean, dictionary-encoded text,
uuid, date, timestamptz, plus `Unsupported`). ACL/RLS are NOT modeled as
snapshot fields: they are enforced at execution time by reusing
`acl::check_table_acl(table_oid)` for every touched table and by routing all
value access through SPI so PostgreSQL RLS applies. The snapshot only carries
the `TableOid` needed to make that ACL call. See
`phase-1-readonly-gql-design.md` for the concrete builder and the fake used in
binder unit tests.

## Logical IR

The logical IR is the shared representation for GQL, existing SQL API lowering,
future SQL/PGQ adapters, and optional openCypher compatibility.

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

Planner decisions should be visible through `gql_explain()` and optional
compatibility explain functions.

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

GQL writes execute PostgreSQL SPI writes first. If PostgreSQL accepts the
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

GQL writes must use PostgreSQL's locking and transaction semantics:

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
- max GQL returned rows;
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

GQL compatibility is tracked by an explicit matrix. A feature is not
"supported" until parser, semantic, execution, negative, explain, and docs
coverage all exist for that row. The matrix lives in
`todo/mutable-graph-projections-todo.md` while this branch is active and should
later move into public compatibility docs when the SQL facade exists.

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

- GQL parser totality;
- openCypher parser totality if compatibility is added;
- unsupported-shape diagnostics;
- expression parser edge cases.

pgrx SQL tests:

- read-only GQL success cases;
- read-only GQL unsupported writes;
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
- GQL equivalent queries should be compared with existing SQL APIs;
- compaction thresholds should be measured on representative graphs.

Generated GQL test dimensions:

- direction: outbound, inbound, undirected;
- label and relationship-type resolution: explicit, omitted, unknown,
  ambiguous;
- predicates: equality, inequality, range, null, missing property, membership;
- return shapes: node, relationship, property, path, count, aggregate, map;
- limits: none, zero, small positive, large over-limit;
- tenant visibility: unscoped, scoped allowed, scoped denied;
- projection state: clean CSR, dirty overlay, stale projection;
- hydration: hydrated JSONB rows and coordinate-only rows.

Stable graph fixtures:

- chain;
- branch;
- cycle;
- disconnected components;
- multi-table labels;
- multi-edge-type graph;
- weighted graph;
- tenant-scoped graph;
- Phase 2 overlay/sync graph with inserts, deletes, tombstones, and replayed
  out-of-band SQL writes.

Pre-code performance baselines:

- unit and pgrx SQL suites are recorded in
  `todo/regression-baseline-2026-05-29.md`;
- Criterion `bfs_bench` baseline is saved as `pre_gql_mutable_overlay`;
- SQL-facing Panama and LDBC sandbox reports are recorded by report path and
  summary;
- heavy memory scripts are recorded, including build RSS and the Linux-only PSS
  script behavior on this macOS host.

Every implementation phase should update the baseline comparison before
claiming completion, especially around CSR traversal, overlay traversal,
filter-index traversal, graph construction, SQL-facing query latency, and
memory footprint.

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
| "Full GQL" implies complete standard coverage | Publish a GQL-compatible subset matrix |
| Later openCypher support implies Neo4j compatibility | Publish a separate compatibility matrix |
| Mutable overlay returns stale answers | Centralize neighbor abstraction and reject unsupported dirty algorithms |
| Query text introduces SQL injection | Catalog validation plus parameterized SPI only |
| Overlay memory grows without bound | GUC limits, compaction thresholds, read-only fallback |
| Writes bypass PostgreSQL durability | PostgreSQL-first writes only |
| Rollback leaks graph deltas | transaction callback tests and delta stack discipline |
| CSR read path regresses | benchmark gates and clean-graph fast path |
| Persistence contract weakens | never persist overlay as authoritative state |
| Docs contradict feature scope | docs contract gate before public API |

## Implementation Readiness Checklist

Status as of 2026-06-01. Per-phase detail lives in the phase design docs
(`phase-1-readonly-gql-design.md` … `phase-4-advanced-writes-opencypher-design.md`).

- [x] Public compatibility target chosen — GQL-compatible subset; matrix-gated.
- [x] Current non-goal docs reconciled — public docs now describe the supported
      GQL, SQL/PGQ adapter, and openCypher compatibility boundaries.
- [x] Critical pre-launch safety/correctness items named — see
      `mutable-graph-projections-todo.md` Phase 1 step 3; P0/P1/P2 public rows
      are closed, including memory sizing visibility and edge-label cardinality
      documentation.
- [x] Parser design accepted — handwritten lexer + recursive descent + Pratt
      `WHERE` parser (see Parser Implementation Strategy above).
- [x] Query IR accepted (shape) — concrete logical/physical types defined in
      `phase-1-readonly-gql-design.md`; frontend-neutral so SQL/PGQ adapts later.
- [x] Projection-mode API accepted — `csr_readonly` / `mutable_overlay`.
- [x] Overlay neighbor abstraction accepted — `NeighborSource` was finalized in
      Phase 2 and is used by dirty-overlay-aware traversal paths.
- [x] SQLSTATE taxonomy accepted — public GQL/openCypher errors map through the
      stable graph SQLSTATE surface.
- [x] GUC additions accepted (list) — see Configuration; defaults preserve
      current read-only behavior.
- [x] Test ladder accepted — see Test Architecture + per-phase docs.
- [x] Benchmark gates accepted — the historical pre-implementation baselines
      were captured 2026-05-29 at `0574e6b`; G1 was rechecked under
      `caffeinate` with no Criterion regression rows.

The ordered phase implementation is closed for the current pgGraph scope. Future
work such as public SQL/PGQ exposure remains gated on PostgreSQL graph-pattern
hooks rather than this branch's implementation checklist.
