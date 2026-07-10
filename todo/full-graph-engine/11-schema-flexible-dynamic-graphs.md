# Schema-Flexible Dynamic Labels And JSONB Properties

## Scheduling

This is a late, post-backlog capability. It does not change the active
checkpoint or add work to the current Roadmap/Known-Issues closure gate.
Implementation starts only after checkpoints 0 through 9 are green, including:

- hard memory governance, out-of-core build, bounded load/sync/compaction, and
  safe generation publication;
- canonical enums/newtypes, exact `GraphValue`, pgrx-first PostgreSQL adapters,
  and the unsafe allowlist;
- full applicable GQL property, expression, coercion, write, transaction, and
  security foundations;
- PostgreSQL 19 native property-graph catalog support and its separate
  SQL/PGQ compatibility boundary.

An exploratory prototype may run earlier only on a disposable branch and does
not change public APIs, catalogs, artifacts, or conformance claims.

## Outcome

Allow one PostgreSQL node table and one PostgreSQL edge table to represent many
graph labels/types and evolving property sets:

```sql
CREATE TABLE app.graph_node (
    node_id uuid PRIMARY KEY,
    label text NOT NULL,
    properties jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE app.graph_edge (
    edge_id uuid PRIMARY KEY,
    source_id uuid NOT NULL REFERENCES app.graph_node(node_id),
    target_id uuid NOT NULL REFERENCES app.graph_node(node_id),
    label text NOT NULL,
    properties jsonb NOT NULL DEFAULT '{}'::jsonb
);
```

Changing a row's label or adding/removing a top-level JSONB property changes
its graph view without table DDL. Stable relational mappings remain the
preferred path for well-defined schemas and hot typed properties.

This is **schema-flexible**, not literally schemaless:

- PostgreSQL still owns table shape, keys, endpoint constraints, JSONB
  validity, generated columns, indexes, triggers, ACLs, RLS, MVCC, WAL,
  backup, and recovery;
- every node and relationship still requires stable source-row identity;
- pgGraph stores derived label membership, statistics, and explicitly selected
  property indexes, never a second authoritative property document;
- every graph write executes PostgreSQL DML first and projection sync follows
  the accepted source-row change.

## Reference Capability And PostgreSQL Boundary

Google Spanner Graph exposes `DYNAMIC LABEL` from a string column and
`DYNAMIC PROPERTIES` from a JSON column. Its documented model commonly uses
one node table and one edge table; only top-level JSON keys become graph
properties, and frequently filtered properties can be promoted into indexed
generated columns.

PostgreSQL 19's current `CREATE PROPERTY GRAPH` grammar describes declared
labels and declared property expressions, not Spanner's dynamic clauses.
Therefore the first pgGraph implementation is an explicit pgGraph extension
mapping available through pgGraph GQL on supported PostgreSQL versions. It
must not:

- pretend the dynamic mapping is part of a PostgreSQL 19 native graph object;
- duplicate one graph definition across independently editable native and
  pgGraph catalogs;
- claim native `GRAPH_TABLE` sees or accelerates dynamic labels/properties;
- add unofficial PostgreSQL parser hooks.

If a later stable PostgreSQL release adds equivalent catalog syntax, the
version adapter may consume that native definition as the authority and retire
the duplicate pgGraph registration path.

References:

