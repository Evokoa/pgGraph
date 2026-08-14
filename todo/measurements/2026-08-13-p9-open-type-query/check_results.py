#!/usr/bin/env python3
"""Validate and reconcile every retained P9 open-type evidence artifact."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import statistics
import subprocess
from collections import defaultdict
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", required=True, type=Path)
    parser.add_argument("--evidence-dir", required=True, type=Path)
    parser.add_argument("--budget-commit", required=True)
    return parser.parse_args()


def rows(path: Path, delimiter: str = ",") -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as handle:
        result = list(csv.DictReader(handle, delimiter=delimiter))
    if not result:
        raise ValueError(f"{path.name} has no evidence rows")
    return result


def number(value: str, field: str, *, allow_zero: bool = False) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or (parsed < 0 if allow_zero else parsed <= 0):
        raise ValueError(f"{field} must be finite and {'nonnegative' if allow_zero else 'positive'}")
    return parsed


def close(actual: float, expected: float, field: str, tolerance: float = 1e-9) -> None:
    if abs(actual - expected) > max(1.0, abs(expected)) * tolerance:
        raise ValueError(f"{field} differs: retained={actual}, recomputed={expected}")


def unique(index: list[tuple[Any, ...]], field: str) -> None:
    if len(index) != len(set(index)):
        raise ValueError(f"duplicate {field}")


def median_interval(samples: list[float]) -> tuple[float, float]:
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
    return ordered[max(0, tail_rank - 1)], ordered[min(count - 1, count - tail_rank)]


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
    transactions = [item[0] for item in parsed]
    if transactions not in (list(range(50)), list(range(1, 51))):
        raise ValueError(f"{path} transaction numbers are not contiguous")
    return [item[1] for item in parsed[10:]]


def expected_criterion_dimensions(cases: dict[str, Any]) -> set[tuple[tuple[str, str], ...]]:
    criterion = cases["criterion"]
    label_counts = [int(value) for value in criterion["registry_lookup"]["label_count"]]
    expected: list[dict[str, str]] = []
    for label_count in label_counts:
        width = 1 if label_count <= 254 else 2 if label_count <= 65_534 else 4
        encoding = f"adaptive_u{width * 8}"
        for request in criterion["registry_lookup"]["request"]:
            expected.append({"query_surface": "registry_lookup", "label_count": str(label_count), "request": request, "encoding": encoding})
        for request_shape in criterion["filter_resolution"]["request_shape"]:
            expected.append({"query_surface": "filter_resolution", "label_count": str(label_count), "request_shape": request_shape, "encoding": encoding})
        for request in criterion["filter_resolution"]["request"]:
            expected.append({"query_surface": "filter_request", "label_count": str(label_count), "request": request, "encoding": encoding})
    baseline = criterion["bfs_oat"]["baseline"]
    baseline_case = (
        int(baseline["label_count"]), int(baseline["degree"]), int(baseline["depth"]),
        baseline["csr_direction"], baseline["selectivity"],
    )
    bfs_cases = {baseline_case}
    bfs_cases.update((int(value), baseline_case[1], baseline_case[2], baseline_case[3], baseline_case[4]) for value in criterion["bfs_oat"]["label_count"])
    bfs_cases.update((baseline_case[0], int(value), baseline_case[2], baseline_case[3], baseline_case[4]) for value in criterion["bfs_oat"]["degree"])
    bfs_cases.update((baseline_case[0], baseline_case[1], int(value), baseline_case[3], baseline_case[4]) for value in criterion["bfs_oat"]["depth"])
    bfs_cases.update((baseline_case[0], baseline_case[1], baseline_case[2], value, baseline_case[4]) for value in criterion["bfs_oat"]["csr_direction"])
    bfs_cases.update((baseline_case[0], baseline_case[1], baseline_case[2], baseline_case[3], value) for value in criterion["bfs_oat"]["selectivity"])
    for label_count, degree, depth, direction, selectivity in bfs_cases:
        width = 1 if label_count <= 254 else 2 if label_count <= 65_534 else 4
        expected.append({
            "query_surface": "bfs", "label_count": str(label_count),
            "selectivity": selectivity, "degree": str(degree), "depth": str(depth),
            "csr_direction": direction,
            "csr_type_bytes_one_direction": str(degree * depth * width),
            "encoding": f"adaptive_u{width * 8}",
        })
    return {tuple(sorted(item.items())) for item in expected}


def validate_criterion(evidence: Path, budgets: dict[str, Any], cases: dict[str, Any]) -> int:
    estimates = rows(evidence / "criterion-estimates.csv")
    results = rows(evidence / "criterion-results.csv")
    hashes = rows(evidence / "criterion-raw-hashes.csv")
    expected_count = int(cases["criterion_expected_case_count"])
    if len(results) != expected_count:
        raise ValueError(f"Criterion case count is {len(results)}, expected {expected_count}")
    unique([(row["benchmark_id"],) for row in results], "Criterion result")
    unique([(row["benchmark_id"], row["statistic"]) for row in estimates], "Criterion estimate")
    if len(estimates) != expected_count or any(row["statistic"] != "median" for row in estimates):
        raise ValueError("Criterion estimates must contain exactly one median per case")
    median = {row["benchmark_id"]: row for row in estimates if row["statistic"] == "median"}
    if set(median) != {row["benchmark_id"] for row in results}:
        raise ValueError("Criterion raw median and normalized result IDs differ")
    result_by_id = {row["benchmark_id"]: row for row in results}
    dimension_fields = (
        "query_surface", "label_count", "request", "request_shape", "selectivity",
        "degree", "depth", "csr_direction", "encoding", "csr_type_bytes_one_direction",
    )
    actual_dimensions = {
        tuple(sorted((field, row[field]) for field in dimension_fields if row[field]))
        for row in results
    }
    expected_dimensions = expected_criterion_dimensions(cases)
    if actual_dimensions != expected_dimensions:
        raise ValueError("normalized Criterion cases differ from the exact cases.json matrix")
    for benchmark_id, estimate in median.items():
        result = result_by_id[benchmark_id]
        lower = number(estimate["lower_bound_ns"], "Criterion lower bound")
        point = number(estimate["point_estimate_ns"], "Criterion median")
        upper = number(estimate["upper_bound_ns"], "Criterion upper bound")
        if not lower <= point <= upper:
            raise ValueError(f"invalid Criterion interval for {benchmark_id}")
        close(number(result["latency_median_ns"], "normalized median"), point, "Criterion median")
        close(number(result["latency_ci_lower_ns"], "normalized lower"), lower, "Criterion lower")
        close(number(result["latency_ci_upper_ns"], "normalized upper"), upper, "Criterion upper")
        expected_throughput = 1_000_000_000.0 / point
        close(
            number(result["throughput_per_second"], "Criterion throughput"),
            expected_throughput,
            "Criterion throughput",
        )
        relative_width = (upper - lower) / point
        if relative_width > float(budgets["max_relative_confidence_interval_width"]):
            raise ValueError(f"Criterion confidence interval too wide for {benchmark_id}")

    hash_by_id = {row["benchmark_id"]: row["estimates_sha256"] for row in hashes}
    if set(hash_by_id) != set(median) or len(hash_by_id) != len(hashes):
        raise ValueError("Criterion raw hash inventory differs from results")
    raw_root = evidence / "raw" / "criterion"
    retained_raw_ids = {
        "/".join(path.relative_to(raw_root).parts[:-2])
        for path in raw_root.glob("open_type_*/**/new/estimates.json")
        if path.is_file()
    }
    if retained_raw_ids != set(hash_by_id):
        raise ValueError("Criterion raw filesystem inventory differs from the hash inventory")
    for benchmark_id, expected_hash in hash_by_id.items():
        components = benchmark_id.split("/")
        if any(component in ("", ".", "..") for component in components):
            raise ValueError(f"unsafe Criterion benchmark ID: {benchmark_id}")
        raw = (raw_root.joinpath(*components) / "new" / "estimates.json").resolve()
        try:
            raw.relative_to(raw_root.resolve())
        except ValueError as error:
            raise ValueError("Criterion raw estimate escapes the retained raw directory") from error
        actual_hash = hashlib.sha256(raw.read_bytes()).hexdigest()
        if actual_hash != expected_hash:
            raise ValueError(f"Criterion raw hash mismatch for {benchmark_id}")
        raw_median = json.loads(raw.read_text(encoding="utf-8"))["median"]
        raw_confidence = raw_median["confidence_interval"]
        estimate = median[benchmark_id]
        close(
            number(estimate["point_estimate_ns"], "Criterion extracted median"),
            number(str(raw_median["point_estimate"]), "Criterion raw median"),
            "Criterion raw median",
        )
        close(
            number(estimate["lower_bound_ns"], "Criterion extracted lower"),
            number(str(raw_confidence["lower_bound"]), "Criterion raw lower"),
            "Criterion raw lower",
        )
        close(
            number(estimate["upper_bound_ns"], "Criterion extracted upper"),
            number(str(raw_confidence["upper_bound"]), "Criterion raw upper"),
            "Criterion raw upper",
        )
        close(
            number(estimate["confidence_level"], "Criterion extracted confidence"),
            number(str(raw_confidence["confidence_level"]), "Criterion raw confidence"),
            "Criterion raw confidence",
        )

    def select(selector: dict[str, Any]) -> dict[str, str]:
        matches = [
            row
            for row in results
            if all(str(row.get(key, "")) == str(value) for key, value in selector.items())
        ]
        if len(matches) != 1:
            raise ValueError(f"Criterion selector resolved {len(matches)} rows: {selector}")
        return matches[0]

    for limit in budgets["criterion_ratio_limits"]:
        numerator = select(limit["numerator"])
        denominator = select(limit["denominator"])
        latency_ratio = number(numerator["latency_median_ns"], "numerator latency") / number(
            denominator["latency_median_ns"], "denominator latency"
        )
        throughput_ratio = number(numerator["throughput_per_second"], "numerator throughput") / number(
            denominator["throughput_per_second"], "denominator throughput"
        )
        if latency_ratio > float(limit["max_median_latency_ratio"]):
            raise ValueError(f"Criterion latency budget failed: {limit['id']}")
        if throughput_ratio < float(limit["min_throughput_ratio"]):
            raise ValueError(f"Criterion throughput budget failed: {limit['id']}")
    return len(results)


def validate_postgres(evidence: Path, budgets: dict[str, Any]) -> int:
    samples = rows(evidence / "postgres-samples.csv")
    summaries = rows(evidence / "postgres-results.csv")
    oracles = rows(evidence / "postgres-oracles.csv")
    log_hashes = rows(evidence / "postgres-log-hashes.csv")
    unique(
        [(row["query_surface"], row["label_count"], row["sample_index"]) for row in samples],
        "PostgreSQL sample",
    )
    grouped: dict[tuple[str, int], list[dict[str, str]]] = defaultdict(list)
    for row in samples:
        grouped[(row["query_surface"], int(row["label_count"]))].append(row)
    expected = {(surface, labels) for surface in ("traverse", "shortest_path", "gql", "cypher") for labels in (254, 65_536)}
    if set(grouped) != expected or len(summaries) != 8:
        raise ValueError("PostgreSQL low/high surface matrix is incomplete")
    summary_by_case = {(row["query_surface"], int(row["label_count"])): row for row in summaries}
    if len(summary_by_case) != 8:
        raise ValueError("duplicate PostgreSQL summary")
    oracle_by_case = {(row["query_surface"], int(row["label_count"])): row for row in oracles}
    if set(oracle_by_case) != expected or len(oracle_by_case) != len(oracles):
        raise ValueError("PostgreSQL oracle matrix is incomplete")
    hash_by_case = {
        (row["query_surface"], int(row["label_count"])): row for row in log_hashes
    }
    if set(hash_by_case) != expected or len(hash_by_case) != len(log_hashes):
        raise ValueError("PostgreSQL raw-log hash inventory is incomplete or duplicate")
    raw_postgres_root = (evidence / "raw" / "postgres").resolve()
    retained_numeric_logs = {
        path.resolve()
        for path in raw_postgres_root.iterdir()
        if path.is_file() and path.name.rsplit(".", 1)[-1].isdigit()
    }
    inventoried_logs = {
        (evidence / row["raw_log_path"]).resolve() for row in log_hashes
    }
    if retained_numeric_logs != inventoried_logs:
        raise ValueError("PostgreSQL raw-log filesystem inventory differs from its hash inventory")
    for key, group in grouped.items():
        if len(group) != 40:
            raise ValueError(f"PostgreSQL case {key} has {len(group)} samples")
        if {int(row["sample_index"]) for row in group} != set(range(1, 41)):
            raise ValueError(f"PostgreSQL case {key} has incomplete sample indices")
        if len({row["result_digest"] for row in group}) != 1:
            raise ValueError(f"PostgreSQL result digest changed for {key}")
        digest = next(iter({row["result_digest"] for row in group}))
        if digest != oracle_by_case[key]["result_digest"]:
            raise ValueError(f"PostgreSQL sample digest differs from the oracle for {key}")
        elapsed = [number(row["elapsed_ns"], "PostgreSQL elapsed_ns") for row in group]
        raw_root = (evidence / "raw" / "postgres").resolve()
        raw_log = (evidence / hash_by_case[key]["raw_log_path"]).resolve()
        try:
            raw_log.relative_to(raw_root)
        except ValueError as error:
            raise ValueError("PostgreSQL raw log escapes the retained raw directory") from error
        if hashlib.sha256(raw_log.read_bytes()).hexdigest() != hash_by_case[key]["sha256"]:
            raise ValueError(f"PostgreSQL raw-log hash differs for {key}")
        raw_elapsed = pgbench_latencies(raw_log)
        if [round(value) for value in elapsed] != raw_elapsed:
            raise ValueError(f"PostgreSQL samples differ from the raw pgbench log for {key}")
        summary = summary_by_case[key]
        if int(summary["sample_count"]) != len(group):
            raise ValueError(f"PostgreSQL summary count differs for {key}")
        close(number(summary["latency_median_ns"], "PostgreSQL median"), statistics.median(elapsed), "PostgreSQL median")
        lower, upper = median_interval(elapsed)
        close(number(summary["latency_ci_lower_ns"], "PostgreSQL lower"), lower, "PostgreSQL lower")
        close(number(summary["latency_ci_upper_ns"], "PostgreSQL upper"), upper, "PostgreSQL upper")
        throughput = 1_000_000_000.0 * len(elapsed) / sum(elapsed)
        close(number(summary["throughput_per_second"], "PostgreSQL throughput"), throughput, "PostgreSQL throughput", 1e-6)
    for surface in ("traverse", "shortest_path", "gql", "cypher"):
        low = oracle_by_case[(surface, 254)]
        high = oracle_by_case[(surface, 65_536)]
        if (low["row_count"], low["result_digest"]) != (high["row_count"], high["result_digest"]):
            raise ValueError(f"PostgreSQL low/high oracle differs for {surface}")
    for limit in budgets["postgres_ratio_limits"]:
        surface = limit["query_surface"]
        numerator = summary_by_case[(surface, int(limit["numerator"]["label_count"]))]
        denominator = summary_by_case[(surface, int(limit["denominator"]["label_count"]))]
        ratio = number(numerator["latency_median_ns"], "PostgreSQL numerator") / number(
            denominator["latency_median_ns"], "PostgreSQL denominator"
        )
        if ratio > float(limit["max_median_latency_ratio"]):
            raise ValueError(f"PostgreSQL latency budget failed for {surface}")
    return len(summaries)


def validate_resources(
    evidence: Path,
    budgets: dict[str, Any],
    protocol: dict[str, Any],
    measurement_commit: str,
) -> int:
    samples = rows(evidence / "resource-samples.tsv", "\t")
    summaries = rows(evidence / "resource-results.csv")
    if len(summaries) != 3:
        raise ValueError("resource evidence needs 1, 4, and 8 backend summaries")
    summary_by_count = {int(row["backend_count"]): row for row in summaries}
    if set(summary_by_count) != {1, 4, 8}:
        raise ValueError("resource backend-count matrix is incomplete or duplicate")
    grouped: dict[str, list[dict[str, str]]] = defaultdict(list)
    unique(
        [
            (row["run_id"], row["phase"], row["sample_id"], row["backend"])
            for row in samples
        ],
        "resource sample",
    )
    for row in samples:
        number(row["rss_bytes"], "resource RSS")
        number(row["pss_bytes"], "resource PSS")
        number(row["epoch_ms"], "resource epoch")
        grouped[row["run_id"]].append(row)
    expected_run_ids = {
        f"{measurement_commit}-b{backend_count}" for backend_count in (1, 4, 8)
    }
    if set(grouped) != expected_run_ids:
        raise ValueError("resource raw run IDs differ from the measurement matrix")
    limits = budgets["high_cardinality_resource_limits"]
    edge_count = int(protocol["resources"]["directed_edge_count"])
    for backend_count, summary in summary_by_count.items():
        expected_run_id = f"{measurement_commit}-b{backend_count}"
        if summary["run_id"] != expected_run_id:
            raise ValueError("resource run ID differs from the measurement commit")
        run = grouped.get(summary["run_id"], [])
        if not run:
            raise ValueError(f"resource run {summary['run_id']} has no raw rows")
        if {int(row["backend"]) for row in run} != set(range(1, backend_count + 1)):
            raise ValueError("resource backend identities are incomplete")
        if any(int(row["label_count"]) != int(protocol["resources"]["label_count"]) for row in run):
            raise ValueError("raw resource label count differs from the protocol")
        if any(int(row["backend_count"]) != backend_count for row in run):
            raise ValueError("raw resource backend count differs from its summary")
        if len({int(row["pid"]) for row in run}) != backend_count:
            raise ValueError("resource run does not retain distinct real backend PIDs")
        if {row["phase"] for row in run} != {"idle", "loaded", "query"}:
            raise ValueError("resource phases are incomplete")
        if int(summary["label_count"]) != int(protocol["resources"]["label_count"]):
            raise ValueError("resource label count differs from the protocol")
        expected_summary = {
            "query_surface": "traverse",
            "filter_shape": "no_filter",
            "degree": str(protocol["resources"]["degree"]),
            "depth": str(protocol["resources"]["depth"]),
        }
        for field, expected_value in expected_summary.items():
            if summary[field] != expected_value:
                raise ValueError(f"resource {field} differs from the protocol")
        max_rss = max(int(row["rss_bytes"]) for row in run)
        max_pss = max(int(row["pss_bytes"]) for row in run)
        close(float(summary["max_per_backend_rss_bytes"]), max_rss, "resource max RSS")
        close(float(summary["max_per_backend_pss_bytes"]), max_pss, "resource max PSS")
        phase_totals: dict[tuple[str, int], tuple[int, int]] = {}
        accumulator: dict[tuple[str, int], list[int]] = defaultdict(lambda: [0, 0])
        for row in run:
            key = (row["phase"], int(row["sample_id"]))
            accumulator[key][0] += int(row["rss_bytes"])
            accumulator[key][1] += int(row["pss_bytes"])
        for phase_sample in accumulator:
            phase, sample_id = phase_sample
            identities = {
                int(row["backend"])
                for row in run
                if row["phase"] == phase and int(row["sample_id"]) == sample_id
            }
            if identities != set(range(1, backend_count + 1)):
                raise ValueError("resource sample does not contain every backend")
        phase_sample_counts = {
            phase: len({int(row["sample_id"]) for row in run if row["phase"] == phase})
            for phase in ("idle", "loaded", "query")
        }
        if phase_sample_counts["idle"] != 10 or phase_sample_counts["loaded"] != 20:
            raise ValueError("resource idle/loaded sample counts differ from the protocol")
        if phase_sample_counts["query"] < 1:
            raise ValueError("resource evidence has no active-query sample")
        phase_totals = {key: (value[0], value[1]) for key, value in accumulator.items()}
        for phase, rss_field, pss_field in (
            ("idle", "max_idle_total_rss_bytes", "max_idle_total_pss_bytes"),
            ("loaded", "max_loaded_total_rss_bytes", "max_loaded_total_pss_bytes"),
            ("query", "max_query_total_rss_bytes", "max_query_total_pss_bytes"),
        ):
            phase_values = [value for (candidate, _), value in phase_totals.items() if candidate == phase]
            expected_rss = max(value[0] for value in phase_values)
            expected_pss = max(value[1] for value in phase_values)
            close(float(summary[rss_field]), expected_rss, rss_field)
            close(float(summary[pss_field]), expected_pss, pss_field)
        expected_delta_rss = max(0, int(summary["max_query_total_rss_bytes"]) - int(summary["max_idle_total_rss_bytes"]))
        expected_delta_pss = max(0, int(summary["max_query_total_pss_bytes"]) - int(summary["max_idle_total_pss_bytes"]))
        close(float(summary["query_minus_idle_total_rss_bytes"]), expected_delta_rss, "resource RSS delta")
        close(float(summary["query_minus_idle_total_pss_bytes"]), expected_delta_pss, "resource PSS delta")
        if max_rss > int(limits["max_rss_bytes_per_backend"]) or max_pss > int(limits["max_pss_bytes_per_backend"]):
            raise ValueError("per-backend resource budget failed")
        artifact_per_edge = int(summary["projection_artifact_bytes"]) / edge_count
        if artifact_per_edge > float(limits["max_projection_artifact_bytes_per_directed_edge"]):
            raise ValueError("projection artifact budget failed")
    one = summary_by_count[1]
    eight = summary_by_count[8]
    total_ratio = int(eight["max_query_total_pss_bytes"]) / int(one["max_query_total_pss_bytes"])
    if total_ratio > float(limits["max_eight_to_one_total_query_pss_ratio"]):
        raise ValueError("aggregate query PSS scaling budget failed")
    one_delta = int(one["query_minus_idle_total_pss_bytes"])
    if one_delta <= 0:
        raise ValueError("one-backend baseline-subtracted PSS must be positive")
    delta_ratio = int(eight["query_minus_idle_total_pss_bytes"]) / one_delta
    if delta_ratio > float(limits["max_eight_to_one_query_minus_idle_total_pss_ratio"]):
        raise ValueError("baseline-subtracted PSS scaling budget failed")
    return len(summaries)


def main() -> int:
    args = parse_args()
    evidence = args.evidence_dir.resolve()
    budgets = json.loads((evidence / "budgets.json").read_text(encoding="utf-8"))
    cases = json.loads((evidence / "cases.json").read_text(encoding="utf-8"))
    protocol = json.loads((evidence / "measurement-protocol.json").read_text(encoding="utf-8"))
    metadata = json.loads((evidence / "run-metadata.json").read_text(encoding="utf-8"))
    if metadata["budget_commit"] != args.budget_commit or protocol["budget_commit"] != args.budget_commit:
        raise ValueError("budget commit metadata differs")
    for relative in ("budgets.json", "cases.json"):
        frozen = subprocess.run(
            [
                "git", "-C", str(args.repo_root), "show",
                f"{args.budget_commit}:todo/measurements/2026-08-13-p9-open-type-query/{relative}",
            ],
            check=True,
            capture_output=True,
        ).stdout
        if (evidence / relative).read_bytes() != frozen:
            raise ValueError(f"{relative} differs from the premeasurement budget commit")
    measurement_commit = metadata["measurement_commit"]
    if metadata.get("git_status_clean") is not True:
        raise ValueError("measurement source tree was not clean before evidence capture")
    if metadata.get("resources", {}).get("os") != "Linux":
        raise ValueError("resource evidence must be captured on Linux")
    for path in (
        ("criterion", "command"), ("criterion", "rustc"),
        ("postgres", "command"), ("postgres", "version"),
        ("resources", "command"), ("resources", "kernel"),
    ):
        if not str(metadata.get(path[0], {}).get(path[1], "")).strip():
            raise ValueError(f"run metadata is missing {'/'.join(path)}")
    provenance = {
        ("postgres", "version"): (evidence / "latency-postgres-version.txt").read_text(encoding="utf-8").strip(),
        ("postgres", "pgbench"): (evidence / "latency-pgbench-version.txt").read_text(encoding="utf-8").strip(),
        ("resources", "kernel"): (evidence / "linux-uname.txt").read_text(encoding="utf-8").strip(),
        ("resources", "postgres"): (evidence / "resource-postgres-version.txt").read_text(encoding="utf-8").strip(),
    }
    for (section, field), retained_value in provenance.items():
        if not retained_value or metadata[section].get(field) != retained_value:
            raise ValueError(f"{section}/{field} differs from retained provenance")
    latency_settings = json.loads(
        (evidence / "latency-settings.json").read_text(encoding="utf-8")
    )
    expected_latency_settings = {
        "memory_limit_mb": int(protocol["postgres"]["memory_limit_mb"]),
        "query_memory_mb": int(protocol["postgres"]["query_memory_mb"]),
    }
    if latency_settings != expected_latency_settings:
        raise ValueError("effective PostgreSQL latency settings differ from the protocol")
    if metadata["postgres"].get("settings") != latency_settings:
        raise ValueError("PostgreSQL latency settings differ from run metadata")
    retained_docker_version = json.loads(
        (evidence / "docker-version.json").read_text(encoding="utf-8")
    )
    if metadata["resources"].get("docker") != retained_docker_version:
        raise ValueError("Docker version differs from run metadata")
    criterion_log = (evidence / "criterion-run.log").read_bytes()
    if not criterion_log:
        raise ValueError("Criterion run log is empty")
    if hashlib.sha256(criterion_log).hexdigest() != metadata["criterion"].get("run_log_sha256"):
        raise ValueError("Criterion run log hash differs from run metadata")
    image_inspect = json.loads(
        (evidence / "docker-image-inspect.json").read_text(encoding="utf-8")
    )
    if not isinstance(image_inspect, list) or len(image_inspect) != 1:
        raise ValueError("Docker image inspection must contain exactly one image")
    if metadata["resources"].get("image_id") != image_inspect[0].get("Id"):
        raise ValueError("Docker image identity differs from run metadata")
    for filename, metadata_field in (
        ("docker-build.log", "docker_build_log_sha256"),
        ("docker-resource.log", "docker_resource_log_sha256"),
    ):
        payload = (evidence / filename).read_bytes()
        if not payload:
            raise ValueError(f"{filename} is empty")
        if hashlib.sha256(payload).hexdigest() != metadata["resources"].get(metadata_field):
            raise ValueError(f"{filename} hash differs from run metadata")
    ancestry = subprocess.run(
        ["git", "-C", str(args.repo_root), "merge-base", "--is-ancestor", args.budget_commit, measurement_commit],
        check=False,
    )
    if ancestry.returncode != 0 or measurement_commit == args.budget_commit:
        raise ValueError("budget commit is not a strict ancestor of measurement commit")
    tooling_paths = [
        "graph/benches/open_type_query_bench.rs",
        "graph/tests/heavy/open_type_query_latency.sh",
        "graph/tests/heavy/run_open_type_query_criterion.sh",
        "graph/tests/heavy/open_type_query_resources.sh",
        "graph/tests/heavy/run_open_type_query_resource_matrix.sh",
        "graph/tests/heavy/run_open_type_query_resources_docker.sh",
        "scripts/extract_p9_open_type_criterion.py",
        "scripts/summarize_p9_open_type_postgres.py",
        "scripts/test_p9_open_type_evidence_tools.py",
        "scripts/write_p9_open_type_run_metadata.py",
        "todo/measurements/2026-08-13-p9-open-type-query/budgets.json",
        "todo/measurements/2026-08-13-p9-open-type-query/cases.json",
        "todo/measurements/2026-08-13-p9-open-type-query/check_results.py",
        "todo/measurements/2026-08-13-p9-open-type-query/measurement-protocol.json",
    ]
    tooling_diff = subprocess.run(
        ["git", "-C", str(args.repo_root), "diff", "--quiet", measurement_commit, "--", *tooling_paths],
        check=False,
    )
    if tooling_diff.returncode != 0:
        raise ValueError("measurement tooling differs from the recorded measurement commit")
    criterion_count = validate_criterion(evidence, budgets, cases)
    postgres_count = validate_postgres(evidence, budgets)
    resource_count = validate_resources(evidence, budgets, protocol, measurement_commit)
    report = {
        "ancestry_verified": True,
        "raw_summary_reconciled": True,
        "budgets_passed": True,
        "counts": {
            "criterion_cases": criterion_count,
            "postgres_cases": postgres_count,
            "resource_runs": resource_count,
        },
    }
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
