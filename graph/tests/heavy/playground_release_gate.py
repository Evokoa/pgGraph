#!/usr/bin/env python3
"""Validate the Streamlit playground query catalog against fixed expectations."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
PLAYGROUND_DIR = ROOT / "sandbox" / "playground"
sys.path.insert(0, str(PLAYGROUND_DIR))

from queries import QUERY_QUESTIONS, query_catalog  # noqa: E402


EXPECTED_RESULTS: dict[str, list[dict[str, object]]] = {
    "Status + Catalog": [
        {"hash": "d5019272452d6a9a66be8ad818f81796", "row_count": 1},
        {"hash": "181a173a49cdbb4b9100e7b8ec693411", "row_count": 1},
        {"hash": "1f56018065f8f7688f33d312b976afd9", "row_count": 1},
    ],
    "Search Mossack": [{"hash": "0c8c22baa3ad02b3ae87b782f5b9e107", "row_count": 20}],
    "Find Mossack": [{"hash": "e5f8e6df0fad12cffba8ddd3df1837b5", "row_count": 20}],
    "Traverse Neighborhood": [{"hash": "8838da4bf5f4f4741822d8646deafe68", "row_count": 100}],
    "Expand Neighborhood": [{"hash": "625782ba027f4a5ace8308363816a3ac", "row_count": 100}],
    "Shortest Path": [{"hash": "0273f33efa04c4ba2bf45e57e703e58d", "row_count": 2}],
    "GQL One-Hop Relationships": [{"hash": "cc43fef5258a696fee573d7ce63d3161", "row_count": 1}],
    "GQL Aggregated Neighbors": [{"hash": "e17964650e3a49bd449fcb1569ac5c31", "row_count": 1}],
    "GQL Explain": [{"hash": "0c52392c32dab707c82a82edb79d1a1c", "row_count": 1}],
    "Component Stats": [
        {"hash": "4d337f672574a60eaa19f44639f30553", "row_count": 1},
        {"hash": "3addbef5dca49ab0f0592d0e69be5b17", "row_count": 20},
    ],
    "Largest Component": [{"hash": "51fe22ad3830b37346808872542f0446", "row_count": 20}],
    "Table Sizes": [{"hash": "a646f0b7bb693783d7dfa9614df6efe5", "row_count": 6}],
    "Relationship Label Counts": [{"hash": "8b637c16226b3140b99263b6501a5021", "row_count": 14}],
    "Top Connected Officers": [{"hash": "5583732c22f50c857e8bc32c8c52586f", "row_count": 25}],
    "Top Connected Entities": [{"hash": "ac1b4a4f20e4b1733f86a54544ef1375", "row_count": 25}],
    "Entity Direct Relationships": [{"hash": "4338b8f8b7c6815b3a2e5ff86306dbd2", "row_count": 4}],
    "Officer Context Packet": [{"hash": "8f10047bbfc2e2140767a6a913685a0a", "row_count": 1}],
    "Search Entity Then Expand": [{"hash": "95023cdc9a5d98a05c658ea9b2da522b", "row_count": 450}],
    "Relationship Filtered Walk": [{"hash": "26233811e6c0e86c99864f42f82ff12f", "row_count": 31}],
    "Capped 3-Hop Investigation": [{"hash": "aec58be67c8c16f21497f76ebbb05944", "row_count": 300}],
    "Build Graph": [{"row_count": 1}],
    "Build Graph Concurrently": [{"row_count": 1}],
    "Build Status": [{"row_count": 1}],
    "Sync Health": [{"row_count": 1}],
    "Apply Sync": [{"row_count": 1}],
    "Scheduled Maintenance": [{"row_count": 1}],
    "Vacuum Graph": [{"row_count": 1}],
    "Maintenance": [{"row_count": 1}],
    "Maintenance Status": [{"row_count": 0}],
}


VOLATILE_HASH_LABELS = {
    "Build Graph",
    "Build Graph Concurrently",
    "Build Status",
    "Sync Health",
    "Apply Sync",
    "Scheduled Maintenance",
    "Vacuum Graph",
    "Maintenance",
    "Maintenance Status",
}

SAME_SESSION_SETUP_LABELS = {
    "Apply Sync",
    "Scheduled Maintenance",
    "Vacuum Graph",
    "Maintenance",
}


SETUP_SQL = """
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.test_enabled();
SELECT graph.reset();
TRUNCATE graph._registered_filter_columns,
         graph._registered_edges,
         graph._registered_tables,
         graph._build_jobs,
         graph._maintenance_jobs,
         graph._sync_log,
         graph._sync_buffer
