# R6 Documentation, Packaging, CI, And Operations Evidence

Date: 2026-07-16

Clean archive source commit: `267bd39`

R6 review correction commit: `57acb58` (test and documentation only; no
runtime source changed after the clean archive gate)

## Results

| Gate | Evidence | Result |
|---|---|---|
| PR release tier | `python3 scripts/run_release.py --tier pr --evidence todo/measurements/2026-07-16-r6-pr-evidence.json` | PASS: script contracts, documentation drift/rendering, and strict Clippy |
| Script contracts | Release runner and playground unit suites plus maintained-entry-point and unsafe-block inventories | PASS: 81 maintained scripts, 10 reviewed unsafe blocks, 2 runner tests, and 6 playground tests |
| Public documentation | `cd docs && npm run check`; `scripts/check_docs_drift.sh`; `python3 scripts/check_external_links.py` | PASS: 42 MDX/README files compiled, rendered, and spell-checked with zero issues; 27 external URLs and all local/API/source-map links passed |
| Rust documentation | Strict `cargo rustdoc`, `cargo doc`, and doctests for `pg17 development` | PASS: no warnings, missing public docs, broken intra-doc links, or doctest failures |
| Clean source package matrix | `cd graph && PG_VERSIONS='14 15 16 17 18' ./tests/heavy/source_archive_smoke.sh` | PASS: exact release archive packaged and installed for PostgreSQL 14, 15, 16, 17, and 18; every major passed `CREATE EXTENSION`, build, traversal, and search smoke |
| Alpha migration | Source-archive gate builds the signature-bearing `v0.1.8` producer source, installs it, removes generated sync objects with `DROP EXTENSION ... CASCADE`, preserves source rows/checksum, installs 1.0.0, re-registers, and rebuilds | PASS: source checksum `7597c464628b5c2e0674ea849bd8337a` and extension version `1.0.0` verified |
| Packaged quickstart | Public `scripts/quickstart.sh quickstart` against the archive-built runtime image | PASS: discovery/build, relational query, search, traversal, and shortest path returned the documented results |
| Packaged playground | Public playground release gate against the archive-built runtime image and pinned Panama dataset | PASS: 40 CSR and 41 mutable catalog queries, including operational build/sync/vacuum/maintenance and mutable GQL examples |
| Discovery production regression | PG17 production install followed by `graph.auto_discover('public')` and `graph.status()`; `PG_VERSION_FEATURE=pg17 DBNAME=pggraph_build_lock_r6_discovery ./tests/heavy/build_lock_regression.sh` | PASS: 2 nodes and 2 edges built; caller-owned mapping-catalog writes cause `auto_discover(..., build => true)` to fail with `55P03`/`PG006` and roll back registration; trusted discovery accepts only its own catalog locks |
| Focused Rust assurance | Formatting, strict all-target Clippy, build-snapshot unit tests, and focused pgrx discovery test | PASS |
| Independent review | Fresh `rust-reviewing` review of final R6 tree and correction | PASS: reviewer-required production discovery-lock regression added; no remaining correctness, security, packaging, CI, or documentation blocker |

## Gate Corrections Found By Clean Artifacts

The clean archive gate exposed and closed three harness/product integration
defects before R6 completion: absolute archive output handling, alpha generated
trigger removal and writable transition targets, and the production
`auto_discover(..., build => true)` catalog-lock boundary. The final gate was
rerun from the clean committed tree after all corrections.

## Deviations

No R6 release-gate waiver is recorded. Package execution used the supported
Linux container path on Apple Silicon; R7 retains the cross-platform RC artifact
and publication evidence requirements.
