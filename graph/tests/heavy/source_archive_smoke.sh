#!/usr/bin/env bash
set -euo pipefail

PG_VERSIONS="${PG_VERSIONS:-14 15 16 17 18}"
RUN_PUBLIC_EXAMPLES="${RUN_PUBLIC_EXAMPLES:-1}"
ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/pggraph-source-archive.XXXXXX")"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/graph/Cargo.toml" | head -1)"
TAG="${1:-v${VERSION}}"
source_root=""
runtime_image="pggraph:source-archive-runtime-${VERSION}"
playground_container="pggraph-source-archive-playground-${VERSION//./-}"

cleanup() {
  docker rm -f "$playground_container" >/dev/null 2>&1 || true
  if [[ -n "$source_root" ]]; then
    PGGRAPH_QUICKSTART_IMAGE="$runtime_image" \
      PGGRAPH_QUICKSTART_PORT=55433 \
      COMPOSE_PROJECT_NAME=pggraph_source_archive \
      "$source_root/scripts/quickstart.sh" clean >/dev/null 2>&1 || true
  fi
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Usage: $0 [vX.Y.Z]" >&2
  exit 2
fi
if [[ "$TAG" != "v${VERSION}" ]]; then
  echo "Tag ${TAG} does not match graph/Cargo.toml version ${VERSION}" >&2
  exit 2
fi
if [[ -n "$(git -C "$ROOT_DIR" status --porcelain --untracked-files=no)" ]]; then
  echo "Source archive smoke requires a clean tracked worktree and index" >&2
  exit 2
fi

"$ROOT_DIR/scripts/build_pgxn_dist.sh" "$TAG" "$WORKDIR"
archive="$WORKDIR/pgGraph-${TAG#v}.zip"
unzip -q "$archive" -d "$WORKDIR/source"
source_root="$WORKDIR/source/pgGraph-${TAG#v}"

docker build \
  --build-arg "PG_VERSIONS=${PG_VERSIONS}" \
  --build-arg RUN_RUST_TESTS=0 \
  --build-arg RUN_PGRX_SQL=0 \
  --build-arg RUN_GQL_WRITE_MATRIX=0 \
  --build-arg RUN_PACKAGE_INSTALL_MATRIX=1 \
  -f "$source_root/graph/tests/heavy/Dockerfile.pg-matrix" \
  -t "pggraph:source-archive-build-${TAG#v}" \
  "$source_root"

transition_context="$WORKDIR/transition"
mkdir -p "$transition_context/alpha-graph" "$transition_context/fixture"
git -C "$ROOT_DIR" archive v0.1.8:graph | tar -x -C "$transition_context/alpha-graph"
cp -R "$source_root/release/fixtures/alpha-to-v1.0/." "$transition_context/fixture/"
docker build \
  --build-arg "BASE_IMAGE=pggraph:source-archive-build-${TAG#v}" \
  -f "$source_root/graph/tests/heavy/Dockerfile.alpha-transition" \
  -t "pggraph:alpha-transition-${TAG#v}" \
  "$transition_context"

if [[ "$RUN_PUBLIC_EXAMPLES" == "1" ]]; then
  docker build --build-arg PG_MAJOR=17 -t "$runtime_image" "$source_root"

  PGGRAPH_QUICKSTART_IMAGE="$runtime_image" \
    PGGRAPH_QUICKSTART_PORT=55433 \
    COMPOSE_PROJECT_NAME=pggraph_source_archive \
    "$source_root/scripts/quickstart.sh" quickstart
  PGGRAPH_QUICKSTART_IMAGE="$runtime_image" \
    PGGRAPH_QUICKSTART_PORT=55433 \
    COMPOSE_PROJECT_NAME=pggraph_source_archive \
    "$source_root/scripts/quickstart.sh" clean

  PGGRAPH_IMAGE_NAME="$runtime_image" \
    PGGRAPH_CONTAINER_NAME="$playground_container" \
    PGGRAPH_PG_PORT=55434 \
    PGGRAPH_PLAYGROUND_YES=1 \
    "$source_root/graph/tests/heavy/playground_release_gate.sh"
  PGGRAPH_IMAGE_NAME="$runtime_image" \
    PGGRAPH_CONTAINER_NAME="$playground_container" \
    PGGRAPH_PG_PORT=55434 \
    PGGRAPH_PLAYGROUND_YES=1 \
    PGGRAPH_PLAYGROUND_MODE=mutable \
    "$source_root/graph/tests/heavy/playground_release_gate.sh"
fi

echo "Clean source archive package and public-example smoke passed for $TAG on PostgreSQL: $PG_VERSIONS"
