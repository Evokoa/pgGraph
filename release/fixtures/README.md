# Release Compatibility Fixtures

Compatibility fixtures are immutable once published. Each release directory
captures the contract needed to prove upgrade, rebuild, and rollback behavior
without treating derived graph artifacts as authoritative data.

## Policy

- Keep the final pre-1.0 fixture and, for 1.x, the oldest supported minor plus
  the immediately previous release.
- Store source-schema/data setup, graph registration, representative queries,
  and expected checksums as text fixtures.
- Store binary `.pggraph`, manifest, and segment fixtures only when testing a
  declared readable format. Record the producing pgGraph/PostgreSQL versions,
  platform, SHA-256, and expected load or rebuild result.
- Never regenerate an existing fixture in place. Add a new versioned fixture.
- Corrupt, truncated, and future-version fixtures are synthetic and must fail
  closed without replacing the last valid generation.
- Artifact incompatibility is allowed only through an explicit rebuild-required
  result. PostgreSQL source rows and constraints remain authoritative.

The `alpha-to-v1.0` fixture is produced from signed tag `v0.1.8` and defines the
supported alpha-to-1.0 migration baseline. The alpha line did not promise
in-place catalog or artifact compatibility, so the supported transition
preserves source tables, installs 1.0 cleanly, reapplies reviewed registration,
and rebuilds derived state. `graph/tests/heavy/alpha_to_v1_fixture.sh` can prepare
the source under the alpha package and verify it after installing 1.0. The 1.x
line uses versioned extension migrations beginning with the first 1.x-to-1.x
release pair; no fictional migration pair is required for the initial 1.0.0
release.
