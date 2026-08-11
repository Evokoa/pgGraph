#!/usr/bin/env python3
"""Check exact sandbox requirement pins without resolving or installing packages."""

from __future__ import annotations

import importlib.metadata
import re
import sys
from pathlib import Path


REQUIREMENT = re.compile(
    r"^(?P<name>[A-Za-z0-9_.-]+)"
    r"(?:\[(?P<extras>[A-Za-z0-9_.,-]+)\])?"
    r"==(?P<version>[^;\s]+)"
    r"(?:\s*;\s*(?P<marker>.+))?$"
)
PYTHON_MARKER = re.compile(
    r"^python_version\s*(?P<operator>==|!=|<=|>=|<|>)\s*"
    r"['\"](?P<version>\d+\.\d+)['\"]$"
)
EXTRA_DISTRIBUTIONS = {
    ("psycopg", "binary"): "psycopg-binary",
}


def normalized_distribution(name: str) -> str:
    """Normalize a distribution name using Python packaging name rules."""

    return re.sub(r"[-_.]+", "-", name).lower()


def version_tuple(value: str) -> tuple[int, ...]:
    """Convert the supported numeric Python marker value to a tuple."""

    return tuple(int(part) for part in value.split("."))


def marker_applies(marker: str | None) -> bool:
    """Evaluate the small marker grammar used by the checked-in requirements."""

    if marker is None:
        return True
    match = PYTHON_MARKER.fullmatch(marker.strip())
    if match is None:
        raise ValueError(f"unsupported environment marker: {marker}")

    current = sys.version_info[:2]
    expected = version_tuple(match.group("version"))
    return {
        "==": current == expected,
        "!=": current != expected,
        "<=": current <= expected,
        ">=": current >= expected,
        "<": current < expected,
        ">": current > expected,
    }[match.group("operator")]


def installed_version(distribution: str) -> str | None:
    """Return an installed version without invoking a package resolver."""

    try:
        return importlib.metadata.version(distribution)
    except importlib.metadata.PackageNotFoundError:
        return None


def check_requirement(line: str, line_number: int) -> list[str]:
    """Return validation errors for one active exact-pin requirement."""

    match = REQUIREMENT.fullmatch(line)
    if match is None:
        return [f"line {line_number}: unsupported requirement: {line}"]
    try:
        applies = marker_applies(match.group("marker"))
    except ValueError as error:
        return [f"line {line_number}: {error}"]
    if not applies:
        return []

    name = normalized_distribution(match.group("name"))
    expected = match.group("version")
    errors: list[str] = []
    actual = installed_version(name)
    if actual is None:
        errors.append(f"line {line_number}: {name} is not installed")
    elif actual != expected:
        errors.append(
            f"line {line_number}: {name} has {actual}, expected {expected}"
        )

    extras = match.group("extras")
    if extras:
        for extra in extras.split(","):
            extra_distribution = EXTRA_DISTRIBUTIONS.get((name, extra))
            if extra_distribution is None:
                errors.append(
                    f"line {line_number}: unsupported extra: {name}[{extra}]"
                )
                continue
            extra_actual = installed_version(extra_distribution)
            if extra_actual is None:
                errors.append(
                    f"line {line_number}: extra distribution "
                    f"{extra_distribution} is not installed"
                )
            elif extra_actual != expected:
                errors.append(
                    f"line {line_number}: {extra_distribution} has "
                    f"{extra_actual}, expected {expected}"
                )
    return errors


def requirements_errors(path: Path) -> list[str]:
    """Validate the exact, offline-checkable requirement subset used here."""

    errors: list[str] = []
    for line_number, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        errors.extend(check_requirement(line, line_number))
    return errors


def main() -> int:
    """Return success only when every active pin is already installed exactly."""

    if len(sys.argv) != 2:
        print("usage: requirements_satisfied.py REQUIREMENTS", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    if not path.is_file():
        print(f"requirements file does not exist: {path}", file=sys.stderr)
        return 2

    errors = requirements_errors(path)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
