# Expressions and types

This document covers:

1. How a `cyrs_plan::Expr` becomes either a SQL fragment or a Rust-side
   value computation.
2. The Cypher value type ↔ Postgres type bridge.
3. The null / 3VL alignment between Cypher and SQL.

## `Expr` translation strategies

`cyrs_plan::Expr` variants (`crates/cyrs-plan/src/lib.rs:483-…`):

| `Expr` variant      | SQL push                                                  | Row eval                                |
| ------------------- | --------------------------------------------------------- | --------------------------------------- |
| `Null`              | `NULL`                                                    | `Value::Null`                           |
| Scalar literals     | param literal of matching type                            | `Value::*`                              |
| `VarRef(v)`         | column reference (must already be in scope)               | lookup in row bindings                  |
| Property access     | `<col_alias>.<prop_col>`                                  | JSONB get-path on the materialised node |
| Binary ops          | translate operator; respect Cypher 3VL                    | Rust impl                               |
| Unary ops           | translate                                                  | Rust impl                               |
| `Call { name, .. }` | only if function is push-safe (see table below)            | dispatch in `row_eval::call`            |
| `CASE … WHEN …`     | SQL `CASE`                                                | Rust match                              |
| List literal        | `array[$1, $2, ...]` for homogeneous primitives; otherwise force row eval | `Value::List`                           |
| Map literal         | `jsonb_build_object(...)`                                  | `Value::Map`                            |
| Parameter ref       | bound SPI parameter (`$N`)                                 | bound facade value                      |
| `Exists(pattern)`   | sub-`EXISTS (...)` against the SQL-pushed inner            | run the sub-plan, check non-empty       |

The bottom-up rule: any `Expr` that requires row eval forces its
parent operator to row eval. We never reconstruct a SQL fragment
around a Rust-evaluated sub-expression.

## Function classification

Built-ins from `cyrs_schema::StandardLibrary` (pending the enumeration
in `feat-request.md` §1.3). Three buckets:

### Push-safe to SQL

| Cypher built-in       | Postgres equivalent          |
| --------------------- | ---------------------------- |
| `coalesce(x, y, …)`   | `COALESCE(x, y, …)`          |
| `toLower(s)`          | `lower(s)`                   |
| `toUpper(s)`          | `upper(s)`                   |
| `trim(s)`             | `btrim(s)`                   |
| `ltrim(s)` / `rtrim`  | `ltrim` / `rtrim`            |
| `substring(s, i[, l])`| `substring(s, i+1, l)` (`i` is 0-based in Cypher) |
| `replace(s, a, b)`    | `replace(s, a, b)`           |
| `size(list)`          | `array_length(list, 1)` for arrays; row-eval for jsonb arrays |
| `length(string)`      | `length(s)`                  |
| `abs/ceil/floor/round`| `abs/ceil/floor/round`       |
| `sqrt/exp/log/log10`  | matching                     |
| `toString(x)`         | `cast(x as text)` for primitives |
| `toInteger(x)`        | `cast(x as bigint)`          |
| `toFloat(x)`          | `cast(x as double precision)`|
| `toBoolean(s)`        | `cast(s as bool)`            |
| Date/datetime ctors   | `make_date`, `make_timestamp`, etc. |

### Row-eval only

`rand`, `randomUUID`, `timestamp()`, `datetime()` (when called without
args — non-deterministic), `id`, `labels`, `keys`, `properties`,
`nodes(p)`, `relationships(p)`, `head/tail/last` on heterogeneous
lists, list predicates `any/all/none/single`, `reduce`.

### Rejected / not yet supported

`shortestPath` / `allShortestPaths` (pending cyrs §1.1; the read-op
form is the supported entry point, not the expression form). Spatial
functions. Temporal functions beyond `date`/`datetime`.

## Value model (Rust-side)

```rust
#[derive(Debug, Clone)]
enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(SmolStr),
    Date(chrono::NaiveDate),
    Datetime(chrono::DateTime<chrono::Utc>),
    List(Vec<Value>),
    Map(IndexMap<SmolStr, Value>),
    Node(NodeRef),               // (table_oid, id)
    Relationship(RelRef),        // (rel_type, from_id, to_id, [junction_pk])
    Path(Vec<PathStep>),         // alternating node/rel/node
}
```

