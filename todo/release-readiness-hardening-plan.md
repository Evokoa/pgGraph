# Release Readiness Hardening Plan

## Objective

Bring the current `feat/mutable-graph-projections` HEAD back to release-ready
shape by resolving the static-check blockers, updating stale dependency pins
through the approved safety wrapper flow, and rerunning the release gates that
matter for the GQL mutable projection stack.

This plan is scoped to release hardening. It should not introduce new public GQL
behavior, new crates, new services, or new architectural boundaries.

## Current Status

Release-ready as of 2026-06-06 after the post-bincode `pg17` release gate.

Passing local checks:

- `cargo fmt --check` from `graph/`
- `cargo clippy --features pg17 --all-targets -- -D warnings` from `graph/`
- `scripts/check_docs_drift.sh` from repository root
- `cargo test --features pg17` from `graph/`: 516 passed, 1 ignored
- `cargo doc --features pg17 --no-deps` from `graph/`
- `git diff --check origin/feat/mutable-graph-projections..HEAD`
- `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_release_build_lock ./tests/heavy/build_lock_regression.sh` from `graph/`
- `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_release_gql_set_tx ./tests/heavy/gql_set_tx_lifecycle.sh` from `graph/`
- `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_release_gql_delete_tx ./tests/heavy/gql_delete_tx_lifecycle.sh` from `graph/`
- `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_release_gql_merge_race ./tests/heavy/gql_merge_race.sh` from `graph/`
- `PG_VERSION_FEATURE=pg17 ./tests/heavy/run_release_gate.sh` from `graph/`: passed after the bincode 2 metadata-format migration

Deferred non-blocking items:

- `nixpkgs` and `rust-overlay`: still stale, but blocked locally because `nix`
  is not installed. This deferral is tracked in `docs/known-issues.mdx`.

Earlier `v0.1.5` release-gate evidence is not sufficient for this HEAD; use the
checks recorded above and in `todo/measurements.md`.

## Constraints

- Use `sfw` for package-manager dependency changes and downloads where a
  package manager introduces or refreshes dependency artifacts.
- Keep dependency versions single-sourced in their existing manifests.
- Do not hide clippy failures with broad lint allowances. Refactor to make the
  function boundaries clearer unless a narrow, justified local allowance is
  more honest than a fake abstraction.
- Preserve persisted artifact compatibility unless a deliberate format-version
  bump and rebuild path are documented.
- Treat PostgreSQL and pgrx boundaries as release-sensitive. pgrx crate updates
  require matching `cargo-pgrx`, Docker, README, and install docs review.
- Record completed verification in `todo/measurements.md`.

## Phase 1: Fix Static-Check Blockers

### 1.1 Collapse GQL Wildcard Expansion Argument Lists

Current clippy blockers:

- `graph/src/query/execute.rs:497`
- `graph/src/query/execute.rs:597`

Plan:

- Introduce a small internal context struct for wildcard path expansion inputs
  that travel together, for example engine, neighbor view, physical plan,
  tenant, path variable table, and row cap.
- Keep mutable traversal state separate from immutable execution context.
- Collapse the nested endpoint `if` without changing traversal semantics.

Required checks:

- `cargo test --features pg17 query::tests::wildcard_path_`
- `cargo test --features pg17 query::tests::multi_pattern_join_`
- `cargo clippy --features pg17 --all-targets -- -D warnings`

### 1.2 Collapse Join Binder Argument Lists

Current clippy blockers:

- `graph/src/query/semantics.rs:695`
- `graph/src/query/semantics.rs:831`
- `graph/src/query/semantics.rs:878`
- `graph/src/query/semantics.rs:1174`

Plan:

- Introduce one or two read-only binder context structs for join projection
  binding. The repeated facts are node slots, slot maps, relationship slot maps,
  path slot maps, and catalog.
- Keep the context borrowed; do not clone maps just to satisfy the refactor.
- Avoid a catch-all "bag of everything" if a narrower projection-scope context
  is enough.

Required checks:

- `cargo test --features pg17 query::tests::binder_`
- `cargo test --features pg17 query::tests::multi_pattern_join_`
- `cargo clippy --features pg17 --all-targets -- -D warnings`

### 1.3 Replace Production `expect`

Current clippy blocker:

- `graph/src/query/semantics.rs:1463`

Plan:

- Replace the `expect("caller only routes path-variable patterns")` path with a
  typed `GqlError` or an earlier pattern match that makes the impossible state
  unrepresentable in that helper.
- Prefer a type-level or control-flow split over returning a generic internal
  error if the caller can cheaply pass a concrete path variable.

Required checks:

- The narrow binder test covering path-variable pattern routing.
- `cargo test --features pg17 query::tests::binder_rejects_unsupported_wildcard_path_shapes`
- `cargo clippy --features pg17 --all-targets -- -D warnings`

### 1.4 Replace Admin `never_loop`

Current clippy blocker:

- `graph/src/sql_facade/admin.rs:78`

Plan:

- Replace the single-row `for row in result { return ... }` shape with
  `if let Some(row) = result.next()`.
- Keep the empty-result behavior unchanged.

Required checks:

- `cargo test --features pg17 sql_facade::admin`
- `cargo clippy --features pg17 --all-targets -- -D warnings`

## Phase 2: Dependency Update And Supply-Chain Review

