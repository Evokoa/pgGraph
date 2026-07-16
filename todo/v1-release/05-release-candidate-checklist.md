# 1.0 Release Candidate Checklist

Status: complete for a release-ready, unpublished source commit. Tag signing,
repository-rule verification, and publication are release-owner handoff
actions and are not claims that 1.0.0 has already been published.

## Product And Compatibility

- [x] The supported PostgreSQL, platform, packaging, SQL, GQL, configuration,
      artifact, and operations contract is frozen and documented in the
      release candidate.
- [x] API and conformance inventories match implementation with no drift.
- [x] All deprecations, migrations, rebuild requirements, and rollback limits
      appear in release notes and the migration guide.
- [x] No unresolved P0 issue; every accepted P1 risk has a public limitation,
      owner, mitigation, and post-release target.

## Correctness, Security, And Transactions

- [x] Full PG14-18 unit, pgrx, SQL contract, ACL/RLS, transaction, concurrency,
      crash, and source-schema matrix is green.
- [x] Miri-eligible mapped validation, PostgreSQL-process sanitizer, fuzz,
      unsafe allowlist, and checked-cast gates are green.
- [x] Parallel relationships, same-name filters, savepoints, visibility without
      hydration, competing publication, and invalid replacement regressions are
      permanent.

## Resources, Persistence, And Operations

- [x] Enforced build/load/query/sync/compaction memory and disk thresholds pass
      on declared production-shaped datasets.
- [x] Fresh install, extension upgrade, artifact upgrade/rebuild, binary/catalog
      rollback expectations, backup/restore, and crash recovery are green.
- [x] Repeated generation publication, reader pinning, compaction, garbage
      collection, and cleanup leave no leaked files or unbounded state.
- [x] Status, health, logs, diagnostics, cancellation, and recovery procedures
      are documented and exercised.

## Distribution And Documentation

- [x] Source, PGXN, Docker/package, checksum/SBOM/signing, license, and dependency
      policy gates are green for every supported target.
- [x] The gitleaks history and pending-change scans pass with no unreviewed
      secret finding, and any false-positive exception is narrow and documented.
- [x] Quickstart, playground, examples, and operations guides pass from clean
      release artifacts rather than a development checkout.
- [x] Public docs render without broken links, stale claims, duplicated
      contracts, or internal planning language.
- [x] CI definitions for required checks, nightly gates, and RC evidence
      retention are versioned and tested. The handoff explicitly requires the
      release owner to verify repository rules, runner availability, and
      protected environments before publication.

## Release Evidence

- [x] Archive commit, toolchain, dependency lockfile, PostgreSQL versions,
      platform/container digests, commands, durations, logs, checksums,
      correctness results, RSS/PSS, spill, latency percentiles, and known
      deviations.
- [x] A second reviewer verifies the evidence against this checklist.
- [x] The release-owner handoff requires a signed tag and publication only from
      the reviewed clean commit after final smoke installation succeeds.
