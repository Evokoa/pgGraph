#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Build pgGraph package artifacts with Docker.

Usage:
  scripts/build_docker_pggraph_package.sh [PG_MAJOR|all] [OUT_DIR]

Arguments:
  PG_MAJOR  PostgreSQL major version to build for. Default: 17.
            Use "all" to build every version in PG_VERSIONS.
  OUT_DIR   Directory where graph-pg<major>/ packages are copied.
            Default: target/docker-packages.

Environment:
  BUILDER_IMAGE_PREFIX  Builder image prefix. Default: pggraph-builder
  PG_VERSIONS           Versions used when PG_MAJOR is "all".
                        Default: 14 15 16 17 18

Examples:
  scripts/build_docker_pggraph_package.sh 17
  scripts/build_docker_pggraph_package.sh all target/docker-packages
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -gt 2 ]]; then
  usage >&2
  exit 2
fi

pg_major="${1:-17}"
out_dir="${2:-target/docker-packages}"
builder_image_prefix="${BUILDER_IMAGE_PREFIX:-pggraph-builder}"
pg_versions="${PG_VERSIONS:-14 15 16 17 18}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
builder_container=""

cleanup() {
  if [[ -n "$builder_container" ]]; then
    docker rm -f "$builder_container" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

validate_pg_major() {
  case "$1" in
    14|15|16|17|18) ;;
    *)
      echo "Unsupported PostgreSQL major: $1" >&2
      echo "Supported versions: 14 15 16 17 18" >&2
      exit 2
      ;;
  esac
}

build_one() {
  local major="$1"
  local image="${builder_image_prefix}:pg${major}"
  local package_name="graph-pg${major}"
  local destination="$repo_root/$out_dir/$package_name"

  validate_pg_major "$major"

  echo "Building pgGraph package for PostgreSQL ${major}..."
  docker build \
    --build-arg "PG_MAJOR=${major}" \
    --target builder \
    -t "$image" \
    "$repo_root"

  if [[ -n "$builder_container" ]]; then
    docker rm -f "$builder_container" >/dev/null 2>&1 || true
    builder_container=""
  fi

  builder_container="$(docker create "$image")"
  rm -rf "$destination"
  mkdir -p "$repo_root/$out_dir"
  docker cp "$builder_container:/src/graph/target/release/${package_name}" "$destination"

  echo "Wrote $out_dir/$package_name"
}

if [[ "$pg_major" == "all" ]]; then
  for major in $pg_versions; do
    build_one "$major"
  done
else
  build_one "$pg_major"
fi
