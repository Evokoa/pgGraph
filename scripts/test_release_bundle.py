"""Release bundle generation and tamper-detection tests."""

from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import prepare_release_bundle
import verify_release_bundle


class ReleaseBundleTests(unittest.TestCase):
    def test_bundle_is_complete_and_tamper_evident(self) -> None:
        version = json.loads((prepare_release_bundle.ROOT / "META.json").read_text())["version"]
        commit = prepare_release_bundle.git("rev-parse", "HEAD")
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "bundle"
            subprocess.run(
                [
                    str(prepare_release_bundle.ROOT / "scripts" / "prepare_release_bundle.py"),
                    "--version",
                    version,
                    "--ref",
                    commit,
                    "--out-dir",
                    str(bundle),
                ],
                cwd=prepare_release_bundle.ROOT,
                check=True,
            )
            with patch(
                "sys.argv",
                ["verify_release_bundle.py", str(bundle), "--version", version, "--commit", commit],
            ):
                self.assertEqual(verify_release_bundle.main(), 0)
            archive = bundle / f"pgGraph-{version}.zip"
            archive.write_bytes(archive.read_bytes() + b"tampered")
            with patch("sys.argv", ["verify_release_bundle.py", str(bundle)]):
                with self.assertRaisesRegex(SystemExit, "digest or size mismatch"):
                    verify_release_bundle.main()


if __name__ == "__main__":
    unittest.main()
