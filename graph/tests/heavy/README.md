# Heavy SQL Tests

Sticky note for contributors and agents: the maintained inventory and layer
selection guide lives in [SQL Tests](../../../docs/contributor_guide/sql-tests.mdx).

Use these scripts when `cargo pgrx test` is not representative enough: client
SQLSTATEs, ACL/RLS role boundaries, crash recovery, backup/restore, package
validation, Docker, pg_upgrade, memory evidence, playground query stability, or
mixed concurrency.

`gql_isolation_matrix.sh` is the two-session backend-local mapped-write
visibility gate. It applies node and relationship `CREATE`, `SET`, `REMOVE`,
relationship `DELETE`, `DETACH DELETE`, and `MERGE` under READ COMMITTED,
REPEATABLE READ, and SERIALIZABLE. Source rows and graph results must agree
inside the writer, inside the concurrent reader snapshot, and after commit.
`gql_merge_race.sh` and `gql_relationship_race.sh` use lock-visible handshakes
for deterministic same-key outcomes. `gql_write_recheck_race.sh` covers stale
predicate and tenant outcomes with exact SQLSTATE and graph-delta assertions.
With `PERSIST_ON_BUILD=on`, the isolation script retains its durable new-node
identity and repeated-build profile. Durable incremental node deletion in a
graph with standalone relationship mappings remains fail-closed with rebuild
guidance, so that mode does not claim the full write-family matrix.

Most scripts expect a disposable database and accept variables such as
`PG_VERSION_FEATURE=pg17`, `PG_CONFIG`, and `DBNAME`. Scripts that kill or
upgrade PostgreSQL require disposable `PGDATA` directories.

For build memory evidence, start with `measure_build_rss.sh`. For opt-in stress
profiles that cover baseline build, small SPI/spool batches, repeated persisted
rebuilds, and low-memory rebuilds, use `build_memory_stress.sh` or set
`RUN_BUILD_MEMORY_STRESS=1` on `run_release_gate.sh`.
