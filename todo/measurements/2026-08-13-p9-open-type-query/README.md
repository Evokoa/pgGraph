# P9 open-type query evidence

This directory separates the P9 acceptance protocol from the measurements that
will evaluate it. The exact 65 Criterion cases and all acceptance limits were
committed in `dd730b8` before any result files existed. The protocol fixes the
PostgreSQL fixture, warmup, sample, and Linux backend-count matrix.
The latency fixture also fixes `graph.memory_limit_mb` at 2,048 MiB and
`graph.query_memory_mb` at 512 MiB so its million-edge projection is measured
under an explicit bounded allowance instead of the smaller interactive default.
Its GQL and Cypher cases use the canonical typed one-hop `:type_1` query, return
only `v`, and require exactly one non-null `v._id.id` equal to `2` before
digesting it. Relationship-object formatting and identity latency are outside
this benchmark's scope; the high-cardinality correctness matrix covers them.

The evidence tooling is committed as a separate checkpoint. After that commit
is clean, run the following commands from the recorded exact commit:

```bash
EVIDENCE_DIR=todo/measurements/2026-08-13-p9-open-type-query \
./graph/tests/heavy/run_open_type_query_criterion.sh

cd graph
PG_VERSION_FEATURE=pg17 \
OUTPUT_DIR=../todo/measurements/2026-08-13-p9-open-type-query \
./tests/heavy/open_type_query_latency.sh
cd ..

RUN_ID=<full-exact-commit> \
OUTPUT_DIR=todo/measurements/2026-08-13-p9-open-type-query \
./graph/tests/heavy/run_open_type_query_resources_docker.sh

python3 scripts/write_p9_open_type_run_metadata.py \
  --repo-root . \
  --evidence-dir todo/measurements/2026-08-13-p9-open-type-query \
  --measurement-commit <full-exact-commit>
python3 todo/measurements/2026-08-13-p9-open-type-query/check_results.py \
  --repo-root . \
  --evidence-dir todo/measurements/2026-08-13-p9-open-type-query \
  --budget-commit dd730b8
```

Criterion measures internal production Rust seams. The PostgreSQL runner
measures the public SQL surfaces with same-backend warmups. The resource runner
measures real PostgreSQL backend RSS and Linux PSS inside Docker. These are
different evidence domains and their artifact-byte fields are not interchangeable.

No retained-result claim is made at this checkpoint. A later clean measurement
commit will add raw inputs, normalized summaries, exact host/toolchain metadata,
and run `check_results.py`. If a predeclared budget fails, P9 remains open and
the adverse result is retained without changing the budget.
