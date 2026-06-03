# Edge Registration Validation TODO

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

Added pgrx coverage proving the current behavior:

- Valid composite-PK junction edge-table registration builds and traverses when
  `to_column` is the second junction foreign-key column.
- The mixed-mode registration is currently accepted by `graph.add_edge()`, but
  `graph.build()` later tries to read the missing edge-table column and fails
  with `column "id" does not exist`.

Verification recorded during the investigation:

- `cargo fmt --check` from `graph/`: passed.
- `git diff --check` from repository root: passed.
- `cargo pgrx test pg17 junction` from `graph/`: passed.

## Required Fixes

1. Tighten `graph.add_edge()` validation so edge-table registrations require
   `to_column` to exist on the source edge table.

   The current validation accepts `to_column` if it exists on either the target
   table or the source table. That is too permissive when `from_table` is not a
   registered node table, because the builder will read both endpoint values
   from `from_table`.

2. Preserve FK-style registration for registered source node tables.

   When `from_table` is already registered with `graph.add_table()`,
   `from_column` is a target primary-key value on the source row and
   `to_column` names the target table key. Existing FK-style tests must keep
   passing.

3. Return a clear registration-time error for mixed-mode registrations.

   The error should name the source table and explain that edge-table
   registrations need `to_column` on the source edge table. This should replace
   the current delayed `graph.build()` failure.

4. Clarify the API reference.

   `docs/user_guide/schema-registration.mdx` already explains the two modes,
   but `docs/user_guide/api-reference.mdx` should explicitly state how
   `from_column` and `to_column` are interpreted in FK-style versus edge-table
   style registration.

5. Decide whether to add a more ergonomic many-to-many helper later.

   The inverse registration shape `users.id -> follows.follower` implies a join
   expansion from one user row to many junction rows. `graph.add_edge()` does
   not currently model that. If this becomes a common source of confusion,
   consider a separate helper or richer relationship registration API instead
   of overloading `add_edge()` further.
