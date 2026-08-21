#!/usr/bin/env python3
"""Verify that a P9 evidence producer is bound to one clean Git commit."""

from __future__ import annotations

import argparse
import os
import subprocess
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", required=True, type=Path)
    parser.add_argument("--evidence-dir", required=True, type=Path)
    parser.add_argument("--measurement-commit", required=True)
    return parser.parse_args()


def command(*arguments: str, cwd: Path) -> str:
    return subprocess.run(
        arguments,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def clean_except_evidence(repo: Path, evidence: Path) -> bool:
    try:
        relative_evidence = evidence.resolve().relative_to(repo.resolve()).as_posix()
    except ValueError:
        relative_evidence = None
    raw = subprocess.run(
        ["git", "status", "--porcelain=v1", "-z", "--untracked-files=all"],
        cwd=repo,
        check=True,
        capture_output=True,
    ).stdout
    for entry in (item for item in raw.split(b"\0") if item):
        if len(entry) < 4 or b"R" in entry[:2] or b"C" in entry[:2]:
            return False
        path = os.fsdecode(entry[3:])
        if relative_evidence is None or not (
            path == relative_evidence or path.startswith(f"{relative_evidence}/")
        ):
            return False
    return True


def main() -> int:
    args = parse_args()
    repo = args.repo_root.resolve()
    evidence = args.evidence_dir.resolve()
    head = command("git", "rev-parse", "HEAD", cwd=repo)
    if len(args.measurement_commit) != 40 or args.measurement_commit != head:
        raise ValueError("measurement commit must equal the exact current Git HEAD")
    if not clean_except_evidence(repo, evidence):
        raise ValueError("source changes outside the evidence directory are not allowed")
    print(head)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
