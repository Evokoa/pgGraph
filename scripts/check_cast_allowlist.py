#!/usr/bin/env python3
"""Reject drift in reviewed Clippy narrowing and sign-loss diagnostics."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GRAPH = ROOT / "graph"
ALLOWLIST = ROOT / "release" / "cast-allowlist.json"
CODES = {"clippy::cast_possible_truncation", "clippy::cast_sign_loss"}


def diagnostics() -> list[dict[str, str]]:
    command = [
        "cargo",
        "clippy",
        "--lib",
        "--features",
        "pg17 development",
        "--message-format=json",
        "--",
        "-W",
        "clippy::cast_possible_truncation",
        "-W",
        "clippy::cast_sign_loss",
    ]
    process = subprocess.run(command, cwd=GRAPH, capture_output=True, text=True, check=False)
    if process.stderr:
        print(process.stderr, file=sys.stderr, end="")
    if process.returncode != 0:
        raise SystemExit(process.returncode)

    found: list[dict[str, str]] = []
    for line in process.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("reason") != "compiler-message":
            continue
        message = event.get("message", {})
        code = (message.get("code") or {}).get("code")
        if code not in CODES:
            continue
        spans = [span for span in message.get("spans", []) if span.get("is_primary")]
        if not spans:
            continue
        span = spans[0]
        snippets = "\n".join(item.get("text", "").strip() for item in span.get("text", []))
        found.append(
            {
                "code": code,
                "path": span.get("file_name", ""),
                "source": snippets,
                "message": message.get("message", ""),
            }
        )
    return sorted(found, key=lambda item: (item["path"], item["source"], item["code"], item["message"]))


def fingerprint(items: list[dict[str, str]]) -> str:
    payload = json.dumps(items, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write",
        action="store_true",
        help="Refresh the reviewed baseline after inspecting every changed diagnostic.",
    )
    args = parser.parse_args()
    items = diagnostics()
    current = {
        "schema_version": 1,
        "policy": (
            "The Rust 1.96 Clippy narrowing/sign-loss diagnostic set is frozen for 1.0. "
            "New or changed diagnostics require an explicit conversion review."
        ),
        "diagnostic_count": len(items),
        "diagnostic_sha256": fingerprint(items),
    }
    if args.write:
        ALLOWLIST.write_text(json.dumps(current, indent=2) + "\n", encoding="utf-8")
        print(f"Wrote {ALLOWLIST.relative_to(ROOT)} with {len(items)} reviewed diagnostics")
        return 0

    expected = json.loads(ALLOWLIST.read_text(encoding="utf-8"))
    if expected != current:
        print("Reviewed cast diagnostic baseline changed.", file=sys.stderr)
        print(f"Expected: {json.dumps(expected, sort_keys=True)}", file=sys.stderr)
        print(f"Actual:   {json.dumps(current, sort_keys=True)}", file=sys.stderr)
        print("Run with --write only after reviewing the complete Clippy diagnostic diff.", file=sys.stderr)
        return 1
    print(
        f"Checked-cast allowlist passed for {current['diagnostic_count']} "
        "reviewed Clippy diagnostics"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
