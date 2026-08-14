#!/usr/bin/env python3
"""Extract and normalize the retained P9 open-type Criterion evidence."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import shutil
import tempfile
from pathlib import Path


ESTIMATE_HEADER = [
    "benchmark_id",
    "statistic",
    "confidence_level",
    "lower_bound_ns",
    "point_estimate_ns",
    "upper_bound_ns",
]
RESULT_HEADER = [
    "benchmark_id",
    "query_surface",
    "label_count",
    "request",
    "request_shape",
    "selectivity",
    "degree",
    "depth",
    "csr_direction",
    "encoding",
    "csr_type_bytes_one_direction",
    "latency_median_ns",
    "latency_ci_lower_ns",
    "latency_ci_upper_ns",
    "throughput_per_second",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--criterion-root", required=True, type=Path)
    parser.add_argument("--evidence-dir", required=True, type=Path)
    return parser.parse_args()


def finite_positive(value: object, field: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        raise ValueError(f"{field} must be finite and positive, got {value!r}")
    return parsed


def atomic_csv(path: Path, header: list[str], rows: list[list[object]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", newline="", encoding="utf-8", dir=path.parent, delete=False
    ) as handle:
        writer = csv.writer(handle)
        writer.writerow(header)
        writer.writerows(rows)
        temporary = Path(handle.name)
    os.replace(temporary, path)


def dimensions(benchmark_id: str) -> dict[str, str]:
    parsed: dict[str, str] = {}
    for component in benchmark_id.split("/"):
        if "=" in component:
            key, value = component.split("=", 1)
            if key in parsed:
                raise ValueError(f"duplicate dimension {key!r} in {benchmark_id}")
            parsed[key] = value
    if "query_surface" not in parsed:
        raise ValueError(f"benchmark lacks query_surface: {benchmark_id}")
    return parsed


def benchmark_identity(estimates_path: Path, criterion_root: Path) -> tuple[str, bytes]:
    metadata_path = estimates_path.with_name("benchmark.json")
    metadata_bytes = metadata_path.read_bytes()
    metadata = json.loads(metadata_bytes)
    benchmark_id = metadata.get("full_id")
    directory_name = metadata.get("directory_name")
    relative_directory = "/".join(estimates_path.relative_to(criterion_root).parts[:-2])
    if not isinstance(benchmark_id, str) or not benchmark_id:
        raise ValueError(f"Criterion benchmark metadata lacks full_id: {metadata_path}")
    if directory_name != relative_directory:
        raise ValueError(f"Criterion directory metadata differs for {metadata_path}")
    components = benchmark_id.split("/")
    if any(component in ("", ".", "..") for component in components):
        raise ValueError(f"unsafe Criterion benchmark ID: {benchmark_id}")
    return benchmark_id, metadata_bytes


def expected_dimensions(cases: dict[str, object]) -> set[tuple[tuple[str, str], ...]]:
    criterion = cases["criterion"]
    label_counts = [int(value) for value in criterion["registry_lookup"]["label_count"]]
    expected: list[dict[str, str]] = []
    for label_count in label_counts:
        encoding = (
            "adaptive_u8"
            if label_count <= 254
            else "adaptive_u16"
            if label_count <= 65_534
            else "adaptive_u32"
        )
        for request in criterion["registry_lookup"]["request"]:
            expected.append(
                {
                    "query_surface": "registry_lookup",
                    "label_count": str(label_count),
                    "request": request,
                    "encoding": encoding,
                }
            )
        for request_shape in criterion["filter_resolution"]["request_shape"]:
            expected.append(
                {
                    "query_surface": "filter_resolution",
                    "label_count": str(label_count),
                    "request_shape": request_shape,
                    "encoding": encoding,
                }
            )
        for request in criterion["filter_resolution"]["request"]:
            expected.append(
                {
                    "query_surface": "filter_request",
                    "label_count": str(label_count),
                    "request": request,
                    "encoding": encoding,
                }
            )

    baseline = criterion["bfs_oat"]["baseline"]
    bfs_cases = {
        (
            int(baseline["label_count"]),
            int(baseline["degree"]),
            int(baseline["depth"]),
            baseline["csr_direction"],
            baseline["selectivity"],
        )
    }
    bfs_cases.update(
        (
            int(label_count),
            int(baseline["degree"]),
            int(baseline["depth"]),
            baseline["csr_direction"],
            baseline["selectivity"],
        )
        for label_count in criterion["bfs_oat"]["label_count"]
    )
    bfs_cases.update(
        (
            int(baseline["label_count"]),
            int(degree),
            int(baseline["depth"]),
            baseline["csr_direction"],
            baseline["selectivity"],
        )
        for degree in criterion["bfs_oat"]["degree"]
    )
    bfs_cases.update(
        (
            int(baseline["label_count"]),
            int(baseline["degree"]),
            int(depth),
            baseline["csr_direction"],
            baseline["selectivity"],
        )
        for depth in criterion["bfs_oat"]["depth"]
    )
    bfs_cases.update(
        (
            int(baseline["label_count"]),
            int(baseline["degree"]),
            int(baseline["depth"]),
            direction,
            baseline["selectivity"],
        )
        for direction in criterion["bfs_oat"]["csr_direction"]
    )
    bfs_cases.update(
        (
            int(baseline["label_count"]),
            int(baseline["degree"]),
            int(baseline["depth"]),
            baseline["csr_direction"],
            selectivity,
        )
        for selectivity in criterion["bfs_oat"]["selectivity"]
    )
    for label_count, degree, depth, direction, selectivity in bfs_cases:
        width = 1 if label_count <= 254 else 2 if label_count <= 65_534 else 4
        expected.append(
            {
                "query_surface": "bfs",
                "label_count": str(label_count),
                "selectivity": selectivity,
                "degree": str(degree),
                "depth": str(depth),
                "csr_direction": direction,
                "csr_type_bytes_one_direction": str(degree * depth * width),
                "encoding": f"adaptive_u{width * 8}",
            }
        )
    return {tuple(sorted(item.items())) for item in expected}


def expected_shape(rows: list[dict[str, str]], cases: dict[str, object]) -> None:
    expected_count = int(cases["criterion_expected_case_count"])
    if len(rows) != expected_count:
        raise ValueError(f"expected {expected_count} Criterion cases, found {len(rows)}")
    identities = {
        tuple(sorted((key, value) for key, value in row.items() if key != "benchmark_id"))
        for row in rows
    }
    expected = expected_dimensions(cases)
    if identities != expected:
        missing = sorted(expected - identities)
        extra = sorted(identities - expected)
        raise ValueError(
            f"Criterion cases differ from cases.json; missing={missing}, extra={extra}"
        )


def main() -> int:
    args = parse_args()
    cases = json.loads((args.evidence_dir / "cases.json").read_text(encoding="utf-8"))
    estimate_files = sorted(
        path
        for path in args.criterion_root.glob("open_type_*/**/new/estimates.json")
        if path.is_file()
    )
    if not estimate_files:
        raise ValueError("no open-type Criterion estimates were found")

    raw_root = args.evidence_dir / "raw" / "criterion"
    if raw_root.exists():
        shutil.rmtree(raw_root)
    estimate_rows: list[list[object]] = []
    result_rows: list[list[object]] = []
    normalized: list[dict[str, str]] = []
    hashes: list[list[object]] = []
    benchmark_ids: set[str] = set()

    for estimates_path in estimate_files:
        benchmark_id, benchmark_metadata = benchmark_identity(
            estimates_path, args.criterion_root
        )
        if benchmark_id in benchmark_ids:
            raise ValueError(f"duplicate benchmark ID: {benchmark_id}")
        benchmark_ids.add(benchmark_id)
        raw_bytes = estimates_path.read_bytes()
        raw_copy = raw_root.joinpath(*benchmark_id.split("/")) / "new" / "estimates.json"
        raw_copy.parent.mkdir(parents=True, exist_ok=True)
        raw_copy.write_bytes(raw_bytes)
        raw_copy.with_name("benchmark.json").write_bytes(benchmark_metadata)
        hashes.append([benchmark_id, hashlib.sha256(raw_bytes).hexdigest()])

        payload = json.loads(raw_bytes)
        median = payload["median"]
        confidence = median["confidence_interval"]
        lower = finite_positive(confidence["lower_bound"], "median lower bound")
        point = finite_positive(median["point_estimate"], "median point estimate")
        upper = finite_positive(confidence["upper_bound"], "median upper bound")
        confidence_level = float(confidence["confidence_level"])
        if not (0 < confidence_level <= 1 and lower <= point <= upper):
            raise ValueError(f"invalid median confidence interval for {benchmark_id}")
        estimate_rows.append(
            [benchmark_id, "median", confidence_level, lower, point, upper]
        )

        parsed = dimensions(benchmark_id)
        parsed["benchmark_id"] = benchmark_id
        normalized.append(parsed)
        surface = parsed["query_surface"]
        throughput = 1_000_000_000.0 / point
        result_rows.append(
            [
                benchmark_id,
                surface,
                parsed.get("label_count", ""),
                parsed.get("request", ""),
                parsed.get("request_shape", ""),
                parsed.get("selectivity", ""),
                parsed.get("degree", ""),
                parsed.get("depth", ""),
                parsed.get("csr_direction", ""),
                parsed.get("encoding", ""),
                parsed.get("csr_type_bytes_one_direction", ""),
                point,
                lower,
                upper,
                throughput,
            ]
        )

    expected_shape(normalized, cases)
    estimate_rows.sort(key=lambda row: (str(row[0]), str(row[1])))
    result_rows.sort(key=lambda row: str(row[0]))
    hashes.sort(key=lambda row: str(row[0]))
    atomic_csv(args.evidence_dir / "criterion-estimates.csv", ESTIMATE_HEADER, estimate_rows)
    atomic_csv(args.evidence_dir / "criterion-results.csv", RESULT_HEADER, result_rows)
    atomic_csv(
        args.evidence_dir / "criterion-raw-hashes.csv",
        ["benchmark_id", "estimates_sha256"],
        hashes,
    )
    print(f"Extracted {len(result_rows)} P9 Criterion cases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
