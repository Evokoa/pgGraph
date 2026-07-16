"""Release bundle generation and tamper-detection tests."""

from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
import zipfile
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

    def test_source_archive_must_match_the_claimed_commit(self) -> None:
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
            archive = bundle / f"pgGraph-{version}.zip"
            changed = bundle / "changed.zip"
            with zipfile.ZipFile(archive) as source, zipfile.ZipFile(changed, "w") as target:
                for info in source.infolist():
                    payload = source.read(info)
                    if info.filename == f"pgGraph-{version}/README.md":
                        payload += b"\nchanged\n"
                    target.writestr(info, payload)
            with self.assertRaisesRegex(SystemExit, "content differs from commit"):
                verify_release_bundle.verify_source_archive(changed, version, commit)


if __name__ == "__main__":
    unittest.main()
