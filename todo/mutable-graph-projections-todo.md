# Working TODO: GQL And Mutable Graph Projections

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

## Goal

Explore GQL/SQL-PGQ support and mutable graph projections without
breaking pgGraph's current PostgreSQL-first contract.

The long-term direction is to let users choose the graph runtime shape when
building a graph:

- read-only projection: very fast CSR-backed graph execution that requires
  rebuild or explicit maintenance to fully sync topology changes.
- mutable projection: read/write graph execution with lower rebuild pressure,
  optimized in-memory topology structures, and source-table consistency.

PostgreSQL source tables remain authoritative. The mutable graph layer should
feel close to native graph speed while avoiding unbounded RAM growth or a second
durable source of truth.

## Contract Reconciliation

This branch intentionally changes pgGraph's future positioning from "no graph
query language" to "GQL subset now, broader GQL/SQL-PGQ planned,
PostgreSQL-first, compatibility matrix required." Public docs and source-level
crate docs now describe `graph.gql()` as a documented subset, not a full GQL,
SQL/PGQ, Cypher, Gremlin, or SPARQL compatibility promise.

Before expanding the public graph-language surface further, keep public docs,
SQL signatures, SQLSTATE rows, and this compatibility matrix in the same
commit as the code change that exposes the behavior.

The existing private `.agents/private/cypher-support-plan.md` is no longer the
primary implementation plan. It remains useful research for parser/planner
shape, JSONB row output, safety limits, and frontend-neutral IR design. The new
primary target is GQL-first with SQL/PGQ alignment and optional future
openCypher compatibility.

## Scope Gates

- Do not expose public GQL functions until the compatibility contract and
  docs positioning are updated together.
- Do not add GQL writes until read-only GQL has a parser, semantic binder,
  logical plan, physical plan, SQL facade, tests, and docs.
- Do not add mutable projection writes until transaction-local overlays,
  rollback behavior, out-of-band SQL sync, and memory-limit behavior are proven.
- Resolve or explicitly reprioritize critical pre-launch safety/correctness
  items before expanding the attack surface with arbitrary query text and graph
  writes.

## Architecture Decisions

- Compatibility target: GQL-compatible subset first, aligned with SQL/PGQ graph
  pattern matching where possible.
- Public API target: `graph.gql(query, params, hydrate)` returning JSONB rows,
  with `graph.gql_explain(query)` for diagnostics.
- Public docs strategy: update roadmap and limitations wording now; add API
  reference docs only when SQL functions exist.
- Projection mode names: `csr_readonly` and `mutable_overlay`.
- GQL writes target PostgreSQL source tables first. Projections react to
  PostgreSQL state and must not become a second durable source of truth.
- Mutable projection writes are transaction-scoped. A transaction must read its
  own GQL writes and rollback must discard projection deltas.
- SQLSTATE additions for the GQL facade are implemented for syntax,
  unsupported-feature, semantic, parameter, execution, and write-on-read-only
  paths; broaden them only when new write categories need distinct handling.
- The first public graph-language milestone should be read-only GQL, not
  openCypher. It should prioritize graph pattern matching semantics that align
  with SQL/PGQ.
- The first write-capable GQL milestone is intentionally narrow:
  - reads: a single bounded graph pattern with `WHERE` and `RETURN`;
  - writes: insert a node mapped to a registered source table, update a mapped
    property, and delete a mapped edge row.
- Defer `MERGE`, cascading deletes, complex path-pattern mutations, broad
  function coverage, and full variable-length write semantics until the
  transaction and projection architecture is proven.
- Projection freshness should keep the existing sync-log freshness model and may
  augment it with PostgreSQL-native positions such as `source_lsn`,
  `projection_lsn`, and transaction identifiers. The current model tracks
  `applied_sync_id` against the durable sync log (`max_sync_log_id`, exposed as
  `sync_lag` / `pending_sync_rows`) plus `SchemaState::{Fresh,Stale}` for schema
  drift; there is no `build_epoch` concept in the code today. LSN values require
  explicit capture at build/sync points and are only comparable within the same
  timeline.
- Runtime health should expose mutable-graph pressure metrics such as
  `dirty_pages`, `tombstone_count`, delta size, and compaction/rebuild need.
- Mutable runtime state is a cache. On restart, rebuild from PostgreSQL source
  tables plus committed sync state rather than trusting a custom durable store.
- Fast mutable snapshots may be considered later, but only if validated against
  PostgreSQL LSN and replayed forward safely after restart.
- Introduce a shared planner layer. GQL, existing SQL functions, SQL/PGQ
  adapters, and openCypher compatibility should compile into common logical and
  physical graph operators.
- MVP GQL node inserts may only create rows for labels that map to registered
  source tables. Creating new labels/tables from GQL is out of scope.
- Do not make CSR mutable. The committed base graph stays immutable CSR so it
  remains compact, cache-friendly, mmap-shareable, and easy to validate.
- Arenas/slabs are candidates for mutation deltas or a future fully resident
  mutable projection mode, not for the committed CSR base.
