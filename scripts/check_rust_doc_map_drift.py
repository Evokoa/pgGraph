#!/usr/bin/env python3
"""Check Rust source layout drift against contributor documentation.

Usage:
  python3 scripts/check_rust_doc_map_drift.py
  python3 scripts/check_rust_doc_map_drift.py --list

What this checks:
  1. Production top-level Rust modules in graph/src/*.rs are represented in
     docs/contributor_guide/repository-map.mdx.
  2. SQL facade files in graph/src/sql_facade/*.rs are represented in the
     repository map's SQL facade section.
  3. pgrx integration test files in graph/src/pg_tests/*.rs are mentioned in
     docs/contributor_guide/testing-release.mdx.
  4. Contributor relationship-identity descriptions match the source fields
     that preserve identity through pending and layered neighbors.

This catches stale repository maps after module splits, renamed facade files,
or new SQL test files. It does not inspect Rust symbols or behavior; pair it
with check_sql_api_drift.py for SQL-facing API drift.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
GRAPH_SRC = ROOT / "graph" / "src"
REPOSITORY_MAP = ROOT / "docs" / "contributor_guide" / "repository-map.mdx"
TESTING_RELEASE = ROOT / "docs" / "contributor_guide" / "testing-release.mdx"
ENGINE_INTERNALS = ROOT / "docs" / "contributor_guide" / "engine-internals.mdx"
TRAVERSAL_PATHS = ROOT / "docs" / "contributor_guide" / "traversal-search-paths.mdx"

# These are support/test harness files, not production modules in the source map.
SOURCE_MODULE_EXCLUSIONS = {"pg_test.rs", "unit_tests.rs"}
PG_TEST_EXCLUSIONS = {"common.rs"}


def listed_backtick_paths(doc: pathlib.Path) -> set[str]:
    return set(re.findall(r"`([^`]+\.rs)`", doc.read_text()))


def path_name(value: str) -> str:
    return pathlib.PurePosixPath(value).name


def documented_rust_path_exists(value: str) -> bool:
    if "*" in value:
        return True
    if value.startswith("src/"):
        return (GRAPH_SRC.parent / value).exists()
    return (
        (GRAPH_SRC / value).exists()
        or (GRAPH_SRC / "sql_facade" / value).exists()
        or (GRAPH_SRC / "pg_tests" / value).exists()
    )


def source_modules() -> set[str]:
    return {
        path.name
        for path in GRAPH_SRC.glob("*.rs")
        if path.name not in SOURCE_MODULE_EXCLUSIONS
    }


def facade_modules() -> set[str]:
    return {path.name for path in (GRAPH_SRC / "sql_facade").glob("*.rs")}


def pg_test_modules() -> set[str]:
    return {
        path.name
        for path in (GRAPH_SRC / "pg_tests").glob("*.rs")
        if path.name not in PG_TEST_EXCLUSIONS
    }


def report(title: str, missing: set[str], stale: set[str]) -> bool:
    if not missing and not stale:
        return False
    print(f"{title}:", file=sys.stderr)
    if missing:
        print(f"  missing from docs: {', '.join(sorted(missing))}", file=sys.stderr)
    if stale:
        print(f"  documented but not present: {', '.join(sorted(stale))}", file=sys.stderr)
    return True


def relationship_identity_drift() -> list[str]:
    engine_source = (GRAPH_SRC / "engine.rs").read_text(encoding="utf-8")
    segment_source = (GRAPH_SRC / "projection" / "segment.rs").read_text(encoding="utf-8")
    neighbor_source = (GRAPH_SRC / "projection" / "neighbors.rs").read_text(encoding="utf-8")
    engine_doc = ENGINE_INTERNALS.read_text(encoding="utf-8")
    traversal_doc = TRAVERSAL_PATHS.read_text(encoding="utf-8")

    required_source = {
        "EdgeMutation relationship identity": (
            engine_source,
            "relationship_id: Option<crate::edge_store::RelationshipId>",
        ),
        "segment relationship identity": (segment_source, "relationship_id: Option<"),
        "layered neighbor relationship identity": (
            neighbor_source,
            "relationship_id: Option<RelationshipId>",
        ),
    }
    required_docs = {
        "engine pending/layered identity": (
            engine_doc,
            "Clean CSR, pending overlay, and layered segment neighbors expose stable",
        ),
        "path pending/layered identity": (
            traversal_doc,
            "Pending overlay and layered\nsegment path steps carry the same generation dictionary identity",
        ),
    }
    failures = [name for name, (text, marker) in required_source.items() if marker not in text]
    failures.extend(name for name, (text, marker) in required_docs.items() if marker not in text)
    stale_claim = "do not yet have durable relationship IDs"
    if stale_claim in engine_doc or stale_claim in traversal_doc:
        failures.append("stale missing-layered-identity claim")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true", help="print discovered Rust files")
    args = parser.parse_args()

    sources = source_modules()
    facades = facade_modules()
    pg_tests = pg_test_modules()

    if args.list:
        print("Source modules:")
        for name in sorted(sources):
            print(f"  {name}")
        print("\nSQL facade modules:")
        for name in sorted(facades):
            print(f"  {name}")
        print("\npgrx SQL test modules:")
        for name in sorted(pg_tests):
            print(f"  {name}")
        return 0

    repo_map_paths = listed_backtick_paths(REPOSITORY_MAP)
    testing_paths = listed_backtick_paths(TESTING_RELEASE)

    repo_map_names = {path_name(value) for value in repo_map_paths}
    testing_names = {path_name(value) for value in testing_paths}

    documented_sources = {name for name in sources | SOURCE_MODULE_EXCLUSIONS if name in repo_map_names}
    documented_facades = {name for name in facades if name in repo_map_names}
    documented_pg_tests = {name for name in pg_tests if name in testing_names}

    stale_repo_rs = {
        value
        for value in repo_map_paths
        if not documented_rust_path_exists(value)
        and path_name(value) not in SOURCE_MODULE_EXCLUSIONS
    }
    # testing-release.mdx also mentions pure Rust unit-test modules. Only
    # missing pg_tests entries are checked here; stale unit-test mentions are
    # covered by the repository-map source-module check.
    stale_test_rs: set[str] = set()

    failed = False
    failed |= report(
        str(REPOSITORY_MAP.relative_to(ROOT)) + " source modules",
        sources - documented_sources,
        stale_repo_rs,
    )
    failed |= report(
        str(REPOSITORY_MAP.relative_to(ROOT)) + " SQL facade modules",
        facades - documented_facades,
        set(),
    )
    failed |= report(
        str(TESTING_RELEASE.relative_to(ROOT)) + " pgrx SQL tests",
        pg_tests - documented_pg_tests,
        stale_test_rs,
    )
    identity_failures = relationship_identity_drift()
    if identity_failures:
        print("Relationship identity documentation drift:", file=sys.stderr)
        for failure in identity_failures:
            print(f"  {failure}", file=sys.stderr)
        failed = True

    if not failed:
        print("Rust source map documentation is in sync.")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
