#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Build a PGXN source distribution archive from the current git checkout.

Usage:
  scripts/build_pgxn_dist.sh TAG [OUT_DIR] [--ref GIT_REF]

Arguments:
  TAG      Release tag in vX.Y.Z form.
  OUT_DIR  Directory for the generated archive. Default: dist.
  GIT_REF  Commit or tag to archive. Default: HEAD.

The archive is intended for release automation or manual upload through the
PGXN web interface.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -lt 1 || $# -gt 4 ]]; then
  usage >&2
  exit 2
fi

tag="$1"
shift
out_dir="dist"
source_ref="HEAD"
if (( $# > 0 )) && [[ "$1" != "--ref" ]]; then
  out_dir="$1"
  shift
fi
if (( $# > 0 )); then
  if [[ "$1" != "--ref" || $# -ne 2 ]]; then
    usage >&2
    exit 2
  fi
  source_ref="$2"
fi

if [[ ! "$tag" =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "TAG must use vX.Y.Z form: $tag" >&2
  exit 2
fi

version="${tag#v}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$out_dir" = /* ]]; then
  archive_dir="$out_dir"
else
  archive_dir="$repo_root/$out_dir"
fi
archive_name="pgGraph-${version}.zip"
source_commit="$(git -C "$repo_root" rev-parse --verify "${source_ref}^{commit}")" || {
  echo "Unable to resolve release source ref: $source_ref" >&2
  exit 2
}
source_version="$(git -C "$repo_root" show "$source_commit:graph/Cargo.toml" | sed -n 's/^version = "\([^"]*\)"/\1/p' | head -n 1)"
if [[ "$source_version" != "$version" ]]; then
  echo "Source ref $source_ref declares graph version $source_version, expected $version" >&2
  exit 2
fi

mkdir -p "$archive_dir"
rm -f "$archive_dir/$archive_name"

git -C "$repo_root" archive \
  --format=zip \
  --prefix="pgGraph-${version}/" \
  --output="$archive_dir/$archive_name" \
  "$source_commit"

echo "$archive_dir/$archive_name"