- GQL and mutability are orthogonal. Read-only GQL should ship on the existing
  immutable CSR path before mutable projection writes.

## Design Checkpoints

- Define the public meaning of a graph projection mode.
- Define the public GQL surface and how it maps to PostgreSQL tables,
  graph catalog metadata, and projection runtimes.
- Decide exactly which GQL reads are legal on read-only CSR projections and
  which GQL writes require mutable projections.
- Preserve a clear user choice at graph build time: read-only CSR speed vs
  mutable read/write graph layer.
- Preserve PostgreSQL MVCC, ACL, RLS, durability, and recovery boundaries.
- Keep sync, invalidation, and rebuild behavior explicit and observable.
- Keep CSR as a physical layout detail, not the product-level contract.
- Design mutable topology deltas around compact indexes, stable handles, and
  cache-conscious adjacency. Use simple per-transaction vectors/maps for small
  overlays; only use arena/slab adjacency where mutation volume and lifetime
  justify the extra complexity.
- Bound memory growth with tombstone compaction, delta thresholds, maintenance
  rebuilds, and read-only fallback states.
- Document consistency guarantees for readers during projection writes.
- Define GQL-to-PostgreSQL type mapping, especially dynamic properties,
  lists, mixed lists, maps, nulls, and missing properties.
- Define how existing SQL APIs behave on mutable projections. Avoid silently
  degrading `graph.traverse()` users from CSR-speed behavior to a slower runtime
  without status, docs, or explicit mode visibility.
- Define how `graph.reset()`, `graph.auto_load`, `graph.load()`,
  `graph.vacuum()`, `graph.maintenance()`, backup/restore, and crash recovery
  behave for each projection mode.
- Define GUCs for projection mode defaults, mutable enablement, transaction
  delta limits, compaction thresholds, and memory caps. Phase 2 added
  `graph.default_projection_mode`, `graph.mutable_enabled`, queued-build mode
  persistence, transaction-delta lifecycle coverage, public GQL node-create
  deltas, delta limits, compaction recommendations, and overlay memory caps.
- Define SQLSTATE policy for GQL syntax, unsupported feature, semantic,
  parameter, type mismatch, schema violation, write-on-read-only, and memory
  limit errors.

## Graph MVCC Direction

Do not make a single shared mutable adjacency list carry all transaction
visibility rules. That would force full multi-version graph storage in Rust and
create a large memory and correctness risk.

IMPORTANT: today the engine is backend-local, not shared. Each PostgreSQL
backend loads or derives its own copy of the graph (forward CSR, reverse CSR,
resolution index, filter index, tenant bitmaps) into backend-local heap, and
catches up to committed state by replaying the durable sync log
(`applied_sync_id` vs `max_sync_log_id`). There is no shared in-memory
committed projection across backends. Any design that treats "the committed
projection" as a single shared read model is proposing major new
shared-memory infrastructure (shared CSR storage, cross-backend invalidation,
and cross-backend locking) and must be tracked as such — it is not how the
current runtime works. The default and lower-risk path is to keep the
per-backend + sync-log model and layer transaction-local overlays on top.

Preferred direction (per-backend committed model):

- Keep each backend's committed projection as its local read model for
  committed data, caught up to the sync log.
- Keep each backend transaction's uncommitted graph deltas backend-local.
- During a transaction, graph reads merge committed projection state with that
  transaction's local insert/update/delete deltas.
- Other sessions must not observe those local deltas until PostgreSQL commit.
- On commit, PostgreSQL tables and WAL are authoritative; each backend's
  projection then applies committed changes (via the sync log) or marks itself
  stale up to the commit LSN. Cross-session visibility of committed writes flows
  through the durable sync-log replay path, not through shared memory.
- On rollback, discard the backend-local graph deltas.
- For out-of-band SQL writes, integrate with the existing durable trigger sync
  log and replay path. Evaluate logical decoding only if trigger overhead or
  coverage becomes unacceptable.

This keeps PostgreSQL responsible for ACID durability and MVCC while pgGraph
handles a transaction-local overlay plus committed projection catch-up.

## Storage Strategy

The committed base graph remains immutable CSR.

Reasons:

- CSR gives one contiguous neighbor slice per node, which is the fastest shape
  for hot traversal and path loops.
- Read-only persisted `.pggraph` sections can be mmap-backed and shared through
  the OS page cache across backends.
- Persistence validation stays simple: fixed sections, offsets, lengths, CRCs,
  and rebuildability from PostgreSQL source tables.
- Mutating CSR in place would break mmap sharing, force per-backend heap copies,
  and weaken the persistence contract.

Mutation state lives in overlays layered on top of the base:

```text
neighbors(node) =
  base_csr_slice(node).filter(not tombstoned)
  + delta_added_edges(node)
```

Small transaction-local overlays should start simple:

- `HashMap<u32, SmallVec<DeltaEdge>>` or equivalent per-source added edges.
- Tombstone bitsets or compact per-node tombstone sets for deleted base edges.
- Per-transaction vectors/maps that can be dropped on rollback.

