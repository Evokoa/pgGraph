# KI-027 Persisted Rebuild Manifest Plan

This plan closes the repeated-build failure where replacing `main.pggraph`
leaves the active mutable-projection manifest bound to the previous base
checksum.

## Invariants

- A successful persisted build publishes a new base-only projection generation
  whose path, format version, checksum, and sync watermark describe the newly
  written graph artifact.
- The new generation is monotonic, links to the prior generation, and carries
  forward operation timestamps used by status and maintenance.
- Segments, identity dictionaries, and base chunks superseded by the rebuild
  are declared obsolete for generation-aware garbage collection; the active
  base artifact is never marked obsolete.
- The rebuilt artifact is not loaded through a stale manifest. Manifest rebasing
  occurs after the artifact is fsynced and before mmap reload validation.
- Build, vacuum, recovery, and repeated-build paths share one rebasing helper
  and cannot publish duplicate generation identifiers.
- This checkpoint does not claim the final generation-specific base-artifact
  switch required by KI-018. A process crash between base replacement and
  manifest publication remains tracked there and must be closed before 1.0.

## Ordered Checkpoints

1. Add a projection recovery helper that derives the next generation, creates
   a base-only manifest for the current artifact, preserves status timestamps,
   and records superseded projection files.
2. Call the helper from persisted build/vacuum before mmap reload, and reconcile
   the full-recovery path so it does not publish a second generation.
3. Add unit coverage for checksum rebasing, generation linkage, obsolete-file
   accounting, and base-only reload.
4. Make the persisted two-session isolation matrix pass repeated builds and
   retain matching source/projection visibility.
5. Update public upgrade, sync, known-issue, and release documentation without
   overstating crash-atomic publication guarantees.

## Exit Evidence

- [x] Two consecutive persisted mutable builds load successfully with the
  latest manifest checksum matching `main.pggraph`.
- [x] Superseded projection artifacts remain protected by retained generations
  and become eligible for bounded garbage collection only under existing pin
  and retention rules.
- [x] The persisted READ COMMITTED, REPEATABLE READ, and SERIALIZABLE matrix is
  green on PostgreSQL 17.
- [x] Independent Rust review reports no KI-027 blocker.
