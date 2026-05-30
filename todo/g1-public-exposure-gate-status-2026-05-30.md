# G1 Public-Exposure Gate Status - 2026-05-30

## Status

Closed by benchmark regression. Do not flip `graph.gql()` or
`graph.gql_explain()` out of `development` until a later run satisfies the
zero-regression `bfs_bench` gate.

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

The remaining G1 gate in `todo/build-sequence.md` requires a `bfs_bench`
comparison against `pre_gql_mutable_overlay` with zero regression.

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

### Follow-up Mitigation Attempt - 2026-05-31

The development-only GQL frontend and shared query planner modules were gated
out of plain `pg17` builds so the prototype cannot perturb production or
benchmark binaries before G1. Validation after the cfg change:

- `cargo build --features pg17 --lib --no-default-features`
  - passed with no GQL/query dead-code warnings
- `cargo check --features "pg17 fuzzing" --lib --no-default-features`
  - passed; GQL parser remains available to fuzz support without compiling the
    shared query planner
- `cargo test --features "pg17 development" query::`
  - `28 passed; 0 failed; 303 filtered out`
- `cargo pgrx test --features "pg17 development" gql`
  - `18 passed; 0 failed; 421 filtered out`
- Targeted benchmark rerun:
  - `cargo bench --features pg17 --bench bfs_bench -- bfs_traverse/d1_supernode/10k --baseline pre_gql_mutable_overlay`
  - result: `4.6040 us`, `+141.44%`, `Performance has regressed`

The targeted rerun is worse than the earlier full run, which makes current-host
measurement drift or load/thermal state likely. It still does **not** satisfy
the G1 zero-regression gate.

## Next Action

Investigate or re-baseline the engine-level benchmark regression before
proceeding past G1. Phase 2 must not begin while this zero-regression gate is
red.

After valid verification exists, record the results in
`todo/regression-baseline-2026-05-29.md` or a replacement baseline note, then
reconsider whether `graph.gql()` can be moved out of the `development` feature.