Arena/slab adjacency is useful only when the mutable region becomes large or
long-lived enough to justify it:

- O(1) amortized edge insert into delta blocks.
- Bulk free by dropping the transaction or projection-local arena.
- Index-based stable handles without raw pointers.
- Bounded fragmentation through fixed-size slab blocks and free lists.

Arena/slab constraints:

- Keep node identity as dense `u32` node indexes. Do not move nodes into a
  generational arena unless resolution, filters, tenants, persistence, and all
  algorithms are redesigned around it.
- Store delta edges in the arena, not the committed graph.
- Maintain forward and reverse delta adjacency together, or mark reverse
  expansion stale/unsupported.
- Use index-based handles only. No raw-pointer arena design and no new unsafe
  code for this path.
- Do not persist arena/slab state. Rebuild or compact from PostgreSQL plus sync
  state on restart.

Compaction means folding overlays into a fresh immutable CSR through the normal
build/rebuild path when delta size, tombstones, or memory pressure crosses a
threshold. The per-backend heap cost should scale with churn, not graph size.

## Existing Infrastructure To Build On

- Durable trigger sync log and replay in `sql_sync.rs`.
- Backend-local `edge_buffer` / `EdgeMutation` overlay state in `engine.rs`.
- Traversal overlay merging through BFS neighbor iteration.
- `ResolutionDeltaIndex` for post-build node resolution.
- Mutable `FilterIndex` `set`/`clear` operations for individual node values.
- `NodeStore` active-bit/tombstone support.
- Tenant membership bitmaps used by traversal hot loops.
- Existing status and health surfaces: `graph.status()` and
  `graph.sync_health()`.

The mutable projection design must either extend these structures or clearly
replace them. Do not create a parallel overlay model without explaining how it
interacts with the current sync path.

## Data Structure Impacts

- `ResolutionIndex`: mutable projections need a mutable lookup path for newly
  created rows and transaction-local nodes.
- `FilterIndex`: property writes must update filterable values, including
  transaction-local visibility rules.
- `NodeStore`: creates/deletes must update active bits or overlay active state
  without leaking uncommitted changes across sessions.
- Tenant bitmaps: creates/updates/deletes must maintain tenant membership or
  reject writes when tenant assignment is ambiguous.
- Edge label registry: dynamic edge creation must respect compact edge type ID
  limits and registered edge labels. Edge `type_id` is currently a `u8`
  (`edge_store.rs`), so there is a hard ceiling of 256 distinct edge types;
  dynamic edge-type creation from GQL must account for this limit.
- Reverse CSR (`reverse_edge_store`): the reverse adjacency is currently a
  derived structure rebuilt from the forward CSR per backend
  (`EdgeStore::reversed`). Edge creates/deletes must update or invalidate both
  the forward and reverse adjacency; mutable projections need an incremental or
  mark-stale strategy for the reverse direction since `expand-in` reads it.
- Node labels: labels map to registered source tables for the MVP.
- Persistence: mutable projections cannot rely on writable mmap CSR sections.
  Read-only mode may use persisted `.pggraph`; mutable mode needs explicit
  owned-memory load/build behavior and clear `graph.load()` semantics.
- Delta adjacency: transaction-local overlays can start with simple maps and
  small vectors. Arena/slab blocks are a later implementation choice for larger
  mutable regions, not a prerequisite for GQL writes.
- Algorithms: overlay-aware execution must cover traversal, shortest path,
  weighted shortest path, connected components, search/traversal hybrids, and
  aggregation where supported. Phase 2A introduced the shared neighbor-source
  abstraction for clean CSR and edge overlays; `bfs.rs`, unweighted
  `path_finder.rs`, `connected_components.rs`, `sql_aggregation.rs`, and
  read-only GQL relationship expansion now consume pending edge overlays.
  Weighted shortest path rejects dirty edge overlays with `PG018` until
  vacuum/maintenance merges weights into CSR. Source-table search remains a
  PostgreSQL source-row lookup rather than a graph-topology algorithm;
  `traverse_search` inherits overlay-aware traversal.

## Blind Spots To Resolve

> Several of these are now resolved as **Decision Records** (DR-1…DR-5) in the
> "Resolved Decisions" section below. The remainder stay here as Phase-2 design
> inputs to address inside `phase-2-mutable-overlay-writes-design.md`.

- Per-backend vs shared committed projection. The engine is backend-local
  today; deciding whether the mutable committed model stays per-backend
  (sync-log catch-up) or becomes shared-memory changes the locking, memory, and
  invalidation design fundamentally. Resolve this before MVCC design.
- Graph MVCC and concurrent transaction visibility.
- Out-of-band SQL mutations against registered source tables.
- Memory-limit behavior for large transactions and large mutation deltas.
- Whether mutable projection over-limit errors abort only the GQL statement
  or the whole transaction.
- Whether any spill-to-disk path is allowed, and if so whether it is temporary
  and non-authoritative.
- GQL dynamic property typing vs PostgreSQL's typed columns.
- Locking strategy for `CREATE`, `SET`, and `DELETE` over source rows and
  edges.
