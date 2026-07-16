"""Release-runner environment isolation tests."""

from __future__ import annotations

import os
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
