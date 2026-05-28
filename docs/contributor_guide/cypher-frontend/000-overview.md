# Cypher frontend for pgGraph — overview

> **Status:** Plan / spec. No code on this branch yet.
> **Branch:** `feat/cypher-frontend`
> **Upstream dependency:** [cyrs](https://github.com/) — Rust openCypher v9 /
> GQL frontend. Frontend-only (parser, HIR, plan IR, sema, diagnostics).
> No executor; pgGraph supplies the executor.
> **Companion document:** `../../../../cyrs/feat-request.md` — the asks we
> are making upstream of cyrs to make this fit.

## What this is

pgGraph today exposes graph queries as PostgreSQL SQL functions:

```sql
SELECT * FROM graph.traverse(
    'public.people'::regclass, 'alice',
    /*max_depth=*/ 2, /*edge_types=*/ ARRAY['KNOWS'],
    /*direction=*/ 'outgoing', ...
);
```

This is the right interface for *the engine*, but it's a poor surface
for *users asking graph questions*. Multi-hop pattern queries with
predicates are exactly what Cypher is good at expressing.

We will add a new SQL function `graph.cypher(text, jsonb)` that accepts
an openCypher v9 query string, parses it through the
[cyrs](https://github.com/) frontend, and dispatches each plan operator
to either pgGraph's existing in-memory engine (reads) or to SPI-issued
DML against the registered source tables (writes).

## What this is NOT

- It is **not** a replacement query language. SQL stays. The
  `graph.cypher(...)` function is *additive*.
- It is **not** a "graph database mode" where the extension owns graph
  storage. Your tables remain the source of truth (per pgGraph's
  founding pitch).
- It is **not** a cost-based optimiser. cyrs produces logical plans;
  pgGraph executes them. Anything resembling a planner is in cyrs or
  in Postgres, never here.
- It is **not** scope creep for the extension. The extension already
  uses SPI to write to user tables (catalog, sync, build). Cypher
  writes use the same machinery against user-registered "label tables".

## Why cyrs

- Layered: we consume **HIR + Plan** (per `cyrs/docs/integration-depth.md`
  decision table — "graph database → HIR + Plan"). Cheaper than building
  our own parser; richer than consuming the agent JSON.
- Has dialect modes; the `OpenCypherV9` mode is exactly the surface
  we want to expose.
- Diagnostics are first-class (`cyrs-diag`, codes `E0xxx` through
  `E5xxx`), span-accurate, and can be projected through Postgres'
  `ereport(ERROR, ...)`.
- The `WriteOp` set is complete for v9 (`CreateNode`, `CreateRel`,
  `MergeNode`, `MergeRel`, `SetProperty`, `SetLabels`,
  `RemoveProperty`, `RemoveLabels`, `Delete{detach}`). We do not need
  to build a write-side IR ourselves.
- Frontend-only by design: no executor to fight with.

## High-level architecture

```
┌──────────────────────────────────────────────────────────────────┐
│ Postgres backend (one transaction)                               │
│                                                                  │
│   SELECT * FROM graph.cypher('MATCH ... RETURN ...', '{}'::jsonb)│
│                              │                                    │
│   ┌──────────────────────────▼─────────────────────────────┐     │
│   │  graph crate — new module: cypher_facade               │     │
│   │                                                         │     │
│   │   1. parse + HIR-lower    (cyrs_hir)                   │     │
│   │   2. sema (schema-aware)  (cyrs_sema + our             │     │
│   │      SchemaProvider impl backed by pgGraph catalog)    │     │
│   │   3. plan-lower           (cyrs_plan)                  │     │
│   │   4. dispatch:                                          │     │
│   │       - ReadOp tree  → engine + row evaluator          │     │
│   │       - WriteOp list → SPI DML on source tables        │     │
│   │   5. materialise rows as JSONB → TableIterator         │     │
│   └─────────────────────────┬───────────────────────────────┘     │
│                              │                                    │
│   ┌──────────────────────────▼─────────────────────────────┐     │
│   │ Existing pgGraph machinery (unchanged):                │     │
│   │   • engine.rs / bfs.rs / path_finder.rs (reads)        │     │
│   │   • Spi::run_with_args(INSERT/UPDATE/DELETE) (writes)  │     │
│   │   • sync triggers pick up writes for index refresh     │     │
│   └────────────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────────────┘
```

## Documents in this directory

- **000-overview.md** — this file.
- **010-architecture.md** — module layout, type contracts, where in
  `graph/src/` each piece lands.
- **020-catalog-extensions.md** — new catalog tables, `SchemaProvider`
  implementation, label↔table mapping, unique-constraint registration.
- **030-read-mapping.md** — `cyrs_plan::ReadOp` → pgGraph engine
  call / SQL emission, operator by operator.
- **040-write-mapping.md** — `cyrs_plan::WriteOp` → SPI DML, operator
  by operator, plus MERGE / DETACH DELETE semantics.
- **050-expr-and-types.md** — `cyrs_plan::Expr` evaluation
  (push-to-SQL vs Rust-side), Cypher↔Postgres type bridge, null/3VL
  alignment.
- **060-diagnostics-and-errors.md** — how cyrs diagnostics surface as
  Postgres `ereport`, embedder-host diagnostic range, error UX.
- **070-milestones-and-tests.md** — milestone plan, openCypher TCK
  subset wiring, integration test fixtures.
- **080-open-questions.md** — known unknowns to resolve before / during
  implementation. Issues blocked on upstream cyrs work cite
  `feat-request.md` sections.

## Reading order

If you're new: 000 → 010 → 070 → 080. (Architecture, then milestones
to know what we're cutting, then open questions to know what's not
settled.)

If you're implementing: 020 (you need the catalog before anything
else) → 030 / 040 / 050 in parallel → 060 → 070.
