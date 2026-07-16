#!/usr/bin/env python3
"""Verify an exact pgGraph release bundle without rebuilding it."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fail(message: str) -> None:
    raise SystemExit(f"release bundle verification failed: {message}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--version")
    parser.add_argument("--commit")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    bundle = args.bundle.resolve()
    manifest_path = bundle / "release-manifest.json"
    checksums_path = bundle / "SHA256SUMS"
    if not manifest_path.is_file() or not checksums_path.is_file():
        fail("release-manifest.json and SHA256SUMS are required")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if args.version and manifest.get("version") != args.version:
        fail(f"expected version {args.version}, got {manifest.get('version')!r}")
    if args.commit and manifest.get("source_commit") != args.commit:
        fail(f"expected commit {args.commit}, got {manifest.get('source_commit')!r}")

    expected_files = {"release-manifest.json"}
    expected_payload = set()
    for record in manifest.get("artifacts", []):
        name = record.get("name", "")
        if not name or Path(name).name != name:
            fail(f"invalid artifact name: {name!r}")
        path = bundle / name
        if not path.is_file():
            fail(f"missing artifact: {name}")
        if sha256_file(path) != record.get("sha256") or path.stat().st_size != record.get("bytes"):
            fail(f"artifact digest or size mismatch: {name}")
        expected_payload.add(name)
        expected_files.add(name)

    archive_name = f"pgGraph-{manifest.get('version')}.zip"
    required_payload = {
        archive_name,
        f"pgGraph-{manifest.get('version')}.spdx.json",
        f"pgGraph-{manifest.get('version')}.provenance.json",
    }
    if expected_payload != required_payload:
        fail(f"unexpected payload set: {sorted(expected_payload)}")

    checksums = {}
    for line in checksums_path.read_text(encoding="utf-8").splitlines():
        try:
            digest, name = line.split("  ", 1)
        except ValueError:
            fail(f"invalid SHA256SUMS line: {line!r}")
        if name in checksums:
            fail(f"duplicate SHA256SUMS entry: {name}")
        checksums[name] = digest
    if set(checksums) != expected_files:
        fail(f"SHA256SUMS file set differs: {sorted(set(checksums) ^ expected_files)}")
    for name, digest in checksums.items():
        if sha256_file(bundle / name) != digest:
            fail(f"SHA256SUMS mismatch: {name}")
    actual_files = {path.name for path in bundle.iterdir() if path.is_file()}
    if actual_files != expected_files | {"SHA256SUMS"}:
        fail(f"bundle has missing or extra files: {sorted(actual_files ^ (expected_files | {'SHA256SUMS'}))}")

    sbom = json.loads((bundle / f"pgGraph-{manifest['version']}.spdx.json").read_text(encoding="utf-8"))
    if sbom.get("spdxVersion") != "SPDX-2.3" or not sbom.get("packages"):
        fail("SPDX SBOM is incomplete")
    described = [
        package
        for package in sbom["packages"]
        if package.get("SPDXID") == "SPDXRef-pgGraph"
    ]
    if len(described) != 1 or described[0].get("versionInfo") != manifest["version"]:
        fail("SPDX SBOM does not describe the release version")
    provenance = json.loads(
        (bundle / f"pgGraph-{manifest['version']}.provenance.json").read_text(encoding="utf-8")
    )
    subjects = provenance.get("subject", [])
    if len(subjects) != 1 or subjects[0].get("name") != archive_name:
        fail("provenance subject does not name the source archive")
    if subjects[0].get("digest", {}).get("sha256") != sha256_file(bundle / archive_name):
        fail("provenance subject digest does not match the source archive")
    predicate = provenance.get("predicate", {})
    parameters = predicate.get("buildDefinition", {}).get("externalParameters", {})
    dependencies = predicate.get("buildDefinition", {}).get("resolvedDependencies", [])
    if parameters.get("version") != manifest["version"]:
        fail("provenance version does not match the release manifest")
    if len(dependencies) != 1 or dependencies[0].get("digest", {}).get("gitCommit") != manifest["source_commit"]:
        fail("provenance source commit does not match the release manifest")
    print(
        f"release bundle verified: v{manifest['version']} at {manifest['source_commit']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
