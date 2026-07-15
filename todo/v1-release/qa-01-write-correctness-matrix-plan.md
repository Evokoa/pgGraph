# QA-01 Advertised Write Correctness Matrix

This plan closes the remaining R1 evidence for PostgreSQL-first GQL writes.
The supported write profile is the public contract in the querying guide and
capability registry; this work adds correctness evidence without expanding the
1.0 syntax surface.

## Ordered Checkpoints

### QA-01A: Trigger-Rewritten Identity

- [x] Reject and roll back relationship `CREATE` when a PostgreSQL trigger
      changes a registered endpoint or dynamic relationship label.
- [x] Reject and roll back `SET`, `REMOVE`, and `MERGE ON MATCH` when a trigger
      changes the registered node primary key.
- [x] Re-check the active tenant scope against the post-trigger update row,
      including the `MERGE ON MATCH` branch.
- [x] Re-read node and relationship source rows after statement triggers so
      side-effecting `AFTER` triggers cannot bypass identity validation and
      ordinary property rewrites are returned authoritatively.
- [x] Document the public SQLSTATE and rollback contract.

### QA-01B: Source-Shape Matrix

- [x] Cover constraint rejection, rejecting and value-mutating triggers,
      partition routing, and composite registered identity for every advertised
      node and relationship write where the source shape applies.
- [x] Prove accepted trigger-mutated non-identity values are returned from the
      PostgreSQL `RETURNING` row and reflected in any local typed filter state.
- [x] Prove every rejected source statement leaves transaction-local projection
      status and same-transaction reads unchanged.

### QA-01C: Isolation And Concurrent Writers

- [x] Cover READ COMMITTED, REPEATABLE READ, and SERIALIZABLE behavior for each
      advertised write family, not only node `CREATE` observers.
- [x] Add real two-session races for same-key `MERGE`, stale `SET`/`REMOVE`,
      relationship `CREATE`/`DELETE`, and `DETACH DELETE`.
- [x] Assert source rows, returned values, transaction-local graph state, and
      client-visible SQLSTATEs after each winner, retry, or rollback path.

### QA-01D: Supported PostgreSQL Majors

- [ ] Run the completed matrix on PostgreSQL 14, 15, 16, 17, and 18.
- [ ] Archive commands, versions, pass counts, and any environment limitations
      in the release evidence log.
- [ ] Retire related known issues only after every supported-major gate passes.

## Checkpoint Gates

Each checkpoint requires formatting, Clippy, rustdoc, focused PostgreSQL tests,
the full PostgreSQL 17 pgrx suite, release-contract and documentation checks,
secret scanning, and an independent raw-diff Rust review. QA-01D additionally
requires the supported-major matrix before R1 can close.
