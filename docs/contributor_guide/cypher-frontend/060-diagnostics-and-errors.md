# Diagnostics, errors, and Postgres surface

The point of using cyrs is the diagnostic quality. We must not lose
it on the way through Postgres.

## Diagnostic taxonomy

cyrs (`cyrs-diag`) produces `Diagnostic { code, severity, message,
primary, labels, notes, related, fixes }`. Code ranges (spec §10.2):

| Range     | Meaning                                        |
| --------- | ---------------------------------------------- |
| E0xxx     | Syntax (lexer + parser)                        |
| E1xxx     | Name resolution                                |
| E2xxx     | Semantic — schema-free                         |
| E3xxx     | Semantic — schema-aware                        |
| E4xxx     | Dialect / compatibility                        |
| E45xx     | **Reserved for embedder rejections** (see `feat-request.md` §3.1) |
| E5xxx     | Type system                                    |
| W6xxx     | Style / lint                                   |
| W7xxx     | Performance                                    |
| N8xxx     | Notes                                          |

The `E45xx` range is the one **we own** as the embedder. We mint
codes from it for situations cyrs cannot diagnose because they're
about our storage model:

| Code   | Meaning                                                                  |
| ------ | ------------------------------------------------------------------------ |
| E4500  | Label not registered (`graph.register_label` required).                  |
| E4501  | Property not registered for label.                                        |
| E4502  | Relationship type not registered.                                         |
| E4503  | Label set not registered as compatible (multi-label `CREATE`).            |
| E4504  | MERGE pattern lacks declared uniqueness on key props.                     |
| E4510  | `SetLabels` / `RemoveLabels` not supported by storage model.              |
| E4520  | Read-after-write within a single statement (sync barrier required).      |
| E4530  | `shortestPath` / `allShortestPaths` not yet implemented (pending §1.1).  |
| E4540  | Property column is `NOT NULL`; `REMOVE` would violate.                    |
| E4550  | Inferred parameter type incompatible with registered column type.        |
| E4560  | Function not supported (push-set + row-eval-set both reject).             |
| E4599  | Catch-all: feature not yet implemented.                                   |

These codes are stable. New codes append; existing codes don't shift.

## ereport surface

```rust
fn report_diag(d: &Diagnostic) -> ! {
    let sqlstate = sqlstate_for(d.code);
    let mut err = ErrorReport::new(PgSqlErrorCode::from_sqlstate(sqlstate),
                                   d.message.clone(),
                                   /* funcname */ "graph.cypher");
    if let Some(byte_offset) = d.primary.span_start_byte() {
        err = err.errposition(byte_offset as i32);
    }
    for label in &d.labels {
        err = err.detail(format!("{}: {}", label.span, label.caption));
    }
    for note in &d.notes {
        err = err.hint(note.clone());
    }
    err.report();  // unwinding panic → pgrx → ereport
}
```

`sqlstate_for`:

| cyrs range  | SQLSTATE | Postgres class                           |
| ----------- | -------- | ---------------------------------------- |
| `E0xxx`     | `42601`  | syntax_error                             |
| `E1xxx`     | `42703`  | undefined_column (used for undefined name in scope) |
| `E2xxx-E3xxx`| `42P10`  | invalid_column_reference                |
| `E4xxx`     | `0A000`  | feature_not_supported                    |
| `E45xx`     | `0A000`  | feature_not_supported (embedder)         |
| `E5xxx`     | `42804`  | datatype_mismatch                        |
| `W6xxx`     | NOTICE level, no SQLSTATE                |
| `W7xxx`     | WARNING level                            |
| `N8xxx`     | NOTICE level                             |

## Multi-error reporting

cyrs returns *all* diagnostics from a check, not just the first.
Postgres' `ereport(ERROR, ...)` aborts on the first error. So:

- Collect all sema diagnostics.
- Sort by primary span byte offset.
- If any are `Error`, emit the first as `ERROR` and append the rest
  into `detail()` lines so the user sees them in the same backend
  message.
- If only `Warning`/`Notice` exist, emit each via `ereport(NOTICE)`
  before continuing.

Optional future: ship a `graph.cypher_check(text) -> SETOF jsonb`
function that returns every diagnostic as a row, never aborts. Useful
for IDE integrations. v2.

## Error sources by phase

| Phase                  | Source                                     | Surface          |
| ---------------------- | ------------------------------------------ | ---------------- |
| HIR lowering           | `cyrs_syntax::SyntaxError` (parse errors), `cyrs_hir::HirLowerError` (pending §4.1) | `42601`          |
| Sema                   | `cyrs_sema` diagnostic stream              | `42703 / 42P10 / 42804` |
| Plan lowering          | `cyrs_plan::PlanLowerError` (`UnresolvedRef`, `UndesugaredExpr`, …) | `XX000` (internal — sema should have caught) |
| Embedder rejection     | facade-side checks (label/rel missing, etc.) | `0A000`          |
| Parameter binding      | type bridge failure                        | `22023`          |
| SPI exec               | Postgres' own error (FK violation, unique violation, NOT NULL violation, …) | original SQLSTATE preserved |

`PlanLowerError` from cyrs at runtime means we failed to keep the
pipeline in sync; we treat that as an internal error and log
verbosely. Should never happen with a correctly implemented sema
gate.

## Diagnostic fixtures

UI-style tests (compiletest-flavoured): pairs of `input.cypher` +
`expected.diag.txt`. The harness drives `graph.cypher()` in a test
backend, captures the `ereport` payload, and compares against the
expected file. `cargo xtask bless` regenerates.

Mirror the structure cyrs already has under `tests/ui/sema/` and
`tests/ui/dialect/` — we add `tests/ui/cypher/`:

```
graph/tests/ui/cypher/
├── label_not_registered/
│   ├── input.cypher
│   └── expected.diag.txt
├── merge_no_unique/
│   ├── input.cypher
│   ├── expected.diag.txt
│   └── schema.sql
└── ...
```

`schema.sql` is the catalog registration script run before the test
input; it documents exactly which registrations the input depends on.

## What we do NOT translate

- We do not translate `errposition` to a row/column. pgrx's
  `errposition` takes a byte offset; that's what we already have
  from cyrs. Postgres' clients (psql, etc.) render it.
- We do not chain related diagnostics through Postgres' "internal
  query" mechanism. They become `DETAIL:` and `HINT:` lines instead.
- We do not emit machine-readable diagnostics in v1. SARIF / JSON
  output is a v2 ask matching cyrs's roadmap D7.
