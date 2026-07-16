#!/usr/bin/env python3
"""Check public documentation HTTP links with bounded retries and timeouts."""

from __future__ import annotations

import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SOURCES = [ROOT / "README.md", ROOT / "README_zh.md", *sorted((ROOT / "docs").rglob("*.mdx"))]
URL_RE = re.compile(r"https?://[^\s)>'\"]+")
SKIP_PREFIXES = (
    "https://img.shields.io/",
    "http://localhost",
    "https://localhost",
)


def reachable(url: str) -> tuple[bool, str]:
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "pgGraph-release-link-check/1.0", "Range": "bytes=0-1023"},
    )
    last = "unknown failure"
    for attempt in range(2):
        try:
            with urllib.request.urlopen(request, timeout=12) as response:
                return 200 <= response.status < 400, f"HTTP {response.status}"
        except urllib.error.HTTPError as error:
            if error.code in {401, 403, 405, 429}:
                return True, f"HTTP {error.code} (reachable but restricted)"
            last = f"HTTP {error.code}"
        except (urllib.error.URLError, TimeoutError) as error:
            last = str(error.reason if isinstance(error, urllib.error.URLError) else error)
        if attempt == 0:
            time.sleep(1)
    return False, last


def main() -> int:
    locations: dict[str, list[str]] = {}
    for path in SOURCES:
        for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            for match in URL_RE.finditer(line):
                url = match.group(0).rstrip("`.,;:")
                if url.startswith(SKIP_PREFIXES):
                    continue
                locations.setdefault(url, []).append(f"{path.relative_to(ROOT)}:{line_no}")
    failures = []
    for url in sorted(locations):
        ok, detail = reachable(url)
        print(f"{'ok' if ok else 'FAIL'} {url} ({detail})")
        if not ok:
            failures.append(f"{url}: {detail}; referenced by {', '.join(locations[url])}")
    if failures:
        print("External documentation link failures:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print(f"External documentation links passed for {len(locations)} unique URLs.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
