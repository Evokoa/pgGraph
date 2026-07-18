# Reconciliation — New Todo (Doc 09) vs The dev 1.0 Release Line

Date: 2026-07-17
Status: authoritative status map. Docs 01–09 were written against `main`
(v0.1.8 alpha). The active line is `dev` (pgGraph 1.0.0, release-ready,
unpublished, validated at commit `4edabea`), which completed the
`todo/v1-release/` program (R0–R7) and rewrote the public registers without
KI numbering. This document maps every doc 09 slice onto that reality.

Continuity: the old plan lives on dev at `todo/v1-release/` (completed,
archived) and `todo/full-graph-engine/` (design references). The alpha
register edits that docs 01–05 were merged into are preserved on branch
`review/alpha-register-merge` (not carried into dev). KI numbers used in
docs 01–09 refer to that superseded alpha numbering — treat them as labels
for the descriptions, not live register IDs.

## Verified against dev code/docs (2026-07-17)

| Doc 09 slice | Status on dev | Evidence |
|---|---|---|
| S0 automated PR CI (KI-017) | **Done** | `.github/workflows/ci.yml` exists (R6) |
| C1 ingest/manifest publication race (KI-012) | **Fixed, different mechanism** | Manifest publish uses `create_new` (O_EXCL) no-overwrite CAS; test `projection_manifest_stale_publisher_loses_compare_and_swap` (`projection/manifest.rs`). The process-local mutex remains as a fast-path guard, no longer the only protection. Verify-only item: confirm loser-retry behavior surfaces to `apply_sync()` callers |
| C2 bidirectional BFS minimality (KI-015) | **Fixed 2026-07-17** | `path_finder.rs` now tracks per-node hop depth in `ParentStep` and fully scans each BFS level before selecting the minimum-combined-distance meeting node, instead of breaking on the first candidate found. Regression: `bidirectional_bfs_selects_minimal_combined_distance_meeting_node` (deterministic) + `bidirectional_bfs_matches_single_direction_bfs` (differential proptest, 20k cases). See `todo/progress.md` 2026-07-17 entry |
| C3 `_sync_log` retention (KI-016) | **Likely still open — re-verify** | No pruning path found on dev (`DELETE FROM graph._sync_log` absent). R3 "bounded storage" covered snapshot/watermark and compaction crash-safety; log retention was not located |
| C4 RLS topology boundary (KI-013) | **Likely still open — re-verify** | Dev user docs still frame RLS as a source-table concern; no topology-read RLS boundary section found in `querying.mdx`/`limitations-and-fit.mdx`. R1 "RLS green" evidence should be audited to see which paths it covered |
| C5 tenant filter-vs-isolation (KI-014) | **Re-verify** | Artifact v6 added a tenant dictionary and dense per-node tenant tokens (R3C); semantics of caller-supplied tenant scope need re-checking against the new representation |
| O1 poison-row dead-lettering (KI-018) | Re-verify (R2/R3 job retry work may cover it) | — |
| O2 atomic artifact+sidecar commit (KI-019) | **Probably obsolete — re-verify** | Persistence moved to a 26-section artifact v6 with generation-CAS publication and pinned generation-specific bases; the `.sync`/`.projection_mode` sidecar-window finding was against the v0.1.8 format |
| O3 truncation signal (KI-020) | **Fixed 2026-07-17** | Added a trailing `capped boolean` column to `graph.traverse()`, `graph.get_neighbors()`, and `graph.traverse_search()`, true only when `max_nodes`/`max_frontier` cut expansion short. Confirmed `shortest_path()`/`weighted_shortest_path()` were already safe via `PathWorkBudget` erroring on budget exhaustion instead of returning a silent "no path". Intentional breaking SQL change; release contract regenerated. See `todo/progress.md` 2026-07-17 entry |
| O4 mutable-overlay production caveat (KI-021) | **Superseded** | 1.0 promotes durable projections as a supported, gated surface (R3 evidence); the alpha caveat no longer applies as written |
| O5 README_zh parity (KI-024) | **Done** | README_zh on dev is at 1.0.0 |
| Stage 3 live mode (L1–L4) | **Largely delivered by R2/R3 — re-scope** | Bounded builds, governed external runs, generation CAS, retained serving generation, durable projection profiles all landed. Remaining from doc 09: weighted-paths-over-overlays (L3) — re-verify; auto-maintenance beyond packaged pg_cron remains a documented operational boundary (dev known-issues "External scheduling is required") |
| Stage 4 hybrid `auto` mode | **Open** | No `projection_mode = 'auto'` policy layer on dev |
| F1 validation-cache / first-backend cost | **Possibly fixed — re-verify** | R3C: "checksum verification is bounded"; current-manifest readers pin generation-specific bases. Benchmark the first-backend query cost on dev before keeping this item |
| F4 shared reverse CSR / F5 direction-optimizing BFS | **Open (F4 partially?)** | Artifact v6 loads "both graph directions … into one compact immutable mapping per backend" per dev limitations doc — re-verify whether reverse CSR is now mmap-shared; F5 not present |
| Stage 2P P1 property-graph catalog import | **Already planned on dev** | `todo/full-graph-engine/08-postgresql-19-property-graphs.md` specifies the CREATE PROPERTY GRAPH catalog frontend — merge doc 09's P1 details (GA timing, `pg_get_propgraphdef()` drift detection) into that plan rather than duplicating |
| Stage 2P P2 LDBC harness incl. GRAPH_TABLE comparison | **Open** | Not found on dev; doc 09 stands. C2 + O3 still gate publication |
| Stage 2P P3 `graph.graph_table()` pattern surface / P4 CustomScan | **Open** | 1.0 contract explicitly excludes PG19 SQL/PGQ; roadmap-owned. Doc 09 stands |
| Stage 6 R1 overlay unification / R2 builder regclass / R3 endianness | **Re-verify** | Persistence and build were heavily reworked in R3/R4; re-check each against dev before scheduling |