RESTART IDENTITY;
SELECT graph.add_table(
  'panama.nodes'::regclass,
  'node_id',
  ARRAY['name', 'countries', 'country_codes', 'label']
);
SELECT graph.add_edge(
  from_table := 'panama.edges'::regclass,
  from_column := 'start_id',
  to_table := 'panama.nodes'::regclass,
  to_column := 'end_id',
  label := 'related_to',
  bidirectional := true,
  label_column := 'rel_type'
);
SET graph.persist_on_build = on;
SELECT * FROM graph.build();
"""


def default_dsn() -> str:
    if dsn := os.environ.get("PGGRAPH_DSN") or os.environ.get("PGGRAPH_PLAYGROUND_DSN"):
        return dsn
    port = os.environ.get("PGGRAPH_PG_PORT", "55432")
    return f"postgresql://postgres:postgres@localhost:{port}/postgres"


def run_psql(dsn: str, sql: str, timeout: int) -> str:
    proc = subprocess.run(
        ["psql", "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tA", dsn],
        input=sql,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip())
    return proc.stdout.strip()


def split_sql(sql: str) -> list[str]:
    uncommented = "\n".join(
        line for line in sql.splitlines() if not line.lstrip().startswith("--")
    )
    statements = [part.strip() for part in uncommented.split(";")]
    return [part for part in statements if part]


def summarize_statement(
    dsn: str, statement: str, timeout: int, prelude: str = ""
) -> dict[str, object]:
    setup = f"\\o /dev/null\n{prelude}\n\\o\n" if prelude else ""
    wrapped = f"""
{setup}
WITH __pggraph_playground_query AS (
{textwrap.indent(statement, "  ")}
),
__pggraph_numbered AS (
  SELECT row_number() OVER () AS row_number,
         to_jsonb(__pggraph_playground_query) AS row_json
  FROM __pggraph_playground_query
)
SELECT jsonb_build_object(
  'row_count', count(*),
  'hash', md5(coalesce(string_agg(row_json::text, E'\\n' ORDER BY row_number), ''))
)::text
FROM __pggraph_numbered;
"""
    raw = run_psql(dsn, wrapped, timeout)
    if not raw:
        raise RuntimeError("statement returned no summary row")
    return json.loads(raw)


def summarize_query(
    dsn: str, sql: str, timeout: int, prelude: str = ""
) -> list[dict[str, object]]:
    return [
        summarize_statement(
            dsn,
            statement,
            timeout,
            prelude if index == 0 else "",
        )
        for index, statement in enumerate(split_sql(sql))
    ]


def validate_catalog(expected: dict[str, list[dict[str, object]]]) -> dict[str, str]:
    queries = query_catalog()
    query_labels = set(queries)
    question_labels = set(QUERY_QUESTIONS)
    expected_labels = set(expected)
    errors: dict[str, str] = {}

    missing_questions = sorted(query_labels - question_labels)
    if missing_questions:
        errors["questions"] = f"missing questions for: {', '.join(missing_questions)}"

    stale_questions = sorted(question_labels - query_labels)
    if stale_questions:
        errors["stale_questions"] = f"questions without queries: {', '.join(stale_questions)}"

    missing_expected = sorted(query_labels - expected_labels)
    if missing_expected:
        errors["expected"] = f"missing expectations for: {', '.join(missing_expected)}"

    stale_expected = sorted(expected_labels - query_labels)
    if stale_expected:
        errors["stale_expected"] = f"expectations without queries: {', '.join(stale_expected)}"

    return errors


def comparable(label: str, summary: list[dict[str, object]]) -> list[dict[str, object]]:
    if label not in VOLATILE_HASH_LABELS:
        return summary
    return [{"row_count": result["row_count"]} for result in summary]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dsn", default=default_dsn())
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument(
        "--dump-expectations",
        action="store_true",
        help="Print the current catalog result summary as Python literals.",
    )
    args = parser.parse_args()

    expected = {
        label: comparable(label, summary)
        for label, summary in EXPECTED_RESULTS.items()
    }
    if not args.dump_expectations:
        catalog_errors = validate_catalog(expected)
        if catalog_errors:
            for key, message in catalog_errors.items():
                print(f"Catalog mismatch [{key}]: {message}", file=sys.stderr)
            return 1

    run_psql(args.dsn, "SELECT count(*) FROM panama.nodes; SELECT count(*) FROM panama.edges;", args.timeout)
    run_psql(args.dsn, SETUP_SQL, args.timeout)

    actual: dict[str, list[dict[str, object]]] = {}
    failures: list[str] = []
    for label, sql in query_catalog().items():
        try:
            prelude = SETUP_SQL if label in SAME_SESSION_SETUP_LABELS else ""
            summary = summarize_query(args.dsn, sql, args.timeout, prelude=prelude)
        except Exception as exc:  # noqa: BLE001
            failures.append(f"{label}: {exc}")
            continue
        actual[label] = comparable(label, summary)
        if not args.dump_expectations and actual[label] != expected[label]:
            failures.append(
                f"{label}: expected {json.dumps(expected[label], sort_keys=True)} "
                f"got {json.dumps(actual[label], sort_keys=True)}"
            )

    if args.dump_expectations:
        if failures:
            print("Could not dump complete playground expectations:", file=sys.stderr)
            for failure in failures:
                print(f"  - {failure}", file=sys.stderr)
            return 1
        print("EXPECTED_RESULTS = {")
        for label, summary in actual.items():
            print(f"    {label!r}: {summary!r},")
        print("}")
        return 0

    if failures:
        print("Playground release gate failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(f"Playground release gate passed: {len(actual)} queries validated")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
