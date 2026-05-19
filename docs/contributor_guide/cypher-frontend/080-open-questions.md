# Open questions

Resolve before, or during, implementation. Each question has a
decision owner and a "needed by" milestone. Items blocked on cyrs
upstream cite the section of `../../../../cyrs/feat-request.md` they
depend on.

## Upstream (blocking on cyrs)

### Q-UP-1 — `ShortestPath` ReadOp lands in `cyrs-plan`

- **Status:** awaiting upstream (cyrs `feat-request.md` §1.1).
- **Needed by:** M5.
- **Impact if delayed:** `shortestPath` / `allShortestPaths`
  permanently emit `E4530` until landed. We can ship M0–M4 without
  it.

### Q-UP-2 — Path-variable surface in Plan

- **Status:** awaiting upstream doc clarification (cyrs §1.2).
- **Needed by:** M2 (varlen patterns with `RETURN p`).
- **Workaround:** treat path variables as JSONB shaped like the
  existing `path` column from `execute_traverse_rows`. If cyrs
  surfaces a structured form, we adapt.

### Q-UP-3 — MERGE key surface on `WriteOp::MergeNode/MergeRel`

- **Status:** awaiting upstream (cyrs §2.1).
- **Needed by:** M4.
- **Workaround for M4:** embedder-side analysis of `MergeNode.props`
  to extract the key. Remove the workaround when upstream ships.

### Q-UP-4 — `label_unique_props`, `rel_type_unique_props` on `SchemaProvider`

- **Status:** awaiting upstream (cyrs §2.2).
- **Needed by:** M4 (so sema can prove MERGE determinism instead of
  embedder rejecting at exec time).
- **Workaround:** runtime check in the facade, with `E4504` if the
  required unique constraint isn't registered.

### Q-UP-5 — `labels_compatible` on `SchemaProvider`

- **Status:** awaiting upstream (cyrs §2.3).
- **Needed by:** M3 (multi-label CREATE).
- **Workaround:** v1 rejects every multi-label CREATE with `E4503`
  until cyrs ships the hook. Single-label CREATE is unaffected.

### Q-UP-6 — Parameter type surface

- **Status:** awaiting upstream (cyrs §2.4).
- **Needed by:** M1.
- **Workaround:** treat every param as JSONB. Loses some pg-side
  type checking; otherwise works. Re-bind once upstream ships.

### Q-UP-7 — Function builtin enumeration

- **Status:** awaiting upstream (cyrs §1.3).
- **Needed by:** M2 (any RETURN with function calls).
- **Workaround:** maintain our own bucket table; CI test asserts every
  name in cyrs's current set is covered. Drift becomes a CI failure,
  not a silent miss.

### Q-UP-8 — `cyrs-hir::lower_statement` returns `Result`

- **Status:** awaiting upstream (cyrs §4.1).
- **Needed by:** M1.
- **Workaround:** catch unwinds from `lower_statement` in a panic
  boundary and translate to `E0xxx` `42601`. Worse UX than a real
  result type.

### Q-UP-9 — `HirId → byte span` accessor

- **Status:** awaiting upstream (cyrs §4.2).
- **Needed by:** M1 (for `errposition`).
- **Workaround:** omit `errposition`; users get diagnostics without
  caret positioning. Tolerable for v1.

### Q-UP-10 — Crates.io publication or stable git tag

- **Status:** awaiting upstream (cyrs §6.1).
- **Needed by:** M0 (cannot pin `Cargo.toml` otherwise).
- **Workaround:** path dependency in development; flip to a tagged
  rev before any release.

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
