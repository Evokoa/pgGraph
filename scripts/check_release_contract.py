#!/usr/bin/env python3
"""Check or explicitly regenerate the machine-readable pgGraph 1.x contract."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import sys
from datetime import date
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "release" / "v1-contract.json"
CHANGES = ROOT / "release" / "contract-changes.json"
DEFAULT_SCHEMA = ROOT / "release" / "v1-schema.sql"
GQL_PROFILE = ROOT / "release" / "v1-gql-profile.json"
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


def schema_contract(schema_path: Path) -> dict[str, object]:
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
    normalized.sort()
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
        if not isinstance(evidence, list) or not evidence:
            raise RuntimeError(f"GQL capability {capability_id} has no evidence")
        for item in evidence:
            path = ROOT / item["path"]
            symbol = item["symbol"]
            source = path.read_text(encoding="utf-8") if path.is_file() else ""
            test_pattern = re.compile(
                rf"#\[(?:pg_test|test)\]\s*fn\s+{re.escape(symbol)}\s*\("
            )
            if not test_pattern.search(source):
                raise RuntimeError(
                    f"GQL capability {capability_id} executable test evidence missing: {item}"
                )
    return profile


def expected_contract(schema_path: Path) -> dict:
    api = load_api_checker()
    functions = sorted(api.implemented_functions())
    internal = [name for name in functions if name.startswith("_") or name == "test_enabled"]
    public = [name for name in functions if name not in internal]
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
        "public_sql_functions": public,
        "internal_sql_functions": internal,
        "gucs": sorted(api.implemented_gucs()),
        "guc_source_sha256": sha256_text(normalized_source(CONFIG)),
        "diagnostics": diagnostics(),
        "gql_profile": validated_gql_profile(),
        "gql_implementation_sha256": source_tree_hash(GQL_SOURCES),
    }


def serialized(value: object) -> str:
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


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
        print(f"wrote {CONTRACT.relative_to(ROOT)}")
        return 0

    if actual != expected:
        print(
            "release/v1-contract.json is stale or incompatible with the supplied schema; "
            "review the compatibility diff before regenerating",
            file=sys.stderr,
        )
        for section in changed_sections(actual, expected):
            print(f"  changed: {section}", file=sys.stderr)
        return 1

    print("pgGraph 1.0 release contract is in sync.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
