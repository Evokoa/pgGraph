# Playground And Scripts Overhaul Plan

## Goals

Make the playground a maintainable, packaged-product example and make scripts
predictable entry points rather than a growing collection of partially
overlapping orchestration logic. Preserve documented commands during the 1.x
compatibility window.

## Current Risks

- `sandbox/playground/app.py` combines connection lifecycle, extension setup,
  source registration, graph build/retry policy, query execution, result
  serialization, and Streamlit rendering in one module.
- `sandbox/playground/queries.py` combines catalog metadata and hundreds of
  lines of SQL strings, making query/profile drift difficult to review.
- `scripts/quickstart.sh` owns Docker orchestration, demo SQL, local pgrx
  installation, playground routing, compatibility aliases, cleanup, and CLI
  parsing.
- Heavy shell tests repeat database lifecycle, pgrx start/install, polling,
  cleanup, logging, and environment validation.
- `run_release_gate.sh` is a long list of boolean environment switches without
  a declarative inventory, tier model, dependency graph, or evidence manifest.
- Script documentation is split across root README, `scripts/scripts.md`, the
  sandbox README, heavy-test README, and contributor docs.
- `sandbox/start_playground.sh` currently falls back to direct `pip` installs
  when `sfw` is unavailable, which violates the repository package-install
  policy.
- Playground provisioning is duplicated in the UI, benchmark preparation, and
  release validator, and each copy uses test-only/private catalog operations.
- The playground release validator splits SQL on semicolons, which is not safe
  for quoted or dollar-quoted SQL.
- The aggregate release gate omits some separately documented docs, upgrade,
  latency, sanitizer, and release-validation checks.

## P1: Inventory And Contract Freeze

- [x] Create a machine-readable script inventory containing owner, audience,
      tier, required tools, inputs, outputs, destructive behavior, timeout,
      supported platforms, and CI/release usage.
- [x] Classify entry points as public/stable, maintainer/stable, or internal.
- [x] Freeze public commands and exit-code/environment contracts for 1.0.
- [x] Identify duplicate, unused, obsolete, or misleading scripts and record
      keep/merge/replace/delete decisions before moving files.
- [x] Add `bash -n` and ShellCheck coverage for every maintained shell script,
      plus focused Python lint/type/unit checks for script helpers.
- [x] Require `sfw` for any dependency installation performed by a script, or
      use a prebuilt/locked environment that performs no runtime installation.

## P2: Shared Script Foundations

- [x] Put shared PostgreSQL/pgrx lifecycle, database naming, polling, timeout,
      cleanup, logging, prerequisite, and evidence helpers in one sourced
      library with a narrow documented interface.
- [x] Require traps to clean only resources created by the current script;
      reject unsafe or ambiguous database/cluster targets.
- [x] Standardize `--help`, exit codes, environment validation, timeouts,
      temporary directories, log locations, and machine-readable summaries.
- [x] Replace copied shell SQL with versioned `.sql` fixtures where doing so
      improves reviewability and reuse.
- [x] Keep platform-specific behavior at adapters; do not spread macOS/Linux/
      Docker branching through every workflow.

## P3: Release Orchestration

- [x] Replace boolean-sprawl orchestration with named tiers such as `pr`,
      `nightly`, `rc`, and `full-matrix`, backed by a declarative gate registry.
- [x] Record exact command, PostgreSQL/pgGraph versions, duration, result,
      dataset checksum, memory/performance thresholds, and artifact locations
      in one evidence manifest.
- [x] Make dependencies and exclusivity explicit so database/port/cluster
      collisions cannot silently corrupt results.
- [x] Support resuming an interrupted release run without treating stale output
      as fresh evidence.
- [x] Preserve `run_release_gate.sh` as a compatibility wrapper for at least the
      documented 1.x deprecation window if its interface changes.
- [x] Reconcile the aggregate runner with every documented required gate,
      including docs drift, release metadata validation, upgrade, latency,
      sanitizers, and supported-major evidence.

## P4: Playground Refactor

- [x] Separate configuration, database client, bootstrap/registration, build
      coordination, query catalog, execution, result models, and Streamlit UI.
- [x] Move query examples into structured data with stable IDs, title,
      question, mode/capability requirements, SQL, expected schema or checksum,
      and documentation link.
- [x] Make the automated release gate and UI consume the same query catalog so
      examples cannot drift.
- [x] Represent multi-statement examples as explicit statement lists; do not
      parse SQL by splitting on semicolons.
- [x] Remove direct truncation of pgGraph internal catalogs from the playground;
      use supported reset/registration APIs or add a narrowly scoped test-only
      fixture mechanism outside the user-facing app.
- [x] Use explicit connection health/reconnect and bounded query execution;
      avoid caching a permanently broken connection.
- [x] Keep mutable examples isolated and resettable so one user/session cannot
      make later examples nondeterministic.
- [x] Separate dataset acquisition/checksum/setup from application startup and
      document licensing, size, provenance, and offline/retry behavior.
- [x] Add unit tests for catalog parsing and result formatting, integration tests
      for bootstrap/reconnect/error handling, and packaged end-to-end smoke tests
      for CSR and mutable modes.
- [x] Review accessibility, error guidance, secret handling, SQL trust boundary,
      row/result caps, and deployment assumptions before presenting the
      playground as production-adjacent.
- [x] Document PostgreSQL 17 as the 1.0 playground reference environment. The
      extension itself still passes the PostgreSQL 14-18 release matrix.

## Compatibility And Migration

- Keep `scripts/quickstart.sh` commands and the documented playground modes as
  wrappers while internals move.
- Print a deprecation warning and replacement command before removing any
  documented alias; follow the project-wide deprecation window.
- Do not rename environment variables or change defaults silently.
- Tests must compare old wrapper and new entry-point behavior for supported
  paths throughout the transition.

## Exit Gate

- Public and maintainer entry points are documented from one generated index.
- No duplicated lifecycle/bootstrap implementation remains without a recorded
  reason.
- Static checks and tier-appropriate smoke tests cover every maintained script.
- The playground query catalog, docs examples, and release gate share one
  source and pass in CSR and mutable modes against the packaged PostgreSQL 17
  reference environment.
- Compatibility wrappers and removal dates are documented.
