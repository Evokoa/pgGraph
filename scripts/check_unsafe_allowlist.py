#!/usr/bin/env python3
"""Reject unowned Rust unsafe blocks or blocks without a SAFETY explanation."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST = ROOT / "release" / "unsafe-allowlist.json"
UNSAFE_BLOCK = re.compile(r"\bunsafe\s*\{")


def main() -> int:
    document = json.loads(ALLOWLIST.read_text(encoding="utf-8"))
    expected = {entry["path"]: entry for entry in document["entries"]}
    actual: dict[str, list[int]] = {}
    errors: list[str] = []
    for path in sorted((ROOT / "graph" / "src").rglob("*.rs")):
        lines = path.read_text(encoding="utf-8").splitlines()
        locations = [index + 1 for index, line in enumerate(lines) if UNSAFE_BLOCK.search(line)]
        if not locations:
            continue
        relative = path.relative_to(ROOT).as_posix()
        actual[relative] = locations
        for line_number in locations:
            context = "\n".join(lines[max(0, line_number - 6) : min(len(lines), line_number + 4)])
            if "SAFETY:" not in context:
                errors.append(f"{relative}:{line_number}: unsafe block lacks a nearby SAFETY explanation")
    if set(actual) != set(expected):
        errors.append(f"unsafe file allowlist differs: unlisted={sorted(set(actual) - set(expected))}, stale={sorted(set(expected) - set(actual))}")
    for path, locations in actual.items():
        if path in expected and len(locations) != expected[path]["blocks"]:
            errors.append(f"{path}: expected {expected[path]['blocks']} unsafe blocks, found {len(locations)}")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Rust unsafe allowlist passed for {sum(map(len, actual.values()))} blocks in {len(actual)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
