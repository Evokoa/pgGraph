# pgGraph 1.1 Release Plan

> **Planning snapshot:** 2026-08-10 on `dev` at `f69771c`.
>
> This document is the working source of truth for pgGraph 1.1 scope. The
> completed 1.0 program remains archived in [`../v1-release/`](../v1-release/README.md).

## Decision

pgGraph 1.1 should be a focused security, correctness, and usability release.

The release should contain:

1. the current unpushed playground, benchmark, packaging, and ACL fixes;
2. cancellation-safe, publish-on-success graph replacement;
3. caller-scoped row-level security for every topology read;
4. the measured removal of redundant query-start catalog work;
5. a troubleshooting entry for missing `::regclass` casts; and
6. backward-compatible relationship-type filters for shortest-path queries.

The bounded batch-mutation design and open-vocabulary relationship-type design
should not ship in the same release. Each design changes a larger public and
storage contract than the 1.1 security work needs. Retarget both designs to a
later release before committing the current roadmap edit.

This scope makes 1.1 large enough to fix a documented security boundary, but
small enough to validate on PostgreSQL 14 through 18 without a broad engine
rewrite.

## Release outcome

For a caller that does not bypass PostgreSQL row-level security (RLS), a graph
query must observe this topology:

```text
visible graph = current pgGraph projection
              ∩ caller-visible node source rows
              ∩ caller-visible relationship source rows
```

The intersection applies before a topology-producing query returns
coordinates, paths, component or aggregate counts, or hydrated values. A
hidden node behaves as if it does not exist. A hidden relationship cannot
connect two otherwise visible nodes.

Operational telemetry is a separate contract. `status()`, runtime and
projection status, active-generation counts, resource snapshots, and job
status report physical projection/artifact state rather than an RLS-filtered
virtual graph. Phase 3 restricts each telemetry surface to its selected or
named-graph authorization boundary (and graph administrators where the data is
cluster-wide or artifact-administrative), and the public docs label those
totals explicitly. pgGraph 1.1 does not build caller-specific status artifacts
or scan all RLS-visible rows merely to produce operator telemetry.

PostgreSQL remains authoritative. pgGraph does not copy policies into the
artifact, cache policy results across statements, or create a second security
model.

## Non-negotiable invariants

- PostgreSQL source tables remain the only durable source of truth.
- Query-time RLS uses the calling role and the active PostgreSQL snapshot.
- ACL checks remain in place even when RLS filtering is active.
- Hidden seeds and targets behave as nonexistent nodes.
- Hidden intermediate nodes block everything reachable only through them.
- Hidden relationship rows block their projected edges.
- Hydrated and coordinate-only query modes have the same visibility.
- Counts and component statistics include visible topology only.
- Existing 1.0 graph artifacts load when they contain the required relationship
  identity sidecar. The RLS implementation must not change artifact bytes by
  default.
- A projection that lacks source relationship identity fails closed when an
  active RLS policy requires that identity. The error must require a rebuild.
- The no-RLS fast path performs no source-table visibility scan and allocates no
  graph-sized visibility bitmap.
- Visibility work is query-scoped. Cross-statement visibility caching is out of
  scope because policy inputs can change between statements.
- Resource and interrupt checks remain active during visibility scans and graph
  execution.
- A failed or cancelled build never replaces or deletes the last published
  generation. When low-memory construction unloads backend residency, the next
  graph query or status read reconciles it from the published manifest.

## Current repository state

The local `dev` branch is 12 commits ahead of `origin/dev`. One additional
uncommitted edit changes [`docs/roadmap.mdx`](../../docs/roadmap.mdx).

### Unpushed commit ledger

| Commit | Outcome | 1.1 disposition |
|---|---|---|
| `1bcd1f1` | Pins the ICIJ benchmark snapshot instead of trusting a moving `LATEST` archive. | Include. |
| `10b73cf` | Deduplicates Panama node identifiers across category CSV files. | Include. |
| `0b23a62` | Allows sandbox virtualenv installation when `sfw` is unavailable. | Include after policy and clean-host validation. |
| `be553e6` | Resolves Python shims to the real interpreter before virtualenv creation. | Include. |
| `dbf181c` | Caches playground graph initialization per connection. | Include. |
| `5c2c84c` | Restores `statement_timeout` after each playground query and removes the redundant build check from the Run SQL path. | Include. |
| `3f23bf0` | Pins the Streamlit dataframe dependency stack and adds a rendering regression test. | Include. |
| `c5ea690` | Fully qualifies Docker Hub base images for Podman compatibility. | Include. |
| `2f5a128` | Renders query results on the first Run SQL click. | Include. |
| `d6fc39f` | Pins Panama release metadata and its validation contract. | Include. |
| `edbdb53` | Adds deep Panama traversal benchmarks and dataset tests. | Include. |
| `f69771c` | Enforces table ACLs for all graph coordinates in traversal, path, and component results. | Include as defense in depth for RLS. |

### Dirty roadmap edit

The uncommitted roadmap section proposes open-vocabulary relationship types as
a second 1.1 track. Do not commit that version unchanged.

Before the 1.1 release branch is prepared:

- make caller-scoped topology RLS the 1.1 focus;
- change bounded batch mutations from a 1.1 commitment to a later proposed
  release;
- change open-vocabulary relationship types from a 1.1 track to a later
  proposed release; and
- keep both designs visible under the post-1.1 roadmap.

## Reported issue disposition

### Playground and benchmark report

| Reported problem | Current status | Required evidence before closure |
|---|---|---|
| Podman cannot resolve short Docker image names. | Fixed by `c5ea690`. | Build through Podman with no `registries.conf`, then repeat through Docker. |
| Playground and benchmark scripts hard-require `sfw`. | Partially addressed by `0b23a62`, but its direct-pip fallback conflicts with repository package-manager policy. | Phase 1 must skip installation when the virtualenv already satisfies the lock file and require `sfw` only when dependencies must be installed or changed. Test clean and pre-provisioned virtualenvs with and without `sfw`. |
| The Panama `LATEST` checksum becomes stale. | Fixed by `1bcd1f1` and `d6fc39f` with a tag-pinned release asset and metadata. | Download from a clean cache, verify the checksum, and prove the release gate never follows the moving upstream URL. |
| Panama node IDs collide across category files. | Fixed by `10b73cf`. | Retain deterministic first-row selection and assert collision counts and final uniqueness. |
| Virtualenv creation fails through uv-managed Python shims. | Fixed by `be553e6`. | Test a real shim and a normal system interpreter on Linux and macOS. |
| Streamlit rechecks or rebuilds the graph on every rerun. | Fixed by `dbf181c`. | Assert one initialization per connection and stable metrics across widget reruns. |
| Run SQL can trigger a rebuild under a leaked timeout. | The playground trigger and timeout leak are fixed by `5c2c84c`. Cancellation-safe engine replacement is separate. | In Phase 1, prove Run SQL never starts a rebuild and always restores the previous timeout. In Phase 2, prove a deliberately cancelled build preserves the last good projection. |
| Unpinned dataframe packages can crash result rendering. | Fixed by `3f23bf0`. | Run the headless dataframe test on supported Python versions and render a real multi-table result. |
| Results do not render until a second click. | Fixed by `2f5a128`. | Assert that the first click renders every result table with one script rerun. |

Do not close the combined issue from commit messages alone. Attach the clean-host
commands and results to the issue or release evidence.

### Missing `::regclass` troubleshooting entry

This issue is still open in the current tree.

Add a section near the start of
[`docs/user_guide/troubleshooting.mdx`](../../docs/user_guide/troubleshooting.mdx)
for this symptom:

```text
ERROR: function graph.add_table(unknown, unknown) does not exist
```

The section must explain that table arguments use PostgreSQL `regclass`, then
show the corrected call:

```sql
SELECT graph.add_table('public.users'::regclass, 'id');
```

Add the same warning beside the first manual-registration example. A docs-drift
check must cover both examples.

### Duplicate query-start catalog work

This issue is open in the current tree.

`ensure_current_graph()` resolves the selected graph, then
`current_catalog_state()` resolves it again through a definer helper. The next
`pending_sync_rows()` call loads the full sync replay catalog again. Large
registered-table and registered-edge queries therefore run more than once at
the start of a topology query.

The 1.1 fix should use the graph ID already resolved by
`ensure_current_graph()`:

1. call `read_catalog_for_graph(graph_id)` once;
2. derive the catalog fingerprint and applicable source OIDs from those rows;
3. replace the sync pending-count path with a narrow definer-mediated query
   that validates the caller's selected graph and reads only the required OIDs;
4. do not trust a caller-supplied OID array as authorization; and
5. retain per-call schema-drift and pending-sync correctness.

The narrow definer helper may return query-start state in one call. It must use
the outer caller identity, enforce graph `read` privilege, pin
`search_path = pg_catalog, pg_temp`, and expose no unrelated graph metadata.

Backend-local caching through relcache or syscache callbacks is not part of
1.1. Consider it only after the one-call path is measured and all invalidation
sources have a written correctness model.

