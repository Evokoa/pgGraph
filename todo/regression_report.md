# Mutable Projection Regression Report

This report is appended after each performance-sensitive or memory-sensitive
change. It records the baseline used, regression commands, outcome, and
decision.

## 2026-06-07: Initial Planning Baseline

| Field | Value |
|---|---|
| Scope | Planning documentation and initial benchmark baseline |
| Code changes | None |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo bench --features pg17 --bench bfs_bench -- --save-baseline pre_durable_projection` |
| Result | Baseline captured successfully |
| Decision | Use `pre_durable_projection` as the comparison baseline until a phase-specific baseline is recorded |

## 2026-06-07: Microphase 0 Test Harness

| Field | Value |
|---|---|
| Scope | Test-only fixture helpers and ignored durable-projection contract tests |
| Code changes | `#[cfg(test)]` projection modules only |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::` |
| Result | Passed; ignored contract tests remain out of the default suite |
| Decision | No benchmark comparison required because no runtime, memory, traversal, SQL, or artifact production code changed |

## 2026-06-07: Microphase 1 Manifest And Generation Table

| Field | Value |
|---|---|
| Scope | Manifest JSON model, active-generation heartbeat helpers, generation metadata table, and public sync docs |
| Code changes | Pure manifest metadata plus SQL bootstrap; no traversal/read-path runtime adoption |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::manifest`; `cd graph && cargo check --features pg17`; `python3 scripts/check_doc_references.py` |
| Result | Manifest tests, non-test compile, and doc references passed. Full `cargo test --features pg17` is intentionally red with 528 passed, 5 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required until manifest loading affects engine status, reads, ingestion, or artifact publication |

## 2026-06-07: Microphase 2 Atomic Manifest Publish And Load

| Field | Value |
|---|---|
| Scope | Manifest filesystem store, atomic publish, latest-generation load, temp-file ignore, and active-reference validation |
| Code changes | Projection manifest file I/O only; no traversal/read-path runtime adoption |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::manifest`; `cd graph && cargo check --features pg17`; `python3 scripts/check_doc_references.py` |
| Result | Publish/load manifest tests, non-test compile, and doc references passed. Full `cargo test --features pg17` is intentionally red with 534 passed, 5 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required until manifests are loaded by engine status, reads, ingestion, cleanup, or repair paths |

## 2026-06-07: Microphase 3 Complete Segment Format

| Field | Value |
|---|---|
| Scope | Delta segment writer/loader, corruption validation, segment contract tests, and fuzz seeds |
| Code changes | Projection segment artifact codec under test/fuzz/development gates; no traversal/read-path runtime adoption |
| Baseline | `todo/measurements.md`, Criterion baseline `pre_durable_projection` |
| Command | `cd graph && cargo test --features pg17 projection::segment`; `cd graph && cargo test --features pg17 projection::test_contracts`; `cargo check --manifest-path graph/fuzz/Cargo.toml` |
| Result | Segment tests passed, two segment contract tests turned green, fuzz package compiled with existing sync-helper dead-code warnings. Full `cargo test --features pg17` is intentionally red with 542 passed, 3 future durable-projection contract failures, and 1 ignored scale test. |
| Decision | No benchmark comparison required until segments are produced by ingestion or consumed by layered reads, compaction, cleanup, or repair paths |
