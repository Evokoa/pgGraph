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

from catalog import query_catalog  # noqa: E402


EXPECTED_RESULTS_CSR: dict[str, list[dict[str, object]]] = {
    "Status + Catalog": [
        {"hash": "da21ad3278cbf648389f1be32503f1b8", "row_count": 1},
        {"hash": "c1741c2f950d88a7f4d9dd218bad9e97", "row_count": 1},
        {"hash": "1e0dbfdd2e1b70091add7b43b7a892f7", "row_count": 1},
    ],
    "Search Mossack": [{"hash": "ccef511d92bf067cffc041afa65a61f5", "row_count": 20}],
    "Find Mossack": [{"hash": "3428e7010463b2ec9aec7ec27fc3c0c7", "row_count": 20}],
    "Traverse Neighborhood": [{"hash": "29af2dc578f0318e598dcffaf3af46b0", "row_count": 100}],
    "Expand Neighborhood": [{"hash": "a4e83cfebb0f9c9dd6b77316b4a67084", "row_count": 100}],
    "Shortest Path": [{"hash": "8f8ad9f558f842c482d47fbde4e388f2", "row_count": 2}],
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
    "Officer Context Packet": [{"hash": "2100c2398f60d2229220c5e8e8030b3f", "row_count": 1}],
    "Search Entity Then Expand": [{"hash": "95023cdc9a5d98a05c658ea9b2da522b", "row_count": 450}],
    "Relationship Filtered Walk": [{"hash": "827a2f4de38d4427a04f64b7a6f3dee3", "row_count": 31}],
    "Capped 3-Hop Investigation": [{"hash": "f10295f757fde99ccc95052fbd2ca05c", "row_count": 300}],
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
        {"hash": "c1741c2f950d88a7f4d9dd218bad9e97", "row_count": 1},
        {"hash": "1e0dbfdd2e1b70091add7b43b7a892f7", "row_count": 1},
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
    "Largest Component",
    "Search Entity Then Expand",
    "Sync Health",
    "Apply Sync",
    "Scheduled Maintenance",
    "Vacuum Graph",
    "Maintenance",
    "Maintenance Status",
}

VOLATILE_HASH_STATEMENTS = {
    "Component Stats": {1},
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
    mutable_setup = "SET graph.mutable_enabled = on; SET graph.query_freshness = off;" if mode == "mutable" else ""
    return f"""
CREATE EXTENSION IF NOT EXISTS graph;
{mutable_setup}
SET graph.query_memory_mb = 512;
SET graph.maintenance_memory_mb = 1024;
SELECT graph.reset();
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


def run_psql(
    dsn: str,
    sql: str,
    timeout: int,
    *,
    stop_on_error: bool = True,
) -> str:
    proc = subprocess.run(
        [
            "psql",
            "-X",
            "-q",
            "-v",
            f"ON_ERROR_STOP={1 if stop_on_error else 0}",
            "-tA",
            dsn,
        ],
        input=sql,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip())
    if not stop_on_error and proc.stderr:
        print(proc.stderr, end="", file=sys.stderr)
        if any("ERROR:" in line for line in proc.stderr.splitlines()):
            raise RuntimeError(
                "one or more playground SQL statements failed; the complete psql error stream is shown above"
            )
    return proc.stdout.strip()


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def concurrent_build_completion_sql(target_table: str) -> str:
    return f"""
DO $$
DECLARE
  job_status text;
  job_error text;
  attempt integer;
BEGIN
  FOR attempt IN 1..300 LOOP
    SELECT jobs.status, jobs.error
      INTO job_status, job_error
      FROM graph._build_jobs AS jobs
      JOIN pg_temp.{target_table} AS target ON target.build_id = jobs.build_id;

    IF job_status = 'completed' THEN
      RETURN;
    END IF;
    IF job_status = 'failed' THEN
      RAISE EXCEPTION 'playground concurrent build failed: %', coalesce(job_error, 'unknown error');
    END IF;
    PERFORM pg_sleep(0.1);
  END LOOP;

  RAISE EXCEPTION 'playground concurrent build did not complete within 30 seconds (last status: %)',
    coalesce(job_status, 'missing');
END
$$;
"""


def summarize_catalog_session(
    dsn: str,
    mode: str,
    timeout: int,
) -> dict[str, list[dict[str, object]]]:
    setup = setup_sql(mode)
    quiet_setup = f"\\o /dev/null\n{setup}\n\\o\n"
    chunks = [quiet_setup]
    for label, example in query_catalog(mode).items():
        if label in SAME_SESSION_SETUP_LABELS:
            chunks.append(quiet_setup)
        for index, statement in enumerate(example.statements):
            chunks.append(f"\\warn pggraph playground gate: {label} [{index}]")
            query_sql = statement
            concurrent_target = "__pggraph_concurrent_build_target"
            if label == "Build Graph Concurrently":
                statement_sql = statement.rstrip().rstrip(";")
                chunks.append(f"DROP TABLE IF EXISTS pg_temp.{concurrent_target};")
                chunks.append(
                    f"CREATE TEMP TABLE {concurrent_target} ON COMMIT PRESERVE ROWS AS\n"
                    f"{statement_sql};"
                )
                query_sql = f"SELECT * FROM pg_temp.{concurrent_target}"
            chunks.append(
                f"""
WITH __pggraph_playground_query AS (
{textwrap.indent(query_sql, "  ")}
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
            if label == "Build Graph Concurrently":
                chunks.append(concurrent_build_completion_sql(concurrent_target))
    # Each wrapped query is an independent autocommit statement. Keep running
    # after SQL errors so one release-gate invocation reports every failing
    # playground example; missing summaries below still make the gate fail.
    raw = run_psql(dsn, "\n".join(chunks), timeout, stop_on_error=False)
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
    expected_labels = set(expected)
    errors: dict[str, str] = {}

    missing_questions = sorted(label for label, example in queries.items() if not example.question)
    if missing_questions:
        errors["questions"] = f"missing questions for: {', '.join(missing_questions)}"

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