- Interaction with the existing build lock protocol, source table locks, and
  advisory transaction locks.
- LSN/XID capture points and timeline limitations for projection freshness
  metrics.
- Migration path between existing CSR-backed graphs and mutable projections.
- Whether mutable projections are allowed while maintenance/vacuum is running
  and what happens to active transaction overlays during rebuild.

## Phased Dependency Plan

### Phase 1: Read-Only GQL

Full design: `phase-1-readonly-gql-design.md`.

1. Reconcile product/docs contract (Phase 1A PR work): `graph/src/lib.rs` line 5
   "No new query language", `docs/user_guide/index.mdx`,
   `docs/contributor_guide/architecture.mdx`.
2. Treat `.agents/private/cypher-support-plan.md` as background research, not
   the primary plan.
3. Confirm critical pre-launch safety/correctness items. **Named items** (from
   `docs/known-issues.mdx`): the Data Correctness, SQL Contract, SQL Safety,
   Persistence, Sync, Internal-Construction, P0, P1, and P2 categories are all
   cleared ("No currently tracked items"). The former P0 per-backend memory
   visibility item is closed by `graph.memory_profile(concurrent_backends)`.
   The former P2 edge-type ceiling item is closed by user-facing documentation;
   GQL writes still respect the existing `PG004` edge-label limit.
4. Add private GQL parser/AST and fuzz tests.
5. Add catalog binding (`CatalogSnapshot`) and shared logical graph IR.
6. Add physical operators for read-only GQL over existing primitives.
7. Expose read-only `graph.gql()` / `graph.gql_explain()` only after docs,
   SQLSTATEs, ACL/RLS, tenant scope, and tests are complete.

**Phase 1 sub-slices** (each is an independently mergeable, TDD-sized PR; the
public `graph.gql()` function ships only when 1A–1D + SQLSTATE + docs
reconciliation are all green — it may live behind `#[cfg(feature = "development")]`
earlier, matching the existing `BuildResult` pattern):

- **1A — Frontend foundation:** docs/contract reconciliation; handwritten lexer
  + recursive-descent parser + AST with spans; parser totality + fuzz target.
  No binding, no execution. Output: a parsed AST for the Phase-1 grammar subset
  and typed syntax errors.
- **1B — Bind + plan + execute a single directed `MATCH`:** `CatalogSnapshot`
  trait + concrete impl + fake; semantic binder; logical IR; physical lowering
  to one `ExpandOutCsr`/`ExpandInCsr` over existing primitives;
  `graph.gql_explain()`; coordinate-only output (no hydration, no `WHERE`).
  First end-to-end vertical slice.
- **1C — Predicates, RETURN shapes, hydration:** `WHERE` predicates
  (eq/neq/range/null/membership) mapped onto the typed filter set; `RETURN`
  node/relationship/property; the canonical JSONB result shape with
  `hydrate := true|false`; ACL/RLS/tenant enforcement at execution.
- **1D — Ordering, limits, var-length:** `ORDER BY`, `SKIP`, `LIMIT` (hard row
  caps); undirected relationships (duplicate/identity rules); bounded
  variable-length relationships with explicit max bounds.

Phase 1 must not be marked complete until every claimed supported GQL feature
has coverage at all required layers: parser, semantic binding, logical plan,
physical execution, negative diagnostics, and docs/status matrix entry.

### Phase 2: Mutable Overlay And GQL Writes

1. Define one overlay-aware neighbor abstraction and route `path_finder`,
   `connected_components`, and other graph algorithms through it or explicitly
   reject dirty mutable projections.
2. Design mutable projection storage and transaction-local delta overlays,
   starting with simple maps/vectors and evaluating arena/slab blocks only for
   larger long-lived deltas.
3. Add narrow GQL writes targeting PostgreSQL first, then projection
   update/invalidation.

Phase 2 must prove read-your-own-writes, rollback discard, concurrent
isolation, out-of-band SQL sync catch-up, and dirty-overlay algorithm behavior.
Writes remain deliberately narrow until locking, transaction callback,
SQLSTATE, tenant, and type-mapping decisions are implemented.

### Phase 3: Advanced Read GQL And SQL/PGQ Adapter

1. Add multi-stage read query support such as `OPTIONAL MATCH`, `WITH`,
   `DISTINCT`, aggregates, and path functions once Phase 1 proves the shared
   read planner.
2. Add SQL/PGQ adapters once the shared IR is stable.

Phase 3 status, 2026-05-31: the advanced read slices are closed for the current
single-pattern planner, and the internal typed SQL/PGQ adapter seam is present.
SQL/PGQ remains non-public until PostgreSQL exposes stable graph-pattern hooks.

### Phase 4: Advanced GQL Writes And Optional Compatibility

1. Add advanced write semantics such as `REMOVE`, `DETACH DELETE`, and `MERGE`
   only after Phase 2 proves PostgreSQL-first writes, row locking,
   transaction-local overlays, and sync replay.
2. Add optional openCypher compatibility only after GQL/SQL-PGQ direction is
   stable.

