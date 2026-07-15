# Full Graph Engine Progress

Last updated: 2026-07-12

The authoritative 1.0 scope now lives in
[`../v1-release/README.md`](../v1-release/README.md). This file continues to
track detailed full-engine implementation evidence; it does not make
PostgreSQL 19, full ISO GQL, competitive breadth, or dynamic graphs a 1.0
release blocker.

## Current State

| Checkpoint | Status | Evidence / next action |
|---|---|---|
| 0. Freeze and measure | In progress | Static audit complete; add the ordered P0 regression pack below and machine-readable conformance baseline. |
| Rust type/unsafe/pgrx boundary | KI-020 retired; RUST-00C through RUST-00F supported-major retirement pending | Mapped stores own validated ranges and pass Miri, full Rust ASan, PostgreSQL 14-18 regressions, and a PostgreSQL-process Valgrind gate. Graph errors unwind through pgrx, durable filter deltas preserve exact values, security-definer functions pin `pg_catalog`, and registered relations retain OID identity. |
| 1A. Security and identity | In progress | Coordinate-only node visibility, mapped single-pattern, supported join, and base wildcard relationship-row visibility, transaction-local relationship CREATE identity, trigger-sync edge identity, mutable-overlay segment relationship IDs, parallel-aware compaction and chunk replacement, base-CSR relationship multiplicity, table-qualified filter identity, persisted relationship identity sidecars, base-CSR query/path ID propagation, base relationship hydration, and savepoint overlays are enforced on PostgreSQL 17; broader writes and isolation coverage remain. |
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

- Preserved parallel source relationship rows through base CSR construction,
  GQL traversal, path identity, and base relationship hydration. PostgreSQL 17
  regressions verify two equal endpoint/type rows return two matches and hydrate
  separate edge-row IDs. Mapped single-pattern, supported join, and base
  wildcard relationship rows are checked under caller RLS before
  coordinate-only output. Transaction-local relationship `CREATE` overlays now
  preserve source identity for same-transaction visibility and hydration.
  Trigger-sync inserted relationship edges now intern source keys, persist those
  IDs in durable projection segments, and expose them through layered
  mutable-overlay neighbor reads. Compacted segment rows preserve those IDs
  when the row is representable by the current source/target/type/direction
  compaction key. Parallel-aware compaction, broader writes, and savepoints
  remain open.

- Added durable source primary-key metadata to relationship registrations and
  catalog fingerprints. Existing mappings backfill declared primary keys and
  fail closed when no stable relationship identity is available.

- Began the filter-identity migration by keying build-time filter registration
  and value assignment by PostgreSQL table OID plus attribute name, so
  same-named registered columns no longer overwrite one another during build.

- Carried table-qualified filter identity through structured pushdown and both
  sync replay paths; ambiguous user-facing names remain rejected until every
  legacy name-only helper is retired.

- Verified same-name filter ambiguity against PostgreSQL 17: unscoped
  structured filters remain rejected instead of selecting an arbitrary table,
  while internal build, pushdown, and sync lookups use relation-qualified keys.

- Independent 1A review follow-up: edge re-registration now updates by source
  and target relation OIDs, catalog fingerprints include all registered OIDs,
  and source-key metadata is checked against the live primary key before use.

- Relationship identity remains open outside the covered base, transaction, and
  trigger-sync segment paths: source keys and relationship IDs are durable and
  used for base GQL hydration, transaction-local creates, sync-published
  segment reads, representable compacted segment reads, and selected
  relationship-row visibility checks, but parallel-aware compaction, broader
  writes, savepoints, and complete relationship-row visibility still need
  end-to-end identity handling.

- Added durable relationship mapping IDs to the catalog. OID-first
  re-registration preserves the mapping surrogate, which will combine with the
  canonical source primary-key tuple when the CSR identity dictionary lands.

- Carried relationship mapping IDs and canonical source-key tuples through the
  bounded build spool in deterministic order. The next step is to intern these
  rows into per-adjacency CSR IDs and persist the dictionary.

- Interned build-spool identity pairs into per-adjacency in-memory CSR IDs,
  including generated reverse adjacencies. Artifact persistence and query use
  remain intentionally pending; no relationship behavior relies on the
  sidecar before reload support exists.

- Persisted relationship identity sidecars and their source-row dictionary in
  artifact format v4. Mmap loads validate sidecar length, reserved ID 0,
  nonzero dictionary entries, mapping IDs, and dictionary references before
  installing the graph. Query, hydration, RLS, overlays, and compaction still
  need to use these IDs before KI-015 can close.

