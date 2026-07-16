# Scripts

This is the quick index for repository scripts. The detailed maintainer guide is
[Contributor Guide: Scripts](../docs/contributor_guide/scripts.mdx).

## Root Scripts

| Script | Purpose |
|---|---|
| `scripts/check_doc_references.py` | Validates local documentation links and references. |
| `scripts/check_docs_drift.sh` | Runs the aggregate documentation drift checks. |
| `scripts/check_secrets.sh` | Runs the gitleaks release gate over Git history and pending tracked changes. |
| `scripts/prepare_release_bundle.py` | Builds a deterministic source archive, SBOM, provenance, manifest, and checksums from an explicit Git ref. |
| `scripts/verify_release_bundle.py` | Verifies the complete release bundle and exact source identity without rebuilding. |
| `scripts/check_dependency_updates.py` | Audits pinned Cargo, Python, Docker, and Nix flake GitHub inputs and reports update candidates at least 6 hours old by default. |
| `scripts/check_rust_doc_map_drift.py` | Checks contributor documentation against the Rust source map. |
| `scripts/check_release_contract.py` | Checks the machine-readable 1.x SQL, GUC, diagnostic, distribution, and GQL-profile inventory. |
| `scripts/check_sql_api_drift.py` | Checks SQL API and GUC documentation against implementation. |
| `scripts/clean_generated_artifacts.sh` | Deletes generated local artifacts: `graph/target/`, `graph/fuzz/target/`, and `.DS_Store` files. |
| `scripts/inspect_pggraph_artifact.py` | Prints JSON metadata for a `.pggraph` persistence artifact. |
| `scripts/quickstart.sh` | Runs quickstart workflows: full local demo, install into existing Docker Postgres, local pgrx install, and one-click playground preset setup. |
| `scripts/build_docker_pggraph_package.sh` | Builds Docker-packaged pgGraph artifacts for one PostgreSQL major or all supported majors. |
| `scripts/copy_pggraph_package_to_docker_postgres.sh` | Copies an existing pgGraph package into a running PostgreSQL Docker container. |
| `scripts/install_into_docker_postgres.sh` | One-shot wrapper that builds a package and installs it into a running PostgreSQL Docker container. |

## Heavy Test Scripts

Heavy release and operational scripts live in `graph/tests/heavy/`. The main
entry point is:

```bash
cd graph
PG_VERSION_FEATURE=pg17 ./tests/heavy/run_release_gate.sh
```

The heavy inventory includes `playground_release_gate.sh`, which prepares the
Docker-backed Panama playground in CSR or mutable mode and verifies every shared
playground SQL example against fixed release-gate result summaries.

The aggregate release gate runs secret scanning by default. Run it directly
from any repository directory when needed:

```bash
scripts/check_secrets.sh all
scripts/check_secrets.sh changes
```

For PostgreSQL-major matrix validation:

```bash
cd graph
./tests/heavy/run_pg_matrix.sh
./tests/heavy/run_pg_matrix_docker.sh
```

See [Contributor Guide: Scripts](../docs/contributor_guide/scripts.mdx) and
[Testing And Release](../docs/contributor_guide/testing-release.mdx) for the full
inventory.
