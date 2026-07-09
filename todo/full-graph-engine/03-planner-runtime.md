# Planner And Streaming Runtime

## Goals

- Execute GQL as composable binding-table operators.
- Choose plans from graph and PostgreSQL statistics rather than source order.
- Stream rows in bounded batches and spill blocking operators.
- Push identity, property, visibility, and relational work to PostgreSQL when
  PostgreSQL is the cheaper authoritative executor.
- Expose estimates, budgets, and decisions through explain.

## Canonical Logical IR

Core logical operators:

```text
GraphScan / IdentityLookup / PropertyIndexScan
Expand / ExpandInto / PathSearch
Filter / Project / Unwind
InnerJoin / LeftJoin / SemiJoin / AntiJoin
Aggregate / Distinct / Sort / TopK / Window
Union / Intersect / Except
Subquery / Exists
Insert / Update / Remove / Delete
VisibilityFilter / Hydrate
```

Logical rows carry typed slots for nodes, relationships, paths, scalars, and
records. Relationship slots always carry `RelationshipId`; node slots carry
source table and stable PK identity. JSON serialization happens at the outer
SQL boundary only.

## Statistics And Costing

Maintain versioned statistics per graph generation:

- label/table cardinality and active/tombstone counts;
- relationship type/mapping cardinality by direction;
- in/out degree histograms, heavy hitters, and zero-degree rate;
- property null fraction, distinct count, ranges, and selected histograms;
- tenant/selectivity summaries that do not leak values to unauthorized roles;
- overlay/segment depth, dirty source ranges, and expected merge cost;
- source visibility/hydration batch and PostgreSQL index estimates.

Use PostgreSQL statistics where mappings align, supplemented by projection
statistics. Invalidate plans on catalog fingerprint, artifact generation,
relevant statistics, role/security context, or semantic GUC change.

Cost dimensions include expected rows, expansions, random/sequential bytes,
private memory, spill bytes, PostgreSQL calls, and parallel/concurrency cost.
The optimizer must compare scan/expand order, join reorder, expand-into,
identity/property lookup, nested-loop/hash/merge join, top-k versus full sort,
and sparse versus dense path state.

## Physical Runtime

Use synchronous, pull-based batch operators suited to a PostgreSQL backend:

```rust
trait Operator {
    fn next_batch(
        &mut self,
        cx: &mut ExecutionContext,
        out: &mut RowBatch,
    ) -> GraphResult<BatchState>;
}
```

`ExecutionContext` owns the snapshot contract, graph generation pin, role and
tenant context, resource leases, spill manager, counters, and interrupt
cadence. Do not introduce an async runtime into backend execution.

### Streaming Rules

- Source scans iterate bitmaps/ranges lazily and honor safe LIMIT early stop.
- Neighbor expansion merges base, segment, sync, and transaction sources without
  a degree-sized materialization.
- Hydration and RLS visibility use table-wise key batches, not per-coordinate
  SPI calls.
- Sort, DISTINCT, grouping, and eligible joins use bounded in-memory runs and
  external merge.
- Path search is an iterative state machine with explicit work/frontier/path
  budgets. It fails deterministically when a semantic result cannot be
  completed within the declared bound.
- Output is yielded as batches to the SQL SRF; avoid collecting the final JSONB
  result twice.
- Long loops check PostgreSQL interrupts by work count, not only wall time.

### Spill Manager

One statement spill manager creates secure operation-scoped files, enforces
disk/file quotas, checks free space, encrypts or avoids sensitive cleartext
when required by the deployment policy, fsyncs only where recovery semantics
need it, and cleans up on success, error, interrupt, and backend exit.

Spillable operators encode typed rows with version and checksum. Query spill
does not need crash recovery; build/compaction staging does.

### Path Algorithms

Keep specialized algorithms behind common operator contracts:

- bidirectional and single-direction BFS;
- weighted Dijkstra with sparse/dense state choice;
- shortest-path and path-mode automata required by GQL;
- bounded k-shortest paths when added;
- worker-only whole-graph analytics.

A planner selects an algorithm from directionality, weights, predicates,
statistics, semantic path mode, available memory, and requested result shape.
No algorithm receives an implicit whole-graph allocation entitlement.

## PostgreSQL Integration

Retain `graph.gql()` as a compatibility facade. Investigate typed set-returning
descriptors and eligible PostgreSQL planner integration so relational
predicates, joins, and costs are visible instead of hiding all work behind one
`VOLATILE` JSONB SRF.

Potential integration, gated by supported-version APIs:

- parameter-aware cost and row estimates;
- predicate/projection pushdown;
- plan invalidation hooks;
- custom scan only for stable, eligible shapes;
- native PostgreSQL 19 property-graph catalog/SQL-PGQ adapter;
- explain fields that combine PostgreSQL and graph work.

Never cache visible row sets or hydrated values across statements. A prepared
plan may cache immutable structure only when role, RLS-sensitive context,
catalog, generation, and GUC invalidation are correct.

## Performance Program

Define scorecards by workload class instead of promising universal wins:

| Workload | Intended advantage |
|---|---|
| PostgreSQL-resident operational graph | No ETL, authoritative DML, local indexes, transaction semantics |
| Warm bounded traversal | Compact CSR, mmap page sharing, typed filters |
| Selective property + graph query | PostgreSQL index pushdown plus graph expansion |
| Continuous source updates | Delta projection and bounded catch-up |
| Large build under small memory | External runs and predictable spill |
| Whole-graph analytics | Quota-controlled workers; not the OLTP backend path |

Compare identical data and semantics, publish hardware/configuration, validate
results, and report cold/warm p50/p95/p99, throughput, RSS/PSS, spill, build,
load, sync lag, and storage amplification.

## Acceptance Gates

- Explain includes estimated/actual rows, expansions, memory, spill, and chosen
  operators.
- Reordering never changes results; differential plan tests compare alternatives.
- `LIMIT 1` over a huge label has bounded source-scan memory.
- A supernode and an unreachable bounded path obey work/memory limits.
- Wide rows, DISTINCT, grouping, and sort spill below query memory cap.
- Batched hydration and visibility remove per-coordinate SPI behavior.
- Cancellation interrupts every operator within a bounded work interval.
- The same logical IR serves GQL and the SQL/PGQ adapter.