Row eval operates over `Value`. SQL-pushed paths skip this entirely.

## Type bridge

Driven by `_registered_label_properties.column_type`. Mapping table is
in `020-catalog-extensions.md`.

The bridge runs at three points:

1. **Read hydration:** Postgres value → `Value`. JSONB columns are
   recursively converted; other primitives map directly.
2. **Parameter binding:** Cypher param `$x` (typed via cyrs's
   inferred param map, `feat-request.md` §2.4) → SPI parameter with
   matching pg type. Maps and lists become JSONB.
3. **Write coercion:** `Value` produced by an `Expr` → SPI parameter
   of the target column's pg type. Cast failures become
   `ereport(ERROR, ..., SQLSTATE 22023 invalid_parameter_value)`.

## Null and 3VL

Cypher uses 3-valued logic. Postgres SQL uses 3-valued logic. They
mostly align. Known cases where they differ:

| Construct                  | Cypher                | Postgres SQL         | Resolution |
| -------------------------- | --------------------- | --------------------- | ---------- |
| `null = null`              | `null`                | unknown (→ false in WHERE) | Same observable behaviour in `Filter`; both drop the row. ✓ |
| `null = x`                 | `null`                | unknown               | Same in `Filter`. ✓ |
| `null OR true`             | `true`                | `true`                | ✓ |
| `null AND false`           | `false`               | `false`               | ✓ |
| `n.prop` on null `n`       | `null`                | type error            | Wrap with `CASE WHEN n IS NULL THEN NULL ELSE n.<col> END` |
| `[1,2,3][null]`            | `null`                | type error            | Force row eval for list indexing |
| `list IN list`             | structural            | per-element via `ANY` | Force row eval for list membership |
| `Distinct` over null cols  | rows with null differ structurally | `DISTINCT` treats null as one group | Force row eval (see `030-read-mapping.md`) |
| `ORDER BY` null            | "smaller than any value" | NULLS FIRST or LAST depending | Use explicit `NULLS LAST` when pushing |

The first rule of the bridge: **when in doubt, row-eval.**
Correctness > performance. Push-to-SQL is opportunistic.

## Parameter typing

Each `$param` in the source surfaces in `PlanStatement.params` (pending
upstream `feat-request.md` §2.4). Pre-binding step:

```rust
fn bind_param(p: &ParamType, j: &serde_json::Value) -> Result<SpiParam, Error> {
    match p {
        ParamType::Scalar(PropertyType::Int) => i64::try_from(j.as_i64()...),
        ParamType::Scalar(PropertyType::String) => ...,
        ParamType::Scalar(PropertyType::Date) => parse_date(j.as_str()?),
        ParamType::List(inner) => to_jsonb_array_typed(j, inner)?,
        ParamType::Map => SpiParam::jsonb(j.clone()),
        ParamType::Unknown => SpiParam::jsonb(j.clone()), // fallback
        ...
    }
}
```

`graph.cypher(query text, params jsonb)` accepts params as a single
JSONB; the JSONB has the parameter names as keys. (We considered a
variadic form but JSONB is easier to forward from clients and is what
existing pgGraph functions like `filter` already accept.)

## Function library scaffolding

```rust
// graph/src/cypher_facade/plan_translator/expr.rs

enum PushResult<'a> {
    Sql(String, Vec<SpiArg<'a>>),
    NeedsRowEval(&'a Expr),
}

fn try_push_call(name: &str, args: &[Expr], ctx: &PushCtx) -> PushResult<'_> {
    match BUILTIN_PUSH.get(name) {
        Some(rule) => rule.emit(args, ctx),
        None       => PushResult::NeedsRowEval(/* expr ref */),
    }
}

static BUILTIN_PUSH: phf::Map<&'static str, &'static dyn PushRule> = phf::phf_map! {
    "coalesce" => &CoalescePushRule,
    "toLower"  => &SimpleRename("lower"),
    "toUpper"  => &SimpleRename("upper"),
    ...
};
```

The table is exhaustive; missing entries default to row eval. CI test
asserts every name in `cyrs_schema::StandardLibrary` is either listed
in `BUILTIN_PUSH` or in a `ROW_EVAL_ONLY` allowlist.
