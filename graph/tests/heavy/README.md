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

`run_gql_write_matrix.sh` runs the full and persisted isolation profiles plus
the deterministic MERGE, relationship, and stale-write race gates against each
requested supported PostgreSQL major. It creates one temporary PostgreSQL
cluster per major and removes it on exit. Set `PG_VERSIONS` to a space-separated
subset or provide `PG_CONFIG_<major>` for a nonstandard installation path.

Most scripts expect a disposable database and accept variables such as
`PG_VERSION_FEATURE=pg17`, `PG_CONFIG`, and `DBNAME`. Crash scripts run through
`scripts/with_disposable_postgres.sh`; PostgreSQL binary upgrades run through
`run_pg_upgrade_matrix.sh`, whose lower-level validator requires a per-run
safety sentinel.

`run_postgres_process_sanitizer.sh` creates its own disposable cluster and
runs the postmaster plus child processes under Valgrind. It covers persisted
mmap build/load/traversal/reload, corruption rejection, build-job execution,
transaction callbacks, and guarded error unwinding. The gate fails if any
process log reports an unsuppressed error. Use `RUN_VALGRIND=1` with
`run_memory_sanitizers.sh`, or set `RUN_POSTGRES_SANITIZER=1` on
`run_pg_matrix_docker.sh` for the containerized release environment. Direct
execution installs pgGraph into the PostgreSQL installation selected by
`PG_CONFIG`; prefer the containerized command when that installation must not
be changed.

`run_durable_projection_matrix.sh` creates one disposable cluster per requested
PostgreSQL major and runs the cross-backend durable lifecycle plus the complete
publication/writer-lock regression. Set `RUN_DURABLE_PROJECTION_MATRIX=1` on
`run_pg_matrix_docker.sh` to run it in the maintained PostgreSQL 14-18 image.
Each child profile is bounded by `PROFILE_TIMEOUT_SECONDS` (600 seconds by
default), and cluster shutdown is verified before its temporary files are
removed.

For build memory evidence, start with `measure_build_rss.sh`. For opt-in stress
profiles that cover baseline build, small SPI/spool batches, repeated persisted
rebuilds, and low-memory rebuilds, use `build_memory_stress.sh` or set
`RUN_BUILD_MEMORY_STRESS=1` on `run_release_gate.sh`.
Use `measure_runtime_resources.sh` for repeated load plus representative query,
hydration, sync-ingest, compaction, and analytics resource evidence. It samples
backend RSS everywhere and PSS when Linux exposes `smaps_rollup`, verifies the
stable `PG007` breaker, and enforces positive `MAX_RSS_MB`/`MAX_PSS_MB`
thresholds (1024 MiB by default). Missing PID, RSS, or Linux PSS samples fail
the profile. Successful representative queries use an explicit
`QUERY_MEMORY_MB` budget (256 MiB by default), while the separate work-limit
case must still return `PG007`. The default evidence shape is 10,000 nodes,
9,999 edges, and two 500-row sync cycles; telemetry assertions verify the
reported operation, peak phase, work, row, and disk counters. The full release
gate runs this profile by default; set
`RUN_RUNTIME_RESOURCES=0` only when intentionally running a narrower gate.
