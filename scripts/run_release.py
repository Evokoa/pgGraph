#!/usr/bin/env python3
"""Run a named pgGraph release tier and write resumable JSON evidence."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import platform
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "release" / "gates.json"
CONTROL_ENV_PREFIXES = (
    "RUN_",
    "SKIP_",
    "PREPARE_",
    "PGGRAPH_",
    "MAX_",
    "MIN_",
    "SYNTHETIC_",
    "BUILD_MEMORY_",
)
CONTROL_ENV_NAMES = {
    "CLIENTS",
    "DBNAME",
    "DB_PREFIX",
    "JOBS",
    "PG_CONFIG",
    "PG_VERSION_FEATURE",
    "PG_VERSIONS",
    "PGDATA",
    "PGDATABASE",
    "PGHOST",
    "PGPASSWORD",
    "PGPORT",
    "PGSERVICE",
    "PGUSER",
    "ROUNDS",
    "TIME",
}


def load_json(path: Path) -> dict:
    with path.open(encoding="utf-8") as stream:
        return json.load(stream)


def fingerprint(name: str, gate: dict, versions: dict[str, str], registry_sha256: str) -> str:
    payload = json.dumps({"name": name, "gate": gate, "versions": versions, "registry_sha256": registry_sha256}, sort_keys=True)
    return hashlib.sha256(payload.encode()).hexdigest()


def command_version(command: list[str]) -> str:
    try:
        proc = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=10, check=False)
    except (OSError, subprocess.SubprocessError) as exc:
        return f"unavailable: {exc}"
    return (proc.stdout or proc.stderr).splitlines()[0][:300] if proc.returncode == 0 else "unavailable"


def source_tree_sha256(excluded: Path) -> str:
    proc = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=ROOT,
        capture_output=True,
        check=True,
    )
    digest = hashlib.sha256()
    excluded_relative = excluded.relative_to(ROOT).as_posix() if excluded.is_relative_to(ROOT) else ""
    for raw_path in proc.stdout.split(b"\0"):
        if not raw_path:
            continue
        relative = raw_path.decode("utf-8", errors="surrogateescape")
        if relative == excluded_relative or relative.startswith("release/evidence/") or "__pycache__" in relative or relative.endswith(".pyc"):
            continue
        path = ROOT / relative
        if path.is_file():
            digest.update(raw_path)
            digest.update(b"\0")
            digest.update(path.read_bytes())
            digest.update(b"\0")
    return digest.hexdigest()


def atomic_write(path: Path, value: dict) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def acquire_locks(resources: list[str]) -> list[object]:
    lock_dir = ROOT / "release" / "evidence" / ".locks"
    lock_dir.mkdir(parents=True, exist_ok=True)
    locks: list[object] = []
    try:
        for resource in sorted(resources):
            lock = (lock_dir / f"{resource}.lock").open("w", encoding="utf-8")
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            lock.write(f"pid={os.getpid()}\n")
            lock.flush()
            locks.append(lock)
    except (OSError, BlockingIOError):
        for lock in locks:
            lock.close()
        raise RuntimeError(f"exclusive release resource is already in use: {resource}")
    return locks


def gate_environment(gate: dict) -> dict[str, str]:
    """Return the caller environment without undeclared release controls."""
    declared = {key: str(value) for key, value in gate.get("environment", {}).items()}
    environment = os.environ.copy()
    for key in list(environment):
        if key in CONTROL_ENV_NAMES or key.startswith(CONTROL_ENV_PREFIXES):
            environment.pop(key)
    environment.update(declared)
    return environment


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tier", choices=("pr", "nightly", "rc", "full-matrix"), default="pr")
    parser.add_argument("--evidence", type=Path, help="evidence manifest path")
    parser.add_argument("--resume", action="store_true", help="reuse passing gates with the same fingerprint")
    parser.add_argument("--list", action="store_true", help="list tiers and gates without running them")
    args = parser.parse_args()
    registry = load_json(REGISTRY)
    if args.list:
        for tier, gates in registry["tiers"].items():
            print(f"{tier}: {', '.join(gates)}")
        return 0
    evidence_path = (args.evidence or ROOT / "release" / "evidence" / f"{args.tier}.json").resolve()
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    previous = load_json(evidence_path) if args.resume and evidence_path.exists() else {}
    prior_gates = {gate["name"]: gate for gate in previous.get("gates", [])}
    versions = {
        "git_commit": command_version(["git", "rev-parse", "HEAD"]),
        "rustc": command_version(["rustc", "--version"]),
        "cargo": command_version(["cargo", "--version"]),
        "postgres": command_version(["pg_config", "--version"]),
        "python": platform.python_version(),
        "platform": platform.platform(),
        "pggraph": next(
            line.split("=", 1)[1].strip().strip('"')
            for line in (ROOT / "graph" / "Cargo.toml").read_text(encoding="utf-8").splitlines()
            if line.startswith("version = ")
        ),
        "source_tree_sha256": source_tree_sha256(evidence_path),
    }
    registry_sha256 = hashlib.sha256(REGISTRY.read_bytes()).hexdigest()
    manifest = {
        "schema_version": 1,
        "tier": args.tier,
        "started_at": datetime.now(timezone.utc).isoformat(),
        "versions": versions,
        "registry_sha256": registry_sha256,
        "datasets": registry.get("datasets", {}),
        "gates": [],
    }
    for name in registry["tiers"][args.tier]:
        gate = registry["gates"][name]
        completed = {record["name"] for record in manifest["gates"] if record["result"] == "pass"}
        missing_dependencies = set(gate.get("depends_on", [])) - completed
        if missing_dependencies:
            print(f"{name}: unmet dependencies: {', '.join(sorted(missing_dependencies))}", file=sys.stderr)
            return 2
        digest = fingerprint(name, gate, versions, registry_sha256)
        if args.resume and prior_gates.get(name, {}).get("result") == "pass" and prior_gates[name].get("fingerprint") == digest:
            record = dict(prior_gates[name])
            record["resumed"] = True
            manifest["gates"].append(record)
            atomic_write(evidence_path, manifest)
            print(f"==> {name}: using matching passing evidence")
            continue
        command = gate["command"]
        cwd = ROOT / gate.get("cwd", ".")
        environment = gate_environment(gate)
        started = time.monotonic()
        print(f"==> {name}: {subprocess.list2cmdline(command)}", flush=True)
        locks: list[object] = []
        try:
            locks = acquire_locks(gate.get("exclusive_resources", []))
            try:
                proc = subprocess.run(command, cwd=cwd, env=environment, timeout=gate["timeout_seconds"], check=False)
                result = "pass" if proc.returncode == 0 else "fail"
                exit_code = proc.returncode
            finally:
                for lock in locks:
                    lock.close()
        except subprocess.TimeoutExpired:
            result, exit_code = "timeout", 124
        except RuntimeError as exc:
            print(f"{name}: {exc}", file=sys.stderr)
            result, exit_code = "blocked", 2
        record = {
            "name": name,
            "command": command,
            "cwd": cwd.relative_to(ROOT).as_posix(),
            "environment": gate.get("environment", {}),
            "timeout_seconds": gate["timeout_seconds"],
            "exclusive_resources": gate.get("exclusive_resources", []),
            "depends_on": gate.get("depends_on", []),
            "thresholds": gate.get("thresholds", {}),
            "duration_seconds": round(time.monotonic() - started, 3),
            "result": result,
            "exit_code": exit_code,
            "fingerprint": digest,
            "artifacts": [evidence_path.relative_to(ROOT).as_posix()] if evidence_path.is_relative_to(ROOT) else [str(evidence_path)],
        }
        manifest["gates"].append(record)
        atomic_write(evidence_path, manifest)
        if result != "pass":
            return exit_code or 1
    manifest["completed_at"] = datetime.now(timezone.utc).isoformat()
    manifest["result"] = "pass"
    atomic_write(evidence_path, manifest)
    print(f"Release tier {args.tier} passed; evidence: {evidence_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
