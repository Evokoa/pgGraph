# Working TODO: Cypher And Mutable Graph Projections

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

## Goal

Explore Cypher/GQL/SQL-PGQ support and mutable graph projections without
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

## Existing Contract Conflicts

This branch intentionally proposes work that conflicts with current public
positioning. These must be resolved before any public Cypher/GQL/SQL-PGQ API is
merged:

- `docs/roadmap.mdx` currently lists Cypher, Gremlin, SPARQL, and GQL as
  non-goals.
- `docs/user_guide/limitations-and-fit.mdx` points users needing Cypher,
  Gremlin, SPARQL, or GQL compatibility to another database.
- `docs/user_guide/index.mdx` says pgGraph does not introduce a new graph query
  language.
- `docs/contributor_guide/architecture.mdx` says SQL is the public API.
- `graph/src/lib.rs` says "No new query language."

The existing private `.agents/private/cypher-support-plan.md` is the baseline
for Cypher parser/planner work. This TODO extends that plan with a possible
future write-capable mutable projection path, but the two documents must be
reconciled before implementation starts.

## Scope Gates

- Do not expose public Cypher functions until the compatibility contract and
  docs positioning are updated together.
- Do not add Cypher writes until read-only Cypher has a parser, semantic binder,
  logical plan, physical plan, SQL facade, tests, and docs.
- Do not add mutable projection writes until transaction-local overlays,
  rollback behavior, out-of-band SQL sync, and memory-limit behavior are proven.
- Resolve or explicitly reprioritize critical pre-launch safety/correctness
  items before expanding the attack surface with arbitrary query text and graph
  writes.

## Architecture Decisions

- Cypher writes target PostgreSQL source tables first. Projections react to
  PostgreSQL state and must not become a second durable source of truth.
- Mutable projection writes are transaction-scoped. A transaction must read its
  own Cypher writes and rollback must discard projection deltas.
- The first public Cypher milestone should remain read-only and follow
  `.agents/private/cypher-support-plan.md`.
- The first write-capable Cypher milestone is intentionally narrow:
  - reads: `MATCH (n)-[r]->(m) WHERE ... RETURN ...`
  - writes: `CREATE (n:Label {props})`, `MATCH (n) SET n.prop = val`, and
    `MATCH (n)-[r]->(m) DELETE r`
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
- Introduce a shared planner layer. Cypher, existing SQL functions, future GQL,
  and future SQL/PGQ adapters should compile into common logical and physical
  graph operators.
- MVP Cypher `CREATE (n:Label ...)` may only create rows for labels that map to
  registered source tables. Creating new labels/tables from Cypher is out of
  scope.
- Do not make CSR mutable. The committed base graph stays immutable CSR so it
  remains compact, cache-friendly, mmap-shareable, and easy to validate.
- Arenas/slabs are candidates for mutation deltas or a future fully resident
  mutable projection mode, not for the committed CSR base.
- Cypher and mutability are orthogonal. Read-only Cypher should ship on the
  existing immutable CSR path before mutable projection writes.

## Design Checkpoints

- Define the public meaning of a graph projection mode.
- Define the public Cypher surface and how it maps to PostgreSQL tables,
  graph catalog metadata, and projection runtimes.
- Decide exactly which Cypher reads are legal on read-only CSR projections and
  which Cypher writes require mutable projections.
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
- Define Cypher-to-PostgreSQL type mapping, especially dynamic properties,
  lists, mixed lists, maps, nulls, and missing properties.
- Define how existing SQL APIs behave on mutable projections. Avoid silently
  degrading `graph.traverse()` users from CSR-speed behavior to a slower runtime
  without status, docs, or explicit mode visibility.
- Define how `graph.reset()`, `graph.auto_load`, `graph.load()`,
  `graph.vacuum()`, `graph.maintenance()`, backup/restore, and crash recovery
  behave for each projection mode.
- Define GUCs for projection mode defaults, mutable enablement, transaction
  delta limits, compaction thresholds, and memory caps.
- Define SQLSTATE policy for Cypher syntax, unsupported feature, semantic,
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
  dynamic edge-type creation from Cypher must account for this limit.
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
  mutable regions, not a prerequisite for Cypher writes.
- Algorithms: overlay-aware execution must cover traversal, shortest path,
  weighted shortest path, connected components, search/traversal hybrids, and
  aggregation where supported. Today the overlay gap is large, not marginal:
  only `bfs.rs` and `sql_aggregation.rs` consume the delta/edge_buffer overlays.
  `path_finder.rs` (shortest / weighted path), `connected_components.rs`, and
  `sql_search.rs` do not consume `resolution_delta` or `edge_buffer` at all, so
  the "make algorithms overlay-aware" step covers most of the algorithm
  surface, not a few stragglers.

## Blind Spots To Resolve

- Per-backend vs shared committed projection. The engine is backend-local
  today; deciding whether the mutable committed model stays per-backend
  (sync-log catch-up) or becomes shared-memory changes the locking, memory, and
  invalidation design fundamentally. Resolve this before MVCC design.
