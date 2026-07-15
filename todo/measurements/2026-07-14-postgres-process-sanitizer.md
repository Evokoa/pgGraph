# PostgreSQL-Process Sanitizer Evidence — 2026-07-14

## Gate

```bash
cd graph
PG_VERSIONS=17 RUN_PGRX_SQL=0 RUN_GQL_WRITE_MATRIX=0 \
  RUN_POSTGRES_SANITIZER=1 ./tests/heavy/run_pg_matrix_docker.sh
```

## Environment

- Docker Desktop Linux arm64
- PostgreSQL 17.10
- Rust 1.96.0
- pgrx 0.19.1
- Valgrind 3.19.0 Memcheck with child-process tracing and origin tracking

## Result

PASS. The disposable PostgreSQL postmaster and its child processes exercised:

- persisted mutable-overlay build, mmap load, traversal, unload, and reload;
- deliberate artifact corruption rejection followed by valid-artifact recovery;
- build-job execution through the same job body used by the background worker;
- savepoint rollback callbacks and guarded PostgreSQL error unwinding.

Every generated Valgrind process log reported an error summary of zero after
applying the checked-in, stack-qualified suppressions for two PostgreSQL
padding reads. Neither suppression contains a `graph.so` frame. Leak checking
was disabled, so this evidence covers invalid memory access rather than
long-lived allocation retention.

The same Docker build passed 718 release Rust tests with one manual scale test
ignored before running the process gate.
