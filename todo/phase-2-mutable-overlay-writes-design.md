# Phase 2 Design: Mutable Overlay And Narrow GQL Writes

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

Phase 2 adds the `mutable_overlay` projection mode, an overlay-aware neighbor
abstraction, and a deliberately narrow set of GQL writes. Depends on Phase 1.
Hard decisions are recorded as **DR-1…DR-5** in
`mutable-graph-projections-todo.md`.

## 0. Entry conditions

- Phase 1 read-only GQL green and publicly exposed.
- DR-1 (per-backend), DR-2 (statement-scoped memory abort, no spill), DR-3
  (per-API dirty behavior), DR-5 (SQLSTATE) accepted.
- Edge-type `u8` 255-ceiling (known-issues P2) wired into the write path as a
  hard, cleanly-erroring limit (`GraphError::EdgeTypeLimit` exists).

## 1. What already exists (extend, don't replace)

From `engine.rs`: `Engine.edge_buffer: Vec<EdgeMutation>`,
`EdgeMutation { source, target, type_id, kind: MutationKind }`,
`Engine.resolution_delta: ResolutionDeltaIndex`, `ReadOnlyReason`
(`MemoryLimit`/`EdgeBufferFull`), `push_edge_mutation()` with a buffer limit.
`bfs.rs` and `sql_aggregation.rs` already merge these overlays. This is the
foundation; Phase 2 generalizes it into a transaction-scoped delta and routes
the remaining algorithms through one neighbor trait.

## 2. Neighbor abstraction (slice 2A, before any write)

```rust
// graph/src/projection/neighbors.rs
pub trait NeighborSource {
    fn neighbors(&self, node: u32, dir: Direction) -> NeighborIter<'_>;
    fn is_tombstoned(&self, node: u32) -> bool;
}

pub struct CsrNeighbors<'a> { fwd: &'a EdgeStore, rev: &'a EdgeStore }      // clean fast path
pub struct OverlayNeighbors<'a> {                                          // dirty path
    base: CsrNeighbors<'a>,
    added: &'a HashMap<u32, SmallVec<[DeltaEdge; 4]>>,
    tombstones: &'a TombstoneSet,
}
```

`neighbors()` for the overlay yields `base slice (skip tombstoned) ++ added`.
DR-3 routing: `path_finder.rs`, `connected_components.rs`, `sql_search.rs` either
consume `NeighborSource` or reject a dirty overlay with a stable SQLSTATE. A
clean `mutable_overlay` must benchmark within noise of `csr_readonly` via
`CsrNeighbors`.

### Slice 2A implementation status - 2026-05-31

- Added `graph/src/projection/neighbors.rs` with `NeighborSource`,
  `CsrNeighbors`, and `OverlayNeighbors`.
- Routed BFS/DFS, unweighted shortest path, and connected components through
  the shared neighbor-source abstraction.
- Kept source-table search as a source-row SQL lookup; `traverse_search`
  inherits the overlay-aware traversal path.
- `graph.weighted_shortest_path()` rejects dirty edge overlays with `PG018`
  until `graph.vacuum()` or `graph.maintenance()` merges edge weights into CSR.
- Tests added:
  - clean overlay equivalence property test;
  - overlay insert/delete neighbor tests;
  - engine shortest-path overlay insert/delete tests;
  - connected-component dirty-overlay test;
  - weighted shortest-path dirty-overlay rejection test.

## 3. Transaction delta model (slice 2B)

```rust
// graph/src/projection/tx_delta.rs
pub struct TxGraphDelta {
    pub added_nodes:    Vec<AddedNode>,                 // table_oid + pk + assigned u32
    pub deleted_nodes:  TombstoneSet,                   // bitset over base u32
    pub added_edges:    HashMap<u32, SmallVec<[DeltaEdge; 4]>>,
    pub deleted_edges:  EdgeTombstoneSet,               // (src,dst,type) tombstones
    pub property_updates: Vec<PropertyUpdate>,          // node u32 + col + typed value
    pub filter_updates:   Vec<FilterUpdate>,
    pub tenant_updates:   Vec<TenantUpdate>,
    pub resolution_updates: ResolutionDeltaIndex,       // reuse existing type
}
pub struct DeltaEdge { pub target: u32, pub type_id: u8, pub weight: Option<u32> }
```

