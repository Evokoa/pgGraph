# Regression Baseline: 2026-05-29

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

## Context

- Branch: `feat/mutable-graph-projections`
- Git commit: `ff08028`
- Captured at: `2026-05-29T14:20:00Z`
- Purpose: baseline current regression state before openCypher, GQL,
  SQL/PGQ, and mutable overlay planning work turns into implementation.

## Worktree State

Tracked dirty file not included in this baseline work:

- `docs/known-issues.mdx`

New baseline/planning files under `todo/` are intended to be tracked on this
branch and deleted before merging to `main`.

## Baseline Commands

### Cargo Through `sfw`

Command:

```sh
cd graph
sfw cargo test --features pg17
```

Result:

```text
Protected by Socket Firewall
```

Exit code: `0`

Notes:

- The local `sfw` wrapper returned immediately with only its banner.
- `sfw cargo --version` behaved the same way.
- Because the repository instructions require package-manager commands to be
  prefixed with `sfw`, Cargo was not run directly.

### Existing Test Binary, Pure Rust Surface

Command:

```sh
cd graph
target/debug/deps/graph-022f1ec96eb5a8d6 --skip tests::pg_
```

Binary SHA-256:

```text
66b7a75b5a10572f9ee6567690c722d67ffdd16a3c3d1bf12ae7fca8474b47db
```

Result:

```text
test result: ok. 286 passed; 0 failed; 1 ignored; 0 measured; 107 filtered out; finished in 4.77s
```

Notes:

- This uses an already-built test binary in `graph/target/debug/deps`.
- It is useful as a local pure-Rust regression comparison point.
- It does not prove the current source rebuilt successfully.

### Existing Test Binary, Full Surface Attempt

Command:

```sh
graph/target/debug/deps/graph-022f1ec96eb5a8d6 --nocapture
```

Result:

```text
test result: FAILED. 286 passed; 107 failed; 1 ignored; 0 measured; 0 filtered out; finished in 5.02s
```

Primary failure reason:

```text
Could not initialize test framework: failed to bind to an ephemeral port for test Postgres
Caused by:
    Operation not permitted (os error 1)
```

Follow-on failures were pgrx test mutex failures after the first pgrx framework
initialization failure.

## Comparison Guidance

For later regression comparison, prefer:

```sh
cd graph
sfw cargo test --features pg17
sfw cargo pgrx test pg17
```

If `sfw` continues to return only the wrapper banner, either fix the local
wrapper behavior or explicitly decide whether to bypass it for validation.

For pure Rust comparison against this captured baseline, the comparable result
is:

```text
286 passed; 0 failed; 1 ignored; 107 filtered out
```

For pgrx SQL comparison, this baseline does not contain a successful pgrx run.
