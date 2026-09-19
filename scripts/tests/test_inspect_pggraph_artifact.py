"""Regression coverage for current and legacy artifact inspection."""

import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

SCRIPT = Path(__file__).resolve().parents[1] / "inspect_pggraph_artifact.py"
spec = importlib.util.spec_from_file_location("artifact_inspector", SCRIPT)
inspector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inspector)


class ArtifactInspectorTests(unittest.TestCase):
    def artifact(self, root, version=7, width=1):
        data = bytearray(512)
        data[:4] = b"PGGH"
        struct.pack_into("<7I", data, 4, version, 512, 0, 0, 0, 0, 26)
        if version == 7:
            struct.pack_into("<I", data, 48, width)
        for index in range(26):
            struct.pack_into("<QQ", data, 64 + index * 16, 512, 0)
        struct.pack_into("<I", data, 44, zlib.crc32(data) & 0xFFFFFFFF)
        path = root / "base.pggraph"
        path.write_bytes(data)
        return path

    def test_accepts_v6_and_all_v7_label_widths(self):
        with tempfile.TemporaryDirectory() as temporary:
            for version, width in [(6, 1), (7, 1), (7, 2), (7, 4)]:
                with self.subTest(version=version, width=width):
                    report = inspector.inspect(self.artifact(Path(temporary), version, width))
                    self.assertEqual(report["version"], version)
                    self.assertTrue(report["crc32_valid"])

    def test_rejects_invalid_v7_width(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(ValueError, "width"):
                inspector.inspect(self.artifact(Path(temporary), 7, 3))

    def test_resolves_supported_manifest_versions_and_checks_checksum(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact = self.artifact(root)
            for version in (2, 3, 4):
                manifest = json.dumps({"version": version, "generation_id": 1,
                                       "base_artifact_path": artifact.name}).encode()
                (root / "projection-generation-00000000000000000001.json").write_bytes(manifest)
                pointer = {"version": 1, "generation_id": 1,
                           "manifest_checksum": f"crc32:{zlib.crc32(manifest) & 0xFFFFFFFF:08x}"}
                (root / "projection-current.json").write_text(json.dumps(pointer))
                self.assertEqual(inspector.resolve_artifact(root), artifact)
            pointer["manifest_checksum"] = "crc32:00000000"
            (root / "projection-current.json").write_text(json.dumps(pointer))
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                inspector.resolve_artifact(root)

    def test_rejects_malformed_current_manifest_fields(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact = self.artifact(root)
            invalid_fields = [
                {"version": value} for value in (None, True, 4.0, "4", 1, 5)
            ] + [
                {"generation_id": value} for value in (None, True, 1.0, "1", 0, 2)
            ] + [
                {"base_artifact_path": value}
                for value in (None, 7, "", "../base.pggraph", str(artifact))
            ]
            for fields in invalid_fields:
                with self.subTest(fields=fields):
                    manifest = json.dumps({"version": 4, "generation_id": 1,
                                           "base_artifact_path": artifact.name, **fields}).encode()
                    (root / "projection-generation-00000000000000000001.json").write_bytes(manifest)
                    pointer = {"version": 1, "generation_id": 1,
                               "manifest_checksum": f"crc32:{zlib.crc32(manifest) & 0xFFFFFFFF:08x}"}
                    (root / "projection-current.json").write_text(json.dumps(pointer))
                    with self.assertRaises(ValueError):
                        inspector.resolve_artifact(root)

    def test_rejects_noninteger_pointer_version(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for version in (True, 1.0, "1"):
                with self.subTest(version=version):
                    pointer = {"version": version, "generation_id": 1,
                               "manifest_checksum": "crc32:00000000"}
                    (root / "projection-current.json").write_text(json.dumps(pointer))
                    with self.assertRaisesRegex(ValueError, "invalid version"):
                        inspector.resolve_artifact(root)

    def test_graph_id_cli_requires_and_uses_database_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            graph_id = "00000000-0000-0000-0000-000000000001"
            root = Path(temporary) / "graph" / "database-42" / graph_id
            root.mkdir(parents=True)
            self.artifact(root).rename(root / "main.pggraph")
            command = [sys.executable, str(SCRIPT), "--graph-id", graph_id,
                       "--pgdata", temporary, "--resolve-only"]
            missing = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertNotEqual(missing.returncode, 0)
            result = subprocess.run(command + ["--database-oid", "42"],
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(root / "main.pggraph"))


if __name__ == "__main__":
    unittest.main()
