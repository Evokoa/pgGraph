#!/usr/bin/env python3
"""Check or explicitly regenerate the machine-readable pgGraph 1.x contract."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import sys
from collections import Counter
from datetime import date
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "release" / "v1-contract.json"
CHANGES = ROOT / "release" / "contract-changes.json"
DEFAULT_SCHEMA = ROOT / "release" / "v1-schema.sql"
GQL_PROFILE = ROOT / "release" / "v1-gql-profile.json"
GQL_DOC = ROOT / "docs" / "user_guide" / "gql-profile.mdx"
SQL_PROFILE = ROOT / "release" / "v1-sql-profile.json"
SQL_DOC = ROOT / "docs" / "user_guide" / "sql-profile.mdx"
SQL_SIGNATURE_DOC = ROOT / "docs" / "user_guide" / "sql-signatures.mdx"
SAFETY = ROOT / "graph" / "src" / "safety.rs"
CONFIG = ROOT / "graph" / "src" / "config.rs"
GQL_SOURCES = [
    ROOT / "graph" / "src" / "gql",
    ROOT / "graph" / "src" / "query",
    ROOT / "graph" / "src" / "pg_tests" / "gql.rs",
]


def load_api_checker():
    path = ROOT / "scripts" / "check_sql_api_drift.py"
    spec = importlib.util.spec_from_file_location("check_sql_api_drift", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def normalized_source(path: Path) -> str:
    return "\n".join(line.rstrip() for line in path.read_text(encoding="utf-8").splitlines())


def source_tree_hash(paths: list[Path]) -> str:
    records: list[str] = []
    for path in paths:
        files = sorted(path.rglob("*.rs")) if path.is_dir() else [path]
        for file in files:
            records.append(str(file.relative_to(ROOT)))
            records.append(normalized_source(file))
    return sha256_text("\n".join(records))


def normalized_schema_blocks(schema_path: Path) -> list[str]:
    text = schema_path.read_text(encoding="utf-8")
    blocks = re.findall(
        r"/\* <begin connected objects> \*/(.*?)/\* </end connected objects> \*/",
        text,
        re.DOTALL,
    )
    normalized: list[str] = []
    for block in blocks:
        lines = [line for line in block.splitlines() if not line.lstrip().startswith("-- src/")]
        compact = re.sub(r"\s+", " ", "\n".join(lines)).strip()
        if compact:
            normalized.append(compact)
    return sorted(normalized)


def schema_contract(schema_path: Path) -> dict[str, object]:
    normalized = normalized_schema_blocks(schema_path)
    function_contracts = sorted(
        block
        for block in normalized
        if re.search(r'CREATE(?: OR REPLACE)?\s+FUNCTION\s+graph\.(?:"[^"]+"|\w+)', block, re.I)
    )
    if not normalized or not function_contracts:
        raise RuntimeError(
            "generated SQL schema produced no contract blocks or functions; "
            "refusing an empty compatibility baseline"
        )
    return {
        "entity_block_count": len(normalized),
        "function_contract_count": len(function_contracts),
        "normalized_sha256": sha256_text("\n".join(normalized)),
        "function_contracts_sha256": sha256_text("\n".join(function_contracts)),
    }


def schema_function_declarations(schema_path: Path) -> list[dict[str, str]]:
    declarations: list[dict[str, str]] = []
    for block in normalized_schema_blocks(schema_path):
        match = re.search(
            r'CREATE(?: OR REPLACE)?\s+FUNCTION\s+graph\.(?:"([^"]+)"|(\w+))',
            block,
            re.I,
        )
        if match is None:
            continue
        declaration = re.sub(r"/\*.*?\*/", "", block)
        declaration = re.split(r"\s+AS\s+(?:'MODULE_PATHNAME'|\$\$)", declaration, maxsplit=1)[0]
        declaration = re.sub(r"\s+", " ", declaration).strip().rstrip(";") + ";"
        declarations.append({"name": match.group(1) or match.group(2), "declaration": declaration})
    return sorted(declarations, key=lambda item: (item["name"], item["declaration"]))


def diagnostics() -> list[dict[str, str]]:
    text = SAFETY.read_text(encoding="utf-8")
    codes = dict(re.findall(r'Self::(\w+)\s*=>\s*"(PG\d{3})"', text))
    states = dict(
        re.findall(
            r'GraphError::(\w+)(?:\s*\{[^}]*\}|\([^)]*\))?\s*=>\s*\(\s*'
            r'GraphDiagnosticCode::\w+,\s*"([0-9A-Z]{5})"',
            text,
            re.DOTALL,
        )
    )
    if set(codes) != set(states):
        raise RuntimeError(
            "diagnostic extraction mismatch: "
            f"codes-only={sorted(set(codes) - set(states))}, "
            f"states-only={sorted(set(states) - set(codes))}"
        )
    return [
        {"diagnostic": codes[name], "sqlstate": states[name], "variant": name}
        for name in sorted(codes, key=lambda name: codes[name])
    ]


def validated_gql_profile() -> dict:
    profile = json.loads(GQL_PROFILE.read_text(encoding="utf-8"))
    assurance_profiles = profile.get("assurance_profiles")
    if not isinstance(assurance_profiles, dict):
        raise RuntimeError("release/v1-gql-profile.json must define assurance_profiles")
    required_tests = {
        "read": {"negative_diagnostics", "acl_rls", "resource", "operator_bounds"},
        "write": {"negative_diagnostics", "acl_rls", "transaction", "resource", "source_of_truth", "write_recheck"},
    }

    def validate_reference(item: dict, *, executable: bool) -> None:
        path = ROOT / item["path"]
        symbol = item["symbol"]
        source = path.read_text(encoding="utf-8") if path.is_file() else ""
        if executable:
            test_pattern = re.compile(
                rf"#\[(?:pg_test|test)\]\s*fn\s+{re.escape(symbol)}\s*\("
            )
            if not test_pattern.search(source):
                raise RuntimeError(f"GQL executable test evidence missing: {item}")
        elif symbol not in source:
            raise RuntimeError(f"GQL implementation/documentation evidence missing: {item}")

    for name, required in required_tests.items():
        assurance = assurance_profiles.get(name)
        if not isinstance(assurance, dict):
            raise RuntimeError(f"missing GQL assurance profile: {name}")
        implementation = assurance.get("implementation", [])
        stages = {item.get("stage") for item in implementation}
        if stages != {"parser", "binder", "executor"}:
            raise RuntimeError(f"GQL assurance profile {name} must cover parser, binder, and executor")
        for item in implementation:
            validate_reference(item, executable=False)
        tests = assurance.get("tests", {})
        if set(tests) != required:
            raise RuntimeError(
                f"GQL assurance profile {name} test dimensions differ: "
                f"missing={sorted(required - set(tests))}, extra={sorted(set(tests) - required)}"
            )
        for items in tests.values():
            if not isinstance(items, list) or not items:
                raise RuntimeError(f"GQL assurance profile {name} has an empty test dimension")
            for item in items:
                validate_reference(item, executable=True)
        documentation = assurance.get("documentation", [])
        if not isinstance(documentation, list) or not documentation:
            raise RuntimeError(f"GQL assurance profile {name} has no documentation evidence")
        for item in documentation:
            validate_reference(item, executable=False)

    corpus_path = ROOT / profile.get("unsupported_corpus", "")
    corpus = json.loads(corpus_path.read_text(encoding="utf-8")) if corpus_path.is_file() else {}
    cases = corpus.get("cases")
    if not isinstance(cases, list) or not cases:
        raise RuntimeError("GQL unsupported corpus must contain cases")
    case_ids = [case.get("id") for case in cases]
    if len(case_ids) != len(set(case_ids)):
        raise RuntimeError("GQL unsupported corpus ids must be unique")
    for case in cases:
        if case.get("sqlstate") not in {"0A000", "42601"} or not all(
            isinstance(case.get(field), str) and case[field]
            for field in ("id", "query", "message_contains")
        ):
            raise RuntimeError(f"invalid GQL unsupported corpus case: {case}")

    capabilities = profile.get("capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        raise RuntimeError("release/v1-gql-profile.json must contain capabilities")
    seen: set[str] = set()
    for capability in capabilities:
        capability_id = capability.get("id")
        evidence = capability.get("evidence")
        if not isinstance(capability_id, str) or capability_id in seen:
            raise RuntimeError(f"invalid or duplicate GQL capability id: {capability_id!r}")
        seen.add(capability_id)
        if capability.get("assurance_profile") not in assurance_profiles:
            raise RuntimeError(f"GQL capability {capability_id} has no valid assurance profile")
        if not isinstance(evidence, list) or not evidence:
            raise RuntimeError(f"GQL capability {capability_id} has no evidence")
        for item in evidence:
            validate_reference(item, executable=True)
    return profile


def validated_sql_profile(public_functions: list[str]) -> dict:
    """Validate exhaustive SQL capability ownership and executable assurance evidence."""
    profile = json.loads(SQL_PROFILE.read_text(encoding="utf-8"))
    groups = profile.get("groups")
    if not isinstance(groups, list) or not groups:
        raise RuntimeError("release/v1-sql-profile.json must contain capability groups")
    dispositions = profile.get("function_dispositions", {})
    if profile.get("default_disposition") != "supported" or not isinstance(dispositions, dict):
        raise RuntimeError("SQL profile must declare a supported default disposition")
    unknown_dispositions = set(dispositions) - set(public_functions)
    invalid_dispositions = set(dispositions.values()) - {
        "supported",
        "bounded_compatibility",
        "intentional_rejection",
    }
    if unknown_dispositions or invalid_dispositions:
        raise RuntimeError(
            "invalid SQL function dispositions: "
            f"unknown={sorted(unknown_dispositions)}, values={sorted(invalid_dispositions)}"
        )

    group_ids: set[str] = set()
    owners: dict[str, list[str]] = {name: [] for name in public_functions}
    for group in groups:
        group_id = group.get("id")
        if not isinstance(group_id, str) or not group_id or group_id in group_ids:
            raise RuntimeError(f"invalid or duplicate SQL capability group: {group_id!r}")
        group_ids.add(group_id)
        patterns = group.get("function_patterns")
        if not isinstance(patterns, list) or not patterns:
            raise RuntimeError(f"SQL capability group {group_id} has no function patterns")
        compiled = [re.compile(pattern) for pattern in patterns]
        matched = [name for name in public_functions if any(regex.fullmatch(name) for regex in compiled)]
        if not matched:
            raise RuntimeError(f"SQL capability group {group_id} matches no public functions")
        for name in matched:
            owners[name].append(group_id)

        tests = group.get("tests")
        if not isinstance(tests, dict) or not tests:
            raise RuntimeError(f"SQL capability group {group_id} has no test evidence")
        if not {"positive", "negative_diagnostics"}.issubset(tests):
            raise RuntimeError(
                f"SQL capability group {group_id} must include positive and negative diagnostics evidence"
            )
        for dimension, items in tests.items():
            if not isinstance(items, list) or not items:
                raise RuntimeError(f"SQL capability group {group_id} has empty {dimension} evidence")
            for item in items:
                path = ROOT / item["path"]
                source = path.read_text(encoding="utf-8") if path.is_file() else ""
                symbol = item["symbol"]
                test_pattern = re.compile(
                    rf"#\[(?:pg_test|test)\]\s*fn\s+{re.escape(symbol)}\s*\("
                )
                if not test_pattern.search(source):
                    raise RuntimeError(f"SQL executable test evidence missing: {item}")

        documentation = group.get("documentation")
        if not isinstance(documentation, list) or not documentation:
            raise RuntimeError(f"SQL capability group {group_id} has no documentation evidence")
        for item in documentation:
            path = ROOT / item["path"]
            source = path.read_text(encoding="utf-8") if path.is_file() else ""
            if item["symbol"] not in source:
                raise RuntimeError(f"SQL documentation evidence missing: {item}")

    invalid = {name: groups for name, groups in owners.items() if len(groups) != 1}
    if invalid:
        raise RuntimeError(
            "public SQL functions must belong to exactly one capability group: "
            f"{invalid}"
        )
    return profile


def expected_contract(schema_path: Path) -> dict:
    api = load_api_checker()
    functions = sorted(api.implemented_functions())
    internal = [name for name in functions if name.startswith("_") or name == "test_enabled"]
    public = [name for name in functions if name not in internal]
    public_signatures = [
        item for item in schema_function_declarations(schema_path) if item["name"] in public
    ]
    gql_profile = validated_gql_profile()
    sql_profile = validated_sql_profile(public)
    return {
        "schema_version": 2,
        "release_line": "1.x",
        "postgresql": {
            "supported_majors": [14, 15, 16, 17, 18],
            "default_test_major": 17,
            "playground_reference_major": 17,
        },
        "distribution": {
            "source": ["PGXN", "GitHub release archive"],
            "containers": ["linux/amd64", "linux/arm64"],
        },
        "artifact_policy": {
            "authoritative_source": "PostgreSQL source tables",
            "incompatible_action": "fail_closed_and_rebuild",
            "silent_reinterpretation": False,
        },
        "sql_schema": schema_contract(schema_path),
        "sql_signatures": public_signatures,
        "public_sql_functions": public,
        "internal_sql_functions": internal,
        "sql_profile": sql_profile,
        "gucs": sorted(api.implemented_gucs()),
        "guc_source_sha256": sha256_text(normalized_source(CONFIG)),
        "diagnostics": diagnostics(),
        "gql_profile": gql_profile,
        "gql_implementation_sha256": source_tree_hash(GQL_SOURCES),
    }


def serialized(value: object) -> str:
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


def gql_profile_doc(profile: dict) -> str:
    lines = [
        "---",
        'title: "GQL 1.0 profile"',
        'description: "The exact GQL read and PostgreSQL-backed write capabilities supported by pgGraph 1.0."',
        "---",
        "",
        "{/* Generated from release/v1-gql-profile.json; do not edit by hand. */}",
        "",
        "pgGraph 1.0 implements the documented subset below. It does not claim full ISO GQL,",
        "full openCypher, or SQL/PGQ conformance. PostgreSQL source tables remain authoritative",
        "for every mapped write.",
        "",
        "| Capability ID | Profile | Positive executable evidence |",
        "|---|---|---|",
    ]
    for capability in profile["capabilities"]:
        evidence = ", ".join(f"`{item['symbol']}`" for item in capability["evidence"])
        lines.append(f"| `{capability['id']}` | {capability['assurance_profile']} | {evidence} |")
    lines.extend(
        [
            "",
            "Each read capability inherits parser, binder, governed executor, negative diagnostic,",
            "ACL/RLS, resource, and documentation evidence. Each write capability additionally",
            "inherits transaction, rollback/resource, and PostgreSQL source-of-truth evidence.",
            "The release contract checker verifies every referenced implementation marker and test.",
            "",
            "Unsupported and compatibility syntax is covered by",
            "`release/v1-gql-unsupported.json`. Those examples must fail before execution with",
            "their recorded SQLSTATE and stable guidance fragment; they are not silently accepted",
            "as a broader compatibility promise.",
            "",
        ]
    )
    return "\n".join(lines)


def sql_profile_doc(profile: dict, public_functions: list[str]) -> str:
    lines = [
        "---",
        'title: "SQL 1.0 profile"',
        'description: "The exact PostgreSQL SQL function groups and assurance evidence supported by pgGraph 1.0."',
        "---",
        "",
        "{/* Generated from release/v1-sql-profile.json and release/v1-contract.json; do not edit by hand. */}",
        "",
        "The functions listed in the 1.0 release contract are the complete supported SQL surface.",
        "Every function belongs to exactly one capability group below. The release checker verifies",
        "that each group has executable positive and negative evidence, applicable authorization,",
        "transaction, resource, or recovery evidence, and public documentation.",
        "Compatibility and intentionally rejected endpoints are identified explicitly below.",
        "",
        "| Capability group | Public functions | Assurance dimensions |",
        "|---|---|---|",
    ]
    for group in profile["groups"]:
        patterns = [re.compile(pattern) for pattern in group["function_patterns"]]
        functions = [
            name
            for name in public_functions
            if any(regex.fullmatch(name) for regex in patterns)
        ]
        function_text = ", ".join(f"`graph.{name}()`" for name in functions)
        dimensions = ", ".join(group["tests"])
        lines.append(f"| `{group['id']}` | {function_text} | {dimensions} |")
    lines.extend(
        [
            "",
            "## Compatibility dispositions",
            "",
        ]
    )
    for name, disposition in sorted(profile["function_dispositions"].items()):
        lines.append(f"- `graph.{name}()` — `{disposition}`")
    lines.extend(
        [
            "",
            "This profile describes pgGraph's SQL extension API. It does not expand the separate",
            "GQL or openCypher compatibility claims; see the GQL 1.0 profile for those boundaries.",
            "",
        ]
    )
    return "\n".join(lines)


def sql_signature_doc(signatures: list[dict[str, str]]) -> str:
    lines = [
        "---",
        'title: "Exact SQL signatures"',
        'description: "Generated PostgreSQL signatures, defaults, and result shapes for the pgGraph 1.0 SQL API."',
        "---",
        "",
        "{/* Generated from release/v1-schema.sql; do not edit by hand. */}",
        "",
        "This page is the exact PostgreSQL 1.0 signature contract. It includes overloads,",
        "argument defaults, return types, and table result columns from the release schema.",
        "The release contract gate regenerates and compares every declaration.",
        "",
    ]
    totals = Counter(item["name"] for item in signatures)
    seen: Counter[str] = Counter()
    for item in signatures:
        seen[item["name"]] += 1
        suffix = f" (overload {seen[item['name']]})" if totals[item["name"]] > 1 else ""
        lines.extend(
            [
                f"### `graph.{item['name']}`{suffix}",
                "",
                "```sql",
                item["declaration"],
                "```",
                "",
            ]
        )
    return "\n".join(lines)


def changed_sections(old: dict, new: dict) -> list[str]:
    return sorted(key for key in set(old) | set(new) if old.get(key) != new.get(key))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schema-file", type=Path, default=DEFAULT_SCHEMA)
    parser.add_argument("--write", action="store_true")
    parser.add_argument(
        "--acknowledge",
        help="required change/review identifier when regenerating a changed contract",
    )
    args = parser.parse_args()

    schema_path = args.schema_file.resolve()
    if not schema_path.is_file():
        print(f"generated SQL schema missing: {schema_path}", file=sys.stderr)
        return 1
    expected = expected_contract(schema_path)
    expected_gql_doc = gql_profile_doc(expected["gql_profile"])
    expected_sql_doc = sql_profile_doc(expected["sql_profile"], expected["public_sql_functions"])
    expected_signature_doc = sql_signature_doc(expected["sql_signatures"])
    actual = json.loads(CONTRACT.read_text(encoding="utf-8")) if CONTRACT.exists() else {}

    if args.write:
        sections = changed_sections(actual, expected)
        if actual and sections and not args.acknowledge:
            print(
                "contract changed in sections " + ", ".join(sections)
                + "; rerun with --acknowledge <review-id> after compatibility review",
                file=sys.stderr,
            )
            return 2
        if sections and args.acknowledge:
            records = json.loads(CHANGES.read_text(encoding="utf-8")) if CHANGES.exists() else []
            records.append(
                {
                    "acknowledgement": args.acknowledge,
                    "date": date.today().isoformat(),
                    "changed_sections": sections,
                    "previous_schema_sha256": actual.get("sql_schema", {}).get("normalized_sha256"),
                    "new_schema_sha256": expected["sql_schema"]["normalized_sha256"],
                }
            )
            CHANGES.write_text(serialized(records), encoding="utf-8")
        CONTRACT.write_text(serialized(expected), encoding="utf-8")
        GQL_DOC.write_text(expected_gql_doc, encoding="utf-8")
        SQL_DOC.write_text(expected_sql_doc, encoding="utf-8")
        SQL_SIGNATURE_DOC.write_text(expected_signature_doc, encoding="utf-8")
        print(f"wrote {CONTRACT.relative_to(ROOT)}")
        return 0

    docs_current = GQL_DOC.is_file() and GQL_DOC.read_text(encoding="utf-8") == expected_gql_doc
    sql_docs_current = SQL_DOC.is_file() and SQL_DOC.read_text(encoding="utf-8") == expected_sql_doc
    signature_docs_current = (
        SQL_SIGNATURE_DOC.is_file()
        and SQL_SIGNATURE_DOC.read_text(encoding="utf-8") == expected_signature_doc
    )
    if actual != expected or not docs_current or not sql_docs_current or not signature_docs_current:
        print(
            "release/v1-contract.json is stale or incompatible with the supplied schema; "
            "review the compatibility diff before regenerating",
            file=sys.stderr,
        )
        for section in changed_sections(actual, expected):
            print(f"  changed: {section}", file=sys.stderr)
        if not docs_current:
            print("  changed: generated GQL profile documentation", file=sys.stderr)
        if not sql_docs_current:
            print("  changed: generated SQL profile documentation", file=sys.stderr)
        if not signature_docs_current:
            print("  changed: generated SQL signature documentation", file=sys.stderr)
        return 1

    print("pgGraph 1.0 release contract is in sync.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
