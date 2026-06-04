# GQL Path Pattern Variables and Wildcard Patterns Plan

## Objective

Implement reviewer-requested GQL pattern support in three phases:

1. Support the useful minimum: `MATCH p=()-[]->() RETURN p` and path functions over `p`.
2. Generalize the same architecture to named node and relationship variables with optional labels and relationship types.
3. Extend the generalized pattern machinery to multi-segment patterns, multi-pattern joins, wildcard property predicates, variable-length wildcard paths, and carefully constrained wildcard writes.

This plan is intentionally additive. It should extend the existing parser, binder, physical plan, executor, and JSON projector without introducing a new crate or rewriting the current single-relationship GQL execution path.

## Non-Goals

- Full GQL, SQL/PGQ, or openCypher compatibility.
- Multi-pattern joins in Phase 1 or Phase 2.
- Multi-relationship fixed patterns such as `()-[]->()-[]->()` in Phase 1 or Phase 2.
- Writes over wildcard relationship patterns in Phase 1 or Phase 2.
- Property predicates on unlabeled wildcard nodes in Phase 1.
- Variable-length wildcard paths in Phase 1 or Phase 2.

## Current Constraints

The parser already represents optional node variables, node labels, relationship variables, and relationship types. Binding currently rejects anonymous nodes, unlabeled nodes, and anonymous relationship types. The read physical plan also assumes one concrete source table, one concrete relationship type, and one concrete target table.

The existing path support is tied to relationship variables on variable-length patterns, such as `[p:friend*1..2]`. A path pattern variable like `p=(s)-[r]->(e)` is a separate syntax and binding concept.

This work also intersects with the known-issues register:

- KI-002: traversal/path entry-point behavior for transaction-created nodes is intentionally narrow today.
- KI-003: GQL and Cypher parser surfaces need dedicated fuzz targets before broadening syntax.
- KI-005: overlay neighbor merging and transaction-delta lookups should be measured before optimizing.
- KI-006: `graph.cypher()` remains a narrow compatibility preview, not full openCypher.
- KI-007: SQL/PGQ remains an internal adapter seam, not a public SQL API.
- KI-008: several GQL parser, semantics, value, facade, and test files are already large enough that new work should avoid making them harder to review.

## Phase 1: Path-Only Wildcard Single-Hop Reads

Status: implemented and documented on 2026-06-02. Verification is recorded in
`todo/measurements.md` under "Phase 1 Executor and SQL Slice".

### Supported Queries

```sql
MATCH p=()-[]->() RETURN p
MATCH p=()-[]->() RETURN nodes(p) AS ns, relationships(p) AS rs, length(p) AS len
MATCH p=()-[]-() RETURN p
MATCH p=()<-[]-() RETURN p
```

### Rejected in Phase 1

```sql
MATCH p=(s)-[]->() RETURN s
MATCH p=()-[r]->() RETURN r
MATCH p=()-[:friend]->() RETURN p
MATCH p=(:users)-[]->() RETURN p
MATCH p=()-[]->() WHERE p.id = 'x' RETURN p
MATCH p=()-[]*1..3->() RETURN p
MATCH p=()-[]->() DELETE p RETURN p
```

These are valid future extensions, but Phase 1 should not expose synthetic node or relationship variables. Only the explicitly declared path variable is visible.

### AST Changes

Add an optional path variable to `Pattern`:

```rust
pub(crate) struct Pattern {
    pub(crate) path_var: Option<Ident>,
    pub(crate) start: NodePat,
    pub(crate) tail: Vec<(RelPat, NodePat)>,
    pub(crate) span: Span,
}
```

Update `parse_pattern()` to accept an optional `Ident Eq` prefix before the first node pattern. Keep this local to pattern parsing so other clauses do not need to understand the prefix syntax.

### Semantic Binding Changes

Introduce explicit selector types instead of representing wildcard state with empty strings or sentinel OIDs:

