# pgGraph 1.2 Release Program

> Active planning snapshot: 2026-08-20 on `dev`, resumed after the 1.1 release.

## Outcome

Ship the post-1.1 open-vocabulary relationship-type work as pgGraph 1.2.0 and
close the two evaluator findings that affect relationship-table onboarding:
safe junction discovery and a clean registration reset. PostgreSQL source
tables remain authoritative.

The unrelated post-1.1 batch-mutation phases P10-P14 are not part of this
release. P9 open-type closure is part of this release; its frozen evidence
protocol remains owned by
[`../measurements/2026-08-13-p9-open-type-query/`](../measurements/2026-08-13-p9-open-type-query/README.md).

## Phase Ledger

A phase closes only after implementation, focused regression coverage, public
supported-feature documentation, an independent Rust review, and a checkpoint
commit.

| Phase | Status | Exit gate |
|---|---|---|
| V12.1 relationship discovery | Complete | Schema-wide and targeted discovery produce one binary mapping for supported junction tables, infer conventional dynamic label columns, preserve exact endpoint identity, and avoid inventing endpoints from composite or multi-way foreign keys. |
| V12.2 registration reset | Not started | A backward-compatible reset option clears all selected-graph registration mappings without changing zero-argument projection-reset behavior, and stale-registration `PG000` recovery is covered. |
| V12.3 open-type closure | Not started | P9.5d retained Criterion, SQL latency, Linux resource, compatibility, and metadata outputs pass the precommitted budgets from one clean checkpoint; public docs match shipped behavior and limits. |
| V12.4 release closure | Not started | Version and upgrade surfaces name 1.2.0; install/upgrade, PostgreSQL 14-18, package, documentation, and release gates pass from the reviewed release commit. |

## V12.1 Evidence

- Red PostgreSQL regression: schema-wide discovery returned two mappings for a
  binary composite-key junction before the fix.
- `cargo +1.96.0 pgrx test --features "pg17 development" pg17 auto_discover`:
  13 passed after remediation, including dynamic `varchar(255)` labels,
  duplicate endpoint identities across tables, composite FKs, three-way
  junctions, repeated endpoint columns, and alternate unique-key references.
- `cargo +1.96.0 pgrx test --features "pg17 development" pg17 classify_as_junction`:
  5 passed.
- Strict Clippy, rustfmt, documentation drift, and `git diff --check` pass.
