# Docs & Todo-Register Audit

Date: 2026-07-16
Scope: `docs/known-issues.mdx`, `docs/roadmap.mdx`, `README.md`, `README_zh.md`,
`.github/workflows/`, verified against `graph/src/`. This repo only.

The project's effective todo list is `docs/known-issues.mdx` (limitations
register) + `docs/roadmap.mdx` (direction). There is no separate todo file in
this repo.

## Verdict

The docs are unusually well-synchronized with the code. Every specific
technical claim spot-checked was accurate. Two real drift items exist
(Chinese README version, CI reality vs. wording), and three important work
items are missing from the register entirely.

## Verified Accurate (spot-checks)

- `graph.sync_mode = 'wal'` reserved-and-rejected: parser accepts
  (`config.rs:420`), resolution rejects (`sql_sync.rs:29`), covered by test
  `pg_tests/sync_config_build.rs:124`.
- `graph.build_scan_mode = 'copy'` reserved-and-rejected: `builder.rs:258-260`,
  test at `pg_tests/sync_config_build.rs:505`.
- `graph.memory_profile()` (`sql_facade/admin.rs:1453`),
  `graph.run_scheduled_maintenance()` (`admin.rs:1572`),
  `graph.sync_health()` (`admin.rs:1492`), `graph.status()` (`admin.rs:1009`)
  all exist with documented signatures.
- `mutable_overlay` exists exactly as the roadmap "Reserved Features" rows
  describe, including the `graph.mutable_enabled` gate
  (`sql_build.rs:355-367`) and write rejection on `csr_readonly`
  (`sql_facade/gql.rs:2393`).
- Version/toolchain coherence: 0.1.8 / Rust 1.96.0 / pgrx 0.19.1 / PG 14–18
  agree across `README.md`, `META.json`, `graph/Cargo.toml`,
  `graph/rust-toolchain.toml`, `docs/index.mdx`, installation docs.
- KI-008 mega-file list is real and current — all eight files still exceed
  1,000 lines: `query/tests.rs` 5,577; `pg_tests/gql.rs` 4,616;
  `query/semantics.rs` 3,932; `sql_facade/gql.rs` 3,275; `engine.rs` 2,557;
  `query/value.rs` 2,508; `bfs.rs` 1,444; `gql/parser.rs` 1,251.

## Drift / Contradictions

### D1 — `README_zh.md` is stale at v0.1.5

Badge, `docker pull …:0.1.5` (lines 95, 100), and "从 v0.1.5 开始…" (line 162)
all lag the English README's 0.1.8 — while its prerequisites already say Rust
1.96, so the file is internally inconsistent. It drifts on every release;
`scripts/check_docs_drift.sh` does not gate it.

### D2 — Docs imply automated CI that does not exist

Roadmap (`roadmap.mdx:45,63`) and several release notes describe a CI gate
ladder (fmt/clippy/test/doc drift "on every change", then pgrx integration,
then heavy gates). Reality: the only workflow is
`.github/workflows/release.yml`, a **manual** `workflow_dispatch` packaging
pipeline (metadata validation → PGXN zip → Docker matrix pg14–18). It runs no
`cargo fmt/clippy/test/pgrx test`. There is **no push/PR CI at all** — the
gates exist only as local scripts (`graph/tests/heavy/run_release_gate.sh`,
`scripts/check_docs_drift.sh`) and a human checklist. The roadmap's future
tense softens this, but release-note readers would assume per-change CI exists.
(The Docker matrix itself — pg 14–18, amd64+arm64, pg17 default — does match
the documented support range.)

## Missing From The Todo Register (untracked gaps)

### M1 — No PR/push CI workflow item

The single most valuable untracked engineering task: wire the existing
release-gate static checks (fmt, clippy, test, doc drift) into an automated
PR workflow. Everything needed already exists as scripts.

### M2 — `mutable_overlay` production-readiness has no known-issues row

Roadmap line 158 admits durable projection needs "production verification,
release documentation, and benchmark signoff" — but the known-issues register
(described as "the authoritative register for known limitations") has no row
for it, and the quickstart (`docs/quickstart.mdx:175`) offers the playground
`mutable` mode with no caveat. An operator enabling it gets no
limitations-level warning.

### M3 — `README_zh.md` release-parity is untracked

Either a tracked release-checklist item or an extension to
`check_docs_drift.sh` to enforce version parity.

Also untracked (found in the code review, see `01-sync-architecture-review.md`
and `03-implementation-review-findings.md`): unbounded `_sync_log` growth,
missing advisory lock on durable ingest, and unused
`_sync_log.error_message`/no poison-row dead-lettering.

## Stale / Completed Items Still Listed

- **KI-001, KI-002, KI-003** sit under "Next Update Scope" but are all
  resolved ("Hardened" / "Coverage added"). Same for **KI-011**. They read as
  audit trail but inflate the apparent open backlog under a heading framed as
  pre-production blockers — move them to a resolved section.
- **KI-005 is listed twice** (Internal Construction + P1). The duplication is
  flagged as deliberate, but a register should say it once and cross-reference.
- The P0 "no rows remain" callout is accurate (memory_profile shipped) — not
  drift.

## Assessment Of The Register As A Todo List

Strengths: disciplined known-issues/roadmap split, honest status wording,
impact-ranked, IDs are stable and referenced from code review docs.

Weaknesses: completed items accumulate in active sections; operational gaps
discovered only by reading code (sync-log growth, ingest locking, CI absence)
never made it in; and the register tracks *limitations* well but *engineering
debt with user impact* (the hybrid-mode automation gap) lives only as soft
roadmap prose. `05-suggested-todos.md` proposes concrete additions.