Phase 4 status, 2026-06-01: all planned Phase 4 slices are closed for the
current pgGraph scope. `REMOVE` is closed for single-node mapped property
removal over scalar columns and registered dotted JSONB property paths.
`DETACH DELETE` is closed for single-node mapped deletes over registered
edge-row relationships. `MERGE` is closed for single-node mapped node identity
matches with lazy `ON CREATE` / `ON MATCH` branches and same-key race coverage.
The optional openCypher compatibility surface is present as separate
`graph.cypher()`, `graph.cypher_explain()`, and `graph.cypher_compatibility()`
functions, accepts only the supported overlap with pgGraph's GQL subset, and
does not claim Neo4j compatibility.

## Implementation Tracks

- GQL parser and planner integration
- GQL SQL function/API surface
- Shared logical graph IR
- Shared physical graph operators such as index scan, expand-out, expand-in,
  filter, project, hash join, update property, create node, and delete edge
- Projection catalog model
- Projection mode selection during graph build (`graph.build(mode := ...)`
  added in Phase 2B infrastructure)
- SQL read/write API shape
- Runtime mutation model
- Immutable CSR base plus mutable overlay storage design
- Simple transaction-local delta maps/vectors (`TxGraphDelta` skeleton added in
  Phase 2B infrastructure)
- Optional arena/slab adjacency for larger long-lived mutable regions
- Backend-local transaction delta overlay
- Sync and invalidation flow
- Out-of-band SQL mutation capture
- Persistence or rebuild strategy
- Sync-id / SchemaState plus optional LSN/XID based projection freshness
  tracking
- Memory accounting and read-only fallback states
- OOM and transaction abort policy
- GQL/PostgreSQL type mapping policy
- Mutable `ResolutionIndex`, `FilterIndex`, `NodeStore`, tenant bitmap, and
  edge label registry interactions
- Existing SQL API behavior on mutable projections
- `graph.status()`, `graph.sync_health()`, `graph.reset()`, `graph.load()`,
  `graph.auto_load`, `graph.vacuum()`, and `graph.maintenance()` behavior
- GUCs and SQLSTATE mapping
- Tests for GQL reads, GQL writes, graph writes, stale reads, rebuilds,
  rollback, concurrent transactions, out-of-band SQL writes, and crash/reload
  behavior
- Benchmarks against current SQL APIs, CSR traversal, and representative native
  graph workloads
- User and contributor documentation updates

## Test Plan

Treat the GQL support surface as a compatibility matrix, not a slogan. Every
supported row in the matrix needs:

- parser tests;
- semantic/planner tests;
- execution tests comparing `graph.gql()` results against existing SQL API
  results where an equivalent SQL API exists;
- negative/security tests;
- explain-output coverage through `graph.gql_explain()`;
- docs/status coverage before the feature is called supported.

Test layers:

- Unit tests for lexer, parser, AST spans, semantic binding, logical plans,
  physical lowering, type mapping, projection formatting, and typed error
  conversion.
- Snapshot tests for stable `graph.gql_explain()` output and parser/planner
  diagnostics.
- Generated combinational tests over direction, labels, relationship types,
  predicates, return shape, limits, tenant scope, hydration, and unsupported
  writes.
- Fuzz tests for GQL parser totality, expression parser totality, and
  unsupported-shape diagnostics. Random input must return a typed error, never
  panic.
- Proptests for mutable overlay invariants, active-node visibility,
  resolution/filter index consistency, edge insert/delete reduction, and
  compaction equivalence between CSR+overlay and rebuilt CSR.
- pgrx SQL tests for read-only GQL, write rejection on read-only projections,
  narrow write support on mutable projections, ACL/RLS, tenant scope, rollback,
  read-your-own-writes, concurrent sessions, out-of-band SQL sync, and
  SQLSTATE stability.
- Heavy tests for crash/reload, maintenance/vacuum interaction, memory-limit
  behavior, status/health row shapes, function metadata drift, backup/restore,
  and release gates.
- Benchmark gates to ensure read-only CSR traversal does not regress when the
  shared planner and mutable projection code are added.
- Docs/API drift checks for every public SQL signature, GUC, SQLSTATE, row
  shape, and behavior change.

Stable fixture graphs:

- simple chain;
- branching graph;
- cycle;
- disconnected graph;
- multi-label/table graph;
- multi-edge-type graph;
- weighted graph;
- tenant-scoped graph;
- graph with deleted/synced overlay rows after Phase 2 starts.

Generated matrix dimensions:

- direction: outbound, inbound, undirected;
- node label: explicit, omitted, unknown, ambiguous;
- relationship type: explicit, omitted, unknown;
- predicate: equality, inequality, range, null, missing property, membership;
- return shape: node, property, relationship, path, count, aggregate, map;
- limit shape: none, small positive, zero, large over-limit;
- tenant shape: unscoped, scoped allowed, scoped denied;
- projection shape: clean CSR, dirty overlay, stale projection;
- hydration shape: hydrated rows, graph coordinates only.

Phase gates:

- Phase 1: read-only GQL parses, binds, plans, executes, explains, and rejects
  writes without requiring mutable overlay support.
