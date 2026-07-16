# R5 Supported Profile Evidence — 2026-07-16

R5 freezes the 134-function public SQL surface and the bounded GQL read/write
surface in machine-readable registries. Every SQL function belongs to exactly
one assurance group. Compatibility facades and deliberate rejection endpoints
have explicit dispositions. The GQL registry verifies parser, binder, executor,
ACL/RLS, transaction, resource, operator-bound, source-of-truth, write-recheck,
positive, negative, and documentation evidence as applicable.

## Verification

| Gate | Result |
|---|---|
| `cargo fmt --check` | Pass |
| `cargo test --features "pg17 development" query::` | Pass: 178 tests |
| `cargo clippy --all-targets --features "pg17 development" -- -D warnings` | Pass |
| `cargo pgrx test pg17 --features "pg17 development" filter_constructor_surface_has_stable_shapes_and_aliases` | Pass: 1 focused test |
| `cargo pgrx test pg17 --features "pg17 development" gql` | Pass: 118 tests |
| `scripts/check_docs_drift.sh` | Pass |
| `scripts/check_doc_references.py` | Pass |
| `scripts/check_rust_doc_map_drift.py` | Pass |
| `scripts/check_release_contract.py` | Pass after reviewed contract regeneration |
| `git diff --check` | Pass |

The unsupported corpus covers syntax that could otherwise imply broader ISO
GQL, openCypher, or SQL/PGQ compatibility. Each case asserts a stable SQLSTATE
and actionable message fragment before execution.
