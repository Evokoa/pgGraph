#!/usr/bin/env python3
"""Verify retained pgGraph release-tier evidence against its source commit."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATHS = (
    "release/cast-allowlist.json",
    "release/contract-changes.json",
    "release/examples.json",
    "release/gates.json",
    "release/maturity.json",
    "release/scripts.json",
    "release/unsafe-allowlist.json",
    "release/v1-contract.json",
    "release/v1-gql-profile.json",
    "release/v1-gql-unsupported.json",
    "release/v1-schema.sql",
    "release/v1-sql-profile.json",
)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fail(message: str) -> None:
    raise SystemExit(f"release evidence verification failed: {message}")


def source_tree_sha256(root: Path) -> str:
    process = subprocess.run(
        ["git", "ls-files", "--cached", "-z"],
        cwd=root,
        capture_output=True,
        check=True,
    )
    digest = hashlib.sha256()
    for raw_path in process.stdout.split(b"\0"):
        if not raw_path:
            continue
        relative = raw_path.decode("utf-8", errors="surrogateescape")
        if relative.startswith("release/evidence/"):
            continue
        path = root / relative
        if path.is_file():
            digest.update(raw_path)
            digest.update(b"\0")
            digest.update(path.read_bytes())
            digest.update(b"\0")
    return digest.hexdigest()


def gate_fingerprint(name: str, gate: dict, versions: dict, registry_sha256: str) -> str:
    payload = json.dumps(
        {
            "name": name,
            "gate": gate,
            "versions": versions,
            "registry_sha256": registry_sha256,
        },
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode()).hexdigest()


def find_manifest(path: Path) -> Path:
    if path.is_file():
        return path
    manifests = []
    for candidate in sorted(path.rglob("*.json")):
        try:
            value = json.loads(candidate.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if isinstance(value, dict) and "tier" in value and "gates" in value:
            manifests.append(candidate)
    if len(manifests) != 1:
        fail(f"expected one evidence manifest under {path}, found {len(manifests)}")
    return manifests[0]


def verify(manifest_path: Path, tier: str, commit: str, root: Path = ROOT) -> None:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    registry_path = root / "release" / "gates.json"
    registry = json.loads(registry_path.read_text(encoding="utf-8"))
    registry_sha256 = sha256_file(registry_path)
    if manifest.get("schema_version") != 2:
        fail(f"unsupported schema version: {manifest.get('schema_version')!r}")
    if manifest.get("tier") != tier or manifest.get("result") != "pass":
        fail(f"expected passing {tier} evidence")
    versions = manifest.get("versions", {})
    if versions.get("git_commit") != commit:
        fail(f"evidence commit {versions.get('git_commit')!r} does not match {commit!r}")
    if versions.get("git_status_porcelain"):
        fail("release evidence was captured from a dirty worktree")
    if not str(versions.get("rustc", "")).startswith("rustc 1.96.0"):
        fail(f"unexpected Rust toolchain: {versions.get('rustc')!r}")
    if versions.get("source_tree_sha256") != source_tree_sha256(root):
        fail("source tree hash does not match the checked-out commit")
    if manifest.get("registry_sha256") != registry_sha256:
        fail("release gate registry hash does not match the checked-out source")
    if manifest.get("datasets") != registry.get("datasets", {}):
        fail("release dataset declarations do not match the checked-out registry")

    expected_hashes = {}
    for relative in ("graph/Cargo.lock", "docs/package-lock.json", "flake.lock"):
        path = root / relative
        if path.is_file():
            expected_hashes[relative] = sha256_file(path)
    if manifest.get("lockfiles") != expected_hashes:
        fail("dependency lockfile hashes do not match the checked-out source")
    contracts = manifest.get("release_contracts", {})
    if set(contracts) != set(CONTRACT_PATHS):
        fail("release evidence does not contain the complete contract hash set")
    for relative, digest in contracts.items():
        path = root / relative
        if not path.is_file() or sha256_file(path) != digest:
            fail(f"release contract hash mismatch: {relative}")

    records = manifest.get("gates", [])
    expected_names = registry.get("tiers", {}).get(tier)
    if expected_names is None:
        fail(f"checked-out registry does not define tier {tier!r}")
    if [record.get("name") for record in records] != expected_names:
        fail("recorded gates do not exactly match the ordered release tier")
    if not records or any(record.get("result") != "pass" for record in records):
        fail("one or more recorded gates did not pass")
    for record in records:
        name = record["name"]
        gate = registry["gates"][name]
        expected_record = {
            "command": gate["command"],
            "cwd": gate.get("cwd", "."),
            "environment": gate.get("environment", {}),
            "timeout_seconds": gate["timeout_seconds"],
            "exclusive_resources": gate.get("exclusive_resources", []),
            "depends_on": gate.get("depends_on", []),
            "thresholds": gate.get("thresholds", {}),
            "fingerprint": gate_fingerprint(name, gate, versions, registry_sha256),
            "exit_code": 0,
        }
        for field, expected in expected_record.items():
            if record.get(field) != expected:
                fail(f"gate {name!r} has mismatched {field}")
        duration = record.get("duration_seconds")
        if not isinstance(duration, (int, float)) or duration < 0:
            fail(f"gate {name!r} has an invalid duration")
        if not record.get("artifacts"):
            fail(f"gate {name!r} has no retained artifact")
        for artifact in record["artifacts"]:
            path = Path(artifact["path"])
            path = path if path.is_absolute() else root / path
            if not path.is_file():
                fail(f"missing gate artifact: {artifact['path']}")
            if (
                path.stat().st_size != artifact.get("bytes")
                or sha256_file(path) != artifact.get("sha256")
            ):
                fail(f"gate artifact mismatch: {artifact['path']}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--tier", choices=("rc", "full-matrix"), required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    manifest = find_manifest(args.evidence.resolve())
    verify(manifest, args.tier, args.commit)
    print(f"release evidence verified: {args.tier} at {args.commit}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
