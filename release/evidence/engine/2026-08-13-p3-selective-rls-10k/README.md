# P3 selective RLS paired benchmark (10k)

This retained PG17 run compares the eager and lazy visibility strategies on
the same clean CSR projection with one warm backend sample per case. It is a
functional/performance gate, not a statistically powered release benchmark.

- Every eager/lazy pair returned the same row count.
- Lazy probes examined 0–4 returned source rows; eager scans examined 99–309
  visible source rows on this sparse-allow fixture.
- Lazy elapsed time was 35–47 ms versus 146–430 ms for the paired eager cases.
- The no-RLS automatic route used zero policy SPI calls and zero source rows.
- Retained node and relationship plans use the production JSONB CTE plus typed
  equality predicate and show an index-only scan with `enable_seqscan=off` to
  isolate index eligibility from small-table planner cost choices.

See `summary.csv`, `samples.csv`, `attempts.csv`, `plans/`, and
`run-metadata.txt` for the raw evidence and environment.