- Carried base-CSR relationship IDs through GQL one-hop rows, path
  relationships, multi-pattern joins, and wildcard path deduplication.
  Overlay and layered neighbors intentionally expose no ID until their segment
  formats migrate or relationship hydration/RLS can fail closed.

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
   UUID, SQL NULL, and tombstone values across sync, segment v4, consecutive
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

- 2026-07-11 — Release planning phase: separated production-critical 1.0 work
  from the long-term engine roadmap and added compatibility, documentation,
  playground/script overhaul, and release-candidate plans.

- 2026-07-09 — Checkpoint 0 mapped-layout phase: made node metadata lookups fallible, validated mapped PK/CSR contents at crate-private constructors, and kept traversal/component corruption failures typed.
- 2026-07-09 — Checkpoint 0 error-boundary phase: replaced direct `errfinish()` FFI with pgrx stack unwinding, standard SQLSTATEs, stable `PGxxx` diagnostics, and a destructor regression.
- 2026-07-09 — Checkpoint 0 durable-filter phase: added projection segment tagged values, staged exact filter reload, consecutive-generation retention, and signed/temporal/text/UUID/NULL/tombstone regressions.
- 2026-07-09 — Checkpoint 0 RUST-00E security-definer phase: every approved definer function now has pgrx-generated `pg_catalog, public` `search_path` metadata, with a catalog audit regression and public security guidance; RUST-00F will remove the temporary public compatibility entry by storing relation identity as OIDs.
- 2026-07-09 — Checkpoint 0 RUST-00F relation-identity phase: registration, discovery, filtering, synchronization, and removal now retain PostgreSQL OIDs; catalog reads derive qualified SQL names from those OIDs, while public result labels remain compatible. Rename/search-path and drop/recreate behavior is covered on PG17.
- 2026-07-09 — Checkpoint 1A node-visibility subphase: GQL batches source-node visibility checks under the caller's PostgreSQL ACL/RLS context before returning coordinates, including `hydrate := false`; relationship-row visibility remains coupled to the pending durable relationship-identity work.
- 2026-07-09 — Independent three-phase review: fixed watermark-only artifact retention, pre-copy ingest budgeting, filter node-range validation, and malformed base dictionary fail-closed handling; the follow-up review and final gates were green.
- 2026-07-10 — Checkpoint 1A relationship-artifact subphase: `.pggraph` v4 now persists and validates per-adjacency relationship IDs plus the source-row identity dictionary, including reverse CSR reload preservation.
- 2026-07-10 — Checkpoint 1A sync/layered identity subphase: trigger-sync inserted edge rows now intern relationship source keys, durable segment edge rows store optional relationship IDs, and layered mutable-overlay reads preserve those IDs for GQL relationship values; compaction remains the next identity gap.
- 2026-07-10 — Checkpoint 1A compaction identity subphase: projection compaction initially preserved relationship IDs only for rows representable by the topology key.
- 2026-07-11 — Checkpoint 1A parallel compaction subphase: segment format v5, identity-aware layered keys, weighted rows, specific tombstones, materialization, and dirty-range chunk replacement now preserve identical endpoint/type/direction relationship rows independently.
- 2026-07-12 — Checkpoint 1A durable identity subphase: manifest version 2 now references a checksummed cumulative relationship-identity dictionary, reload validates segment IDs against it, and trigger replay covers standalone mapped relationship tables without requiring node registration.
- 2026-07-12 — Checkpoint 1A identity-delete subphase: transaction and immediate overlays now key tombstones by direction and optional relationship ID, GQL deletes carry the matched identity to PostgreSQL primary-key DML, and durable sync deletion preserves parallel siblings after reload.
- 2026-07-12 — Checkpoint 1A mutable-overlay visibility subphase: caller-role RLS checks now have durable post-build segment coverage across coordinate, hydrated, aggregate, and existence result shapes on PostgreSQL 17.
- 2026-07-12 — Checkpoint 1A PostgreSQL write-boundary subphase: GQL CREATE now has PostgreSQL 17 evidence for partition routing plus CHECK-constraint and user-trigger rejection without residual transaction deltas.
- 2026-07-13 — Checkpoint 1A isolation subphase: a two-session gate proves the existing query-freshness path observes a later committed mapped write under READ COMMITTED while REPEATABLE READ and SERIALIZABLE retain their transaction snapshot. Durable new-node ingestion defect KI-026 remains open for the storage phase.
- 2026-07-13 — Checkpoint 1A durable-node subphase: projection segment v6 stores exact primary-key and tenant identities, a non-serving planner allocates contiguous slots and same-batch endpoints, manifest loading stages node state atomically, direct ingest reloads the backend, cross-backend publication shares the graph advisory lock, and unrepresentable TRUNCATE fails without watermark advancement.
- 2026-07-13 — Checkpoint 1A persisted-rebuild subphase: repeated persisted mutable builds publish a monotonic base-only manifest bound to the replacement checksum and watermark, preserve operation timestamps, and retire superseded projection artifacts through generation-aware GC accounting.

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