- Graph MVCC and concurrent transaction visibility.
- Out-of-band SQL mutations against registered source tables.
- Memory-limit behavior for large transactions and large mutation deltas.
- Whether mutable projection over-limit errors abort only the Cypher statement
  or the whole transaction.
- Whether any spill-to-disk path is allowed, and if so whether it is temporary
  and non-authoritative.
- Cypher dynamic property typing vs PostgreSQL's typed columns.
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

1. Reconcile product/docs contract.
2. Reconcile this TODO with `.agents/private/cypher-support-plan.md`.
3. Finish or explicitly defer critical pre-launch safety/correctness items.
4. Track A: add private Cypher parser/AST and fuzz tests.
5. Track A: add catalog binding and shared logical graph IR.
6. Track A: add physical operators for read-only Cypher over existing
   primitives.
7. Track A: expose read-only `graph.cypher()` / `graph.cypher_explain()` only
   after docs,
   SQLSTATEs, ACL/RLS, tenant scope, and tests are complete.
8. Track B: define one overlay-aware neighbor abstraction and route
   `path_finder`, `connected_components`, and other graph algorithms through it
   or explicitly reject dirty mutable projections.
9. Track B: design mutable projection storage and transaction-local delta
   overlays, starting with simple maps/vectors and evaluating arena/slab blocks
   only for larger long-lived deltas.
10. Track C: add narrow Cypher writes targeting PostgreSQL first, then projection
    update/invalidation.
11. Add GQL and SQL/PGQ parser/adapters once the shared IR is stable.

## Implementation Tracks

- Cypher parser and planner integration
- Cypher SQL function/API surface
- Shared logical graph IR
- Shared physical graph operators such as index scan, expand-out, expand-in,
  filter, project, hash join, update property, create node, and delete edge
- Projection catalog model
- Projection mode selection during graph build
- SQL read/write API shape
- Runtime mutation model
- Immutable CSR base plus mutable overlay storage design
- Simple transaction-local delta maps/vectors
- Optional arena/slab adjacency for larger long-lived mutable regions
- Backend-local transaction delta overlay
- Sync and invalidation flow
- Out-of-band SQL mutation capture
- Persistence or rebuild strategy
- Sync-id / SchemaState plus optional LSN/XID based projection freshness
  tracking
- Memory accounting and read-only fallback states
- OOM and transaction abort policy
- Cypher/PostgreSQL type mapping policy
- Mutable `ResolutionIndex`, `FilterIndex`, `NodeStore`, tenant bitmap, and
  edge label registry interactions
- Existing SQL API behavior on mutable projections
- `graph.status()`, `graph.sync_health()`, `graph.reset()`, `graph.load()`,
  `graph.auto_load`, `graph.vacuum()`, and `graph.maintenance()` behavior
- GUCs and SQLSTATE mapping
- Tests for Cypher reads, Cypher writes, graph writes, stale reads, rebuilds,
  rollback, concurrent transactions, out-of-band SQL writes, and crash/reload
  behavior
- Benchmarks against current SQL APIs, CSR traversal, and representative native
  graph workloads
- User and contributor documentation updates

## Test Plan

- Unit tests for lexer, parser, AST spans, semantic binding, logical plans,
  physical lowering, type mapping, and projection formatting.
- Fuzz tests for Cypher parser totality and unsupported-shape diagnostics.
- Proptests for mutable overlay invariants, active-node visibility,
  resolution/filter index consistency, and edge insert/delete reduction.
- pgrx SQL tests for read-only Cypher, write rejection on read-only
  projections, narrow write support on mutable projections, ACL/RLS, tenant
  scope, rollback, read-your-own-writes, concurrent sessions, and out-of-band
  SQL sync.
- Heavy tests for crash/reload, maintenance/vacuum interaction, memory-limit
  behavior, status/health row shapes, and function metadata drift.
- Benchmark gates to ensure read-only CSR traversal does not regress when the
  shared planner and mutable projection code are added.
- Docs/API drift checks for every public SQL signature, GUC, SQLSTATE, row
  shape, and behavior change.

## Open Questions

- What syntax should select projection mode during build or registration?
- What row locks are required for Cypher `SET` and `DELETE` to match
  PostgreSQL isolation semantics?
- How much type flexibility should the MVP allow before requiring `jsonb`
  property columns?
- Should GQL and SQL/PGQ be separate parser frontends from day one, or tracked
  as adapter milestones after the shared IR stabilizes?
- Should the proposed `graph.cypher(query, params, hydrate)` API from
  `.agents/private/cypher-support-plan.md` remain the public API, or should
  mutable writes require separate typed entry points?
- How should the project phrase compatibility: Cypher-inspired subset,
  openCypher subset, eventual comprehensive Cypher, GQL subset, or SQL/PGQ
  accelerator?
