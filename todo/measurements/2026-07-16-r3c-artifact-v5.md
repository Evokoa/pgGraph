# R3C Artifact v5 And Bounded Load — 2026-07-16

> Superseded before release by artifact v6, which adds exact tenant metadata
> and the unidirectional-edge capability flag. See
> `2026-07-16-r3c-artifact-v6.md`.

## Result

Artifact v5 is complete as the validated mapped-load boundary. Its manual
little-endian header describes 23 aligned sections and rejects incompatible,
truncated, overlapping, noncanonical, or semantically inconsistent data before
typed mapped views are created.

The immutable base keeps node metadata, both CSR directions and their
relationship IDs, filter values and lexical dictionaries, resolution data,
and relationship identity descriptors and keys mapped. Only bounded labels,
column metadata, and sparse mutation overlays remain on the backend heap.

## Evidence

| Gate | Result |
|---|---|
| Rust library suite | PASS: 817 passed, 0 failed, 1 intentionally ignored |
| Focused persistence suite | PASS: 57 tests, including CRC-valid topology, count, weight, resolution, filter-range, registry, and identity corruption |
| Mapped metadata budget | PASS: 512 empty filter columns reject at a one-byte-short metadata boundary before load allocation |
| Clippy | PASS with `-D warnings` across all targets and `pg17 development` |
| Rust documentation | PASS with rustdoc warnings denied; doctests pass |
| Public documentation drift | PASS |
| Independent Rust review | PASS after two closure rounds; no remaining R3C blockers |

Direct source-run streaming into a generation-specific artifact is deliberately
the R3D checkpoint. The legacy writer is governed and fallible, but it remains
an owned-engine compatibility and differential path until R3D is green.
