#!/usr/bin/env python3
"""Verify immutable release fixtures and their signature-bearing tag binding."""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "release" / "fixtures" / "alpha-to-v1.0"


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def main() -> int:
    failures: list[str] = []
    metadata = json.loads((FIXTURE / "metadata.json").read_text(encoding="utf-8"))
    tag = metadata["producer_tag"]
    commit = metadata["producer_commit"]
    try:
        if git("cat-file", "-t", tag) != "tag":
            failures.append(f"producer {tag} is not an annotated tag")
        tag_object = git("cat-file", "-p", tag)
        if "-----BEGIN SSH SIGNATURE-----" not in tag_object:
            failures.append(f"producer tag {tag} has no SSH signature")
        if git("rev-parse", f"{tag}^{{commit}}") != commit:
            failures.append(f"producer tag {tag} does not resolve to {commit}")
    except subprocess.CalledProcessError as error:
        failures.append(f"cannot resolve producer tag {tag}: {error}")

    expected_files = {"source.sql", "register.sql", "verify.sql", "metadata.json"}
    manifest: dict[str, str] = {}
    for line in (FIXTURE / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        checksum, name = line.split(maxsplit=1)
        manifest[name] = checksum
    if set(manifest) != expected_files:
        failures.append("alpha fixture SHA256SUMS file set differs from the immutable contract")
    for name, expected in manifest.items():
        actual = hashlib.sha256((FIXTURE / name).read_bytes()).hexdigest()
        if actual != expected:
            failures.append(f"alpha fixture checksum mismatch for {name}")

    if failures:
        for failure in failures:
            print(f"fixture integrity: {failure}", file=sys.stderr)
        return 1
    print(f"Alpha fixture integrity passed for signature-bearing producer tag {tag}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
