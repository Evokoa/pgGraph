#!/usr/bin/env python3
"""Bounded executable self-test for the P9 evidence producer interfaces."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EVIDENCE = ROOT / "todo/measurements/2026-08-13-p9-open-type-query"
sys.dont_write_bytecode = True


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def rejects(callable_value) -> None:
    try:
        callable_value()
    except (ValueError, KeyError):
        return
    raise AssertionError("tampered evidence was accepted")


def main() -> int:
    extractor = load("p9_extractor", ROOT / "scripts/extract_p9_open_type_criterion.py")
    summarizer = load("p9_summarizer", ROOT / "scripts/summarize_p9_open_type_postgres.py")
    checker = load("p9_checker", EVIDENCE / "check_results.py")
    cases = json.loads((EVIDENCE / "cases.json").read_text(encoding="utf-8"))
    assert len(extractor.expected_dimensions(cases)) == 65
    fixed = [float(value) for value in range(1, 41)]
    assert summarizer.median_interval(fixed) == (14.0, 27.0)
    assert checker.median_interval(fixed) == (14.0, 27.0)

    with tempfile.TemporaryDirectory(prefix="pggraph-p9-tools-") as temporary:
        temporary_path = Path(temporary)
        criterion_root = temporary_path / "criterion"
        sanitized = criterion_root / "open_type_bfs" / "query_surface=bfs" / "truncated" / "new"
        sanitized.mkdir(parents=True)
        estimates_fixture = sanitized / "estimates.json"
        estimates_fixture.write_text("{}", encoding="utf-8")
        full_id = "open_type_bfs/query_surface=bfs/label_count=65536/encoding=adaptive_u32"
        (sanitized / "benchmark.json").write_text(
            json.dumps(
                {
                    "full_id": full_id,
                    "directory_name": "open_type_bfs/query_surface=bfs/truncated",
                }
            ),
            encoding="utf-8",
        )
        assert extractor.benchmark_identity(estimates_fixture, criterion_root)[0] == full_id
        (sanitized / "benchmark.json").write_text(
            json.dumps({"full_id": full_id, "directory_name": "wrong"}),
            encoding="utf-8",
        )
        rejects(lambda: extractor.benchmark_identity(estimates_fixture, criterion_root))

        raw = temporary_path / "case.123"
        raw.write_text(
            "".join(
                f"0 {transaction} {transaction}.0 0 1700000000 1\n"
                for transaction in range(1, 51)
            ),
            encoding="utf-8",
        )
        expected = [value * 1_000 for value in range(11, 51)]
        assert summarizer.pgbench_latencies(raw) == expected
        assert checker.pgbench_latencies(raw) == expected
        raw.write_text(raw.read_text(encoding="utf-8").replace("0 50 50.0", "0 49 50.0"), encoding="utf-8")
        rejects(lambda: summarizer.pgbench_latencies(raw))
        rejects(lambda: checker.pgbench_latencies(raw))

    for script in (
        ROOT / "scripts/extract_p9_open_type_criterion.py",
        ROOT / "scripts/summarize_p9_open_type_postgres.py",
        ROOT / "scripts/write_p9_open_type_run_metadata.py",
        ROOT / "scripts/verify_p9_measurement_source.py",
        EVIDENCE / "check_results.py",
    ):
        subprocess.run(["python3", str(script), "--help"], check=True, capture_output=True)
    print("P9 evidence tooling self-test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
