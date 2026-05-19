# Architecture

## Module layout (additions to `graph/src/`)

```
graph/src/
├── lib.rs                       # add: pub mod cypher_facade;
│                                # add: pg_extern fn cypher(text, jsonb)
├── cypher_facade/
│   ├── mod.rs                   # entry: compile() → Plan; execute() → rows
│   ├── schema_provider.rs       # impl cyrs_schema::SchemaProvider over catalog
│   ├── plan_translator/
│   │   ├── mod.rs
│   │   ├── read.rs              # ReadOp → engine calls / SQL
│   │   ├── write.rs             # WriteOp → SPI DML
│   │   ├── expr.rs              # cyrs_plan::Expr → SQL fragment OR Rust eval
│   │   ├── path.rs              # MATCH p = ... path materialisation
│   │   └── shortest.rs          # ShortestPath op → path_finder
│   ├── row_eval.rs              # in-process row evaluator for Filter /
│   │                            # Project / Aggregate / OrderBy / Skip /
│   │                            # Limit / Distinct / Unwind when SQL can't
│   ├── param_bind.rs            # JSONB params → cyrs param map + pg type
│   ├── diag_to_pg.rs            # cyrs_diag::Diagnostic → ereport
│   └── tests/
│       └── ...
├── catalog/
│   ├── mod.rs                   # existing; add label/rel mapping read+write
│   ├── labels.rs                # NEW: label↔table↔column mapping
│   └── unique.rs                # NEW: registered uniqueness constraints
└── sql/                         # NEW migrations for new catalog tables
    └── cypher_catalog.sql
```

No changes to `engine.rs`, `bfs.rs`, `path_finder.rs`, `edge_store.rs`,
`node_store.rs`, `sync.rs`. The facade is strictly additive.

## Cargo.toml additions

```toml
[dependencies]
cyrs-hir     = { version = "...", default-features = false }
cyrs-plan    = { version = "...", default-features = false }
cyrs-schema  = { version = "...", default-features = false }
cyrs-sema    = { version = "..." }
cyrs-diag    = { version = "..." }
smol_str     = "0.3"             # cyrs surfaces SmolStr; we'll see it in matches
```

Version pin: a single git tag or crates.io minor version. See
`080-open-questions.md` Q-PKG-1.

## Public surface

Exactly one new pgrx SQL function:

```sql
-- Returns the row stream of a Cypher query.
-- result_jsonb is one row per RETURN row, columns flattened into a single JSONB object.
CREATE FUNCTION graph.cypher(query text, params jsonb DEFAULT '{}'::jsonb)
    RETURNS TABLE (row jsonb)
    LANGUAGE c STRICT VOLATILE;
```

`VOLATILE` because writes are allowed. A future `graph.cypher_read(...)`
companion declared `STABLE` is a possible optimisation (gates writes,
allows query-planner re-use) but is out of scope for v1.

## Pipeline contract

```rust
// cypher_facade/mod.rs (sketch)

pub fn execute(query: &str, params: serde_json::Value)
    -> Result<Vec<JsonB>, FacadeError>
{
    // 1. parse + HIR-lower.
    let hir = cyrs_hir::lower::lower_statement(query)?;

    // 2. schema-aware sema. Schema = pgGraph catalog snapshot.
    let schema = SchemaProvider::from_catalog(snapshot_catalog()?);
    let diags = cyrs_sema::check(&hir, &schema);
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(FacadeError::SemaErrors(diags));
    }

    // 3. plan-lower.
    let plan = cyrs_plan::lower::lower_statement(&hir)?;

    // 4. bind params (params.jsonb → cyrs param table, typed via 2.4 of feat-request).
    let bound = param_bind::bind(&plan, params)?;

    // 5. execute read tree, applying writes per row.
    let rows = plan_translator::execute(&plan, &bound, &schema)?;

    Ok(rows)
}
```

Steps 1–3 are pure functions; their result is cacheable on `(query,
catalog_fingerprint, schema_digest)`. The `catalog_fingerprint`
already exists (`catalog::catalog_fingerprint`). The `schema_digest`
comes from `cyrs_schema::SchemaProvider::schema_digest()`. We'll
share these for a per-backend statement cache in a later milestone.

## Boundaries with the existing engine

The facade calls into pgGraph's existing read path through a thin
adapter layer it owns. We don't expose engine internals back to cyrs.

| Read op                       | Engine entry point we'll call                  |
| ----------------------------- | ---------------------------------------------- |
| `Source { label, bind }`      | `sql_search::source_table_search_rows` or `Spi` table scan |
| `Expand { single }`           | new helper over `engine::Engine::adjacent` (one hop)       |
| `Expand { variable-length }`  | `sql_traversal::execute_traverse_rows` + `TraverseRequest` |
| `ShortestPath` (cy-feat §1.1) | `path_finder` (the existing shortest-path module)          |
| All other ops                 | `row_eval` (in-process), composing engine results          |

Write ops compose existing SPI helpers; the facade owns the SQL it
emits because pgGraph's current SPI users target the catalog/sync
path, not arbitrary user-table DML.

## Threading and transactions

- `graph.cypher(...)` is called inside a Postgres query, which is
  inside a transaction. All SPI calls inherit that transaction.
- A whole Cypher statement therefore commits or rolls back atomically
  with the rest of the user's transaction. No special savepoints
  needed.
- The facade is single-threaded per invocation; we don't introduce
  worker threads. pgGraph's background workers are unchanged.

## Error model

| Origin                                   | Surfaces as                                  |
| ---------------------------------------- | -------------------------------------------- |
| `cyrs_syntax` parse errors               | `ereport(ERROR, ..., SQLSTATE 42601)`        |
| `cyrs_sema` `Error` diagnostics          | `ereport(ERROR, ..., SQLSTATE 42P10)`        |
| `cyrs_plan` `PlanLowerError`             | `ereport(ERROR, ..., SQLSTATE XX000)`        |
| Embedder rejection (e.g. unmapped label) | `ereport(ERROR, ..., SQLSTATE 0A000)`        |
| Underlying SPI error                     | bubble up the original SQLSTATE              |
| `cyrs_sema` `Warning` / `Note`           | `ereport(NOTICE / WARNING, ...)` per severity|

cyrs diagnostic spans become Postgres `errposition()` offsets where
available — wraps the `HirId → byte span` accessor request (§4.2 of
`feat-request.md`).