```rust
pub(crate) enum BoundNodeSelector {
    Any,
    Table { label: String, table_oid: u32, properties: BTreeSet<String> },
}

pub(crate) enum BoundRelTypeSelector {
    Any,
    Type { rel_type: String },
}
```

For Phase 1, bind only this shape:

- `Pattern.path_var` is present.
- Pattern has exactly one relationship.
- Both node patterns have no variable, no label, and no property map.
- Relationship has no variable, no type, no property map, and no variable-length bounds.
- `RETURN` items reference only the path variable or supported path functions over that variable.
- `WHERE`, `WITH`, `ORDER BY`, aggregates, and writes are rejected unless already proven safe for this path-only shape.

Add a dedicated logical statement variant if that keeps the existing concrete `LogicalPlan` clean:

```rust
LogicalStatement::WildcardPathRead(LogicalWildcardPathPlan)
```

The alternative is widening the existing `LogicalPlan`; use that only if the final enum shape remains clear and avoids repeated `match Any` branches in unrelated paths.

### Physical Plan Changes

Create a dedicated physical plan for Phase 1 wildcard path reads:

```rust
pub(crate) struct PhysicalWildcardPathPlan {
    pub(crate) path_var: String,
    pub(crate) direction: BoundDirection,
    pub(crate) returns: Vec<ReturnSlot>,
    pub(crate) required_node_table_oids: BTreeSet<u32>,
    pub(crate) table_labels: BTreeMap<u32, String>,
    pub(crate) rel_type_labels: BTreeMap<u8, String>,
    pub(crate) skip: Option<u64>,
    pub(crate) limit: Option<u64>,
}
```

This keeps ACL, hydration, explain, and projection explicit for wildcard plans and avoids weakening the concrete one-hop plan.

### Catalog Snapshot Changes

Extend the catalog port with enumeration APIs:

```rust
fn node_labels(&self) -> Vec<NodeLabelInfo>;
fn rel_types(&self) -> Vec<RelTypeInfo>;
```

`CatalogSnapshotImpl` should derive these from the already-loaded label and relationship snapshots. `FakeCatalog` should implement the same APIs so unit tests can bind wildcard paths without SPI.

### Executor Changes

Add a wildcard path executor that scans active graph edges and produces one path row per matching edge and direction:

- For `->`, use forward CSR/overlay neighbors.
- For `<-`, use reverse CSR/overlay neighbors, while preserving registered edge start/end orientation in output.
- For `-`, scan both directions and deduplicate identical relationship orientations.
- Respect tenant filtering for both endpoints.
- Respect deleted nodes and transaction edge overlays.
- Enforce the existing GQL result row cap behavior.

Each returned path relationship must carry the actual relationship type ID or label. The current projector uses the plan-level relationship type; wildcard path rows need per-step type data.

### Projection Changes

Add wildcard path projection functions or generalize existing path projection around a small trait-like helper:

- Node label lookup by table OID.
- Relationship type lookup by type ID.
- Relationship endpoint output based on actual endpoint table OIDs.

Output shape must follow `todo/gql-path-output-spec.md`.

### ACL and Hydration

ACL must fail closed. For Phase 1, check `SELECT` privilege on every registered node table that can appear in a wildcard path before executing the plan. This may be conservative, but it prevents leaking existence of relationships involving inaccessible tables.

Hydration should use the existing row hydration path, but wildcard path plans must pass all table OIDs that appear in the result set or all predeclared possible table OIDs. Prefer result-set hydration to limit work after ACL has already passed.

Transaction-created nodes must follow the KI-002 policy. Phase 1 should either reject wildcard path traversal that would need temporary-ID traversal support with a typed GQL execution error, or explicitly implement temporary-ID traversal before allowing those nodes as wildcard path entry points. Do not silently omit transaction-created nodes while documenting wildcard path reads as fully overlay-aware.

### Explain Output

Add a stable explain string:

```text
WildcardPathExpand(path=p, rel=*, hops=1..1, return=[p])
```