Required performance evidence:

- reproduce the fixed-work benchmark with the reporter's pinned seed shape;
- count SPI statements before and after the change;
- compare median and p95 latency across at least 40 warm runs;
- keep row counts identical; and
- retain `graph.status()` as a negative control.

### Relationship-typed shortest paths

This issue is accepted for 1.1, but the 1.0 signatures must remain available.

Add overloads rather than replacing the existing functions:

```sql
graph.shortest_path(
  source_table regclass,
  source_id text,
  target_table regclass,
  target_id text,
  edge_types text[],
  max_depth integer DEFAULT 20,
  hydrate boolean DEFAULT true
)

graph.weighted_shortest_path(
  source_table regclass,
  source_id text,
  target_table regclass,
  target_id text,
  edge_types text[]
)
```

The required `edge_types` argument keeps four-argument calls unambiguous. The
existing overloads delegate with `edge_types = NULL`. Both algorithms resolve
labels once before entering the hot loop and compare compact type IDs during
expansion.

Tests must cover:

- `NULL` or the legacy overload using all registered relationship types;
- one allowed type;
- multiple allowed types;
- an unknown type returning the established typed diagnostic;
- a path that becomes longer after filtering;
- no path after filtering;
- weighted and unweighted parity; and
- interaction with hidden RLS nodes and relationships.

## RLS architecture

### Use the existing engine seams

Do not create a new topology trait hierarchy.

The engine already routes adjacency through `NeighborSource` and
`WeightedNeighborSource`. BFS and DFS already use `candidate_allowed()` as an
admission gate. Tenant filtering already proves that a query-scoped
`RoaringBitmap` check can live in the traversal hot path.

Add one pure runtime type:

```rust
enum VisibilityScope {
    Unrestricted,
    Enforced {
        hidden_nodes: RoaringBitmap,
        hidden_relationships: RoaringBitmap,
    },
}
```

The exact fields may change during implementation, but the type must expose a
small contract:

```text
allows_node(node_idx) -> bool
allows_relationship(relationship_id) -> bool
```

The SQL adapter builds the scope. Core graph algorithms only read it. No core
algorithm should issue SPI or know how PostgreSQL evaluates RLS.

Use `RoaringBitmap` for both node and relationship IDs. Both identities are
`u32`, and the dependency already exists. Do not add a new collection
dependency without benchmark evidence.

### Module ownership and dependency direction

Keep the implementation in the existing crate. This feature does not justify
a new crate or a new topology trait: it has one PostgreSQL adapter, one pure
policy value, and existing algorithm call sites.

Use these module boundaries:

| Module | Owns | Must not own |
|---|---|---|
| `visibility.rs` | The pure `VisibilityScope` value and node/relationship admission methods. | SPI, PostgreSQL role lookup, catalog reads, or graph loading. |
| `sql_visibility.rs` | `check_enable_rls()` adaptation, governed cursor scans, source-key resolution, and construction of `VisibilityScope`. | BFS, path selection, component logic, or GQL semantics. |
| `sql_facade/runtime.rs` | The query-start composition root: freshness, selected graph, catalog state, caller-scoped visibility, and stable error translation. | Independent visibility rules for individual query APIs. |
| Core algorithm modules | Admission at the point a node or edge can influence topology. | PostgreSQL policy evaluation or SPI calls. |
| SQL/GQL facade modules | Construction and threading of one query context into core operations. | Reimplementation of bitmap membership rules. |

The dependency direction is:

```text
PostgreSQL/pgrx
      │
      ▼
sql_visibility + sql_facade/runtime
      │ builds
      ▼
VisibilityScope + QueryExecutionContext
      │ consumed by
      ▼
BFS / DFS / paths / components / aggregation / GQL
```

Core algorithms depend only on the pure visibility contract. The PostgreSQL
adapter may depend on catalog, resolution, relationship identity, transaction
delta, resource-governor, and pgrx modules. The reverse dependency is
forbidden.

Introduce a small borrowed execution value instead of adding visibility and
governor parameters independently to every function:

```rust
struct QueryExecutionContext<'a> {
    governor: &'a ResourceGovernor,
    visibility: &'a VisibilityScope,
}
```

The exact name can follow existing conventions, but it must remain a borrowed,
query-scoped value. Do not put the loaded graph, SPI client, role identity, or
mutable global state inside it.

At the SQL boundary, change query initialization to return one owned
`QueryStartState` rather than `()` once the caller-identity phase is complete.
It should single-source the selected graph ID, catalog state needed for
freshness and visibility, and the constructed visibility scope. Avoid storing
the same catalog fact in multiple fields; callers should derive secondary
values such as applicable relation OIDs from this state.

### Error and unsafe boundaries

- Continue using the existing `GraphResult` and stable SQL diagnostic system.
- Translate PostgreSQL scan and policy failures in `sql_visibility.rs`; core
  algorithms should receive no SPI errors.
- Add a stable rebuild-required diagnostic for missing relationship identity.
  Do not silently downgrade to endpoint-only visibility.
- Keep `check_enable_rls()` inside one narrow unsafe wrapper with a documented
  `// SAFETY:` contract for relation OID, caller role OID, snapshot, and
  PostgreSQL version assumptions.
- Add no manual user-ID switching and no new panic/unwind boundary.
- Add no dependency for this design; `roaring`, the resource governor, and pgrx
  bindings already provide the required primitives.

### Detect active RLS without SPI

For each distinct registered node or relationship source relation, call
PostgreSQL's `check_enable_rls(relid, outer_role_id, false)`. Passing `false`
for `noError` preserves PostgreSQL's error behavior when policy enforcement is
required but the current environment forbids it.

Interpret the PostgreSQL result as follows:

| Result | Query behavior |
|---|---|
| `RLS_NONE` | The relation is unrestricted for this statement. |
| `RLS_NONE_ENV` | The current caller bypasses the policy through ownership or `BYPASSRLS`; treat it as unrestricted for this statement. |
| `RLS_ENABLED` | Build visibility for this relation through caller-scoped SQL. |

Call the PostgreSQL function from one documented unsafe adapter. The adapter
must include a `// SAFETY:` contract and PostgreSQL 14 through 18 tests.

If no relation returns `RLS_ENABLED`, return `VisibilityScope::Unrestricted`
immediately. This path performs no source scan.

### Build node visibility eagerly

For each RLS-active registered node table:

1. initialize the restricted set from projected membership, including active
   transaction-local additions;
2. scan caller-visible primary keys through an invoker SPI cursor;
3. use the existing composite-primary-key expression and resolution index to
   map each key to a node index;
4. remove each visible index from the hidden-node bitmap; and
5. keep unresolved visible rows out of the scope because they are not in the
   current projection.

The cursor must fetch bounded batches, check interrupts, charge elapsed work,
and reserve memory through the query governor. Do not pass every projected key
to one unbounded `ANY(text[])` parameter.

`visible_node_keys_governed()` remains useful for targeted checks and as the
SQL/composite-key pattern. The eager builder needs a cursor-based scan because
its input can be the whole projected table.

### Build relationship visibility eagerly

For each RLS-active relationship source mapping:

1. enumerate projected relationship identities for that mapping, including
   transaction-local identities;
2. initialize those IDs as hidden;
3. scan caller-visible source keys from the relationship source table;
4. resolve `(mapping_id, source_key)` through the relationship identity store;
5. remove visible relationship IDs from the hidden bitmap; and
6. group mappings that share the same relation and source-key expression when
   this avoids duplicate scans without changing semantics.

Every production edge in an RLS-active mapping must carry a stable
relationship ID. If an edge does not, return a stable rebuild-required error
before execution. Do not guess visibility from endpoints alone.

### Build timing and snapshot

Build the visibility scope after `ensure_current_graph_for_query()` has applied
the selected freshness behavior and before any seed is resolved or topology is
read. All visibility SPI calls must run as the caller under the statement's
active snapshot.

Do not retain the scope after the SQL statement. Policies can depend on role,
session settings, time, or other tables.

### Security context

Remove `SECURITY DEFINER` from these query functions:

- `graph.traverse(seed_table, ...)`;
- `graph.connected_components()`; and
- `graph.component_stats()`.

The other traversal, shortest-path, search, workflow, GQL, and Cypher entry
points already run as invokers.

The named-graph catalog remains protected through the existing narrow definer
functions, including `graph.current_graph()` and the selected-graph helpers.
Do not add `SetUserIdAndSecContext()` or manual identity switching.

Changing these three functions from definer to invoker changes recorded 1.x
function metadata. Treat the change as a security correction. Update the
machine-readable contract, metadata audit, release notes, upgrade test, and
role-grant test in the same checkpoint.

### Admission points

Visibility must be checked at every place where a node or relationship becomes
observable or can influence reachability.

