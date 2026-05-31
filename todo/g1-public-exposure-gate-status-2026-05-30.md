# G1 Public-Exposure Gate Status - 2026-05-30

## Status

Benchmark gate refreshed. The original saved `pre_gql_mutable_overlay`
Criterion data is stale for this host; a same-host redo comparing current tree
against baseline commit `0574e6b` showed no BFS regression.

## Completed Evidence

- Phase 1A through 1D implementation commits exist on
  `feat/mutable-graph-projections`.
- DR-5 SQLSTATE taxonomy is implemented and committed:
  `12f75a8 feat(gql): map stable SQLSTATEs`.
- Tenant-scope topology filtering is implemented and committed:
  `078f094 fix(gql): enforce tenant scope`.
- Public positioning now says SQL functions are the current public API while
  GQL and SQL/PGQ are planned work:
  `1e57650 docs(gql): align public query language positioning`.
- Relationship identity projection is implemented for single-hop relationship
  variables, including inbound orientation preserving registered edge start/end.
- Phase 1 return-shape scope has been corrected to node returns, scalar
  property returns, and coordinate-only relationship identity returns. Raw path
  variables, path functions, and relationship source-row hydration are deferred
  to later read-path work rather than being part of the G1 public gate.
- Static hygiene passed:
  - `rustfmt --edition 2021 --check graph/src/gql/*.rs graph/src/query/*.rs graph/src/sql_facade/gql.rs graph/src/pg_tests/gql.rs graph/src/lib.rs`
  - `git diff --check`
- Standalone non-Cargo SQLSTATE/ACL boundary passed:
  - `DBNAME=pggraph_boundary_g1 graph/tests/heavy/run_sqlstate_acl_boundary.sh`
- Direct Cargo verification passed after the 2026-05-31 `sfw` policy
  clarification:
  - `cargo test --features "pg17 development" query::`
    - `28 passed; 0 failed; 303 filtered out`
  - `cargo pgrx test --features "pg17 development" gql`
    - `18 passed; 0 failed; 421 filtered out`
  - `cargo fmt --check`
  - `git diff --check`

## Cargo Verification Policy

The user clarified on 2026-05-31 that `sfw` is only required for package
installation, fetch, update, and other dependency-changing operations. Direct
Cargo is allowed for ordinary build, test, format, and benchmark commands.

## Missing Evidence

The G1 gate in `todo/build-sequence.md` requires a `bfs_bench` comparison
against the pre-GQL baseline with zero regression.

The 2026-05-31 direct Cargo comparison was:

```sh
cargo bench --features pg17 --bench bfs_bench -- --baseline pre_gql_mutable_overlay
```

Result: exit code `0`, but Criterion reported multiple statistically
significant regressions. Representative rows:

| Benchmark | Median/estimate | Change vs `pre_gql_mutable_overlay` | Criterion result |
|---|---:|---:|---|
| `bfs_traverse/d1_supernode/10k` | `2.0747 us` | `+7.1094%` | Performance has regressed |
| `bfs_traverse/d3_supernode/10k` | `62.257 us` | `+5.7855%` | Performance has regressed |
| `bfs_traverse/d3_leaf/10k` | `6.6976 us` | `+4.0042%` | Performance has regressed |
| `bfs_traverse/d1_supernode/100k` | `8.6323 us` | `+2.4086%` | Performance has regressed |
| `bfs_traverse/d5_supernode/100k` | `1.6795 ms` | `+2.0782%` | Performance has regressed |
| `bfs_traverse/d3_supernode/500k` | `125.28 us` | `+3.8378%` | Performance has regressed |
| `graph_construction/build/100k` | `21.465 ms` | `+3.5110%` | Performance has regressed |
| `bfs_overlay_paths/sparse_overlay_d3` | `90.304 us` | `+3.0220%` | Performance has regressed |

### Follow-up Diagnosis - 2026-05-31

The development-only GQL frontend and shared query planner modules are gated out
of plain `pg17` builds. That is the intended production shape before G1, not a
benchmark workaround. Validation:

- `cargo build --features pg17 --lib --no-default-features`
  - passed with no GQL/query dead-code warnings
