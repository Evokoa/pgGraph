# R7 Release-Ready Evidence

Date: 2026-07-16

## Outcome

pgGraph 1.0.0 is release-ready but unpublished. The only published line remains
v0.1.8. This record closes implementation and local release assurance; it does
not claim that a signed tag, GitHub Release, PGXN archive, GHCR image, or
Homebrew formula has been published.

## Exact-Commit Acceptance

The final clean commit passes:

```bash
PYTHONDONTWRITEBYTECODE=1 python3 scripts/run_release.py --tier full-matrix
python3 scripts/verify_release_evidence.py --tier full-matrix \
  --commit "$(git rev-parse HEAD)" release/evidence/full-matrix.json
```

The machine-readable record in `release/evidence/full-matrix.json` owns exact
commit/tree identity, toolchain and PostgreSQL versions, commands, durations,
logs, checksums, resource measurements, and gate results.

## R7 Regression Evidence

```bash
cargo test --features "pg17 development" sql_jobs::tests
cargo clippy --features "pg17 development" --all-targets -- -D warnings
cargo doc --no-deps --features "pg17 development"
PG_VERSION_FEATURE=pg17 DBNAME=pggraph_concurrency CLIENTS=3 ROUNDS=3 \
  ./tests/heavy/concurrency_stress.sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/run_release.py --tier pr
```

Results:

- all five background-worker metadata boundary tests passed;
- the PostgreSQL 17 concurrency profile passed exact build, maintenance,
  due-job scheduler, and non-superuser forced-RLS worker checks;
- the probe left no login role behind and restored `graph.sync_mode`;
- formatting, Clippy with warnings denied, rustdoc, public docs, script
  contracts, bundle tests, and the PR tier passed;
- an independent Rust/release review reported no actionable findings after the
  cleanup hardening.

## Publication Handoff

The release owner still verifies repository rules, protected environments,
runner availability, credentials, and signing identity. The prepare
workflow consumes this commit's full-matrix evidence and creates immutable
prepared artifacts. Only after review does the owner create the signed tag and
publish those exact bytes and image digests without rebuilding.