| Area | Required gate |
|---|---|
| BFS and DFS | Check the seed first. Extend `candidate_allowed()` to check the candidate node and relationship before visited/frontier admission. |
| Unweighted shortest path | Check source and target before the equal-node fast path. Gate forward, backward, and single-direction expansion. |
| Weighted shortest path | Check source and target. Gate each Dijkstra edge before relaxation. |
| Connected components | Skip hidden nodes in the outer scan and final accounting. Union only through visible relationships to visible targets. |
| GQL node scans | Filter `source_nodes()`, identity lookup, join seeds, and optional-match seeds before row creation. |
| GQL relationship expansion | Gate both `append_matching_neighbors()` and `append_all_neighbors()`, including wildcard and join paths. |
| GQL writes | Apply visibility to the read or MATCH phase before relationship `CREATE` resolves existing endpoints, or before `SET`, `REMOVE`, `DELETE`, `DETACH DELETE`, or `MERGE` chooses a source row. PostgreSQL DML remains the final RLS authority. |
| SQL aggregation | Replace raw `EdgeStore` reads with the same node and relationship gates. Gate coordinate resolution supplied through JSON input. |
| Path-count estimation | Count only paths admitted by the visibility scope. |
| Direct APIs | Keep the targeted source-row probe for `get_node()`. Build a traversal scope for `get_neighbors()`. |
| Search | Keep source-table SQL as the candidate authority. Reuse the traversal scope when search results become traversal seeds. |
| Workflow APIs | Reuse the governed traversal, search, and shortest-path functions. Do not add separate visibility logic. |
| Component pages and statistics | Compute and paginate the caller-visible component result. Do not post-filter an unrestricted component result. |
| Cypher | Reuse the GQL execution path and its scope. |

Post-execution GQL visibility checks remain as defense in depth. They must no
longer be the primary enforcement mechanism or turn ordinary hidden rows into
an execution error.

### Gate ordering

Apply visibility before these observable controls:

- visited-set insertion;
- frontier insertion;
- parent or path recording;
- result and pagination counts;
- component union and size accounting; and
- GQL row caps.

The resource governor may still charge examined projection entries to keep the
backend safe. Release documentation must not claim formal timing or
resource-exhaustion noninterference. The supported guarantee is PostgreSQL-like
result and topology visibility, not a proof against every side channel.

### Configuration

Add a superuser-settable enum GUC with this recommended contract:

```text
graph.rls_mode = enforce | legacy_bypass
default = enforce
context = SUSET
```

`legacy_bypass` preserves the 1.0 topology behavior for a controlled emergency
rollback. The name must make the weaker behavior explicit. Do not call the
mode simply `bypass`, because pgGraph is skipping topology filtering rather
than granting a PostgreSQL `BYPASSRLS` role attribute.

Keep `graph.allow_rls_tables` as a deprecated compatibility setting throughout
the documented deprecation window. Under `enforce`, `graph.build()` accepts
RLS-enabled tables because query-time enforcement is active. The old setting
does not disable enforcement.

Document how `legacy_bypass` interacts with `allow_rls_tables`, table ACLs,
hydration, and existing 1.0 deployments.

## Test strategy

Write each behavior test before or with its implementation change. Unit tests
cover pure bitmap admission. PostgreSQL integration tests cover identity,
policies, snapshots, and SQL-visible behavior.

### Core unit tests

- unrestricted scope admits every valid node and relationship;
- hidden-node scope blocks a candidate before frontier insertion;
- hidden-relationship scope blocks an edge with visible endpoints;
- DFS reverse iteration has the same visibility as BFS;
- bidirectional shortest path cannot meet through a hidden node or edge;
- Dijkstra ignores a hidden cheaper edge and returns the longer visible path;
- component accounting excludes hidden nodes and relationships;
- aggregation and path counting use the same admitted topology;
- transaction-local node and relationship IDs are checked; and
- missing relationship identity fails closed only when its mapping has active
  RLS.

### PostgreSQL role and policy matrix

Use real login roles for the primary heavy test. Add focused `SET ROLE` tests
where they improve local coverage.

Cover:

- a policy based on `current_user`;
- a policy based on a session GUC;
- permissive and restrictive policies;
- table owner behavior without `FORCE ROW LEVEL SECURITY`;
- table owner behavior with `FORCE ROW LEVEL SECURITY`;
- a role with `BYPASSRLS`;
- `row_security = off` failure behavior;
- composite primary keys;
- partitioned registered tables;
- separate relationship tables with visible endpoints and hidden edge rows;
- node-backed foreign-key relationships;
- transaction-local inserts and deletes;
- savepoint rollback;
- pending-sync freshness modes;
- `hydrate := true` and `hydrate := false`; and
- named-graph read grants with no direct access to internal catalogs.

### Required semantic scenarios

- A hidden seed returns no rows.
- A hidden target returns no path.
- A hidden intermediate node blocks the visible node behind it.
- A hidden relationship with visible endpoints blocks traversal.
- A hidden short path yields the longer visible path.
- A hidden weighted edge yields the higher-cost visible path.
- GQL `MATCH`, optional match, joins, wildcard paths, and Cypher return only
  visible topology.
- A GQL write cannot select a hidden row during its MATCH phase, and
  relationship `CREATE` cannot resolve a hidden existing endpoint.
- Component IDs, sizes, ranks, isolated counts, and totals derive from visible
  topology.
- Aggregates and path estimates cannot count hidden topology.
- Search remains source-SQL-driven and traversal from search results stays
  visible.
- ACL denial remains deterministic when the caller lacks table-level
  `SELECT`.

### Performance gates

Record separate results for unrestricted and RLS-active queries.

For the unrestricted path:

- use `check_enable_rls()` without SPI;
- perform no source visibility scan;
- create no graph-sized bitmap;
- run the existing BFS Criterion groups; and
- compare SQL fixed-work traversal median and p95 before and after the change.

For the RLS-active path:

- report visibility-build time separately from graph-execution time;
- report visible and hidden node and relationship counts;
- report peak governed memory;
- test sparse-allow and sparse-deny policies; and
- test a production-shaped graph, including the Panama scale when the fixture
  can express the policy safely.

The Phase 0 no-RLS budget is no more than a 5% median latency regression and no
more than a 10% p95 regression against the clean Phase 4 SQL baseline for the
fixed-work traversal cases. Exceeding either threshold blocks Phase 5. Change
the budget only through an explicit plan amendment with retained benchmark
evidence; do not waive it ad hoc during review.

## Phase 0 topology and replacement inventory

This inventory is the completeness checklist for the implementation phases.
The generated 1.0 SQL profile remains authoritative for the complete public
function set. This table classifies the subset that can create, inspect,
replace, or derive topology and assigns each surface to one implementation
phase and at least one behavioral regression.

`Public query authorization` below means the function's extension `EXECUTE`
contract, selected-graph read authorization, and source-table `SELECT` checks.
`Build authorization` means the existing graph ownership/administration and
source-table checks. Those grants remain in force; RLS is an additional
caller-row boundary, not a replacement.

### Public SQL topology surface

