# Open questions

Resolve before, or during, implementation. Each question has a
decision owner and a "needed by" milestone.

## Upstream (cyrs) — all resolved

Every upstream ask landed in **cyrs 0.1.0** (19 crates published to
crates.io 2026-05-10; embedder PRs #56 and #58). The detailed problem
statements live in `../../../../cyrs/feat-request.md`; the table below
records what shipped. **No `Q-UP-*` item blocks pgGraph any longer.**

| ID | Ask | Shipped API | § |
|----|-----|-------------|---|
| Q-UP-1 | `ShortestPath` ReadOp | `cyrs_plan::ReadOp::ShortestPath { input, from, to, rel, kind, bind_path }` | §1.1 |
| Q-UP-2 | Path-variable surface | Documented contract: the plan IR has no `Path` type; the embedder owns path materialisation; `cyrs_hir::VarKind::Path` carries the contract | §1.2 |
| Q-UP-3 | MERGE key surface | `WriteOp::MergeNode` / `MergeRel` gained `key_props: Vec<SmolStr>`, populated by lowering when `props` is a literal `Expr::Map` | §2.1 |
| Q-UP-4 | Uniqueness on `SchemaProvider` | `label_unique_props` / `rel_type_unique_props`; `cyrs-sema` now proves MERGE determinism upstream | §2.2 |
| Q-UP-5 | `labels_compatible` | `SchemaProvider::labels_compatible(&[SmolStr]) -> Option<bool>` (`None` = no opinion) | §2.3 |
| Q-UP-6 | Typed parameters | `PlanStatement::params`, `ParamRef` + `ParamType` (`Unknown` variant for unconstrained params) | §2.4 |
| Q-UP-7 | Function enumeration | `StandardLibrary::builtin_signature()` — per-function `deterministic` / `null_propagating` metadata; normative builtin enumeration | §1.3 |
| Q-UP-8 | `lower_*` returns `Result` | `lower_statement` / `lower_parse` → `Result<Statement, HirLowerError>` (`ParseFailed` / `Invariant`) | §4.1 |
| Q-UP-9 | `HirId → span` | `Statement::span_of(HirId) -> Option<Range<usize>>` — a byte range, ready for `errposition()` | §4.2 |
| Q-UP-10 | Stable release channel | cyrs 0.1.0 — 19 crates on crates.io | §6.1 |

Two further feat-request items resolved (never tracked as `Q-UP`):
**§3.1** — `E4500..=E4999` is now a formally reserved embedder-owned
diagnostic range, policed by a `DiagCode::ALL` test, so pgGraph's
`E4500`–`E4560` codes cannot collide with cyrs's own. **§5.1 / §5.2**
— the `ReadOp::Filter` 3VL and empty-key `ReadOp::Aggregate`
semantics pgGraph depends on are documented as stable contracts.

### Consequences for the build

- The `graph/Cargo.toml` cyrs dependency stays a `../../cyrs` path
  dependency through M1–M5 co-development; flip it to the crates.io
  `0.1.0` release before any pgGraph release (feat-request §6.1).
- `cypher_facade::schema_provider` now implements `labels_compatible`,
  `label_unique_props`, and `rel_type_unique_props` as real
  `SchemaProvider` trait methods, not inherent stubs.
- Every "until cyrs ships …" workaround in
  `070-milestones-and-tests.md` is gone: M3 multi-label `CREATE`, M4
  MERGE-key extraction, and M5 `shortestPath` build straight against
  the shipped API.
- `cypher_facade::diag_to_pg` can use `Statement::span_of` for
  `errposition()` carets from M1 — the "omit errposition" workaround
  is unnecessary.

## Internal (no upstream dependency)

### Q-IN-1 — Read-after-write within a single statement

- **Decision needed:** for v1, do we
  (a) reject every Cypher statement that has both a write op and a
      subsequent read referencing the just-written entities (`E4520`),
      or
  (b) accept it but document that the read won't see the write
      through the graph engine (only through SQL)?
- **Recommendation:** (a). Cleaner UX; never silently wrong.
- **Needed by:** M3.

### Q-IN-2 — Label set arithmetic without discriminator column

- **Decision needed:** do we (1) reject `SetLabels` / `RemoveLabels`
  on non-discriminator tables (`E4510`), (2) introduce a v2-only
  `_node_labels` junction table, or (3) require every label table to
  declare a discriminator column at registration time?
- **Recommendation for v1:** (1). Discriminator-backed tables get
  full support; everything else gets `E4510` with a clear remediation
  hint.
- **Needed by:** M3.

### Q-IN-3 — JSON shape of the `graph.cypher()` result

- **Decision needed:** one `jsonb` column with the whole RETURN row
  as an object, or `RETURNS SETOF jsonb` per item, or `RETURNS TABLE
  (..)` with one column per RETURN item dynamically typed?
- **Recommendation:** single `jsonb row` column for v1.
  `RETURNS TABLE` with dynamic columns is not supported by pgrx;
  emulating it via `record` types is fragile and gives bad UX. Users
  who want flat columns wrap in `jsonb_to_record(...)`.
- **Needed by:** M1.

### Q-IN-4 — Statement compile cache

- **Decision needed:** cache `(text, schema_digest, dialect)` →
  `PlanStatement`, or recompile every call?
- **Recommendation:** v1 recompiles. Add a per-backend `LruCache`
  in M5 if compile-time shows up in profiling.
- **Needed by:** post-M5 perf pass.

### Q-IN-5 — Configurable default depth / row caps

- **Decision needed:** does `graph.cypher()` honour
  `graph.default_max_depth` / `graph.max_nodes` / `graph.max_frontier`
  GUCs the same way `graph.traverse()` does?
- **Recommendation:** yes. Same GUCs, same semantics. Adds a `NOTICE`
  when a cap is reached.
- **Needed by:** M2.

### Q-IN-6 — Tenant scoping

- **Decision needed:** how does `WHERE` interact with the existing
  tenant column? Auto-injection of a tenant predicate is convenient
  but surprising.
- **Recommendation:** auto-inject `<tenant_column> =
  current_setting('pgGraph.tenant')` on every `Source` over a tenanted
  table when the GUC is set. Emit a `NOTICE` when first auto-injected
  in a statement. Document it as a documented invariant, not as
  magic.
- **Needed by:** M1.

### Q-IN-7 — Dialect default

- **Decision needed:** what's the default `DialectMode` for
  `graph.cypher()`? GqlAligned or OpenCypherV9?
- **Recommendation:** `OpenCypherV9`. It's what the green-tag TCK
  subset targets, and it's the dialect most existing tooling and
  documentation expects. Selectable via GUC `graph.cypher_dialect`.
- **Needed by:** M1.

### Q-IN-8 — Should `graph.cypher` accept multiple statements?

- **Decision needed:** one statement per call, or a script
  (statement; statement; …)?
- **Recommendation:** one. Multi-statement is what `psql` and the
  outer transaction give you for free.
- **Needed by:** M1.

### Q-IN-9 — `EXPLAIN`-equivalent

- **Decision needed:** ship a companion `graph.explain_cypher(text)
  RETURNS text` in v1, or defer?
- **Recommendation:** defer to M5 once the operator catalogue
  stabilises. Print the `cyrs_plan::pretty` form.
- **Needed by:** M5.

## To document later

Not blocking, but worth writing up once we have a working M1:

- A `user_guide/` page explaining `register_label` / friends and
  showing a "hello cypher" example on the demo schema.
- A `roadmap.mdx` entry pointing at this directory.
- A note in the top-level `README.md` once M5 ships.
