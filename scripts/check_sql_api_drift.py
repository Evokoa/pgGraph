#!/usr/bin/env python3
"""Check documentation drift for the SQL API and graph GUCs.

Usage:
  python3 scripts/check_sql_api_drift.py
  python3 scripts/check_sql_api_drift.py --list-implemented

What this checks:
  1. SQL functions implemented by #[pg_extern] or graph/sql/bootstrap.sql are
     listed in docs/user_guide/api-reference.mdx.
  2. Any graph.foo() function call documented anywhere under docs/ exists in
     the implementation or bootstrap SQL.
  3. GUCs registered in graph/src/config.rs are listed in
     docs/user_guide/configuration.mdx, and that page does not document removed
     graph.* settings.
  4. The public diagnostic SQLSTATE/PGxxx table exactly matches the release
     contract extracted from Rust error mappings.

The script intentionally checks names, not full signatures. pgrx owns SQL DDL
generation for Rust functions, so this lightweight guard is meant to catch
stale or missing documentation during normal edits without needing a running
PostgreSQL instance.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
GRAPH_SRC = ROOT / "graph" / "src"
BOOTSTRAP_SQL = ROOT / "graph" / "sql" / "bootstrap.sql"
DOCS_DIR = ROOT / "docs"
API_REFERENCE = DOCS_DIR / "user_guide" / "api-reference.mdx"
CONFIG_REFERENCE = DOCS_DIR / "user_guide" / "configuration.mdx"
DIAGNOSTIC_REFERENCE = DOCS_DIR / "user_guide" / "administration-and-security.mdx"
CONFIG_RS = GRAPH_SRC / "config.rs"
RELEASE_CONTRACT = ROOT / "release" / "v1-contract.json"


def implemented_functions() -> set[str]:
    """Return SQL function names exposed in the graph schema."""
    pg_extern = re.compile(
        r"#\[pg_extern\((.*?)\)\]"
        r"(?:\s*#\[[^\]]*\])*"
        r"\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+(\w+)",
        re.DOTALL,
    )
    names: set[str] = set()

    for rust_file in GRAPH_SRC.rglob("*.rs"):
        text = rust_file.read_text()
        for attr, rust_name in pg_extern.findall(text):
            sql_name = re.search(r'name\s*=\s*"([^"]+)"', attr)
            name = sql_name.group(1) if sql_name else rust_name
            if not name.startswith("_test"):
                names.add(name)

    bootstrap = BOOTSTRAP_SQL.read_text()
    names.update(
        re.findall(
            r"CREATE\s+OR\s+REPLACE\s+FUNCTION\s+graph\.([a-zA-Z_][a-zA-Z0-9_]*)\s*\(",
            bootstrap,
            re.IGNORECASE,
        )
    )

    return names


def documented_functions(path: pathlib.Path) -> set[str]:
    """Return graph.foo() call names mentioned in one documentation file."""
    text = path.read_text()
    return set(re.findall(r"graph\.([a-zA-Z_][a-zA-Z0-9_]*)\s*\(", text))


def documented_functions_by_location() -> dict[str, list[str]]:
    """Return graph.foo() documentation locations for all docs pages."""
    locations: dict[str, list[str]] = {}
    for doc in DOCS_DIR.rglob("*.mdx"):
        for line_no, line in enumerate(doc.read_text().splitlines(), start=1):
            for name in re.findall(r"graph\.([a-zA-Z_][a-zA-Z0-9_]*)\s*\(", line):
                rel = doc.relative_to(ROOT)
                locations.setdefault(name, []).append(f"{rel}:{line_no}")
    return locations


def implemented_gucs() -> set[str]:
    """Return graph.* GUC names registered by graph/src/config.rs."""
    text = CONFIG_RS.read_text()
    return set(
        re.findall(
            r"GucRegistry::define_(?:bool|int|string)_guc\(\s*c\"(graph\.[^\"]+)\"",
            text,
            re.DOTALL,
        )
    )


def documented_gucs() -> set[str]:
    """Return graph.* setting names in the first column of config tables."""
    names: set[str] = set()
    for line in CONFIG_REFERENCE.read_text().splitlines():
        match = re.match(r"\|\s*`(graph\.[a-zA-Z_][a-zA-Z0-9_]*)`\s*\|", line)
        if match:
            names.add(match.group(1))
    return names


def documented_diagnostics() -> set[tuple[str, str]]:
    pairs: set[tuple[str, str]] = set()
    for line in DIAGNOSTIC_REFERENCE.read_text(encoding="utf-8").splitlines():
        match = re.match(r"\|\s*`([0-9A-Z]{5})`\s*\|\s*`(PG[0-9]{3})`\s*\|", line)
        if match:
            pairs.add((match.group(1), match.group(2)))
    return pairs


def format_graph_functions(names: set[str]) -> str:
    return ", ".join(f"graph.{name}()" for name in sorted(names))


def report_set_diff(
    title: str,
    missing: set[str],
    extra: set[str],
    formatter=lambda names: ", ".join(sorted(names)),
) -> bool:
    """Print one drift report. Return True when drift was found."""
    if not missing and not extra:
        return False

    print(f"{title}:", file=sys.stderr)
    if missing:
        print(f"  missing from docs: {formatter(missing)}", file=sys.stderr)
    if extra:
        print(f"  documented but not implemented: {formatter(extra)}", file=sys.stderr)
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list-implemented",
        action="store_true",
        help="print implemented SQL functions and GUCs instead of checking docs",
    )
    args = parser.parse_args()

    implementation = implemented_functions()
    contract = json.loads(RELEASE_CONTRACT.read_text(encoding="utf-8"))
    public_functions = set(contract["public_sql_functions"])
    internal_functions = set(contract["internal_sql_functions"])
    if public_functions | internal_functions != implementation:
        print("release SQL function inventory differs from implementation", file=sys.stderr)
        return 1
    gucs = implemented_gucs()

    if args.list_implemented:
        print("SQL functions:")
        for name in sorted(implementation):
            print(f"  graph.{name}()")
        print("\nGUCs:")
        for name in sorted(gucs):
            print(f"  {name}")
        return 0

    failed = False

    api_functions = documented_functions(API_REFERENCE)
    failed |= report_set_diff(
        str(API_REFERENCE.relative_to(ROOT)),
        public_functions - api_functions,
        api_functions - public_functions,
        format_graph_functions,
    )

    documented_locations = documented_functions_by_location()
    stale_function_names = set(documented_locations) - implementation
    if stale_function_names:
        failed = True
        print("docs/ function references:", file=sys.stderr)
        for name in sorted(stale_function_names):
            print(
                f"  graph.{name}() is documented but not implemented: "
                + ", ".join(documented_locations[name]),
                file=sys.stderr,
            )

    config_gucs = documented_gucs()
    failed |= report_set_diff(
        str(CONFIG_REFERENCE.relative_to(ROOT)),
        gucs - config_gucs,
        config_gucs - gucs,
    )

    contract_diagnostics = {
        (item["sqlstate"], item["diagnostic"]) for item in contract["diagnostics"]
    }
    docs_diagnostics = documented_diagnostics()
    failed |= report_set_diff(
        str(DIAGNOSTIC_REFERENCE.relative_to(ROOT)) + " diagnostics",
        contract_diagnostics - docs_diagnostics,
        docs_diagnostics - contract_diagnostics,
        lambda pairs: ", ".join(f"{state}/{code}" for state, code in sorted(pairs)),
    )

    if not failed:
        print("SQL API and GUC documentation are in sync.")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