## Corrected next-actions list (post-1.0 publication)

Still-open, verified items first:

1. ~~**C2** — bidirectional BFS minimal-meeting fix.~~ **Done 2026-07-17.**
2. ~~**O3** — truncation/`capped` signal on bounded traversal results.~~
   **Done 2026-07-17** (breaking SQL change; release contract regenerated;
   live-verified — see item 3's full pgrx run, which superseded the
   originally-deferred "pgrx test run owed" note).
3. ~~**C4/C5** — RLS topology boundary and tenant-scope semantics.~~
   **Fixed (behavior change) 2026-07-17**, superseding the earlier
   documentation-only decision at the user's explicit request:
   `graph.build()` now refuses tables with row-level security enabled unless
   `graph.allow_rls_tables = on` (new GUC, diagnostic `PG021`), and
   `resolve_tenant_scope()` now rejects an explicit tenant SQL argument for
   non-pinned tenant_column-registered graphs while
   `graph.enforce_tenant_scope = on`, forcing the trusted session-setting
   path instead. Three existing pgrx tests were updated to the new behavior
   and two new ones added. Also fixed a local pgrx-test blocker
   (`LC_ALL=C LANG=C` works around a macOS `postmaster became multithreaded`
   startup failure) that had been silently preventing all live pgrx
   verification this session. With it fixed, ran the complete pg17 pgrx
   suite (`--features "pg17 development"`, matching the release-gate
   convention): **1143 passed, 0 failed, 1 ignored** — live confirmation of
   both this change and O3. True per-row RLS-aware topology filtering (vs.
   this build-time acknowledgment gate) remains unimplemented; it would
   require a panic-safe temporary PostgreSQL security-context switch to the
   outer caller, judged too risky to hand-roll in this pass. See
   `todo/progress.md` 2026-07-17 entry.
4. ~~**C3** — `_sync_log` retention.~~ **Implemented and live-verified
   2026-07-17**, following the design in
   `todo/full-graph-engine/12-sync-log-retention-plan.md`:
   `graph._sync_watermarks` heartbeat table, the safe-floor computation (pure,
   unit- and proptest-covered), pruning wired into `graph.maintenance()` only,
   3 new `sync_health()` diagnostic columns (breaking SQL change; release
   contract regenerated), and — a gap found and closed during implementation
   that the original design didn't specify — a `graph._graphs.sync_log_pruned_before_id`
   watermark plus a new `PG022` guard so a resumed stale backend fails closed
   instead of silently replaying past pruned rows. 6 new pgrx tests (found
   and fixed a real off-by-one and two flawed test simulations by actually
   running them live, not just compiling). Full pg17 pgrx suite:
   **1156/1156**. See `todo/progress.md` 2026-07-17 entry.
5. **Stage 2P** — fold doc 09's P1/P2 specifics into
   `todo/full-graph-engine/08-postgresql-19-property-graphs.md`; keep P2's
   LDBC + GRAPH_TABLE comparison and the PG19-GA clock.
