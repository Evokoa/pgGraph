# R2D Generation Publication Baseline — 2026-07-16

## Method

The release gate was run as a non-fail-fast inventory. Sandbox-only PostgreSQL
port binding failures were rerun with the required host permissions, and every
downstream heavy gate was allowed to finish so repairs could be batched.

## Passing Baseline

- Formatting, Clippy, rustdoc, schema generation, contract drift, documentation
  drift, secret scanning, fuzz compilation and seed corpora, and package
  validation passed.
- Rust passed 779 tests with one intentional ignore.
- PostgreSQL 17 pgrx passed 1,049 tests with one intentional ignore after the
  sandbox port-binding limitation was removed.
- Fresh install, alpha-to-1.0 migration, function metadata, SQLSTATE/ACL,
  background job locking, concurrency, synthetic 50k-node smoke, pgbench sync,
  public GQL mutation races and transaction lifecycles, named graphs,
  cross-backend publication, projection recovery, write rechecks, isolation,
  and the 87 MiB runtime resource profile passed.

## Failure Inventory

| Gate | Classification | Repair |
|---|---|---|
| `cargo deny` | Dependency advisory | Update `crossbeam-epoch` from 0.9.18 to the patched 0.9.20 release and rerun the advisory gate. |
| Backup/restore | Gate and documentation drift | Preserve fail-closed OID identity; explicitly re-register source relations after a logical restore into a new database, then rebuild. |
| Transaction delta lifecycle | Test-helper identity drift | Give test-injected relationships the stable source-row identity now required by all relationship query paths. |
| Build publication lock | Gate resource-policy drift | Run the deliberately large publisher fixture with an explicit 2 GiB maintenance budget while retaining the production 256 MiB default. |
| Playground | Infrastructure plus release-profile drift | The initial Docker metadata fetch stalled. The complete rerun then found the 64 MiB production query default was too small for several 2-million-node playground examples, the upstream `LATEST` archive had changed deterministic results, and the production shared library did not export three background-worker entry points. Pin the 2025-03-31 archive checksum, use a sandbox-only 512 MiB query profile, keep production defaults unchanged, export the worker symbols, and rerun the full catalog non-fail-fast. |

## Batch Loop

The actionable failures were repaired as one batch before their focused gates
and the complete release gate were rerun. The pinned Panama archive contains
2,016,523 nodes and 6,678,534 relationships. Its undirected golden was reviewed
against the authoritative source row: node `23000018` has one
`same_intermediary_as` relationship (`edge_id` 3144399) to node `11012822`.
The two identical GQL rows were duplicate forward/reverse storage orientations,
not two source relationships; executor identity deduplication restores one row
while preserving parallel relationships with different source identities.
The capped three-hop example also now orders equal-node paths by its rendered
path, eliminating a nondeterministic 300-row hash without changing its result
set.

## R2D Publication Evidence

- Persisted projection manifests now become serving state only through a
  bounded, checksummed current-generation pointer under graph-scoped PostgreSQL
  exclusion and a cross-process artifact lock.
- Normal sync, compaction, chunk rewrite, and rebuild publishers use explicit
  generation comparison. Authoritative recovery compares the raw bounded
  pointer token so it can replace corrupt pointer metadata only after the new
  manifest is durable and validated.
- Shared reader locks cover graph, manifest, segment, and chunk loading. Active
  heartbeat generations and current-pointer ancestry protect installed readers.
- GC shares the exclusive artifact lock, retains cleanup intent while an old
  reader protects a candidate, and removes abandoned manifests, their unshared
  segment/chunk/identity artifacts, and abandoned temp files.
- Focused Rust evidence covers concurrent publishers, stale CAS, pointer and
  manifest checksum corruption, oversized pointer input, failure immediately
  after pointer rename, recovery, reader-versus-GC exclusion, old-reader cleanup
  intent, rollback retention, and bounded orphan cleanup. Cross-backend build
  and projection publication locking passes on PostgreSQL 17.

## Final Checkpoint Gate

The final R2D diff passed formatting, warnings-denied Clippy, rustdoc, contract
drift, 790 Rust tests (one intentional ignore), 1,061 PostgreSQL-backed tests
(one intentional ignore), dependency policy, fuzz seed corpora, packaging,
fresh install, alpha-to-1.0, SQLSTATE/ACL, backup/restore, background/build
locks, concurrency, the 50,000-node synthetic profile, and all 40 pinned Panama
CSR playground queries.

The aggregate gate caught two stale GC counter assertions after GC began
counting collected manifest files, plus one misplaced playground path-order
tie-breaker. The assertions now verify artifact and manifest cleanup, and only
the capped three-hop query orders by its projected `readable_path`.

The unchanged pgbench post-sync threshold remains red: the workload completed
3,034 transactions with zero failures at 101.20 TPS, but the fresh-backend
query smoke measured 509 ms against 250 ms. Exact repeats measured 450 ms total:
40 ms for status, 359 ms for traversal auto-load, and 136 ms for source search
when measured separately. The dominant cost is the existing load-time owned
inbound CSR reconstruction. The threshold is not relaxed; R3 must turn this
gate green by persisting and mapping inbound CSR before the storage phase can
close.

The aggregate tail was then resumed with the already-recorded pgbench result
excluded. It repeated the 790-test Rust suite and 1,061-test PostgreSQL suite,
then passed dependency policy, fuzz corpora, runtime resource profiling (85 MiB
peak backend RSS), named graphs, cross-backend durable publication, projection
recovery and garbage collection, transaction delta lifecycle, GQL write
rechecks, and the isolation matrix. This establishes a stable R2D manifest
checkpoint while keeping the single R3-owned latency blocker visible.