| Public function or overload family | Current execution context | Authorization and grant boundary | Projection/artifact dependency | Owning phase | Required regression |
|---|---|---|---|---|---|
| `graph.build()`, `graph.build_graph()`, `graph.build_async_graph()` | Mixed invoker/definer wrappers around the same build pipeline | Build authorization | Creates candidate artifact and generation manifest; replaces backend engine | 2 | Cancellation before publication retains A; post-publication fault retains B |
| `graph.vacuum()`, `graph.vacuum_graph()` | Existing maintenance wrappers | Build authorization | Rewrites the base projection and publishes a generation | 2 | Cancelled vacuum retains the last published generation |
| `graph.maintenance()`, `graph.maintenance_graph()`, scheduled/background maintenance | Existing maintenance/job wrappers | Build/job authorization | May rebuild and publish the base projection | 2 | Foreground and background failure retain the last published generation |
| `graph.status()` | Invoker; selected graph is resolved through catalog helpers | Selected-graph read authorization, made explicit and role-tested in Phase 3 | Reports physical backend-local engine state reconciled with the selected graph; not RLS-row-filtered | 3 | Ungranted roles are denied; an authorized status call after Phase 2 cancellation reports the retained generation |
| `graph.projection_status()` | Invoker; no direct privilege check at the SQL entry point today | Phase 3 adds explicit selected-graph admin authorization | Reports physical manifest, watermark, segment, and artifact state; not RLS-row-filtered | 3 | Reader/admin roles prove the grant boundary; cancellation still identifies only the published generation |
| `graph.active_generation_count()` | Invoker; no selected-graph or privilege check today | Phase 3 restricts cluster-wide generation telemetry to graph administrators | Reports physical unexpired generation heartbeats; not RLS-row-filtered | 3 | Public/reader execution is denied and an administrator receives the physical count |
| `graph.build_resource_status()` | Invoker; selected graph is resolved through status helpers | Phase 3 adds explicit selected-graph admin authorization | Reports physical resource use for the latest build | 3 | Reader/admin roles prove the grant boundary; a failed replacement cannot change the active generation |
| `graph.resource_status()` | Invoker; no direct graph privilege check today | Phase 3 restricts backend-local operation telemetry to graph administrators | Reports the physical last-operation resource snapshot | 3 | Public/reader execution is denied and an administrator receives the physical snapshot |
| `graph.build_status()`, `graph.build_status_for_graph()` | Pinned definer catalog mediators that capture and authorize the outer caller | Phase 3 makes build-job read authorization explicit | Reports synchronous/backend-local or durable build-job outcomes | 3 | Raw job tables deny public reads; cancelled build is failed/cancelled while generation A remains current; cross-graph status is denied or not found |
| `graph.maintenance_status()`, `graph.maintenance_status_for_graph()` | Pinned definer catalog mediators that capture and authorize the outer caller | Phase 3 makes maintenance-job read authorization explicit | Reports durable maintenance-job outcomes | 3 | Raw job tables deny public reads; cancelled maintenance is failed/cancelled while generation A remains current; cross-graph status is denied or not found |
| `graph.current_graph()` | Definer with explicit caller-role checks | Named-graph read authorization | Reports session-selected metadata without loading or publishing an artifact | 2 | A low-privilege role sees only graph metadata granted to that role |
| `graph.set_current_graph()` | Definer with explicit caller-role checks | Named-graph read authorization | Selects graph metadata without changing the published generation | 2 | A low-privilege role cannot select an ungranted graph |
| `graph.select_graph()` | Definer with explicit caller-role checks | Named-graph read authorization | Selects a graph and may eager-load its published artifact | 2 | Low-privilege selection cannot expose another graph; cancelled eager load leaves the prior published generation usable |
| `graph.load_graph()`, `graph.unload_graph()` | Definer with explicit caller-role checks | Named-graph admin authorization | Loads or evicts backend-local residency without changing the published manifest | 2 | Load failure retains the prior backend slot; unload never removes the durable generation |
| `graph.loaded_graphs()`, `graph.graph_runtime_status()` | Definer; runtime status filters catalog rows by the captured caller, while loaded-graph filtering requires an explicit audit | Phase 3 enforces named-graph read authorization for every returned row | Reports physical node/edge counts, backend residency, and artifact presence; not RLS-row-filtered | 3 | `SET ROLE` and real-role tests cannot observe ungranted graph IDs, names, counts, or artifact metadata |
| `graph.projection_compact()` | Invoker with explicit graph-admin check | Build/graph-admin authorization | Publishes a compacted projection-manifest generation | 2 | Cancellation before manifest publication retains the prior generation |
| `graph.projection_repair()` | Invoker with explicit graph-admin check | Build/graph-admin authorization | May repair chunks or invoke the full maintenance rebuild and publish a generation | 2 | Targeted-repair and full-rebuild cancellation retain the prior published generation |
| `graph.ingest_projection()` | Invoker with explicit graph-admin check | Build/graph-admin authorization | Publishes committed sync rows into durable projection segments | 2 | Cancellation cannot expose a partial segment generation or advance the watermark without publication |
| `graph.apply_sync()` | Definer with explicit caller-role and graph-admin checks | Build/graph-admin authorization | Applies committed sync rows backend-locally or through durable projection ingestion | 2 | Cancellation preserves the last published generation and a replayable sync watermark |
| `graph.projection_gc()` | Invoker with explicit graph-admin check | Build/graph-admin authorization | Deletes only generations outside the retained/active publication set | 2 | Cancellation never deletes the current or protected generation |
| `graph.traverse()` including the multi-seed overload | Definer for the primary overload today; must be invoker after Phase 3 | Public query authorization | Resolution index, node membership, adjacency, relationship identity, optional source hydration | 5 | Hidden seed, intermediate, and relationship; hydrate parity |
| `graph.get_node()` | Invoker | Public query authorization | Resolution index plus targeted source-row visibility probe | 6 | Hidden coordinate behaves as nonexistent |
| `graph.get_neighbors()` | Invoker | Public query authorization | Resolution index and adjacency | 6 | Visible endpoints connected only by a hidden relationship are absent |
| Existing `graph.shortest_path()` overload | Invoker | Public query authorization | Resolution index, bidirectional adjacency, parent/path reconstruction | 6 | Hidden target and hidden shorter path |
| New typed `graph.shortest_path()` overload | Invoker | Public query authorization | Reuses visible shortest-path expansion with compact relationship-type IDs | 9 | Legacy calls and `NULL` use all types; filtered longer/no-path cases |
| Existing `graph.weighted_shortest_path()` overload | Invoker | Public query authorization | Resolution index, weighted adjacency, relationship identity | 6 | Hidden cheap edge yields the visible higher-cost path |
| New typed `graph.weighted_shortest_path()` overload | Invoker | Public query authorization | Reuses visible Dijkstra expansion with compact relationship-type IDs | 9 | Legacy/type-filter parity and hidden-edge composition |
| `graph.traverse_search()` | Invoker | Source search ACL/RLS plus public query authorization | Source-SQL candidates become projection traversal seeds | 6 | Hidden search seed cannot expose hidden traversal topology |
| `graph.expand()` | Invoker | Public query authorization | Delegates to governed traversal | 6 | Wrapper result equals direct visible traversal |
| `graph.find_related()` | Invoker | Source search ACL/RLS plus public query authorization | Search candidates plus governed traversal | 6 | Hidden candidate/intermediate cannot affect related results |
| `graph.path()` | Invoker | Public query authorization | Delegates to governed shortest path and path formatting | 6 | Hidden shorter path yields longer visible formatted path |
| `graph.connection()` | Invoker | Source search ACL/RLS plus public query authorization | Search candidates plus repeated shortest paths | 6 | No connection is formed through hidden topology |
| `graph.neighborhood()` | Invoker | Public query authorization | Delegates to governed traversal | 6 | `node_count`, `sample_nodes`, and `truncated` describe visible topology only |
| `graph.connected_components()` | Definer today; must be invoker after Phase 3 | Existing component authorization plus source `SELECT` | Scans active nodes and raw adjacency | 7 | Hidden bridge splits components |
| `graph.component_stats()` | Definer today; must be invoker after Phase 3 | Existing component authorization plus source `SELECT` | Derived component state and active-node accounting | 7 | Totals, isolated count, and maximum use visible topology |
| `graph.components()`, `graph.largest_component()` | Invoker | Existing component authorization plus source `SELECT` | Derived component state, ordering, and pagination | 7 | Sizes, ranks, and page totals use visible topology |
| `graph.component()`, `graph.isolated_nodes()` | Invoker | Existing component authorization plus source `SELECT` | Component membership plus optional hydration | 7 | Hidden members are absent in hydrated and coordinate-only modes |
| `graph.aggregate()` | Invoker | Public query authorization | Raw adjacency through `sql_aggregation.rs`, coordinate input, node properties | 7 | JSON coordinates cannot smuggle hidden nodes; counts exclude hidden edges |
| `graph.path_count_estimate()` | Invoker | Public query authorization | Raw/derived path expansion | 7 | Estimate matches the visible admitted topology |
| `graph.gql()` read statements | Invoker | Public query authorization and plan-level source ACL checks | Node scans, identity lookup, joins, wildcard paths, and adjacency expansion | 8 | MATCH/OPTIONAL/join/wildcard scenarios exclude hidden topology before row caps |
| `graph.gql()` mapped write statements | Invoker | PostgreSQL DML ACL/RLS plus graph mapping authorization | MATCH/read phase selects projection rows before PostgreSQL DML; relationship CREATE resolves existing endpoints through projection node scans | 8 | Hidden rows cannot be selected by SET/REMOVE/DELETE/DETACH/MERGE, and relationship CREATE cannot resolve a hidden existing endpoint |
| `graph.cypher()` | Invoker | Same authorization as lowered GQL | Lowers into the GQL executor and projection reads | 8 | Cypher and equivalent GQL return the same visible topology |

### Internal topology admission seams

| Internal seam | Current responsibility | Risk if ungated | Owning phase and regression |
|---|---|---|---|
| `engine.rs` seed resolution verification | Converts source coordinates to node indexes | Hidden seeds enter algorithms | Phase 5: hidden seed behaves as nonexistent |
| `path_finder.rs` target resolution verification | Converts target coordinates to node indexes | Hidden targets enter path algorithms | Phase 6: hidden target behaves as nonexistent |
| `bfs.rs::candidate_allowed()` and DFS iteration | Frontier admission, tenant/filter checks, visited state | Hidden nodes or edges influence reachability and limits | Phase 5/6: hidden intermediate/edge and reverse traversal |
| `path_finder.rs` forward, backward, and Dijkstra expansion | Path meeting and relaxation | Hidden shorter/cheaper paths win | Phase 6: longer visible unweighted and weighted paths |
| `connected_components.rs` outer scan and union | Component membership and sizes | Hidden topology changes component IDs and counts | Phase 7: hidden bridge and isolated-node scenarios |
| `sql_aggregation.rs` raw adjacency sites | Path enumeration and aggregate inputs | Bypasses traversal gates entirely | Phase 7: aggregate/path-count parity with visible traversal |
| `query/execute.rs::source_nodes()` and identity/join seeding | Creates GQL binding rows | Hidden nodes enter rows before expansion | Phase 8: scan, join, optional, and write-MATCH scenarios |
| `query/execute.rs::append_matching_neighbors()` and `append_all_neighbors()` | Typed and wildcard GQL relationship expansion | Hidden edges influence reachability and row caps | Phase 8: typed/wildcard visibility before limits |
| `sql_hydration.rs` and GQL post-filters | Source-row materialization and defense-in-depth probes | Post-filtering cannot undo hidden reachability | Phase 5/8: hydrate parity and no hidden-row execution error |
| `relationship_identity_store.rs` durable identities | Maps persisted edges back to relationship source rows | Endpoint-only visibility leaks hidden relationship rows | Phase 5: hidden relationship and missing-identity fail-closed test |
| Transaction delta relationship identities | Maps transaction-local edges back to source rows | New hidden edges bypass durable identity checks | Phase 6: transaction-local relationship visibility |

