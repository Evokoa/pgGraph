#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Build a PGXN source distribution archive from the current git checkout.

Usage:
  scripts/build_pgxn_dist.sh TAG [OUT_DIR]

Arguments:
  TAG      Release tag in vX.Y.Z form.
  OUT_DIR  Directory for the generated archive. Default: dist.

The archive is intended for release automation or manual upload through the
PGXN web interface.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage >&2
  exit 2
fi

tag="$1"
out_dir="${2:-dist}"

if [[ ! "$tag" =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "TAG must use vX.Y.Z form: $tag" >&2
  exit 2
fi

version="${tag#v}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
archive_dir="$repo_root/$out_dir"
archive_name="pgGraph-${version}.zip"

mkdir -p "$archive_dir"
rm -f "$archive_dir/$archive_name"

git -C "$repo_root" archive \
  --format=zip \
  --prefix="pgGraph-${version}/" \
  --output="$archive_dir/$archive_name" \
  HEAD

echo "$out_dir/$archive_name"