For direction-specific queries:

```text
WildcardPathExpand(path=p, direction=out, rel=*, hops=1..1, return=[p])
```

## Phase 2: Named Variables and Optional Concrete Filters

Status: implemented and documented on 2026-06-02. Verification is recorded in
`todo/measurements.md` under "Phase 2 Named Element Slice".

### Supported Queries

```sql
MATCH p=(s)-[r]->(e) RETURN p, s, r, e
MATCH p=(s:users)-[]->(e) RETURN p, s, e
MATCH p=(s)-[:friend]->(e) RETURN p, s, e
MATCH p=(s:users)-[r:friend]->(e:users) RETURN p, r
MATCH p=(s)-[r]->(e) RETURN nodes(p), relationships(p), length(p)
```

### Binding Rules

- Explicit node variables become visible in scope.
- Explicit relationship variables become visible in scope.
- A path variable is visible independently from node and relationship variables.
- Duplicate variable names across path, node, and relationship bindings are rejected.
- Optional node labels narrow the node selector to one table.
- Optional relationship types narrow the edge type selector to one relationship type.
- If a node is unlabeled but a property reference is requested, reject unless the property is known to be unambiguous across all possible concrete tables for that binding.
- If a relationship is untyped but projected, its `_type` comes from the actual matched edge type.

### Planner Shape

Phase 2 can either:

- Extend `PhysicalWildcardPathPlan` with optional node/relationship variable slots, or
- Introduce a more general `PhysicalPatternPlan` with one relationship segment.

Prefer a single-segment `PhysicalPatternPlan` once Phase 2 starts, because it will support all combinations of concrete and wildcard selectors without duplicating executor logic.

### Property Semantics

Property binding over wildcard nodes is the most sensitive Phase 2 area.

Recommended first rule:

- Concrete label: property binding behaves as today.
- Wildcard label: property references are rejected unless every possible concrete table for the binding has the property and the property is not reserved.

This is conservative and prevents silent nulls or table-dependent output schemas.

### Relationship Projection

Relationship JSON must use actual per-edge metadata:

- `_type` from the matched edge type ID.
- `_start` and `_end` in registered edge direction.
- Endpoint labels resolved from endpoint table OIDs.

For reverse query matches, the relationship JSON should still describe the registered relationship orientation, while path order remains query traversal order.

### Cypher and SQL/PGQ Surfaces

Do not automatically expose Phase 1 or Phase 2 syntax through `graph.cypher()` until the overlapping openCypher semantics are reviewed and documented. The known issue register explicitly treats `graph.cypher()` as a narrow compatibility preview.

Do not expose a public SQL/PGQ API as part of this work. Any SQL/PGQ adapter changes should remain internal and should exist only to keep shared binding tests coherent.

### File and Module Boundaries

This feature touches files already listed in KI-008, especially:

- `graph/src/gql/parser.rs`
- `graph/src/query/semantics.rs`
- `graph/src/query/value.rs`
- `graph/src/query/tests.rs`
- `graph/src/sql_facade/gql.rs`
- `graph/src/pg_tests/gql.rs`

Keep Phase 1 edits narrow. If a file grows materially harder to review, introduce a focused module such as `query/wildcard_path.rs` or split test fixtures before adding more cases to an already oversized file.

## Risks

### ACL Leakage

Wildcard patterns can reveal the existence of relationships across tables the caller cannot read. Pre-check all possible node tables for Phase 1. For Phase 2, pre-check concrete selector tables plus every table reachable through wildcard selectors.

### Incorrect Relationship Type in Output

Existing path projection uses a plan-level relationship type. Wildcard matching needs per-edge type metadata in `GqlPathRelationship` or an equivalent row structure.

### Incorrect Node Labels in Output

Existing path projection maps table OID to either source or target label. Wildcard paths need an explicit table-OID-to-label map.

### Dynamic Relationship Labels

Edges registered with `label_column` can map to multiple labels. Wildcard execution must preserve the built edge type ID and project that actual label.

