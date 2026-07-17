# Strategy — Turning pgGraph Into A Real Graph Engine Inside Postgres

Date: 2026-07-16
Status: strategy/plan only, no code.
Depends on: docs 01–05 in this folder.

## The Question

> How do I turn this into an actual proper fast graph engine inside Postgres,
> fully compatible with the new Postgres graph updates, to replace native
> graph databases?

## The Strategic Fact That Changes Everything

PostgreSQL 19 (Beta 1, June 2026) ships `GRAPH_TABLE` / `CREATE PROPERTY
GRAPH` per ISO SQL/PGQ. Two properties of that implementation define pgGraph's
opportunity:

1. **It is a rewrite, not a runtime.** A property graph is catalog metadata
   (`pg_propgraph`, `pg_propgraph_element`, `pg_propgraph_element_label`,
   `pg_propgraph_label`, `pg_propgraph_label_property`,
   `pg_propgraph_property`); a `GRAPH_TABLE` pattern is rewritten into a tree
   of relational joins that the ordinary planner executes. There is no graph
   execution engine in core.
2. **Fixed-depth patterns only.** Variable-length / unbounded traversal — the
   thing people actually buy graph databases for (fraud rings, reachability,
   k-hop neighborhoods) — did not land and is future work.

So Postgres 19 standardizes the *language and the graph definition* but not
the *execution*. pgGraph already has the execution: CSR adjacency, bounded
BFS/Dijkstra, layered mutable overlays. **The winning position is: Postgres
owns the syntax and the catalog; pgGraph becomes the runtime that makes deep
and variable-length patterns fast.** That is the same relationship pgvector
has to `ORDER BY … <->`: standard-looking SQL, extension-provided index/
executor that makes it viable.

Everything below is organized around five pillars.

---

## Pillar A — SQL/PGQ Compatibility (the "fully compatible" part)

### A1. Consume the PG19 property-graph catalog as the registration source (near-term, high value, low risk)

Today pgGraph has its own registration (`graph.add_node_table()`,
`graph.add_edge()`). PG19 users will define graphs once with
`CREATE PROPERTY GRAPH`. Build a bridge:

- `graph.import_property_graph('mygraph')` — read `pg_propgraph_*`, map
  vertex tables → registered node tables, edge tables → registered edges,
  labels → pgGraph labels, properties → filter columns. One definition of the
  graph, zero duplicate registration. `pg_get_propgraphdef()` gives a
  dump-stable text form for drift checks.
- Keep it one-way (property graph → pgGraph registration) with a drift
  detector in `graph.status()`/validation: if the property graph definition
  changes, flag `needs_rebuild` with a reason.
- On PG <19, the existing registration API remains the only path; the bridge
  is `#[cfg]`/version-gated.

This is cheap, shippable before PG19 GA, and makes the marketing sentence
true on day one: "define your graph with the Postgres 19 standard, query it
1000× faster with pgGraph."

### A2. Same-semantics query surface for what PG19 can't do (the wedge)

PG19 `GRAPH_TABLE` rejects variable-length quantifiers. Offer the missing
piece with identical pattern semantics:

- `graph.graph_table('mygraph', 'MATCH (a)-[e:KNOWS]->{1,5}(b) WHERE … COLUMNS (…)')`
  — accept the *standard SQL/PGQ pattern grammar* (the part inside
  GRAPH_TABLE), including the variable-length forms core rejects, resolved
  against the same property-graph catalog imported in A1.
- This is a deliberate, scoped exception to the current "pgGraph does not
  parse SQL/PGQ text" stance (`query/sqlpgq_adapter.rs:1-5`). The philosophy
  ("PostgreSQL owns SQL parsing") was right when there was no standard surface
  in core; now that PG19 defines the grammar and catalog, matching it exactly
  is compatibility, not competition. The existing GQL parser
  (`gql/parser.rs`) already covers most of the pattern grammar — SQL/PGQ
  patterns and GQL patterns are siblings from the same ISO family — so this
  is a frontend adaptation, not a new parser.
