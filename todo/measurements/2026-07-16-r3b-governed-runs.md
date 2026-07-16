# R3B Governed Build Runs — 2026-07-16

## Result

The standalone run-file and fixed-fanout merge boundary is complete and ready
for the direct persisted builder. It introduces no dependency and uses the
existing CRC and resource-governor facilities.

## Evidence

- Seven run tests pass for every run kind, a nine-input/fanout-two merge with
  at least four passes, duplicate preservation, pre-read payload corruption,
  oversized records, disk and file limits, private permissions, and narrow
  abandoned-workspace cleanup.
- Disk-lease growth and explicit release accounting passes its focused resource
  test.
- `cargo clippy --features pg17 --no-default-features -- -D warnings` passes.

The merge holds one bounded reader buffer and at most one current plus one
sorted-order lookbehind record per input. Output disk is reserved while inputs
remain charged, so peak spill usage reflects the real merge boundary. Run
headers are exactly 128 bytes and bind kind, graph, build, catalog fingerprint,
record count, payload length, and checksums.
