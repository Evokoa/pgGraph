# R2B Live Build Budget Evidence

Date: 2026-07-15

Scope: effective replacement budgets, live phase reservations, adaptive byte
batches, fallible major-vector growth, and persistence workspace accounting on
PostgreSQL 17.

## Results

| Gate | Result | Evidence |
|---|---|---|
| Rust library suite | Pass | `cargo test --features "pg17 development"`: 728 passed, 1 ignored, 0 failed; doctests passed. |
| PostgreSQL 17 extension suite | Pass | `cargo pgrx test --features "pg17 development"`: 994 passed, 1 ignored, 0 failed; doctests passed. |
| Static analysis | Pass | `cargo fmt --check` and Clippy for all targets with warnings denied. |
| Rust documentation | Pass | `cargo doc --features "pg17 development" --no-deps` and doctests completed without warnings or failures. |
| Public documentation and release contract | Pass | Local references, SQL API/GUC inventory, Rust source map, and the 1.0 release contract are synchronized. |
| Secret scanning | Pass | `scripts/check_secrets.sh` found no secrets in Git history or pending tracked changes. |

## Behaviors Covered

- The configured cap is reduced by current backend-private serving residency
  and a one-sixteenth safety reserve capped at 64 MiB before replacement work
  is authorized.
- Phase leases cover node and relationship construction, filters, tenants,
  resolution, both CSR directions, relationship identities, and optional
  persistence serialization.
- Node lookup, unresolved endpoint, and edge spool batches retain the public
  row ceiling while flushing earlier on a 64 KiB to 8 MiB adaptive byte target.
- A 3,000-row long-key build reports byte-pressure flushes and stays below its
  60 MiB effective replacement budget under a 64 MiB configured cap.
- A 450,000-row source with deliberately stale positive statistics is rejected
  during live construction without installing a partial engine.
- Low-memory replacement and persisted sparse typed-filter lifecycle tests pass
  with resource accounting enabled.

R2B bounds the Rust-side construction and persistence workspaces described
above. PostgreSQL executor and temporary-file memory, mapped load metadata, and
query, sync, compaction, and analytics work remain explicit R2C scope.