### 2026-07-09 Checkpoint 1A filter/catalog gate

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" -- -D warnings` | PASS |
| PostgreSQL 17 suite | `cd graph && cargo pgrx test --features "pg17 development" pg17` | PASS: 908 passed, 1 ignored; doctests 0 |
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

### 2026-07-10 Relationship Identity Artifact Phase

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Compile | `cd graph && cargo check --features "pg17 development"` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Relationship identity roundtrip | `cd graph && cargo test --features pg17 persisted_mmap_load_preserves_relationship_identity_metadata` | PASS: 1 passed |
| Empty relationship source key | `cd graph && cargo test --features pg17 persisted_relationship_identity_allows_empty_source_key` | PASS: 1 passed |
| Malformed relationship dictionary | `cd graph && cargo test --features pg17 load_graph_file_rejects_empty_relationship_identity_slot` | PASS: 1 passed |
| Mmap persistence subset | `cd graph && cargo test --features pg17 persistence::tests::persisted_mmap_load` | PASS: 3 passed |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 671 passed, 1 ignored; doctests 0 |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-10 Sync and Layered Relationship Identity Phase

| Gate | Exact command | Result |
|---|---|---|
| Focused segment roundtrip | `cd graph && cargo test --features pg17 projection::segment::tests::delta_segment_roundtrips_edge_topology_weight_and_delete_sections` | PASS: 1 passed |
| Focused layered ID preservation | `cd graph && cargo test --features pg17 projection::layered::tests::layered_neighbors_preserve_segment_relationship_id` | PASS: 1 passed |
| Focused sync identity interning | `cd graph && cargo test --features pg17 sql_sync::tests::sync_relationship_identity_interning_uses_mapping_and_source_key` | PASS: 1 passed |
| Old segment rejection | `cd graph && cargo test --features pg17 projection::segment::tests::delta_segment_rejects_pre_typed_filter_format_version` | PASS: 1 passed |
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Whitespace | `git diff --check` | PASS |

### 2026-07-11 R1 Table-Qualified Filter Identity Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Same-name identity unit regression | `cd graph && cargo test --features pg17 same_named` | PASS: table OID plus column identity keeps equal display names distinct |
| Structured filter unit tests | `cd graph && cargo test --features pg17 sql_filters::tests` | PASS: 12 passed |
| PostgreSQL ambiguous-filter regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 traverse_rejects_ambiguous_raw_jsonb_filter_columns` | PASS: ambiguous unqualified public filters fail instead of choosing a table |
| PostgreSQL scoped sync regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 table_scoped_same_named_filters_stay_distinct_through_sync` | PASS: scoped traversal selects the owning table's value and sync updates only that table's same-named filter index |
| Independent Rust review | `rust-reviewing` subagent over filter lookup call sites and tests | PASS after adding the requested positive table-scoped build/query/sync regression; no production name-only lookup remains |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 685 passed, 1 ignored, doctests 0 |
| Contract and docs drift | `scripts/check_docs_drift.sh` | PASS |
| Whitespace | `git diff --check` | PASS |

