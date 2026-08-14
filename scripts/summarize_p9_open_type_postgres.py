#!/usr/bin/env python3
"""Recompute P9 PostgreSQL latency summaries from retained raw samples."""

from __future__ import annotations

import argparse
import csv
import hashlib
import math
import os
import statistics
import tempfile
from collections import defaultdict
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--samples", required=True, type=Path)
    parser.add_argument("--raw-log-dir", required=True, type=Path)
    parser.add_argument("--hashes-output", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args()


def median_interval(samples: list[float]) -> tuple[float, float]:
    """Return a distribution-free two-sided 95% interval for the median."""
    ordered = sorted(samples)
    count = len(ordered)
    cumulative = 0.0
    tail_rank = 0
    for successes in range(count + 1):
        candidate = cumulative + math.comb(count, successes) * 0.5**count
        if candidate > 0.025:
            break
        cumulative = candidate
        tail_rank = successes + 1
    lower_index = max(0, tail_rank - 1)
    upper_index = min(count - 1, count - tail_rank)
    return ordered[lower_index], ordered[upper_index]


def pgbench_latencies(path: Path) -> list[int]:
    parsed: list[tuple[int, int]] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        fields = line.split()
        if len(fields) < 6:
            raise ValueError(f"{path}:{line_number} is not a complete pgbench log row")
        if int(fields[0]) != 0 or int(fields[3]) != 0:
            raise ValueError(f"{path}:{line_number} is not the one-client/one-script case")
        transaction = int(fields[1])
        latency_ns = round(float(fields[2]) * 1_000.0)
        if latency_ns <= 0 or int(fields[4]) <= 0 or int(fields[5]) < 0:
            raise ValueError(f"{path}:{line_number} contains invalid pgbench timing data")
        parsed.append((transaction, latency_ns))
    if len(parsed) != 50:
        raise ValueError(f"{path} has {len(parsed)} pgbench rows, expected 50")
    transaction_numbers = [item[0] for item in parsed]
    if transaction_numbers not in (list(range(50)), list(range(1, 51))):
        raise ValueError(f"{path} transaction numbers are not contiguous")
    return [item[1] for item in parsed[10:]]


def main() -> int:
    args = parse_args()
    if median_interval([float(value) for value in range(1, 41)]) != (14.0, 27.0):
        raise AssertionError("the fixed n=40 median interval regression failed")
    grouped: dict[tuple[str, int], list[tuple[int, float, str]]] = defaultdict(list)
    with args.samples.open(newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        expected = [
            "query_surface",
            "label_count",
            "sample_index",
            "elapsed_ns",
            "result_digest",
        ]
        if reader.fieldnames != expected:
            raise ValueError(f"unexpected PostgreSQL sample header: {reader.fieldnames}")
        for row in reader:
            elapsed = float(row["elapsed_ns"])
            if not math.isfinite(elapsed) or elapsed <= 0:
                raise ValueError("PostgreSQL elapsed_ns must be finite and positive")
            grouped[(row["query_surface"], int(row["label_count"]))].append(
                (int(row["sample_index"]), elapsed, row["result_digest"])
            )

    expected_cases = {
        (surface, labels)
        for surface in ("traverse", "shortest_path", "gql", "cypher")
        for labels in (254, 65_536)
    }
    if set(grouped) != expected_cases:
        raise ValueError(f"unexpected PostgreSQL cases: {sorted(grouped)}")

    rows: list[list[object]] = []
    hash_rows: list[list[object]] = []
    for (surface, labels), samples in sorted(grouped.items()):
        if len(samples) != 40:
            raise ValueError(f"{surface}/{labels} has {len(samples)} samples, expected 40")
        indices = {sample[0] for sample in samples}
        if indices != set(range(1, 41)):
            raise ValueError(f"{surface}/{labels} sample indices are incomplete")
        digests = {sample[2] for sample in samples}
        if len(digests) != 1 or "" in digests:
            raise ValueError(f"{surface}/{labels} result digest is unstable")
        elapsed = [sample[1] for sample in samples]
        log_candidates = sorted(args.raw_log_dir.glob(f"{surface}-{labels}.[0-9]*"))
        log_candidates = [
            path for path in log_candidates if path.name.rsplit(".", 1)[-1].isdigit()
        ]
        if len(log_candidates) != 1:
            raise ValueError(
                f"{surface}/{labels} needs exactly one raw pgbench log, found {len(log_candidates)}"
            )
        log_path = log_candidates[0]
        raw_elapsed = pgbench_latencies(log_path)
        if [round(value) for value in elapsed] != raw_elapsed:
            raise ValueError(f"{surface}/{labels} samples differ from the raw pgbench log")
        hash_rows.append(
            [
                surface,
                labels,
                log_path.relative_to(args.raw_log_dir.parent.parent).as_posix(),
                hashlib.sha256(log_path.read_bytes()).hexdigest(),
            ]
        )
        median = statistics.median(elapsed)
        lower, upper = median_interval(elapsed)
        throughput = 1_000_000_000.0 * len(elapsed) / sum(elapsed)
        rows.append([surface, labels, len(elapsed), median, lower, upper, throughput])

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", newline="", encoding="utf-8", dir=args.output.parent, delete=False
    ) as handle:
        writer = csv.writer(handle)
        writer.writerow(
            [
                "query_surface",
                "label_count",
                "sample_count",
                "latency_median_ns",
                "latency_ci_lower_ns",
                "latency_ci_upper_ns",
                "throughput_per_second",
            ]
        )
        writer.writerows(rows)
        temporary = Path(handle.name)
    os.replace(temporary, args.output)
    args.hashes_output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", newline="", encoding="utf-8", dir=args.hashes_output.parent, delete=False
    ) as handle:
        writer = csv.writer(handle)
        writer.writerow(["query_surface", "label_count", "raw_log_path", "sha256"])
        writer.writerows(hash_rows)
        temporary = Path(handle.name)
    os.replace(temporary, args.hashes_output)
    print(f"Summarized {len(rows)} PostgreSQL cases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