- Phase 2: GQL writes update PostgreSQL first, then transaction-local overlay
  state. Public mapped `CREATE`, `SET`, `DELETE`, `REMOVE`, `DETACH DELETE`,
  and `MERGE` coverage plus heavy transaction lifecycle tests prove rollback
  discard, commit cleanup, concurrent backend isolation, query-time sync
  catch-up for source-table writes, memory-limit aborts, and crash/reload
  behavior for uncommitted transaction overlays.

## GQL Compatibility Matrix

Status values: `phase_1`, `phase_2`, `phase_3`, `phase_4`, `out_of_scope`,
`unknown`.

Coverage values: `supported`, `required`, `reject`, `deferred`, `optional`.

| GQL feature area | Target status | Parser | Semantics | Execution | Negative tests | Notes |
|---|---:|---:|---:|---:|---:|---|
| `MATCH` single graph pattern | phase_1 | supported | supported | supported | supported | First public read surface. |
| Node labels mapped to registered source tables | phase_1 | supported | supported | supported | supported | Unknown/ambiguous labels fail. |
| Relationship types mapped to registered edge labels | phase_1 | supported | supported | supported | supported | Unknown types fail. |
| Directed outbound/inbound relationships | phase_1 | supported | supported | supported | supported | Maps to forward/reverse execution. |
| Undirected relationships | phase_1 | supported | supported | supported | supported | Includes duplicate/identity rules. |
| `WHERE` property predicates | phase_1 | supported | supported | supported | supported | Covers eq/neq/range/null/membership plus boolean combinations. |
| Parameters through JSONB | phase_1 | supported | supported | supported | supported | Missing/wrong type errors covered. |
| `RETURN` node and scalar property values | phase_1 | supported | supported | supported | supported | JSONB row shape is stable. |
| JSONB list/map property paths | phase_3 | supported | supported | supported | supported | Dotted registered properties rooted at a JSONB source column; missing keys project null but do not match `IS NULL`. |
| `RETURN` coordinate-only single-hop relationship identity | phase_1 | supported | supported | supported | supported | Relationship source-row hydration is deferred. |
| `RETURN` raw paths and path functions | phase_3 | supported | supported | supported | supported | Bounded named relationship paths and `nodes`, `relationships`, `length` are supported; standalone path variables remain later work. |
| `ORDER BY`, `SKIP`, `LIMIT` | phase_1 | supported | supported | supported | supported | Hard row limits still apply. |
| Bounded variable-length relationships | phase_1 | supported | supported | supported | supported | Requires explicit max bounds; variable-length relationship return is rejected. |
| `OPTIONAL MATCH` | phase_3 | supported | supported | supported | supported | Top-level single-relationship optional matches are supported; node-only optional matches and post-`WITH` optional joins remain later multi-pattern work. |
| `WITH` | phase_3 | supported | supported | supported | supported | Projection-stage `WITH` is supported; aggregate `WITH` projections remain later multi-stage row-stream work. |
| `DISTINCT` | phase_3 | supported | supported | supported | supported | `RETURN DISTINCT`, `WITH DISTINCT`, and aggregate `DISTINCT` are bounded by the GQL unique-key cap. |
| Aggregates: `count`, `sum`, `avg`, `min`, `max`, `collect` | phase_3 | supported | supported | supported | supported | `RETURN` aggregates over node-only and single-relationship row streams; aggregate `DISTINCT` is supported. |
| Path functions: `nodes`, `relationships`, `length` | phase_3 | supported | supported | supported | supported | Supported over bounded named relationship path values in final `RETURN`. |
| SQL/PGQ typed adapter seam | phase_3 | n/a | supported | supported | supported | Internal hook-targeted adapter only; no SQL text parser or public SQL/PGQ API. |
| `CREATE` registered node rows | phase_2 | supported | supported | supported | supported | PostgreSQL-first, registered labels only; creating relationship rows is outside the current write subset. |
| `SET` mapped properties | phase_2 | supported | supported | supported | supported | Single-node mapped property update with PostgreSQL type checks and row locking. |
| `DELETE` mapped relationships | phase_2 | supported | supported | supported | supported | Single directed relationship backed by a registered edge row; endpoint nodes are not cascaded. |
| `REMOVE` property/label | phase_4 | supported | supported | supported | supported | Single-node mapped property removal is supported for scalar columns and registered dotted JSONB paths; label removal is an explicit unsupported-shape rejection. |
| `DETACH DELETE` | phase_4 | supported | supported | supported | supported | Single-node mapped detach delete is supported when incident relationships are backed by registered edge row tables. |
| `MERGE` | phase_4 | supported | supported | supported | supported | Single-node mapped merge is supported by registered primary-key identity with lazy `ON CREATE`/`ON MATCH` branches and read-before-write locking. |
| openCypher compatibility surface | phase_4 | supported | supported | supported | supported | `graph.cypher()` accepts the overlapping supported subset through the shared IR and exposes a separate no-Neo4j-claim compatibility matrix. |
| GQL DDL/schema creation | out_of_scope | optional | required | reject | required | Do not create arbitrary tables from GQL. |