### Explicitly excluded non-topology surfaces

`graph.search()`, `graph.search_nodes()`, and `graph.find()` are source-table SQL
searches and already receive PostgreSQL RLS before producing candidates.
`graph.reset()` is an explicit destructive administration operation rather than
a replacement publisher; its existing admin and artifact-removal tests remain
authoritative. `graph.format_path()` formats supplied path rows without discovering topology.
Predicate constructors do not read the graph. `graph.gql_explain()` and
`graph.cypher_explain()` plan text without executing topology, while
`graph.pgq()` and `graph.create_row_predicate_subgraph()` are intentional
rejections in the 1.0 profile. Their non-execution behavior remains covered by
the existing profile tests.

The public support-status source of truth is
[`docs/user_guide/supported_features.md`](../../docs/user_guide/supported_features.md).
This plan owns implementation order and exit evidence; it must not claim that
planned behavior is shipped. Update the public ledger only when a checkpoint
changes support status.

## Phased delivery sequence

The phases below are dependency-ordered. Do not begin a later phase while an
earlier exit gate is red. Each phase should be one reviewable PR or checkpoint;
do not collapse the RLS phases into one large security PR.

Every behavior phase follows the same TDD loop:

1. **Red:** add the smallest regression test that demonstrates the missing
   behavior at that phase's public boundary.
2. **Green:** implement only the production path needed for that phase.
3. **Refactor:** consolidate shared admission logic without widening scope.
4. **Gate:** run the phase checks plus all earlier phase checks.
5. **Record:** retain test, benchmark, contract, and migration evidence against
   the exact commit.

Tests that intentionally demonstrate the old security leak must not be merged
as passing characterization tests. The PR contains the regression and its fix
together.

### Phase dependency map

```text
Phase 0: scope and risk freeze
    │
    ▼
Phase 1: integrate current fixes and docs
    │
    ▼
Phase 2: cancellation-safe build replacement
    │
    ▼
Phase 3: caller identity and invoker query boundary
    │
    ▼
Phase 4: one authoritative query-start state
    │
    ▼
Phase 5: end-to-end RLS vertical slice (traverse/BFS)
    │
    ▼
Phase 6: remaining direct traversal and path APIs
    │
    ▼
Phase 7: components, aggregation, and path analytics
    │
    ▼
Phase 8: GQL, Cypher, workflows, and write MATCH
    │
    ▼
Phase 9: relationship-typed shortest paths
    │
    ▼
Phase 10: compatibility, documentation, and release candidate
```

Phase 9 deliberately follows the Phase 8 security-complete checkpoint. The
accepted API work must not delay or become entangled with the security
correction. Phase 10 depends on every preceding phase.

### Phase status ledger

Update this table when an exit gate changes state. A phase is complete only
when its evidence and exit gate are both satisfied.

| Phase | Status at this snapshot | Blocking item |
|---|---|---|
| 0 | Complete | Scope, non-goals, compatibility direction, phase ownership, and supported-feature tracking are locked. |
| 1 | Complete | Reported fixes, policy-compliant virtualenv reuse, full Panama/Docker evidence, docs gates, and independent review are complete. |
| 2 | Complete | Cancellation-safe replacement, repair recovery, heavy cancellation/concurrency evidence, full pg17 suite, and independent Rust review pass. |
| 3 | Complete | Invoker query modes, caller-preserving catalog mediators, telemetry authorization, update SQL, real-login evidence, and independent Rust review pass. |
| 4 | Not started | Depends on caller identity and security-mode contract from Phase 3. |
| 5 | Not started | Depends on the authoritative query-start seam and optimized baseline from Phase 4. |
| 6 | Not started | Depends on the accepted traverse/BFS vertical slice from Phase 5. |
| 7 | Not started | Depends on complete direct-algorithm visibility from Phase 6. |
| 8 | Not started | Depends on stable direct and derived admission behavior from Phases 6 and 7. |
| 9 | Not started | Depends on the Phase 8 security-complete checkpoint. |
| 10 | Not started | Depends on all implementation phases and their retained evidence. |

### Phase 0: Freeze scope, contracts, and risk

**Purpose:** prevent unrelated roadmap work or compatibility decisions from
moving underneath the security implementation.

**Work:**

- Approve the 1.1 scope and explicit non-goals in this document.
- Retarget bounded batch mutations and open-vocabulary relationship types to a
  post-1.1 release.
- Inventory every topology-producing SQL function, GQL/Cypher path, direct
  `EdgeStore` read, function security mode, grant, and artifact dependency.
- Assign each inventory row to exactly one later phase and one regression test.
- Decide the RLS GUC names, compatibility behavior, no-RLS performance budget,
  downgrade position, and `sfw` fallback disposition.
- Record that no new crate, trait hierarchy, async runtime, cache, dependency,
  or artifact format is planned.

**Evidence:** reviewed surface inventory, accepted decision table, and an owner
for every open decision.

**Exit gate:** no release-blocking design decision remains open and every query
surface has a named enforcement phase. If a surface is discovered later, add
it to the inventory before implementation continues.

### Phase 1: Integrate and verify the current fixes

**Purpose:** create a clean, known-good base for security and performance work.

**Work:**

- Rebase the 12 local commits onto the intended release branch.
- Preserve commit separation or squash by coherent issue, not into one release
  dump.
- Run each playground test from its supported working directory.
- Run the available local container workflow and the fully qualified
  Dockerfile-reference regression. Repeat clean-host Docker and Podman smokes
  as a mandatory Phase 10 release-candidate matrix.
- Run the pinned Panama preparation from an empty dataset cache.
- Add the missing `::regclass` troubleshooting section and docs-drift check.
- Commit the Phase 0 roadmap retargeting without rewriting historical plans.

**Evidence:** available-runtime command log, Dockerfile portability regression,
dataset checksum and uniqueness results, playground regression results, and
docs-drift result.

**Recorded Phase 1 evidence (2026-08-10):**

- `sandbox/playground/.venv/bin/python -m unittest discover -s
  sandbox/playground -p 'test_*.py'`: 31 passed, including Streamlit `AppTest`
  rendering and first-click behavior.
- `npm run check` in `docs`, `scripts/check_docs_drift.sh`, release validation,
  script inventory, shell syntax, Python compilation, and `git diff --check`
  pass.
- A clean temporary Panama transform verified archive SHA-256
  `34475194b6a8c2d683fddc55cca02f88f08f0a538521fb13a324975221624380`,
  emitted 2,016,523 nodes and 3,339,267 source edges, and removed 1,139
  duplicate node rows.
- An isolated PostgreSQL 17.10 Docker preparation loaded the normalized data,
  built 2,016,523 projected nodes and 6,678,534 directed edges, and returned
  matching `graph.status()` counts.
- The Dockerfile qualification regression passes. A no-cache Docker build
  resolved both fully qualified Docker Hub references but the external
  registry metadata fetch did not complete on this host. Podman is not
  installed here; the qualified-reference regression is the retained portable
  evidence, with a clean Podman smoke repeated at the release-candidate gate.
- With `sfw` removed from `PATH`, an already-provisioned playground virtualenv
  is reused without invoking pip. An unsatisfied environment fails closed and
  requires `sfw` before installation.

**Exit gate:** every pasted playground and documentation issue has a linked
test or docs check, the available Docker workflow and static Podman-sensitive
image-reference contract pass, and this exact commit becomes the performance
and behavior baseline for all subsequent phases. Clean-host Docker and Podman
remain mandatory before Phase 10 can complete.

### Phase 2: Make build replacement cancellation-safe

**Purpose:** guarantee that a failed or cancelled replacement cannot leave a
previously usable graph with no active generation. This phase fixes the engine
invariant exposed by the playground report; it is not part of the UI timeout
work.

The publication linearization point is the generation-manifest
compare-and-swap. The required invariant is:

```text
active generation A + attempted replacement B
    error/cancel before manifest publication => discard B and serve A
    manifest publication succeeds            => B is authoritative
    error after manifest publication          => keep and reconcile B
```

