# P4.4 unweighted-path RLS checkpoint

This directory retains a single-sample PG17 functional/performance gate for
the P4.4 resumable unweighted-path executor. It is not a statistical latency
claim.

The run used 10,000 scalar nodes, 10,000 composite nodes, one measured sample,
no warmups, and the `p3_selective` profile. The successful path cases in
`samples.csv` show:

- eager caller-RLS path: 9,324.821 ms and 30,000 source rows;
- lazy caller-RLS path: 45.606 ms, 7 visibility SPI calls, 8 requested keys,
  and 8 returned source rows;
- no-RLS automatic path: 34.102 ms and zero visibility SPI/source rows.

The profile also reran the established BFS/DFS eager-lazy row-count and bounded
source-work gates. `run-metadata.txt`, `commit.txt`, `git-status.txt`, logs,
plans, samples, summaries, and database metadata retain the exact environment
and dirty working-tree state. Reproduce from `graph/` with the invocation
recorded in `run-metadata.txt`; use the `pg_config` belonging to the PostgreSQL
server that the script's `psql` connects to.
