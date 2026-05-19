# Write-side mapping: `cyrs_plan::WriteOp` → SPI DML

Write ops form a `Vec<WriteOp>` on `PlanStatement`. They execute
**per output row of the read tree**, in order. Each op may bind a new
variable visible to subsequent ops on the same row. Atomicity is
inherited from the surrounding Postgres transaction.

Identities used here:

- **Node identity** = `(table_name, id)` where `id` is text matching
  the registered `id_column`.
- **Relationship identity** = `(rel_type, from_id, to_id)` and, for
  junction-table edges, also the row's primary key in the junction
  table.

## `CreateNode { labels, props, bind }`

1. Look up the (unique) label in `_registered_labels`. Multi-label
   `labels.len() > 1` requires the sorted set to appear in
   `_registered_label_sets`; otherwise reject with host-range
   diagnostic. (Tracked at sema time via `labels_compatible` —
   `feat-request.md` §2.3 — but we re-check at execution to defend
   against catalog races.)
2. Evaluate `props` to a `serde_json::Map<String, Value>`.
3. For each property: look up `_registered_label_properties`; map
   `property → column_name`. Properties without a mapping become an
   error unless we add a "spill to JSONB column" convention later
   (deferred).
4. Build:

   ```sql
   INSERT INTO <table_name> (<col_a>, <col_b>, ...)
   VALUES ($1, $2, ...)
   RETURNING <id_column>
   ```

   Execute via `Spi::run_with_args`. The returned `id` populates the
   `bind` variable for downstream ops.

`NOT NULL` columns without defaults that are missing from `props`
become a write-time error from Postgres. We surface those as
`ereport(ERROR)` with the original SQLSTATE.

Multi-label with discriminator: if any of the labels uses a
discriminator column, set it explicitly in the INSERT.

## `CreateRel { from, to, rel_type, props, bind }`

Lookup `_registered_rel_types[rel_type]`. Two structural cases
depending on the underlying edge shape:

### Case A — FK column on `from_table`

```sql
UPDATE <from_table>
SET <from_column> = $to_id
WHERE <id_column> = $from_id
```

Set-typed FKs are not in scope (one-to-many requires a junction).

### Case B — Junction table

```sql
INSERT INTO <junction_table> (
    <from_id_col>, <to_id_col>, <label_column?>, <props_cols...>
) VALUES ($1, $2, $3, ...)
RETURNING <junction_pk>
```

`label_column` only present if the registered edge is polymorphic on
type (one junction holds multiple rel types).

In both cases the `bind` variable carries a relationship identity for
downstream `SetProperty` / `Delete` calls.

## `MergeNode { labels, props, on_create, on_match, bind }`

**Sema precondition** (relies on `feat-request.md` §2.1 / §2.2):
the planner identified `key_props: Vec<SmolStr>` as the determinism
key. Sema validated, via `SchemaProvider::label_unique_props`, that
these props correspond to a registered `_registered_unique_props` row,
which itself was validated to correspond to a real Postgres unique
constraint at registration time.

Execution:

```sql
INSERT INTO <table> (<all_columns_from_props>)
VALUES ($1, $2, ...)
ON CONFLICT (<key_cols>) DO UPDATE
    SET <key_cols[0]> = EXCLUDED.<key_cols[0]>  -- no-op, just to fire RETURNING
RETURNING <id_column>, (xmax = 0) AS __was_created
```

`__was_created` selects between `on_create` and `on_match`. We then
execute each `WriteOp` in the chosen vec, in order, with `bind`
already populated.

Race-free because the constraint is real Postgres uniqueness; the
upsert is atomic.

## `MergeRel { from, to, rel_type, props, on_create, on_match, bind }`

Two cases mirror `CreateRel`:

- **FK column:** can't really MERGE — an FK column either has a value
  or it doesn't. We treat MERGE as "if from.<col> is NULL set it,
  else verify it equals to_id; if it doesn't, choose `on_match`
  side." Rare in practice.
- **Junction table:** requires a `UNIQUE (from_col, to_col[, type])`
  on the junction. `_registered_unique_props` records this. Then:

  ```sql
  INSERT INTO <junction_table> (<from_col>, <to_col>, [<type_col>,] <props...>)
  VALUES ($1, $2, [$3,] ...)
  ON CONFLICT (<from_col>, <to_col>[, <type_col>]) DO UPDATE
      SET <from_col> = EXCLUDED.<from_col>
  RETURNING <junction_pk>, (xmax = 0) AS __was_created
  ```

