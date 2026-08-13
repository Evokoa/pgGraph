#!/usr/bin/env python3
"""Validate the exhaustive public topology visibility route inventory."""

from __future__ import annotations

import json
import pathlib
import re
import sys

from check_sql_api_drift import implemented_functions

ROOT = pathlib.Path(__file__).resolve().parents[1]
GRAPH_SRC = ROOT / "graph" / "src"
CONTRACT = ROOT / "release" / "v1-contract.json"
INVENTORY = ROOT / "todo" / "post-v1-1" / "topology-security-inventory.json"
PARTITION_KEYS = (
    "topology_visibility_required",
    "postgres_direct_visibility",
    "not_topology_read",
)
STRATEGIES = {
    "targeted_scope",
    "targeted_source_probe",
    "whole_graph_eager",
    "conditional_statement",
}
TERMINAL_CALLS = {
    "prepare_eager_visibility(",
    "source_row_visible(",
}


def load_json(path: pathlib.Path) -> dict[str, object]:
    return json.loads(path.read_text(encoding="utf-8"))


def topology_pg_extern_entrypoints(
    required_sql_names: set[str],
) -> set[tuple[str, str, str]]:
    """Return public topology SQL/Rust/source triples from pgrx attributes."""
    pattern = re.compile(
        r"#\[pg_extern\((.*?)\)\]"
        r"(?:\s*#\[[^\]]*\])*"
        r"\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+(\w+)",
        re.DOTALL,
    )
    entrypoints: set[tuple[str, str, str]] = set()
    for path in GRAPH_SRC.rglob("*.rs"):
        text = path.read_text(encoding="utf-8")
        for attributes, rust_function in pattern.findall(text):
            override = re.search(r'name\s*=\s*"([^"]+)"', attributes)
            sql_name = override.group(1) if override else rust_function
            if sql_name in required_sql_names:
                entrypoints.add(
                    (sql_name, rust_function, path.relative_to(ROOT).as_posix())
                )
    return entrypoints


def rust_function_body(path: pathlib.Path, function: str) -> str | None:
    """Return one Rust function body while ignoring braces in comments/strings."""
    text = path.read_text(encoding="utf-8")
    match = re.search(rf"\bfn\s+{re.escape(function)}\s*\(", text)
    if match is None:
        return None
    start = text.find("{", match.end())
    if start < 0:
        return None

    depth = 0
    index = start
    state = "code"
    while index < len(text):
        char = text[index]
        following = text[index + 1] if index + 1 < len(text) else ""
        if state == "code":
            if char == '"':
                state = "string"
            elif char == "'" and following and following not in " ,)>]}":
                state = "char"
            elif char == "/" and following == "/":
                state = "line_comment"
                index += 1
            elif char == "/" and following == "*":
                state = "block_comment"
                index += 1
            elif char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    return text[start + 1 : index]
        elif state == "string":
            if char == "\\":
                index += 1
            elif char == '"':
                state = "code"
        elif state == "char":
            if char == "\\":
                index += 1
            elif char == "'":
                state = "code"
        elif state == "line_comment":
            if char == "\n":
                state = "code"
        elif state == "block_comment" and char == "*" and following == "/":
            state = "code"
            index += 1
        index += 1
    return None


def validate_route(route: object, label: str, errors: list[str]) -> None:
    if not isinstance(route, list) or not route:
        errors.append(f"{label}: route must be a non-empty array")
        return
    parsed_steps: list[tuple[str, list[str]]] = []
    for index, step in enumerate(route):
        step_label = f"{label}.route[{index}]"
        if not isinstance(step, dict):
            errors.append(f"{step_label}: expected an object")
            continue
        path_value = step.get("path")
        function = step.get("function")
        calls = step.get("calls")
        if (
            not isinstance(path_value, str)
            or not isinstance(function, str)
            or not isinstance(calls, list)
            or not calls
            or not all(isinstance(call, str) for call in calls)
        ):
            errors.append(
                f"{step_label}: path/function and a non-empty calls array are required"
            )
            continue
        path = ROOT / path_value
        if not path.is_file():
            errors.append(f"{step_label}: missing {path_value}")
            continue
        body = rust_function_body(path, function)
        if body is None:
            errors.append(f"{step_label}: fn {function}( not found or not parseable")
            continue
        for call in calls:
            if call not in body:
                errors.append(
                    f"{step_label}: {call!r} is not called by fn {function} in {path_value}"
                )
        parsed_steps.append((function, calls))
    for index, (_, calls) in enumerate(parsed_steps[:-1]):
        next_function = parsed_steps[index + 1][0]
        if f"{next_function}(" not in calls:
            errors.append(
                f"{label}.route[{index}] must explicitly call next route function "
                f"{next_function}("
            )
    if parsed_steps and not (set(parsed_steps[-1][1]) & TERMINAL_CALLS):
        errors.append(
            f"{label}: final route step must call one of {sorted(TERMINAL_CALLS)}"
        )


