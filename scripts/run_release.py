#!/usr/bin/env python3
"""Run a named pgGraph release tier and write resumable JSON evidence."""

from __future__ import annotations

import argparse
import fcntl
import glob
import hashlib
import json
import os
import platform
import signal
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "release" / "gates.json"
RELEASE_CONTRACT_PATHS = (
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


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def lockfile_hashes() -> dict[str, str]:
    hashes = {}
    for relative in ("graph/Cargo.lock", "docs/package-lock.json", "flake.lock"):
        path = ROOT / relative
        if path.is_file():
            hashes[relative] = sha256_file(path)
    return hashes


def validate_registry(registry: dict) -> None:
    """Reject invalid tier dependencies and unsafe crash-gate declarations."""
    gates = registry.get("gates", {})
    for tier, names in registry.get("tiers", {}).items():
        completed: set[str] = set()
        for name in names:
            if name not in gates:
                raise ValueError(f"tier {tier!r} references unknown gate {name!r}")
            missing = set(gates[name].get("depends_on", [])) - completed
            if missing:
                raise ValueError(
                    f"tier {tier!r} gate {name!r} precedes dependencies: "
                    f"{', '.join(sorted(missing))}"
                )
            completed.add(name)
    for name, gate in gates.items():
        environment = gate.get("environment", {})
        crash_enabled = environment.get("RUN_CRASH") == "1" or environment.get("RUN_TX_DELTA_CRASH") == "1"
        command = gate.get("command", [])
        disposable_wrapper = command and Path(command[0]).name == "with_disposable_postgres.sh"
        destructive_cluster_script = any(
            Path(argument).name in {"crash_recovery.sh", "tx_delta_crash_recovery.sh"}
            for argument in command
        )
        if crash_enabled and "PGDATA" not in environment and not disposable_wrapper:
            raise ValueError(
                f"gate {name!r} enables crash tests without PGDATA or "
                "with_disposable_postgres.sh"
            )
        if destructive_cluster_script and not disposable_wrapper:
            raise ValueError(
                f"gate {name!r} invokes a destructive cluster script without "
                "with_disposable_postgres.sh"
            )


def fingerprint(name: str, gate: dict, versions: dict[str, str], registry_sha256: str) -> str:
    payload = json.dumps({"name": name, "gate": gate, "versions": versions, "registry_sha256": registry_sha256}, sort_keys=True)
    return hashlib.sha256(payload.encode()).hexdigest()


def command_version(command: list[str], cwd: Path = ROOT) -> str:
    try:
        proc = subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=10, check=False)
    except (OSError, subprocess.SubprocessError) as exc:
        return f"unavailable: {exc}"
    if proc.returncode != 0:
        return "unavailable"
    lines = (proc.stdout or proc.stderr).splitlines()
    return lines[0][:300] if lines else ""


def git_status_porcelain() -> list[str]:
    proc = subprocess.run(
        ["git", "status", "--porcelain=v1"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return proc.stdout.splitlines()


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


def run_gate(command: list[str], cwd: Path, environment: dict[str, str], timeout: int, log_path: Path) -> tuple[str, int]:
    """Run one gate while streaming and retaining its combined output."""
    log_path.parent.mkdir(parents=True, exist_ok=True)
    with log_path.open("w", encoding="utf-8") as log:
        proc = subprocess.Popen(
            command,
            cwd=cwd,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            start_new_session=True,
        )

        def pump_output() -> None:
            assert proc.stdout is not None
            for line in proc.stdout:
                log.write(line)
                log.flush()
                sys.stdout.write(line)
                sys.stdout.flush()

        pump = threading.Thread(target=pump_output, daemon=True)
        pump.start()
        try:
            exit_code = proc.wait(timeout=timeout)
            result = "pass" if exit_code == 0 else "fail"
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGTERM)
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
            result, exit_code = "timeout", 124
        except KeyboardInterrupt:
            os.killpg(proc.pid, signal.SIGTERM)
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
            result, exit_code = "interrupted", 130
        pump.join(timeout=10)
        if proc.stdout is not None:
            proc.stdout.close()
    return result, exit_code


def artifact_record(path: Path) -> dict[str, object]:
    relative = path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else str(path)
    return {"path": relative, "sha256": sha256_file(path), "bytes": path.stat().st_size}


def collect_artifacts(gate: dict, log_path: Path) -> list[dict[str, object]]:
    paths = {log_path.resolve()}
    for pattern in gate.get("artifacts", []):
        candidate = (ROOT / pattern).resolve()
        if not candidate.is_relative_to(ROOT):
            raise ValueError(f"artifact path escapes repository: {pattern}")
        paths.update(Path(match).resolve() for match in glob.glob(str(candidate), recursive=True))
    return [artifact_record(path) for path in sorted(paths) if path.is_file()]


def passing_record_is_reusable(record: dict) -> bool:
    if record.get("result") != "pass":
        return False
    for artifact in record.get("artifacts", []):
        path = Path(artifact["path"])
        path = path if path.is_absolute() else ROOT / path
        if not path.is_file() or sha256_file(path) != artifact.get("sha256"):
            return False
    return True


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
    try:
        validate_registry(registry)
    except ValueError as exc:
        print(f"invalid release registry: {exc}", file=sys.stderr)
        return 2
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
        "git_status_porcelain": git_status_porcelain(),
        "rustc": command_version(["rustc", "--version"], ROOT / "graph"),
        "cargo": command_version(["cargo", "--version"], ROOT / "graph"),
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
        "schema_version": 2,
        "tier": args.tier,
        "started_at": datetime.now(timezone.utc).isoformat(),
        "versions": versions,
        "registry_sha256": registry_sha256,
        "lockfiles": lockfile_hashes(),
        "release_contracts": {
            relative: sha256_file(ROOT / relative)
            for relative in RELEASE_CONTRACT_PATHS
        },
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
        if args.resume and prior_gates.get(name, {}).get("fingerprint") == digest and passing_record_is_reusable(prior_gates[name]):
            record = dict(prior_gates[name])
            record["resumed"] = True
            manifest["gates"].append(record)
            atomic_write(evidence_path, manifest)
            print(f"==> {name}: using matching passing evidence")
            continue
        command = gate["command"]
        cwd = ROOT / gate.get("cwd", ".")
        environment = gate_environment(gate)
        log_path = evidence_path.parent / "logs" / evidence_path.stem / f"{name}.log"
        started = time.monotonic()
        print(f"==> {name}: {subprocess.list2cmdline(command)}", flush=True)
        locks: list[object] = []
        try:
            locks = acquire_locks(gate.get("exclusive_resources", []))
            try:
                result, exit_code = run_gate(
                    command,
                    cwd,
                    environment,
                    gate["timeout_seconds"],
                    log_path,
                )
            finally:
                for lock in locks:
                    lock.close()
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
            "artifacts": collect_artifacts(gate, log_path),
        }
        manifest["gates"].append(record)
        atomic_write(evidence_path, manifest)
        if result != "pass":
            manifest["completed_at"] = datetime.now(timezone.utc).isoformat()
            manifest["result"] = result
            manifest["failed_gate"] = name
            atomic_write(evidence_path, manifest)
            return exit_code or 1
    manifest["completed_at"] = datetime.now(timezone.utc).isoformat()
    manifest["result"] = "pass"
    atomic_write(evidence_path, manifest)
    print(f"Release tier {args.tier} passed; evidence: {evidence_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
