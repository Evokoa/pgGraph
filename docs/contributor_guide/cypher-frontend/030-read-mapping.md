# Read-side mapping: `cyrs_plan::ReadOp` → pgGraph execution

Read operators are a tree. The facade walks the tree post-order, with
two emission strategies per node:

- **SQL push** — emit a SQL fragment, accumulate it into a query the
  pgGraph engine or a SPI call executes. Cheap, leverages Postgres'
  planner.
- **Row eval** — materialise rows from the child, evaluate in Rust
  using `row_eval.rs`. Required when an op references graph-shaped
  values (paths, lists of nodes, maps) that don't have natural SQL
  expressions, or when an `Expr` includes a function we haven't
  classified as push-safe (see `050-expr-and-types.md`).

The decision is per node, made bottom-up: if any descendant required
row eval, the parent does too (we don't reshape rows back into SQL).

## Operator catalogue

### `Source { label, bind }`

**Translation:** look up `label` in `_registered_labels`. Two cases:

1. *No discriminator:* `SELECT <id_col> AS __id, <prop_cols...> FROM
   <table_name>`.
2. *With discriminator:* same, plus `WHERE <discriminator_col> =
   <discriminator_val>`.

The `bind` VarId records the resulting node's identity for downstream
ops. We materialise node identity as `(regclass, text)` to match
pgGraph's existing engine contract.

**Tenant scoping:** if the registered table has a `tenant_column` and
the session has a tenant set (`pgGraph.tenant` GUC), append
`AND <tenant_column> = current_setting('pgGraph.tenant')`.

**`label = None` (all-node scan):** `UNION ALL` over every registered
table that has at least one label registered against it. Diagnose
"label-free Source with empty catalog" as a host-range error.

### `Expand { from, rel, to, bind_rel, bind_to, input }`

The single most important operator and the one that benefits most
from pgGraph's engine.

#### Single-hop (`RelLength::Single`)

- If `from` is already a single concrete node (resolved upstream),
  call `engine::Engine::adjacent(from, rel.types, rel.direction)`
  directly.
- If `from` is a stream of nodes, batch: collect into a
  `Vec<(regclass, id)>`, then call a new
  `engine::Engine::adjacent_batch(...)` (small wrapper around the
  existing per-node call).
- `to.labels` and `to.properties` apply as a Filter on the resulting
  rows.

#### Variable-length (`RelLength::Variable { min, max }`)

This is exactly `graph.traverse(...)`. Build a `TraverseRequest`:

```rust
TraverseRequest {
    root_table: pg_class_of(from.label),
    root_id:    from.id,
    max_depth:  max.unwrap_or(default_max_depth) as i32,
    edge_types: Some(rel.types),
    direction:  rel.direction.into(),
    node_tables: to_node_tables_for(&to.labels),
    filter:     expr_to_filter_jsonb(&to.properties),
    strategy:   "bfs",
    uniqueness: "node_global",
    include_start: false,
    hydrate: true,
    max_rows: default_max_rows,
    row_offset: 0,
    max_nodes: default_max_nodes,
    max_frontier: default_max_frontier,
}
```

Call `execute_traverse_rows` (already exists). The result already has
the path JSONB and edge-path JSONB; we keep those for `RETURN p`
support.

Edge cases:

- `min = 0`: include the start node as a zero-length match. We handle
  this in the row evaluator, not by re-running traverse — we emit a
  union of `{depth=0, node=from}` and the traverse result.
- `min > 1`: post-filter by `depth >= min`. The traversal already
  enumerates from depth 1 upward.
- `max = None` (unbounded): use the configured `graph.default_max_depth`
  as a hard cap, and emit a `NOTICE` if the cap was hit. Cypher does
  not actually allow truly unbounded varlen in practice — there's
  always a cap somewhere.

### `Filter { input, predicate }`

Two-stage strategy:

1. **Push the pushable.** Walk `predicate` in CNF; each conjunct that
   is composed entirely of push-safe functions and references
   columns we already pulled in `Source` / `Expand` becomes a SQL
   `WHERE` fragment.
2. **Row-eval the rest.** Remaining conjuncts run over the
   materialised stream from the child.

Cypher null/3VL: a `Filter` drops both `false` and `null` rows (spec
§12.1 N3). SQL `WHERE` already drops `null`. They align. (See
`feat-request.md` §5.1.)

### `Project { input, items }` / `With { input, items, filter }`

- For each `Projection`, evaluate its `Expr` either in SQL (as part
  of the surrounding SELECT) or in Rust.
- `With` has the same shape as `Project` but with an optional
  trailing filter and a new scope barrier; downstream variable
  references after a `With` are scoped to its output items only.
- Star-projection (`RETURN *`) was already expanded by HIR into an
  explicit item list. Nothing special here.

### `Aggregate { input, keys, aggs }`

- If `input` materialised to SQL: `SELECT <keys>, <aggs> FROM (...) GROUP BY <keys>`.
- If row-eval: stream rows into a `HashMap<key-tuple, accumulator>`.
- Empty `keys` → single output row even on empty input (Cypher
  semantics; see `feat-request.md` §5.2).
- Supported aggregates: `count`, `count(*)`, `sum`, `avg`, `min`,
  `max`, `collect`. `stDev`/`stDevP`/`percentile*` deferred to a
  later milestone.

### `OrderBy { input, keys }` / `Skip { input, count }` / `Limit { input, count }`

SQL surface for both. Cypher's sort is stable and uses Cypher
ordering, which doesn't match SQL's `ORDER BY` for mixed-type columns
(e.g. `null` last vs `null` first). Two-stage approach:

- If keys are all of a homogeneous primitive type and not mixed with
  null-ordering rules, push to SQL with explicit `NULLS LAST`.
- Otherwise row-eval with the Cypher ordering predicate.

`count` for `Skip` / `Limit` is an `Expr`; we evaluate it at the
start of execution (must be a constant integer; sema enforces).

### `Distinct { input }`

`SELECT DISTINCT` if SQL-pushed; otherwise a `HashSet<row-fingerprint>`
in row-eval. The row fingerprint must respect Cypher value equality
(`null != null`), which differs from SQL's `DISTINCT` (`null = null`).
That's a gotcha — when rows contain nulls, we have to row-eval.

### `Unwind { input, list, bind }`

- If `list` evaluates to a SQL array, use `unnest(list)`.
- If `list` is a Cypher list value (JSONB array, possibly heterogeneous),
  row-eval iterating the JSONB elements.

### `Union { left, right, kind }`

Run both sub-plans, emit rows of `left` then `right`. `UnionKind::All`
keeps duplicates; `UnionKind::Distinct` dedups by Cypher value
equality (same gotcha as `Distinct`).

### `OptionalJoin { input, pattern }`

Conceptually a left outer join: for each row of `input`, evaluate
`pattern`; if it produces zero rows, emit one row with every variable
introduced by `pattern` bound to `null`.

Implementation: lateral execution.

```sql
SELECT outer.*, inner.*
FROM (<input>) outer
LEFT JOIN LATERAL (<pattern>) inner ON true
```

If `pattern` is row-eval, do it explicitly in Rust: for each outer
row, call the inner sub-plan; if empty, emit one null-bound row.

### `ShortestPath { ... }` — pending upstream cyrs

This op doesn't exist yet (see `feat-request.md` §1.1). Once it
lands, route directly to `path_finder.rs`. Until then, we reject
`shortestPath(...)` / `allShortestPaths(...)` with a host-range
diagnostic explaining the limitation.

## Path materialisation (`MATCH p = ...`)

Pending the upstream clarification in `feat-request.md` §1.2. Our
working assumption (subject to confirmation):

- A `VarKind::Path` binding produces a Plan-level value whose runtime
  representation is a list-of-(node, rel, node) triples.
- pgGraph already produces this as `path JSONB` from
  `execute_traverse_rows`.
- Functions over paths (`length(p)`, `nodes(p)`, `relationships(p)`)
  consume this JSONB.

If cyrs surfaces a more structured Plan-level path constructor, we'll
match that shape.

## Heuristics: when to row-eval vs push to SQL

Default: **push to SQL until something forces row-eval**.

Forces row-eval (sticky from that operator upward):

- Any `Expr::Call` to a function not classified push-safe.
- Any reference to a Cypher-typed value with no natural SQL projection
  (paths, lists-of-nodes, maps).
- `OptionalJoin` where the inner pattern itself row-evals.
- Cypher value-equality semantics where SQL's would diverge
  (`Distinct`/`Union Distinct` on rows containing null).

Profile-driven re-tuning is a v2 concern. v1 picks the obvious
strategy per op and ships.