### 2026-07-12 R1 Relationship Authorization Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Wildcard identity unit regression | `cd graph && cargo test --features pg17 wildcard_relationship_mapping` | PASS: mapped missing/mismatched identities fail closed while intentionally unmapped types remain supported |
| Wildcard binding metadata | `cd graph && cargo test --features pg17 wildcard_path_carries_edge_mapping_metadata` | PASS: mapped relationship type labels survive logical-to-physical lowering |
| PostgreSQL empty-result ACL preflight | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_join_and_wildcard_preflight_edge_acl_before_empty_results` | PASS: join and wildcard/EXISTS return SQLSTATE 42501 without edge-table SELECT even when predicates match no rows |
| Independent Rust review | `rust-reviewing` subagent over authorization diff | PASS after correcting the regression fixture to build an empty mapped topology and documenting conservative wildcard candidate ACL scope |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 686 passed, 1 ignored, doctests 0 |

### 2026-07-11 R1 Relationship Visibility Result Shapes

| Gate | Exact command | Result |
|---|---|---|
| Single-pattern RLS result shapes | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_coordinate_only_relationships_fail_closed_when_edge_row_is_not_visible` | PASS: coordinate, hydrated, aggregate, and existence output fail closed |
| Wildcard relationship-list RLS | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_wildcard_relationships_fail_closed_when_edge_row_is_not_visible` | PASS: path, relationship, and `relationships(path)` output fail closed |

| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 681 passed, 1 ignored; doctests 0 |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-10 Compaction Relationship Identity Phase

| Gate | Exact command | Result |
|---|---|---|
| Focused compaction ID preservation | `cd graph && cargo test --features pg17 projection::compact::tests::compaction_preserves_segment_relationship_ids` | PASS: 1 passed |
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Whitespace | `git diff --check` | PASS |

### 2026-07-11 R1 Savepoint Delta Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Transaction-delta unit tests | `cd graph && cargo test --features pg17 projection::tx_delta::tests` | PASS: 17 passed, including nested snapshot memory preflight |
| PostgreSQL savepoint lifecycle | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_tx_delta ./tests/heavy/tx_delta_lifecycle.sh` | PASS: rollback restores the prior overlay and release retains it, including lazy callback registration inside a savepoint |
| PostgreSQL-first GQL lifecycle | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_create_tx ./tests/heavy/gql_create_tx_lifecycle.sh` | PASS: nested savepoint abort/release and PL/pgSQL exception subtransactions keep source rows and graph overlays consistent |
| Independent Rust review | `rust-reviewing` subagent over savepoint callbacks and frames | PASS after charging snapshot copies before cloning, adding nested real-GQL and PL exception coverage, and clearing state at transaction prepare |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 685 passed, 1 ignored, doctests 0 |
| Contract and docs drift | `scripts/check_docs_drift.sh` | PASS |
| Whitespace | `git diff --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 682 passed, 1 ignored; doctests 0 |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-10 Relationship ID Query Propagation Phase

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Compile | `cd graph && cargo check --features "pg17 development"` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| One-hop relationship ID propagation | `cd graph && cargo test --features pg17 executor_propagates_relationship_ids_into_rows_and_paths` | PASS: 1 passed |
| Wildcard parallel relationship identity | `cd graph && cargo test --features pg17 wildcard_path_executor_preserves_parallel_relationship_ids` | PASS after red regression confirmed endpoint-only dedup collapsed `[Some(41), Some(42)]` to `[Some(41)]` |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 673 passed, 1 ignored, doctests 0 |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-10 Relationship Hydration Identity Phase

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Compile | `cd graph && cargo check --features "pg17 development"` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Relationship identity validation | `cd graph && cargo test --features pg17 relationship_identity_validation_rejects_missing_and_wrong_mapping` | PASS: 1 passed |
| Relationship source-key predicate | `cd graph && cargo test --features pg17 relationship_source_key_predicate_uses_registered_primary_key_columns` | PASS: 1 passed |
| PostgreSQL parallel hydration regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_preserves_parallel_source_relationship_rows` | PASS: 1 passed; verifies equal endpoint/type rows hydrate `f1,f_parallel` instead of repeating one endpoint match |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 675 passed, 1 ignored, doctests 0 |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-10 Relationship Row Visibility Phase

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Compile | `cd graph && cargo check --features "pg17 development"` | PASS |
| Relationship identity validation | `cd graph && cargo test --features pg17 relationship_identity_validation_rejects_missing_and_wrong_mapping` | PASS: 1 passed |
| PostgreSQL relationship RLS regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_coordinate_only_relationships_fail_closed_when_edge_row_is_not_visible` | PASS: 1 passed; verifies `hydrate := false` and `hydrate := true` both fail closed when edge-table RLS hides the mapped relationship row |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 675 passed, 1 ignored, doctests 0 |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-10 Join Relationship Row Visibility Phase

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Compile | `cd graph && cargo check --features "pg17 development"` | PASS |
| Join mapping metadata regression | `cd graph && cargo test --features pg17 multi_pattern_join_carries_edge_mapping_metadata` | PASS: 1 passed |
| PostgreSQL join relationship RLS regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_join_relationships_fail_closed_when_edge_row_is_not_visible` | PASS: 1 passed; verifies `hydrate := false` and `hydrate := true` both fail closed when edge-table RLS hides a mapped relationship row returned through a supported join |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 676 passed, 1 ignored, doctests 0 |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Whitespace | `git diff --check` | PASS |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-10 Wildcard Relationship Row Visibility Phase

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Compile | `cd graph && cargo check --features "pg17 development"` | PASS |
| Wildcard mapping metadata regression | `cd graph && cargo test --features pg17 wildcard_path_carries_edge_mapping_metadata` | PASS: 1 passed |
| PostgreSQL wildcard relationship RLS regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_wildcard_relationships_fail_closed_when_edge_row_is_not_visible` | PASS: 1 passed; verifies `hydrate := false` and `hydrate := true` both fail closed when edge-table RLS hides a mapped relationship row returned through a wildcard path |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 677 passed, 1 ignored, doctests 0 |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Independent Rust review | `rust-reviewing` subagent over the join, wildcard, and transaction overlay identity phases | ATTEMPTED: subagent stalled without findings and was interrupted; local review found no blockers |
| Whitespace | `git diff --check` | PASS |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-10 Transaction Overlay Relationship Identity Phase

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Compile | `cd graph && cargo check --features "pg17 development"` | PASS |
| PostgreSQL same-transaction relationship hydration regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_create_relationship_inserts_edge_row_and_records_delta` | PASS: 1 passed; verifies a mapped relationship created through `graph.gql()` can be read and hydrated later in the same transaction through the transaction-local overlay |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 677 passed, 1 ignored, doctests 0 |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Whitespace | `git diff --check` | PASS |
| Documentation drift | `scripts/check_docs_drift.sh` | FAIL: pre-existing missing inline path references to `graph/fuzz/target/` in `docs/contributor_guide/scripts.mdx` |

### 2026-07-11 R0 Release Contract Phase

| Gate | Exact command | Result |
|---|---|---|
| Generated SQL contract | `cd graph && cargo pgrx schema --features pg17 --out /tmp/pggraph-schema.sql`; `python3 scripts/check_release_contract.py --schema-file /tmp/pggraph-schema.sql` | PASS: 155 schema blocks and 153 function contracts match the frozen baseline |
| Contract and docs drift | `scripts/check_docs_drift.sh` | PASS |
| Alpha-to-1.0 fixture | `DBNAME=pggraph_alpha_gate graph/tests/heavy/alpha_to_v1_fixture.sh all-current` | PASS: source checksum, 3 nodes, 2 edges, and 2-row traversal verified in a disposable PostgreSQL 17 database |
| Secret scan | `scripts/check_secrets.sh changes` | PASS |
| Python syntax | `PYTHONPYCACHEPREFIX=/tmp/pggraph-pycache python3 -m py_compile scripts/check_release_contract.py` | PASS |
| Shell syntax | `bash -n graph/tests/heavy/alpha_to_v1_fixture.sh graph/tests/heavy/run_release_gate.sh` | PASS |
| Independent release review | `rust-reviewing` subagent over the R0 diff | PASS: no release-blocking issue remains |
| Whitespace | `git diff --check` | PASS |

### 2026-07-11 R1 Parallel Relationship Compaction Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 684 passed, 1 ignored, doctests 0 |
| Compaction regressions | `cd graph && cargo test --features pg17 compaction_` | PASS: 16 passed; parallel identities, weights, and identity-specific deletion survive compaction |
| Segment regressions | `cd graph && cargo test --features pg17 projection::segment::tests` | PASS: 12 passed; segment v5 identity sidecars round-trip |
| Layered-store regressions | `cd graph && cargo test --features pg17 projection::layered::tests` | PASS: 13 passed |
| Chunk rewrite regressions | `cd graph && cargo test --features pg17 projection::chunk::tests` | PASS: 7 passed; dirty-range rewrites retain relationship IDs |
| Independent Rust review | `rust-reviewing` subagent over the R1 compaction diff | PASS after adding the requested forced dirty-range chunk rewrite regression for parallel IDs, weights, and an identity tombstone |
| Contract and docs drift | `scripts/check_docs_drift.sh` | PASS |
| Whitespace | `git diff --check` | PASS |

### 2026-07-12 R1 Durable Relationship Identity Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 694 passed, 1 ignored, doctests 0 |
| Layered reload regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_relationship_expansion_uses_layered_manifest_snapshot` | PASS: unit and PostgreSQL-backed cases |
| Parallel identity reload regression | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_ingested_parallel_relationship_identity_survives_reload` | PASS: standalone relationship trigger, two equal-topology rows, fresh-backend hydration IDs `f1,f_parallel` |
| No-FK relationship replay | `cd graph && cargo pgrx test --features "pg17 development" pg17 standalone_relationship_sync_without_fk_uses_registered_endpoint_fallback` | PASS: endpoint resolution falls back across registered node tables without physical FK metadata |
| Rust documentation | `cd graph && cargo doc --features pg17 --no-deps` | PASS |
| Independent Rust review | `rust-reviewing` subagent over the durable identity diff and follow-up fixes | PASS after closing transient dictionary publication, duplicate interning, empty-key compatibility, no-FK replay, bounded artifact reads, and failed-validation cleanup findings |
| Contract and docs drift | `scripts/check_docs_drift.sh`; `scripts/check_release_contract.py` | PASS after acknowledged GQL implementation hash refresh |
| Whitespace | `git diff --check` | PASS |

### 2026-07-12 R1 Identity-Specific Relationship Delete Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Formatting | `cd graph && cargo fmt --check` | PASS |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 697 passed, 1 ignored, doctests 0 |
| GQL delete regressions | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_delete` | PASS: 8 existing delete cases |
| Parallel durable deletion | `cd graph && cargo pgrx test --features "pg17 development" pg17 synced_parallel_relationship_delete_preserves_sibling_through_reload` | PASS: direct PostgreSQL deletion of one equal-topology row preserves and hydrates the sibling after ingestion and reload |
| Composite identity deletion | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_delete_relationship_with_composite_source_key` | PASS |
| RLS denial | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_delete_rls_denial_keeps_source_row_and_projection` | PASS: denied PostgreSQL DELETE leaves both source row and projection visible |
| Bidirectional reverse deletion | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_delete_edge_handles_bidirectional_reverse_match` | PASS |
| Independent Rust review | `rust-reviewing` subagent over the identity-delete diff and follow-up fixes | PASS after preserving wildcard tombstones beside identified inserts and adding composite-key and two-role RLS gates |
| Rust documentation | `cd graph && cargo doc --features pg17 --no-deps` | PASS |

### 2026-07-12 R1 Mutable-Overlay Relationship RLS Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| PostgreSQL 17 two-role RLS | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_mutable_overlay_relationships_fail_closed_under_rls` | PASS: one post-build durable relationship is hidden from coordinate, hydrated, aggregate, and existence output |

