# End-State Spec: openCypher, GQL, SQL/PGQ, And Mutable Graph Projections

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

## Purpose

This document describes the complete intended end state, not the delivery
sequence. It assumes pgGraph has matured from its current SQL-function-only
interface into a PostgreSQL-native graph projection engine with graph query
language frontends and selectable execution runtimes.

## Product Positioning

pgGraph remains a PostgreSQL extension for graph querying over existing
PostgreSQL data. PostgreSQL source tables remain authoritative for storage,
constraints, WAL, MVCC, ACLs, RLS, backup/restore, and crash recovery.

The completed system provides:

- an openCypher-compatible query surface;
- a GQL-compatible query surface where feasible;
- an adapter path for PostgreSQL SQL/PGQ once PostgreSQL exposes stable graph
  query support;
- a shared graph query planner and execution runtime beneath those frontends;
- selectable graph projection modes for read-heavy and mutable workloads.

pgGraph is not a separate graph database and does not introduce a second
durable graph store. It is a derived, rebuildable, PostgreSQL-governed graph
execution layer.

## User-Facing Capabilities

Users can register relational tables and relationships as graph metadata, then
build a graph projection in one of two runtime modes:

```sql
SELECT graph.build(mode := 'csr_readonly');
SELECT graph.build(mode := 'mutable_overlay');
```

The exact SQL may change, but the user-facing choice must remain explicit:

- `csr_readonly`: optimized for fast bounded traversal, path queries, search,
  and analytics over mostly static or batch-refreshed topology.
- `mutable_overlay`: optimized for graph reads and writes where changes should
  be visible within PostgreSQL transaction boundaries without requiring a full
  rebuild after every write.

Users can query through the existing SQL APIs:

```sql
SELECT * FROM graph.traverse(...);
SELECT * FROM graph.shortest_path(...);
SELECT * FROM graph.search(...);
```

Users can also query through openCypher:

```sql
SELECT *
FROM graph.cypher(
  query := $$MATCH (u:users {id: $id})-[:follows]->(v:users)
             RETURN v.id, v.name
             ORDER BY v.name
             LIMIT 20$$,
  params := '{"id": "u1"}'::jsonb,
  hydrate := true
);
```

The openCypher call returns JSONB rows initially:

```sql
graph.cypher(query text, params jsonb default '{}', hydrate boolean default true)
RETURNS TABLE (row jsonb)
```

Users can inspect the plan:

```sql
SELECT *
FROM graph.cypher_explain(
  query := $$MATCH (u:users)-[:follows]->(v:users) RETURN v$$,
  params := '{}'::jsonb
);
```

The intended GQL surface is parallel after the shared planner is proven:

```sql
SELECT *
FROM graph.gql(
  query := $$...$$,
  params := '{}'::jsonb,
  hydrate := true
);
```

The SQL/PGQ integration target is an adapter into the same planner and runtime.
Where PostgreSQL exposes SQL/PGQ graph table definitions or graph pattern
execution hooks, pgGraph should map eligible graph patterns to pgGraph
projections. General unsupported graph queries should remain PostgreSQL's
responsibility.

## Query Language Compatibility

The end state aims for a documented openCypher-compatible subset that can grow
toward broad openCypher coverage. The public contract must always be a
compatibility matrix, not an unqualified "full Cypher" claim.

Supported end-state openCypher categories should include:

- `MATCH`
- `OPTIONAL MATCH`
- `WHERE`
- `RETURN`
- `WITH`
- `ORDER BY`
- `SKIP`
- `LIMIT`
- `DISTINCT`
- path variables
- variable-length relationships with bounded execution
- node labels mapped to registered source tables or aliases
- relationship types mapped to registered edge labels
- property predicates over registered columns
- parameters through JSONB
- basic scalar, list, map, boolean, and path expressions
- aggregations such as `count`, `sum`, `avg`, `min`, `max`, and `collect`
- selected path functions such as `nodes(path)`, `relationships(path)`, and
  `length(path)`

Supported end-state write categories may include:

- `CREATE`
- `SET`
- `DELETE`
- `REMOVE`
- `DETACH DELETE`
- carefully constrained `MERGE`

Writes are only supported where the planner can prove a mapping to registered
PostgreSQL source tables and edge tables. Creating new PostgreSQL tables,
labels, or schema objects from openCypher is out of scope unless a separate DDL
contract is designed.

GQL support should target standards-aligned graph pattern and graph operation
semantics where they can be mapped to pgGraph's property graph model. GQL should
not force the engine to abandon PostgreSQL's table-authoritative model.

## Property Graph Model

The end state has a canonical property graph model shared by SQL APIs,
openCypher, GQL, and SQL/PGQ adapters.

Core concepts:

- graph projection: a named or active derived graph view over registered source
  tables;
- node label: a registered source table or a catalog alias for one;
- relationship type: a registered edge label;
- node identity: source table OID plus primary key, translated to an internal
  dense `u32` node index for execution;
- relationship identity: registered edge row identity where available, or a
  stable derived edge coordinate for read-only relationships;
- property: a PostgreSQL source column exposed through catalog metadata;
- path: an ordered sequence of graph coordinates, not internal node indexes.

Property typing must be explicit:

- fixed typed columns keep PostgreSQL types;
- dynamic map/list values require `jsonb`-backed property columns or documented
  rejection;
- null and missing-property semantics must be documented separately;
- mixed-type Cypher lists require `jsonb` or rejection in typed-column mode.

## Runtime Modes

### `csr_readonly`

The committed base graph is immutable CSR:

- forward adjacency is compact and cache-friendly;
- persisted `.pggraph` sections can be mmap-backed and shared through the OS
  page cache;
