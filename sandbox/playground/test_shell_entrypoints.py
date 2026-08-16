"""Unit tests for shared sandbox shell-entrypoint behavior."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PYTHON_HELPERS = ROOT / "sandbox" / "common" / "python.sh"
TEMPORARY_ROOT = ROOT / ".temporary_files"


def write_executable(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    path.chmod(0o755)


def run_helper(command: str, *, path: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["/bin/bash", "-c", f'source "$1"; {command}', "bash", str(PYTHON_HELPERS)],
        text=True,
        capture_output=True,
        check=False,
        env={**os.environ, "PATH": path},
    )


class ShellEntrypointTests(unittest.TestCase):
    """Verify dependency installation selects the intended safety boundary."""

    @classmethod
    def setUpClass(cls) -> None:
        TEMPORARY_ROOT.mkdir(exist_ok=True)

    def test_requirements_install_uses_sfw_when_change_is_needed(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            root = Path(temporary_dir)
            venv = root / "venv"
            tools = root / "tools"
            requirements = root / "requirements.txt"
            requirements.write_text("example==1.0\n", encoding="utf-8")
            write_executable(venv / "bin" / "python", "#!/bin/sh\nexit 1\n")
            write_executable(tools / "sfw", "#!/bin/sh\nprintf 'sfw:%s\\n' \"$*\"\n")

            result = run_helper(
                f'pggraph_prepare_venv_requirements "{venv}" "{requirements}"',
                path=f"{tools}:/usr/bin:/bin",
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), f"sfw:pip install -r {requirements}")

    def test_requirements_install_uses_resolved_sfw_not_venv_shadow(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            root = Path(temporary_dir)
            venv = root / "venv"
            tools = root / "tools"
            requirements = root / "requirements.txt"
            requirements.write_text("example==1.0\n", encoding="utf-8")
            write_executable(venv / "bin" / "python", "#!/bin/sh\nexit 1\n")
            write_executable(
                venv / "bin" / "sfw",
                "#!/bin/sh\nprintf 'shadowed-sfw-ran\\n'\nexit 19\n",
            )
            write_executable(tools / "sfw", "#!/bin/sh\nprintf 'trusted:%s\\n' \"$*\"\n")

            result = run_helper(
                f'pggraph_prepare_venv_requirements "{venv}" "{requirements}"',
                path=f"{tools}:/usr/bin:/bin",
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), f"trusted:pip install -r {requirements}")
            self.assertNotIn("shadowed-sfw-ran", result.stdout)

    def test_python_resolution_follows_executable_symlinks(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            shim = Path(temporary_dir) / "python3.12"
            shim.symlink_to(sys.executable)

            result = run_helper(
                f'pggraph_resolve_python "{shim}"',
                path="/usr/bin:/bin",
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(Path(result.stdout.strip()), Path(sys.executable).resolve())

    def test_satisfied_venv_skips_sfw_requirement(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            root = Path(temporary_dir)
            venv = root / "venv"
            requirements = root / "requirements.txt"
            requirements.write_text("example==1.0\n", encoding="utf-8")
            write_executable(venv / "bin" / "python", "#!/bin/sh\nexit 0\n")

            result = run_helper(
                f'pggraph_prepare_venv_requirements "{venv}" "{requirements}"',
                path="/usr/bin:/bin",
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("already satisfies", result.stdout)
            self.assertNotIn("sfw is required", result.stderr)

    def test_unsatisfied_venv_requires_sfw(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            root = Path(temporary_dir)
            venv = root / "venv"
            requirements = root / "requirements.txt"
            requirements.write_text("example==1.0\n", encoding="utf-8")
            write_executable(venv / "bin" / "python", "#!/bin/sh\nexit 1\n")

            result = run_helper(
                f'pggraph_prepare_venv_requirements "{venv}" "{requirements}"',
                path="/usr/bin:/bin",
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("sfw is required", result.stderr)
            self.assertNotIn("pip install", result.stdout)

    def test_requirements_install_does_not_bypass_a_failing_sfw(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            root = Path(temporary_dir)
            venv = root / "venv"
            tools = root / "tools"
            requirements = root / "requirements.txt"
            requirements.write_text("example==1.0\n", encoding="utf-8")
            write_executable(tools / "sfw", "#!/bin/sh\nexit 17\n")
            write_executable(venv / "bin" / "python", "#!/bin/sh\nexit 1\n")

            result = run_helper(
                f'pggraph_prepare_venv_requirements "{venv}" "{requirements}"',
                path=f"{tools}:/usr/bin:/bin",
            )

            self.assertEqual(result.returncode, 17)
            self.assertNotIn("unexpected fallback", result.stdout)


if __name__ == "__main__":
    unittest.main()
