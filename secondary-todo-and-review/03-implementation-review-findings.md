# Implementation Review — Findings, Ranked By Severity

Date: 2026-07-16
Scope: core engine (`engine.rs`, `bfs.rs`, `path_finder.rs`,
`connected_components.rs`, `persistence.rs`, `builder.rs`, `node_store.rs`,
`edge_store.rs`, `safety.rs`, `acl.rs`, `quote.rs`, `config.rs`, SQL facade)
plus the sync findings cross-referenced from `01-sync-architecture-review.md`.
Review only — no code changed.

## Overall Verdict

Nothing is "seriously wrong" with the architecture. The unsafe mmap layer is
genuinely defensive, error/SQLSTATE mapping is disciplined, there are no
`unwrap()`/`expect()`/`panic!` in production hot or unsafe paths, artifact
swaps are atomic, and the PostgreSQL-first write principle (AGENTS.md) is
honored — GQL `CREATE` performs `INSERT INTO … RETURNING` via SPI against the
registered table *before* recording the transaction-local graph delta
(`sql_facade/gql.rs:835-870`, then `tx_delta` at `:347-350`, `:2338`).

The serious findings are about the **security model of reads** and two
**concurrency/correctness seams**, not memory safety.

## HIGH

### H1 — RLS is not enforced on topology reads (confidentiality gap)

`graph.search()` is RLS-safe (re-queries source tables via SPI as the caller,
`sql_search.rs:171-182`). But `graph.traverse()`, `shortest_path()`,
`weighted_shortest_path()`, and `connected_components()` walk the in-memory
CSR and return `node_id` (source PK), depth, paths, and edge labels directly
(`engine.rs:678-777`, `873-953`, `1178-1212`). The only check on that path is
**table-level** `pg_class_aclcheck(ACL_SELECT)` (`acl.rs:52-68`). Row-level
security is never consulted.

The graph artifact is built by whoever runs `graph.build()` — typically an
owner/superuser who bypasses RLS — so it contains every row. A low-privilege
caller holding only table-level SELECT can then enumerate PKs, adjacency,
connectivity, and path existence of rows their RLS policy hides. Hydration
partially masks it (RLS-hidden rows hydrate as `node = NULL`,
`sql_facade/traversal.rs:217-228`) but `node_id` is always emitted, and
in-memory filter predicates (`bfs.rs:583`) let a caller probe hidden rows'
filter-column values.