- validation remains section/offset/length/CRC based;
- topology mutations require sync overlays, vacuum, maintenance, or rebuild.

This mode is optimized for predictable read speed and minimal per-backend heap
growth.

### `mutable_overlay`

The committed base still remains immutable CSR. The mutable layer is an overlay
on top of it:

```text
neighbors(node) =
  base_csr_slice(node).filter(not tombstoned)
  + delta_added_edges(node)
```

The mutable layer contains only deltas:

- added nodes;
- deleted/tombstoned nodes;
- added edges;
- deleted/tombstoned base edges;
- property/filter updates;
- tenant membership updates;
- resolution-index updates;
- edge-label registry updates where allowed.

Small transaction-local overlays use simple maps, vectors, small vectors, and
bitsets. Arena or slab adjacency is only used for larger, longer-lived mutable
regions where insert volume and lifetime justify the complexity.

The mutable overlay is never the durable source of truth. It is a cache derived
from PostgreSQL table changes and backend-local transaction state.

## Write Semantics

All graph writes target PostgreSQL source tables first.

For openCypher/GQL writes:

```text
query text
  -> parse
  -> semantic binding
  -> logical write plan
  -> parameterized PostgreSQL INSERT/UPDATE/DELETE through SPI
  -> backend-local projection delta
  -> commit/rollback handling
```

Rules:

- PostgreSQL constraints, defaults, triggers, ACL, RLS, and WAL apply first.
- The projection reflects successful writes only after PostgreSQL accepts them.
- A transaction sees its own uncommitted graph writes through backend-local
  deltas.
- Other sessions do not see those deltas until commit and sync-log replay.
- Rollback discards backend-local deltas.
- Out-of-band SQL writes flow through the existing durable trigger sync log and
  replay path.

## Consistency And Freshness

The completed system preserves PostgreSQL transaction boundaries.

Visibility model:

- each backend owns its own active `Engine`;
- committed projection state is backend-local and caught up through durable sync
  replay;
- uncommitted graph changes are backend-local transaction deltas;
- read-your-own-writes is implemented by merging local transaction deltas into
  graph reads;
- cross-backend visibility is through committed PostgreSQL state and sync-log
  replay, not shared mutable Rust state.

Freshness surfaces expose:

- projection mode;
- sync mode;
- applied sync ID;
- max sync-log ID;
- pending sync rows;
- schema freshness;
- optional LSN/XID fields if explicitly captured and timeline-safe;
- mutable overlay size;
- tombstone count;
- dirty/delta memory;
- compaction/rebuild recommendation.

## Persistence And Restart

Read-only CSR projections may use persisted `.pggraph` artifacts.

Mutable overlay state is not persisted as authoritative storage. On restart,
mutable state is rebuilt from PostgreSQL source tables and committed sync state.

Optional fast snapshots are allowed only if:

- they are treated as cache snapshots;
- they are validated against PostgreSQL freshness markers;
- missing committed changes are replayed safely;
- invalid snapshots are discarded and rebuilt.

No writable mmap graph store is part of the end state.

## Existing SQL API Behavior

Existing SQL functions continue to work:

- `graph.traverse()`
- `graph.shortest_path()`
- `graph.weighted_shortest_path()`
- `graph.search()`
- `graph.traverse_search()`
- component APIs
- aggregation APIs
- admin/status/maintenance APIs

Each function must either:

- execute against the selected projection mode with correct overlay semantics;
  or
- reject execution with a stable SQLSTATE and explanation when the current
  projection state is dirty and the algorithm is not overlay-aware.

No existing API should silently return stale topology when mutable overlays are
present.

## Safety And Limits

The completed system has explicit safety limits:

- query text length;
- AST node count;
- variable count;
- pattern count;
- maximum variable-length path bound;
- maximum rows;
- maximum hydrated rows;
- maximum transaction delta nodes/edges/properties;
- maximum overlay memory;
- maximum arena/slab memory where used;
- compaction thresholds;
- read-only fallback states.

The parser must be total: malformed input returns typed errors, never panics.

No new unsafe code is expected for graph language parsing, planning, or mutable
overlay storage. Any future unsafe must be justified, isolated, documented, and
reviewed under the repository's existing unsafe/panic boundary rules.

## Observability

`graph.status()` and `graph.sync_health()` expose projection-mode state:

- current projection mode;
- mutable enablement;
- dirty overlay count;
- transaction delta count where visible to the current backend;
- tombstone count;
- overlay memory estimate;
- base CSR memory estimate;
- compaction recommended;
- maintenance recommended;
- read-only state and reason;
- unsupported algorithm state where applicable.

`graph.cypher_explain()` and eventual `graph.gql_explain()` expose:

- parse stage;
- semantic binding summary;
- logical plan;
- physical plan;
- chosen runtime;
- fallback/rejection reasons;
- estimated row and memory bounds where available.

## Documentation End State

Public docs no longer say "no new query language" without qualification. They
explain:

- pgGraph remains PostgreSQL-first;
- openCypher/GQL support is optional and projection-backed;
- compatibility is a documented subset;
- PostgreSQL source tables remain authoritative;
- read-only CSR and mutable overlay modes have different tradeoffs;
- writes are Postgres-first;
- unsupported clauses fail clearly.

## Non-Goals

The end state does not include:

- mutable in-place CSR;
- writable mmap graph storage;
- graph-native durability outside PostgreSQL;
- full graph MVCC in Rust;
- creating arbitrary PostgreSQL schemas through Cypher/GQL DDL;
- distributed graph execution;
- a second graph database hidden inside PostgreSQL.