### Hydration Cost

Wildcard paths may touch many tables. Keep row caps enforced before hydration where possible, and avoid hydrating unused rows when `hydrate := false`.

### Duplicate Rows

Undirected wildcard matching can see the same stored edge from both forward and reverse scans. Deduplicate on registered relationship identity and traversal orientation according to the output contract.

### Transaction Overlay Drift

Wildcard expansion must merge base CSR edges and transaction overlay edges the same way existing concrete traversal does. Deleted nodes and deleted edges must remain hidden.

Transaction-created node entry points remain a known policy gap. Phase 1 must include a test proving either the typed rejection behavior or the implemented temporary-ID traversal behavior.

### Scope Drift

Phase 1 synthetic variables must not leak into `RETURN`, `WHERE`, `WITH`, or `ORDER BY`.

### Over-Broad Planner Changes

Do not widen every existing concrete plan field to `Option<T>`. Use selector enums or a dedicated wildcard plan so unsupported states are represented explicitly and reviewed locally.

### Performance Regression Risk

Wildcard path expansion may exercise overlay neighbor merging and transaction-delta lookup paths more heavily than concrete relationship expansion. Follow KI-005: measure first, then optimize. Do not change duplicate suppression, overlay indexing, or transaction delta snapshotting solely by inspection.

## Test Plan

### Parser Unit Tests

- Parses `MATCH p=()-[]->() RETURN p` with `Pattern.path_var = p`.
- Parses inbound and undirected path variable patterns.
- Rejects malformed prefixes such as `MATCH p ()-[]->() RETURN p`.
- Preserves existing parsing for `MATCH (u:users)-[:friend]->(v:users) RETURN u`.

### Binder Unit Tests

- Phase 1 accepts `MATCH p=()-[]->() RETURN p`.
- Phase 1 accepts `nodes(p)`, `relationships(p)`, and `length(p)`.
- Phase 1 rejects `RETURN s` when `s` was not explicitly bound.
- Phase 1 rejects explicit node variables until Phase 2.
- Phase 1 rejects labels and relationship types until Phase 2.
- Phase 1 rejects `WHERE`, `WITH`, aggregates, and writes over wildcard path plans.
- Phase 1 plans include all registered node table OIDs for ACL.
- Phase 1 plans include table label and relationship type lookup maps.

### Executor Unit Tests

- Outbound wildcard path returns one row per outgoing edge.
- Inbound wildcard path returns one row per incoming edge in query traversal order.
- Undirected wildcard path deduplicates repeated stored relationships.
- Tenant scope filters both endpoints.
- Deleted transaction nodes and edges are hidden.
- Inserted overlay edges are visible.
- Row cap is enforced before unbounded result collection.

### Projection Unit Tests

- `RETURN p` returns `_path.nodes` and `_path.relationships`.
- `nodes(p)` equals `p._path.nodes`.
- `relationships(p)` equals `p._path.relationships`.
- `length(p)` equals the relationship count.
- Relationship `_type` uses the actual matched edge type.
- Node `_id.table` uses table-OID label lookup, not source/target assumptions.
- Hydrated and coordinate-only output both match the output spec.

### SQL/PGRX Tests

- Register at least two node tables and two relationship types, then run `MATCH p=()-[]->() RETURN p`.
- Confirm dynamic label-column relationships project actual labels.
- Confirm ACL failure when caller lacks access to one possible wildcard node table.
- Confirm `hydrate := false` returns coordinate-only nodes.
- Confirm `LIMIT` works and does not require hydrating rows beyond the window when no sort/aggregate is present.

### Fuzz and Regression Tests

- Status: GQL and Cypher parser fuzz targets are present. The GQL corpus now
  includes path-variable and wildcard-delete seeds; the Cypher corpus includes
  compatible-match and unsupported-call seeds.
- Add path variable syntax to the existing GQL parser fuzz corpus. Done for
  `MATCH p=()-[]->() RETURN p, nodes(p), relationships(p), length(p)`.
