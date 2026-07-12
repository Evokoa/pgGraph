# 1.0 Release Candidate Checklist

## Product And Compatibility

- [ ] The supported PostgreSQL, platform, packaging, SQL, GQL, configuration,
      artifact, and operations contract is frozen and published.
- [ ] API and conformance inventories match implementation with no drift.
- [ ] All deprecations, migrations, rebuild requirements, and rollback limits
      appear in release notes and the migration guide.
- [ ] No unresolved P0 issue; every accepted P1 risk has a public limitation,
      owner, mitigation, and post-release target.

## Correctness, Security, And Transactions

- [ ] Full PG14-18 unit, pgrx, SQL contract, ACL/RLS, transaction, concurrency,
      crash, and source-schema matrix is green.
- [ ] Miri-eligible mapped validation, PostgreSQL-process sanitizer, fuzz,
      unsafe allowlist, and checked-cast gates are green.
- [ ] Parallel relationships, same-name filters, savepoints, visibility without
      hydration, competing publication, and invalid replacement regressions are
      permanent.

## Resources, Persistence, And Operations

- [ ] Enforced build/load/query/sync/compaction memory and disk thresholds pass
      on declared production-shaped datasets.
- [ ] Fresh install, extension upgrade, artifact upgrade/rebuild, binary/catalog
      rollback expectations, backup/restore, and crash recovery are green.
- [ ] Repeated generation publication, reader pinning, compaction, garbage
      collection, and cleanup leave no leaked files or unbounded state.
- [ ] Status, health, logs, diagnostics, cancellation, and recovery procedures
      are documented and exercised.

## Distribution And Documentation

- [ ] Source, PGXN, Docker/package, checksum/SBOM/signing, license, and dependency
      policy gates are green for every supported target.
- [ ] The gitleaks history and pending-change scans pass with no unreviewed
      secret finding, and any false-positive exception is narrow and documented.
- [ ] Quickstart, playground, examples, and operations guides pass from clean
      release artifacts rather than a development checkout.
- [ ] Public docs render without broken links, stale claims, duplicated
      contracts, or internal planning language.
- [ ] CI required checks, nightly gates, and RC evidence retention are enabled
      and documented.

## Release Evidence

- [ ] Archive commit, toolchain, dependency lockfile, PostgreSQL versions,
      platform/container digests, commands, durations, logs, checksums,
      correctness results, RSS/PSS, spill, latency percentiles, and known
      deviations.
- [ ] A second reviewer verifies the evidence against this checklist.
- [ ] Tag and publish only from a clean, reproducible commit after final smoke
      installation succeeds.