- [Spanner Graph: manage schemaless data](https://docs.cloud.google.com/spanner/docs/graph/manage-schemaless-data)
- [PostgreSQL 19: CREATE PROPERTY GRAPH](https://www.postgresql.org/docs/19/sql-create-property-graph.html)
- [PostgreSQL JSONB types and indexing](https://www.postgresql.org/docs/19/datatype-json.html)
- [PostgreSQL indexes on expressions](https://www.postgresql.org/docs/19/indexes-expressional.html)
- [PostgreSQL generated columns](https://www.postgresql.org/docs/19/ddl-generated-columns.html)

## Existing Foundations And Material Gaps

Reuse these existing or active foundations after their current work packages
are stable:

- dynamic relationship labels already flow from the registered edge
  `label_column` through build and sync;
- registered dotted JSONB paths have validation/read foundations that preserve
  missing versus explicit JSON null;
- hydration is PostgreSQL-authoritative and already has individual/batched
  `to_jsonb(src.*)` paths;
- normal property payloads are not copied into the topology artifact;
- typed filter values, mutable deltas, source row images, `SourceChange`, and
  artifact-vNext plans can carry the bounded incremental design.

The late package must close rather than work around these gaps:

- nodes have no dynamic label source; current node membership and label
  binding derive from the registered table;
- there is no typed `properties_column` mapping for node or relationship
  tables, and JSONB keys must be registered individually;
- binder/catalog logic assumes one node table per label, making multiple
  mappings that emit the same runtime label ambiguous instead of unionable;
- current relationship label IDs use a narrow `u8` registry and bind-time
  discovery can collect an unbudgeted `SELECT DISTINCT` label set;
- filter registration and source search accept physical columns rather than a
  typed JSONB property-path source;
- change capture does not yet cover every standalone edge-row table, and a
  dotted property string must never be quoted as though it were one physical
  column name.

Relevant ownership points include `graph/src/builder.rs`,
`graph/src/catalog/validate.rs`, `graph/src/query/catalog_snapshot.rs`,
`graph/src/query/semantics.rs`, `graph/src/sql_hydration.rs`,
`graph/src/sql_search.rs`, `graph/src/sql_sync.rs`, and
`graph/src/sync.rs`. Preserve these as shared engine boundaries; do not fork
schemaless-specific copies.

## Canonical Model

Do not add another collection of optional strings to the registration catalog.
Convert SQL names once into stable OID/attribute identities and use typed
domain state:

```text
ElementMapping
  Static(DeclaredElementMapping)
  Dynamic(DynamicElementMapping)

LabelSource
  Static(LabelName)
  TextColumn(AttributeId)
  TextArrayColumn(AttributeId)       # later, after single-label semantics
  StaticAndColumn { static_label, attribute }

PropertySource
  Declared(PropertyBindings)
  JsonbObject(AttributeId)
  Hybrid { declared, jsonb_attribute, conflict_policy }

PropertyConflictPolicy
  Reject                            # safe default
  DeclaredWins                      # explicit compatibility choice
```

`LabelSource`, `PropertySource`, and `PropertyConflictPolicy` are closed enums.
Runtime labels and property keys are open vocabularies, so they are validated
`LabelName` and `PropertyKey` newtypes rather than impossible-to-maintain Rust
enums or unvalidated `String` values. Table and column identities use the
production OID/attribute newtypes from the Rust boundary plan.

The initial supported profile is deliberately narrow:

- one data-driven label per row, optionally combined with one declared label;
- one JSONB object column per element mapping;
- top-level JSONB keys become dynamic properties;
- nested objects/arrays remain exact values and are accessed through explicit
  JSON/path operations rather than being recursively flattened;
- SQL NULL document, missing key, JSON null, and a present value remain four
  distinguishable states where the selected GQL/SQL semantics require it;
- a declared/dynamic property-name collision is rejected unless the mapping
  explicitly selects `DeclaredWins`;
- empty, invalid, or normalization-colliding labels fail with typed mapping or
  source-data errors rather than silently disappearing.

Label spelling and comparison rules must be derived once from the selected GQL
identifier rules. Preserve original display spelling, canonicalize lookup in
one place, and reject two stored labels that collapse to the same canonical
name. Do not copy Spanner's lowercase-only restriction unless it is selected
as an explicit compatibility profile.

## Architecture

Keep this inside the existing pgrx crate and its planned modules. It does not
justify a new crate, async runtime, background task family, or second query
engine.

```text
PostgreSQL registration/native catalog adapters
                  |
                  v
       typed ElementMapping port
                  |
     +------------+-------------+
     |                          |
bounded build/sync       binder/planner/runtime
     |                          |
dynamic label IDs       GraphValue + JSON coercion
     |                          |
validated artifact       visibility/hydration port
     +------------+-------------+
                  |
       authoritative PostgreSQL rows
```

Reuse the canonical binder, planner, streaming operators, visibility adapter,
hydration adapter, PostgreSQL-first DML, source-change model, generation
publisher, and resource governor. Do not create a schemaless-specific parser,
planner, runtime, projection lifecycle, or property store.

## Work Packages

### DYN-0: Freeze Semantics And SQL Contract

Start with failing acceptance tests and a contract document covering:

- single versus multiple dynamic labels and static-plus-dynamic labels;
- label normalization, quoted identifiers, Unicode, and collision behavior;
- top-level property exposure and explicit nested JSON/path access;
- SQL NULL, missing, JSON null, scalar, object, and array behavior;
- heterogeneous values for the same property key across rows/labels;
- strict versus lax conversion, comparison, arithmetic, grouping, DISTINCT,
  and ordering semantics;
- declared/dynamic property collisions and JSONB container-column visibility;
- unsupported native PostgreSQL 19/`GRAPH_TABLE` behavior;
- stable error/diagnostic identifiers and mapping guidance.

Dynamic JSONB properties lower into the existing exact `GraphValue` algebra.
Their bound type retains JSON-origin metadata so the binder can require an
explicit strict/lax conversion when an operation needs one stable scalar type.
Do not make `serde_json::Value` or `f64` the execution value system.

**Exit:** one reviewed contract and conformance-registry extension owns every
semantic choice; public API names and upgrade behavior are frozen before
catalog or artifact changes.

### DYN-1: OID-Stable Registration And Catalog Model

- Add typed dynamic element mappings behind the shared catalog port.
- Accept relations as `regclass`/pgrx OIDs and resolve label/property columns
  to stable attribute identities once.
- Validate node/edge keys, endpoint mappings, label source types, JSONB object
  source type, tenant mapping, privileges, RLS mode, and writable columns.
- Keep one authoritative mapping kind per graph: manual static, PostgreSQL 19
  native, or pgGraph dynamic extension.
- Fingerprint every dependency and fail closed on dropped/retyped columns,
  relation recreation, or incompatible mapping drift.
- Generate `graph_map()`/status output from the typed mapping rather than a
  second hand-maintained vocabulary.

**Exit:** registration survives relation/schema rename through OID identity,
rejects drop/recreate and invalid source types, and has forward upgrade and
rollback/re-registration tests.

### DYN-2: Bounded Build And Artifact Representation

- Replace the narrow relationship-label registry with the checked production
  `LabelId` capacity selected by RUST-3; the dynamic capability does not ship
  with the current `u8`/254-user-label ceiling.
- Assign `LabelId` values from an external-sort/spill dictionary; never retain
  unbounded distinct labels or property keys in build or bind memory.
- Persist label dictionary and node/relationship label membership in mmap-ready
  sections with checked widths, validation, versioning, and rebuild policy.
- Keep full JSONB documents in PostgreSQL. The default artifact contains no
  whole-document copy.
- Persist only explicitly promoted derived property/filter indexes, each with a
  stable mapping/property identity and resource owner.
- Enforce byte/count limits for label length, labels per element, distinct
  labels, JSONB document size, top-level keys, key length, value size, sampled
  statistics, and spill storage.
- Preserve stable relationship source identity and parallel edges.

Under pressure the builder shrinks batches and spills dictionaries/runs. A
large or adversarial JSONB document returns a typed resource/data error before
allocation; it does not OOM a PostgreSQL backend.

**Exit:** a graph whose label dictionary and source JSONB exceed the heap
budget builds through forced spill; in-memory/spill checksums and query results
match; corrupt dynamic sections cannot construct a serving graph.

### DYN-3: Query, Hydration, And Typed Conversion

- Match dynamic node and relationship labels through the same label predicates
  and binding-table IR as declared labels.
- Replace `label -> one table` binding with a typed multi-source label scan that
  unions every authorized mapping contributing the label; do not run an
  unbudgeted full-table `SELECT DISTINCT` merely to bind a query.
- Resolve a top-level dynamic property lazily from the source JSONB document or
  an explicitly promoted derived index.
- Batch visibility/hydration under the caller's PostgreSQL role and snapshot;
  never perform one SPI lookup per property or element.
- Preserve exact JSONB numeric values through `GraphValue`/PostgreSQL `numeric`.
- Implement strict and lax conversions by one typed conversion registry, with
  PostgreSQL differential tests for supported values and errors.
- Push eligible containment/existence/path predicates to PostgreSQL; explain
  whether execution used source JSONB, an expression/GIN index, or a derived
  pgGraph index.
- Keep source scans, JSONB bytes, extracted keys, conversions, groups, sorts,
  DISTINCT state, output, and interrupts under the query governor.

**Exit:** label/property matches, missing/null handling, nested access,
conversion, grouping, ordering, and explain tests match the frozen contract and
remain bounded on wide documents and high-cardinality labels.

### DYN-4: PostgreSQL-First Writes And Incremental Sync

- Route dynamic property `SET`/`REMOVE` through set-based PostgreSQL JSONB DML
  with row locks/rechecks, bound values, `RETURNING`, constraints, triggers,
  ACLs, RLS, MVCC, and savepoint behavior intact.
- Route label changes through the authoritative label column and validate the
  resulting canonical label before projection membership changes.
- Capture old/new label membership and only the property/index changes needed
  for exact replay; do not place an unbounded old/new document in an in-memory
  overlay without a budget lease.
- Install exact change capture for registered standalone edge-row tables as
  well as node tables, and compile typed property-source descriptors into
  trigger expressions rather than quoting dotted paths as physical columns.
- Preserve transaction/subtransaction ordering, idempotency, durable
  watermarks, crash recovery, and source-row relationship identity.
- A property-only change that has no promoted index does not rewrite topology;
  a label/endpoint/identity change updates the corresponding derived sections.
- Batch JSONB updates by source keys where semantics allow; never issue one
  DML statement per matched row.

**Exit:** clean rebuild and incremental replay converge at the same watermark
for insert, replace, set, remove, label change, edge change, delete, rollback,
savepoint, concurrent update, crash, and restart scenarios.

### DYN-5: PostgreSQL Index Promotion And Costing

- Detect usable source GIN, expression, partial, and generated-column indexes
  without creating or mutating user schema automatically.
- Provide explain/advisory output for frequently filtered JSONB paths and label
  columns, including example generated-column/expression-index DDL that the
  operator may review and execute.
- Add an explicit promotion mapping from a JSONB path to a typed declared
  property/filter. Promotion uses PostgreSQL's generated/expression value as
  authority and invalidates on DDL drift.
- Generalize filter and source-search registration around typed physical-column
  or JSONB-path sources; never interpolate a property path as an identifier.
- Collect bounded per-label/property presence, type, null, distinct, and
  selectivity statistics. Never enumerate every arbitrary key merely to plan a
  query.
- Cost PostgreSQL JSONB pushdown, derived index lookup, and source hydration;
  retain the slower bounded fallback when no index exists.

**Exit:** checked benchmarks cover label lookup, GIN containment, promoted
scalar filters, cold/warm hydration, JSONB write amplification, sync lag, and
storage amplification with identical result semantics.

### DYN-6: Security, Operations, And PostgreSQL 19 Interop

- Apply ACL/RLS visibility before dynamic labels, property existence, key
  names, types, counts, paths, errors, statistics, or explain data become
  observable.
- Prevent tenant, spill filename, diagnostics, logs, and telemetry from
  exposing dynamic property names/values by default.
- Expose mapping fingerprint, distinct-label/key estimates, promoted indexes,
  stale reason, budget/peak/spill, sync lag, and rebuild guidance.
- Backup/restore, crash recovery, generation rollback, and cleanup treat all
  dynamic artifacts as disposable derived state.
- Differential-test the declared/static subset against PostgreSQL 19
  `GRAPH_TABLE`; mark the dynamic extension as pgGraph-only until PostgreSQL
  has a stable equivalent.
- Keep PG14-18 behavior green and add PG19 mapping/version tests without
  unsupported parser or executor hooks.

**Exit:** two-role and two-tenant tests prove no dynamic schema/value side
channel, operations are observable/recoverable, and native-versus-extension
capability documentation cannot overclaim interoperability.

## Test-First Matrix

### Pure unit/property/fuzz tests

- label canonicalization, Unicode, empty/oversized values, and collisions;
- typed mapping construction and invalid state rejection;
- JSONB top-level extraction, exact numeric conversion, missing/null states,
  objects/arrays, key conflicts, and heterogeneous property types;
- dictionary/index serialization roundtrips and checked arithmetic;
- generated label/property sequences with rebuild-versus-delta equivalence;
- fuzz malformed mapped sections, JSONB-derived values, property paths, and
  every input-dependent unsafe boundary.

### PostgreSQL-backed tests

- one dynamic node/edge table, static-plus-dynamic labels, parallel edge IDs,
  composite keys, partitions, views, generated columns, and expression/GIN
  indexes;
- multiple authorized source mappings contributing the same label without
  binder ambiguity, plus a previously unseen label arriving through sync
  without a full rebuild;
- constraints for allowed labels, required keys, JSON value types, and unique
  promoted properties;
- INSERT/UPDATE/DELETE, JSONB set/remove, triggers, savepoints, isolation,
  concurrent writers, cancellation, and statement timeout;
- ACL/RLS, security invoker/definer, search path, tenant isolation, and
  coordinate-only output;
- registration rename/drop/recreate/type drift and artifact upgrade/rebuild;
- PG14 through PG19 extension behavior plus the supported PG19 static
  differential subset.

### Resource and fault tests

- millions of distinct labels/keys, long labels/keys, huge documents, deep
  nesting, high-cardinality values, and adversarial type variation;
- forced-spill build, small-memory load/query/sync/compaction, ENOSPC, corrupt
  dictionary/index, interrupted publish, and repeated update/rebuild cycles;
- outer `LIMIT 1`, property existence, missing promoted index, wide hydration,
  group/sort/DISTINCT, and label change under hard RSS/work/disk thresholds.

## Operational Guidance To Ship With The Feature

- Prefer declared typed columns for stable, frequently queried, constrained, or
  write-heavy properties.
- Prefer JSONB dynamic properties for genuinely evolving/sparse fields; keep
  documents reasonably sized because PostgreSQL locks the containing row on
  update.
- Use PostgreSQL CHECK constraints to restrict labels, required keys, and JSON
  value types where the application has a real schema contract.
- Use GIN for broad containment/existence workloads and targeted expression or
  generated-column indexes for hot typed paths. pgGraph reports advice but does
  not silently create user indexes.
- Give every relationship a stable source key independent of endpoints so
  parallel relationships remain distinct.
- Document that pgGraph GQL supports the dynamic extension while native
  PostgreSQL 19 `GRAPH_TABLE` does not unless PostgreSQL later adds equivalent
  semantics.

## Overall Definition Of Done

- Users can add/change labels and top-level JSONB properties through
  authoritative PostgreSQL row updates without table DDL or full graph rebuild.
- Declared and dynamic mappings share one typed planner/runtime, visibility
  model, resource governor, projection lifecycle, and PostgreSQL DML adapter.
- Dynamic labels/properties preserve exact identity, null/missing/value,
  transaction, RLS, and source-of-truth semantics across build, query, writes,
  sync, persistence, compaction, crash, and restart.
- Low memory causes smaller batches/spill/slower completion or a typed error,
  never backend/container OOM.
- Hot paths can use PostgreSQL JSONB/generated/expression indexes or explicitly
  promoted pgGraph indexes with explainable cost decisions.
- PG14-19 behavior, native-PG19 limitations, examples, security guidance,
  operations, and performance tradeoffs are documented without claiming that
  schema-flexible data is truly schema-free.
