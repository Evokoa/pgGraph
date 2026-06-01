#!/usr/bin/env python3
"""Check pinned dependency updates without applying them by default.

The checker intentionally recommends only releases that have passed the age
gate. Newer releases are flagged for review, but not recommended, so release
managers have time to catch supply-chain incidents before adopting fresh
artifacts. Fresh package-manager fetches or installs must still go through sfw.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MIN_AGE_HOURS = 6


@dataclass(frozen=True)
class Dependency:
    ecosystem: str
    name: str
    current: str
    file: Path
    update_supported: bool
    owner: str | None = None
    repo: str | None = None
    ref: str | None = None

    @property
    def key(self) -> str:
        return f"{self.ecosystem}:{self.name}"


@dataclass(frozen=True)
class Release:
    version: str
    released_at: dt.datetime | None


def utc_now() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc)


def parse_timestamp(raw: str | None) -> dt.datetime | None:
    if not raw:
        return None
    normalized = raw.replace("Z", "+00:00")
    try:
        value = dt.datetime.fromisoformat(normalized)
    except ValueError:
        return None
    if value.tzinfo is None:
        return value.replace(tzinfo=dt.timezone.utc)
    return value.astimezone(dt.timezone.utc)


def version_key(version: str) -> tuple:
    numbers = tuple(int(part) for part in re.findall(r"\d+", version))
    return numbers or (0,)


def is_prerelease(version: str) -> bool:
    return bool(re.search(r"(?i)(alpha|beta|rc|dev|pre|[ab]\d)", version))


def request_json(url: str) -> dict:
    request = urllib.request.Request(url, headers={"User-Agent": "pggraph-dependency-audit"})
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.loads(response.read().decode("utf-8"))


def cargo_dependencies() -> list[Dependency]:
    manifests = [ROOT / "graph/Cargo.toml", ROOT / "graph/fuzz/Cargo.toml"]
    deps: list[Dependency] = []
    dep_line = re.compile(r'^([A-Za-z0-9_-]+)\s*=\s*(.+)$')
    version_field = re.compile(r'version\s*=\s*"=([^"]+)"')
    quoted_exact = re.compile(r'"=([^"]+)"')
    in_deps = False
    for manifest in manifests:
        for line in manifest.read_text().splitlines():
            stripped = line.strip()
            if stripped.startswith("["):
                in_deps = stripped in {"[dependencies]", "[dev-dependencies]"}
                continue
            if not in_deps or not stripped or stripped.startswith("#"):
                continue
            match = dep_line.match(stripped)
            if not match:
                continue
            name, spec = match.groups()
            version_match = version_field.search(spec) or quoted_exact.search(spec)
            if version_match:
                deps.append(Dependency("cargo", name, version_match.group(1), manifest, True))
    return deps


def python_dependencies() -> list[Dependency]:
    requirements = [
        ROOT / "sandbox/playground/requirements.txt",
        ROOT / "sandbox/benchmark/requirements.txt",
    ]
    deps: list[Dependency] = []
    pattern = re.compile(r"^([A-Za-z0-9_.-]+)(?:\[[^\]]+\])?==([^;\s]+)")
    for req in requirements:
        for line in req.read_text().splitlines():
            match = pattern.match(line.strip())
            if match:
                deps.append(Dependency("pypi", match.group(1), match.group(2), req, True))
    return deps


def docker_dependencies() -> list[Dependency]:
    dockerfile = ROOT / "Dockerfile"
    deps: list[Dependency] = []
    patterns = [
        re.compile(r"^FROM\s+([^:\s$]+):([^@\s]+)"),
        re.compile(r"^ARG\s+[A-Za-z_][A-Za-z0-9_]*=([^:\s]+):([^@\s]+)"),
    ]
    for line in dockerfile.read_text().splitlines():
        for pattern in patterns:
            match = pattern.match(line.strip())
            if not match:
                continue
            image, tag = match.groups()
            # Docker updates need tag/digest review, so this script reports but
            # does not rewrite Dockerfile base image references automatically.
            deps.append(Dependency("docker", image, tag, dockerfile, False))
            break
    return deps


def flake_dependencies() -> list[Dependency]:
    lockfile = ROOT / "flake.lock"
    if not lockfile.exists():
        return []

    data = json.loads(lockfile.read_text())
    deps: list[Dependency] = []
    for name, node in data.get("nodes", {}).items():
        locked = node.get("locked", {})
        original = node.get("original", {})
        if locked.get("type") != "github" or "rev" not in locked:
            continue
        owner = locked.get("owner")
        repo = locked.get("repo")
        if not owner or not repo:
            continue
        deps.append(
            Dependency(
                "github",
                name,
                locked["rev"],
                lockfile,
                False,
                owner=owner,
                repo=repo,
                ref=original.get("ref"),
            )
        )
    return deps


def cargo_releases(name: str) -> list[Release]:
    data = request_json(f"https://crates.io/api/v1/crates/{name}")
    releases = []
    for item in data.get("versions", []):
        if item.get("yanked"):
            continue
        releases.append(Release(item["num"], parse_timestamp(item.get("created_at"))))
    return releases


def pypi_releases(name: str) -> list[Release]:
    data = request_json(f"https://pypi.org/pypi/{name}/json")
    releases = []
    for version, files in data.get("releases", {}).items():
        timestamps = [parse_timestamp(file.get("upload_time_iso_8601")) for file in files]
        timestamps = [value for value in timestamps if value is not None]
        releases.append(Release(version, min(timestamps) if timestamps else None))
    return releases


def docker_releases(image: str) -> list[Release]:
    namespace = "library"
    repo = image
    if "/" in image:
        namespace, repo = image.split("/", 1)
    data = request_json(f"https://hub.docker.com/v2/repositories/{namespace}/{repo}/tags?page_size=100")
    releases = []
    for item in data.get("results", []):
        releases.append(Release(item["name"], parse_timestamp(item.get("last_updated"))))
    return releases


def github_releases(dep: Dependency) -> list[Release]:
    if not dep.owner or not dep.repo:
        raise ValueError(f"{dep.key} is missing GitHub owner/repo metadata")
    ref = dep.ref
    if ref is None:
        repo_data = request_json(f"https://api.github.com/repos/{dep.owner}/{dep.repo}")
        ref = repo_data.get("default_branch")
    if not ref:
        raise ValueError(f"{dep.key} is missing a GitHub ref")
    data = request_json(f"https://api.github.com/repos/{dep.owner}/{dep.repo}/commits/{ref}")
    committed_at = data.get("commit", {}).get("committer", {}).get("date")
    return [Release(data["sha"], parse_timestamp(committed_at))]


def releases_for(dep: Dependency) -> list[Release]:
    if dep.ecosystem == "cargo":
        return cargo_releases(dep.name)
    if dep.ecosystem == "pypi":
        return pypi_releases(dep.name)
    if dep.ecosystem == "docker":
        return docker_releases(dep.name)
    if dep.ecosystem == "github":
        return github_releases(dep)
    raise ValueError(dep.ecosystem)


def docker_tag_matches_current_family(current: str, candidate: str) -> bool:
    current_parts = current.split("-")
    candidate_parts = candidate.split("-")
    if len(current_parts) < 2 or len(candidate_parts) < 2:
        return candidate == current
    current_version = current_parts[0]
    current_suffix = "-".join(current_parts[1:])
    candidate_version = candidate_parts[0]
    candidate_suffix = "-".join(candidate_parts[1:])
    if candidate_suffix != current_suffix:
        return False
    # PostgreSQL images are pinned by major.minor; keep the same major.
    if current_version.count(".") == 1:
        return candidate_version.split(".")[0] == current_version.split(".")[0]
    # Rust images are pinned by full compiler version; avoid crossing compiler
    # versions without an explicit toolchain review.
    return candidate_version == current_version


def choose_releases(
    dep: Dependency, releases: Iterable[Release], cutoff: dt.datetime
) -> tuple[Release | None, Release | None]:
    candidates = list(releases)
    if dep.ecosystem in {"cargo", "pypi"} and not is_prerelease(dep.current):
        candidates = [release for release in candidates if not is_prerelease(release.version)]
    if dep.ecosystem == "docker":
        candidates = [
            release
            for release in candidates
            if docker_tag_matches_current_family(dep.current, release.version)
        ]
    stable = [
        release for release in candidates if release.released_at and release.released_at <= cutoff
    ]
    all_known = [release for release in candidates if release.released_at]
    recommended = max(stable, key=lambda item: version_key(item.version), default=None)
    latest = max(all_known, key=lambda item: version_key(item.version), default=None)
    return recommended, latest


def rewrite_dependency(dep: Dependency, target_version: str) -> None:
    text = dep.file.read_text()
    if dep.ecosystem == "cargo":
        text = re.sub(
            rf'({re.escape(dep.name)}\s*=\s*"=)[^"]+(")',
            rf"\g<1>{target_version}\2",
            text,
        )
        text = re.sub(
            rf'({re.escape(dep.name)}\s*=\s*\{{[^}}]*version\s*=\s*"=)[^"]+(")',
            rf"\g<1>{target_version}\2",
            text,
        )
    elif dep.ecosystem == "pypi":
        text = re.sub(
            rf"^({re.escape(dep.name)}(?:\[[^\]]+\])?==)[^;\s]+",
            rf"\g<1>{target_version}",
            text,
            flags=re.MULTILINE,
        )
    else:
        raise ValueError(f"automatic updates are not supported for {dep.key}")
    dep.file.write_text(text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--min-age-hours",
        type=int,
        default=DEFAULT_MIN_AGE_HOURS,
        help="Minimum release age before recommending updates. Defaults to 6.",
    )
    parser.add_argument(
        "--min-age-days",
        type=int,
        default=None,
        help="Compatibility override for the release age gate, in days.",
    )
    parser.add_argument(
        "--update",
        action="append",
        default=[],
        metavar="ECOSYSTEM:NAME",
        help="Update one pinned dependency to the recommended version.",
    )
    parser.add_argument("--yes", action="store_true", help="Required with --update.")
    args = parser.parse_args()

    min_age = (
        dt.timedelta(days=args.min_age_days)
        if args.min_age_days is not None
        else dt.timedelta(hours=args.min_age_hours)
    )
    age_label = (
        f"{args.min_age_days} days"
        if args.min_age_days is not None
        else f"{args.min_age_hours} hours"
    )
    cutoff = utc_now() - min_age
    deps = (
        cargo_dependencies()
        + python_dependencies()
        + docker_dependencies()
        + flake_dependencies()
    )
    updates = set(args.update)
    if updates and not args.yes:
        print("--update requires --yes; no files changed", file=sys.stderr)
        return 2

    had_review_items = False
    for dep in deps:
        try:
            recommended, latest = choose_releases(dep, releases_for(dep), cutoff)
        except (urllib.error.URLError, TimeoutError, ValueError) as exc:
            had_review_items = True
            print(f"REVIEW {dep.key}: lookup failed: {exc}")
            continue

        if recommended is None:
            had_review_items = True
            print(f"REVIEW {dep.key}: no release at least {age_label} old found")
            continue

        latest_note = ""
        if latest and latest.version != recommended.version:
            latest_note = f"; latest {latest.version} is newer than the age gate"
            had_review_items = True

        needs_update = (
            recommended.version != dep.current
            if dep.ecosystem == "github"
            else version_key(recommended.version) > version_key(dep.current)
        )
        if needs_update:
            had_review_items = True
            print(
                f"UPDATE {dep.key}: {dep.current} -> {recommended.version} "
                f"(released {recommended.released_at.date()}){latest_note}"
            )
            if dep.key in updates:
                if not dep.update_supported:
                    print(f"  not changed: {dep.key} requires manual digest/package review")
                else:
                    rewrite_dependency(dep, recommended.version)
                    print(f"  updated {dep.file.relative_to(ROOT)}")
        else:
            print(f"OK {dep.key}: pinned {dep.current}; recommended {recommended.version}{latest_note}")

    unknown = updates - {dep.key for dep in deps}
    for key in sorted(unknown):
        had_review_items = True
        print(f"REVIEW {key}: not found in pinned manifests")

    return 1 if had_review_items else 0


if __name__ == "__main__":
    raise SystemExit(main())
