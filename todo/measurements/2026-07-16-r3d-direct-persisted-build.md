# R3D Direct Persisted Build — 2026-07-16

## Result

Persisted build and vacuum now scan the fenced PostgreSQL source snapshot into
governed external-sort runs and stream a generation-specific artifact v6
candidate without constructing a complete owned engine. Candidate files use
exclusive creation, exact disk reservation, bounded zero-fill preallocation,
positioned section writes, fixed-buffer checksum finalization, production load
validation, a final source-boundary recheck, and generation compare-and-swap.

The previous generation remains current on scan, run, quota, write, load,
source-boundary, or publication failure. Nonpersisted builds retain the owned
builder as the backend-local implementation and differential oracle.

## Focused Evidence

| Gate | Exact command | Result |
|---|---|---|
| Formatting and strict Clippy | `cd graph && cargo fmt --check && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Direct build and persistence | `cd graph && cargo test --features "pg17 development" persisted_` | PASS: 25 tests |
| Governed runs | `cd graph && cargo test --features "pg17 development" build_runs` | PASS: 11 tests |
| Persistence validation | `cd graph && cargo test --features "pg17 development" persistence::tests` | PASS: 67 tests after candidate fault tests were added |
| Candidate ownership/faults | `cd graph && cargo test --features "pg17 development" direct_candidate` | PASS: quota-before-create, private-failure cleanup, and create-new preservation |
| PostgreSQL differential | `cd graph && cargo pgrx test pg17 --features "pg17 development" persisted_direct_build_matches_owned_build_for_public_queries` | PASS: owned and persisted traversal, indexed filtering, and GQL outputs match |

## Release Evidence

| Gate | Exact command | Result |
|---|---|---|
| Full release gate | `cd graph && RUN_RSS=1 RUN_BUILD_MEMORY_STRESS=1 ./tests/heavy/run_release_gate.sh` | PASS: secret, formatting, strict Clippy, rustdoc, contract, 856 Rust tests (1 ignored), 1,128 pgrx tests (1 ignored), fuzz seed, package/install/upgrade, security, backup/restore, concurrency, transaction, recovery, and memory profiles |
| Synthetic scale | full release gate | PASS: 50,000-node build in 4,998 ms and representative query in 82 ms |
| Build residency | full release gate | PASS: 200,000 nodes and 200,000 relationships at 210 MiB peak RSS |
| Runtime residency | full release gate | PASS: 92 MiB peak RSS; production-shaped memory profiles remained between 192 and 268 MiB |
| Playground differential | full release gate | PASS: all 40 CSR queries; capped traversal stayed byte-for-byte stable after physical source reordering |
| Supported PostgreSQL matrix | `cd graph && RUN_DURABLE_PROJECTION_MATRIX=1 RUN_POSTGRES_SANITIZER=0 ./tests/heavy/run_pg_matrix_docker.sh` | PASS on PostgreSQL 14, 15, 16, 17, and 18: 860 Rust tests (1 ignored) and 1,132 serialized pgrx tests (1 ignored) per major, the GQL write/isolation profiles, and cross-backend lifecycle, publication-lock, and concurrent relation-identity durable profiles |
| Independent R3 review | raw R3 diff reviewed after the release gate | PASS after generation-orphan reservation and typed candidate-error preservation fixes |

The matrix test harness runs pgrx cases serially because those integration tests
intentionally share PostgreSQL maintenance state. Its container also includes
the public artifact inspector used by the publication-lock regression. These
are harness contracts, not retries: any failing test or durable profile still
fails the matrix immediately.

The full Rust, PostgreSQL, documentation, security, packaging, production
resource, and supported-major gates are green for the R3 checkpoint.
