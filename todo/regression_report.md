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