Treat durable publication and backend-local residency as separate state. The
current persisted path already writes a generation candidate and publishes its
manifest only after validation. The remaining risk is the low-memory path,
which may clear the backend-local engine before the candidate succeeds.

**Red tests first:**

- build generation A, cancel generation B with `statement_timeout`, then prove
  A's generation, counts, and representative query results are unchanged;
- repeat with `pg_cancel_backend()` from a second connection;
- repeat with `graph.low_memory_build = on` so cancellation happens after the
  backend-local engine has been unloaded;
- run the checks from the building backend and a fresh backend to distinguish
  residency loss from durable-generation loss;
- assert `graph.status()` reconciles to A rather than reporting an empty
  projection after the cancelled low-memory build;
- inject ordinary errors during source scan, candidate write, validation, and
  immediately before publication;
- inject an error immediately after publication and prove B remains current
  rather than being cleaned up or rolled back; and
- cover `graph.build()`, foreground and background maintenance rebuilds,
  vacuum, projection compaction, targeted/full projection repair, durable
  ingestion, sync application, and projection garbage collection paths that
  share replacement/publication machinery.

**Work:**

- Model replacement as explicit states: `ServingOld`, `BuildingCandidate`,
  `CandidateValidated`, and `PublishedNew`.
- Keep generation A's manifest current until B is fully validated and the
  publication compare-and-swap succeeds.
- On persisted low-memory failure, reload the last published generation and
  restore backend slot metadata before a later query or status call observes
  an empty projection.
- Ensure a fresh backend always resolves the last published generation and
  never a partial candidate.
- Reject low-memory replacement before unloading when persistence is disabled
  and no recoverable copy of A exists. The diagnostic should tell the operator
  to enable persistence, raise the memory limit, or disable low-memory build.
- Remove only unpublished candidate files and sidecars. Never delete a
  candidate that won publication or the generation still referenced by the
  current manifest.
- Make recovery idempotent so the next statement can repair backend residency
  even if PostgreSQL cancellation interrupts ordinary Rust cleanup.
- Preserve build locks, source-snapshot verification, resource accounting, and
  generation compare-and-swap behavior.

**Error and interruption boundary:** do not rely only on a Rust `Drop`
implementation to repair state. PostgreSQL statement cancellation can escape
through its error boundary. The authoritative recovery fact must be the
published generation manifest, and query/status initialization must be able to
reconcile backend residency from it after an interrupted statement.

**Evidence:** forced-stage fault tests, statement-timeout and
`pg_cancel_backend()` heavy tests, old/new generation and row-count assertions,
candidate cleanup inspection, fresh-backend verification, and successful build
immediately after each failure case.

**Recorded Phase 2 evidence (2026-08-10):**

- Development-only forced faults at source scan, candidate write, validation,
  immediately before publication, and immediately after publication prove that
  pre-publication failures retain generation A while a post-publication failure
  preserves and reloads generation B.
- The PostgreSQL regression covers low-memory eviction, same-backend
  `graph.status()` reconciliation, nonrecoverable and checksum-corrupt
  persisted-generation rejection before eviction, including full checksum and
  decode validation of the relationship-identity sidecar, and generation/count
  assertions.
- `build_lock_regression.sh` cancels a persisted low-memory replacement with
  both `statement_timeout` and `pg_cancel_backend()` after loading A in the
  same backend and observing the low-memory unload. The building backend and a
  fresh backend both retain A, orphan base/manifest counts return to their
  baseline, and an immediate publisher retry reconciles its pending marker and
  publishes the three pending source rows as generation A+1. The same gate
  inventories every generation-scoped candidate/identity/temp/sidecar file and
  delivers a real `statement_timeout` from inside
  compaction and proves the backend `ENGINE` remains usable.
- Every candidate publisher, including compaction, shares the per-graph
  PostgreSQL advisory lock. Interrupted cleanup acquires that lock before
  deleting a reusable candidate generation; if another publisher owns it,
  cleanup is deferred while ordinary reads continue serving generation A and
  retain the recovery marker for a later statement.
- Compaction keeps its mmap snapshot in backend-local recovery ownership, not
  on a Rust stack whose `Arc` destructor can be skipped by PostgreSQL longjmp.
  Normal completion releases it immediately; cancellation releases it at the
  next query or `graph.status()` recovery boundary, graph switch/unload, reset,
  or replacement start.
- The recovery marker records the exact candidate generation. Cleanup unit
  tests remove only that unpublished generation's manifest, base, segment,
  chunk, relationship-identity, sidecar, and temporary artifacts while
  protecting files referenced by every other manifest and retaining a
  candidate that won publication.
- Compatibility ingestion without an existing projection manifest records the
  committed source generation selected by the ingester, so segment and
  relationship-identity candidates remain exactly recoverable.
- PostgreSQL failure/retry regressions cover vacuum, foreground and background
  maintenance, durable ingestion, durable `apply_sync`, compaction, targeted
  repair, and full repair. Projection GC remains a non-publisher and its
  existing crash/idempotency tests prove it never moves or invalidates the
  current generation.
- Full-repair reconciliation reads only the raw publication generation needed
  for exact candidate cleanup when the current manifest checksum is already
  corrupt; serving and reload paths still require full manifest validation.
  The interrupted full-repair regression proves the next repair can publish
  the intended replacement generation.
- The final serial pg17 pgrx suite passes with 1,177 tests passed, one
  intentionally ignored, and zero failures. The heavy writer-lock and real
  cancellation gate also passes against PostgreSQL 17.
- Independent Rust review found no remaining High, Medium, or Low findings
  after rechecking lock serialization, longjmp ownership, exact cleanup,
  corrupt-manifest repair, and per-graph recovery-marker isolation.

**Exit gate:** every supported replacement path is publish-on-success; a
failure or cancellation leaves generation A queryable with unchanged results,
and nonrecoverable low-memory/nonpersisted replacement is rejected before it
can clear A.

### Phase 3: Correct caller identity at the query boundary

**Purpose:** ensure every later source-table SPI query naturally runs as the
application caller before visibility logic is introduced.

**Red tests first:**

- a real login role with a `current_user` policy proves that invoker SPI and
  hydration observe the application caller (topology-intersection assertions
  begin in Phase 5);
- a minimally privileged named-graph reader can still select and query an
  authorized graph; and
- function metadata asserts invoker mode for topology query entry points and
  definer mode only for narrow catalog mediators;
- public and ordinary graph readers cannot execute cluster-wide or
  artifact-administrative telemetry; and
- `loaded_graphs()`, `graph_runtime_status()`, build/maintenance job status,
  and selected-graph status never return metadata for an ungranted graph under
  real login roles or `SET ROLE`.

**Work:**

- Remove `SECURITY DEFINER` from `graph.traverse()`,
  `graph.connected_components()`, and `graph.component_stats()`.
- Keep selected-graph and catalog authorization behind narrow, pinned
  `search_path` definer helpers.
- If catalog access fails for a restricted caller, narrow the helper rather
  than switching Rust user IDs.
- Audit every operational telemetry function in the Phase 0 inventory. Add
  explicit selected/named-graph read checks for graph-local status and
  graph-admin checks for cluster-wide generation, artifact, resource, build,
  and maintenance telemetry. Preserve physical totals and document that they
  are not RLS-row-filtered query results.
- Start the `1.0.0 -> 1.1.0` update script here, including the security-mode
  changes, ownership, and grants. Extend the same script in later phases.
- Update the function metadata audit, stable contract, and security-change
  release-note draft.

**Evidence:** real-login role results, function metadata diff, fresh-install
test, and packaged update smoke test.

**Recorded Phase 3 evidence (2026-08-10):**

- `graph.traverse()`, `graph.connected_components()`, and
  `graph.component_stats()` now run as invokers. Eight narrow catalog,
  heartbeat, and quota mediators retain `SECURITY DEFINER` with a pinned
  `pg_catalog, pg_temp` search path and capture the outer caller before
  authorization.
- A real login role protected by a `current_user` RLS policy hydrates only its
  visible source row. A minimally privileged named-graph reader can traverse,
  inspect authorized graph-local status, and cannot observe an ungranted graph
  through `status()`, `loaded_graphs()`, `graph_runtime_status()`, or named job
  status.
- Graph-local telemetry now requires selected or named read/admin grants as
  appropriate. Cluster-wide generation and backend resource telemetry require
  graph-schema administration. These remain physical operational totals and
  are explicitly documented as not row-filtered RLS query results.
- Raw build and maintenance job tables deny public reads. Their four status
  functions are pinned definer catalog mediators that authorize the captured
  outer caller, and the mutable heartbeat mediators consume backend-private
  one-shot state rather than accepting caller-supplied generations or
  watermarks. The one-shot values are bound to the originating caller and
  cleared through `PgTryBuilder::finally`, including after PostgreSQL errors,
  Rust panics, and query cancellation; injected-cancellation regressions prove
  a later direct mediator call still fails with `42501`.
- The legacy `build_status()` fallback recognizes only the synchronous zero
  UUID and only when the backend's loaded graph matches the caller's selected
  graph. A two-graph low-privilege regression proves that a hidden loaded graph
  cannot make a different selected graph report a completed build.