### 2026-07-12 R1 PostgreSQL Write-Boundary Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Partition/constraint/trigger | `cd graph && cargo pgrx test --features "pg17 development" pg17 gql_create_respects_partition_routing_constraints_and_user_triggers` | PASS: accepted row lands in a child partition; CHECK and user-trigger failures leave no source row or transaction delta |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 697 passed, 1 ignored, doctests 0 |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust documentation | `cd graph && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features "pg17 development"` | PASS |
| Contract and docs drift | `scripts/check_release_contract.py`; `scripts/check_docs_drift.sh` | PASS after acknowledged PostgreSQL write-boundary contract refresh |
| Independent Rust review | `rust-reviewing` subagent over the write-boundary diff and SQLSTATE follow-up | PASS: exact CHECK and trigger SQLSTATEs, rejected-state cleanup, partition routing, documentation, and matrix pin are release-ready |
| Whitespace | `git diff --check` | PASS |

### 2026-07-13 R1 Transaction Isolation Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Two-session isolation matrix | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_isolation ./tests/heavy/gql_isolation_matrix.sh` | PASS: READ COMMITTED source/GQL counts advance from 0 to 1; REPEATABLE READ and SERIALIZABLE remain 0 inside their snapshots; every row is visible after the reader transaction ends |
| Persisted isolation | `cd graph && PERSIST_ON_BUILD=on PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_isolation_persisted_ki027 ./tests/heavy/gql_isolation_matrix.sh` | PASS: durable new-node identity, reload, and repeated builds preserve source/projection agreement under READ COMMITTED, REPEATABLE READ, and SERIALIZABLE |
| Rust tests | `cd graph && cargo test --features pg17` | PASS: 697 passed, 1 ignored, doctests 0 |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust documentation | `cd graph && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features "pg17 development"` | PASS |
| Shell syntax | `bash -n graph/tests/heavy/gql_isolation_matrix.sh graph/tests/heavy/run_release_gate.sh` | PASS; ShellCheck is not installed in the current environment and remains for the aggregate script gate |
| Contract and docs drift | `scripts/check_release_contract.py`; `scripts/check_docs_drift.sh` | PASS after acknowledged transaction-isolation implementation hash refresh |
| Independent Rust review | `rust-reviewing` subagent over the isolation gate, orchestration, and public claims | PASS after removing a redundant runtime SPI refresh, replacing timing sleeps with advisory handshakes, adding same-reader post-commit checks, and narrowing persisted-path claims to KI-026 |
| Whitespace | `git diff --check` | PASS |

