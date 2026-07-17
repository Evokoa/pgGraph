# Suggested Secondary Todo List

Date: 2026-07-16
Source: `01`–`04` review documents in this folder. Ranked by impact.
These are *suggestions* to merge into `docs/known-issues.mdx` /
`docs/roadmap.mdx` (the authoritative registers) — this file is a staging
area, not a third register.

## P0 — Correctness & security (do before promoting mutable mode)

- [ ] **Advisory-lock durable projection ingest.** Take the per-graph
      PostgreSQL advisory lock (as build/vacuum do, `sql_build.rs:315-330`)
      around ingest + manifest publication (`projection/ingest.rs:86-111`,
      `manifest.rs:225-230`). Two concurrent `apply_sync()` backends can
      currently drop a segment generation (last-writer-wins rename).
- [ ] **Document (or gate) the RLS boundary on topology reads.** Topology
      functions return PKs/adjacency from a builder-scoped artifact; RLS is
      never consulted (`acl.rs:52-68`, `engine.rs:678-777`). Minimum: loud
      section in administration-and-security docs + known-issues row. Better:
      refuse or warn on RLS-enabled tables without an explicit opt-in GUC.
- [ ] **Clarify tenant scoping is a filter, not isolation** for
      bitmap-tenanted graphs (`sql_sync.rs:1883-1927`); align docs and
      consider an enforcement option.
- [ ] **Fix bidirectional BFS first-intersection break**
      (`path_finder.rs:165-172,201-208`) — finish the level and pick the
      minimal combined-distance meeting node; add a two-meeting-point
      regression test. Shortest paths may currently be one hop too long.
- [ ] **Add `_sync_log` retention/pruning.** Applied rows are never deleted
      anywhere; the log grows without bound and apply scans degrade. Prune
      below the durable watermark from maintenance /
      `run_scheduled_maintenance()`.

## P1 — Operational hardening

- [ ] **Wire fmt/clippy/test/doc-drift into an automated PR CI workflow.**
      Docs describe a CI gate ladder; the repo has only a manual packaging
      workflow (`.github/workflows/release.yml`). All needed scripts already
      exist (`graph/tests/heavy/run_release_gate.sh`,
      `scripts/check_docs_drift.sh`).
- [ ] **Poison-row dead-lettering for sync apply.** Wire the unused
      `_sync_log.error_message` column; skip-and-mark after repeated failure;
      surface a count in `sync_health()`.
- [ ] **Atomic artifact+sidecar commit.** `.sync` / `.projection_mode` are
      written after the `.pggraph` rename (`persistence.rs:615-622`) — crash
      window pairs a new artifact with a stale watermark. Also add parent-dir
      fsync after rename (manifest path already does this).
- [ ] **Truncation signal in traversal results.** `max_nodes`/`max_frontier`
      caps return partial results indistinguishable from complete ones
      (`bfs.rs:304-324`); shortest-path truncation can yield a false
      "no path". Add a `truncated` column / error contract.
- [ ] **Known-issues row for `mutable_overlay` production status.** Roadmap
      admits it needs verification + benchmark signoff; the limitations
      register says nothing and the quickstart offers `mutable` with no
      caveat.

## P2 — Performance follow-ups (benchmark-driven)

- [ ] **Stop cloning tenant state per traversal** — borrow
      `tenant_membership`/`tenanted_table_oids` in `BfsConfig`
      (`engine.rs:733-737`).
- [ ] **Empty-overlay fast path**: short-circuit
      `traversal_edge_overlay`/layered-provider assembly when edge buffer +
      tx delta are empty (`engine.rs:725,816-871`); prerequisite for "hybrid
      reads at CSR speed".
- [ ] **Per-generation load validation cache.** Full-file CRC + full content
      walk on every backend's first query (`persistence.rs:312-408,773`)
      likely dominates the known ~4.4 s first-backend cost; validate once per
      artifact generation instead.
- [ ] **Clamp frontier pre-allocation** to `min(max_frontier, node_count)`
      (`bfs.rs:248,362,510`).
- [ ] **Weighted shortest path over overlays** — extend Dijkstra to the
      layered neighbor source so writes stop disabling weighted queries
      (`engine.rs:966-980`). (Also a mode-design prerequisite, see doc 02.)

## P3 — Hygiene & docs

- [ ] **Bump `README_zh.md` to 0.1.8** (currently 0.1.5 badges/pulls, lines
      18/95/100/162) and add version parity to `check_docs_drift.sh`.
- [ ] **Move resolved KI rows (KI-001/002/003/011) out of "Next Update
      Scope"** into a resolved section; de-duplicate KI-005.
- [ ] **Builder table-name resolution parity**: use the OID-stable regclass
      path hydration uses instead of raw `format!` names
      (`builder.rs:341,478-482`) — search_path stability at build time.
- [ ] **Document little-endian-only mmap format** and add a
      `cfg!(target_endian)` guard (`edge_store.rs:508-509`,
      `node_store.rs:321-445`).
- [ ] **Unify the two overlay systems** (legacy `edge_buffer` vs durable
      segments) per the direction in `02-dual-mode-design-proposal.md` —
      larger refactor; roadmap-owned.

## Mode-design work (see `02-dual-mode-design-proposal.md` for detail)

- [ ] Phase 1 prerequisites: ingest lock, sync-log retention, dead-lettering
      (items above).
- [ ] Phase 2 "live" mode: auto-maintenance consuming the existing
      `sync_health()` recommendation booleans; require persistence for
      `mutable_overlay` builds; weighted paths over overlays; benchmark
      signoff; consider relaxing the `mutable_enabled` double gate.
- [ ] Phase 3 hybrid: `projection_mode = 'auto'` policy layer (escalate
      apply/compaction cadence and optionally mode on write pressure), with
      the empty-overlay read fast path making clean generations read at CSR
      speed.
- [ ] Phase 4: background pre-applier; WAL/logical-replication sync to remove
      trigger overhead for high-write tables.
