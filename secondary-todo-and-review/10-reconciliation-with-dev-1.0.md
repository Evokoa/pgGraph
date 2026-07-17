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
   live `cargo pgrx test` run and fresh full-matrix evidence capture still
   owed before this can be folded into a release-gate re-run).
3. ~~**C4/C5 audit** — RLS topology boundary and tenant-scope semantics.~~
   **Done 2026-07-17** (documentation-only, no behavior change, per explicit
   decision).
4. **C3** — `_sync_log` retention. **Design complete 2026-07-17:**
   `todo/full-graph-engine/12-sync-log-retention-plan.md` specifies a
   `graph._sync_watermarks` heartbeat table (mirroring the existing
   `_projection_generations` pattern), the exact hook points
   (`refreshed_engine_status()` in `sql_facade/admin.rs`, the
   `apply_pending_sync` freshness path), the safe-floor algorithm with an
   explicit bootstrap safety rule (zero heartbeats ⇒ prune nothing, never
   default to pruning everything), new `sync_health()` diagnostics, and a
   full test plan. **Not yet implemented** — deliberately deferred as a
   separate implementation pass given the failure mode (silent sync-log data
   loss) warrants its own dedicated TDD cycle rather than being rushed in the
   same pass as the design.
5. **Stage 2P** — fold doc 09's P1/P2 specifics into
   `todo/full-graph-engine/08-postgresql-19-property-graphs.md`; keep P2's
   LDBC + GRAPH_TABLE comparison and the PG19-GA clock.
6. Re-verification sweep for the remaining "re-verify" rows above before
   scheduling any of them.
7. **Release-gate re-run owed**: a live `cargo pgrx test` pass and a fresh
   `release/evidence/full-matrix.json` capture, since the O3 SQL contract
   change post-dates the recorded full-matrix evidence at 4edabea.

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
