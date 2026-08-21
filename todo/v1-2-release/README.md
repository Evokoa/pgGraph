# pgGraph 1.2 Release Program

> Completed planning snapshot: 2026-08-21 on `dev`, resumed after the 1.1 release.

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
| V12.4 release closure | Complete | Version and upgrade surfaces name 1.2.0; install/upgrade, PostgreSQL 14-18, package, documentation, and release gates pass from the reviewed release commit. |

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

## V12.4 Evidence

- Reviewed release checkpoint:
  `3e9fb41c8fb4e4e555533660e8c71a32c2aacf20`.
- The exact clean-source archive gate passed fresh package installation on
  PostgreSQL 14-18, the v0.1.8-to-v1.2.0 transition, the public quickstart, all
  40 CSR playground queries, and all 41 mutable-overlay playground queries.
- The exact packaged v1.1.0-to-v1.2.0 matrix passed on PostgreSQL 14-18. It
  verified pre-rebuild v6 artifact reads, owner and ACL preservation,
  registration-clearing reset, 259 exact dynamic relationship types,
  high-ID traversal, version-matched backup/restore, and rollback rebuilds.
- The Rust suite passed with 1,169 tests and three ignored tests. Strict
  Clippy, rustfmt, rustdoc/doctests, generated-SQL validation, public-doc drift,
  shell syntax, script inventory, and the unsafe allowlist all passed.
- Focused pgrx coverage for relationship discovery, reset/ACL behavior, and
  edge-type inventory passed. The release PR tier passed from the candidate
  source tree.
- A fresh independent Rust review of the complete release diff reported no
  findings. It also matched the generated `edge_types` and `reset(bool)` SQL
  attributes to the additive 1.1.0-to-1.2.0 upgrade script and verified that
  existing object identities, owners, and ACLs remain unchanged.
