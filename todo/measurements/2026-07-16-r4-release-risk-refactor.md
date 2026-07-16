# R4 Release-Risk Refactoring — 2026-07-16

## Result

Release orchestration and the packaged playground now have explicit ownership
boundaries without removing or renaming a documented 1.x command. A generated
inventory covers every maintained shell and Python entry point. Named `pr`,
`nightly`, `rc`, and `full-matrix` tiers use a declarative registry with
dependencies, cross-process exclusive-resource locks, timeouts, thresholds,
dataset provenance, fingerprints, and atomic resumable evidence.

The playground separates environment configuration, reconnectable database
ownership, public bootstrap/registration, bounded execution, result formatting,
structured query metadata, and Streamlit rendering. The UI and release gate
share stable query IDs and explicit statement lists. The user-facing app no
longer truncates internal catalogs or calls test-only SQL.

## Evidence

| Gate | Exact command | Result |
|---|---|---|
| Script inventory/static checks | `PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_script_inventory.py` | PASS: 75 maintained entry points; `bash -n` complete and ShellCheck error checks enabled when installed |
| Unsafe boundary inventory | `PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_unsafe_allowlist.py` | PASS: 10 explained blocks in 9 owned files |
| Playground units | `PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=sandbox/playground python3 -m unittest sandbox/playground/test_catalog.py` | PASS: catalog IDs/boundaries/modes, formatting, reconnect, and bounded error shape |
| Named PR tier | `PYTHONDONTWRITEBYTECODE=1 python3 scripts/run_release.py --tier pr --evidence todo/measurements/2026-07-16-r4-pr-evidence.json` | PASS: script contracts, docs/contract drift, and strict Clippy |
| CSR packaged playground | `cd graph && PREPARE_PLAYGROUND=0 PGGRAPH_DSN=postgresql://postgres:postgres@localhost:55432/postgres ./tests/heavy/playground_release_gate.sh` | PASS: 40 expected query summaries on PostgreSQL 17 |
| Mutable packaged playground | `cd graph && PREPARE_PLAYGROUND=0 PGGRAPH_PLAYGROUND_MODE=mutable PGGRAPH_DSN=postgresql://postgres:postgres@localhost:55432/postgres ./tests/heavy/playground_release_gate.sh` | PASS: 41 expected query summaries on PostgreSQL 17 |

The initial CSR run caught terminal semicolons embedded in checksum CTEs; the
catalog now retains terminators only for editor display. The initial mutable
run caught production-scale durable catch-up between example statements; the
disposable mutable session now uses the documented `query_freshness = off`
isolation policy while separate examples exercise explicit sync and
maintenance. Both fixes were applied at shared boundaries and the full catalogs
then passed without expectation changes.
