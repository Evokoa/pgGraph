# P6 edge-type width evidence

Status: complete

This checkpoint measures a synthetic encoded edge-type section. It is a
microbenchmark of sequential decode, artifact copying, and bounded
encoded-section access patterns; it is not production graph traversal or RLS
latency.

- Exact commit: `487b7d64633b58719f0eb1a7ec302642a84b7de9`
- Command: `cargo bench --features "pg17 benchmarks" --bench edge_type_width_bench`
- Date: 2026-08-13
- Host: MacBook Pro Mac16,7, Apple M4 Pro (14 cores), 24 GB RAM
- OS: macOS 26.5 (25F71), arm64
- Toolchain: rustc 1.95.0, cargo 1.95.0
- Fixture: 1,000,000 edge-type values
- Criterion: decode/copy 30 samples; traversal 10 samples, one warmup second

## Result

The evidence supports adaptive 1/2/4-byte physical storage for P7. It preserves
one artifact byte per edge through 254 user labels, expands to two bytes through
65,534, and uses four bytes above that boundary. Logical IDs remain fixed u32.

At 254 labels, fixed u32 used 4,000,000 artifact bytes versus u8's 1,000,000.
Its sequential decode median was 3.806x u8 and its representative synthetic
traversal median (degree 8, depth 4, Out, no filter) was 2.115x. Copy
throughput per byte was 0.997x. All three satisfy the predeclared limits in
`budgets.json` (4.0x, 2.5x, and at least 0.7x respectively).

The u16 sequential decode loop was unexpectedly slower on this host, while its
small-window traversal cases remained between u8 and u32 in most shapes. This
is a reason to preserve raw results and re-run on release hosts, not a reason
to choose fixed u32 and pay four bytes per edge at low cardinality.

## Files

- `budgets.json`: acceptance limits committed before measurement.
- `summary.csv`: u8, u16, and u32 artifact bytes, decode medians, and copy medians.
- `traversal.csv`: filter selectivity, degree, direction, and depth sweep medians.
- `criterion-estimates.csv`: extracted Criterion median and 95% confidence bounds.

The traversal matrix uses one-factor-at-a-time sweeps around degree 8, depth 4,
Out, no filter. It covers degree 1/8/64/1024, depth 1/4/16, Out/In, and
none/one/all filter selectivity without claiming a full Cartesian product.