- `graph--1.0.0--1.1.0.sql` changes the three topology entry points to invoker
  mode and installs the pinned mediators. The exact update smoke preserves the
  registered source table, a custom traversal grant, and the traversal
  function owner across `ALTER EXTENSION graph UPDATE TO '1.1.0'`.
- The function metadata audit, real-login SQLSTATE/ACL boundary gate, SQL API
  release contract, documentation drift checks, MDX render/spell checks,
  formatting, Clippy with warnings denied, and 891-test Rust suite pass.
- The serial PostgreSQL 17 pgrx suite passes with 1,178 tests passed, one
  intentionally ignored, and zero failures. Four tests that intentionally
  contend for the graph maintenance lock were also rerun independently after
  confirming that a parallel suite reports the expected `PG006` exclusion.

**Exit gate:** source SQL sees the outer caller without manual identity
switching, named-graph authorization still works for low-privilege roles, and
all topology query functions have the intended security mode.

### Phase 4: Single-source query-start state and remove duplicate catalog work

**Purpose:** stabilize the composition seam that Phase 5 will use, and remove
known repeated SPI work before measuring RLS overhead.

**Red tests and baselines first:**

- instrument the selected fixed-work query to count catalog and sync SPI
  statements;
- retain schema-drift, pending-sync, role-grant, and freshness tests; and
- record median and p95 latency over at least 40 warm runs with
  `graph.status()` as the negative control.

**Work:**

- Change query initialization from a side-effect-only `()` result to one owned
  `QueryStartState` composition value.
- Resolve the selected graph once and call `read_catalog_for_graph(graph_id)`
  once.
- Derive fingerprints and applicable relation OIDs from that state.
- Replace `pending_sync_rows()`'s `SyncReplayContext::load()` path with a narrow
  authorized query that cannot use caller-supplied OIDs to expand access.
- Keep per-statement schema drift, freshness, ACL, and pending-sync validation.
- Do not add backend-local caching or invalidation callbacks.

**Evidence:** before/after SPI trace, fixed-work benchmark table, unchanged row
counts, and passing freshness/drift tests.

**Exit gate:** one authoritative query-start state owns the selected graph and
catalog facts, no redundant full catalog read remains, and the performance
baseline for RLS is updated to the optimized path.

### Phase 5: Prove one end-to-end RLS vertical slice

**Purpose:** validate the architecture on the smallest complete user-visible
path before plumbing every algorithm.

The vertical slice is `graph.traverse()` through seed resolution, BFS
admission, edge admission, result coordinates, and optional hydration.

**Red tests first:**

- hidden seed returns no rows;
- hidden intermediate node blocks the visible node behind it;
- hidden relationship with visible endpoints blocks traversal;
- `hydrate := true` and `hydrate := false` return the same visible topology;
- no-RLS and `BYPASSRLS` callers retain current results; and
- an RLS-active legacy edge without relationship identity fails closed with
  the rebuild-required diagnostic.

**Work:**

- Add pure `VisibilityScope` and `QueryExecutionContext` values in the module
  boundaries defined above.
- Add the single unsafe `check_enable_rls()` adapter using the outer caller and
  error-producing PostgreSQL behavior.
- Implement governed, bounded node and relationship visibility cursor scans.
- Build the scope after freshness handling and before seed resolution.
- Add `graph.rls_mode = enforce | legacy_bypass`, defaulting to `enforce`, and
  retain `graph.allow_rls_tables` as the deprecated compatibility setting.
- Gate the traverse seed, BFS candidate node, and relationship before visited,
  frontier, parent, limit, or result accounting.
- Keep table ACL checks and hydration probes as defense in depth.

**Evidence:** pure unit tests, real-login PostgreSQL tests, memory/interrupt
tests for cursor scans, diagnostic audit, and unrestricted/RLS-active benchmark
split.

**Exit gate:** the complete traverse/BFS vertical slice satisfies the RLS
intersection contract, the no-RLS path performs no visibility scan or
graph-sized allocation, and its regression stays inside the Phase 0 budget.
Do not expand to other algorithms until this gate is green.

### Phase 6: Complete direct traversal and path APIs

**Purpose:** reuse the proven scope and admission contract across every direct
reachability algorithm without adding another policy implementation.

**Red tests first:** hidden target, reverse traversal, hidden shorter path,
hidden cheaper weighted edge, bidirectional meeting through hidden topology,
transaction-local identities, and `get_neighbors()` visibility.

**Work:**

- Gate DFS and reverse iteration.
- Gate all forward, backward, and single-direction unweighted shortest-path
  expansion and the source-equals-target fast path.
- Gate Dijkstra before edge relaxation.
- Gate `get_neighbors()` and any direct coordinate-resolution entry path.
- Thread `QueryExecutionContext` alongside existing governed variants; remove
  temporary duplicated visibility parameters created during Phase 5.
- Verify search-derived seeds use the same direct traversal path.

**Evidence:** core algorithm tests, PostgreSQL path scenarios, transaction and
savepoint tests, and BFS/shortest-path benchmarks for unrestricted and active
RLS callers.

**Exit gate:** all direct traversal, neighbor, unweighted-path, and
weighted-path APIs share the same visibility scope and pass the required
semantic scenarios.

### Phase 7: Enforce RLS in derived topology and analytics

**Purpose:** close paths that read `EdgeStore` or derived graph state without
going through the direct traversal gates.

**Red tests first:** hidden nodes and edges change component membership and
counts, aggregation cannot count hidden topology, JSON coordinates cannot
smuggle hidden nodes, and path estimates match visible traversal.

**Work:**

- Gate connected-component outer scans, union operations, result construction,
  pagination, and every statistic.
- Gate both raw adjacency sites in SQL aggregation.
- Gate coordinate resolution supplied through aggregation JSON input.
- Gate path-count estimation and any other raw `EdgeStore` consumer found in
  the Phase 0 inventory.
- Compute derived results from visible topology; never compute unrestricted
  results and post-filter them.

**Evidence:** component and aggregation unit tests, PostgreSQL semantic tests,
pagination/count consistency, and production-shaped memory/runtime evidence.

**Exit gate:** components, component statistics, aggregation, and path-count
surfaces cannot observe or be influenced by hidden topology.

### Phase 8: Enforce RLS in GQL, Cypher, workflows, and write matching

**Purpose:** close the broad query executor only after the lower-level
admission contract is stable.

**Red tests first:** GQL node scans, identity lookups, joins, optional matches,
wildcard paths, Cypher lowering, workflow composition, every write MATCH
operation selecting hidden rows, and relationship `CREATE` resolving a hidden
existing source or target endpoint.

**Work:**

- Filter `source_nodes()`, identity lookup, join seeds, and optional-match
  seeds before row creation.
- Gate `append_matching_neighbors()` and `append_all_neighbors()` before GQL
  row caps and result accounting.
- Reuse the same scope through Cypher lowering and workflow delegation.
- Apply visibility when relationship `CREATE` resolves existing endpoints and
  during the read/MATCH phase of `SET`, `REMOVE`, `DELETE`, `DETACH DELETE`,
  and `MERGE`; PostgreSQL DML remains the final write authority.
- Retain current post-execution visibility probes only as defense in depth.
- Complete the real-role PostgreSQL policy matrix, including `FORCE RLS`,
  `BYPASSRLS`, session-GUC policies, `row_security = off`, partitions,
  composite keys, relationship mappings, transactions, and savepoints.

**Evidence:** GQL/Cypher read and write lifecycle suites, policy-matrix log,
workflow tests, row-cap ordering tests, and governed-resource evidence.

**Exit gate:** every topology-producing surface in the Phase 0 inventory has a
pre-topology visibility gate and a passing real-role regression test. This is
the security-complete checkpoint for pgGraph 1.1.

### Phase 9: Add relationship-typed shortest-path overloads

**Purpose:** deliver the accepted API usability improvement only after the RLS
security surface is complete.

**Red tests first:** SQL overload ambiguity, legacy call compatibility, one and
multiple type filters, unknown labels, longer/no path, weighted parity, and
filters composed with hidden RLS nodes and relationships.

**Work:**

- Add backward-compatible SQL overloads with required `edge_types text[]`.
- Resolve relationship labels once before algorithm entry.
- Apply compact type-ID filters during unweighted and weighted expansion after
  visibility admission.
- Update the update script, SQL profile, API reference, examples, and contract
  inventory.
- Add cross-engine comparison tests against a typed recursive SQL query.

**Evidence:** overload metadata, compatibility tests, recursive-query parity,
and path benchmarks with and without type filters.

**Exit gate:** existing calls are unchanged, filtered paths compose with RLS,
and callers no longer need a second graph artifact for relationship-type
restriction.

### Phase 10: Compatibility, documentation, and release candidate

**Purpose:** prove the exact candidate across installation, upgrade, artifacts,
PostgreSQL versions, documentation, and rollback.

**Work:**

