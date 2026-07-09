# PostgreSQL 19 Native Property Graph Support

## Outcome

At completion, pgGraph supports PostgreSQL 19 native property graph definitions
as a first-class catalog frontend. A user can define vertices, relationships,
keys,
labels, and properties with `CREATE PROPERTY GRAPH`, then build and query a
pgGraph projection without recreating the same mapping through
`graph.add_table()` and `graph.add_edge()`.

PostgreSQL remains authoritative:

- the native property graph definition owns the logical graph mapping;
- base tables, views, keys, permissions, RLS, MVCC, and properties remain
  PostgreSQL state;
- pgGraph stores only derived projection metadata and artifacts;
- `GRAPH_TABLE` remains PostgreSQL SQL/PGQ behavior unless a stable supported
  planner integration can safely select pgGraph execution;
- full GQL and SQL/PGQ maintain separate conformance ledgers over shared IR.

Official PostgreSQL 19 references:

- [Property graphs](https://www.postgresql.org/docs/19/ddl-property-graphs.html)
- [CREATE PROPERTY GRAPH](https://www.postgresql.org/docs/19/sql-create-property-graph.html)
- [GRAPH_TABLE queries](https://www.postgresql.org/docs/19/queries-graph.html)

## PG19-0: Toolchain And Contract Baseline

- Track the pgrx release that supports PostgreSQL 19; add `pg19` only when the
  extension builds and tests against the supported pgrx API.
- Add PostgreSQL 19 to the supported-version policy, CI matrix, install docs,
  packaging, fresh-install smoke test, and upgrade test.
- Inventory stable PostgreSQL catalog and extension APIs for property graphs,
  DDL invalidation, query planning, and object identity.
- Record which integration points are public/stable and which would require
  unsupported PostgreSQL internals.
- Add a SQL/PGQ conformance registry distinct from the GQL registry.

**Exit:** a minimal extension builds, installs, and runs pgrx tests on PG19; the
catalog and planner contract is documented with no dependency on guessed or
unstable APIs.

## PG19-1: Native Catalog Adapter

Define a narrow core-owned catalog port representing:

- property graph identity and schema;
- vertex/relationship element-table aliases;
- vertex and relationship key columns, including composite keys;
- relationship source/destination key mappings;
- multiple labels and shared-label compatibility;
- property names, types, column/expression mappings, and visibility;
- temporary versus persistent graph lifetime;
- catalog fingerprint and dependency OIDs.

Implement a version-gated PostgreSQL 19 adapter that reads native definitions
into that model. PostgreSQL 14-18 continue using the existing registered graph
catalog adapter.

Do not duplicate the native definition as a second editable mapping. pgGraph
may cache a fingerprinted normalized mapping, but refreshes or invalidates it
from the native object.

**Exit:** catalog fixtures cover schema-qualified and temporary graphs,
composite keys, explicit source/destination keys, aliases, multiple labels,
property lists, property expressions, views, and clearly rejected unsupported
objects.

## PG19-2: Attach, Build, And Invalidation

- Add an explicit attach/select operation for a native property graph, or
  automatic attachment if the stable catalog can make that deterministic.
- Store only the native object identity, normalized fingerprint, projection
  policy, artifact generation, and operational state in pgGraph catalogs.
- Build from the native mapping with the same bounded-memory and snapshot/
  watermark architecture as manually registered graphs.
- Translate native vertex and relationship keys into stable pgGraph node and
  `RelationshipId` values without endpoint/type-only identity.
- Detect CREATE/ALTER/DROP PROPERTY GRAPH and dependent table/view/key/property
  changes. Mark or remove projections transactionally and fail closed on stale
  mappings.
- Define behavior for temporary native graphs: backend/session-scoped derived
  artifacts only, with no durable orphan.

**Exit:** a user can create a supported native property graph, attach/build it
without duplicate manual registration, alter/drop it safely, and receive a
typed mapping error for unsupported definitions.

## PG19-3: Query Interoperation

- Lower native catalog mappings and the pgGraph GQL frontend into the shared
  typed binding-table IR.
- Keep PostgreSQL's native `GRAPH_TABLE` path available and correct.
- Establish the supported boundary explicitly:
  - catalog interoperation is required;
  - differential result testing against `GRAPH_TABLE` is required;
  - transparent `GRAPH_TABLE` acceleration is optional until PostgreSQL
    exposes a stable supported planner/executor hook;
  - unsupported hook experiments never ship in the stable extension.
- Rebase `query/sqlpgq_adapter.rs` on canonical IR instead of translating into
  the subset GQL AST.
- Add SQL/PGQ explain/conformance evidence separate from full GQL evidence.

**Exit:** identical supported patterns over the same native property graph
produce equivalent typed results through native `GRAPH_TABLE` and pgGraph;
the execution/acceleration boundary is documented without overclaiming.

## PG19-4: Security And PostgreSQL Semantics

- Check privileges on the native property graph and every underlying relation
  as the executing role, matching PostgreSQL semantics.
- Apply RLS before coordinates, topology, counts, paths, or hydrated values are
  observable, including coordinate-only pgGraph output.
- Use one documented MVCC snapshot/freshness contract for topology and source
  properties.
- Cover SECURITY INVOKER/DEFINER boundaries, search path, temporary objects,
  schemas, partitions, views, foreign tables, and unsupported cases.
- Preserve source constraints, triggers, locks, and DML authority for all GQL
  writes against a native mapping.

**Exit:** two-role tests show native `GRAPH_TABLE` and pgGraph expose the same
authorized graph for supported shapes, and unsupported security semantics fail
closed with stable SQLSTATEs.

## PG19-5: Compatibility And Release

- Run PG14-18 regression gates unchanged.
- Run PG19 install, upgrade, catalog, GQL, SQL/PGQ differential, RLS, DDL
  invalidation, build/load, sync, crash, memory, and package gates.
- Document supported native definition shapes and typed rejection guidance.
- Add examples using only `CREATE PROPERTY GRAPH` plus pgGraph attach/build/
  query operations.
- Publish the SQL/PGQ compatibility matrix and benchmark native versus pgGraph
  execution with identical semantics and source data.

**Exit:** the PostgreSQL 19 and PostgreSQL SQL/PGQ Roadmap rows graduate to the
Current Baseline, KI-007 is removed from active Known Issues with release-note
evidence, and older PostgreSQL versions remain green.

## Overall Definition Of Done

- PostgreSQL 19 is supported by the extension toolchain and release matrix.
- Native property graph definitions are consumable without duplicate manual
  mappings.
- Stable vertex, relationship, label, property, and key semantics survive
  build, load, sync, compaction, and GQL execution.
- DDL invalidation and unsupported mappings fail closed.
- ACL, RLS, MVCC, temporary-object, and transaction behavior is tested.
- Native `GRAPH_TABLE` and pgGraph supported-result differentials are green.
- Transparent native-query acceleration is claimed only if a stable supported
  PostgreSQL hook exists and its tests are green.
- PG14-18 compatibility is unchanged.
- Public Roadmap, Known Issues, API docs, examples, and release notes are
  updated from evidence rather than intent.
