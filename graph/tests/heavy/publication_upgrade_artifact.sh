#!/usr/bin/env bash
set -euo pipefail

# Run through scripts/with_disposable_postgres.sh with exclusive ownership of
# the selected PostgreSQL installation. This creates local test packages only.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$ROOT_DIR/scripts/lib/pggraph-common.sh"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_CONFIG="${PG_CONFIG:-pg_config}"
DB_PREFIX="${DB_PREFIX:-pggraph_publication_upgrade}"
export DB_PREFIX
pggraph_validate_database_name "$DB_PREFIX"
[[ "$DB_PREFIX" != postgres && ${#DB_PREFIX} -le 63 ]] || exit 2
[[ "$PG_VERSION_FEATURE" =~ ^pg(14|15|16|17|18)$ ]] || exit 2
pggraph_validate_disposable_cluster "$DB_PREFIX" postgres || exit 2

previous_commit="$(git -C "$ROOT_DIR" rev-parse --verify 'v1.2.0^{commit}')"
candidate_commit="$(git -C "$ROOT_DIR" rev-parse HEAD)"
workdir="$(pggraph_make_temp_dir publication-upgrade)"
candidate_ready=0

install_package() {
  local package="$1" sharedir pkglibdir library
  sharedir="$("$PG_CONFIG" --sharedir)"
  pkglibdir="$("$PG_CONFIG" --pkglibdir)"
  library="$package$pkglibdir/graph.so"
  [[ -f "$library" ]] || library="$package$pkglibdir/graph.dylib"
  [[ -s "$library" && -s "$package$sharedir/extension/graph.control" ]] || return 1
  cp "$package$sharedir/extension/"graph--*.sql "$sharedir/extension/"
  cp "$package$sharedir/extension/graph.control" "$sharedir/extension/"
  cp "$library" "$pkglibdir/"
}

cleanup() {
  local code=$?
  trap - EXIT
  if [[ "$candidate_ready" == 1 ]]; then
    install_package "$workdir/candidate-package" || code=1
  fi
  rm -rf "$workdir"
  exit "$code"
}
trap cleanup EXIT

git -C "$ROOT_DIR" archive "$previous_commit" graph | tar -x -C "$workdir"
CARGO_TARGET_DIR="$workdir/previous-target" cargo pgrx package \
  --manifest-path "$workdir/graph/Cargo.toml" --pg-config "$PG_CONFIG" \
  --out-dir "$workdir/previous-package" --no-default-features --features "$PG_VERSION_FEATURE"
cargo pgrx package --manifest-path "$ROOT_DIR/graph/Cargo.toml" \
  --pg-config "$PG_CONFIG" --out-dir "$workdir/candidate-package" \
  --no-default-features --features "$PG_VERSION_FEATURE"
candidate_ready=1

# Preserve exact packaged SQL and package-file digests when evidence is requested.
if [[ -n "${UPGRADE_OUTPUT_DIR:-}" ]]; then
  mkdir "$UPGRADE_OUTPUT_DIR"
  python3 - "$workdir" "$UPGRADE_OUTPUT_DIR" "$previous_commit" "$candidate_commit" <<'PY'
import hashlib
import json
import shutil
import sys
from pathlib import Path
work, output = map(Path, sys.argv[1:3])
packages = {}
for name in ("previous-package", "candidate-package"):
    package = work / name
    packages[name] = {str(path.relative_to(package)): hashlib.sha256(path.read_bytes()).hexdigest()
                      for path in sorted(package.rglob("*")) if path.is_file()}
    sql = output / name
    sql.mkdir()
    for path in package.rglob("graph*.sql"):
        shutil.copyfile(path, sql / path.name)
with (output / "packages.json").open("x") as handle:
    json.dump({"previous_commit": sys.argv[3], "candidate_commit": sys.argv[4],
               "packages": packages}, handle, indent=2)
    handle.write("\n")
PY
fi

install_package "$workdir/previous-package"
bash "$ROOT_DIR/graph/tests/heavy/publication_upgrade.sh" prepare-1.2.0
# Preparation's psql backends have exited before replacing the shared library.
install_package "$workdir/candidate-package"
bash "$ROOT_DIR/graph/tests/heavy/publication_upgrade.sh" verify-1.2.1
printf 'Packaged 1.2.0-to-1.2.1 publication upgrade passed for %s\n' "$PG_VERSION_FEATURE"
