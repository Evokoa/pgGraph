# pgGraph 1.0 Release Program

This directory is the single source of truth for work required to publish
pgGraph 1.0. The broader full-engine plans remain useful design references,
but an item is a 1.0 blocker only when it appears here.

## Release Contract

pgGraph 1.0 is a production release of the documented PostgreSQL-first graph
engine on PostgreSQL 14 through 18. It stabilizes the existing SQL API, the
explicitly documented GQL profile, projection storage and synchronization,
operational tooling, packaging, and upgrade behavior. PostgreSQL tables remain
the source of truth for every durable mutation.

Version 1.0 does not promise full ISO GQL conformance, PostgreSQL 19 SQL/PGQ,
competitive analytics breadth, WAL-driven synchronization, semantic-guided
search, distributed execution, or schema-flexible dynamic graphs. Those items
remain visible on the public roadmap without blocking this release.

## Ordered Work

| Phase | Scope | Exit condition | Status |
|---|---|---|---|
| R0 | Freeze the 1.0 product and compatibility contract | Supported surfaces, compatibility rules, deprecation policy, and release evidence format are approved and documented. | In progress |
| R1 | Correctness, security, identity, and transactions | No open P0 correctness/security issue; RLS, parallel relationships, filter identity, savepoints, and supported-major regressions are green. | In progress |
| R2 | Resource containment and safe publication | Build, load, query, sync, and compaction stay within enforced limits; failed or competing publication preserves the last good generation. | Planned |
| R3 | Bounded storage, build, sync, and compaction | Production-scale operations spill, mmap, or fail predictably; snapshot/watermark and compaction behavior is crash-safe and equivalent. | Planned |
| R4 | Release-risk refactoring | High-risk boundaries are typed and isolated; playground, scripts, and release orchestration have clear ownership without changing public behavior. | Planned |
| R5 | Supported query and write profile | Every advertised SQL/GQL feature has conformance, security, transaction, resource, diagnostic, and documentation evidence. Unsupported syntax fails deterministically. | Planned |
| R6 | Documentation, packaging, CI, and operations | Public docs are coherent; PG14-18 install/upgrade/package matrix and required CI/release gates are green. | Planned |
| R7 | Release candidate and 1.0 publication | Clean install, upgrade, rollback, backup/restore, crash, performance, and artifact evidence is archived; no release-blocking issue remains. | Planned |

Phases are completed in order. Work may be prepared in parallel, but a phase
does not close until its exit evidence is recorded in `todo/progress.md`.

## Required Plans

- [Compatibility and lifecycle](./01-compatibility-and-lifecycle.md)
- [Production readiness](./02-production-readiness.md)
- [Documentation and public API](./03-documentation-and-api.md)
- [Playground and scripts overhaul](./04-playground-and-scripts.md)
- [Release candidate checklist](./05-release-candidate-checklist.md)

## Scope Rules

- A 1.0 blocker protects documented behavior, data, security, bounded resource
  use, installation, upgrades, rollback, or operability.
- Refactoring is release work only when it removes a demonstrated release risk
  or makes a required gate maintainable. Broad architectural cleanup stays on
  the roadmap.
- New syntax or features do not enter 1.0 after R0 unless they close a security,
  correctness, compatibility, or operations gap.
- Conditional ideas must not linger in this directory. Either make them a
  release requirement with an evidence gate or move them to the roadmap.
- Every completed phase updates public docs, the public roadmap or known issues
  when applicable, and the measurement log.

## Relationship To Existing Plans

The detailed documents under `todo/full-graph-engine/` remain technical input.
For 1.0, their checkpoints map as follows:

| Full-engine checkpoint | 1.0 disposition |
|---|---|
| 0, 1A | Required in R1. |
| 1B, 1C | Required in R2. |
| 2, 3 | Required in R3, limited to bounded production behavior and recovery. |
| 4 | Only release-risk type, pgrx, unsafe, ownership, playground, and script refactors are required in R4. The broader decomposition continues after 1.0. |
| 5 | Required only where needed to bound and correctly execute the advertised 1.0 query profile. General cost-based planning continues after 1.0. |
| 6 | Required for the advertised 1.0 GQL profile, not full ISO GQL. Full conformance remains on the roadmap. |
| 7, 8, 10 | Post-1.0 roadmap. |
| 9 | Recast as R6 and R7: close the public 1.0 backlog and archive release evidence. |