### 2026-07-13 R1 Durable Node Identity Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Rust unit suite | `cd graph && cargo test --features pg17 --lib --no-fail-fast` | PASS: 710 passed, 1 ignored |
| Persisted lifecycle | `cd graph && cargo pgrx test --features "pg17 development" ingest_projection_publishes_committed_sync_log_rows` | PASS: clean persisted-snapshot allocation, Unicode composite identities, typed filters, tenant-only slot preservation, standalone relationship tables, later-in-batch endpoints, direct reload, and second-batch allocation |
| TRUNCATE fail-closed | `cd graph && cargo pgrx test --features "pg17 development" ingest_projection_rejects_truncate_without_advancing_watermark` | PASS: SQLSTATE 0A000 with rebuild guidance; watermark unchanged |
| Standalone endpoint lifecycle | `cd graph && cargo pgrx test --features "pg17 development" ingest_projection_rejects_standalone_endpoint_identity_changes` | PASS: a deferred-FK endpoint delete/reinsert fails with SQLSTATE 0A000 and rebuild guidance without advancing the watermark |
| Standalone no-FK fallback | `cd graph && cargo pgrx test --features "pg17 development" standalone_relationship_sync_without_fk_uses_registered_endpoint_fallback` | PASS: unique registered endpoint fallback remains supported for inserts |
| Cross-backend publication and writer barrier | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_build_lock_ki026 ./tests/heavy/build_lock_regression.sh` | PASS: build, vacuum, ingest, and active-writer contention fail closed; legacy trigger definitions require refresh; a same-transaction write cannot ingest before commit; apply statistics match the exact published batch; out-of-order commit and rollback cannot skip a sync ID; the 20,001-row publisher wins with an empty retry |
| Persisted isolation | `cd graph && PERSIST_ON_BUILD=on PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_isolation_persisted_ki027 ./tests/heavy/gql_isolation_matrix.sh` | PASS: repeated persisted builds publish manifests matching the replacement base checksum while isolation visibility remains correct |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust documentation | `cd graph && cargo test --doc --features pg17`; `cd graph && RUSTDOCFLAGS="-D warnings" cargo doc --features pg17 --no-deps` | PASS: doctests 0; rustdoc warning-free |
| Contract and docs drift | `scripts/check_release_contract.py`; `scripts/check_docs_drift.sh` | PASS |
| Shell syntax | `bash -n graph/tests/heavy/build_lock_regression.sh graph/tests/heavy/gql_isolation_matrix.sh` | PASS |
| Independent Rust review | `rust-reviewing` subagent over KI-026 and follow-up compatibility fixes | PASS: no blockers after trigger-upgrade preflight, fail-fast lock ordering, same-transaction guard coverage, exact endpoint resolution, standalone lifecycle protection, and apply-stat consistency fixes |

### 2026-07-13 R1 Persisted Rebuild Manifest Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Rebase and recovery unit tests | `cd graph && cargo test --features pg17 projection::recovery::tests --lib` | PASS: repeated rebuild checksum/watermark rebasing, generation linkage, corrupt-segment obsolete accounting, base-only reload, corrupt-manifest recovery, and targeted chunk repair |
| Full rebuild repair | `cd graph && cargo pgrx test --features "pg17 development" full_rebuild_restores_valid_projection_generation` | PASS: unreadable generation is superseded by the expected monotonic generation and the backend reloads it |
| Targeted chunk repair | `cd graph && cargo pgrx test --features "pg17 development" projection_repair_rewrites_corrupt_base_chunk_generation` | PASS |
| Persisted isolation | `cd graph && PERSIST_ON_BUILD=on PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_isolation_persisted_ki027_final ./tests/heavy/gql_isolation_matrix.sh` | PASS: durable new-node ingestion and repeated builds preserve source/projection isolation agreement |
| Build/vacuum writer barrier | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_build_lock_ki027c ./tests/heavy/build_lock_regression.sh` | PASS: active registered writers reject persisted build, vacuum, and ingestion with 55P03; commit/rollback and exact apply-count assertions remain green |
| Rust unit suite | `cd graph && cargo test --features pg17 --lib --no-fail-fast` | PASS: 711 passed, 1 ignored |
| Clippy | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings` | PASS |
| Rust documentation | `cd graph && cargo test --doc --features pg17`; `cd graph && RUSTDOCFLAGS="-D warnings" cargo doc --features "pg17 development" --no-deps` | PASS: doctests 0; rustdoc warning-free |
| Contract, docs, shell syntax, and whitespace | `scripts/check_release_contract.py`; `scripts/check_docs_drift.sh`; `bash -n graph/tests/heavy/build_lock_regression.sh graph/tests/heavy/gql_isolation_matrix.sh`; `git diff --check` | PASS |
| Independent Rust review | `rust-reviewing` subagent over KI-027 and concurrency/recovery follow-ups | PASS after serializing vacuum with active writers, acquiring the build lock before repair planning, preserving corrupt-generation artifact accounting, and correcting the public recovery description |

### 2026-07-14 R1 Security-Definer Search-Path Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Full PostgreSQL 17 suite | `cd graph && cargo pgrx test --features "pg17 development" pg17` | PASS: 970 passed, 1 ignored; doctests 0 |
| Function metadata | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_metadata_ki023_checkpoint ./tests/heavy/function_metadata_audit.sh` | PASS: extension and generated sync definers pin `pg_catalog, pg_temp` |
| Publication and writer barrier | `cd graph && PG_VERSION_FEATURE=pg17 DBNAME=pggraph_build_lock_ki023 ./tests/heavy/build_lock_regression.sh` | PASS: stale triggers fail closed, out-of-order commit/rollback horizons cannot skip sync IDs, and concurrent publishers remain serialized |
| Security regressions | Full PostgreSQL 17 suite cases `generated_sync_definers_pin_search_path_and_ignore_shadow_functions`, `temporary_catalog_shadow_cannot_bypass_graph_admin_check`, and `persistent_function_and_operator_shadows_cannot_bypass_graph_admin_check` | PASS |
| Node-backed relationship write boundary | `cd graph && cargo test --features "pg17 development" binder_rejects_create_for_node_backed_foreign_key_relationship` | PASS: readable relationship identity does not advertise standalone-row writes |
| Scheduler rebuild priority | Full-suite case `scheduled_maintenance_prioritizes_rebuild_over_durable_ingest` plus unit decision coverage | PASS: pending durable rows never ingest against a rebuild-required or read-only graph; maintenance starts first, and scheduler-only durable catch-up reports applied work |
| Clippy and Rust documentation | `cd graph && cargo clippy --features "pg17 development" --all-targets -- -D warnings`; `cd graph && RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps --features "pg17 development"` | PASS |
| Release contract and documentation drift | `python3.12 scripts/check_release_contract.py`; `scripts/check_docs_drift.sh` | PASS after acknowledged `KI-023` SQL metadata and GQL implementation hash refresh |
| Secret and whitespace checks | `scripts/check_secrets.sh`; `git diff --check` | PASS |
| Independent Rust review | `rust-reviewing` subagent over KI-023 plus full-suite regression fixes | PASS after restoring authoritative sync-log reads under the production writer barrier, keeping direct ingestion trigger audits strict, separating node-backed read identity from standalone-row writes, and refreshing the frozen 1.0 SQL contract |