## Pre-Code Baselines

Baseline results are tracked in `todo/regression-baseline-2026-05-29.md`.
Before coding starts, compare against:

- `cargo test --features pg17`
- `cargo pgrx test pg17`
- `cargo bench --features pg17 --bench bfs_bench -- --save-baseline pre_gql_mutable_overlay`
- `sandbox/run_benchmarks.sh panama --yes`
- `sandbox/run_benchmarks.sh ldbc --yes`
- `graph/tests/heavy/measure_build_rss.sh`
- `graph/tests/heavy/measure_mmap_pss.sh` on Linux only

Do not begin graph-language or mutable-overlay implementation without either
refreshing these baselines or explicitly recording why a baseline could not be
captured on the current host.

Policy note: `sfw` is only required for dependency-changing package-manager
operations such as install, fetch, add, and update. Direct `cargo` is allowed for
build, test, format, and benchmark commands.

**Freshness (2026-05-30):** the baselines were captured 2026-05-29 at commit
`0574e6b`, which is still `HEAD`, so they are current for Phase 1A start. The
mmap PSS script is the only outstanding baseline (Linux-only; this is a macOS
host) and must be captured on Linux before any claim about shared
page-cache cost — it does not gate Phase 1.

## Resolved Decisions (formerly Open Questions)

These were the four open questions plus the design points Codex flagged. Each is
now decided with justification. Phase-2-specific decisions are also captured as
"Decision Records" below so they are not lost between phases.

### Q1. Row locks for GQL updates/deletes (Phase 2)

**Decision:** GQL writes are issued as ordinary parameterized SPI
`INSERT`/`UPDATE`/`DELETE` against the registered source tables and inherit
PostgreSQL's standard row locking automatically. The narrow Phase 2 write set
(insert one mapped node, update one mapped property, delete one mapped edge row)
is exactly one DML statement per write with **no read-modify-write in Rust**, so
no explicit `SELECT ... FOR UPDATE` is required beyond what the DML already
takes. **Justification:** PostgreSQL already implements correct row locking and
MVCC; the lowest-risk path is to not reinvent it and to keep Phase 2 writes to
single statements. Read-before-write semantics (`MERGE`/upsert) are the only
case needing explicit `FOR UPDATE` / `ON CONFLICT`, and they are deliberately
deferred to Phase 4.

### Q2. Type flexibility before requiring `jsonb` (Phase 1 read / Phase 2 write)

**Decision:** The MVP supports only the closed set of scalar PostgreSQL types
the engine's typed `FilterCondition` set and catalog already model — integer /
bigint / numeric, boolean, dictionary-encoded text, uuid, date, timestamptz, and
SQL `NULL`. GQL scalar literals and JSONB parameters map onto these. Mixed-type
lists, maps, and open/dynamic property bags require a `jsonb` source column and
are deferred to Phase 3. A GQL predicate or `RETURN` targeting a non-modeled
type is a typed `UnsupportedFeature`/`UnsupportedPropertyType` rejection, never a
silent coercion. **Justification:** the engine already has a closed,
well-tested typed-filter set with fast filter-index pushdown; aligning GQL types
to it avoids inventing a parallel type system and keeps execution on the fast
path. JSONB-backed dynamic properties are a deliberate Phase 3 expansion.

### Q3. SQL/PGQ frontend timing

**Decision:** SQL/PGQ is an **adapter milestone (Phase 3), not a day-one
frontend.** Day one ships only the GQL frontend. The IR boundary is, however,
designed **frontend-neutral from day one** so SQL/PGQ lowering can be added
without reshaping the IR, and SQL/PGQ gets **no parser directory** — it lowers
into `graph/src/query/` through an adapter. **Justification:** building two
frontends before either executes doubles parser/fuzz surface and risks shaping
the IR around a speculative second consumer; PostgreSQL's own SQL/PGQ support is
still maturing, so the adapter target is a moving spec. Keep the IR neutral but
do not write the SQL/PGQ parser yet.

Status, 2026-05-31: the internal typed adapter seam exists in
`graph/src/query/`. It lowers eligible PostgreSQL-owned typed graph-pattern
shapes into the shared IR. It is not a SQL parser and is not a public API.

### Q4. openCypher positioning

**Decision:** **Not promised until GQL/SQL-PGQ are stable.** When added (Phase
4) it is a **separate function (`graph.cypher()`) with a separate compatibility
matrix**, lowering into the same IR. The `graph/src/cypher/` and
`sql_facade/cypher.rs` modules are **removed from the initial layout and must
not be scaffolded before Phase 4.** **Justification:** scaffolding Cypher early,
or promising it, implies Neo4j-shaped compatibility we cannot yet honor and
invites "is this Neo4j?" confusion. Deferring keeps the public surface honest;
see `phase-4-advanced-writes-opencypher-design.md`.

