# P4.5 weighted-path RLS checkpoint

This directory retains one PG17 functional/performance sample for the P4.5
resumable weighted-path executor. It is a checkpoint, not a statistical
latency claim.

The run used 10,000 scalar nodes, 10,000 composite nodes, no warmups, and the
`p3_selective` profile. The weighted-path rows in `summary.csv` show:

- eager caller-RLS path: 9,598.182 ms and 30,000 source rows;
- lazy caller-RLS path: 57.649 ms, 5 visibility SPI calls, 5 requested keys,
  and 5 returned source rows;
- no-RLS automatic path: 34.731 ms and zero visibility SPI/source rows.

The profile also reran the established BFS, DFS, and unweighted-path gates.
`run-metadata.txt`, logs, plans, samples, summaries, database metadata, and the
dirty-tree record preserve the exact environment. Reproduce from `graph/` by
running `tests/heavy/rls_large_table_baseline.sh` with the settings recorded in
`run-metadata.txt` and `RUN_PROFILE=p3_selective`.