6. Re-verification sweep for the remaining "re-verify" rows above before
   scheduling any of them.
7. **Release-gate re-run — DONE.** The live pg17 `cargo pgrx test` pass is now
   done (1156/1156, covering O3/C4/C5/C3 together), but the full-matrix
   evidence at `release/evidence/full-matrix.json` (recorded at `4edabea`)
   still predates all four changes and has not been regenerated. Remaining:
   the multi-version PostgreSQL 14–18 matrix and the non-pgrx gates
   (`docs-render`, `external-links`, `crash-recovery`, `pg-upgrade-matrix`,
   `package-install-matrix`); the Linux `postgres-sanitizer` gate remains
   unavailable in this environment; signing/tagging/publication remain
   release-owner actions. In progress: re-run iteration 5 hit a genuine
   regression from C4 itself — `graph/tests/heavy/concurrency_stress.sh`'s
   RLS-worker-identity probe legitimately enables RLS and now needs the new
   `graph.allow_rls_tables` opt-in, same as the earlier `gql.rs` pgrx test
   fix. Fixed, live-verified standalone, committed `48b18dd`; matrix
   relaunched as iteration 6.
   **Iteration 6 result: 16/17 gates pass** (only the expected
   `postgres-sanitizer` macOS/valgrind gap fails), but two docs/tooling-only
   commits landed on the tree mid-run so the evidence's commit stamp
   (`48b18dd`) trails actual `HEAD` by two commits; neither touches
   Rust/SQL so gate outcomes are unaffected, but a 7th, final iteration was
   relaunched against a static, confirmed-clean tree at `84cba5b` for a
   pristine single-commit evidence bundle.
   **Iteration 7 failed** at `legacy-release`'s playground gate: a
   pre-existing, non-regression race between the sandbox container's
   periodic scheduled-maintenance worker and `graph.build()`'s advisory
   build lock (PG006), which `sandbox/common/run_benchmarks.py` already
   retries around but `playground_release_gate.py` did not. Root-caused
   and live-verified against the real advisory lock (not just read from
   source); fixed with the same retry precedent, committed `d150c64`.
   **Iteration 8 failed** at `read-latency` -- a genuine environmental blip
   (an unrelated ambient Homebrew Postgres service restarting), confirmed
   not a pgGraph bug by an immediate clean standalone re-run; no fix
   needed. **Iteration 9** reached `package-install-matrix` before hitting
   the same PG006 cascade again, this time proving `d150c64`'s retry
   itself had a bug: `run_psql`'s `stop_on_error=False` branch (the only
   branch the playground gate actually uses) raised a generic hardcoded
   message instead of the real stderr, so the retry's PG006 substring
   check could never match. My original verification of `d150c64` had
   exercised the *other* branch (`stop_on_error=True`), so it looked
   correct without actually covering the real call path. Fixed by
   including the real stderr in the raised message (commit `04338c3`),
   re-verified live using the correct branch this time.
   **Iteration 10: clean pass, 16/17 gates, `git_commit = ff96701`, no
   commits mid-run.** Every gate passes except the expected
   `postgres-sanitizer` macOS/no-valgrind gap. This closes out the
   release-gate re-validation owed since the takeover. See
   `todo/progress.md` 2026-07-17/18 entries for the full blow-by-blow.

## Release-takeover record (2026-07-17)

- The interrupted validation run completed after takeover: `full-matrix` tier
  on `dev@4edabea`, **16/17 gates pass**. The only failure is
  `postgres-sanitizer`, which fails closed in 0.4 s because valgrind is
  unavailable on this macOS workstation (`run_postgres_process_sanitizer.sh`
  requires it). This is an environment-deferred release-operator gate on
  Linux, consistent with the repo's existing convention for
  environment-specific gates. Evidence:
  `release/evidence/full-matrix.json` + `release/evidence/logs/full-matrix/`
  (local, gitignored by design).
- The previous agent's planned "sfw Dockerfile correction" was **rejected**:
  `sfw` is a host-side wrapper (`/usr/local/bin/sfw`) that does not exist in
  Docker build containers; the AGENTS.md policy governs agent-executed host
  commands, not shipped build files. Adding it to the production Dockerfile
  would break every user's image build and force needless evidence
  regeneration. `dev@4edabea` therefore remains the validated release
  candidate; no new commit or evidence regeneration is required for this
  item.
- Remaining release work is release-owner action only: Linux sanitizer gate,
  signing/tagging, publication (per `todo/v1-release/README.md`).
