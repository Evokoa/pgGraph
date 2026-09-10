# Package Manager Safety Wrapper

`sfw` is a safety wrapper for package-manager commands. It is strictly intended
to sit in front of supported package managers only during the installation of
new packages, ensuring that new dependency downloads and additions go through
the wrapper before reaching the underlying tool.

Before running any command to add or install a new dependency via a supported
package manager, prefix the command with `sfw`. Routine commands like build,
test, or run should be executed normally without the wrapper.

Supported package managers:

- JavaScript/TypeScript: `npm`, `yarn`, `pnpm`
- Python: `pip`, `uv`
- Rust: `cargo`

Examples:

- Installing new packages: use `sfw npm install --save some-package@1.33.7`,
  not `npm install --save some-package@1.33.7`
- Adding new packages: use `sfw cargo add serde`, not `cargo add serde`
- Installing new packages: use `sfw uv pip install flask`, not
  `uv pip install flask`
- Running routine commands: run `npm test` or `cargo build` normally

This applies strictly to subcommands that introduce new dependencies, such as
`install` and `add`. It does not apply to `update`, `fetch`, `build`, `test`,
`run`, and other routine package-manager subcommands unless the user explicitly
asks to bypass this rule.

# Current Project Scope

pgGraph is an Apache-2.0 PostgreSQL extension at version 1.2.0, written in Rust
2021 with Rust 1.96 and pgrx 0.19.1. PostgreSQL 14 through 18 are supported,
PostgreSQL 17 is the default development target, and the PostgreSQL 13 feature
is legacy best-effort rather than release-gated.

Use `graph/Cargo.toml` for version and feature facts. Use
`docs/contributor_guide/repository-map.mdx` for the current source layout and
`docs/contributor_guide/testing-release.mdx` for validation. Do not copy old
module or test inventories into this file.

# Repository Independence And Compatibility

This is an independent public open source repository. pgGraph must build,
install, test, run, and release without a sibling repository or another Evokoa
extension being present. Do not add local path dependencies or require a sibling
checkout.

pgGraph is designed to coexist with pgContext when both are installed. Keep that
integration optional and use documented PostgreSQL SQL and data contracts.
PostgreSQL source tables remain authoritative, and graph indexes remain derived
and rebuildable. Standalone pgGraph verification is required even when a change
also has a combined compatibility test.

# Local Planning And Task Files

Do not commit plans, detailed future scope, implementation sequences, acceptance
gates, benchmark workbooks, handoffs, progress notes, TODO files, or scratch
task lists to this public repository. Keep them outside a standalone clone or in
the external planning workspace configured by the contributor. Documentation
under `docs/` must describe behavior present in the current code, except for one
concise high-level roadmap.

# pgGraph Source-Of-Truth Principle

PostgreSQL source tables are the source of truth. pgGraph should feel like a
full graph layer, but graph-style operations must map to PostgreSQL-first
behavior whenever they mutate data.

When adding graph write features:

- expose graph ergonomics when labels, relationship types, properties, and
  identities can map cleanly to registered PostgreSQL tables and columns;
- route writes through PostgreSQL DML first, so constraints, triggers, ACLs,
  RLS, MVCC, and indexes remain authoritative;
- update pgGraph projections through sync, deltas, or rebuilds after the
  PostgreSQL write boundary accepts the change;
- reject unmapped graph writes with clear errors and mapping guidance;
- do not create durable graph-only state that can diverge from PostgreSQL as a
  second source of truth.