Options (in increasing effort): document loudly as a boundary ("topology is
builder-scoped; do not grant graph functions to RLS-restricted roles"), add a
guard that refuses topology reads on tables with RLS enabled unless an opt-in
GUC acknowledges the boundary, or build RLS-aware traversal (per-caller
visibility bitmap, expensive). At minimum the first option belongs in
`docs/user_guide/administration-and-security.mdx` and the known-issues
register **now**.

### H2 — Cross-backend durable-ingest race (from sync review)

Durable projection ingest/manifest publication is guarded only by a
process-local `static Mutex<HashSet<PathBuf>>` (`projection/ingest.rs:86-111`)
plus a TOCTOU `final_path.exists()` check (`manifest.rs:225-230`). Two
backends running `graph.apply_sync()` concurrently on the same persisted
mutable graph can publish the same generation id; POSIX rename-replace means
last-writer-wins and one backend's segments are silently dropped. Build and
vacuum already take a per-graph advisory lock (`sql_build.rs:315-330`) —
ingest needs the same. Full detail in `01-sync-architecture-review.md` (W1).

### H3 — Tenant scoping is a filter, not an authorization boundary

For multi-tenant-table graphs (tenant bitmaps, graph not pinned to one
tenant), `resolve_tenant_scope` (`sql_sync.rs:1883-1927`) accepts any
caller-supplied tenant string; `bfs.rs:572-582` just checks bitmap
membership. Any user with table SELECT can read any tenant's subgraph.
Combined with H1, cross-tenant topology is readable. This may be intended
(`graph.enforce_tenant_scope` covers the pinned-graph case) but docs currently
suggest more isolation than exists — must not be marketed as an isolation
guarantee.

## MEDIUM

### M1 — Bidirectional BFS may return a non-minimal "shortest" path

`bidirectional_bfs` breaks at the **first** frontier intersection
(`path_finder.rs:165-172`, `201-208`) instead of finishing the level and
choosing the meeting node minimizing `d_fwd + d_bwd`. Since visited sets span
multiple prior levels, two meeting points found in one expansion can differ in
combined distance — the first found is not guaranteed minimal. Existing tests
only cover single-path chains (`path_finder.rs:516-579`). Likely a real
off-by-one-hop correctness bug on the default bidirectional path. Needs a
targeted regression test with two meeting nodes at different combined depths.

### M2 — Circuit breakers truncate silently

`max_nodes`/`max_frontier` stop traversal and return partial results with no
`truncated` flag (`bfs.rs:304-324`, `416-438`, `640-649`). Callers cannot
distinguish "fully explored" from "capped". Worse: BFS truncation inside
shortest-path search (`path_finder.rs:287-371`) can produce a **false
"no path"** for a reachable target. The result contract needs a
`truncated`/`capped` signal, and path functions should error rather than
answer wrongly when capped.

### M3 — Every traversal deep-clones all tenant state

`traverse_with_filter_ops` clones `tenanted_table_oids` and the entire
`tenant_membership` `HashMap<String, RoaringBitmap>` into `BfsConfig` on every
call, even with no tenant argument (`engine.rs:733-737`). Biggest avoidable
per-query cost in the read path; `BfsConfig` should borrow.

### M4 — Builder interpolates raw catalog table names into dynamic SQL

Hydration resolves names OID-stably via regclass (`catalog.rs:163-167`), but
the builder formats the raw registered name into `SELECT … FROM {}` directly
(`builder.rs:341`, `478-482`). Registration validates via `to_regclass($1)`,
so not classic injection — but the build re-resolves under the *build-time*
`search_path`, so an unqualified registered name can bind to a different
relation than at registration. Use the same catalog-name path as hydration.

### M5 — Artifact + sidecars are not atomically committed as a unit

`write_graph_file_internal` renames `.pggraph` (`persistence.rs:615-620`) and
*then* writes the `.sync` (applied watermark) and `.projection_mode` sidecars
(`:621-622`). A crash in between pairs a new artifact with a stale sync
checkpoint → replay from the wrong watermark on reload. No parent-directory
fsync after the rename either (the projection manifest path does this
correctly — `manifest.rs:233-251`). Fold checkpoint + mode into the artifact
or a single fsynced manifest.

### M6 — Full-file CRC + full content validation on every backend load

`load_graph_file` CRCs the entire mapping (`persistence.rs:773`) and
`validate_persisted_contents` walks every edge/PK offset (`:312-408`) on each
backend's first query — faulting in the whole file and negating mmap laziness.
This is very likely the measured ~4.4 s first-backend-query cost called out in
the roadmap. A per-generation validation cache (validate once per artifact
generation per cluster, not per backend) is the obvious optimization.

## LOW

- **L1 — Endianness**: mmap fast paths read `u32`/`u64` slices in native
  endianness (`edge_store.rs:508-509,717-777`; `node_store.rs:321-445`) while
  the format is little-endian. Silent misread on big-endian hosts (s390x).
  Document LE-only and add a `cfg!(target_endian)` guard.
- **L2 — Frontier pre-allocation scales with the GUC** not the graph
  (`bfs.rs:248,362,510`): `max_frontier` is a `Userset` GUC up to 10M → ~40 MB
  allocation per traversal a user can self-inflict. Clamp to
  `min(max_frontier, node_count)`.
- **L3 — Hand-rolled `errstart`/`errfinish` FFI** for custom `PG0xx` SQLSTATEs
  (`safety.rs:238-281`). Correct today and tested, but bypasses pgrx and rides
  on ereport ABI stability across pg14–18 — a maintenance watch item.
- **L4 — Poison sync rows have no dead-letter path**; `_sync_log.error_message`
  exists but is never written/read (sync review W3).

## What Is Well Done (preserve these)

- Defense-in-depth unsafe layer: pointer/size/bounds/alignment validation
  before every `from_raw_parts` (`node_store.rs:77-151`) — even a bypassed
  layout validator would not cause UB.
- Load-time integrity ordering: CRC → monotonic offsets → target bounds →
  flag domain → UTF-8 PK validation, all before store construction
  (`persistence.rs:312-506`).
- Atomic artifact swap; never in-place mutation; per-graph advisory
  xact locks for build/vacuum (`sql_build.rs:13-38`).
- Panic boundaries on all SQL entry points; overflow-checked arithmetic in
  persistence; edge-type registry capped at 255.
- Sparse traversal metadata mode (`bfs.rs:201-211`) avoiding O(node_count)
  allocations for small-visit queries.
- PostgreSQL-first GQL writes with source-row locking + predicate re-check at
  the write boundary (KI-001 hardening confirmed in code).
