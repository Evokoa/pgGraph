#!/usr/bin/env python3
"""Write machine-readable metadata for a completed P9 evidence capture."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import subprocess
import tempfile
from pathlib import Path


BUDGET_COMMIT = "dd730b8"
CRITERION_COMMAND = "./graph/tests/heavy/run_open_type_query_criterion.sh"
CRITERION_BENCHMARK_COMMAND = (
    "cargo +1.96.0 bench --features 'pg17 benchmarks' "
    "--bench open_type_query_bench"
)
POSTGRES_COMMAND = "./graph/tests/heavy/open_type_query_latency.sh"
RESOURCE_COMMAND = "./graph/tests/heavy/run_open_type_query_resources_docker.sh"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", required=True, type=Path)
    parser.add_argument("--evidence-dir", required=True, type=Path)
    parser.add_argument("--measurement-commit", required=True)
    return parser.parse_args()


def command(*arguments: str, cwd: Path | None = None) -> str:
    return subprocess.run(
        arguments,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def clean_except_evidence(repo: Path, evidence: Path) -> bool:
    relative_evidence = evidence.resolve().relative_to(repo.resolve()).as_posix()
    raw = subprocess.run(
        ["git", "status", "--porcelain=v1", "-z"],
        cwd=repo,
        check=True,
        capture_output=True,
    ).stdout
    entries = [entry for entry in raw.split(b"\0") if entry]
    index = 0
    while index < len(entries):
        entry = entries[index]
        if len(entry) < 4:
            return False
        status = entry[:2]
        path = os.fsdecode(entry[3:])
        if b"R" in status or b"C" in status:
            return False
        if not (path == relative_evidence or path.startswith(f"{relative_evidence}/")):
            return False
        index += 1
    return True


def required_text(path: Path) -> str:
    value = path.read_text(encoding="utf-8").strip()
    if not value:
        raise ValueError(f"required metadata file is empty: {path}")
    return value


def sha256(path: Path) -> str:
    payload = path.read_bytes()
    if not payload:
        raise ValueError(f"required evidence file is empty: {path}")
    return hashlib.sha256(payload).hexdigest()


def main() -> int:
    args = parse_args()
    repo = args.repo_root.resolve()
    evidence = args.evidence_dir.resolve()
    head = command("git", "rev-parse", "HEAD", cwd=repo)
    if args.measurement_commit != head or len(head) != 40:
        raise ValueError("measurement commit must equal the exact current Git HEAD")
    if not clean_except_evidence(repo, evidence):
        raise ValueError("source changes outside the evidence directory are not allowed")
    linux_kernel = required_text(evidence / "linux-uname.txt")
    postgres_version = required_text(evidence / "latency-postgres-version.txt")
    image_inspect = json.loads(
        (evidence / "docker-image-inspect.json").read_text(encoding="utf-8")
    )
    if not isinstance(image_inspect, list) or len(image_inspect) != 1:
        raise ValueError("Docker image inspection must contain exactly one image")
    image = image_inspect[0]
    metadata = {
        "schema_version": 1,
        "budget_commit": BUDGET_COMMIT,
        "measurement_commit": head,
        "git_status_clean": True,
        "criterion": {
            "command": CRITERION_COMMAND,
            "benchmark_command": CRITERION_BENCHMARK_COMMAND,
            "rustc": command("rustup", "run", "1.96.0", "rustc", "--version", cwd=repo),
            "cargo": command("cargo", "+1.96.0", "--version", cwd=repo),
            "run_log_sha256": sha256(evidence / "criterion-run.log"),
        },
        "postgres": {
            "command": POSTGRES_COMMAND,
            "version": postgres_version,
            "pgbench": required_text(evidence / "latency-pgbench-version.txt"),
        },
        "resources": {
            "command": RESOURCE_COMMAND,
            "os": "Linux",
            "kernel": linux_kernel,
            "postgres": required_text(evidence / "resource-postgres-version.txt"),
            "docker": json.loads((evidence / "docker-version.json").read_text(encoding="utf-8")),
            "image_id": image["Id"],
            "repo_digests": image.get("RepoDigests") or [],
            "docker_build_log_sha256": sha256(evidence / "docker-build.log"),
            "docker_resource_log_sha256": sha256(evidence / "docker-resource.log"),
        },
        "capture_host": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
        },
    }
    output = evidence / "run-metadata.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", encoding="utf-8", dir=output.parent, delete=False
    ) as handle:
        json.dump(metadata, handle, indent=2, sort_keys=True)
        handle.write("\n")
        temporary = Path(handle.name)
    os.replace(temporary, output)
    print(f"Wrote {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
