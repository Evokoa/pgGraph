# R2A Checked Resource Policy Evidence

Date: 2026-07-15

Scope: typed checked byte accounting, conservative build estimation,
statement-local reservation accounting, and fail-closed over-budget build
policy on PostgreSQL 17.

## Results

| Gate | Result | Evidence |
|---|---|---|
| Rust library suite | Pass | `cargo test --features "pg17 development" --lib --no-fail-fast`: 723 passed, 1 ignored, 0 failed in 6.52 seconds. |
| PostgreSQL 17 extension suite | Pass | `cargo pgrx test --features "pg17 development" pg17`: 987 passed, 1 ignored, 0 failed in 15.02 seconds of reported test time. |
| Static analysis | Pass | `cargo fmt --check` and Clippy for all targets with warnings denied. |
| Rust documentation | Pass | Generated documentation with `RUSTDOCFLAGS="-D warnings"`; doctests passed. |
| Public documentation and release contract | Pass | Local references, SQL API/GUC inventory, source map, and the 1.0 release contract are synchronized. |

## Behaviors Covered

- Checked byte arithmetic rejects negative estimates and integer overflow.
- Unknown or zero PostgreSQL row statistics use an exact source count before
  authorizing construction.
- Reservations conserve current memory, retain the existing lease after failed
  growth, and report the phase responsible for peak accounting.
- `error`, `readonly`, and `read_only` compatibility spellings all reject an
  over-budget build before allocation and preserve the previous engine.
- A replacement that fits only after unloading the backend-private serving
  graph retains the documented low-memory rebuild path.

R2A establishes the shared accounting boundary. R2B is responsible for holding
live leases during allocation and replacing fixed build batches with adaptive
byte-bounded batches; this evidence does not claim those later guarantees.