If the catalog doesn't record a matching uniqueness tuple, MERGE on
this rel is rejected at sema time. No silent SELECT-then-INSERT
fallback — that would violate atomicity guarantees Cypher users
rightfully expect.

## `SetProperty { target, prop, value }`

`target` resolves to a `(table_or_junction, id)`. Look up `prop` in
the appropriate property table. Emit:

```sql
UPDATE <table> SET <column_name> = $value WHERE <id_col> = $id
```

`value` is an `Expr` evaluated to a Cypher value, then cast to the
column type via the type-bridge (`050-expr-and-types.md`). Cast
failures (e.g. assigning a list into a scalar column) become
`ereport(ERROR, ..., SQLSTATE 22023)`.

## `SetLabels { target, labels }`

Hard case. Three possible interpretations:

1. **Discriminator-column model:** `UPDATE <table> SET <disc_col> =
   $new_label`. Trivial when applicable.
2. **Multi-table model:** moving a row between tables. We do not
   support this in v1 — reject with host-range diagnostic
   `E45xx — label set arithmetic not supported by storage model`.
3. **Junction-row model:** insert a row into a per-row `_node_labels`
   junction. Not in scope for v1; tracked in `080-open-questions.md`.

The diagnostic message names the underlying constraint
("`SET n:Foo:Bar` cannot run because `Person` is stored without a
discriminator column"). The user fix is to `register_label_property`
with an explicit discriminator.

## `RemoveProperty { target, prop }`

```sql
UPDATE <table> SET <column_name> = NULL WHERE <id_col> = $id
```

Reject if the column is `NOT NULL` (host-range diagnostic citing the
catalog's `required = true`).

## `RemoveLabels { target, labels }`

Mirror of `SetLabels`. Same v1 restriction: only the discriminator
model is supported.

## `Delete { targets, detach: false }`

For each `target` expr (must evaluate to a node or relationship
reference):

```sql
DELETE FROM <table> WHERE <id_col> = $id
```

Postgres enforces "no orphaned edges" via FKs on the registered
edges. If a non-`DETACH DELETE` would orphan an edge, Postgres raises
`23503` (`foreign_key_violation`); we map it to a Cypher-style
diagnostic citing the rel type that blocked the delete.

## `Delete { targets, detach: true }`

`DETACH DELETE` removes incident edges first.

pgGraph already knows the incident edges (the index). For each
target node:

1. Look up all `_registered_rel_types` whose `from_table` or
   `to_table` matches the target's table.
2. For each, issue:
   - If junction edge: `DELETE FROM <junction> WHERE <from_col> =
     $id OR <to_col> = $id`.
   - If FK edge: `UPDATE <neighbor_table> SET <fk_col> = NULL WHERE
     <fk_col> = $id` (sets the FK to NULL — only legal if the column
     is nullable; otherwise the DETACH DELETE itself is illegal in
     pgGraph's model and we diagnose).
3. Then `DELETE FROM <table> WHERE <id_col> = $id` as in the
   non-detach case.

This runs as a single SPI sequence inside the per-row write step,
inside the surrounding transaction.

## Sync integration

We do nothing special. Every write above hits a registered table
through SPI. pgGraph's existing sync triggers fire on those tables
and refresh the in-memory index just as they would for any
application-issued DML. The index sees a consistent view at the next
read.

## Per-row write ordering and visibility

Within one Cypher statement, write ops execute in the order they
appear, per output row of the read tree. Effects of earlier ops are
visible to later ops *within the same row's variable bindings*
(because the bound variables are passed forward in-memory by the
facade); the **graph index** may or may not have been refreshed by
the time a later op runs (sync is async-ish), so we MUST NOT depend
on it for write-then-read on the same statement.

What this means in practice: if a Cypher statement does
`CREATE (a:X)-[:R]->(b:Y) ... MATCH (q)-[:R]->(b) ...` in the same
statement, the `MATCH` after `CREATE` will *not* see the freshly
created edge through the graph engine. Cypher semantics expect it to.
We have two options:

1. **Materialise writes through SQL only**, and run every subsequent
   MATCH as a SQL traversal (recursive CTE / repeated joins) within
   the same transaction so it sees uncommitted writes. Heavy.
2. **Reject mixed read-after-write within a single statement** with
   a clear diagnostic, and let users split into two statements (the
   sync barrier between them refreshes the index).

For v1: ship option 2 with a clean diagnostic. Option 1 is a v2 ask
that may justify a bigger redesign of the engine read path. See
`080-open-questions.md` Q-RW-1.
