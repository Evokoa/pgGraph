# Full Graph Engine Progress

Last updated: 2026-07-09

## Current State

| Checkpoint | Status | Evidence / next action |
|---|---|---|
| 0. Freeze and measure | In progress | Static audit complete; add the ordered P0 regression pack below and machine-readable conformance baseline. |
| Rust type/unsafe/pgrx boundary | RUST-00A through RUST-00F implemented on PG17; matrix pending | Safe mapped access is validated, graph errors unwind through pgrx, durable filter deltas preserve exact values, security-definer functions pin `pg_catalog`, and registered relations retain OID identity. Run Miri/sanitizer and supported-major evidence before checkpoint exit. |
| 1A. Security and identity | In progress | Coordinate-only node visibility and base-CSR relationship multiplicity are enforced on PostgreSQL 17; durable relationship keys, relationship-row RLS, filter identity, and savepoints remain. |
| 1B. Memory containment | Partial mitigation | Commit `8fea899` reduces old/new rebuild overlap; hard governor and query/load/compaction controls remain. |
| 1C. Safe publication | Not started | Add cross-backend lock/CAS and validate before switch. |
| 2. Artifact vNext/out-of-core | Planned | Focused predecessor is `todo/out-of-core-build-plan.md`. |
| 3. Bounded load/sync/compaction | Not started | mmap inbound/filter data and range compaction. |
| 4. Refactor foundations | Not started | Complete canonical enum/newtype, pgrx-adapter, and unsafe-allowlist work from `10-rust-type-safety-pgrx-boundaries.md` while splitting modules. |
| 5. Streaming costed runtime | Not started | Canonical IR and resource governor are prerequisites. |
| 6. Full GQL slices | Not started | Conformance registry is prerequisite. |
| 7. PostgreSQL 19 SQL/PGQ | Planned | Pinned pgrx 0.19.1 exposes `pg19`; add the graph feature/experimental lane, then execute PG19-0 through PG19-5 without duplicate registration. |
| 8. Competitive program | Planned | Requires correctness and reproducible baseline. |
| 9. Clear current public backlog | Planned | Close every ledger row and remove delivered/resolved work from active Roadmap/Known Issues. |
| Public backlog closure | In progress | `09-public-backlog-closure.md` maps every current Roadmap/KI row to evidence or an explicit decision. |

## Completed This Review

- Preserved parallel source relationship rows through base CSR construction and
  GQL traversal; an explicit PostgreSQL-backed regression verifies two equal
  endpoint/type rows return two matches. Durable source-row keys remain the
  next 1A slice for hydration, RLS, mutable overlays, and persistence.

- Added durable source primary-key metadata to relationship registrations and
  catalog fingerprints. Existing mappings backfill declared primary keys and
  fail closed when no stable relationship identity is available.

- Began the filter-identity migration by keying build-time filter registration
  and value assignment by PostgreSQL table OID plus attribute name, so
  same-named registered columns no longer overwrite one another during build.

- Carried table-qualified filter identity through structured pushdown and both
  sync replay paths; ambiguous user-facing names remain rejected until every
  legacy name-only helper is retired.

- Reviewed roadmap, known issues, TODO history, memory model, build/load,
  projection, GQL, query execution, hydration, and major refactor hotspots.
- Committed pre-existing low-memory rebuild mitigation as `8fea899`.
- Verified formatting, warnings-denied clippy, 652 Rust tests (1 ignored),
  targeted build tests, and targeted PostgreSQL-backed low-memory/GUC tests.
- Created the full-engine program plans in this folder.
- Updated public Roadmap and Known Issues to reflect P0 reality and full GQL.
- Added explicit PostgreSQL 19 native property-graph work packages and a public
  backlog closure ledger.
- Audited production Rust types, exact-value semantics, unsafe/mmap/FFI sites,
  pgrx integration, security-definer paths, relation identity, and worker
  transactions; added the executable Rust boundary plan and public KI owners.
- Verified pinned pgrx 0.19.1 already exposes `pg19`; the remaining PG19 work
  is an explicit feature/toolchain/test/package gate, not a wait for pgrx.

## Next Executable Checkpoint

Checkpoint 0 regression pack, in this order:

1. **Complete:** out-of-range mapped node lookup and malformed CSR offset tests
   exercise mapped accessors with aligned in-memory backing; mapped constructors
   reject invalid PK/CSR contents before a store exists.
2. **Complete on PG17; supported-major matrix pending:** error-boundary test
   proves Rust destructors unwind before PostgreSQL ERROR; standard SQLSTATEs
   carry stable pgGraph diagnostics in `DETAIL`.
3. **Complete on PG17; supported-major matrix pending:** durable filter
   differential preserves signed, large, temporal, boolean, Unicode text,
   UUID, SQL NULL, and tombstone values across sync, segment v3, consecutive
   manifest generations, and fresh-backend reload.
4. **Complete on PG17; supported-major matrix pending:** security-definer catalog
   metadata asserts a pinned `pg_catalog` path for every approved definer
   function.
