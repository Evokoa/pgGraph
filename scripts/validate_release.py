#!/usr/bin/env python3
"""Validate pgGraph release metadata for preparation or publication."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
CARGO_PACKAGE_RE = re.compile(r"^\[package\]\s*(.*?)(?=^\[|\Z)", re.MULTILINE | re.DOTALL)
CARGO_VERSION_RE = re.compile(r'^version\s*=\s*"([^"]+)"\s*$', re.MULTILINE)
ACTION_SHA_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40}$")
IMAGE_DIGEST_RE = re.compile(r"^[^\s@]+(?::[^\s@]+)?@sha256:[0-9a-f]{64}$")


def fail(message: str) -> None:
    print(f"release validation failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def run_git(args: list[str]) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def read_text(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def validate_version_metadata(version: str) -> None:
    package_match = CARGO_PACKAGE_RE.search(read_text("graph/Cargo.toml"))
    cargo_match = CARGO_VERSION_RE.search(package_match.group(1)) if package_match else None
    if cargo_match is None:
        fail("graph/Cargo.toml does not declare a package version")
    cargo_version = cargo_match.group(1)
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

    maturity = json.loads(read_text("release/maturity.json"))
    if maturity.get("candidate_version") != version:
        fail(
            f"release/maturity.json candidate_version {maturity.get('candidate_version')!r} "
            f"does not match {version!r}"
        )
    release_heading = f"## v{version}:"
    if release_heading not in read_text("docs/release-notes.mdx"):
        fail(f"docs/release-notes.mdx is missing {release_heading}")


def validate_release_dependencies() -> None:
    for path in (".github/workflows/ci.yml", ".github/workflows/release.yml"):
        for line_number, line in enumerate(read_text(path).splitlines(), start=1):
            match = re.match(r"\s*-?\s*uses:\s*([^\s#]+)", line)
            if match:
                dependency = match.group(1)
                if not dependency.startswith("./") and not ACTION_SHA_RE.fullmatch(dependency):
                    fail(f"{path}:{line_number} action is not pinned to a full commit: {dependency}")
            match = re.match(r"\s*container:\s*([^\s#]+)", line)
            if match and not IMAGE_DIGEST_RE.fullmatch(match.group(1)):
                fail(f"{path}:{line_number} container is not pinned by digest: {match.group(1)}")

    dockerfile = read_text("Dockerfile")
    for name in ("RUST_IMAGE", "POSTGRES_IMAGE"):
        match = re.search(rf"^ARG {name}=([^\s]+)$", dockerfile, re.MULTILINE)
        if not match or not IMAGE_DIGEST_RE.fullmatch(match.group(1)):
            fail(f"Dockerfile {name} must be pinned by digest")
    matrix_image = read_text("graph/tests/heavy/Dockerfile.pg-matrix").splitlines()[0]
    if not re.fullmatch(r"FROM\s+[^\s]+@sha256:[0-9a-f]{64}", matrix_image):
        fail("Dockerfile.pg-matrix builder image must be pinned by digest")

    release_workflow = read_text(".github/workflows/release.yml")
    postgres_images = re.findall(r'image="(postgres:[^"]+)"', release_workflow)
    expected_majors = {str(major) for major in range(14, 19)}
    actual_majors: set[str] = set()
    for image in postgres_images:
        if not IMAGE_DIGEST_RE.fullmatch(image):
            fail(f"release workflow PostgreSQL image is not pinned by digest: {image}")
        major_match = re.fullmatch(r"postgres:(\d+)-bookworm@sha256:[0-9a-f]{64}", image)
        if major_match:
            actual_majors.add(major_match.group(1))
    if actual_majors != expected_majors:
        fail(
            "release workflow must pin PostgreSQL base images for majors 14 through 18; "
            f"found {sorted(actual_majors)}"
        )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--version", help="Prepare X.Y.Z from --ref before tagging")
    mode.add_argument("--tag", help="Publish an existing vX.Y.Z tag")
    parser.add_argument("--ref", default="HEAD", help="Immutable preparation ref")
    parser.add_argument("--check-main", action="store_true")
    parser.add_argument("--require-clean", action="store_true")
    parser.add_argument("--require-signed-tag", action="store_true")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.version:
        version = args.version
        if not VERSION_RE.fullmatch(version):
            fail(f"version must use X.Y.Z form, got {version!r}")
        release_ref = args.ref
    else:
        tag_match = re.fullmatch(r"v(.+)", args.tag)
        if not tag_match or not VERSION_RE.fullmatch(tag_match.group(1)):
            fail(f"tag must use vX.Y.Z form, got {args.tag!r}")
        version = tag_match.group(1)
        release_ref = args.tag
        if run_git(["cat-file", "-t", args.tag]) != "tag":
            fail(f"{args.tag} must be an annotated tag")
        if args.require_signed_tag:
            try:
                subprocess.run(["git", "verify-tag", args.tag], cwd=ROOT, check=True)
            except subprocess.CalledProcessError:
                fail(f"signature verification failed for {args.tag}")

    release_commit = run_git(["rev-parse", "--verify", f"{release_ref}^{{commit}}"])
    head_commit = run_git(["rev-parse", "HEAD"])
    if release_commit != head_commit:
        fail(f"checked-out HEAD {head_commit} does not match release ref {release_commit}")
    if args.require_clean and run_git(["status", "--porcelain=v1"]):
        fail("release worktree is not clean")

    validate_version_metadata(version)
    validate_release_dependencies()

    if args.check_main:
        run_git(["fetch", "--no-tags", "origin", "main:refs/remotes/origin/main"])
        containing = run_git(["branch", "-r", "--contains", release_commit])
        if "origin/main" not in containing.split():
            fail(f"release commit {release_commit} is not contained in origin/main")

    label = args.tag if args.tag else f"v{version} candidate"
    print(f"release validation passed for {label} at {release_commit}")


if __name__ == "__main__":
    main()
