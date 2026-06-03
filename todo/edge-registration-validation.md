# Edge Registration Validation

## Context

A user reported that `graph.build()` fails with `column "id" does not exist`
for a many-to-many junction table:

```sql
SELECT graph.add_edge(
  'public.follows'::regclass, 'follower',
  'public.users'::regclass, 'id',
  label := 'follows',
  bidirectional := true
);
```

The immediate failure is a mixed-mode edge registration. For edge-table style
relationships, both endpoint values are read from `from_table`, so the correct
registration is:

```sql
SELECT graph.add_edge(
  'public.follows'::regclass, 'follower',
  'public.users'::regclass, 'followee',
  label := 'follows',
  bidirectional := true
);
```

`to_table` identifies the registered node table used for endpoint resolution;
`to_column` identifies the source edge-table column that contains the target
node primary-key value.

## Regression Coverage

Added pgrx coverage for the corrected behavior:

- Valid composite-PK junction edge-table registration builds and traverses when
  `to_column` is the second junction foreign-key column.
- Mixed-mode registration now fails during `graph.add_edge()` with a
  registration-mode error instead of later failing during `graph.build()`.

Verification recorded during the investigation:

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo pgrx test pg17 junction` from `graph/`: passed.

## Status

Completed on 2026-06-03. `graph.add_edge()` now validates endpoint columns
according to the actual registration mode before writing the catalog row:
registered source node tables use FK-style validation, while unregistered
source tables use edge-table validation.

Verification is recorded in `todo/measurements.md` under "Edge Registration
Validation Slice".

## Completed Fixes

1. Tightened `graph.add_edge()` validation so edge-table registrations require
   `to_column` to exist on the source edge table.

   The previous validation accepted `to_column` if it existed on either the
   target table or the source table. That was too permissive when `from_table`
   was not a registered node table, because the builder reads both endpoint
   values from `from_table`.

2. Preserved FK-style registration for registered source node tables.

   When `from_table` is already registered with `graph.add_table()`,
   `from_column` is a target primary-key value on the source row and
   `to_column` names the target table key. Existing FK-style tests pass with
   this behavior preserved.

3. Returned a clear registration-time error for mixed-mode registrations.

   The error should name the source table and explain that edge-table
   registrations need `to_column` on the source edge table. This should replace
   the current delayed `graph.build()` failure.

4. Clarified the API reference.

   `docs/user_guide/schema-registration.mdx` already explains the two modes,
   but `docs/user_guide/api-reference.mdx` should explicitly state how
   `from_column` and `to_column` are interpreted in FK-style versus edge-table
   style registration.

## Helper Decision

No new many-to-many helper is planned as part of this checkpoint.

The inverse registration shape `users.id -> follows.follower` implies a join
expansion from one user row to many junction rows. `graph.add_edge()` does not
currently model that. A separate helper or richer relationship registration API
would be a separate product change if repeated user reports show this remains
confusing after the registration-time error and API reference clarification.
