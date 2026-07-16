#!/usr/bin/env bash
# Run every static documentation drift check.
#
# Usage:
#   scripts/check_docs_drift.sh
#
# This script does not need a running PostgreSQL instance. It validates local
# docs links/path references, SQL API/GUC documentation, and Rust source-map
# coverage. Run it from any directory inside the repository.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

python3 scripts/check_doc_references.py
python3 scripts/check_sql_api_drift.py
python3 scripts/check_rust_doc_map_drift.py
python3 scripts/check_release_contract.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_public_docs.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_fixture_integrity.py
