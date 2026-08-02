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

    def test_venv_pip_uses_sfw_when_available(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            root = Path(temporary_dir)
            venv = root / "venv"
            tools = root / "tools"
            write_executable(tools / "sfw", "#!/bin/sh\nprintf 'sfw:%s\\n' \"$*\"\n")

            result = run_helper(
                f'pggraph_venv_pip "{venv}" install -r requirements.txt',
                path=f"{tools}:/usr/bin:/bin",
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "sfw:pip install -r requirements.txt")
            self.assertNotIn("using the sandbox virtualenv", result.stderr)

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

    def test_venv_pip_falls_back_when_sfw_is_unavailable(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            root = Path(temporary_dir)
            venv = root / "venv"
            write_executable(venv / "bin" / "python", "#!/bin/sh\nprintf 'python:%s\\n' \"$*\"\n")

            result = run_helper(
                f'pggraph_venv_pip "{venv}" install -r requirements.txt',
                path="/usr/bin:/bin",
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "python:-m pip install -r requirements.txt")
            self.assertIn("using the sandbox virtualenv", result.stderr)

    def test_venv_pip_does_not_bypass_a_failing_sfw(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            root = Path(temporary_dir)
            venv = root / "venv"
            tools = root / "tools"
            write_executable(tools / "sfw", "#!/bin/sh\nexit 17\n")
            write_executable(venv / "bin" / "python", "#!/bin/sh\nprintf 'unexpected fallback\\n'\n")

            result = run_helper(
                f'pggraph_venv_pip "{venv}" install -r requirements.txt',
                path=f"{tools}:/usr/bin:/bin",
            )

            self.assertEqual(result.returncode, 17)
            self.assertNotIn("unexpected fallback", result.stdout)


if __name__ == "__main__":
    unittest.main()
