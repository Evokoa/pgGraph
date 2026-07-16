# R3A Coherent Source Boundary — 2026-07-16

## Result

The R3A source boundary passes its Rust and production PostgreSQL 17 checkpoint.
Graph build and vacuum now establish one transaction-scoped boundary across the
mapping catalog, registered partition roots and descendants, the sync-writer
barrier, catalog fingerprint, source schema, caller privileges, and sync-log
watermark.

## Evidence

- `cargo test --features pg17 --no-default-features build_snapshot::tests`:
  2 passed.
- `cargo pgrx test pg17 --features "pg17 development"
  projection_mode_build_and_status_contract`: 1 passed.
- `cargo clippy --features pg17 --no-default-features -- -D warnings`: passed.
- `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_r3a_build_lock6
  ./tests/heavy/build_lock_regression.sh`: passed.

The production gate proves stable `55P03`/`PG006` rejection for caller-owned
mapping-catalog writes, caller-owned registered-source writes, and a direct
write-capable lock on a partition leaf. It also repeats the existing competing
publisher, active-writer, commit/rollback horizon, durable watermark, and
publication-preservation regressions. The gate's former slow-build catalog/view
hack was replaced with a valid 200,000-row registered table, and advisory-lock
inspection is now scoped to the test database.
