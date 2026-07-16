# R2C Runtime Resource Breakers

Date: 2026-07-15

## Result

R2C's local PostgreSQL 17 checkpoint is green. Persisted load, traversal,
paths, GQL candidate/projection work, hydration, durable sync ingestion,
range compaction, and connected-component analytics now use typed memory,
row, work, disk, elapsed, and interrupt breakers. Failed reservations return
SQLSTATE `54000` with diagnostic `PG007` before publication.

Direct source search preflights token-predicate SQL construction from checked
input and token counts. Workflow search, expansion, relationship discovery,
shortest-path summaries, and neighborhood grouping retain the same statement
governor through fallible output allocation and postprocessing.

## Verification

| Gate | Result |
|---|---|
| Formatting and diff hygiene | Pass: `cargo fmt --check`; `git diff --check`; shell syntax checks for the runtime and aggregate release gates. |
| Rust tests | Pass: 778 tests, 1 intentional ignore, 0 failures with `cargo test --no-default-features --features "pg17 development" --no-fail-fast`. |
| PostgreSQL 17 | Pass: 1,049 tests, 1 intentional ignore, 0 failures with `cargo pgrx test --features "pg17 development" pg17`. |
| Static analysis | Pass: all-target Clippy with warnings denied. |
| Rust documentation | Pass: rustdoc with warnings denied; doctests have no failures. |
| Public documentation and contract | Pass: local references, SQL API/GUC inventory, Rust source map, and the 1.0 release contract are synchronized. |
| Secret scanning | Pass: gitleaks full-history and pending tracked-change scans. |
| Runtime resource profile | Pass: `NODE_COUNT=10000`, `EDGE_COUNT=9999`, three persisted reloads, a 101-row hydrated traversal, a 10,000-row hydrated GQL scan, connected components, two 500-row source-update ingestion/compaction cycles, and an intentional work-limit rejection. |

## Runtime Profile

Command:

```text
PG_VERSION_FEATURE=pg17 DBNAME=pggraph_runtime_resources_r2c \
NODE_COUNT=10000 EDGE_COUNT=9999 \
LOAD_ROUNDS=3 SYNC_ROWS=500 ./tests/heavy/measure_runtime_resources.sh
```

Observed on macOS/arm64 with PostgreSQL 17:

- peak backend RSS: 87 MiB;
- each persisted load preflighted approximately 1.0 MiB of graph-owned load
  workspace and completed three consecutive cycles;
- the hydrated GQL scan reserved 180,211,824 bytes across candidate,
  execution, hydration, and blocking workspaces under the gate's explicit
  256 MiB successful-query budget, and consumed 80,000 projection work units;
- analytics reserved 400,000 bytes and consumed 19,999 work units;
- each sync cycle read 500 committed source rows and normalized 1,500
  projection rows within the declared 64 MiB input ceiling; end-to-end
  ingestion, candidate reload, and governed manifest publication peaked at
  204,948,265 memory bytes and 98,764 staged disk bytes;
- both compaction cycles merged published edge segments and installed validated
  generations; the second cycle compacted two retained segments;
- setting `graph.query_work_limit = 1` rejected the GQL scan with SQLSTATE
  `54000` and diagnostic `PG007`; `graph.resource_status()` reported one
  candidate work unit.

Linux PSS is not available from macOS. On Linux the same gate samples
`/proc/<pid>/smaps_rollup` and enforces `MAX_PSS_MB`; it is wired into
`run_release_gate.sh` through the enabled-by-default
`RUN_RUNTIME_RESOURCES=1`. The supported-environment release run must retain
that Linux PSS evidence rather than substituting RSS.

## Preserved State And Remaining KI-013 Work

- A replacement load with no remaining private-memory headroom rejects before
  allocation; the serving engine is not mutated.
- Projection installation derives and pins one immutable layered snapshot per
  generation. Repeated queries continue from that snapshot without rereading
  deleted segment files.
- Compaction interruption leaves the previous generation current; sync
  watermark tests advance only after successful publication.
- KI-013 remains open. R3 still owns mmap-backed inbound CSR and large filter
  metadata, and whole-graph analytics still need worker isolation. The public
  Known Issues page names only those remaining gaps.
