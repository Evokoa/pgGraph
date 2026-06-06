# Mutable Projection Progress

## 2026-06-07

Completed planning documentation, and recorded initial baseline for regression
testing.

Microphase 0 started and completed the test-harness checkpoint:

- Added test-only projection fixture helpers for temporary artifact
  directories, manifest and segment paths, synthetic sync rows, normalized
  mutations, CSR construction, and full-neighbor equivalence checks.
- Added the six ignored contract tests named by `build_order.md`; running them
  with `--ignored` fails because the targeted production modules do not exist
  yet.
- Tests run:
  - `cd graph && cargo fmt --check` before edits: passed.
  - `cd graph && cargo test --features pg17 projection::` before edits:
    passed.
  - `cd graph && cargo test --features pg17 projection::` after edits:
    passed, with 21 passed and 6 ignored.
  - `cd graph && cargo test --features pg17 projection::test_contracts --
    --ignored`: failed as expected with each contract reporting the missing
    production feature.
- Regression report: no runtime or memory-sensitive code changed in this
  checkpoint; existing `pre_durable_projection` baseline remains current.
