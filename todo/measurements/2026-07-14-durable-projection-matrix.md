# PostgreSQL 14-18 Durable Projection Matrix — 2026-07-14

## Gate

```bash
cd graph
RUN_RUST_TESTS=0 PG_VERSIONS='14 15 16 17 18' RUN_PGRX_SQL=0 \
  RUN_GQL_WRITE_MATRIX=0 RUN_POSTGRES_SANITIZER=0 \
  RUN_DURABLE_PROJECTION_MATRIX=1 ./tests/heavy/run_pg_matrix_docker.sh
```

The release Rust and pgrx suites for the same five majors had already passed in
the [supported-major release matrix](./2026-07-14-pg14-18-matrix.md), so this
run selected only the additional durable lifecycle, lock, and concurrent-DDL
profiles.

## Environment

- Docker Desktop Linux arm64
- PostgreSQL 14.23, 15.18, 16.14, 17.10, and 18.4
- Rust 1.96.0
- pgrx 0.19.1

## Results

| PostgreSQL | Cross-backend durable lifecycle | Publication and writer locks | Concurrent relation DDL |
|---|---:|---:|---:|
| 14.23 | PASS | PASS | PASS |
| 15.18 | PASS | PASS | PASS |
| 16.14 | PASS | PASS | PASS |
| 17.10 | PASS | PASS | PASS |
| 18.4 | PASS | PASS | PASS |

The lock profile covers graph-scoped publication exclusion, competing
publishers, exact watermark completion, legacy trigger rejection and refresh,
active writer commit/rollback horizons, same-transaction ingestion rejection,
artifact preservation after failed publication, and build/vacuum exclusion.

The concurrent-DDL profile holds PostgreSQL `AccessExclusiveLock` during a
rename and a drop/recreate transaction. A concurrent build must stop at the
relation lock with SQLSTATE `55P03`. After commit, the renamed table rebuilds
through the original registered OID, while the replacement table has a new OID
and the old registration fails closed with explicit guidance.

During harness validation, the existing lock gate's pre-named-graph artifact
path assumption was corrected to resolve the selected graph UUID. The focused
PostgreSQL 17 profile passed after that correction before the full matrix ran.
The focused concurrent-DDL profile also passed on PostgreSQL 17 before it was
added to the five-major matrix. The final matrix completed in 202 seconds.

An independent raw-diff review identified an unbounded failure-cleanup wait in
the first harness revision. The final harness tracks the active coordination
key, terminates its PostgreSQL backend, and applies bounded TERM/KILL handling
to client processes before waiting. Shell syntax, ShellCheck, and the focused
PostgreSQL 17 profile passed after the fix; independent follow-up review passed
without remaining findings.
