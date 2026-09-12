"""Release-runner environment isolation tests."""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import run_release


class GateEnvironmentTests(unittest.TestCase):
    def test_undeclared_release_controls_are_removed(self) -> None:
        caller = {
            "PATH": "/usr/bin",
            "HOME": "/tmp/home",
            "RUN_INSTALL": "0",
            "PG_VERSIONS": "17",
            "PGHOST": "unexpected",
            "MAX_RSS_MB": "1",
            "SKIP_PACKAGE_INSTALL": "1",
            "PREPARE_PLAYGROUND": "0",
        }
        gate = {"environment": {"RUN_INSTALL": "1", "PG_VERSIONS": "14 15 16 17 18"}}
        with patch.dict(os.environ, caller, clear=True):
            environment = run_release.gate_environment(gate)
        self.assertEqual(environment["RUN_INSTALL"], "1")
        self.assertEqual(environment["PG_VERSIONS"], "14 15 16 17 18")
        self.assertNotIn("PGHOST", environment)
        self.assertNotIn("MAX_RSS_MB", environment)
        self.assertNotIn("SKIP_PACKAGE_INSTALL", environment)
        self.assertNotIn("PREPARE_PLAYGROUND", environment)
        self.assertEqual(environment["PATH"], "/usr/bin")

    def test_undeclared_control_does_not_reach_gate(self) -> None:
        with patch.dict(os.environ, {"RUN_PLAYGROUND": "0", "PGPORT": "9999"}, clear=True):
            environment = run_release.gate_environment({})
        self.assertNotIn("RUN_PLAYGROUND", environment)
        self.assertNotIn("PGPORT", environment)


