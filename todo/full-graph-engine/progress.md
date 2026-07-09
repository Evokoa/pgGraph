# Full Graph Engine Progress

Last updated: 2026-07-09

## Current State

| Checkpoint | Status | Evidence / next action |
|---|---|---|
| 0. Freeze and measure | In progress | Static audit complete; add five P0 reproductions and machine-readable conformance baseline. |
| 1A. Security and identity | Not started | RLS topology, relationship identity, filter identity, savepoints. |
| 1B. Memory containment | Partial mitigation | Commit `8fea899` reduces old/new rebuild overlap; hard governor and query/load/compaction controls remain. |
| 1C. Safe publication | Not started | Add cross-backend lock/CAS and validate before switch. |
| 2. Artifact vNext/out-of-core | Planned | Focused predecessor is `todo/out-of-core-build-plan.md`. |
| 3. Bounded load/sync/compaction | Not started | mmap inbound/filter data and range compaction. |
| 4. Refactor foundations | Not started | Begin after P0 tests/owners are fixed. |
| 5. Streaming costed runtime | Not started | Canonical IR and resource governor are prerequisites. |
| 6. Full GQL slices | Not started | Conformance registry is prerequisite. |
| 7. PostgreSQL 19 SQL/PGQ | Planned | Research current native catalog/planner boundary. |
| 8. Competitive program | Planned | Requires correctness and reproducible baseline. |

## Completed This Review

- Reviewed roadmap, known issues, TODO history, memory model, build/load,
  projection, GQL, query execution, hydration, and major refactor hotspots.
- Committed pre-existing low-memory rebuild mitigation as `8fea899`.
- Verified formatting, warnings-denied clippy, 652 Rust tests (1 ignored),
  targeted build tests, and targeted PostgreSQL-backed low-memory/GUC tests.
- Created the full-engine program plans in this folder.
- Updated public Roadmap and Known Issues to reflect P0 reality and full GQL.

## Next Executable Checkpoint

Checkpoint 0 regression pack, in this order:

1. Two-role GQL RLS test with `hydrate := false` for node, relationship, path,
   scalar identity, aggregate count, and existence.
2. Same-name filter columns on two registered tables.
3. Parallel same-type/same-endpoint edge rows with distinct PK/properties.
4. Two-backend concurrent projection publication.
5. Invalid staged persisted replacement preserving the previous generation.
6. Memory profiles for stale/no statistics, many filters, supernode LIMIT 1,
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

## Measurement Log

Add dated entries with dataset shape, PostgreSQL/pgGraph settings, exact command,
peak RSS/PSS, spill, elapsed time, result checksum, and pass/fail threshold.
No benchmark result should be recorded without its correctness checksum and
environment.
