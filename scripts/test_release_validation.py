#!/usr/bin/env python3
"""Unit tests for release metadata and retained-evidence validation."""

from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path
from unittest.mock import patch

import validate_release
import verify_release_evidence


class ReleaseMetadataTests(unittest.TestCase):
    def test_current_candidate_metadata_agrees(self) -> None:
        validate_release.validate_version_metadata("1.0.0")

    def test_candidate_version_mismatch_fails(self) -> None:
        original = validate_release.read_text

        def read_text(path: str) -> str:
            if path == "release/maturity.json":
                return json.dumps({"candidate_version": "1.0.1"})
            return original(path)

        with patch.object(validate_release, "read_text", side_effect=read_text):
            stderr = StringIO()
            with redirect_stderr(stderr), self.assertRaises(SystemExit) as failure:
                validate_release.validate_version_metadata("1.0.0")
            self.assertEqual(failure.exception.code, 1)
            self.assertIn("candidate_version", stderr.getvalue())

    def test_release_dependencies_are_immutable(self) -> None:
        validate_release.validate_release_dependencies()

    def test_moving_release_action_fails(self) -> None:
        original = validate_release.read_text

        def read_text(path: str) -> str:
            if path == ".github/workflows/release.yml":
                return "steps:\n  - uses: actions/checkout@v4\n"
            return original(path)

        with patch.object(validate_release, "read_text", side_effect=read_text):
            stderr = StringIO()
            with redirect_stderr(stderr), self.assertRaises(SystemExit):
                validate_release.validate_release_dependencies()
            self.assertIn("not pinned to a full commit", stderr.getvalue())

    def test_moving_postgres_release_image_fails(self) -> None:
        original = validate_release.read_text

        def read_text(path: str) -> str:
            value = original(path)
            if path == ".github/workflows/release.yml":
                return value.replace(
                    "postgres:17-bookworm@sha256:"
                    "4f736ae292687621d4dbe0d499ffd024a36bd2ee7d8ca6f2ccd4c800f047b394",
                    "postgres:17-bookworm",
                )
            return value

        with patch.object(validate_release, "read_text", side_effect=read_text):
            stderr = StringIO()
            with redirect_stderr(stderr), self.assertRaises(SystemExit):
                validate_release.validate_release_dependencies()
            self.assertIn("PostgreSQL image is not pinned by digest", stderr.getvalue())


class ReleaseEvidenceTests(unittest.TestCase):
    @staticmethod
    def digest(path: Path) -> str:
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def fixture(self, root: Path) -> Path:
        lockfile = root / "graph" / "Cargo.lock"
        contract = root / "release" / "gates.json"
        artifact = root / "release" / "evidence" / "logs" / "gate.log"
        gate = {
            "command": ["true"],
            "cwd": ".",
            "environment": {},
            "timeout_seconds": 10,
            "exclusive_resources": [],
            "depends_on": [],
            "thresholds": {"failures": 0},
        }
        registry = {
            "datasets": {},
            "tiers": {"full-matrix": ["example"]},
            "gates": {"example": gate},
        }
        files = [
            (lockfile, "locked\n"),
            (contract, json.dumps(registry) + "\n"),
            (artifact, "gate passed\n"),
        ]
        files.extend(
            (root / relative, "{}\n")
            for relative in verify_release_evidence.CONTRACT_PATHS
            if relative != "release/gates.json"
        )
        for path, value in files:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value, encoding="utf-8")

        versions = {
            "git_commit": "a" * 40,
            "git_status_porcelain": [],
            "rustc": "rustc 1.96.0 (stable)",
            "source_tree_sha256": "tree",
        }
        registry_digest = self.digest(contract)
        contracts = {
            relative: self.digest(root / relative)
            for relative in verify_release_evidence.CONTRACT_PATHS
        }
        manifest = {
            "schema_version": 2,
            "tier": "full-matrix",
            "result": "pass",
            "versions": versions,
            "registry_sha256": registry_digest,
            "datasets": {},
            "lockfiles": {"graph/Cargo.lock": self.digest(lockfile)},
            "release_contracts": contracts,
            "gates": [
                {
                    "name": "example",
                    **gate,
                    "duration_seconds": 1.0,
                    "result": "pass",
                    "exit_code": 0,
                    "fingerprint": verify_release_evidence.gate_fingerprint(
                        "example", gate, versions, registry_digest
                    ),
                    "artifacts": [
                        {
                            "path": "release/evidence/logs/gate.log",
                            "bytes": artifact.stat().st_size,
                            "sha256": self.digest(artifact),
                        }
                    ],
                }
            ],
        }
        path = root / "release" / "evidence" / "full-matrix.json"
        path.write_text(json.dumps(manifest), encoding="utf-8")
        return path

    def test_passing_evidence_is_bound_to_source_and_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self.fixture(root)
            with patch.object(verify_release_evidence, "source_tree_sha256", return_value="tree"):
                verify_release_evidence.verify(manifest, "full-matrix", "a" * 40, root)

    def test_tampered_artifact_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self.fixture(root)
            artifact = root / "release" / "evidence" / "logs" / "gate.log"
            artifact.write_text("changed\n", encoding="utf-8")
            with patch.object(verify_release_evidence, "source_tree_sha256", return_value="tree"):
                with self.assertRaisesRegex(SystemExit, "artifact mismatch"):
                    verify_release_evidence.verify(
                        manifest, "full-matrix", "a" * 40, root
                    )

    def test_incomplete_full_matrix_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = self.fixture(root)
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["gates"] = []
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with patch.object(verify_release_evidence, "source_tree_sha256", return_value="tree"):
                with self.assertRaisesRegex(SystemExit, "ordered release tier"):
                    verify_release_evidence.verify(
                        manifest_path, "full-matrix", "a" * 40, root
                    )


if __name__ == "__main__":
    unittest.main()