Status, 2026-06-01: the optional compatibility surface is present as
`graph.cypher()`, `graph.cypher_explain()`, and `graph.cypher_compatibility()`.
It accepts only the supported overlap with pgGraph's GQL subset, rejects
unmappable Cypher-only constructs, and does not claim Neo4j compatibility.

### Q5. Parser implementation strategy

**Decision:** handwritten lexer + recursive-descent parser + Pratt `WHERE`
expression parser; no generator, no external parser crate. Recorded in
`architecture-plan.md` → "Parser Implementation Strategy". **Justification:**
precise spans/diagnostics, minimal pgrx-free dependency surface for untrusted
input, simple totality + fuzzing, incremental grammar growth, house style.

## Decision Records (promoted from Blind Spots — required before coding the relevant phase)

### DR-1. Per-backend vs shared committed projection — **DECIDED: per-backend** (gates Phase 2)

Keep the existing backend-local engine + durable sync-log catch-up model. A
shared committed projection would require major new shared-memory infrastructure
(shared CSR, cross-backend invalidation, cross-backend locking) and is explicitly
out of scope. Transaction-local overlays layer on top of the per-backend model.
**Justification:** matches today's runtime; lowest correctness/memory risk;
PostgreSQL remains the cross-backend source of truth via the sync log. See
"Graph MVCC Direction" above.

### DR-2. Memory-limit / over-limit behavior — **DECIDED: statement-scoped abort, no spill** (gates Phase 2)

Exceeding any transaction-delta limit (delta nodes/edges/properties or overlay
memory) aborts the **current GQL statement** with a typed memory-limit error and
leaves the surrounding transaction alive so the app can `ROLLBACK` or retry with
a smaller write. The failed statement's backend-local overlay deltas are
discarded (always safe — the overlay is cache, not a source of truth). If SPI
writes for the statement were already issued before the limit tripped, the
statement error propagates and the user must roll back; the overlay is never
left inconsistent with committed PostgreSQL state. **No spill-to-disk** for
overlay state, ever (consistent with the non-goal of any durable graph store).
**Justification:** statement-level abort mirrors PostgreSQL's own `work_mem`-style
behavior, maximizes app recovery latitude, and avoids killing unrelated work in
a multi-statement transaction.

### DR-3. Existing SQL API behavior on dirty mutable projections — **DECIDED per API** (gates Phase 2)

No existing SQL API may silently return stale topology on a dirty overlay.
Per-API:

| API (file) | Phase 2 behavior |
|---|---|
| `graph.traverse()` / BFS (`bfs.rs`) | **Overlay-aware** (already consumes deltas). |
| Aggregation (`sql_aggregation.rs`) | **Overlay-aware** (already consumes deltas). |
| `graph.shortest_path()` (`path_finder.rs`) | **Overlay-aware** through `NeighborSource`. |
| `graph.weighted_shortest_path()` (`path_finder.rs`) | **Reject dirty edge overlay** with `PG018` because pending mutations do not carry weights. |
| `graph.connected_components()` (`connected_components.rs`) | **Overlay-aware** through `NeighborSource`. |
| `graph.search()` / `traverse_search` (`sql_search.rs`) | Source-table search is not a graph-topology read; `traverse_search` inherits overlay-aware traversal. |

`graph.status()` / `graph.sync_health()` must expose the dirty flag and the
read-only/rejection reason. **Justification:** correctness over convenience;
reject-loudly beats silent staleness; the two already-overlay-aware paths show
the target end state, the rest are routed through the shared neighbor
abstraction incrementally.

### DR-4. ACL / RLS / tenant enforcement — **DECIDED: execution-time via existing paths** (gates Phase 1C)

ACL is enforced by calling `acl::check_table_acl(table_oid)` for every touched
source table; RLS is preserved by routing all value access and hydration through
SPI so PostgreSQL applies row security; tenant scope reuses the existing tenant
column + bitmap path. These are **not** modeled as catalog-snapshot fields — the
snapshot only carries the `TableOid` needed to make the ACL call.
**Justification:** reuse the audited enforcement paths rather than
re-implementing authorization in the graph layer.

### DR-5. SQLSTATE taxonomy — **DECIDED: typed GQL facade SQLSTATEs** (gates public `graph.gql()`)

Read-only GQL maps frontend and execution failures to stable `GraphError`
variants at the SQL facade boundary:

| Category | SQLSTATE | Variant |
|---|---:|---|
| Syntax | `PG013` | `GqlSyntax` |
| Unsupported feature | `PG014` | `GqlUnsupported` |
| Semantic/bind | `PG015` | `GqlSemantic` |
| Parameter | `PG016` | `GqlParameter` |
| Execution/cardinality | `PG017` | `GqlExecution` |
| Access denial | `PG002` | `AclDenied` |
| Write on read-only projection | `PG012` | `ReadOnly` |
| Memory limit | `PG001` | `Oom` |
| Internal | `XX000` | `Internal` |

Type-mismatch and schema-violation cases in the current read-only subset are
reported through semantic or execution categories, depending on whether they
are detected during binding or row evaluation. Future write-specific schema
violations may add narrower SQLSTATEs only if callers need to distinguish them
programmatically.
