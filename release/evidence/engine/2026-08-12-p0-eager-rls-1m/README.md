# P0 eager-RLS baseline: 1M rows per source mapping

Date: 2026-08-12  
Commit: `577ef2a` plus the uncommitted P0 benchmark/test checkpoint  
PostgreSQL: 17, local Apple Silicon development build  
Profile: `RUN_PROFILE=compact`, `SAMPLES=1`, `WARMUPS=0`,
`graph.query_memory_mb=1024`

## Fixture

- 1,000,000 scalar-key node rows
- 1,000,000 composite-key node rows
- 999,999 standalone relationship rows
- 2,000,000 projected nodes and 1,999,998 directed edges
- build time: 111,477 ms
- reported projection memory: 176.25 MiB
- source relation sizes: 107,003,904 bytes scalar nodes, 116,080,640
  bytes composite nodes, and 82,771,968 bytes relationship rows

## Result

The no-RLS depth-zero control completed in 3,712 ms. The development visibility
metric reported 9 microseconds, zero hidden nodes, and zero hidden relationships;
the remaining time includes query start and loading the persisted backend-local
projection.

The first node-RLS broad-allow depth-zero query did not finish after more than
600 seconds and was cancelled manually. PostgreSQL returned query-cancelled and
the backend remained usable. This is a censored lower bound, not a completed
latency sample.

The eager builder scans every RLS-active registered source mapping before seed
resolution. In this fixture one targeted depth-zero query therefore scans the
million-row scalar and composite node mappings even though it requests one
scalar identity. The observed result is sufficient to reject eager full-source
visibility as the scalable targeted-query strategy.

## 10M decision

A 10M-per-mapping graph profile was not executed on this workstation. The 1M
RLS query already exceeded ten minutes and the 2M-node projection used 176 MiB
before query workspace. Extrapolating this eager implementation to 10M would
consume substantial local time and memory without changing the architectural
decision. The benchmark runner retains a parameterized 10M soak profile for a
dedicated benchmark host. P5 must produce completed 1M and 10M lazy/eager
comparisons before the scalable strategy can ship.

Exact SPI-call, key-scan, and byte-scan counts are recorded as unavailable in
the runner until dedicated instrumentation exists; this checkpoint does not
infer them from elapsed time.

`observations.csv`, `database-metadata.csv`, and `run-metadata.txt` retain the
values recorded during the manual run. The original per-statement log, exact
unrounded timing sample, and source `EXPLAIN` plans were not retained, so this
directory does not fabricate them after the fact. The hardened runner now
retains those artifacts automatically, including on statement timeout.
