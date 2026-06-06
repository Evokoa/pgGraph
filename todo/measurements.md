# Mutable Projection Measurements

This file records the current performance baseline before durable projection
implementation begins. Use it to detect regressions while adding manifest,
segment, layered-neighbor, compaction, GC, and recovery work.

## Baseline Run

| Field | Value |
|---|---|
| Date | 2026-06-07 |
| Repository commit | `abd1512` |
| Working tree | Documentation/TODO changes only; no Rust source changes for this plan |
| Rust | `rustc 1.95.0 (59807616e 2026-04-14)` |
| Cargo | `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` |
| Host | `aarch64-apple-darwin` |
| Hardware query | `sysctl` CPU/memory query was blocked by the sandbox |
| Benchmark command | `cd graph && cargo bench --features pg17 --bench bfs_bench -- --save-baseline pre_durable_projection` |
| Criterion baseline name | `pre_durable_projection` |

The benchmark compiled and completed successfully. Criterion artifacts are under
`graph/target/criterion/`.

## Traversal Baseline

`bfs_traverse` measures raw BFS over synthetic CSR stores. It does not include
PostgreSQL, SPI, SQL row materialization, or hydration.

| Case | Mean time | Throughput |
|---|---:|---:|
| `d1_supernode/10k` | `2.1033 us` | `4.7545 Gelem/s` |
| `d3_supernode/10k` | `69.905 us` | `143.05 Melem/s` |
| `d5_supernode/10k` | `718.58 us` | `13.916 Melem/s` |
| `d3_leaf/10k` | `7.0625 us` | `1.4159 Gelem/s` |
| `d1_supernode/100k` | `8.4706 us` | `11.806 Gelem/s` |
| `d3_supernode/100k` | `111.33 us` | `898.26 Melem/s` |
| `d5_supernode/100k` | `2.2646 ms` | `44.158 Melem/s` |
| `d3_leaf/100k` | `10.432 us` | `9.5862 Gelem/s` |
| `d1_supernode/500k` | `35.271 us` | `14.176 Gelem/s` |
| `d3_supernode/500k` | `159.56 us` | `3.1337 Gelem/s` |
| `d5_supernode/500k` | `6.0244 ms` | `82.996 Melem/s` |
| `d3_leaf/500k` | `46.776 us` | `10.689 Gelem/s` |
| `d1_supernode/2M_panama` | `137.80 us` | `14.513 Gelem/s` |
| `d3_supernode/2M_panama` | `300.93 us` | `6.6460 Gelem/s` |
| `d5_supernode/2M_panama` | `17.848 ms` | `112.06 Melem/s` |
| `d3_leaf/2M_panama` | `146.21 us` | `13.679 Gelem/s` |

## Graph Construction Baseline

`graph_construction` measures synthetic CSR/index construction from already
generated benchmark fixtures. It is not SQL `graph.build()` latency.

| Case | Mean time |
|---|---:|
| `build/10k` | `1.7964 ms` |
| `build/100k` | `20.971 ms` |
| `build/500k` | `113.29 ms` |

## Overlay Hot-Path Baseline

`bfs_overlay_paths` is the most relevant current benchmark group for durable
layered-neighbor work. It uses a 100k-node graph, depth 3 traversal, and the
current backend-local overlay model.

| Case | Mean time | Throughput |
|---|---:|---:|
| `no_overlay_d3` | `106.51 us` | `938.85 Melem/s` |
| `sparse_overlay_d3` | `106.30 us` | `940.75 Melem/s` |
| `dense_overlay_d3` | `117.22 us` | `853.12 Melem/s` |

## Filter Index Baseline

`bfs_filter_index_paths` measures traversal with registered numeric filter
columns. Durable filter-delta segments must preserve this behavior.

| Case | Mean time | Throughput |
|---|---:|---:|
| `score_gte_50_d3/sparse_10pct` | `8.7164 us` | `11.473 Gelem/s` |
| `score_gte_50_d3/dense_100pct` | `24.329 us` | `4.1103 Gelem/s` |

## Regression Use

Use Criterion comparison against the saved baseline:

```bash
cd graph
cargo bench --features pg17 --bench bfs_bench -- --baseline pre_durable_projection
```

Before replacing committed `Engine.edge_buffer` behavior, add benchmark cases
for:

- base-only manifest with no segments;
- L0 segment reads;
- many L0 segment reads;
- compacted L1/L2 reads;
- dirty base chunk rewrite pressure;
- durable weight segment lookup;
- GQL relationship expansion over layered segments;
- transaction-local delta layered on top of durable segments.

The overlay hot-path group is the first guardrail. Durable layered reads should
not regress `no_overlay_d3`, `sparse_overlay_d3`, or `dense_overlay_d3` without
recorded evidence and an explicit release decision.