- Include anonymous/wildcard node and relationship patterns in parser fuzz
  targets. Done for both wildcard path projection and wildcard relationship
  delete seeds.
- Include malformed path-variable prefixes such as missing `=`, repeated `=`,
  and path variables before non-pattern clauses. Done in the GQL parser corpus.
- Include mixed directed forms: `->`, `<-`, and `-`. Done in the GQL parser
  corpus.
- Include bounded syntax such as `MATCH p=()-[]*1..3->() RETURN p`. Done in
  the GQL parser corpus; this is now an accepted Phase 3D shape rather than a
  Phase 1 rejection.
- Include syntactically adjacent labels and types, such as
  `MATCH p=(:users)-[]->() RETURN p` and
  `MATCH p=()-[:friend]->() RETURN p`. Done in the GQL parser corpus; these
  are now accepted Phase 2 selector shapes.
- Include duplicate variable names once Phase 2 starts, such as
  `MATCH p=(p)-[]->() RETURN p` and `MATCH p=(s)-[s]->(e) RETURN p`. Done in
  the GQL parser corpus.
- Add a Cypher parser regression corpus entry only if `graph.cypher()` is intentionally taught to parse the new syntax; otherwise add a compatibility test proving the syntax is rejected with a documented limitation. Done for the current compatibility parser surface with one compatible match seed and one unsupported-call seed.
- Keep parser fuzzing as a Phase 1 release gate, not a post-implementation cleanup task, because KI-003 explicitly calls out fuzz coverage before broadening language syntax.

### Benchmark and Measurement Tests

- Add a small benchmark or measured unit fixture for wildcard expansion over mixed edge labels.
- Compare concrete `MATCH (u:users)-[:friend]->(v:users)` against wildcard `MATCH p=()-[]->() RETURN length(p)` on the same topology to catch obvious overhead.
- Add an overlay-heavy measurement fixture before changing overlay merge or transaction-delta lookup internals.

## Implementation Order

1. Parser and AST test for `Pattern.path_var`.
2. Catalog enumeration APIs and fake catalog support.
3. Phase 1 logical/physical plan and explain output.
4. Phase 1 executor with actual edge type metadata.
5. Phase 1 projection and output spec tests.
6. Phase 1 SQL/PGRX coverage.
7. Phase 2 scope binding for explicit node and relationship variables.
8. Phase 2 optional label/type selectors.
9. Phase 2 property binding rules for concrete and wildcard nodes.
10. Phase 2 SQL/PGRX coverage.

## Completion Criteria

Phase 1 is complete when `MATCH p=()-[]->() RETURN p` works through `graph.gql()` with correct ACL, tenant filtering, hydration, path JSON, and path functions.

Phase 2 is complete when named path, node, and relationship variables work for a single relationship segment with optional node labels and relationship types, while unsupported wildcard property and write cases fail with explicit GQL errors.

## Phase 3: Multi-Segment, Join, Predicate, Variable-Length, and Write Expansion

Phase 3 is where the deferred reviewer-adjacent capabilities belong. These are viable, but they should not be mixed into Phase 1 or Phase 2 because they require row-stream planning, multi-segment path state, or write-boundary correctness work.

### Phase 3A: Multi-Relationship Fixed Patterns

Status: implemented and documented on 2026-06-02. Verification is recorded in
`todo/measurements.md` under "Phase 3A Fixed Multi-Segment Slice".

Target examples:

```sql
MATCH p=()-[]->()-[]->() RETURN p
MATCH p=(a)-[:friend]->(b)-[:works_at]->(c) RETURN p, a, b, c
```

Architectural plan:

- Replace the one-segment physical pattern plan with a segment-chain plan.
- Represent each segment with source selector, relationship selector, direction, and target selector.
- Carry path state across segments, including per-step relationship type and endpoint metadata.
- Define duplicate semantics for repeated nodes and repeated relationships.
- Add row caps before hydration and projection.

