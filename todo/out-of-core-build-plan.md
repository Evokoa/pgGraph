# Out-Of-Core Graph Build Plan

> **Archived 2026-07-16:** The release-scoped design was implemented through
> governed external runs, direct persisted artifact v6 construction, bounded
> load/sync/compaction, and release resource gates. This file is retained only
> as historical design context and contains no remaining task.

> **Program note:** This focused sketch predates the full codebase review.
> [Memory Governance And Out-Of-Core Execution](./full-graph-engine/01-memory-governance.md)
> is now authoritative where the plans differ. It also covers mmap load, query
> execution, sync, projection compaction, cross-backend publication, source
> snapshot/watermark correctness, and relationship identity.

## Goal

Allow `graph.build()` and rebuild workflows to complete under bounded memory by
trading time and disk space for lower peak RSS. The target behavior is:

- use a configurable fraction of available memory, such as 70%;
- detect container/cgroup memory limits when available;
- spill large intermediates to disk instead of growing unbounded `Vec`s;
- write the final graph artifact directly to the persisted format;
- mmap the completed artifact for serving;
- fail early only when the final artifact cannot be created safely or disk/temp
  capacity is insufficient.

## Current State

The near-term memory guard now prevents many Docker OOM crashes by estimating
replacement graph memory before allocation. It can also reduce rebuild peaks
with `graph.low_memory_build = on`, which unloads the current backend-local
graph before constructing the replacement.

That stabilizes rebuilds that fail because old and new graphs coexist in one
backend. It does not make a final graph fit when the final in-memory graph
itself exceeds the memory limit.

## Typical Architecture

The long-term solution is a bounded-memory external builder:

```text
PostgreSQL scan
  -> bounded batches
  -> disk-backed run files
  -> external sort/merge by source node, edge type, and target
  -> streaming CSR/filter/resolution artifact writer
  -> atomic artifact publish
  -> mmap final artifact for serving
```

`mmap` is most useful for the final read-mostly artifact. It does not by itself
make the build memory-safe if the builder still constructs the whole graph in
owned heap allocations before writing the artifact.

## Proposed Settings

- `graph.build_memory_target`: fraction or absolute size, for example `70%`,
  `4096MB`, or `auto`.
- `graph.build_spill_mode`: `off`, `auto`, or `force`.
- `graph.build_spill_dir`: optional temp directory override; default to the
  PostgreSQL temp directory or pgGraph artifact temp directory.
- `graph.build_spill_disk_limit_mb`: optional cap for temp files.

Effective memory budget should be:

```text
min(
  configured build cap,
  graph.memory_limit_mb minus current pgGraph private residency,
  detected cgroup limit minus current cgroup usage minus safety reserve,
  host available memory minus safety reserve
)
```

Container/cgroup remaining memory should win over host memory whenever present.
Using a percentage of the total cgroup limit is unsafe when PostgreSQL or other
backends already consume most of that limit.

## Implementation Phases

### Phase 1: Budget Detection And Planning

- Add cgroup-aware memory limit detection for Linux containers.
- Subtract current cgroup/backend residency and a safety reserve.
- Keep platform fallbacks conservative for macOS and non-cgroup systems.
- Convert `graph.build_memory_target` into a concrete per-build budget.
- Capture a coherent PostgreSQL snapshot/catalog fingerprint and source-change
  watermark before chunk scanning.
- Expose the resolved build budget in build progress/status output.
- Add unit tests for cgroup parsing, percentage parsing, and fallback behavior.

### Phase 2: Disk-Backed Edge Runs

- Scan PostgreSQL source tables in bounded batches.
- Write node, relationship-identity, filter, resolution, outbound-edge, and
  inbound-edge run files containing compact typed records.
- Include checksums, schema version, graph id, and build id in run metadata.
- Bound per-run buffers by the resolved build budget.
- Add cleanup for abandoned temp runs.

### Phase 3: External Sort And Merge

- Sort each run by `(source_node, edge_type, target_node)`.
- Merge sorted runs with bounded fanout.
- Preserve duplicate handling and weight semantics.
- Keep merge buffers under the build budget.
- Add stress tests for high edge counts, skewed degree distributions, and
  repeated rebuilds.

### Phase 4: Streaming Artifact Writer

- Write node arrays, both CSR directions, relationship identities, weights,
  mmap-friendly filter sections, and resolution sections directly to the
  artifact file.
- Avoid constructing a complete owned `Engine` for persisted builds.
- Write a generation-specific artifact, mmap-validate it, replay captured
  source changes to a final watermark, then atomically switch the generation
  manifest under cross-backend serialization.
- Ensure failed builds leave the previous artifact intact.

### Phase 5: mmap Load And Rebuild Semantics

- Load the completed artifact through the existing mmap path.
- Preserve runtime metadata currently carried across persisted rebuilds.
- Keep `graph.low_memory_build` useful for the current backend's loaded graph,
  but make out-of-core persisted builds avoid old + new heap ownership by
  design.
- Confirm other backends keep serving the previous graph until they reload.

### Phase 6: Operational And Release Gates

- Extend `measure_build_rss.sh` to support spill mode and memory-target knobs.
- Extend `build_memory_stress.sh` with large spill-mode profiles.
- Add a release-gate option for bounded-memory out-of-core builds.
- Document expected tradeoffs: slower builds, more disk I/O, and larger temp
  space requirements.

## Risks And Tradeoffs

- Builds will be slower, especially on Docker bind mounts or slow disks.
- Temporary disk usage may be large and must be capped/monitored.
- Random traversal over an mmap artifact can still be slow if the active query
  working set exceeds RAM.
- External sort/merge complexity must preserve existing semantics for edge
  types, weights, filters, resolution indexes, catalog drift, and rollback.
- Crash cleanup and artifact atomicity are required so failed builds do not
  corrupt the previous serving graph.

## Acceptance Criteria

- A graph that exceeds the old heap-build peak can complete when temp disk is
  available and the final artifact is valid.
- Peak backend RSS stays under the resolved build budget plus a small fixed
  tolerance.
- If disk spill capacity is insufficient, the build fails with a clear pgGraph
  SQLSTATE instead of Docker OOM.
- Existing query semantics and persisted artifact validation remain unchanged.
- Release documentation includes commands for normal, stress, and release-gate
  validation.
