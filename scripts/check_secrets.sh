#!/usr/bin/env bash
# Scan pgGraph Git history and pending tracked changes for committed secrets.
#
# Usage:
#   scripts/check_secrets.sh [all|history|changes]
#
# The default `all` mode scans every reachable commit and the current tracked
# diff. Findings are redacted from terminal output. Set GITLEAKS_TIMEOUT_SECONDS
# to override the 300-second timeout applied to each scan.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
MODE="${1:-all}"
TIMEOUT_SECONDS="${GITLEAKS_TIMEOUT_SECONDS:-300}"

usage() {
  cat <<'USAGE'
Scan pgGraph for secrets with gitleaks.

Usage:
  scripts/check_secrets.sh [all|history|changes]

Modes:
  all      Scan full Git history and pending tracked changes. Default.
  history  Scan every reachable commit.
  changes  Scan pending tracked changes for pre-commit use.

Environment:
  GITLEAKS_TIMEOUT_SECONDS  Per-scan timeout in seconds. Default: 300.
USAGE
}

case "${MODE}" in
  all|history|changes)
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    echo "Error: unsupported mode '${MODE}'." >&2
    usage >&2
    exit 2
    ;;
esac

if ! command -v gitleaks >/dev/null 2>&1; then
  echo "Error: gitleaks is required to run the secret-scanning gate." >&2
  echo "Install gitleaks with your platform package manager, then rerun this script." >&2
  exit 127
fi

if [[ ! "${TIMEOUT_SECONDS}" =~ ^[1-9][0-9]*$ ]]; then
  echo "Error: GITLEAKS_TIMEOUT_SECONDS must be a positive integer." >&2
  exit 2
fi

cd "${ROOT_DIR}"

gitleaks_common=(
  --redact
  --no-banner
  --no-color
  --timeout "${TIMEOUT_SECONDS}"
)

if [[ "${MODE}" == "all" || "${MODE}" == "history" ]]; then
  echo "Scanning full Git history for secrets..."
  gitleaks git "${gitleaks_common[@]}" .
fi

if [[ "${MODE}" == "all" || "${MODE}" == "changes" ]]; then
  echo "Scanning staged changes for secrets..."
  gitleaks git "${gitleaks_common[@]}" --staged .
  echo "Scanning unstaged tracked changes for secrets..."
  gitleaks git "${gitleaks_common[@]}" --pre-commit .
fi

echo "Secret scan passed (${MODE})."
