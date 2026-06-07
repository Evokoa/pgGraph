#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

# cargo-fuzz sets RUSTFLAGS itself, which displaces graph/.cargo/config.toml.
# Keep macOS pgrx extension symbols unresolved until PostgreSQL loads them.
if [[ "$(uname -s)" == "Darwin" ]]; then
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-undefined,dynamic_lookup"
fi

cargo +nightly fuzz run load_projection_manifest fuzz/corpus/load_projection_manifest -- -runs=0
cargo +nightly fuzz run load_projection_segment fuzz/corpus/load_projection_segment -- -runs=0
