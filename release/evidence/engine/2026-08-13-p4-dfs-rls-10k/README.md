# P4 resumable DFS selective-RLS checkpoint

This directory retains the P4 DFS rows from a completed PostgreSQL 17
`p3_selective` harness run on 2026-08-13. The run used 10,000 scalar and
10,000 composite source rows, one warmup, one recorded sample, a 1,024 MiB
query budget, and a 60-second statement timeout. It is a checkpoint gate, not
a statistical latency claim.

Command:

```sh
NODE_COUNT=10000 COMPOSITE_COUNT=10000 RUN_PROFILE=p3_selective \
  WARMUPS=1 SAMPLES=1 STATEMENT_TIMEOUT_MS=60000 \
  DBNAME=pggraph_p4_dfs_10k ROLE_NAME=pggraph_p4_dfs_10k_reader \
  OUTPUT_DIR=/private/tmp/pggraph-p4-dfs-rls-10k \
  ./tests/heavy/rls_large_table_baseline.sh
```

The paired Out/In/Any DFS rows matched exactly. Lazy source work stayed at two
rows or fewer and three SPI calls, while each eager case examined 300 source
rows. The no-RLS automatic strategy used zero visibility SPI calls and zero
source rows. A separate one-million-row run retained a censored eager BFS
lower bound at the configured 60-second timeout before reaching the new DFS
pairs; P4.7/P5 own completed million-row statistical evidence.

Environment: Apple arm64 Darwin, PostgreSQL 17.9 (Homebrew). The source tree
was intentionally dirty because this measured the uncommitted P4.3 candidate
based on commit `a394f71fa6ae6656edf857db6d2efb692b81b7d6`.
