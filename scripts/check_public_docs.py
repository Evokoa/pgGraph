#!/usr/bin/env python3
"""Validate public documentation maturity, navigation, examples, and README parity."""

from __future__ import annotations

import hashlib
import importlib
import importlib.util
import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PUBLIC_DOCS = [
    ROOT / "README.md",
    ROOT / "README_zh.md",
    ROOT / "docs" / "index.mdx",
    ROOT / "docs" / "quickstart.mdx",
    ROOT / "docs" / "known-issues.mdx",
    ROOT / "docs" / "roadmap.mdx",
    ROOT / "docs" / "release-notes.mdx",
]
PUBLIC_DOCS.extend((ROOT / "docs" / "user_guide").glob("*.mdx"))
FORBIDDEN_PUBLIC_PHRASES = {
    r"\bPhase [0-9][A-Za-z]?\b": "internal phase name",
    r"\blater phase\b": "internal future-phase wording",
    r"(?:^|[\s`])todo/": "private todo path",
}


def cargo_version() -> str:
    text = (ROOT / "graph" / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if not match:
        raise RuntimeError("graph/Cargo.toml package version is missing")
    return match.group(1)


def navigation_failures() -> list[str]:
    failures: list[str] = []
    for directory in (ROOT / "docs", ROOT / "docs" / "user_guide", ROOT / "docs" / "contributor_guide"):
        meta = directory / "_meta.js"
        text = meta.read_text(encoding="utf-8")
        keys = []
        for line in text.splitlines():
            match = re.match(r"\s*(?:'([^']+)'|([A-Za-z_][\w-]*))\s*:", line)
            if match:
                key = match.group(1) or match.group(2)
                if key != "title":
                    keys.append(key)
        for key in keys:
            if not ((directory / f"{key}.mdx").is_file() or (directory / key).is_dir()):
                failures.append(f"{meta.relative_to(ROOT)} references missing route {key!r}")
        visible = {path.stem for path in directory.glob("*.mdx") if not path.name.startswith("_")}
        missing = visible - set(keys)
        if missing:
            failures.append(
                f"{directory.relative_to(ROOT)} has orphan pages: {', '.join(sorted(missing))}"
            )
    return failures


def style_failures() -> list[str]:
    failures: list[str] = []
    for path in PUBLIC_DOCS:
        text = path.read_text(encoding="utf-8")
        for pattern, description in FORBIDDEN_PUBLIC_PHRASES.items():
            for match in re.finditer(pattern, text, re.IGNORECASE | re.MULTILINE):
                line = text.count("\n", 0, match.start()) + 1
                failures.append(f"{path.relative_to(ROOT)}:{line}: {description}")
        for line_no, line in enumerate(text.splitlines(), start=1):
            if line.rstrip() != line:
                failures.append(f"{path.relative_to(ROOT)}:{line_no}: trailing whitespace")
    return failures


def readme_failures(version: str, maturity: dict) -> list[str]:
    failures: list[str] = []
    files = {
        "README.md": (ROOT / "README.md").read_text(encoding="utf-8"),
        "README_zh.md": (ROOT / "README_zh.md").read_text(encoding="utf-8"),
        "docs/quickstart.mdx": (ROOT / "docs" / "quickstart.mdx").read_text(encoding="utf-8"),
        "docs/user_guide/installation.mdx": (
            ROOT / "docs" / "user_guide" / "installation.mdx"
        ).read_text(encoding="utf-8"),
        "docs/index.mdx": (ROOT / "docs" / "index.mdx").read_text(encoding="utf-8"),
    }
    required_version_files = ("README.md", "README_zh.md", "docs/user_guide/installation.mdx", "docs/index.mdx")
    for name in required_version_files:
        if version not in files[name]:
            failures.append(f"{name} does not mention current version {version}")
    exact_version_markers = {
        "README.md": (f"version-{version}",),
        "README_zh.md": (f"version-{version}", f"`{version}`"),
        "docs/quickstart.mdx": (f"`{version}`",),
        "docs/user_guide/installation.mdx": (version,),
        "docs/index.mdx": (f'value="{version}',),
    }
    if maturity["state"] == "published":
        exact_version_markers["README.md"] += (
            f"ghcr.io/evokoa/pggraph:{version}",
            f"installs pgGraph {version}",
        )
        exact_version_markers["README_zh.md"] += (f"ghcr.io/evokoa/pggraph:{version}",)
        exact_version_markers["docs/user_guide/installation.mdx"] += (
            f"installs pgGraph {version}",
            f"ghcr.io/evokoa/pggraph:{version}",
            f"`pg14-{version}` through `pg18-{version}`",
        )
    for name, markers in exact_version_markers.items():
        for marker in markers:
            if marker not in files[name]:
                failures.append(f"{name} is missing current-version marker {marker!r}")
    shared_tokens = (
        "scripts/quickstart.sh",
        "scripts/quickstart.sh playground panama csr",
        "scripts/quickstart.sh playground panama mutable",
        "ghcr.io/evokoa/pggraph",
    )
    for token in shared_tokens:
        for name in ("README.md", "README_zh.md"):
            if token not in files[name]:
                failures.append(f"{name} is missing shared command {token!r}")
    for name in ("README.md", "README_zh.md", "docs/quickstart.mdx"):
        if not re.search(r"PostgreSQL 14\s+(?:through|to)\s+18|PostgreSQL 14[–-]18", files[name]):
            failures.append(f"{name} does not state PostgreSQL 14-18 support")
    if "README_SYNC_POLICY" not in files["README_zh.md"]:
        failures.append("README_zh.md does not declare its synchronization policy")
    maturity_phrases = {
        "alpha": "alpha",
        "candidate": "not yet tagged or published",
    }
    if maturity["state"] not in (*maturity_phrases, "release_ready", "published"):
        failures.append(f"release/maturity.json has unknown state {maturity['state']!r}")
        return failures
    localized_maturity_phrases = {
        "README_zh.md": {
            "alpha": "alpha",
            "candidate": "尚未创建 tag 或发布",
        }
    }
    if maturity["state"] in maturity_phrases:
        for name in ("README.md", "README_zh.md", "docs/index.mdx", "docs/known-issues.mdx"):
            maturity_phrase = localized_maturity_phrases.get(name, {}).get(
                maturity["state"], maturity_phrases[maturity["state"]]
            )
            if maturity_phrase.lower() not in files.get(name, (ROOT / name).read_text(encoding="utf-8")).lower():
                failures.append(f"{name} does not reflect maturity state {maturity['state']!r}")
    if maturity["state"] in {"alpha", "candidate"}:
        forbidden_availability = {
            "README.md": ("The fastest way to try pgGraph is to pull the pre-built Docker image", "pgGraph is available on PGXN"),
            "README_zh.md": ("pgGraph 在 PGXN 上以源码分发包的形式提供",),
            "docs/quickstart.mdx": ("Docker release images are published for PostgreSQL 14 through 18",),
            "docs/user_guide/installation.mdx": (
                f"installs pgGraph {version}",
                "pgGraph is available on PGXN",
                "A pre-built image is published on GitHub Container Registry for each release",
            ),
        }
        for name, phrases in forbidden_availability.items():
            for phrase in phrases:
                if phrase in files[name]:
                    failures.append(f"{name} claims unavailable candidate distribution: {phrase!r}")
    return failures


def example_failures() -> list[str]:
    failures: list[str] = []
    registry = json.loads((ROOT / "release" / "examples.json").read_text(encoding="utf-8"))
    script_inventory = json.loads((ROOT / "release" / "scripts.json").read_text(encoding="utf-8"))
    maintained_scripts = {item["path"] for item in script_inventory["scripts"]}
    ids: set[str] = set()
    required = {"id", "owner", "content_paths", "version_requirement", "expected_contract", "reset_strategy", "smoke_test"}
    for collection in registry.get("collections", []):
        if set(collection) != required:
            failures.append(f"example collection fields differ for {collection.get('id')!r}")
            continue
        if collection["id"] in ids:
            failures.append(f"duplicate example collection id {collection['id']!r}")
        ids.add(collection["id"])
        for field in required - {"content_paths"}:
            if not isinstance(collection[field], str) or not collection[field].strip():
                failures.append(f"example {collection['id']} has empty {field}")
        for raw_path in collection["content_paths"]:
            if not (ROOT / raw_path).is_file():
                failures.append(f"example {collection['id']} references missing {raw_path}")
        if collection["smoke_test"] not in maintained_scripts:
            failures.append(f"example {collection['id']} smoke test is not maintained")

    playground = ROOT / "sandbox" / "playground"
    sys.path.insert(0, str(playground))
    module = importlib.import_module("catalog")
    examples = module.query_examples("mutable")
    example_ids: set[str] = set()
    for example in examples:
        if example.id in example_ids:
            failures.append(f"duplicate playground example id {example.id!r}")
        example_ids.add(example.id)
        checksum = hashlib.sha256("\0".join(example.statements).encode("utf-8")).hexdigest()
        if example.statement_checksum != checksum:
            failures.append(f"playground example {example.id} checksum drift")
        for field in ("owner", "minimum_pggraph_version", "reset_strategy", "smoke_test"):
            if not getattr(example, field):
                failures.append(f"playground example {example.id} has empty {field}")
        if not (ROOT / example.smoke_test).is_file():
            failures.append(f"playground example {example.id} smoke test is missing")

    gate_path = ROOT / "graph" / "tests" / "heavy" / "playground_release_gate.py"
    spec = importlib.util.spec_from_file_location("playground_release_gate", gate_path)
    if spec is None or spec.loader is None:
        failures.append("playground result gate cannot be loaded")
        return failures
    gate = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gate)
    for mode, expected in (
        ("csr", gate.EXPECTED_RESULTS_CSR),
        ("mutable", gate.EXPECTED_RESULTS_MUTABLE),
    ):
        catalog = module.query_catalog(mode)
        if set(catalog) != set(expected):
            failures.append(f"playground {mode} expected-output IDs differ from the catalog")
        for title, summaries in expected.items():
            if not summaries or any(
                not isinstance(summary.get("row_count"), int) or summary["row_count"] < 0
                for summary in summaries
            ):
                failures.append(f"playground {mode} example {title!r} lacks expected output")
    return failures


def internal_api_failures() -> list[str]:
    contract = json.loads((ROOT / "release" / "v1-contract.json").read_text(encoding="utf-8"))
    internal = contract["internal_sql_functions"]
    failures: list[str] = []
    sources = [ROOT / "README.md", ROOT / "README_zh.md", *PUBLIC_DOCS]
    for path in sources:
        text = path.read_text(encoding="utf-8")
        for function in internal:
            marker = f"graph.{function}"
            if marker in text:
                failures.append(
                    f"{path.relative_to(ROOT)} exposes internal SQL function {marker}"
                )
    return failures


def main() -> int:
    maturity = json.loads((ROOT / "release" / "maturity.json").read_text(encoding="utf-8"))
    failures = []
    failures.extend(navigation_failures())
    failures.extend(style_failures())
    failures.extend(readme_failures(cargo_version(), maturity))
    failures.extend(example_failures())
    failures.extend(internal_api_failures())
    if failures:
        print("Public documentation contract drift:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print("Public documentation maturity, navigation, style, README, and examples are in sync.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
