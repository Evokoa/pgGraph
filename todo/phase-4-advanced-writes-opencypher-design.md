# Phase 4 Design: Advanced GQL Writes And Optional openCypher Compatibility

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

Phase 4 adds advanced write semantics once Phase 2 has proven PostgreSQL-first
writes, and adds **optional** openCypher compatibility (Q4) only after the
GQL/SQL-PGQ direction is stable.

## 0. Entry conditions

- Phase 2: PostgreSQL-first writes, row locking, tx-local overlays, rollback
  discard, sync replay all proven.
- Phase 3 advanced reads stable.
- `graph/src/cypher/` and `sql_facade/cypher.rs` do **not** exist yet — created
  here, never before (Q4).

## 1. Advanced write grammar (matrix `phase_4` rows)

```ebnf
write_clause = ... | remove_clause | detach_delete | merge_clause ;
remove_clause= "REMOVE" , property_ref ;                       (* or label *)
detach_delete= "DETACH" , "DELETE" , var ;                     (* node var *)
merge_clause = "MERGE" , node_pat , [ "ON" , "CREATE" , set_clause ]
                                  , [ "ON" , "MATCH" , set_clause ] ;
```

Tokens: `Remove`, `Detach`, `Merge`, `On`. Each must map to registered source/
edge tables; creating arbitrary schema objects from GQL stays out of scope.

## 2. IR + execution

Logical: `RemoveProperty`, `DetachDeleteNode`, `Merge`. Physical reuses
`SpiUpdateProperty`/`SpiDeleteEdge` plus a new `SpiUpsertNode` for `MERGE`.

- **`REMOVE`** sets the mapped column to `NULL` (typed columns) or drops a jsonb
  key (Phase-3 jsonb rules); needs the null/missing semantics from Phase 3.
- **`DETACH DELETE`** requires an explicit **cascade policy**: enumerate incident
  edges (forward + reverse), tombstone them, then tombstone the node — all
  PostgreSQL-first via SPI in one statement group, with documented ordering.
- **`MERGE`** is the one genuinely read-before-write path: lower to
  `SELECT ... FOR UPDATE` then conditional insert, or `INSERT ... ON CONFLICT`,
  so two sessions racing the same key serialize correctly. This is precisely why
  it was deferred past Phase 2's single-statement model.

## 3. Optional openCypher compatibility (slice 4D)

If added:

- **Separate function** `graph.cypher(query, params, hydrate)` +
  `graph.cypher_explain(...)`; never folded into `graph.gql()`.
- **Separate compatibility matrix**; explicitly *not* a Neo4j claim.
- New code only in `graph/src/cypher/` (lexer/parser/ast/semantics/lower) +
  `sql_facade/cypher.rs`; lowers into the **same logical IR** as GQL.
- openCypher features that cannot map to the PostgreSQL-authoritative model are
  rejected during semantic binding with stable diagnostics.

Positioning is compatibility, not the primary standards path; docs keep
GQL/SQL-PGQ primary.

## 4. PR slices (TDD order)

- **4A — `REMOVE`.** Property/label; null/missing per Phase 3. Tests: typed +
  jsonb cases, idempotency.

  Status, 2026-05-31: closed for single-node mapped property removal. Scalar
  mapped columns are set to SQL `NULL`; registered dotted JSONB property paths
  remove the addressed key with PostgreSQL JSONB operators; repeated removal is
  idempotent. Label removal is parsed and rejected because pgGraph labels map to
  registered source tables rather than dynamic node labels.
- **4B — `DETACH DELETE`.** Cascade policy. Tests: incident-edge enumeration,
  ordering, reverse consistency, partial-failure rollback.

  Status, 2026-05-31: closed for single-node mapped detach deletes whose
  incident relationships are backed by registered edge row tables. Execution
  deletes incident edge rows before deleting the node row, records
  transaction-local edge and node tombstones for read-your-own-writes, and
  rejects incident endpoint-table relationships until pgGraph has a safe
  PostgreSQL-first rewrite policy for them.
- **4C — `MERGE`.** Locking. Tests: two-session race on same key, ON CREATE / ON
  MATCH branches, constraint interaction.

  Status, 2026-05-31: closed for single-node mapped node merge. Execution uses
  the registered primary-key columns as the identity, probes existing rows and
  uses `FOR UPDATE` only when `ON MATCH SET` can update the row, inserts through
  `ON CONFLICT DO NOTHING`, retries the match path after insert races, and
  evaluates `ON CREATE`/`ON MATCH` branch values only on the branch taken. Tests
  cover parser/binder shape, insert/match branches,
  missing identity, readonly projection rejection, branch-lazy parameters,
  delta-limit rollback, and a heavy two-session same-key race.

  Status, 2026-06-01: `MERGE` ACL preflight is branch-sensitive. The statement
  requires `SELECT` and `INSERT` on the mapped source table; `UPDATE` is required
  only when the query includes an `ON MATCH SET` branch.
- **4D — openCypher frontend (optional).** `cypher/` modules + `graph.cypher()`.
  Tests: parser totality fuzzing, rejection corpus for unmappable features,
  SQLSTATE stability across both function surfaces, shared-IR equivalence with
  the GQL form of the same query.

  Status, 2026-06-01: closed for a narrow openCypher compatibility surface.
  `graph.cypher()` and `graph.cypher_explain()` parse the overlapping supported
  syntax through `graph/src/cypher/`, bind/lower through the same GQL logical IR,
  and execute through the existing SQL facade. `graph.cypher_compatibility()`
  returns the separate compatibility matrix and explicitly records that Neo4j
  compatibility is not claimed. Tests cover parser totality corpus entries,
  unmappable feature rejection, shared logical-IR equivalence, matching explain
  output, mapped write execution, and SQLSTATE stability.

## 5. Risk controls (Risk Register)

- "openCypher implies Neo4j compatibility" → separate honest matrix; never
  advertise unqualified Cypher support.
- Advanced writes widen the attack surface → catalog-validated, parameterized
  SPI, ACL/RLS/tenant-checked, PostgreSQL-first, like Phase 2.
