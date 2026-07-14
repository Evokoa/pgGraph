# KI-023 Security-Definer Search-Path Plan

This checkpoint closes the remaining search-path exposure in trigger-sync
functions generated for registered PostgreSQL source tables.

## Invariants

- Every pgGraph `SECURITY DEFINER` function executes with `pg_catalog` before
  the explicitly listed `pg_temp` schema, preventing implicit temporary-schema
  precedence for relation and type lookup.
- Dynamically generated row and truncate trigger functions qualify pgGraph
  relations and cannot resolve attacker-controlled functions from the caller's
  search path.
- Metadata audits cover both extension-defined entrypoints and generated
  trigger functions.
- Trigger synchronization behavior and SQL contracts remain unchanged.

## Ordered Checkpoints

1. Pin generated row and truncate trigger functions to `pg_catalog, pg_temp`.
2. Add a unit assertion over generated SQL.
3. Add a PostgreSQL-backed shadow-schema regression that proves source DML
   cannot invoke an attacker-controlled advisory-lock function.
4. Harden caller-role and graph-admin checks against temporary relations and
   persistent function/operator shadows.
5. Extend the function metadata release gate to create and audit generated
   trigger functions.
6. Update public security, release, known-issue, and contributor documentation.

## Exit Evidence

- [x] Generated trigger functions expose `search_path=pg_catalog, pg_temp` in
  `pg_proc.proconfig`.
- [x] A caller-controlled shadow schema cannot intercept trigger execution.
- [x] Temporary catalog relations and persistent function/operator shadows
  cannot bypass graph-admin authorization.
- [x] The metadata audit and focused PostgreSQL regression pass.
- [x] Independent Rust review reports no KI-023 blocker.
