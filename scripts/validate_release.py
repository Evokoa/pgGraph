#!/usr/bin/env python3
"""Validate that release metadata agrees with the release tag."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VERSION_RE = re.compile(
    r"^v(?P<version>(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*))$"
)


def fail(message: str) -> None:
    print(f"release validation failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def run_git(args: list[str]) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def read_text(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tag",
        required=True,
        help="Release tag in vX.Y.Z form.",
    )
    parser.add_argument(
        "--check-main",
        action="store_true",
        help="Require the tagged commit to be contained in origin/main.",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    match = VERSION_RE.match(args.tag)
    if not match:
        fail(f"tag must use vX.Y.Z form, got {args.tag!r}")

    version = match.group("version")

    cargo = tomllib.loads(read_text("graph/Cargo.toml"))
    cargo_version = cargo["package"]["version"]
    if cargo_version != version:
        fail(f"graph/Cargo.toml version {cargo_version!r} does not match {version!r}")

    meta = json.loads(read_text("META.json"))
    if meta.get("version") != version:
        fail(f"META.json version {meta.get('version')!r} does not match {version!r}")

    provides = meta.get("provides", {}).get("graph", {})
    if provides.get("version") != version:
        fail(
            "META.json provides.graph.version "
            f"{provides.get('version')!r} does not match {version!r}"
        )

    if args.check_main:
        run_git(["fetch", "--no-tags", "origin", "main:refs/remotes/origin/main"])
        tag_sha = run_git(["rev-list", "-n", "1", args.tag])
        containing = run_git(["branch", "-r", "--contains", tag_sha])
        if "origin/main" not in containing.split():
            fail(f"{args.tag} commit {tag_sha} is not contained in origin/main")

    print(f"release validation passed for {args.tag}")


if __name__ == "__main__":
    main()
