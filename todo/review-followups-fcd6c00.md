# Review Follow-ups Since fcd6c00

Review range: `fcd6c00c575d9890745441c7319a541c2e580cc8..HEAD`

Created from the 2026-06-05 Rust review of the GQL path-pattern, join,
wildcard traversal, and mutable-write hardening stack.

## Findings to Resolve

### 1. Clarify or Correct Transaction-Created Traversal Entry Rejection

Status: resolved in `64e2de0` and the current test stack

Relevant code:

- `graph/src/query/execute.rs`
- `source_id_equalities`
- `reject_tx_created_traversal_entry_points`

Current concern:

The traversal entry-point rejection narrows transaction-created node checks only
when it can extract source `id` equality predicates. If `allowed_node_ids` is
`None`, any transaction-created node in the table can reject the traversal, even
when the query predicate would later exclude it. The `AND` handling also unions
id equality sets, so contradictory predicates such as `u.id = 'u1' AND u.id =
'u2'` can still reject `u1` instead of naturally producing zero rows.

Resolution options:

- Make the narrowing logically exact enough for supported predicates.
- Or document the current policy as table-level rejection whenever the executor
  cannot prove that a transaction-created node is excluded before traversal.

Suggested implementation direction:

- `AND` should intersect id equality sets when both sides constrain the same
  binding.
- `OR` can union only when both sides constrain the same binding.
- Unknown predicate shapes should preserve the conservative table-level
  rejection behavior, but tests and docs should describe that behavior.

Required coverage:

- A traversal with an unrelated tx-created node and a source `id` predicate still
  succeeds.
- Contradictory source `id` predicates do not produce a tx-created traversal
  rejection for only one side of the contradiction.
- Unknown or non-id predicates retain the intended conservative behavior.

Resolution:

`source_id_equalities` now intersects source-id equalities across `AND` and
unions `OR` only when both sides provide exact id sets. The regression stack
covers unrelated transaction-created nodes excluded by source id, contradictory
source-id predicates, source-id disjunctions, multi-pattern bound-later source
slots, and wildcard/unlabeled wildcard path entry points.

### 2. Add Write-Boundary Recheck Coverage for REMOVE and Edge DELETE

Status: resolved in the current change set

Relevant code:

- `graph/src/sql_facade/gql.rs`
- `graph/tests/heavy/gql_write_recheck_race.sh`
- `graph/src/pg_tests/gql.rs`

Current concern:

`docs/known-issues.mdx` marks KI-001 as hardened for `SET`, `REMOVE`, `DELETE`,
and `DETACH DELETE`, but the heavy race coverage found during review appears to
exercise stale `SET`, stale tenant recheck, and stale `DETACH DELETE`. Add direct
regression coverage for the remaining write paths before relying on the KI-001
claim.

Required coverage:

- `REMOVE` rechecks the matched node predicate at the final PostgreSQL write
  boundary and fails if the locked row no longer matches.
- Mapped edge `DELETE` rechecks the matched endpoint predicates at the final
  PostgreSQL write boundary and fails if either locked endpoint row no longer
  matches.
- Verification should be recorded in `todo/measurements.md` once the tests are
  added and run.

Resolution:

`graph/tests/heavy/gql_write_recheck_race.sh` now includes a two-session
`REMOVE` race proving stale matched node predicates fail after row locking and
leave the concurrently changed source row intact. Edge `DELETE` final
write-boundary coverage lives in
`gql_delete_edge_write_recheck_rejects_stale_endpoint_predicate`, which uses the
`pg_test`-only `_test_recheck_delete_edge_predicate` helper to exercise
`lock_and_recheck_edge_write` directly after an endpoint row no longer matches
the bound target predicate. The public `graph.gql()` path filters already-stale
hydrated endpoint predicates before edge deletion cardinality checking, so the
focused helper keeps the final recheck guard covered without changing public
query semantics.

## Review Notes

The commit stack was large for the stated scope: 35 commits and roughly 8.6k
added lines across parser fuzzing, GQL path patterns, multi-pattern joins,
aggregation, wildcard deletes, write rechecks, docs, and tests. The subjects
look like incremental feature slices rather than obvious session corruption, but
future changes in this area should be checkpointed around smaller behavioral
boundaries.

Review verification:

- `git diff --check fcd6c00c575d9890745441c7319a541c2e580cc8..HEAD`: passed.
- `cargo fmt --check` from `graph/`: passed.
- `cargo test --features pg17 query::` from `graph/`: passed, 158 tests.