5. **Complete on PG17; supported-major matrix pending:** registered-relation
   OID identity survives rename and search-path changes and fails closed after
   drop/recreate.
6. Two-role GQL RLS test with `hydrate := false` for node, relationship, path,
   scalar identity, aggregate count, and existence.
7. Same-name filter columns on two registered tables.
8. Parallel same-type/same-endpoint edge rows with distinct PK/properties.
9. Two-backend concurrent projection publication.
10. Invalid staged persisted replacement preserving the previous generation.
11. Memory profiles for stale/no statistics, many filters, supernode LIMIT 1,
   auto-load, and compaction.

Do not implement broad syntax until these tests establish the current
correctness and safety boundary.

## Phase Updates

- 2026-07-09 — Checkpoint 0 mapped-layout phase: made node metadata lookups fallible, validated mapped PK/CSR contents at crate-private constructors, and kept traversal/component corruption failures typed.
- 2026-07-09 — Checkpoint 0 error-boundary phase: replaced direct `errfinish()` FFI with pgrx stack unwinding, standard SQLSTATEs, stable `PGxxx` diagnostics, and a destructor regression.
- 2026-07-09 — Checkpoint 0 durable-filter phase: added projection segment v3 tagged values, staged exact filter reload, consecutive-generation retention, and signed/temporal/text/UUID/NULL/tombstone regressions.
- 2026-07-09 — Checkpoint 0 RUST-00E security-definer phase: every approved definer function now has pgrx-generated `pg_catalog, public` `search_path` metadata, with a catalog audit regression and public security guidance; RUST-00F will remove the temporary public compatibility entry by storing relation identity as OIDs.
- 2026-07-09 — Checkpoint 0 RUST-00F relation-identity phase: registration, discovery, filtering, synchronization, and removal now retain PostgreSQL OIDs; catalog reads derive qualified SQL names from those OIDs, while public result labels remain compatible. Rename/search-path and drop/recreate behavior is covered on PG17.
- 2026-07-09 — Checkpoint 1A node-visibility subphase: GQL batches source-node visibility checks under the caller's PostgreSQL ACL/RLS context before returning coordinates, including `hydrate := false`; relationship-row visibility remains coupled to the pending durable relationship-identity work.
- 2026-07-09 — Independent three-phase review: fixed watermark-only artifact retention, pre-copy ingest budgeting, filter node-range validation, and malformed base dictionary fail-closed handling; the follow-up review and final gates were green.

## Decisions

| Date | Decision | Rationale |
|---|---|---|
| 2026-07-09 | ISO GQL is the primary language target; SQL/PGQ has a separate adapter/matrix. | Avoid conflating related standards or expanding openCypher first. |
| 2026-07-09 | Spill is the default low-memory degradation path. | Slower completion is preferable to backend/container OOM. |
| 2026-07-09 | Module split before workspace split. | Remove coupling before choosing crate boundaries. |
| 2026-07-09 | Artifact publication is generation-based and validation-first. | Preserve the last good serving graph on every failure. |
| 2026-07-09 | RLS applies to topology as well as hydrated properties. | PostgreSQL remains authoritative even for coordinate-only graph results. |
| 2026-07-09 | PostgreSQL 19 native property graph definitions are authoritative mappings. | Avoid duplicate `add_table`/`add_edge` registration and share one PostgreSQL-owned graph definition across GQL and SQL/PGQ. |
| 2026-07-09 | Closed domain state uses canonical enums and identities/units use production newtypes. | Prevent invalid states and cross-domain primitive mixups; convert strings/primitives only at SQL/serialization boundaries. |
| 2026-07-09 | pgrx-native integration is the default; raw PostgreSQL and mmap code is an audited allowlist. | Preserve supported PostgreSQL guard, datum, GUC, identity, search-path, transaction, and lifetime behavior. |

## Measurement Log

Add dated entries with dataset shape, PostgreSQL/pgGraph settings, exact command,
peak RSS/PSS, spill, elapsed time, result checksum, and pass/fail threshold.
No benchmark result should be recorded without its correctness checksum and
environment.

### 2026-07-09 Checkpoint 0 Baseline

Environment: macOS arm64, Rust 1.96.0, pgrx 0.19.1, PostgreSQL 17 from
Homebrew. This establishes the correctness baseline before the mapped-layout
safety regression pack; workload RSS and Criterion measurements remain pending
until the corresponding deterministic checksum fixtures are selected.

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 652 passed, 1 ignored; doctests 0 |
| Rust docs | `cd graph && cargo doc --features pg17 --no-deps` | PASS |
| PostgreSQL-backed tests | `cd graph && cargo pgrx test --features "pg17 development" pg17` | PASS: 888 passed, 1 ignored; doctests 0 |

### 2026-07-09 Mapped-Layout Safety Phase