def validate_test(reference: object, label: str, errors: list[str]) -> None:
    if not isinstance(reference, dict):
        errors.append(f"{label}: expected an object")
        return
    path_value = reference.get("path")
    symbol = reference.get("symbol")
    if not isinstance(path_value, str) or not isinstance(symbol, str):
        errors.append(f"{label}: path and symbol must be strings")
        return
    path = ROOT / path_value
    if not path.is_file() or symbol not in path.read_text(encoding="utf-8"):
        errors.append(f"{label}: {symbol!r} not found in {path_value}")


def main() -> int:
    inventory = load_json(INVENTORY)
    contract = load_json(CONTRACT)
    public_functions = set(contract["public_sql_functions"])
    if public_functions - implemented_functions():
        print("release public SQL inventory is not implemented", file=sys.stderr)
        return 1

    errors: list[str] = []
    partition = inventory.get("public_sql_partition")
    if not isinstance(partition, dict):
        print("public_sql_partition must be an object", file=sys.stderr)
        return 1

    classified: set[str] = set()
    for key in PARTITION_KEYS:
        names = partition.get(key)
        if not isinstance(names, list) or not all(isinstance(name, str) for name in names):
            errors.append(f"public_sql_partition.{key} must be a string array")
            continue
        name_set = set(names)
        if len(name_set) != len(names):
            errors.append(f"public_sql_partition.{key} contains duplicates")
        overlap = classified & name_set
        if overlap:
            errors.append(f"public SQL classification overlap: {sorted(overlap)}")
        classified |= name_set
    if classified != public_functions:
        errors.append(
            "public SQL partition differs from the release contract: "
            f"missing={sorted(public_functions - classified)}, "
            f"stale={sorted(classified - public_functions)}"
        )

    required = set(partition.get("topology_visibility_required", []))
    covered: set[str] = set()
    entries = inventory.get("entrypoints")
    if not isinstance(entries, list):
        errors.append("entrypoints must be an array")
        entries = []
    rust_entries: set[tuple[str, str, str]] = set()
    for index, entry in enumerate(entries):
        label = f"entrypoints[{index}]"
        if not isinstance(entry, dict):
            errors.append(f"{label}: expected an object")
            continue
        sql_name = entry.get("sql_name")
        rust_function = entry.get("rust_function")
        if not isinstance(sql_name, str) or sql_name not in required:
            errors.append(f"{label}.sql_name must name a required topology function")
            continue
        if not isinstance(rust_function, str) or not rust_function:
            errors.append(f"{label}.rust_function must be a non-empty string")
            continue
        route = entry.get("route")
        first_step = route[0] if isinstance(route, list) and route else None
        first_function = first_step.get("function") if isinstance(first_step, dict) else None
        first_path = first_step.get("path") if isinstance(first_step, dict) else None
        if first_function != rust_function:
            errors.append(
                f"{label}: rust_function must equal the first route function"
            )
        if not isinstance(first_path, str):
            first_path = ""
        identity = (sql_name, rust_function, first_path)
        if identity in rust_entries:
            errors.append(f"duplicate topology Rust entrypoint: {identity}")
        rust_entries.add(identity)
        covered.add(sql_name)
        if entry.get("strategy") not in STRATEGIES:
            errors.append(f"{label}.strategy must be one of {sorted(STRATEGIES)}")
        validate_route(route, label, errors)
        validate_test(entry.get("semantic_evidence"), f"{label}.semantic_evidence", errors)

    if covered != required:
        errors.append(
            "topology entrypoints differ from required topology functions: "
            f"missing={sorted(required - covered)}, stale={sorted(covered - required)}"
        )
    actual_rust_entries = topology_pg_extern_entrypoints(required)
    if rust_entries != actual_rust_entries:
        errors.append(
            "declared topology Rust entrypoints differ from #[pg_extern]: "
            f"missing={sorted(actual_rust_entries - rust_entries)}, "
            f"stale={sorted(rust_entries - actual_rust_entries)}"
        )
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(
        "Topology SQL surface inventory is in sync; each declared Rust entry point "
        "has a body-scoped route to visibility composition. Rust context types "
        "and semantic PostgreSQL tests remain the execution-boundary proof."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