### 2026-07-14 R1 Owning Mapped-Layout Checkpoint

| Gate | Exact command | Result |
|---|---|---|
| Rust unit suite | `cd graph && cargo test --features pg17 --lib --no-fail-fast` | PASS: 718 passed, 1 ignored |
| PostgreSQL 17 suite | `cd graph && cargo pgrx test --features "pg17 development"` | PASS: 975 passed, 1 ignored; doctests 0 |
| AddressSanitizer | `cd graph && RUN_ASAN=1 RUN_PGRX=0 RUN_PGBENCH=0 ./tests/heavy/run_memory_sanitizers.sh` | PASS: 718 passed, 1 ignored |
| Mapped soundness | Focused Miri plus source-inode truncation, drop-order, malformed-layout, alignment, and out-of-range regressions | PASS; snapshot bytes remain valid and are counted as backend-private memory |
| Independent Rust review | `rust-reviewing` subagent over KI-020 and follow-up fixes | PASS after replacing file-backed typed views with an anonymous read-only snapshot and correcting accounting, portability, predicate-boundary, and lifecycle documentation |
| PostgreSQL-process sanitizer | `PG_VERSIONS=17 RUN_PGRX_SQL=0 RUN_GQL_WRITE_MATRIX=0 RUN_POSTGRES_SANITIZER=1 ./tests/heavy/run_pg_matrix_docker.sh` | PASS: PostgreSQL 17.10 postmaster and child processes completed persisted mmap, corruption, callback, guarded-error, and build-job paths with zero unsuppressed Valgrind errors |
| Supported-major mapped regressions | PostgreSQL 14-18 Docker matrix | PASS: 718 release Rust tests and 981 pgrx tests per supported major; KI-020 retired |
| Supported-major durable lifecycle and locks | PostgreSQL 14-18 Docker durable projection matrix | PASS: cross-backend lifecycle and publication/writer lock profiles passed on every supported major; KI-026 retired |
| Supported-major relation identity and R1 safety closure | PostgreSQL 14-18 Docker durable projection matrix plus the preceding 981-test pgrx matrix per major | PASS: concurrent rename/drop-recreate serialization, OID-stable rename, fail-closed recreation, mapped validation, guarded errors, exact durable values, ACL/RLS identity, filters, and definer metadata; KI-014 through KI-017 and KI-021 through KI-024 retired |
| R1 safety closure independent review | `rust-reviewing` subagent over the raw checkpoint diff | PASS after tracking the active DDL coordination key and adding bounded TERM/KILL client cleanup |
