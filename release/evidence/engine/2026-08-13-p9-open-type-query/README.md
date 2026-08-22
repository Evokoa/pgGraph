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
RUN_ID=<full-exact-commit> \
EVIDENCE_DIR=release/evidence/engine/2026-08-13-p9-open-type-query \
./graph/tests/heavy/run_open_type_query_criterion.sh

cd graph
PG_VERSION_FEATURE=pg17 \
RUN_ID=<full-exact-commit> \
OUTPUT_DIR=../release/evidence/engine/2026-08-13-p9-open-type-query \
./tests/heavy/open_type_query_latency.sh
cd ..

RUN_ID=<full-exact-commit> \
OUTPUT_DIR=release/evidence/engine/2026-08-13-p9-open-type-query \
./graph/tests/heavy/run_open_type_query_resources_docker.sh

python3 scripts/write_p9_open_type_run_metadata.py \
  --repo-root . \
  --evidence-dir release/evidence/engine/2026-08-13-p9-open-type-query \
  --measurement-commit <full-exact-commit>
python3 release/evidence/engine/2026-08-13-p9-open-type-query/check_results.py \
  --repo-root . \
  --evidence-dir release/evidence/engine/2026-08-13-p9-open-type-query \
  --budget-commit dd730b8
```

The frozen budget files still resolve from their original paths inside the
historical `dd730b8` tree. The reconciler uses those historical Git objects;
the live retained evidence now belongs under `release/evidence/engine/`.

The `cargo bench` Criterion run measures internal production Rust seams. The PostgreSQL runner
measures the public SQL surfaces with same-backend warmups. The resource runner
measures real PostgreSQL backend RSS and Linux PSS inside Docker. These are
different evidence domains and their artifact-byte fields are not interchangeable.

## Retained result

Exact commit: `a07b662523180685af3bc65cbb56921a38fe21ec`.

The deterministic checker passed all budgets frozen at `dd730b8`, verified the
measurement commit's ancestry, and reconciled every raw input with its summary:
65 Criterion cases, eight PostgreSQL cases, and Linux resource runs with 1, 4,
and 8 backends. The 65,536-label resource fixture retained distinct real
PostgreSQL PIDs and nonzero PSS for every backend. Its 8-to-1 total query PSS
ratio was 5.86x and its baseline-subtracted ratio was 6.36x, both below the
9.0x limit. The immutable projection used 54.7 bytes per directed edge, below
the 64-byte limit.

The `open_type_query_resources.sh` resource image was built from `git archive`
for the exact commit. The
retained metadata binds its image revision and source-archive SHA-256 to that
commit. Each query-phase sample was accepted only while every exact backend PID
reported the marked traversal statement as active.

The first Criterion capture overlapped the PostgreSQL and Docker producers. One
case produced a 21.1% confidence-interval width against the frozen 20% limit,
so that attempt was inconclusive. Its raw and normalized outputs remain under
`attempts/a07b662-parallel-inconclusive/`. The retained result is the complete
65-case serial rerun from the same exact commit; no budget was changed.
