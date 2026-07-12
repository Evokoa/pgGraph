# 1.0 Compatibility And Lifecycle Plan

## Objective

Make the 1.0 contract predictable for users installing, upgrading, operating,
and integrating pgGraph. Compatibility is tested behavior, not an informal
promise.

## Versioning Policy To Publish

- Use semantic versioning for the extension distribution and public docs.
- Patch releases preserve documented SQL signatures, result columns, defaults,
  SQLSTATE classes, catalog meaning, and configuration behavior. They continue
  to read artifacts from the same minor line unless a security or corruption
  fix makes that unsafe.
- Minor 1.x releases may add optional functions, GQL capabilities, and
  configuration values. Existing signatures, result shapes, and defaults do
  not change; use a new function or explicitly versioned contract when an
  additive result field would still break dependent views or clients.
- Incompatible changes wait for 2.0 unless a security or data-corruption issue
  requires an earlier break. Such a break requires prominent release notes,
  an upgrade preflight, an actionable error, and a rollback/rebuild procedure.
- PostgreSQL major support is explicit per pgGraph release. Version 1.0 targets
  PostgreSQL 14 through 18; PostgreSQL 19 is not implied by the 1.0 version.
- A 1.x minor may retire a PostgreSQL major only after upstream community
  support ends, with at least one minor release and 90 days of notice. The last
  compatible pgGraph minor receives security fixes under the published support
  policy; a patch release never drops a PostgreSQL major.

## Compatibility Surfaces

Create a generated or drift-checked inventory that assigns an owner and test to
each surface:

| Surface | 1.x rule | Required evidence |
|---|---|---|
| SQL functions and arguments | Preserve names, types, defaults, volatility, security context, permissions, and result-column order. | API snapshot plus upgrade tests on every supported PostgreSQL major. |
| SQLSTATE and diagnostics | Preserve standard SQLSTATE class and stable `PGxxx` diagnostic identifier for documented failures. Text may improve without breaking machine-readable fields. | Negative contract tests and docs inventory. |
| GUCs and environment variables | Preserve accepted values and defaults. Additions are safe; rename/removal follows deprecation policy. | Configuration snapshot, invalid-value tests, upgrade test. |
| Extension catalogs | Migrate through versioned extension SQL. Never require users to edit internal tables. Unknown or incompatible state fails closed. | Sequential upgrade tests from every supported 1.x release fixture. |
| Projection artifacts and segments | Readers validate version/features before mapping. An incompatible derived artifact returns a rebuild-required error without replacing the last good generation; it is never silently reinterpreted. | Golden fixtures, corrupt/future-version tests, rebuild and rollback tests. |
| Source registrations | Preserve PostgreSQL OID-based identity and mapping semantics across supported rename/search-path changes; fail closed after incompatible drop/recreate or key drift. | Catalog and source-schema migration matrix. |
| GQL and compatibility syntax | Freeze the advertised profile. New syntax is additive; changed semantics require a compatibility decision and conformance-row update. | Machine-readable capability registry and differential tests. |
| CLI/scripts and container inputs | Stabilize documented entry points, flags, required variables, exit semantics, and user-visible output locations. Internal helper paths and implementation layout may change. | Shell interface tests and release smoke tests. |
| Playground examples | Examples are documentation contracts and must execute against the packaged extension. | Dataset checksum plus automated query-result assertions. |

## Deprecation Policy

1. Mark the old surface deprecated in release notes and public reference docs.
2. Provide a replacement and, where practical, a compatibility alias.
3. Emit a non-sensitive warning at most once per relevant session or operation.
4. Keep the old surface for at least two 1.x minor releases and six months,
   whichever is longer.
5. Remove only in a major release, except for security/data-loss emergencies.
6. Test both deprecated and replacement paths throughout the window.

Internal catalogs and undocumented implementation details are not public APIs,
but upgrades must still migrate them safely without data loss.

## Upgrade And Rollback Matrix

- Test fresh installation on PostgreSQL 14, 15, 16, 17, and 18.
- Before 1.0 RC, create a final pre-1.0 fixture and prove upgrade to 1.0.
- For each subsequent 1.x release, retain fixtures for the oldest supported 1.x
  version and the immediately previous minor/patch release.
- Test extension SQL upgrade, server restart, artifact load, forced rebuild,
  source-schema validation, backup/restore, and rollback to the previous binary
  when catalogs and artifacts remain compatible.
- When binary rollback is unsafe, preflight must stop the upgrade before any
  irreversible change and explain the supported restore procedure.
- Never silently reinterpret an old artifact, catalog enum, relationship key,
  filter identity, generation, or watermark.

## R0 Checklist

- [x] Approve the supported PostgreSQL 14-18 matrix and platform/package matrix.
- [x] Decide the exact 1.0 SQL and GQL supported profile from current behavior.
- [x] Generate the public API/configuration/diagnostic inventory.
- [x] Choose the artifact compatibility window and golden fixture policy.
- [x] Add the signed alpha-to-1.0 source-preserving migration fixture and define
      versioned 1.x extension-upgrade fixtures beginning with the first real
      1.x-to-1.x release pair.
- [x] Publish the compatibility and deprecation policy in public docs.
- [x] Add a release-note template with compatibility, migration, rebuild, and
      rollback sections.
- [x] Add a breaking-change review checkbox to contribution/release guidance.