- Users learn one syntax. Queries that core can execute run in core; queries
  that need depth run through pgGraph; both read the same graph definition.

### A3. Transparent acceleration of core GRAPH_TABLE (the long game)

Ideal end state: `SELECT … FROM GRAPH_TABLE (mygraph MATCH …)` silently uses
pgGraph when eligible. Realistically, in order of feasibility:

1. **Planner-hook + CustomScan provider.** Because GRAPH_TABLE lowers to an
   ordinary join tree, an extension can in principle recognize the shape via
   planner hooks and substitute a `CustomScan` that runs the pgGraph
   traversal. The catch: after rewrite the "this came from GRAPH_TABLE"
   provenance may be gone, making recognition fragile. Prototype against
   PG19 beta to find out whether the rewritten RTEs carry anything
   identifiable.
2. **Upstream contribution.** The clean fix is a hook in core: either a
   provenance marker on GRAPH_TABLE-derived RTEs, or a proper "graph pattern
   handler" hook (the thing the repo docs already anticipate as "stable
   graph-pattern hooks"). Variable-length paths are already core's declared
   future work — showing up on pgsql-hackers with a working CSR runtime,
   LDBC numbers, and a concrete hook proposal is how pgGraph shapes PG20/21
   instead of chasing it. This is the highest-leverage long-term move the
   project can make.
3. **Interim without hooks:** A2's `graph.graph_table()` is the compatible
   surface, and the docs position it as "same standard, runs today."

The typed adapter seam (`query/sqlpgq_adapter.rs`, compatibility matrix
already drafted) is exactly the right internal boundary for all three — it
just needs a producer.

---

## Pillar B — Performance: actually beating native graph databases

"Fast graph engine" means winning benchmarks people trust. The engine core is
good; the losses today are around it.

### B1. Kill the fixed costs (from docs 03/05, now perf-critical)

- **Per-generation load validation cache** — full-file CRC + full content walk
  per backend (`persistence.rs:312-408,773`) is the likely source of the
  ~4.4 s first-query cost. Neo4j has no per-connection warmup; this must go.
  Validate once per artifact generation cluster-wide (a validity marker keyed
  by generation + CRC in a small catalog table), not per backend.
- **Stop cloning tenant maps per traversal** (`engine.rs:733-737`); borrow in
  `BfsConfig`.
- **Empty-overlay short-circuit** (`engine.rs:725,816-871`) so clean graphs
  read at pure CSR speed.

### B2. Shared memory, not per-backend heaps

Reverse CSR, filter index, and edge-type registry are rebuilt per backend
(`architecture-tradeoffs.mdx:29`). At 50 backends that's 50× the heap and 50×
the deserialize cost. Move them into mmap-friendly artifact sections (already
an open question in the tradeoffs doc — answer it: yes). Target state: a
backend's private state is only overlays + tx deltas; everything immutable is
page-cache-shared.

### B3. Execution upgrades (benchmark-driven, in this order)

1. **Direction-optimizing BFS** (top-down/bottom-up switching) — the single
   biggest classic win for BFS on social-topology graphs; needs the reverse
   CSR, which B2 makes shared.
2. **Fix bidirectional BFS minimality** (doc 03 M1) before publishing any
   shortest-path benchmark — a wrong answer in a bench war is fatal.
3. **Weighted paths over overlays** (`engine.rs:966-980`) and delta-stepping
   or bucket-based Dijkstra if profiles justify it.
4. **Parallel build**: the reserved `build_scan_mode = 'copy'` scanner, plus
   parallel CSR construction. An 87 s Panama build (2M nodes/5.8M edges)
   should be <15 s; build time is part of the "replace a graph DB" story
   because it bounds recovery and schema-change agility.
5. **Parallel traversal for analytics** via background workers (components,
   PageRank), never in the OLTP path — keeps the circuit-breaker philosophy.

### B4. Prove it publicly

