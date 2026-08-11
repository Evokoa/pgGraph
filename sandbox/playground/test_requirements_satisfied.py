"""Tests for offline virtual-environment requirement validation."""

from __future__ import annotations

import importlib.metadata
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKER = ROOT / "sandbox" / "common" / "requirements_satisfied.py"
TEMPORARY_ROOT = ROOT / ".temporary_files"


class RequirementsSatisfiedTests(unittest.TestCase):
    """The checker must never resolve or install packages."""

    @classmethod
    def setUpClass(cls) -> None:
        TEMPORARY_ROOT.mkdir(exist_ok=True)

    def run_checker(self, requirements: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            path = Path(temporary_dir) / "requirements.txt"
            path.write_text(requirements, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(CHECKER), str(path)],
                text=True,
                capture_output=True,
                check=False,
            )

    def test_exact_installed_pin_is_satisfied(self) -> None:
        pip_version = importlib.metadata.version("pip")
        result = self.run_checker(f"pip=={pip_version}\n")

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_or_wrong_pin_is_unsatisfied(self) -> None:
        result = self.run_checker("pggraph-not-installed==999.0\n")

        self.assertEqual(result.returncode, 1)
        self.assertIn("not installed", result.stderr)

    def test_inactive_python_marker_is_ignored(self) -> None:
        result = self.run_checker('pggraph-not-installed==999.0; python_version < "2.0"\n')

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unrecognized_requirement_fails_closed(self) -> None:
        result = self.run_checker("pip>=1\n")

        self.assertEqual(result.returncode, 1)
        self.assertIn("unsupported requirement", result.stderr)

    def test_unknown_extra_fails_closed(self) -> None:
        pip_version = importlib.metadata.version("pip")
        result = self.run_checker(f"pip[unknown]=={pip_version}\n")

        self.assertEqual(result.returncode, 1)
        self.assertIn("unsupported extra", result.stderr)

    def test_psycopg_binary_extra_checks_both_distributions(self) -> None:
        try:
            psycopg_version = importlib.metadata.version("psycopg")
            binary_version = importlib.metadata.version("psycopg-binary")
        except importlib.metadata.PackageNotFoundError:
            self.skipTest("psycopg binary extra is not installed in this interpreter")
        self.assertEqual(psycopg_version, binary_version)

        result = self.run_checker(f"psycopg[binary]=={psycopg_version}\n")

        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
