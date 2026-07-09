# Full Graph Engine Progress

Last updated: 2026-07-09

## Current State

| Checkpoint | Status | Evidence / next action |
|---|---|---|
| 0. Freeze and measure | In progress | Static audit complete; add the ordered P0 regression pack below and machine-readable conformance baseline. |
| Rust type/unsafe/pgrx boundary | Review complete; blockers open | Execute RUST-00A through RUST-00F before expanding unsafe or claiming production readiness, then complete RUST-1 through RUST-8 by their owning checkpoints. |
| 1A. Security and identity | Not started | RLS topology, relationship identity, filter identity, savepoints. |
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

1. Out-of-range mapped node lookup and malformed CSR offset tests that exercise
   every safe accessor without OS mmap.
2. Custom SQLSTATE/error-boundary test proving Rust destructors unwind before
   PostgreSQL ERROR on every supported major.
3. Durable filter differential for signed, large, temporal, boolean, text,
   UUID, NULL, and tombstone values across sync, segment, and reload.
4. Security-definer shadow-schema/catalog path assertion and stable-relation-
   identity rename/search-path/drop-recreate tests.
5. Two-role GQL RLS test with `hydrate := false` for node, relationship, path,
   scalar identity, aggregate count, and existence.
6. Same-name filter columns on two registered tables.
7. Parallel same-type/same-endpoint edge rows with distinct PK/properties.
8. Two-backend concurrent projection publication.
9. Invalid staged persisted replacement preserving the previous generation.
10. Memory profiles for stale/no statistics, many filters, supernode LIMIT 1,
   auto-load, and compaction.

Do not implement broad syntax until these tests establish the current
correctness and safety boundary.

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