| Gate | Exact command | Result |
|---|---|---|
| Targeted mapped regressions | `cd graph && cargo test --features pg17 mmap_` | PASS: 13 passed |
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 656 passed, 1 ignored; doctests 0 |
| Rust docs | `cd graph && cargo doc --features pg17 --no-deps` | PASS |
| Rust doctests | `cd graph && cargo test --doc --features pg17` | PASS: 0 doctests |
| PostgreSQL-backed tests | `cd graph && cargo pgrx test --features "pg17 development" pg17` | PASS: 892 passed, 1 ignored; doctests 0 |
| Miri mapped-edge checks | `cd graph && env MIRIFLAGS='-Zmiri-tree-borrows -Zmiri-permissive-provenance' cargo +nightly miri test --features pg17 mmap_edge` | PASS: 4 passed |
| Miri mapped-node layout checks | `cd graph && env MIRIFLAGS='-Zmiri-tree-borrows -Zmiri-permissive-provenance' cargo +nightly miri test --features pg17 mmap_node` | PASS: 3 passed |
| Miri mapped-node accessor check | `cd graph && env MIRIFLAGS='-Zmiri-tree-borrows -Zmiri-permissive-provenance' cargo +nightly miri test --features pg17 mmap_metadata_lookups_reject_out_of_range_nodes` | PASS: 1 passed |

The default Stacked Borrows model reports inside `bitvec` pointer tagging during
mapped-to-owned materialization, and the broad `mmap_` filter also selects
OS-file tests that Miri isolation cannot unlink. The focused in-memory gates use
Tree Borrows and permissive provenance for the deliberate overflow-pointer
fixtures; they do not disable isolation or undefined-behavior checking.

### 2026-07-09 pgrx Error-Boundary Phase

| Gate | Exact command | Result |
|---|---|---|
| Red regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 graph_error_reporting_unwinds_before_postgres_error` before the fix | EXPECTED FAIL: wire SQLSTATE was `PG005`; the direct `errfinish()` path did not reach the destructor assertion |
| Targeted Rust classification | `cd graph && cargo test --features pg17 safety::tests` | PASS: 28 passed |
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 655 passed, 1 ignored; doctests 0 |
| PostgreSQL-backed tests | `cd graph && cargo pgrx test --features "pg17 development" pg17` | PASS: 892 passed, 1 ignored; doctests 0 |
| SQLSTATE/ACL boundary | `graph/tests/heavy/run_sqlstate_acl_boundary.sh` | PASS on PG17 |
| Documentation drift | `scripts/check_docs_drift.sh` | PASS |

### 2026-07-09 Durable Typed-Filter Phase

| Gate | Exact command | Result |
|---|---|---|
| Red PostgreSQL regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 sparse_typed_filters_survive_persisted_load_traverse_search_and_sync` before the lifecycle fix | EXPECTED FAIL: the exact text token reloaded but filter-only rows incorrectly tombstoned the updated node |
| Targeted ingest regressions | `cd graph && cargo test --features pg17 projection::ingest::tests` | PASS: 10 passed |
| Targeted PostgreSQL differential | `cd graph && cargo pgrx test --features "pg17 development" pg17 sparse_typed_filters_survive_persisted_load_traverse_search_and_sync` | PASS: exact values and prior-generation SQL NULL survive consecutive sync generations and fresh-backend reload |
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Independent Rust review | Separate `rust-reviewing` subagent over `8271565`, `63d8f4a`, and the RUST-00D worktree | Four RUST-00D findings fixed: watermark reference retention, borrowed pre-copy budget validation, dense/sparse node-range rejection, and malformed dictionary validation |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 667 passed, 1 ignored; doctests 0 |
| Rust docs | `cd graph && cargo doc --features pg17 --no-deps` | PASS |
| Rust doctests | `cd graph && cargo test --doc --features pg17` | PASS: 0 doctests |
| PostgreSQL-backed tests | `cd graph && cargo pgrx test --features "pg17 development" pg17` | PASS: 904 passed, 1 ignored; doctests 0 |
| Documentation drift | `scripts/check_docs_drift.sh` | PASS |

### 2026-07-09 Security-Definer Search-Path Phase

| Gate | Exact command | Result |
|---|---|---|
| Red metadata regression | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_metadata_rust_00e ./tests/heavy/function_metadata_audit.sh` before the attribute change | EXPECTED FAIL: all approved security-definer functions lacked `pg_proc.proconfig` search-path settings |
| Metadata regression | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_metadata_rust_00e ./tests/heavy/function_metadata_audit.sh` | PASS: every approved definer function has `search_path=pg_catalog, public` |
| Targeted compatibility | `cd graph && cargo pgrx test --features "pg17 development" pg17 pg_traverse_accepts_structured_jsonb_numeric_filters` | PASS |
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust docs | `cd graph && cargo doc --features pg17 --no-deps` | PASS |
| Rust doctests | `cd graph && cargo test --doc --features pg17` | PASS: 0 doctests |
| PostgreSQL-backed tests | `cd graph && cargo pgrx test --features "pg17 development" pg17` | PASS: 904 passed, 1 ignored; doctests 0 |