Status: partially complete on 2026-06-05. Supported Cargo and PyPI pins were
updated, lockfiles were refreshed, pgrx docs were aligned to `0.18.1`, and
unit/docs/fuzz/deny/bench compile checks passed. `bincode` moved from `1.3.3`
to the latest usable major, `2.0.1`, with a deliberate `.pggraph` format version
bump so old derived artifacts fail closed and are regenerated through
`SELECT graph.build()`. The published `bincode 3.0.0` crate is intentionally
skipped because it contains a top-level compile error.

The remaining dependency items are:

- `nixpkgs` and `rust-overlay`: blocked in this environment because `nix` is
  not installed.

### 2.1 Refresh The Dependency Audit

Run the dependency freshness check with network access:

```bash
python3 scripts/check_dependency_updates.py
```

Expected current update classes:

- Cargo: `pgrx`, `pgrx-tests`, `roaring`, `serde_json`, `criterion`,
  `libfuzzer-sys`
- Python: `streamlit`, `psycopg`
- GitHub lock inputs: `nixpkgs`, `rust-overlay`
- Docker: currently OK in the latest observed check

### 2.2 Apply Supported Manifest Updates

Use the checker for supported Cargo and PyPI manifest rewrites, then use
`sfw` for package-manager dependency resolution steps.

Candidate command shape:

```bash
python3 scripts/check_dependency_updates.py --update cargo:pgrx --yes
python3 scripts/check_dependency_updates.py --update cargo:pgrx-tests --yes
python3 scripts/check_dependency_updates.py --update cargo:serde_json --yes
python3 scripts/check_dependency_updates.py --update cargo:roaring --yes
python3 scripts/check_dependency_updates.py --update cargo:libfuzzer-sys --yes
python3 scripts/check_dependency_updates.py --update pypi:streamlit --yes
python3 scripts/check_dependency_updates.py --update pypi:psycopg --yes
sfw cargo update -p pgrx -p pgrx-tests -p serde_json -p roaring -p libfuzzer-sys
```

Review `criterion` before applying:

- `criterion` `0.5.1 -> 0.8.2` is a dev-dependency major update. Apply if the
  bench harness compiles without large rewrites; otherwise record the deferral
  and reason.

### 2.3 Review Manual Pins

Manual items are not automatically rewritten by the checker:

- `flake.lock`: update `nixpkgs` and `rust-overlay` only after reviewing the
  lock diff and confirming the development shell still exposes the expected
  Rust, PostgreSQL, and pgrx tooling.
- Docker base image pins: review by tag and digest when the checker reports an
  update; do not auto-rewrite.

Candidate command shape for flakes:

```bash
nix flake lock --update-input nixpkgs
nix flake lock --update-input rust-overlay
```

### 2.4 Keep pgrx Version Surfaces In Sync

If `pgrx` and `pgrx-tests` move to `0.18.1`, review and update all related
surfaces in the same change:

- `graph/Cargo.toml`
- `graph/Cargo.lock`
- `Dockerfile` `cargo-pgrx` version or build arg
- `README.md`
- `README_zh.md`
- `docs/user_guide/installation.mdx`
- `docs/contributor_guide/testing-release.mdx`
- Any quickstart or release docs that pin `cargo-pgrx 0.18.0`

Run docs drift after the doc edits.

### 2.5 Supply-Chain Validation

After dependency changes:

- Inspect manifest and lockfile diffs for unexpected transitive additions.
- Run `cargo tree -d --features pg17` and review duplicates introduced by major
  updates.
- Run `cargo deny check` from `graph/`.
- Run the fuzz build wrapper after `libfuzzer-sys` changes:

```bash
bash fuzz/build.sh
```

## Phase 3: Release Verification Gates

Run narrow checks first, then broaden:

```bash
cd graph
cargo fmt --check
cargo clippy --features pg17 --all-targets -- -D warnings
cargo test --features pg17
cargo doc --features pg17 --no-deps
```

Then from the repository root:

```bash
scripts/check_docs_drift.sh
python3 scripts/check_dependency_updates.py
```

Then PostgreSQL-backed gates:

```bash
cd graph
cargo pgrx test pg17
PG_VERSION_FEATURE=pg17 ./tests/heavy/run_release_gate.sh
```

If the full release gate is too expensive during iteration, run these focused
gates before the full gate:

```bash
cd graph
cargo pgrx test --features "pg17 development" gql
PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_write_recheck ./tests/heavy/gql_write_recheck_race.sh
PG_VERSION_FEATURE=pg17 DBNAME=pggraph_tx_delta ./tests/heavy/tx_delta_lifecycle.sh
PG_VERSION_FEATURE=pg17 DBNAME=pggraph_gql_merge_race ./tests/heavy/gql_merge_race.sh
```

## Phase 4: Release Notes And Evidence

Before calling the branch release-ready:

- Update `docs/release-notes.mdx` so validation evidence reflects the current
  HEAD, not earlier `v0.1.5` evidence.
- Add a dated entry to `todo/measurements.md` with every command run and its
  result.
- Confirm version references remain consistent across `graph/Cargo.toml`,
  README badges, docs index badges, and release notes.
- Confirm `git status --short --branch` is clean except for intended release
  commits.

## Release-Ready Definition

The branch is ready for a new release only when:

- Clippy passes with `-D warnings`.
- Dependency freshness check reports no actionable updates or every deferral is
  explicitly documented with a security/release rationale.
- `cargo deny check` passes after dependency updates.
- Rust unit tests, docs build, docs drift, pgrx SQL tests, and full `pg17`
  heavy release gate pass at the same HEAD.
- Release notes and measurements record the current evidence.