- `cargo check --features "pg17 fuzzing" --lib --no-default-features`
  - passed; GQL parser remains available to fuzz support without compiling the
    shared query planner
- `cargo test --features "pg17 development" query::`
  - `28 passed; 0 failed; 303 filtered out`
- `cargo pgrx test --features "pg17 development" gql`
  - `18 passed; 0 failed; 421 filtered out`

The original `pre_gql_mutable_overlay` Criterion data is stale for this host,
and the first full rerun was invalidated by system sleep. Focused control runs
for `bfs_traverse/d1_supernode/10k` showed that the baseline commit and current
tree match when both are rebuilt on the same host:

| Run | Command | Median/estimate |
|---|---|---:|
| Baseline commit `0574e6b` in `/private/tmp/pggraph-baseline-0574e6b` | `cargo bench --features pg17 --bench bfs_bench -- bfs_traverse/d1_supernode/10k --save-baseline baseline_commit_current_host` | `2.7823 us` |
| Current tree, fresh target `/private/tmp/pggraph-current-target` | `CARGO_TARGET_DIR=/private/tmp/pggraph-current-target cargo bench --features pg17 --bench bfs_bench -- bfs_traverse/d1_supernode/10k --save-baseline current_fresh_target_control` | `2.7776 us` |
| Current tree, same-binary control | `cargo bench --features pg17 --bench bfs_bench -- bfs_traverse/d1_supernode/10k --baseline g1_same_binary_control` | `4.5908 us`, no statistically significant change vs its fresh control |

Interpretation: the earlier `4.6040 us` targeted run came from contaminated or
stale local build/measurement state.

The full comparison was then redone under `caffeinate` so macOS sleep could not
interrupt it:

1. Baseline commit `0574e6b` saved a same-host baseline:
   `caffeinate -i cargo bench --features pg17 --bench bfs_bench -- --save-baseline pre_gql_current_host_redo`
2. The current tree copied only the Criterion baseline data into a separate
   current-target directory, compiled its own bench binary, and compared against
   that baseline:
   `CARGO_TARGET_DIR=/private/tmp/pggraph-current-compare-target caffeinate -i cargo bench --features pg17 --bench bfs_bench -- --baseline pre_gql_current_host_redo`

Result: exit code `0`. Every reported comparison was either `Performance has
improved` or no regression. Representative rows:

| Benchmark | Current median/estimate | Change vs same-host baseline | Criterion result |
|---|---:|---:|---|
| `bfs_traverse/d1_supernode/10k` | `1.9519 us` | `-30.231%` | Performance has improved |
| `bfs_traverse/d3_supernode/10k` | `58.960 us` | `-30.003%` | Performance has improved |
| `bfs_traverse/d5_supernode/100k` | `1.7170 ms` | `-24.217%` | Performance has improved |
| `bfs_traverse/d3_supernode/500k` | `127.74 us` | `-28.472%` | Performance has improved |
| `bfs_traverse/d5_supernode/2M_panama` | `17.783 ms` | `-10.200%` | Performance has improved |
| `graph_construction/build/500k` | `118.23 ms` | `-44.950%` | Performance has improved |
| `bfs_overlay_paths/sparse_overlay_d3` | `92.285 us` | `-27.752%` | Performance has improved |
| `bfs_filter_index_paths/score_gte_50_d3/dense_100pct` | `20.577 us` | `-27.806%` | Performance has improved |

## Next Action

The benchmark part of G1 is satisfied by the same-host redo. The remaining G1
work is the public-exposure audit and code/docs flip:

- Update the compatibility matrix so Phase 1 rows state actual supported
  coverage, not only `required` coverage.
- Update public docs that currently say GQL and SQL/PGQ are planned/not current
  APIs before moving `graph.gql()` or `graph.gql_explain()` out of
  `development`.
- Keep non-Phase-1 rows clearly future/deferred so public docs do not imply full
  GQL, SQL/PGQ, Cypher, or write support.
- Re-run GQL unit/pgrx tests, SQLSTATE/ACL boundary, `cargo fmt --check`,
  `git diff --check`, and the refreshed same-host `bfs_bench` comparison after
  the public SQL surface changes.

After the public exposure diff exists, record its verification results here or
in a replacement baseline note before marking G1 complete.