class RegistryTests(unittest.TestCase):
    def test_current_registry_has_ordered_dependencies_and_safe_crash_gate(self) -> None:
        run_release.validate_registry(run_release.load_json(run_release.REGISTRY))

    def test_local_validation_retains_full_checks_without_release_bundle(self) -> None:
        registry = run_release.load_json(run_release.REGISTRY)
        self.assertEqual(
            registry["tiers"]["local-validation"],
            [name for name in registry["tiers"]["full-matrix"] if name != "release-bundle"],
        )
        self.assertIn("rls-evidence-gate", registry["tiers"]["local-validation"])
        self.assertIn("package-install-matrix", registry["tiers"]["local-validation"])
        self.assertIn("publication-upgrade", registry["tiers"]["local-validation"])
        self.assertIn("publication-upgrade", registry["gates"]["release-bundle"]["depends_on"])
        gate = registry["gates"]["rls-evidence-gate"]
        self.assertEqual(Path(gate["command"][0]).name, "with_disposable_postgres.sh")
        self.assertEqual(Path(gate["command"][1]).name, "rls_large_table_gate_regression.sh")

    def test_crash_gate_requires_disposable_cluster_boundary(self) -> None:
        registry = {
            "tiers": {"rc": ["release"]},
            "gates": {
                "release": {
                    "command": ["./tests/heavy/run_release_gate.sh"],
                    "environment": {"RUN_CRASH": "1"},
                }
            },
        }
        with self.assertRaisesRegex(ValueError, "without PGDATA"):
            run_release.validate_registry(registry)

    def test_destructive_script_requires_disposable_wrapper(self) -> None:
        registry = {
            "tiers": {"rc": ["crash"]},
            "gates": {
                "crash": {
                    "command": ["./tests/heavy/crash_recovery.sh"],
                    "environment": {},
                }
            },
        }
        with self.assertRaisesRegex(ValueError, "destructive cluster script"):
            run_release.validate_registry(registry)

    def test_cluster_validator_fails_without_wrapper_token(self) -> None:
        script = run_release.ROOT / "scripts" / "lib" / "pggraph-common.sh"
        proc = subprocess.run(
            [
                "/bin/bash",
                "-c",
                f"source '{script}'; PGDATA=/tmp/pggraph-fake "
                "pggraph_validate_disposable_cluster pggraph_safety_test",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 2)
        self.assertIn("disposable-cluster wrapper", proc.stderr)


def write_executable(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nset -eu\n" + body, encoding="utf-8")
    path.chmod(0o755)


class ShellGateTests(unittest.TestCase):
    def run_docker_smoke(self, root: Path, result: str, *, existing: bool = False,
                         port: str = "55439") -> subprocess.CompletedProcess[str]:
        tools = root / "bin"
        write_executable(tools / "docker", """
printf '%s\\n' "$*" >> "$CALLS"
case "$1" in
  container) test "$EXISTING" = 1 ;;
  run) printf 'created-container-id\\n' ;;
  exec)
    case "$*" in
      *psql*) cat >> "$SQL_LOG"; case "$*" in *-qAt*) printf '%s\\n' "$RESULT" ;; esac ;;
    esac ;;
esac
""")
        return subprocess.run(
            ["bash", str(run_release.ROOT / "graph/tests/heavy/docker_smoke.sh")],
            env={**os.environ, "PATH": f"{tools}:/usr/bin:/bin", "CALLS": str(root / "calls"),
                 "SQL_LOG": str(root / "sql"), "EXISTING": str(int(existing)),
                 "RESULT": result, "PG_PORT": port, "CONTAINER": "isolated-smoke"},
            capture_output=True, text=True, check=False,
        )

    def test_docker_smoke_checks_results_and_binds_configured_loopback_port(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proc = self.run_docker_smoke(root, "1:child\nchild,root")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            calls = (root / "calls").read_text()
            self.assertIn("-p 127.0.0.1:55439:5432", calls)
            self.assertIn("rm -f created-container-id", calls)
            self.assertNotIn("rm -f isolated-smoke", calls)
            self.assertIn("graph.traverse", (root / "sql").read_text())

    def test_docker_smoke_rejects_wrong_search_or_traversal_results(self) -> None:
        for result in ("0:\nchild,root", "1:child\nchild", "2:child,root\nchild,root", "1:root\nchild,root"):
            with self.subTest(result=result), tempfile.TemporaryDirectory() as directory:
                proc = self.run_docker_smoke(Path(directory), result)
                self.assertEqual(proc.returncode, 1, proc.stderr)
                self.assertIn("result mismatch", proc.stderr)
                self.assertNotIn("Docker smoke passed", proc.stdout)

    def test_docker_smoke_preserves_existing_container(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proc = self.run_docker_smoke(root, "", existing=True)
            self.assertEqual(proc.returncode, 2)
            self.assertEqual((root / "calls").read_text().splitlines(),
                             ["container inspect isolated-smoke"])

    def test_docker_smoke_rejects_invalid_ports_before_docker(self) -> None:
        for port in ("0", "65536", "bad", "123456789"):
            with self.subTest(port=port), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                proc = self.run_docker_smoke(root, "", port=port)
                self.assertEqual(proc.returncode, 2)
                self.assertFalse((root / "calls").exists())

    def run_playground_modes(self, root: Path, fail_mode: str = "none") -> subprocess.CompletedProcess[str]:
        script = root / "graph/tests/heavy/playground_release_gate.sh"
        script.parent.mkdir(parents=True)
        shutil.copyfile(run_release.ROOT / "graph/tests/heavy/playground_release_gate.sh", script)
        helper = root / "sandbox/common/docker.sh"
        helper.parent.mkdir(parents=True)
        helper.write_text("""
require_docker() { :; }
ensure_pggraph_image() { printf 'image:%s\\n' "$PGGRAPH_REBUILD_IMAGE" >> "$CALLS"; }
ensure_pggraph_container() { printf 'container:%s\\n' "$PGGRAPH_RECREATE_CONTAINER" >> "$CALLS"; }
pggraph_container_host_port() { printf '55439\\n'; }
""")
        write_executable(root / "bin/python3", '''
printf '%s\\n' "$*" >> "$CALLS"
case "$*" in *"--mode $FAIL_MODE") exit 17 ;; esac
''')
        return subprocess.run(
            ["bash", str(script), "--all-modes"],
            env={**os.environ, "PATH": f"{root / 'bin'}:/usr/bin:/bin",
                 "CALLS": str(root / "calls"), "PGGRAPH_REBUILD_IMAGE": "1",
                 "PGGRAPH_RECREATE_CONTAINER": "1", "PREPARE_PLAYGROUND": "0", "FAIL_MODE": fail_mode},
            capture_output=True, text=True, check=False,
        )

    def test_all_playground_modes_prepare_matching_projection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proc = self.run_playground_modes(root)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            calls = (root / "calls").read_text().splitlines()
            self.assertEqual(calls[0:2], ["image:1", "container:1"])
            self.assertIn("--build-mode csr_readonly --prepare-only", calls[2])
            self.assertIn("--mode csr", calls[3])
            self.assertEqual(calls[4:6], ["image:0", "container:0"])
            self.assertIn("--build-mode mutable_overlay --prepare-only", calls[6])
            self.assertIn("--mode mutable", calls[7])
            self.assertEqual(len(calls), 8)

    def test_all_playground_modes_propagate_either_catalog_failure(self) -> None:
        for mode, expected_calls in (("csr", 4), ("mutable", 8)):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                proc = self.run_playground_modes(root, mode)
                self.assertEqual(proc.returncode, 17, proc.stderr)
                self.assertEqual(len((root / "calls").read_text().splitlines()), expected_calls)

    def run_cleanup_fixture(self, root, *, command_code=0, stop_code=0, start_code=0, signal_name=""):
        tools = root / "bin"
        write_executable(tools / "pg_config", f"printf '%s\\n' {shlex.quote(str(tools))}\n")
        write_executable(tools / "initdb", 'while [ "$1" != -D ]; do shift; done\nmkdir -p "$2"\n')
        write_executable(tools / "pg_ctl", '''
printf '%s\\n' "$*" >> "$CALLS"
previous=''
for argument in "$@"; do
  if [ "$previous" = -D ]; then printf '%s\\n' "$argument" > "$DATA_PATH"; fi
  previous="$argument"
done
case "$*" in
  *stop) exit "$STOP_CODE" ;;
  *start) exit "$START_CODE" ;;
esac
''')
        write_executable(tools / "pg_isready", 'exit 0\n')
        write_executable(tools / "python3", '''
source=$(cat)
case "$source" in
  *socket*) printf '55439\\n' ;;
  *uuid*) printf 'fixture-cluster-token\\n' ;;
  *) exit 1 ;;
esac
''')
        return subprocess.run(
            ["bash", str(run_release.ROOT / "scripts/with_disposable_postgres.sh"),
             "/bin/sh", "-c", 'if [ -n "$SIGNAL_NAME" ]; then kill -"$SIGNAL_NAME" "$PPID"; fi; exit "$COMMAND_CODE"'],
            env={**os.environ, "PATH": f"{tools}:/usr/bin:/bin", "TMPDIR": str(root),
                 "PG_CONFIG": str(tools / "pg_config"), "CALLS": str(root / "calls"),
                 "DATA_PATH": str(root / "data-path"), "STOP_CODE": str(stop_code),
                 "START_CODE": str(start_code), "COMMAND_CODE": str(command_code), "SIGNAL_NAME": signal_name},
            capture_output=True, text=True, timeout=10,
        )

    def test_failed_shutdown_preserves_data_and_original_failure(self) -> None:
        for command_code, stop_code, expected, retained in ((0, 0, 0, False), (7, 0, 7, False),
                                                          (0, 1, 1, True), (7, 1, 7, True)):
            with self.subTest(command=command_code, stop=stop_code), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result = self.run_cleanup_fixture(root, command_code=command_code, stop_code=stop_code)
                self.assertEqual(result.returncode, expected, result.stderr)
                data = Path((root / "data-path").read_text().strip())
                self.assertEqual(data.exists(), retained)
                self.assertEqual(data.parent.exists(), retained)
                if retained:
                    self.assertIn("retained disposable cluster", result.stderr)
                    self.assertTrue((data / ".pggraph-disposable-cluster").is_file())

    def test_signal_cleanup_returns_nonzero_once(self) -> None:
        for name, expected in (("INT", 130), ("TERM", 143)):
            with self.subTest(signal=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result = self.run_cleanup_fixture(root, signal_name=name)
                self.assertEqual(result.returncode, expected, result.stderr)
                calls = (root / "calls").read_text().splitlines()
                self.assertEqual(sum(line.endswith("stop") for line in calls), 1)
                self.assertFalse(Path((root / "data-path").read_text().strip()).exists())

    def test_failed_start_attempts_shutdown_and_retains_failed_stop(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.run_cleanup_fixture(root, start_code=5, stop_code=1)
            self.assertEqual(result.returncode, 5, result.stderr)
            self.assertTrue(Path((root / "data-path").read_text().strip()).exists())
            self.assertTrue((root / "calls").read_text().splitlines()[-1].endswith("stop"))

    def test_disposable_cluster_preserves_fsync_on_start_and_restart(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "bin"
            write_executable(tools / "pg_config", f"printf '%s\\n' {shlex.quote(str(tools))}\n")
            write_executable(tools / "initdb", 'while [ "$1" != -D ]; do shift; done\nmkdir -p "$2"\n')
            write_executable(tools / "pg_ctl", 'printf "%s\\n" "$*" >> "$CALLS"\n')
            write_executable(tools / "pg_isready", 'exit 0\n')
            write_executable(tools / "python3", """
source=$(cat)
case "$source" in
  *socket*) printf '55439\\n' ;;
  *uuid*) printf 'fixture-cluster-token\\n' ;;
  *) exit 1 ;;
esac
""")
            proc = subprocess.run(
                ["bash", str(run_release.ROOT / "scripts/with_disposable_postgres.sh"),
                 "/bin/sh", "-c", 'printf "%s\\n" "$POSTGRES_OPTS"'],
                env={**os.environ, "PATH": f"{tools}:/usr/bin:/bin",
                     "PG_CONFIG": str(tools / "pg_config"), "CALLS": str(root / "calls")},
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            start = (root / "calls").read_text().splitlines()[0]
            self.assertIn("-c fsync=on", start)
            self.assertNotIn("-F", start)
            self.assertIn("-c fsync=on", proc.stdout)
            self.assertNotIn("-F", proc.stdout)


class EvidenceTests(unittest.TestCase):
    def test_run_gate_retains_combined_output_and_digest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            log_path = Path(directory) / "gate.log"
            result, exit_code = run_release.run_gate(
                ["/bin/sh", "-c", "printf 'out\\n'; printf 'err\\n' >&2"],
                run_release.ROOT,
                os.environ.copy(),
                10,
                log_path,
            )
            self.assertEqual((result, exit_code), ("pass", 0))
            self.assertEqual(log_path.read_text(encoding="utf-8"), "out\nerr\n")
            record = run_release.artifact_record(log_path)
            self.assertEqual(record["sha256"], run_release.sha256_file(log_path))
            self.assertEqual(record["bytes"], 8)

    def test_failed_gate_is_captured(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            log_path = Path(directory) / "failed.log"
            result, exit_code = run_release.run_gate(
                ["/bin/sh", "-c", "printf 'failure detail\\n' >&2; exit 7"],
                run_release.ROOT,
                os.environ.copy(),
                10,
                log_path,
            )
            self.assertEqual((result, exit_code), ("fail", 7))
            self.assertEqual(log_path.read_text(encoding="utf-8"), "failure detail\n")

    def test_resume_requires_unchanged_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "artifact.txt"
            path.write_text("original", encoding="utf-8")
            record = {"result": "pass", "artifacts": [run_release.artifact_record(path)]}
            self.assertTrue(run_release.passing_record_is_reusable(record))
            path.write_text("changed", encoding="utf-8")
            self.assertFalse(run_release.passing_record_is_reusable(record))


if __name__ == "__main__":
    unittest.main()
