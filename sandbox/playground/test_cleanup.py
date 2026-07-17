"""Unit tests for bounded playground cleanup behavior."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from unittest.mock import patch


COMMON_RUNNER = Path(__file__).resolve().parents[1] / "common" / "run_benchmarks.py"
SPEC = importlib.util.spec_from_file_location("pggraph_run_benchmarks", COMMON_RUNNER)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"could not load {COMMON_RUNNER}")
RUNNER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUNNER
SPEC.loader.exec_module(RUNNER)


class Completed:
    """Minimal subprocess result used by cleanup command tests."""

    def __init__(self, *, returncode: int = 0, stdout: str = "", stderr: str = "") -> None:
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class CleanupTests(unittest.TestCase):
    """Verify disposable PostgreSQL data does not leak across cleanup runs."""

    def test_cleanup_removes_container_and_anonymous_volumes(self) -> None:
        with patch.object(RUNNER, "require_docker"):
            with patch.object(
                RUNNER,
                "run",
                side_effect=[
                    Completed(stdout="pggraph-sandbox\n"),
                    Completed(),
                ],
            ) as run:
                RUNNER.cleanup_docker(
                    "pggraph-sandbox",
                    "pggraph-postgres:17",
                    remove_image=False,
                    dry_run=False,
                )

        self.assertEqual(
            run.call_args_list[1].args[0],
            ["docker", "rm", "-f", "-v", "pggraph-sandbox"],
        )


if __name__ == "__main__":
    unittest.main()
