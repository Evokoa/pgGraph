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


EXPECTED_RESULTS_CSR: dict[str, list[dict[str, object]]] = {
    "Status + Catalog": [
        {"hash": "da21ad3278cbf648389f1be32503f1b8", "row_count": 1},
        {"hash": "181a173a49cdbb4b9100e7b8ec693411", "row_count": 1},
        {"hash": "1f56018065f8f7688f33d312b976afd9", "row_count": 1},
    ],
    "Search Mossack": [{"hash": "0c8c22baa3ad02b3ae87b782f5b9e107", "row_count": 20}],
    "Find Mossack": [{"hash": "e5f8e6df0fad12cffba8ddd3df1837b5", "row_count": 20}],
    "Traverse Neighborhood": [{"hash": "8838da4bf5f4f4741822d8646deafe68", "row_count": 100}],
    "Expand Neighborhood": [{"hash": "625782ba027f4a5ace8308363816a3ac", "row_count": 100}],
    "Shortest Path": [{"hash": "0273f33efa04c4ba2bf45e57e703e58d", "row_count": 2}],
    "GQL Parameterized Match": [{"hash": "2d77220992fd54f4cd7150bb9bb984dc", "row_count": 1}],
    "GQL Scalar Projection": [{"hash": "bdb8fabf93f84ef1c80dacef37133512", "row_count": 4}],
    "GQL One-Hop Relationships": [{"hash": "cc43fef5258a696fee573d7ce63d3161", "row_count": 1}],
    "GQL Relationship Projection": [{"hash": "decfd268084234436536db856f335c70", "row_count": 1}],
    "GQL Inbound Relationships": [{"hash": "8667d6872adef948f4cd19a6d418af56", "row_count": 1}],
    "GQL Undirected Relationships": [{"hash": "471d8f696e537993a8f1e8a9be703095", "row_count": 1}],
    "GQL Distinct Labels": [{"hash": "2e9e4f9151f7e0f5d63cd7a1f38533ab", "row_count": 1}],
    "GQL Aggregated Neighbors": [{"hash": "e17964650e3a49bd449fcb1569ac5c31", "row_count": 1}],
    "GQL Aggregate By Label": [{"hash": "672202fe114050577e9ea56668f354bc", "row_count": 1}],
    "GQL Collect Neighbor Labels": [{"hash": "60c9a3b2e3efa2d06f11bbfbff41e9b1", "row_count": 1}],
    "GQL Variable-Length Paths": [{"hash": "27db82ea68fe6f630e0705bf080e742d", "row_count": 4}],
    "GQL Path Functions": [{"hash": "c565149448609e644287b30bf0345f6e", "row_count": 3}],
    "GQL Hydration Off": [{"hash": "e4b9e13b69f2e410f2729fa40b208524", "row_count": 1}],
    "GQL Explain": [{"hash": "a928d32b543bc838ae25f9b27b24ae90", "row_count": 1}],
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


EXPECTED_RESULTS_MUTABLE: dict[str, list[dict[str, object]]] = {
    **EXPECTED_RESULTS_CSR,
    "Status + Catalog": [
        {"hash": "d1427afc6ed07bf69332eea22aa72ac3", "row_count": 1},
        {"hash": "181a173a49cdbb4b9100e7b8ec693411", "row_count": 1},
        {"hash": "1f56018065f8f7688f33d312b976afd9", "row_count": 1},
    ],
    "Mutable GQL Merge Node": [{"hash": "ec7299e8a08b19f8202d48f113f0c37a", "row_count": 1}],
    "Mutable GQL Merge Update": [{"hash": "90d1e0d59d2c10b8ca6ac1edd9985a9b", "row_count": 1}],
    "Table Sizes": [{"hash": "a1005853cb0ad369d9598975b7654122", "row_count": 6}],
}
EXPECTED_RESULTS_MUTABLE.pop("Build Graph Concurrently", None)


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

VOLATILE_HASH_STATEMENTS = {
    "Status + Catalog": {0},
}

SAME_SESSION_SETUP_LABELS = {
    "Apply Sync",
    "Scheduled Maintenance",
    "Vacuum Graph",
    "Maintenance",
}


def setup_sql(mode: str) -> str:
    build_mode = "mutable_overlay" if mode == "mutable" else "csr_readonly"
    mutable_setup = "SET graph.mutable_enabled = on;" if mode == "mutable" else ""
    return f"""
CREATE EXTENSION IF NOT EXISTS graph;
SELECT graph.test_enabled();
{mutable_setup}
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
DELETE FROM panama.edges
WHERE start_id LIKE 'pggraph-playground-%'
   OR end_id LIKE 'pggraph-playground-%';
DELETE FROM panama.nodes
WHERE node_id LIKE 'pggraph-playground-%';
SET graph.persist_on_build = on;
SELECT * FROM graph.build('{build_mode}');
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


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


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


def summarize_catalog_session(
    dsn: str,
    mode: str,
    timeout: int,
) -> dict[str, list[dict[str, object]]]:
    setup = setup_sql(mode)
    quiet_setup = f"\\o /dev/null\n{setup}\n\\o\n"
    chunks = [quiet_setup]
    for label, sql in query_catalog(mode).items():
        if label in SAME_SESSION_SETUP_LABELS:
            chunks.append(quiet_setup)
        for index, statement in enumerate(split_sql(sql)):
            chunks.append(f"\\warn pggraph playground gate: {label} [{index}]")
            chunks.append(
                f"""
WITH __pggraph_playground_query AS (
{textwrap.indent(statement, "  ")}
),
__pggraph_numbered AS (
  SELECT row_number() OVER () AS row_number,
         to_jsonb(__pggraph_playground_query) AS row_json
  FROM __pggraph_playground_query
)
SELECT jsonb_build_object(
  'label', {sql_literal(label)},
  'statement_index', {index},
  'row_count', count(*),
  'hash', md5(coalesce(string_agg(row_json::text, E'\\n' ORDER BY row_number), ''))
)::text
FROM __pggraph_numbered;
"""
            )
    raw = run_psql(dsn, "\n".join(chunks), timeout)
    actual: dict[str, list[dict[str, object]]] = {}
    for line in raw.splitlines():
        if not line:
            continue
        result = json.loads(line)
        label = result.pop("label")
        result.pop("statement_index", None)
        actual.setdefault(label, []).append(result)
    return actual


def validate_catalog(expected: dict[str, list[dict[str, object]]], mode: str) -> dict[str, str]:
    queries = query_catalog(mode)
    query_labels = set(queries)
    question_labels = set(QUERY_QUESTIONS)
    all_query_labels = set(query_catalog("csr")) | set(query_catalog("mutable"))
    expected_labels = set(expected)
    errors: dict[str, str] = {}

    missing_questions = sorted(query_labels - question_labels)
    if missing_questions:
        errors["questions"] = f"missing questions for: {', '.join(missing_questions)}"

    stale_questions = sorted(question_labels - all_query_labels)
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
        volatile_indexes = VOLATILE_HASH_STATEMENTS.get(label, set())
        return [
            {"row_count": result["row_count"]} if index in volatile_indexes else result
            for index, result in enumerate(summary)
        ]
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
    parser.add_argument(
        "--mode",
        choices=["csr", "csr_readonly", "mutable", "mutable_overlay"],
        default=os.environ.get("PGGRAPH_PLAYGROUND_MODE", "csr"),
        help="Playground mode to validate.",
    )
    args = parser.parse_args()
    args.mode = "mutable" if args.mode in {"mutable", "mutable_overlay"} else "csr"

    expected_results = EXPECTED_RESULTS_MUTABLE if args.mode == "mutable" else EXPECTED_RESULTS_CSR
    expected = {
        label: comparable(label, summary)
        for label, summary in expected_results.items()
    }
    if not args.dump_expectations:
        catalog_errors = validate_catalog(expected, args.mode)
        if catalog_errors:
            for key, message in catalog_errors.items():
                print(f"Catalog mismatch [{key}]: {message}", file=sys.stderr)
            return 1

    failures: list[str] = []
    run_psql(args.dsn, "SELECT count(*) FROM panama.nodes; SELECT count(*) FROM panama.edges;", args.timeout)
    try:
        actual = {
            label: comparable(label, summary)
            for label, summary in summarize_catalog_session(args.dsn, args.mode, args.timeout).items()
        }
    except Exception as exc:  # noqa: BLE001
        failures.append(str(exc))
        actual = {}

    for label in query_catalog(args.mode):
        if label not in actual:
            failures.append(f"{label}: no summary produced")
            continue
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

    print(f"Playground release gate passed: {len(actual)} {args.mode} queries validated")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
