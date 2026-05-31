# Phase 3 Design: Advanced Read GQL And SQL/PGQ Adapter

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

Phase 3 broadens the read surface once Phase 1 proves the shared read planner,
and adds SQL/PGQ as an **adapter** into the same IR (Q3). No new query-language
directory for SQL/PGQ — it lowers into `graph/src/query/`.

## 0. Entry conditions

- Phase 1 read planner stable; logical IR unchanged for any phase_1 row.
- IR frontend boundary confirmed neutral (no GQL-specific assumptions below
  `logical_plan.rs`).

## 1. Grammar additions (matrix `phase_3` rows)

```ebnf
query        = match_clause , { with_clause , [ match_clause ] } ,
               [ where_clause ] , return_clause , ... ;
match_clause = ( "OPTIONAL" )? , "MATCH" , pattern ;
with_clause  = "WITH" , [ "DISTINCT" ] , return_item , { "," , return_item } ,
               [ where_clause ] , [ order_by ] , [ skip ] , [ limit ] ;
return_item  = ( expr | aggregate | path_func ) , [ "AS" , alias ] ;
aggregate    = ( "count" | "sum" | "avg" | "min" | "max" | "collect" ) ,
               "(" , [ "DISTINCT" ] , ( expr | "*" ) , ")" ;
path_func    = ( "nodes" | "relationships" | "length" ) , "(" , path_var , ")" ;
```

Tokens added: `Optional`, `With`, plus aggregate/path-func names parsed as
`Ident` and resolved at bind time (not reserved keywords, to avoid clashing with
property names).

## 2. Binding scope chain (slice 3A — the enabling change)

`WITH` is the structural addition: it closes one variable scope and opens the
next. The binder becomes a **scope stack** instead of a single binding set.
Each `WITH`/`RETURN` projection defines the variables visible downstream;
`OPTIONAL MATCH` marks its pattern's new variables as nullable in the scope.
This is the largest Phase-3 design risk — build it first and prove it before the
feature rows below.

## 3. IR + physical operators activated here

Logical: `Optional` (left-outer over a pattern), `Aggregate { group_keys,
aggregations }`, `Distinct`, multi-stage `Project`, `Join`. Physical: `HashJoin`,
`NestedLoopBounded`, `AggregateRows` (beyond `count`), `DistinctRows`. `Optional`
lowers to a left-outer expand that null-extends unmatched rows.

## 4. JSONB type expansion (Q2 follow-through, slice 3F)

`jsonb`-backed dynamic/open properties, mixed-type lists, and maps arrive here.
Define the **missing-key vs explicit-`null`** rule for jsonb property bags
(distinct from the Phase-1 typed-column rule, where missing can't occur).
Typed columns keep the fast filter-index path; jsonb properties use
hydration-time evaluation. Update the canonical JSONB result shape doc with the
list/map encoding.

## 5. SQL/PGQ adapter (slice 3G)

When PostgreSQL exposes stable SQL/PGQ graph-table definitions or pattern hooks,
map eligible graph patterns to pgGraph projections by lowering into the shared
IR; unsupported SQL/PGQ stays PostgreSQL's responsibility. The adapter is a new
lowering entry into `graph/src/query/`, **not** a parser. A separate
compatibility matrix tracks SQL/PGQ coverage; unmappable patterns are rejected
with stable diagnostics.

## 6. PR slices (TDD order)

- **3A — `WITH` + scope chain.** Binder scope stack; multi-stage projection.
  Tests: variable visibility across stages, shadowing rules, scope-leak negatives.

  Status, 2026-05-31: projection-stage `WITH` is implemented for the current
  single-pattern executor. It supports aliases, shadowing, scalar property
  aliases, and leak-negative binding tests. `WITH ... MATCH ...` remains a
  later multi-pattern planner task because it requires row-stream joins rather
  than only scope rebinding.
- **3B — `OPTIONAL MATCH`.** Null-extension. Tests vs equivalent left-outer SQL.

  Status, 2026-05-31: top-level single-relationship `OPTIONAL MATCH` is
  implemented. Unmatched source rows and target-predicate misses return JSON
  `null` for target/relationship projections, and SQL tests compare the result
  shape against equivalent left-outer SQL. Node-only optional matches and
  post-`WITH` optional joins remain later multi-pattern planner work.
- **3C — Aggregates.** `count` (exists) → `sum`/`avg`/`min`/`max`/`collect`;
  grouping. Tests: correctness vs SQL aggregation, empty-group, null handling.
- **3D — `DISTINCT`.** With memory limit (DR-2 style). Tests: dedup correctness,
  over-limit abort.
- **3E — Path functions.** `nodes`/`relationships`/`length` over a stable path
  value model. Tests: path value-shape snapshots.
- **3F — jsonb properties.** Dynamic/list/map type mapping; missing-vs-null rule.
- **3G — SQL/PGQ adapter.** Success + rejection corpus; own compatibility matrix.

Benchmark gate unchanged: existing CSR traversal must not regress.
