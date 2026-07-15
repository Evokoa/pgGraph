# KI-020 Owning Mapped Layout Plan

This checkpoint closes the remaining Rust soundness boundary around the
read-only `.pggraph` mapping. The persisted format remains little-endian and
the PostgreSQL source tables remain authoritative.

## Invariants

- Safe node, edge, and resolution access cannot outlive the mapping that owns
  the referenced bytes.
- A private `ValidatedGraphLayout` validates all section ranges, alignment,
  fixed lengths, CSR monotonicity, target bounds, primary-key offsets, UTF-8,
  bincode framing, checksums, and arithmetic before mapped stores exist.
- A private owning `MappedGraphArtifact` is the only production constructor for
  mapped node and edge views. Each installed view retains shared ownership of
  the immutable read-only mapping.
- Mapped typed loads are permitted only on little-endian targets. Big-endian
  targets fail with rebuild/compatibility guidance instead of reinterpreting
  persisted little-endian bytes.
- Out-of-range safe accessors keep returning empty/`None`/zero results without
  performing pointer arithmetic.
- The loader copies the artifact into an anonymous mapping and makes it
  read-only before creating typed views, so another process cannot invalidate
  those views by writing or truncating the source inode.
- The mapped representation does not add graph-only durable state or change
  PostgreSQL DML, ACL, RLS, MVCC, trigger, or constraint behavior.

## Ordered Checkpoints

1. Capture mapped-test and warning-free build baselines.
2. Replace the raw range tuple with `ValidatedGraphLayout`.
3. Introduce `MappedGraphArtifact` owning `Arc<Mmap>` and validated layout.
4. Convert mapped node and edge arrays from borrowed pointers to owning typed
   ranges; make mapped-store construction safe and artifact-controlled.
5. Retain shared mapping ownership for resolution lookup and engine accounting.
6. Add explicit native-endian policy and big-endian rejection tests.
7. Add drop-order/lifetime, corrupt-range, alignment, CSR, target, UTF-8,
   overflow, and out-of-range accessor regressions.
8. Run Miri-eligible in-memory layout tests, PostgreSQL-process sanitizer gate,
   supported-major matrix, full release gates, and an independent Rust review.
9. Update public known-issue, compatibility, release, and contributor safety
   documentation together with the implementation.

## Exit Evidence

- [x] Only an owning validated artifact constructs production mapped stores.
- [x] Mapped views retain ownership independently of engine field/drop order.
- [x] Big-endian native mmap loading is explicitly rejected.
- [x] Corrupt and out-of-range tests cannot reach unchecked pointer arithmetic.
- [x] Miri-eligible and Rust ASan gates pass.
- [x] PostgreSQL-process sanitizer gate passes.
- [x] PostgreSQL 14-18 mapped-load and corruption evidence passes.
- [x] PostgreSQL 17 release gates and independent Rust review report no blocker.

## Baseline — 2026-07-14

| Gate | Result |
|---|---|
| `cd graph && /usr/bin/time -p cargo test --features pg17 mmap_` | PASS: 14 passed; test execution 0.10s, wall 0.55s |
| `cd graph && cargo pgrx test --features "pg17 development" pg17` | PASS at preceding KI-023 checkpoint: 970 passed, 1 ignored |
| `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS at preceding KI-023 checkpoint |

## Checkpoint Evidence — 2026-07-14

| Gate | Result |
|---|---|
| `cargo test --features pg17 --lib --no-fail-fast` | PASS after review fixes: 718 passed, 1 ignored |
| `cargo pgrx test --features "pg17 development"` | PASS after review fixes: 975 passed, 1 ignored |
| Miri mapped edge, node layout, and node accessor gates | PASS: 4 + 3 + 1 tests |
| `RUN_ASAN=1 ... ./tests/heavy/run_memory_sanitizers.sh` | PASS: 718 passed, 1 ignored; macOS runtime setup is automatic |
| `ASAN_TEST_FILTER=mmap_` focused sanitizer gate | PASS: 14 mapped tests |
| PostgreSQL 14-18 matrix | PASS: 718 release Rust tests and 981 pgrx tests passed on each supported major; see the [matrix evidence](../measurements/2026-07-14-pg14-18-matrix.md) |
| `PG_VERSIONS=17 RUN_PGRX_SQL=0 RUN_GQL_WRITE_MATRIX=0 RUN_POSTGRES_SANITIZER=1 ./tests/heavy/run_pg_matrix_docker.sh` | PASS: disposable PostgreSQL 17.10 postmaster and child processes completed persisted mmap, corruption, callback, guarded-error, and build-job coverage with zero unsuppressed Valgrind errors; see the [process-sanitizer evidence](../measurements/2026-07-14-postgres-process-sanitizer.md) |
| Independent Rust review | PASS after fixing source-inode mutability, status accounting, stale lifecycle docs, predicate wording/coverage, and checked PK offset conversion |
