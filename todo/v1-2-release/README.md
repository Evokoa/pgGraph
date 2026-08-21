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
| V12.2 registration reset | Complete | A backward-compatible reset option clears all selected-graph registration mappings without changing zero-argument projection-reset behavior, and stale-registration `PG000` recovery is covered. |
| V12.3 open-type closure | Complete | P9.5d retained Criterion, SQL latency, Linux resource, compatibility, and metadata outputs pass the precommitted budgets from one clean checkpoint; public docs match shipped behavior and limits. |
| V12.4 release closure | In progress | Version and upgrade surfaces name 1.2.0; install/upgrade, PostgreSQL 14-18, package, documentation, and release gates pass from the reviewed release commit. |

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

## V12.2 Evidence

- `cargo +1.96.0 pgrx test --features "pg17 development" pg17 reset`:
  2 passed, including stale-OID cleanup and zero-argument compatibility.
- `cargo +1.96.0 pgrx test --features "pg17 development" pg17 graph_catalog_mutation_requires_admin_privileges`:
  the focused ACL regression passed with SQLSTATE `42501` for
  `graph.reset(true)` under a restricted role.
- The generated release contract contains exactly one added SQL object,
  `graph.reset(bool)`, with no removed objects; the contract and documentation
  drift checks pass.
- Strict Clippy, rustfmt, and `git diff --check` pass.
- A fresh independent Rust review reported no findings after the ACL,
  generated-contract, and executable-quickstart remediations.

## V12.3 Evidence

- Exact measurement commit:
  `a07b662523180685af3bc65cbb56921a38fe21ec`.
- The P9 reconciler verified ancestry and raw-to-summary integrity, and passed
  every budget frozen at `dd730b8`: 65 Criterion cases, eight PostgreSQL
  latency cases, and Linux resource runs for 1, 4, and 8 backends.
- The 65,536-label Linux fixture retained one real PostgreSQL PID per backend
  with nonzero PSS. Its 8-to-1 total and baseline-subtracted query PSS ratios
  were 5.86x and 6.36x against 9.0x limits; its immutable artifact used 54.7
  bytes per directed edge against a 64-byte limit.
- The resource image is commit-labeled and built from the retained
  source-archive digest. Query samples require every exact backend PID to be
  active in the marked traversal statement.
- The four retained-evidence Rust contracts are active rather than ignored and
  re-run the lineage, case-matrix, raw-summary, and budget checks.
