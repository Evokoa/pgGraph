"""Exercise package upgrade ordering and failure restoration without PostgreSQL."""
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def executable(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/bash\nset -euo pipefail\n" + text)
    path.chmod(0o755)


class PublicationUpgradeTests(unittest.TestCase):
    def test_artifact_gate_refuses_unowned_cluster_before_cargo(self):
        environment = {key: value for key, value in os.environ.items()
                       if key not in {"PGDATA", "PGGRAPH_DISPOSABLE_CLUSTER_TOKEN",
                                      "PGGRAPH_DISPOSABLE_CLUSTER_SENTINEL"}}
        result = subprocess.run(["bash", str(ROOT / "graph/tests/heavy/publication_upgrade_artifact.sh")],
                                env=environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("disposable-cluster wrapper", result.stderr)

    def test_boundary_can_connect_before_owned_database_exists(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            data = root / "data"
            data.mkdir()
            (data / ".pggraph-disposable-cluster").write_text("synthetic-token\n")
            (data / "postmaster.pid").write_text(str(os.getpid()) + "\n")
            executable(root / "bin/psql", "printf '%s\\n' \"$*\" > \"$TEST_CALLS\"\nprintf '%s\\n' \"$TEST_PGDATA\"\n")
            executable(root / "bin/ps", "printf 'postgres -D %s\\n' \"$TEST_PGDATA\"\n")
            environment = dict(os.environ, PATH=str(root / "bin") + ":/usr/bin:/bin",
                               PGDATA=str(data), TEST_PGDATA=str(data), TEST_CALLS=str(root / "calls"),
                               PGGRAPH_DISPOSABLE_CLUSTER_TOKEN="synthetic-token",
                               PGGRAPH_DISPOSABLE_CLUSTER_SENTINEL=str(data / ".pggraph-disposable-cluster"))
            result = subprocess.run(["bash", "-c", 'source "$1"; pggraph_validate_disposable_cluster owned_upgrade postgres',
                                     "boundary", str(ROOT / "scripts/lib/pggraph-common.sh")],
                                    env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("-d postgres", (root / "calls").read_text())

    def run_fixture(self, root, fail_prepare=False, fail_verify=False):
        source, tools = root / "source", root / "bin"
        heavy = source / "graph/tests/heavy"
        heavy.mkdir(parents=True)
        shutil.copyfile(ROOT / "graph/tests/heavy/publication_upgrade_artifact.sh",
                        heavy / "publication_upgrade_artifact.sh")
        common = source / "scripts/lib/pggraph-common.sh"
        common.parent.mkdir(parents=True)
        common.write_text('pggraph_validate_database_name() { :; }\n'
                          'pggraph_validate_disposable_cluster() { :; }\n'
                          'pggraph_make_temp_dir() { mktemp -d "$FIXTURE/work.XXXXXX"; }\n')
        previous = root / "previous/graph"
        previous.mkdir(parents=True)
        (previous / "Cargo.toml").write_text('[package]\nversion = "1.2.0"\n')
        (source / "graph/Cargo.toml").write_text('[package]\nversion = "1.2.1"\n')
        with tarfile.open(root / "previous.tar", "w") as archive:
            archive.add(previous, arcname="graph")
        install = root / "installed"
        (install / "share/extension").mkdir(parents=True)
        (install / "lib").mkdir(parents=True)
        executable(tools / "pg_config", '''case "$1" in
--sharedir) echo "$FIXTURE/installed/share";;
--pkglibdir) echo "$FIXTURE/installed/lib";;
esac
''')
        executable(tools / "git", '''case "$*" in
*archive*) cat "$FIXTURE/previous.tar";;
*'v1.2.0^{commit}'*) printf '%040d\\n' 1;;
*) printf '%040d\\n' 2;;
esac
''')
        executable(tools / "cargo", '''out=''
while (( $# )); do
  if [[ "$1" == --out-dir ]]; then out="$2"; shift; fi
  shift
done
[[ -n "$out" ]]
if [[ "$out" == *previous-package ]]; then version=1.2.0; else version=1.2.1; fi
mkdir -p "$out$FIXTURE/installed/share/extension" "$out$FIXTURE/installed/lib"
printf '%s\\n' "$version" > "$out$FIXTURE/installed/lib/graph.so"
printf '%s\\n' "$version" > "$out$FIXTURE/installed/share/extension/graph.control"
printf '%s\\n' "$version" > "$out$FIXTURE/installed/share/extension/graph--$version.sql"
printf 'package %s\\n' "$version" >> "$FIXTURE/calls"
''')
        executable(heavy / "publication_upgrade.sh", '''version="$(cat "$FIXTURE/installed/lib/graph.so")"
printf '%s %s\\n' "$1" "$version" >> "$FIXTURE/calls"
case "$1" in
prepare-1.2.0) [[ "$version" == 1.2.0 ]]; [[ "$FAIL_PREPARE" != 1 ]];;
verify-1.2.1) [[ "$version" == 1.2.1 ]]; [[ "$FAIL_VERIFY" != 1 ]];;
esac
''')
        env = dict(os.environ, PATH=str(tools) + ":/usr/bin:/bin", FIXTURE=str(root),
                   FAIL_PREPARE=str(int(fail_prepare)), FAIL_VERIFY=str(int(fail_verify)))
        result = subprocess.run(["bash", str(heavy / "publication_upgrade_artifact.sh")],
                                env=env, capture_output=True, text=True)
        return result, (install / "lib/graph.so").read_text().strip(), (root / "calls").read_text().splitlines()

    def test_actual_package_order_and_restore_on_phase_failure(self):
        for prepare, verify in ((False, False), (True, False), (False, True)):
            with self.subTest(prepare=prepare, verify=verify), tempfile.TemporaryDirectory() as directory:
                result, installed, calls = self.run_fixture(Path(directory), prepare, verify)
                self.assertEqual(result.returncode, 1 if prepare or verify else 0, result.stderr)
                self.assertEqual(installed, "1.2.1")
                expected = ["package 1.2.0", "package 1.2.1", "prepare-1.2.0 1.2.0"]
                if not prepare:
                    expected.append("verify-1.2.1 1.2.1")
                self.assertEqual(calls, expected)

    def test_invalid_fixture_mode_exits_before_database_commands(self):
        result = subprocess.run(["bash", str(ROOT / "graph/tests/heavy/publication_upgrade.sh"), "unknown"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("Usage:", result.stderr)


if __name__ == "__main__":
    unittest.main()