Tests:

- Two-segment concrete pattern returns expected paths.
- Two-segment wildcard pattern preserves each relationship's actual `_type`.
- Mixed concrete/wildcard segments filter only the intended segment.
- Repeated-node/cycle behavior is explicit and tested.
- Tenant, deleted-node, and overlay filtering apply at every segment.

### Phase 3B: Multi-Pattern Joins

Status: initial fixed single-hop join slice implemented and documented on
2026-06-02. Reused node variables join by graph coordinate, independent
patterns produce Cartesian combinations under the result cap, and node/node
property returns with joined node-property `WHERE` predicates and `SKIP`/`LIMIT`
are supported, including `ORDER BY` over joined node properties or returned
property aliases, fixed single-hop relationship variable returns, fixed
single-hop path variable returns, path functions over fixed single-hop path
variables, node and node-property aggregate inputs, relationship-value and
path-value `count`/`collect`, and projected-row `RETURN DISTINCT`, plus node
and node-property, relationship, and path `WITH` projections with
`WITH DISTINCT`, path-function `WITH` projections, and aggregate `WITH`
projections, plus bounded variable-length relationship patterns. Relationship
properties and relationship/path property aggregate inputs remain planned within
this phase.

Target examples:

```sql
MATCH (a)-[]->(b), (b)-[]->(c) RETURN a, c
MATCH (u:users)-[]->(v), (v)-[]->(w) RETURN u, w
```

Architectural plan:

- Introduce an intermediate row-stream representation for bound variables.
- Bind each pattern against the current variable scope.
- Add join semantics for reused variables, including node identity and relationship identity equality.
- Add cardinality controls before projection.
- Keep join ordering conservative at first: execute patterns left-to-right, then benchmark before adding a cost model.

Tests:

- Reused node variables join by graph coordinate.
- Independent patterns produce expected Cartesian behavior under row caps.
- Conflicting labels/types on reused variables fail with a binding error.
- Predicates over variables from different patterns are evaluated after both bindings exist.
- Projected-row `RETURN DISTINCT` deduplicates after projection and before
  ordering/windowing.
- `nodes(p)`, `relationships(p)`, and `length(p)` project from fixed
  single-hop path variables in multi-pattern joins.
- `count(*)` and node-property aggregates group over fixed single-hop
  multi-pattern join rows.
- Relationship-value and path-value `count`/`collect` aggregate over fixed
  single-hop multi-pattern join rows.
- Node, node-property, relationship, and path `WITH` projections update
  downstream multi-pattern join scope, and `WITH DISTINCT` deduplicates before
  later projection or aggregation.
- Path-function `WITH` projections update downstream multi-pattern join scope.
- Aggregate `WITH` projections update downstream multi-pattern join scope for
  supported aggregate inputs, including grouped aggregate `WITH` clauses with
  hidden grouping keys, direct aggregate `WITH DISTINCT` clauses, later
  `WITH DISTINCT` projections over already-projected aggregate aliases, and
  aggregate expressions over projected aggregate aliases after aggregate
  `WITH` boundaries.
- Optional multi-pattern joins null-extend unmatched fixed single-hop pattern
  expansions while preserving previously bound node variables; missing
  relationship and path projections return JSON `null`.
- Bounded variable-length multi-pattern joins preserve path length and path
  values while joining later patterns against the bounded endpoint.
- Variable-length relationship variables in multi-pattern joins are treated as
  path values.

### Phase 3C: Property Predicates on Unlabeled Wildcard Nodes

Status: compatible path-node property predicates implemented and documented on
2026-06-02. Verification is recorded in `todo/measurements.md` under
"Phase 3C Wildcard Property Predicate Slice". Relationship properties and
partially available or ambiguous node properties remain planned.

Target examples:

```sql
MATCH (n)-[]->() WHERE n.name = 'Alice' RETURN n
MATCH p=(s)-[]->(e) WHERE s.status = 'active' RETURN p
```

