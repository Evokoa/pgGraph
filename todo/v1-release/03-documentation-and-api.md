# 1.0 Documentation And Public API Plan

## Findings To Resolve

The public documentation contains useful material, but release status,
directional roadmap, known limitations, contributor internals, and historical
hardening detail overlap. Some behavior is repeated across the root README,
user guide, roadmap, Known Issues, and contributor guides. The scripts index
also has a broken relative link, and the docs drift gate currently interprets
generated `graph/fuzz/target/` paths as missing documentation references.
Contributor documentation also still describes the older four-field
`EdgeMutation` and claims layered neighbors lack stable relationship identity
in `engine-internals.mdx` and `traversal-search-paths.mdx`. Those statements no
longer match the segment and layered-identity implementation.

## Information Architecture

Use the following ownership rules:

| Information | Single source of truth |
|---|---|
| Installation and supported platforms/PostgreSQL majors | User guide installation page |
| First successful graph workflow | Quickstart page |
| SQL functions, GUCs, options, result shapes, SQLSTATEs | Generated/drift-checked API reference |
| Supported GQL profile | Generated capability page backed by conformance metadata |
| Current limitations and operational consequences | Known Issues |
| Shipped changes and migrations | Release notes |
| Future product direction | Roadmap |
| Internal architecture, scripts, tests, and release process | Contributor guide |
| Active implementation work and measurements | `todo/` only; never linked as public product docs |

README files should introduce and route readers to these sources instead of
duplicating detailed contracts.

## Cleanup Checklist

- [x] Rewrite the public roadmap around Current Baseline, 1.0 Focus, and
      Post-1.0 Direction; remove internal checkpoint language and stale alpha
      history from the public page.
- [x] Prune Known Issues to unresolved user-visible limitations. Move resolved
      migration history to release notes and internal engineering tasks to the
      1.0 plan.
- [x] Reconcile root `README.md`, `README_zh.md`, and docs quickstart so commands,
      supported versions, playground modes, and feature claims agree.
- [x] Generate API/configuration/diagnostic/GQL capability tables or make their
      existing hand-maintained sources drift-checked.
- [x] Add public compatibility, upgrade, rollback, deprecation, backup/restore,
      resource-limit, and production-readiness guidance.
- [x] Consolidate architecture descriptions and keep deep implementation detail
      in contributor docs rather than user pages.
- [x] Correct stale relationship-identity descriptions in contributor engine
      and traversal documentation, then add source-linked drift coverage for
      persisted and layered identity fields.
- [x] Split the oversized API reference, querying guide, and release-note
      history into navigable owned sections without changing their public URLs
      until redirects are available.
- [x] Fix `scripts/scripts.md` links and the docs reference checker treatment of
      generated or intentionally absent paths.
- [x] Run local-link, external-link, navigation, code-example, API drift, Rust
      source-map drift, spelling/style, and static rendered-MDX checks. The
      downstream docs host remains responsible for its application-shell build.
- [x] Review public tone: describe supported behavior and constraints directly;
      remove private planning phrases, phase history, and future-tense claims
      that read as current functionality.
- [x] Give every maintained example an owner, expected output/checksum, version
      requirements, and an automated smoke test; delete stale examples.
- [x] Add a release-note template and a 1.0 migration guide before the first RC.
- [x] State release maturity consistently: the current line is alpha, and 1.0
      becomes production-supported only after the RC checklist is green.
- [x] Define how `README_zh.md` is synchronized with the canonical English
      documentation and prevent unsupported-version or command drift.
- [x] Verify Rust public API docs, `# Errors`, `# Panics`, `# Safety`, doctests,
      and generated `cargo doc` navigation for the supported feature set.

## Documentation Release Gate

- No broken internal links or navigation entries.
- No undocumented exported SQL function, GUC, option, result contract, stable
  diagnostic, or supported GQL capability.
- No public claim exceeds conformance or release evidence.
- Quickstart and playground instructions pass from a clean packaged install.
- Known Issues contains every unresolved user-visible P0/P1 limitation and no
  resolved historical item.
- Upgrade, rebuild, rollback, backup/restore, and compatibility guidance is
  complete for PostgreSQL 14-18.
