# Full GQL Conformance Plan

## Normative Target

Target [ISO/IEC 39075:2024, GQL](https://www.iso.org/standard/76120.html),
including applicable published corrections. Track the in-progress
[Technical Corrigendum 1](https://www.iso.org/standard/93701.html) without
claiming conformance to draft text.

The implementation must use a legally obtained standards copy for clause-level
work. Repository metadata should identify features and tests without copying
copyrighted standards text.

`graph.gql(text, params, hydrate)` remains a useful PostgreSQL transport, but it
is not by itself proof of complete language support. Full support means every
in-scope normative feature has parser, semantic, planner, runtime, diagnostic,
transaction/security, conformance-test, and documentation coverage.

## Single Conformance Registry

Create a machine-readable registry, for example
`conformance/gql-39075-2024.toml`, with one stable feature ID per requirement:

```toml
[[feature]]
id = "gql.example.stable-id"
source = "ISO/IEC 39075:2024"
status = "unsupported"
parser = "missing"
binder = "missing"
planner = "missing"
executor = "missing"
diagnostics = "missing"
security = "missing"
tests = []
docs = []
postgres_delegation = ""
```

Generate public capability tables and drift tests from this registry. Do not
maintain the current long prose matrix by hand. Status values should distinguish
`supported`, `partial`, `delegated-to-postgresql`, `extension`, `unsupported`,
and `not-applicable`, with a reason for every non-supported row.

## P0 Semantic Foundations

### RLS-safe graph visibility

Graph candidate visibility must be evaluated under the caller's PostgreSQL
snapshot before any coordinate or topology is observable. Apply this to nodes,
source relationship rows, paths, aggregates, shortest-path existence, and
`hydrate := false`. Batch visibility checks and cache only within the statement.

### Stable relationship identity

Add a `RelationshipId` based on registration/mapping identity plus the source
row's stable primary-key tuple. FK-style relationships use the authoritative
source-row key. Edge tables must expose stable identity columns; `ctid` is not
a durable identity.

Carry relationship ID through forward/inbound CSR, overlays, transaction
deltas, projection segments, paths, hydration, updates, deletes, compaction,
and artifact validation. Never deduplicate solely by endpoint/type.

### Stable filter identity

Replace name-only filters with graph/table/attribute identity. Binding may
accept a friendly property name only after the source label/table disambiguates
it.

### PostgreSQL transaction contract

Define one snapshot rule for topology and source properties. Add delta frames
per PostgreSQL subtransaction so SAVEPOINT rollback/release works. Gate read and
write behavior under READ COMMITTED, REPEATABLE READ, and SERIALIZABLE.

## Feature Inventory

The clause-level ledger must cover at least these families; exact naming and
requirements come from the selected standards edition:

- session, graph reference, catalog, schema, graph type, and graph value rules;
- node and relationship pattern syntax, labels/types, direction, and identity;
- multiple patterns, optional matching, binding-table composition, and nested
  query forms;
- quantified and parenthesized path patterns, match modes, search prefixes,
  shortest-path families, and deterministic path semantics;
- scalar, Boolean, numeric, string, temporal, duration, list, record/map, graph
  element, null/missing, coercion, comparison, conditional, and function rules;
- grouping, aggregate, DISTINCT, ordering, pagination, set/bag semantics, and
  empty-input behavior;
- data modification, set-based/multi-row behavior, labels/properties,
  relationship changes, and returned binding tables;
- parameters, diagnostics, transaction behavior, privileges, and exception
  conditions;
- implementation-defined limits and PostgreSQL-delegated behavior.

Current Cypher-shaped `CREATE` and `MERGE` behavior must be classified
separately from the ISO data-modification syntax. Compatibility aliases cannot
silently stand in for a normative feature.

## Frontend And Runtime Shape

```text
GQL text parser
SQL/PGQ typed adapter
SQL function adapters
optional compatibility parsers
        |
        v
typed semantic model and binding-table logical IR
        |
statistics + rewrites + cost/resource planning
        |
streaming physical operators
        |
projection stores + PostgreSQL visibility/property/DML adapter
```

Do not add a new physical statement enum for every syntax feature. Represent a
query as binding-table operators that can compose scans, matches, optional
matches, filters, projections, grouping, ordering, subqueries, and writes.

Keep a typed internal value model. JSONB is an external SQL encoding, not the
engine's only data type. Implement GQL null/missing and three-valued logic
explicitly rather than inheriting accidental serde/JSON behavior.
RUST-4 in the
[Rust boundary plan](./10-rust-type-safety-pgrx-boundaries.md) defines the
required `GraphValue` algebra and PostgreSQL differential gates. Decimal/
numeric and temporal semantics must not pass through `f64` or JSON, and no
unrepresentable value may silently become JSON null.

Compile path patterns into a reusable automaton/state machine with explicit
search mode, uniqueness, direction, label/type predicates, capture slots, and
resource cost. Recursive Rust functions are not the long-term execution model.

## PostgreSQL SQL/PGQ Boundary

PostgreSQL 19 documentation now includes native
[property graph definitions](https://www.postgresql.org/docs/19/ddl-property-graphs.html),
[CREATE PROPERTY GRAPH](https://www.postgresql.org/docs/19/sql-create-property-graph.html),
and [GRAPH_TABLE queries](https://www.postgresql.org/docs/19/queries-graph.html).
The roadmap should no longer wait abstractly for future graph-pattern support.

Plan a PostgreSQL-version adapter:

- PostgreSQL 14-18: `graph.gql()` and existing SQL functions remain the public
  frontends.
- PostgreSQL 19: consume native property-graph definitions as authoritative
  mappings without duplicate registration, with DDL invalidation, security,
  `GRAPH_TABLE` differential tests, and common statistics.
- Keep GQL and SQL/PGQ conformance ledgers separate. They are related standards
  and may share IR, but one frontend does not imply conformance to the other.
- Do not depend on unstable internal parser/planner APIs without a versioned
  adapter and CI coverage.
- Delegate native `GRAPH_TABLE` execution to PostgreSQL unless a stable,
  supported hook can safely select pgGraph acceleration.
- Benchmark native SQL/PGQ and pgGraph execution on identical PostgreSQL source
  tables rather than using synthetic marketing comparisons.

The current `sqlpgq_adapter.rs` is dead-code guarded and its matrix is stale
relative to current multi-pattern GQL capabilities. Rebase it on the canonical
IR instead of translating into the subset AST.

## Delivery Slices

1. **Registry and baseline:** map every current feature and known rejection;
   generate docs and drift tests.
2. **Correct graph values:** relationship identity, multigraph storage, filter
   identity, RLS visibility, null/missing, and typed values.
3. **Composable binding tables:** replace the one-MATCH/one-RETURN shape with
   a general clause/operator pipeline.
4. **Expressions and functions:** implement typed operations in vertical slices
   with conformance vectors.
5. **Path language:** automaton, match modes, quantified patterns, search and
   shortest-path families under deterministic resource controls.
6. **Read composition:** optional, multi-pattern, subquery, set/bag, aggregate,
   order, and pagination semantics.
7. **Standards-aligned writes:** set-based PostgreSQL DML with RETURNING, source
   row locks/rechecks, deltas, savepoints, and concurrency gates.
8. **Catalog/session/delegation:** implement or explicitly delegate graph
   catalog, session, transaction, and privilege features.
9. **PostgreSQL 19 adapter:** complete the explicit work packages in
   [the native property-graph plan](./08-postgresql-19-property-graphs.md), with
   native mapping authority and a separate SQL/PGQ matrix.
10. **Conformance closeout:** every applicable normative row is green; rows
    delegated to PostgreSQL have proof at the adapter boundary; no full GQL
    claim before this gate.

## Acceptance Gates

- The registry is machine-readable, complete for the selected edition, and
  fails CI when code/docs/tests drift.
- Every supported feature has positive, negative, diagnostic-location,
  security, memory-limit, and transaction tests as applicable.
- Parallel same-type/same-endpoint relationships preserve separate identity and
  properties through all storage and mutation paths.
- Two-role tests prove hidden rows/edges cannot affect coordinate-only or
  hydrated nodes, paths, aggregates, or existence.
- Savepoint rollback/release and all supported isolation levels preserve
  PostgreSQL-first truth.
- Parser/binder fuzzing and generated AST/property tests cover the expanded
  grammar without panics or unbounded work.
- `graph.gql_explain()` reports semantic operators, estimates, chosen path
  mode, memory/work budget, spill decisions, and pushed PostgreSQL predicates.
- Public docs clearly distinguish full GQL status, SQL/PGQ status, and the
  narrow openCypher compatibility layer.
