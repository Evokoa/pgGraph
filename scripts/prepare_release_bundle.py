#!/usr/bin/env python3
"""Build a deterministic pgGraph source bundle with SBOM and provenance."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import uuid
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def git_file(commit: str, path: str) -> str:
    return subprocess.check_output(
        ["git", "show", f"{commit}:{path}"], cwd=ROOT, text=True
    )


def cargo_packages(lock_text: str) -> list[dict[str, str]]:
    packages: list[dict[str, str]] = []
    for block in lock_text.split("[[package]]")[1:]:
        values = {}
        for key in ("name", "version", "source", "checksum"):
            match = re.search(rf'^\s*{key}\s*=\s*"([^"]+)"', block, re.MULTILINE)
            if match:
                values[key] = match.group(1)
        if "name" in values and "version" in values:
            packages.append(values)
    return packages


def npm_packages(lock_text: str) -> list[dict[str, str]]:
    lock = json.loads(lock_text)
    packages = []
    for path, value in lock.get("packages", {}).items():
        if not path.startswith("node_modules/") or not isinstance(value, dict):
            continue
        name = path.removeprefix("node_modules/")
        version = value.get("version")
        if version:
            packages.append(
                {
                    "name": name,
                    "version": str(version),
                    "license": str(value.get("license", "NOASSERTION")),
                    "resolved": str(value.get("resolved", "")),
                    "integrity": str(value.get("integrity", "")),
                }
            )
    return packages


def spdx_id(kind: str, name: str, version: str, index: int) -> str:
    token = re.sub(r"[^A-Za-z0-9.-]+", "-", f"{kind}-{name}-{version}-{index}")
    return f"SPDXRef-{token}"


def build_sbom(version: str, commit: str, created: str) -> dict:
    packages = [
        {
            "SPDXID": "SPDXRef-pgGraph",
            "name": "pgGraph",
            "versionInfo": version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "Apache-2.0",
            "licenseDeclared": "Apache-2.0",
            "copyrightText": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": f"pkg:github/evokoa/pggraph@{commit}",
                }
            ],
        }
    ]
    relationships = []
    cargo = sorted(
        cargo_packages(git_file(commit, "graph/Cargo.lock")),
        key=lambda value: (value["name"], value["version"], value.get("source", "")),
    )
    for index, package in enumerate(cargo):
        identifier = spdx_id("cargo", package["name"], package["version"], index)
        entry = {
            "SPDXID": identifier,
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": package.get("source", "NOASSERTION"),
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "copyrightText": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
                }
            ],
        }
        if checksum := package.get("checksum"):
            entry["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
        packages.append(entry)
        relationships.append(
            {
                "spdxElementId": "SPDXRef-pgGraph",
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": identifier,
            }
        )
    npm = sorted(
        npm_packages(git_file(commit, "docs/package-lock.json")),
        key=lambda value: (value["name"], value["version"]),
    )
    for index, package in enumerate(npm):
        identifier = spdx_id("npm", package["name"], package["version"], index)
        packages.append(
            {
                "SPDXID": identifier,
                "name": package["name"],
                "versionInfo": package["version"],
                "downloadLocation": package["resolved"] or "NOASSERTION",
                "filesAnalyzed": False,
                "licenseConcluded": "NOASSERTION",
                "licenseDeclared": package["license"],
                "copyrightText": "NOASSERTION",
                "externalRefs": [
                    {
                        "referenceCategory": "PACKAGE-MANAGER",
                        "referenceType": "purl",
                        "referenceLocator": f"pkg:npm/{package['name']}@{package['version']}",
                    }
                ],
            }
        )
        relationships.append(
            {
                "spdxElementId": identifier,
                "relationshipType": "DEV_DEPENDENCY_OF",
                "relatedSpdxElement": "SPDXRef-pgGraph",
            }
        )
    namespace_id = uuid.uuid5(uuid.NAMESPACE_URL, f"pggraph:{version}:{commit}")
    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"pgGraph-{version}",
        "documentNamespace": f"https://github.com/evokoa/pggraph/sbom/{namespace_id}",
        "creationInfo": {
            "created": created,
            "creators": ["Tool: pgGraph-prepare_release_bundle.py"],
        },
        "documentDescribes": ["SPDXRef-pgGraph"],
        "packages": packages,
        "relationships": relationships,
    }


def artifact(path: Path) -> dict[str, object]:
    return {"name": path.name, "sha256": sha256_file(path), "bytes": path.stat().st_size}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", help="Release version in X.Y.Z form; defaults to META.json at --ref")
    parser.add_argument("--ref", default="HEAD", help="Git commit or tag to package")
    parser.add_argument("--out-dir", type=Path, default=ROOT / "dist")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    commit = git("rev-parse", "--verify", f"{args.ref}^{{commit}}")
    version = args.version or json.loads(git_file(commit, "META.json"))["version"]
    if not VERSION_RE.fullmatch(version):
        raise SystemExit(f"--version must use X.Y.Z form: {version}")
    created = (
        datetime.fromisoformat(git("show", "-s", "--format=%cI", commit))
        .astimezone(timezone.utc)
        .isoformat()
        .replace("+00:00", "Z")
    )
    out_dir = args.out_dir.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    for path in out_dir.glob(f"pgGraph-{version}*"):
        if path.is_file():
            path.unlink()
    for name in ("SHA256SUMS", "release-manifest.json"):
        path = out_dir / name
        if path.exists():
            path.unlink()

    subprocess.run(
        [
            str(ROOT / "scripts" / "build_pgxn_dist.sh"),
            f"v{version}",
            str(out_dir),
            "--ref",
            commit,
        ],
        cwd=ROOT,
        check=True,
    )
    archive = out_dir / f"pgGraph-{version}.zip"
    sbom_path = out_dir / f"pgGraph-{version}.spdx.json"
    provenance_path = out_dir / f"pgGraph-{version}.provenance.json"
    sbom_path.write_text(
        json.dumps(build_sbom(version, commit, created), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    provenance = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": archive.name, "digest": {"sha256": sha256_file(archive)}}],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "buildType": "https://github.com/evokoa/pggraph/release/source-archive/v1",
                "externalParameters": {"version": version, "source_ref": args.ref},
                "resolvedDependencies": [
                    {"uri": "git+https://github.com/evokoa/pggraph.git", "digest": {"gitCommit": commit}}
                ],
            },
            "runDetails": {
                "builder": {"id": "https://github.com/evokoa/pggraph/scripts/prepare_release_bundle.py"},
                "metadata": {"startedOn": created, "finishedOn": created},
            },
        },
    }
    provenance_path.write_text(
        json.dumps(provenance, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    payload = [archive, sbom_path, provenance_path]
    manifest = {
        "schema_version": 1,
        "version": version,
        "intended_tag": f"v{version}",
        "source_commit": commit,
        "source_commit_time": created,
        "artifacts": [artifact(path) for path in payload],
    }
    manifest_path = out_dir / "release-manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    checksummed = [*payload, manifest_path]
    (out_dir / "SHA256SUMS").write_text(
        "".join(f"{sha256_file(path)}  {path.name}\n" for path in sorted(checksummed)),
        encoding="utf-8",
    )
    print(out_dir)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
