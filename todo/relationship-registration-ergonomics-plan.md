# Relationship Registration Ergonomics Plan

## Objective

Decide whether pgGraph needs a many-to-many helper or richer relationship
registration API after the current `graph.add_edge()` validation and
documentation improvements have had time to prove themselves with users.

This is a planning checkpoint, not an implementation commitment. The current
API is valid and documented: edge-table registrations read both endpoint values
from the source edge table, while FK-style registrations use a registered source
node table and a target-table key. This checkpoint records the current product
decision for common junction-table schemas.

## Status

Completed on 2026-06-05. The current decision is Option A: keep
`graph.add_edge()` as the public registration API and rely on the stricter
registration-time validation plus clearer public documentation. A dedicated
junction-table helper remains deferred until repeated post-fix user reports show
that the documented edge-table registration shape is still too easy to misuse.

## Current Boundary

`graph.add_edge()` supports two registration modes:

- FK-style edges from a registered source node table.
- Edge-table style relationships where an unregistered source table contains
  both endpoint values.

Mixed-mode calls now fail during registration instead of later during
`graph.build()`. That keeps the current SQL contract explicit and prevents a
wrong catalog row from becoming the source of truth.

## Problem Statement

The confusing user shape is:

```sql
SELECT graph.add_edge(
  'public.follows'::regclass,
  'follower',
  'public.users'::regclass,
  'id',
  label := 'follows',
  bidirectional := true
);
```

For a junction table, the correct current call is:

```sql
SELECT graph.add_edge(
  'public.follows'::regclass,
  'follower',
  'public.users'::regclass,
  'followee',
  label := 'follows',
  bidirectional := true
);
```

The mistaken call reads like "connect `follows.follower` to `users.id`", but
the edge-table registration contract needs both endpoint columns on
`follows`. A helper could encode that shape more directly if the current
error and docs are still not enough.

## Design Constraints

- PostgreSQL source tables remain the system of record.
- Registration metadata should stay explicit enough that `graph.build()` can
  resolve endpoints without guessing or schema-dependent rewrites.
- The catalog should avoid ambiguous mode inference when a table is both a node
  table and an edge-like table.
- New API shape must preserve ACL preflight and existing validation guarantees.
- The implementation should stay inside the existing single crate. This does
  not justify a new crate, workspace split, async task, or service boundary.
- Traits or abstraction layers are not needed unless the registration planner
  gains multiple independently testable strategies.

## API Options

### Option A: Keep Current API and Improve Diagnostics

Keep `graph.add_edge()` as the only public registration function, but improve
the mixed-mode error to include an example corrected call when PostgreSQL
foreign-key metadata makes the intended junction shape obvious.

Use this if:

- User reports drop after the current registration-time error and docs.
- The validation path can infer a helpful hint without changing catalog
  semantics.

### Option B: Add a Junction-Table Helper

Add a narrow helper for common many-to-many tables, for example:

```sql
SELECT graph.add_junction_edge(
  'public.follows'::regclass,
  source_column := 'follower',
  target_column := 'followee',
  node_table := 'public.users'::regclass,
  label := 'follows',
  bidirectional := true
);
```

The helper should compile to the same catalog representation as edge-table
`graph.add_edge()` once validation passes.

Use this if:

- Repeated user reports show the current call shape remains confusing.
- The helper can be kept as a thin, explicit facade over existing catalog
  semantics.

### Option C: Add a Richer Relationship Registration API

Introduce a more descriptive API that separates source endpoint table,
relationship table, and target endpoint table. This is more flexible, but it
also expands the SQL contract and validation surface.

Use this only if:

- Real schemas need relationships that the edge-table helper cannot express.
- The catalog model can represent the relationship without runtime joins that
  break bounded traversal assumptions.

## Architecture Decision

Prefer Option A until there are repeated reports after the current error and
documentation improvements. If more support is needed, prefer Option B before
Option C.

The helper route preserves the existing data ownership model: the relationship
edge row still owns both endpoint values, and `graph.build()` still consumes the
same catalog shape. A richer API should be treated as a separate product
change because it may imply joins from node rows to relationship rows rather
than direct edge-table scans.

## Test Strategy

Before implementation, write failing tests for the selected option:

- Pgrx registration tests for corrected junction-table behavior.
- Pgrx weak-path tests for mixed-mode calls that should fail with a typed
  registration error and actionable hint.
- Catalog tests proving any helper produces the same catalog row shape as the
  existing edge-table registration.
- ACL tests if a new helper touches different table metadata than
  `graph.add_edge()`.
- Documentation examples that run through the existing SQL test path if the
  docs add executable snippets.

No fuzzing is needed for a narrow SQL helper unless a new parser or free-form
relationship specification language is introduced.

## Implementation Boundaries

Likely modules if this becomes implementation work:

- `graph/src/sql_facade/admin.rs` for the SQL-facing registration entry point.
- `graph/src/catalog/validate.rs` for mode-specific validation and hint text.
- `graph/src/catalog.rs` for catalog row construction if helper-to-catalog
  translation needs a shared function.
- `graph/src/pg_tests/maintenance_admin.rs` or the closest existing
  registration pgrx test module for SQL boundary coverage.
- `docs/user_guide/schema-registration.mdx` and
  `docs/user_guide/api-reference.mdx` for public examples.

Keep the implementation additive. Do not change existing `graph.add_edge()`
semantics unless a failing regression proves the current documented behavior is
wrong.

## Resolved Questions

Resolved for the current checkpoint:

- Keep the helper threshold evidence-based: repeated post-fix user reports are
  required before adding another SQL registration function.
- If a helper becomes necessary, decide same-node-table versus distinct
  endpoint-table support from fresh user examples instead of guessing now.
- If a helper becomes necessary, prefer explicit column names and use
  PostgreSQL foreign-key metadata only for diagnostics or validation hints.
- Bidirectional self-junction ambiguity continues to be enforced by the
  existing edge-table path unless a future helper adds stricter pre-validation.

## Exit Criteria

This plan is complete when one of these decisions is recorded:

- Keep current API, with no further action beyond docs/error hints. **Selected
  on 2026-06-05.**
- Add a junction-table helper that compiles to existing edge-table catalog
  semantics.
- Defer to a richer relationship registration redesign with a separate plan.

Record verification in `todo/measurements.md` if implementation work follows.