Architectural plan:

- For unlabeled node bindings, compute the set of possible concrete node tables from pattern selectors and relationship registrations.
- Permit property access only when every possible table has that property with compatible semantics.
- Reject ambiguous or partially available properties with typed binding errors.
- Keep JSONB path behavior explicit: require compatible root columns across all possible concrete tables.

Tests:

- Wildcard property predicate succeeds when all possible tables expose the property.
- Predicate fails when the property exists on only some possible tables.
- Predicate fails when property semantics differ between scalar and JSONB path usage.
- Hydration failures remain fail-closed.

### Phase 3D: Variable-Length Wildcard Paths

Status: bounded single-segment wildcard variable-length path variables
implemented and documented on 2026-06-02 using bounded walk semantics. Nodes and
relationships may repeat within the explicit maximum hop bound, and row caps are
enforced before projection. Verification is recorded in `todo/measurements.md`
under "Phase 3D Variable-Length Wildcard Path Slice". Named node/relationship
variables on variable-length wildcard segments, multi-segment variable-length
wildcard paths, and type alternation remain planned.

Target examples:

```sql
MATCH p=()-[]*1..3->() RETURN p
MATCH p=(s)-[:friend|works_at*1..3]->(e) RETURN length(p)
```

Architectural plan:

- Generalize path expansion to allow a relationship selector per hop.
- Store actual relationship type metadata for every step.
- Use bounded walk semantics for this first wildcard variable-length slice.
- Enforce maximum hop bounds and row caps before projection.
- Benchmark wildcard variable-length expansion separately from concrete variable-length expansion.

Tests:

- Variable-length wildcard paths return all expected bounded paths.
- Relationship labels are correct per step.
- Cycle handling matches the documented path semantics.
- Row-cap exhaustion returns the expected typed execution error.
- Overlay and tenant filtering apply at every hop.

### Phase 3E: Writes over Wildcard Relationship Patterns

Status: exact single-mapping wildcard relationship deletes implemented and
documented on 2026-06-02. Endpoint labels and relationship types may be omitted
only when the pattern resolves to exactly one static-label registered edge-row
mapping. Ambiguous, dynamic-label, or unmapped wildcard relationship writes
remain rejected before execution. Verification is recorded in
`todo/measurements.md` under "Phase 3E Wildcard Relationship Delete Slice".

Target examples:

```sql
MATCH ()-[r]->() DELETE r RETURN r
MATCH (s)-[r:friend]->(e) DELETE r RETURN s, e
```

Architectural plan:

- Restrict wildcard writes to explicitly named relationship variables.
- Resolve every matched relationship to an exact registered edge-row mapping before write execution.
- Pre-check `SELECT` and `DELETE` privileges on every possible node and edge table before execution.
- Reject ambiguous bidirectional/self-edge mappings with typed errors.
- Re-check matched predicates and relationship identity at the PostgreSQL write boundary, aligning with KI-001.
- Keep wildcard writes unavailable for relationships that are not backed by a registered edge-row table.

Tests:

- Wildcard delete removes the exact matched edge row.
- Dynamic relationship labels delete only matching rows.
- Ambiguous bidirectional/self-edge rows are rejected.
- Missing edge-table delete privilege fails before mutation.
- Predicate re-check prevents stale-match drift under concurrent source-row changes.
- Rollback and SQLSTATE behavior match existing GQL write weak-path policy.

### Phase 3 Release Criteria

Phase 3 should not be treated as one large all-or-nothing milestone. Each subphase is complete only when it has parser, binder, executor, projection, SQL/PGRX, fuzz, and where applicable benchmark coverage.

Status: parser fuzz target coverage for the GQL and Cypher frontends is in
place. Local macOS fuzz builds use `graph/fuzz/build.sh` so `cargo-fuzz`
preserves the pgrx dynamic-lookup linker flag; measurements are recorded in
`todo/measurements.md` under "Phase 3 Parser Fuzz Gate".
