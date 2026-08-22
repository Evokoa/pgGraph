# P3 selective RLS scale boundary (1M)

The forced eager sparse-RLS BFS was censored by the 600-second statement
timeout before producing a sample. The raw attempt, PostgreSQL diagnostic,
plans, database metadata, and run metadata are retained here rather than
discarding the negative result.

After installing the final P3 snapshot and rebuilding the same disposable
projection, the equivalent forced-lazy query completed in 3,248.530 ms and
returned the same expected count of one. Its visibility metrics were:

```json
{"strategy":"lazy","spi_calls":3,"source_rows":2,"requested_keys":3,"returned_keys":2,"requested_key_bytes":9,"returned_key_bytes":6}
```

The 10k retained directory contains the completed paired correctness run. This
1M directory establishes the scale boundary: eager exceeded 600 seconds while
lazy completed in about 3.25 seconds with two returned source rows.
