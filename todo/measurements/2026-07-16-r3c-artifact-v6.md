# R3C Artifact v6 Tenant-Safe Follow-Up — 2026-07-16

## Result

Artifact v6 supersedes the unreleased v5 checkpoint. Its 512-byte manual
little-endian header describes 26 aligned sections. The additional sections
persist sorted tenanted-table OIDs, a lexical tenant dictionary, and dense
per-node tenant tokens; the header also persists the unidirectional-edge flag.
This keeps tenant filtering and traversal capability identical after reload.

Readers acquire a generation pin, resolve the current manifest's immutable
base artifact, and validate the exact checksum and version before installing
mapped views. Candidate checksum verification uses a fixed 64 KiB buffer.

## Focused Evidence

| Gate | Result |
|---|---|
| Persistence | PASS: 64 tests |
| Projection recovery | PASS: 13 tests |
| Mapped tenant index | PASS: 7 tests |
| Governed build runs | PASS: 11 tests |
| Build governor | PASS: memory and configured spill-disk ceilings enforced |
| Independent Rust review | P1/P2 findings addressed in bounded checksum, generation resolution, opaque preparation, and merge accounting |

The full Rust, PostgreSQL, documentation, and release gates remain R3D exit
evidence after direct source-run construction is integrated.