`TxGraphDelta` is backend-local and lives for one transaction. Reads merge it via
`OverlayNeighbors`. Reverse adjacency: maintain forward+reverse delta together,
or mark reverse stale and reject `expand-in` on a dirty overlay (start with
mark-stale; upgrade if benchmarks demand).

### Slice 2B infrastructure status - 2026-05-31

- Added `ProjectionMode` parsing plus `graph.default_projection_mode` and
  `graph.mutable_enabled`.
- Added `graph.build(mode := 'csr_readonly' | 'mutable_overlay')`; mutable mode
  is rejected until `graph.mutable_enabled = on`, and queued builds persist the
  selected projection mode for the background worker.
- Added `TxGraphDelta` storage and backend-local transaction/subtransaction
  callbacks that clear deltas at transaction end and track nested
  subtransaction depth. Mutable edge-delta recording rejects writes while a
  subtransaction is active; top-level deltas are preserved across unrelated
  subtransaction aborts.
- Routed transaction-local edge insert/delete overlays into traversal,
  unweighted shortest path, and connected components. Weighted shortest path
  rejects dirty transaction edge overlays until committed weights are folded
  into CSR.
- Routed read-only GQL relationship expansion through the same overlay-aware
  neighbor path and covered internal transaction edge inserts/deletes from
  SQL-visible tests.
- Added `tx_delta_lifecycle.sh` heavy coverage for the internal transaction
  edge overlay path: read-your-own-writes, rollback discard, commit cleanup,
  concurrent backend isolation, and trigger-sync catch-up from source-table
  updates.
- Added `tx_delta_crash_recovery.sh` heavy coverage showing an uncommitted
  transaction edge overlay disappears after postmaster crash/reload while the
  persisted base graph still reloads.
- Exposed projection mode and empty transaction-delta counters through
  `graph.status()` and `graph.sync_health()`.
- Routed public GQL node `CREATE` into `TxGraphDelta` as an added-node delta
  after PostgreSQL accepts the mapped source-table insert.

## 4. Transaction callbacks (slice 2B) — NEW infrastructure

There are no xact callbacks in the codebase today. Register one via
`pg_sys::RegisterXactCallback` at extension init:

- `XACT_EVENT_PRE_COMMIT` / `COMMIT`: clear local deltas; committed visibility
  flows through the existing sync-log replay path (DR-1) — do **not** promote
  deltas into a shared structure.
- `XACT_EVENT_ABORT`: discard local deltas.
- Subtransaction (`RegisterSubXactCallback`): maintain a nested delta stack
  **or** reject write clauses inside `SAVEPOINT`/PL subtxn contexts until
  designed (start with reject + stable SQLSTATE).

Wrap the callback body in the existing panic boundary (`safety.rs`); a panic in
a C callback is UB.

## 5. Write grammar additions (slices 2C–2E)

```ebnf
query        = [ match_clause ] , [ where_clause ] ,
               { write_clause } , [ return_clause ] , ... ;
write_clause = create_clause | set_clause | delete_clause ;

create_clause= "CREATE" , node_pat ;                    (* registered label only *)
set_clause   = "SET" , property_ref , "=" , (literal | param) ;  (* one mapped prop *)
delete_clause= "DELETE" , var ;                         (* a bound relationship var *)
```

Token additions: `Create`, `Set`, `Delete`. Phase-2 constraints (bind-time
rejection otherwise): `CREATE` only for labels mapped to a registered table;
`SET` only on a typed (Q2) mapped column; `DELETE` only of a relationship
variable bound to a registered edge row; no `MERGE`/`REMOVE`/`DETACH DELETE`
(Phase 4); no multi-row pattern writes.

## 6. AST + IR + physical write operators

AST (`gql/ast.rs` additions): `CreateClause { node: NodePat }`,
`SetClause { target: PropertyRef, value: Operand }`,
`DeleteClause { var: Ident }`; `Query.writes: Vec<WriteClause>`.

Logical (`logical_plan.rs`): `CreateNode`, `CreateEdge`, `SetProperty`,
`DeleteEdge`; `LogicalPlan.is_write = true`; `supported_modes` excludes
`csr_readonly` for any write (bind-time `WriteOnReadOnly` rejection there).