- Bump live package and candidate metadata to `1.1.0` only now.
- Complete 1.1 release notes, security correction, GUC, upgrade, rebuild, and
  rollback guidance.
- Remove the resolved topology-RLS item from known issues only after Phase 8.
- Validate the final `1.0.0 -> 1.1.0` extension update path, ownership, grants,
  and function metadata.
- Load a 1.0.0 artifact under 1.1 and exercise enforced RLS; test the targeted
  rebuild-required path for artifacts without usable relationship identity.
- Run PostgreSQL 14 through 18 source, package, install, update, artifact, and
  rollback gates.
- Run the playground from clean Docker and Podman hosts with no unqualified
  registry configuration, pre-provisioned-venv reuse, and fresh dependency
  installation through `sfw`.
- Re-run unrestricted and RLS-active performance suites against the exact
  candidate.
- Archive security, correctness, performance, playground, SBOM, provenance,
  and reproducibility evidence.

**Evidence:** the full release-evidence list below, tied to one candidate commit.

**Exit gate:** the release candidate installs fresh and updates from 1.0.0 on
every supported PostgreSQL major, the public docs match its behavior, rollback
is explicit and tested to the promised level, and no unresolved Phase 0
inventory row remains.

## Compatibility and migration

### SQL contract

- Keep all 1.0 function signatures.
- Add shortest-path overloads instead of replacing existing functions.
- Record the three invoker-context corrections as explicit security changes.
- Preserve existing result row shapes.
- Add the new GUC to `release/v1-contract.json`.
- Retain `graph.allow_rls_tables` as a deprecated accepted setting.

### Extension update

The repository currently generates a fresh-install SQL file during packaging,
but 1.1 is the first stable 1.x update that must prove `ALTER EXTENSION`.

Provide an update script that:

- changes the three query functions to `SECURITY INVOKER`;
- creates the new shortest-path overloads;
- preserves function ownership and intended grants;
- leaves extension catalogs and source rows intact; and
- can run on every supported PostgreSQL major.

Test this exact sequence from the packaged 1.0.0 extension:

```sql
ALTER EXTENSION graph UPDATE TO '1.1.0';
SELECT extversion FROM pg_extension WHERE extname = 'graph';
```

### Artifact compatibility

The intended RLS design uses existing node resolution and relationship identity
data. It should keep the artifact format at v6.

Add a fixture that loads a 1.0.0 artifact under 1.1 and enforces RLS without a
rebuild. If any supported 1.0.0 artifact can lack required relationship
identity, document a targeted rebuild requirement and preserve the last valid
generation until the rebuild succeeds.

### Rollback

The release notes must state whether in-place downgrade is supported. At
minimum, support restore of the pre-upgrade backup with the matching 1.0.0
package. Do not claim that installing an older binary over 1.1 is safe without
an explicit downgrade gate.

## Documentation changes

Update these public pages in the same release:

- [`docs/known-issues.mdx`](../../docs/known-issues.mdx): remove the resolved
  builder-scoped topology-RLS limitation and retain any documented performance
  cost or side-channel boundary.
- [`docs/user_guide/administration-and-security.mdx`](../../docs/user_guide/administration-and-security.mdx):
  document enforced topology RLS, caller roles, `FORCE ROW LEVEL SECURITY`,
  `BYPASSRLS`, `legacy_bypass`, and `allow_rls_tables` deprecation.
- [`docs/user_guide/configuration.mdx`](../../docs/user_guide/configuration.mdx):
  add `graph.rls_mode`, update `graph.allow_rls_tables`, and document the safe
  `graph.low_memory_build` failure behavior.
- [`docs/user_guide/build-and-persistence.mdx`](../../docs/user_guide/build-and-persistence.mdx):
  document the generation publication point, cancellation recovery, candidate
  cleanup, and the nonpersisted low-memory rejection.
- [`docs/user_guide/sync-and-maintenance.mdx`](../../docs/user_guide/sync-and-maintenance.mdx):
  apply the same replacement guarantee to vacuum and maintenance rebuilds.
- [`docs/user_guide/querying.mdx`](../../docs/user_guide/querying.mdx): state
  hidden-seed, hidden-edge, path, count, and staleness semantics.
- [`docs/user_guide/api-reference.mdx`](../../docs/user_guide/api-reference.mdx):
  add typed shortest-path overloads and security behavior.
- [`docs/user_guide/troubleshooting.mdx`](../../docs/user_guide/troubleshooting.mdx):
  add the `::regclass` symptom and rebuild-required relationship identity error.
- [`docs/user_guide/versioning-and-compatibility.mdx`](../../docs/user_guide/versioning-and-compatibility.mdx):
  explain the security-context correction and 1.0-to-1.1 update.
- [`docs/release-notes.mdx`](../../docs/release-notes.mdx): add the complete
  1.1 security, compatibility, performance, artifact, and rollback note.
- [`docs/roadmap.mdx`](../../docs/roadmap.mdx): make RLS the 1.1 focus and
  retarget the two larger designs.

Update `README.md`, `README_zh.md`, `META.json`, `graph/Cargo.toml`, release
candidate metadata, image tags, and package validation expectations only where
they describe the live release. Do not rewrite historical 1.0 release notes or
migration fixtures to say 1.1.

## Required release evidence

The release candidate is not ready until the exact commit has retained evidence
for:

- `git diff --check` and `cargo fmt --check`;
- Rust unit and property tests;
- pgrx tests on the default PostgreSQL version;
- the real-login ACL/RLS heavy test;
- function metadata and stable diagnostic audits;
- GQL read/write lifecycle and transaction tests;
- savepoint, concurrency, sync, compaction, and crash-sensitive gates;
- statement-timeout, `pg_cancel_backend()`, low-memory, and staged build-fault
  replacement gates;
- unrestricted and RLS-active query benchmarks;
- clean Docker and Podman playground setup;
- pinned Panama dataset preparation and query-catalog validation;
- package validation and fresh install;
- packaged `ALTER EXTENSION` from 1.0.0;
- PostgreSQL 14, 15, 16, 17, and 18 matrix runs;
- docs drift, contract drift, license, SBOM, and provenance checks; and
- verified release-bundle reproducibility.

The release owner still performs signing, tagging, package publication, image
publication, and push after the gates pass.

## Explicit non-goals for 1.1

- no new topology trait or full port architecture;
- no `SetUserIdAndSecContext()` role switching;
- no lazy frontier visibility resolver;
- no cross-statement RLS cache;
- no relcache/syscache callback cache;
- no artifact format change unless a supported 1.0 fixture proves it necessary;
- no bounded batch-mutation API;
- no open-vocabulary relationship ID widening;
- no broad GQL expansion;
- no PostgreSQL 19 or SQL/PGQ surface; and
- no push or publication before the release owner approves retained evidence.

## Definition of done

pgGraph 1.1 is complete when all of these statements are true:

1. Every current local fix has a passing regression test and public issue
   disposition.
2. Every topology-producing query observes the caller's source-table RLS before
   topology becomes visible.
3. Hidden relationship rows cannot connect visible endpoints.
4. The no-RLS path avoids source scans and stays within the approved regression
   threshold.
5. Query startup no longer repeats full selected-graph and sync catalog reads.
6. Failed or cancelled build, vacuum, and maintenance replacement paths retain
   the last published generation, including the persisted low-memory path.
7. Existing shortest-path calls remain valid, and new overloads filter by
   relationship type.
8. A missing `::regclass` cast has direct troubleshooting guidance.
9. The 1.0.0-to-1.1.0 packaged update passes on PostgreSQL 14 through 18.
10. Artifact compatibility, rebuild behavior, and rollback are documented and
   tested.
11. The roadmap no longer commits batch mutations or open-vocabulary labels to
    1.1.
12. The exact release commit has complete retained evidence and a clean working
    tree.

## Locked implementation decisions

The instruction to complete every phase accepts the recommended direction
below. Reopen a decision only when implementation evidence proves the selected
contract unsafe or impossible.

| Decision | Selected answer | Verification checkpoint |
|---|---|---|
| Public RLS GUC name | `graph.rls_mode` | Phase 5 contract tests |
| Compatibility value | `legacy_bypass` rather than `bypass` | Phase 5 security tests |
| Visibility bitmaps | Store hidden node and relationship IDs in `RoaringBitmap` | Phase 5 failing unit tests |
| Shortest-path API | Same-name overload with required `edge_types text[]` | Phase 9 SQL ambiguity test |
| No-RLS regression budget | No more than 5% median or 10% p95 latency regression against the clean Phase 4 fixed-work baseline | Phases 4 and 5 benchmarks |
| `sfw`-absent behavior | Reuse a satisfying virtualenv without `sfw`; require `sfw` before any dependency installation or change | Phase 1 clean-host tests |
| Cancelled-build guarantee | Preserve and reload the last published generation; reject destructive nonpersisted low-memory replacement | Phase 2 fault-injection and cancellation tests |
| In-place downgrade | Do not promise in-place downgrade unless the packaged downgrade gate is added and passes | Phase 10 release tests |
