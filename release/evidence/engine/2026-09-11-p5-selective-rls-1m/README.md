# Selective RLS measurements (1m profile)

Tested source commit: `5e9977151c3e2419a570e6fc78421ef6cf160df0`. Artifact SHA-256:
`a92759fd50f1805bb0f1d15fd7dc5f444cdc53150d7cf1b51aea294384f14909`. Build and runtime facts are separately supplied
in `runtime-attestation.json`. This run used PostgreSQL 17.11 on Linux aarch64,
four CPUs, a 4 GiB container limit and the optimized development build.

All four cases completed ten measured samples after three warmups. Raw-result,
percentile, strategy and resource validation passed within the frozen budgets.
The source and library stayed unchanged, the owned cluster stopped successfully,
and no new OOM event occurred. These measurements do not establish a latency SLA.

| Case | Selected strategy | p95 total (ms) | Maximum policy-source rows |
|---|---|---:|---:|
| Identity one-hop, automatic | Lazy | 131.8951 | 3 |
| Same identity query, forced eager | Eager | 3448.56055 | 3000000 |
| Sparse-policy whole-source scan | Eager | 1990.8059 | 30000 |
| Identity one-hop, no RLS | Lazy | 43.75315 | 0 |

Automatic and forced eager identity queries produced the same one-row signature
in every sample. Automatic execution performed no repeated GQL read recheck;
forced eager and no-RLS execution each rechecked one matched row. The
whole-source case rechecked ten rows. Full per-sample timings, recheck counts,
memory and work charges remain in `samples.csv` and `summary.csv`.

The fixture has 1000000 scalar nodes plus 1000000 composite nodes, with two N-1
relationship mappings. The profile name is not the total graph node count.
The four cases cover scalar one-hop eager/auto, sparse-policy whole-source work,
and a no-RLS identity baseline. They do not qualify every topology query shape.

Reproduce from this source commit using a fresh disposable PG17 cluster, the
verified development-only release artifact (without pg_test), the recorded
runtime limits and graph governors, and ordinary libpq connection settings:

```sh
NODE_COUNT=1000000 COMPOSITE_COUNT=1000000 SAMPLES=10 WARMUPS=3 QUERY_MEMORY_MB=1024 PERSIST_ON_BUILD=on STATEMENT_TIMEOUT_MS=600000 RUN_PROFILE=p5_release SKIP_INSTALL=1 PG_VERSION_FEATURE=pg17 DBNAME=pggraph_fresh_measurement ROLE_NAME=pggraph_fresh_reader OUTPUT_DIR=/path/to/fresh-output PG_CONFIG=/path/to/pg_config bash graph/tests/heavy/rls_large_table_baseline.sh
```

Configure all `effective_governors` from `runtime-attestation.json` before running.
The runner does not independently configure every recorded graph governor.
Work, hard-memory and spill settings used the clean extension defaults;
autovacuum used the fresh PostgreSQL cluster default. The retained commands and
source defaults establish these settings, which this run did not query
separately as effective GUCs. The runner does not explicitly ANALYZE the fixture.
Percentiles use PostgreSQL percentile_cont interpolation. Visibility source_rows
counts returned policy rows, not all PostgreSQL examined rows or hydration work.
Zero GQL rechecks does not imply zero hydration SQL; predicates or ordering can
require hydration even with hydrate=false. Governor memory is not process RSS.

Measurement files are copied byte-for-byte. redactions.json records only local
host/path removal from run metadata. Relative log references in attempts.csv
point to private originals, whose logs and cluster files are deliberately omitted.
`SHA256SUMS` covers selected files except itself. These results qualify the 1M
profile only; they do not stand in for the separate 10M profile.