- **LDBC SNB** interactive + BI subsets (an `ldbc` preset already exists in
  the playground) vs. Neo4j, Memgaph, KùzuDB, and — critically — vs. **plain
  PG19 GRAPH_TABLE** on the same box. The last comparison is the marketing
  one: "standard syntax, N× faster with the extension."
- Publish the harness in-repo (extends the existing
  `sandbox/benchmark`/launch-evidence discipline, which is already strong).

---

## Pillar C — Writes: the live/hybrid graph (already planned)

Doc 02 covers this fully; it is a prerequisite for "replace a graph database"
because native graph DBs have no rebuild step at all. Summary of what must
land: ingest advisory lock, sync-log retention, dead-lettering (P0s from
doc 05); auto-maintenance consuming `sync_health()` signals; weighted paths
over overlays; `projection_mode = 'auto'`; then **WAL/logical-decoding sync**
(`sync_mode = 'wal'` is already reserved) so high-write tables don't pay
per-row trigger overhead. WAL sync is what makes "the graph is just always
current" honest at OLTP write rates.

## Pillar D — Feature completeness (what graph-DB refugees will ask for)

In rough order of demand:

1. **Variable-length paths in the standard syntax** (Pillar A2 — the wedge).
2. **Path algorithm variants**: ALL SHORTEST, k-shortest, cheapest-with-
   constraints. The BFS/Dijkstra core supports these as variations.
3. **Analytics library as scheduled/background operations**: PageRank,
   Louvain/label propagation, betweenness (approximate), triangle counting.
   Run via background workers writing results to tables — never inline in
   OLTP. Components already set the pattern (`connected_components.rs`).
4. **Semantic/GraphRAG search** (roadmap's `guided_path()` + pgvector) — this
   is the differentiator no native graph DB has natively, and it aligns with
   the polygres.com managed positioning.
5. **openCypher surface**: keep `graph.cypher()` as a migration aid
   (compatibility matrix honesty as today), not a compatibility promise.
   The standards bet (GQL + SQL/PGQ) is correct — Neo4j itself is moving to
   GQL.

## Pillar E — Trust: the things that disqualify you in evaluations

From docs 03/04 — these are adoption blockers, not polish:

- **RLS boundary on topology reads** (doc 03 H1) — a security reviewer finds
  this in week one of any serious evaluation. Fix or gate + document before
  courting production users.
- **Tenant isolation semantics** (H3) — same reviewer, same week.
- **Automated CI on every PR** (doc 04 D2) — "no CI" reads as "not a serious
  project" to exactly the audience this strategy targets.
- Truncation signals (M2) — silent partial results are the kind of story that
  ends up in a "why we went back to Neo4j" blog post.

---

## Sequencing

**Now → PG19 GA (~fall 2026):**
P0 security/correctness fixes (doc 05) · CI · property-graph catalog import
(A1) · load-validation cache + fixed-cost perf wins (B1) · LDBC harness
baseline including PG19-beta GRAPH_TABLE comparison.

**GA + 0–6 months:**
`graph.graph_table()` standard-pattern surface with variable-length support
(A2) · live-mode automation + WAL sync design (C) · shared reverse
CSR/filter index (B2) · direction-optimizing BFS (B3) · publish LDBC numbers.

**6–18 months:**
CustomScan prototype against PG19/20 (A3) · pgsql-hackers hook proposal with
benchmarks · analytics library (D3) · guided_path/GraphRAG (D4) · parallel
build/COPY scanner.

## Honest Bounds

Where this wins: data already in Postgres (the majority case), transactional
consistency with source rows, ops/backup/HA story, cost, GraphRAG-on-Postgres,
and — post-PG19 — standard syntax. Where it does not win, and shouldn't
pretend to: billion-edge whole-graph analytics (distributed engines),
graph-native storage workloads where the graph *is* the primary data and
tables are the afterthought, and sub-millisecond p99 traversal SLAs on cold
backends until B1/B2 land. "Replace native graph databases" is achievable for
the 80% of graph workloads that are bounded traversal over operational
relational data — which is precisely the segment PG19 just legitimized and
left unserved on performance.
