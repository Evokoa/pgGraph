#!/usr/bin/env python3
"""Verify an exact pgGraph release bundle without rebuilding it."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import zipfile
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


def git_output(*args: str) -> bytes:
    return subprocess.check_output(["git", *args], cwd=Path(__file__).resolve().parents[1])


def verify_source_archive(archive: Path, version: str, commit: str) -> None:
    prefix = f"pgGraph-{version}/"
    tree: dict[str, str] = {}
    for record in git_output("ls-tree", "-rz", "--full-tree", commit).split(b"\0"):
        if not record:
            continue
        metadata, raw_name = record.split(b"\t", 1)
        _mode, kind, object_id = metadata.decode("ascii").split()
        if kind == "blob":
            tree[raw_name.decode("utf-8")] = object_id

    with zipfile.ZipFile(archive) as source_zip:
        members: dict[str, zipfile.ZipInfo] = {}
        for info in source_zip.infolist():
            if info.flag_bits & 0x1:
                fail(f"encrypted source-archive member: {info.filename}")
            if not info.filename.startswith(prefix):
                fail(f"source-archive member is outside {prefix!r}: {info.filename}")
            relative = info.filename.removeprefix(prefix)
            if not relative or info.is_dir():
                continue
            if relative.startswith("/") or ".." in Path(relative).parts:
                fail(f"unsafe source-archive member: {info.filename}")
            if relative in members:
                fail(f"duplicate source-archive member: {relative}")
            members[relative] = info

        if set(members) != set(tree):
            difference = sorted(set(members) ^ set(tree))
            fail(f"source archive differs from commit tree: {difference[:10]}")
        for name, object_id in tree.items():
            if source_zip.read(members[name]) != git_output("cat-file", "blob", object_id):
                fail(f"source-archive content differs from commit: {name}")

        required = {
            "LICENSE",
            "META.json",
            "README.md",
            "graph/Cargo.lock",
            "graph/Cargo.toml",
            "graph/graph.control",
        }
        if missing := sorted(required - set(members)):
            fail(f"source archive is missing required files: {missing}")
        meta = json.loads(source_zip.read(members["META.json"]))
        if meta.get("name") != "pgGraph" or meta.get("version") != version:
            fail("source-archive META.json does not describe the release version")
        cargo = source_zip.read(members["graph/Cargo.toml"]).decode("utf-8")
        cargo_version = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
        if not cargo_version or cargo_version.group(1) != version:
            fail("source-archive Cargo.toml version differs from the release")
        control = source_zip.read(members["graph/graph.control"]).decode("utf-8")
        if "default_version = '@CARGO_VERSION@'" not in control:
            fail("source-archive control file does not use the package version token")


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
    verify_source_archive(bundle / archive_name, manifest["version"], manifest["source_commit"])

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
