# Selective RLS measurements (10M profile)

Tested source commit: `455351ca049a6dac73852fd90f0fd0a97d6556e4`.
Installed library SHA-256:
`d33cf6a74a76c3c8a9d7e46cff251cb9b501dde953d6d5523e9e62ab469c399d`.
This was an optimized PostgreSQL 17.11 Linux aarch64 development build
(`pg17 development`, without `pg_test`). Build and runtime facts are separately
attested in `runtime-attestation.json`; the library hash does not identify its
features by itself.

All four cases completed ten measured samples after three warmups. Exact-result,
percentile, strategy and resource validation passed within the frozen budgets.
The source and library stayed unchanged and the owned cluster stopped cleanly.
These measurements cover this profile, not every query shape or a latency SLA.

| Case | Selected strategy | p95 total (ms) | Maximum policy-source rows |
|---|---|---:|---:|
| Identity one-hop, automatic | Lazy | 2792.5595 | 3 |
| Same identity query, forced eager | Eager | 64947.0115 | 30000000 |
| Sparse-policy whole-source scan | Eager | 28408.12585 | 300000 |
| Identity one-hop, no RLS | Lazy | 321.46415 | 0 |

Automatic and forced eager identity queries produced the same one-row signature
in every sample. Automatic execution performed no repeated GQL read recheck;
forced eager and no-RLS execution rechecked one matched row, and the whole-source
case rechecked ten. `samples.csv` and `summary.csv` retain every measured timing,
signature, memory charge, work charge and recheck count.

The fixture contains ten million scalar nodes plus ten million composite nodes,
with two N-1 relationship mappings: 20 million nodes and 19,999,998 relationships
in total. The profile name is not the total graph node count. The CSR build took
374614.234252 ms and reported 1953.179370880127 MiB of projection memory.

The container had four CPUs and a 6 GiB memory limit inside a Docker Desktop VM
configured with 12 GiB. The cgroup lifetime `memory.peak` reached exactly 6 GiB
and `memory.events.max` increased from 0 to 31679, indicating pressure at the
container limit. No OOM or OOM-kill event occurred. This run does not demonstrate
spare container memory. A previous preparation attempt with the same container
limit on an approximately 8 GiB outer VM was killed by a global VM OOM during
build, before any query samples. It is retained as a separate preparation
failure, not a latency observation.

Query workspace was 2048 MiB, the graph memory limit was 4096 MiB, the work limit
was 100 million units, and query spill remained 4096 MiB. Only fixture building
used a 16384 MiB spill allowance, reset before query execution. The effective
settings are retained in `work-governor.csv`, `build-spill-setting.csv` and
`query-spill-setting.csv`. The frozen query acceptance budgets were unchanged.

## Reproduction

Use the tested commit, the attested development-only release build and a fresh
disposable PostgreSQL 17 cluster with the recorded CPU/memory envelope. Configure
`fsync=on`, `autovacuum=off`, TCP listening off, and the effective graph governors
in `runtime-attestation.json`. Use an owned Unix socket and ordinary libpq
connection settings. The maintained runner does not explicitly ANALYZE the
fixture.

Copy `graph/tests/heavy/rls_large_table_baseline.sh` outside the clean checkout.
Set only the copy's `SCRIPT_DIR` to the absolute source `graph/tests/heavy`
directory, so its source provenance still names the clean checkout. Around its
single `SELECT * FROM graph.build();` preparation statement, add:

```sql
SET graph.spill_disk_limit_mb = 16384;
SELECT * FROM graph.build();
RESET graph.spill_disk_limit_mb;
```

Record `current_setting('graph.spill_disk_limit_mb')` immediately before and
after that build, as in the retained setting CSVs. No query SQL or query budget
changes are needed. The original runner, adapted runner and adaptation diff
hashes are recorded in `validation.json`; local copy/capture paths affect the
adapted hashes.

```sh
NODE_COUNT=10000000 COMPOSITE_COUNT=10000000 SAMPLES=10 WARMUPS=3 QUERY_MEMORY_MB=2048 PERSIST_ON_BUILD=on STATEMENT_TIMEOUT_MS=6000000 RUN_PROFILE=p5_release SKIP_INSTALL=1 PG_VERSION_FEATURE=pg17 DBNAME=pggraph_fresh_measurement ROLE_NAME=pggraph_fresh_reader OUTPUT_DIR=/path/to/fresh-output PG_CONFIG=/path/to/pg_config bash /path/to/copied-runner.sh
```

Percentiles use PostgreSQL `percentile_cont` interpolation. Visibility
`source_rows` counts returned policy rows, not all PostgreSQL rows examined or
hydration work. Zero GQL rechecks does not imply zero hydration SQL. Governor
memory is distinct from process RSS and cgroup lifetime memory. The 1M and 10M
profiles used different source commits and resource envelopes, so their timings
are not a controlled scaling comparison.

Measurement CSVs and plans are copied byte-for-byte. `redactions.json` records
only local host/path removal from run metadata and the validator's source path.
`selection-provenance.json` identifies original file and validation hashes;
selection itself is not validation. Relative log references in `attempts.csv`
identify omitted original logs. Cluster files and local qualification wrappers
are also omitted. `SHA256SUMS` covers all selected files except itself.