Physical (`physical_plan.rs`): `SpiInsertNode`, `SpiUpdateProperty`,
`SpiDeleteEdge`, `ApplyTxDelta`. Execution order per write (DR-1, Q1):
parameterized SPI DML first → on PostgreSQL success → `ApplyTxDelta` records the
backend-local delta. Single-statement DML inherits row locking; no explicit
`FOR UPDATE`.

## 7. GUCs (config.rs, mirror existing `GucSetting` pattern)

```rust
pub static DEFAULT_PROJECTION_MODE: GucSetting<Option<CString>> = ...; // "csr_readonly"
pub static MUTABLE_ENABLED:         GucSetting<bool>  = GucSetting::new(false);
pub static MAX_TX_DELTA_NODES:      GucSetting<i32>   = GucSetting::new(100_000);
pub static MAX_TX_DELTA_EDGES:      GucSetting<i32>   = GucSetting::new(100_000);
pub static MAX_OVERLAY_MEMORY_MB:   GucSetting<i32>   = GucSetting::new(256);
pub static COMPACTION_THRESHOLD:    GucSetting<i32>   = GucSetting::new(50_000);
```

Defaults preserve current behavior: `mutable_overlay` is opt-in. Over-limit ⇒
DR-2 statement-scoped abort (reuse `ReadOnlyReason`, add `OverlayLimit`).

## 8. Projection-mode selection at build (slice 2B)

`graph.build(mode := 'csr_readonly' | 'mutable_overlay')`. Mode is recorded in
engine state and surfaced in `graph.status()`. `csr_readonly` rejects writes;
`mutable_overlay` enables the overlay path.

## 9. Observability (slice 2F)

Extend `EngineStatus` (`types.rs`) + `graph.status()`/`graph.sync_health()`:
projection mode, overlay dirty flag, added/deleted node+edge delta counts,
tombstone count, overlay memory estimate, compaction-recommended, read-only/
rejection reason.

### Slice 2F implementation status - 2026-05-31

- Added public status and sync-health fields for overlay tombstone count,
  estimated overlay memory, and durable-overlay compaction recommendation.
  Overlay dirty state remains derivable from `edge_buffer_used` plus
  `tx_delta_dirty`.
- Added `graph.max_tx_delta_nodes`, `graph.max_tx_delta_edges`,
  `graph.max_overlay_memory_mb`, and `graph.compaction_threshold`.
- Mapped GQL writes fail with `PG019` before source-table DML when node or edge
  delta limits would be exceeded. Transaction-delta recording also checks the
  overlay memory cap before mutating backend-local state.
- Maintenance recommendations now include explicit compaction recommendations.

## 10. Compaction (slice 2F)

Fold overlays into a fresh immutable CSR via the normal build/rebuild path when
delta size / tombstones / memory cross `COMPACTION_THRESHOLD`. Per-backend heap
scales with churn, not graph size.

## 11. PR slices (TDD order)

- **2A — NeighborSource refactor.** Trait + `CsrNeighbors`/`OverlayNeighbors`;
  route `path_finder`/`connected_components`/`sql_search` through it or reject
  dirty. Tests: proptest neighbor equivalence (clean overlay ≡ CSR); benchmark
  clean overlay ≈ `csr_readonly`. **No writes yet.**
- **2B — Overlay storage + tx callbacks + mode selection + read-your-own-writes
  on reads.** `TxGraphDelta`, xact/subxact callbacks, `build(mode:=)`. Tests:
  rollback discards deltas; concurrent sessions isolated; out-of-band SQL
  catch-up via sync log; crash/reload rebuilds (doesn't trust) overlay.
- **2C — `CREATE` mapped node.** SPI-first insert + delta. Tests:
  read-your-own-writes, ACL/RLS/tenant, unregistered-label rejection, and
  read-only projection rejection. Public docs keep edge-row creation outside
  the current write subset.
- **2D — `SET` mapped property.** Tests: typed-column only, type-mismatch
  rejection, filter-index delta visibility.
- **2E — `DELETE` mapped edge.** Tests: tombstone reduces neighbors, no cascade,
  reverse-direction consistency.
- **2F — Compaction + observability + memory limits.** Tests: compaction
  equivalence (CSR+overlay ≡ rebuilt CSR), DR-2 statement-scoped abort, status
  row shape, heavy memory caps.

Phase 2 complete only when read-your-own-writes, rollback discard, concurrent
isolation, out-of-band sync catch-up, and dirty-overlay behavior are all proven.
