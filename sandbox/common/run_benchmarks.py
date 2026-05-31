#!/usr/bin/env python3
"""SQL-facing pgGraph benchmark and dataset preparation harness."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import platform
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
import zipfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from statistics import median


DOCKER_HELP = """If you need to install Docker, see:
  Mac:     https://docs.docker.com/desktop/setup/install/mac-install/
  Windows: https://docs.docker.com/desktop/setup/install/windows-install/
  Linux:   https://docs.docker.com/desktop/setup/install/linux/"""


@dataclass(frozen=True)
class DatasetSpec:
    key: str
    name: str
    url: str
    archive_name: str
    compressed_size: str
    uncompressed_size: str


@dataclass(frozen=True)
class WorkloadQuery:
    name: str
    question: str
    sql: str


DATASETS = {
    "panama": DatasetSpec(
        key="panama",
        name="Panama Papers / ICIJ Offshore Leaks",
        url="https://offshoreleaks-data.icij.org/offshoreleaks/csv/full-oldb.LATEST.zip",
        archive_name="full-oldb.LATEST.zip",
        compressed_size="73 MB",
        uncompressed_size="626 MB",
    ),
    "ldbc": DatasetSpec(
        key="ldbc",
        name="LDBC SNB Interactive SF1 CsvBasic LongDateFormatter",
        url="https://datasets.ldbcouncil.org/snb-interactive-v1/social_network-sf1-CsvBasic-LongDateFormatter.tar.zst",
        archive_name="social_network-sf1-CsvBasic-LongDateFormatter.tar.zst",
        compressed_size="230 MB",
        uncompressed_size="unknown",
    ),
}


def run(cmd: list[str], timeout: int | None = 30, **kwargs: object) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, text=True, capture_output=True, timeout=timeout, check=False, **kwargs)


def require_docker() -> None:
    if shutil.which("docker") is None:
        print("Error: Docker is not installed or not available on PATH.", file=sys.stderr)
        print(DOCKER_HELP, file=sys.stderr)
        raise SystemExit(1)

    info = run(["docker", "info"], timeout=20)
    if info.returncode != 0:
        print("Error: Cannot connect to the Docker daemon. Is Docker Desktop running?", file=sys.stderr)
        print(DOCKER_HELP, file=sys.stderr)
        raise SystemExit(1)


def psql(
    container: str,
    sql: str,
    *,
    timeout: int | None = None,
    tuples_only: bool = False,
    progress_label: str | None = None,
) -> str:
    cmd = [
        "docker",
        "exec",
        "-i",
        container,
        "psql",
        "-U",
        "postgres",
        "-d",
        "postgres",
        "-v",
        "ON_ERROR_STOP=1",
    ]
    if tuples_only:
        cmd.append("-At")
    if progress_label and timeout is None:
        process = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        assert process.stdin is not None
        process.stdin.write(sql)
        process.stdin.close()
        started = time.perf_counter()
        while process.poll() is None:
            elapsed = int(time.perf_counter() - started)
            print(f"{progress_label} still running ({elapsed}s)...", flush=True)
            time.sleep(30)
        stdout = process.stdout.read() if process.stdout is not None else ""
        stderr = process.stderr.read() if process.stderr is not None else ""
        completed = subprocess.CompletedProcess(cmd, process.returncode, stdout, stderr)
    else:
        completed = subprocess.run(cmd, input=sql, text=True, capture_output=True, timeout=timeout, check=False)
    if completed.returncode != 0:
        raise RuntimeError(completed.stderr.strip() or completed.stdout.strip())
    return completed.stdout


def docker_cp(src: Path, container: str, dest: str) -> None:
    completed = run(["docker", "cp", str(src), f"{container}:{dest}"], timeout=None)
    if completed.returncode != 0:
        raise RuntimeError(completed.stderr.strip() or completed.stdout.strip())


def restart_container(container: str) -> None:
    completed = run(["docker", "restart", container], timeout=120)
    if completed.returncode != 0:
        raise RuntimeError(completed.stderr.strip() or completed.stdout.strip())
    for _ in range(60):
        ready = run(["docker", "exec", container, "pg_isready", "-U", "postgres"], timeout=5)
        if ready.returncode == 0:
            return
        time.sleep(1)
    raise RuntimeError(f"PostgreSQL did not become ready after restarting {container}")


def cleanup_path(path: Path, dry_run: bool) -> None:
    if not path.exists():
        print(f"Already clean: {path}")
        return
    if dry_run:
        print(f"Would remove: {path}")
        return
    if path.is_dir():
        shutil.rmtree(path)
    else:
        path.unlink()
    print(f"Removed: {path}")


def cleanup_docker(container: str, image: str, remove_image: bool, dry_run: bool) -> None:
    if dry_run:
        print(f"Would inspect/remove Docker container: {container}")
        if remove_image:
            print(f"Would inspect/remove Docker image: {image}")
        return
    require_docker()
    inspect_container = run(["docker", "ps", "-a", "--format", "{{.Names}}"], timeout=30)
    if inspect_container.returncode == 0 and container in inspect_container.stdout.splitlines():
        if dry_run:
            print(f"Would remove Docker container: {container}")
        else:
            completed = run(["docker", "rm", "-f", container], timeout=120)
            if completed.returncode != 0:
                raise RuntimeError(completed.stderr.strip() or completed.stdout.strip())
            print(f"Removed Docker container: {container}")
    else:
        print(f"Already clean: Docker container {container}")

    if not remove_image:
        return
    inspect_image = run(["docker", "image", "inspect", image], timeout=30)
    if inspect_image.returncode != 0:
        print(f"Already clean: Docker image {image}")
        return
    if dry_run:
        print(f"Would remove Docker image: {image}")
        return
    completed = run(["docker", "rmi", image], timeout=120)
    if completed.returncode != 0:
        raise RuntimeError(completed.stderr.strip() or completed.stdout.strip())
    print(f"Removed Docker image: {image}")


def cleanup_generated(args: argparse.Namespace) -> int:
    targets = set(args.cleanup)
    if "all" in targets:
        targets.update({"datasets", "results", "venv", "docker"})
    if "datasets" in targets:
        cleanup_path(args.datasets_dir, args.dry_run)
    if "results" in targets:
        cleanup_path(args.results_dir, args.dry_run)
    if "venv" in targets:
        cleanup_path(args.benchmark_venv, args.dry_run)
        cleanup_path(args.playground_venv, args.dry_run)
    if "docker" in targets:
        cleanup_docker(args.container, args.image, args.remove_image, args.dry_run)
    return 0


def prompt_download(spec: DatasetSpec, archive_path: Path, yes: bool) -> None:
    if archive_path.exists():
        return
    message = (
        f"Download {spec.name}?\n"
        f"  URL: {spec.url}\n"
        f"  Compressed: {spec.compressed_size}\n"
        f"  Uncompressed: {spec.uncompressed_size}\n"
        f"  Destination: {archive_path}\n"
    )
    if yes:
        print(message + "Proceeding because --yes was provided.")
        return
    answer = input(message + "Proceed? [y/N] ").strip().lower()
    if answer not in {"y", "yes"}:
        raise SystemExit(f"Skipped {spec.key}; dataset download was not approved.")


def download(spec: DatasetSpec, dataset_dir: Path, yes: bool) -> Path:
    archive_path = dataset_dir / spec.archive_name
    prompt_download(spec, archive_path, yes)
    if archive_path.exists():
        return archive_path

    tmp_path = archive_path.with_suffix(archive_path.suffix + ".part")
    print(f"Downloading {spec.name} ({spec.compressed_size})...")
    request = urllib.request.Request(
        spec.url,
        headers={
            "User-Agent": "pggraph-sandbox/0.1 (+https://docs.evokoa.com/pggraph)",
            "Accept": "application/octet-stream,*/*",
        },
    )
    try:
        with urllib.request.urlopen(request) as response, tmp_path.open("wb") as out:
            shutil.copyfileobj(response, out)
    except Exception:
        if tmp_path.exists():
            tmp_path.unlink()
        raise
    tmp_path.replace(archive_path)
    return archive_path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def norm_name(value: str) -> str:
    return value.strip().strip(":").replace(".", "_").replace(" ", "_").lower()


def first_value(row: dict[str, str], *names: str) -> str:
    normalized = {norm_name(key): value for key, value in row.items()}
    for name in names:
        value = normalized.get(norm_name(name))
        if value is not None:
            return value.strip()
    return ""


def write_csv(path: Path, fieldnames: list[str], rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)


def csv_writer(path: Path, fieldnames: list[str]) -> tuple[object, csv.DictWriter]:
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = path.open("w", newline="", encoding="utf-8")
    writer = csv.DictWriter(handle, fieldnames=fieldnames)
    writer.writeheader()
    return handle, writer


def extract_panama(archive_path: Path, work_dir: Path) -> Path:
    extract_dir = work_dir / "raw"
    marker = extract_dir / ".extracted"
    if marker.exists():
        return extract_dir
    if extract_dir.exists():
        shutil.rmtree(extract_dir)
    extract_dir.mkdir(parents=True)
    with zipfile.ZipFile(archive_path) as archive:
        archive.extractall(extract_dir)
    marker.write_text(datetime.now(timezone.utc).isoformat() + "\n", encoding="utf-8")
    return extract_dir


def find_files(root: Path, contains: str, suffix: str = ".csv") -> list[Path]:
    return sorted(path for path in root.rglob(f"*{suffix}") if contains.lower() in path.name.lower())


def first_line(path: Path) -> str:
    with path.open("r", encoding="utf-8-sig", errors="replace") as handle:
        return handle.readline().strip()


def prefixed(prefix: str, value: str) -> str:
    return f"{prefix}:{value.strip()}"


def find_one_file(root: Path, filename_prefix: str) -> Path:
    matches = sorted(path for path in root.rglob("*.csv") if path.name.startswith(filename_prefix))
    if not matches:
        raise RuntimeError(f"LDBC archive did not contain expected file prefix {filename_prefix!r}.")
    return matches[0]


def stream_node_file(
    source: Path,
    output: Path,
    id_prefix: str,
    output_id_column: str,
    output_columns: list[str],
    column_map: dict[str, str],
) -> tuple[int, set[str]]:
    ids: set[str] = set()
    count = 0
    handle, writer = csv_writer(output, [output_id_column, *output_columns])
    try:
        with source.open(newline="", encoding="utf-8-sig") as raw:
            for row in csv.DictReader(raw, delimiter="|"):
                raw_id = first_value(row, "id")
                if not raw_id:
                    continue
                node_id = prefixed(id_prefix, raw_id)
                ids.add(node_id)
                count += 1
                out = {output_id_column: node_id}
                for out_col in output_columns:
                    out[out_col] = first_value(row, column_map[out_col])
                writer.writerow(out)
    finally:
        handle.close()
    return count, ids


def stream_edge_file(
    source: Path,
    output: Path,
    fieldnames: list[str],
    src_col: str,
    dst_col: str,
    src_prefix: str,
    dst_prefix: str,
    src_ids: set[str],
    dst_ids: set[str],
    extra_columns: list[str] | None = None,
) -> int:
    extra_columns = extra_columns or []
    count = 0
    handle, writer = csv_writer(output, fieldnames)
    try:
        with source.open(newline="", encoding="utf-8-sig") as raw:
            reader = csv.reader(raw, delimiter="|")
            header = next(reader, [])
            extra_indexes = {name: header.index(name) for name in extra_columns if name in header}
            for values in reader:
                if len(values) < 2:
                    continue
                src_id = prefixed(src_prefix, values[0])
                dst_id = prefixed(dst_prefix, values[1])
                if src_id not in src_ids or dst_id not in dst_ids:
                    continue
                row = {src_col: src_id, dst_col: dst_id}
                for name in extra_columns:
                    idx = extra_indexes.get(name)
                    row[name] = values[idx].strip() if idx is not None and idx < len(values) else ""
                writer.writerow(row)
                count += 1
    finally:
        handle.close()
    return count


def transform_panama(archive_path: Path, work_dir: Path) -> dict[str, object]:
    normalized_dir = work_dir / "normalized"
    metadata_path = normalized_dir / "metadata.json"
    if metadata_path.exists():
        return json.loads(metadata_path.read_text(encoding="utf-8"))

    raw_dir = extract_panama(archive_path, work_dir)
    node_files = [path for path in raw_dir.rglob("*.csv") if path.name.lower().startswith("nodes")]
    edge_files = find_files(raw_dir, "relationship")
    if not node_files or not edge_files:
        raise RuntimeError("Panama archive did not contain expected nodes*.csv and relationships*.csv files.")

    node_ids: set[str] = set()
    node_count = 0
    node_handle, node_writer = csv_writer(
        normalized_dir / "nodes.csv",
        ["node_id", "label", "name", "countries", "country_codes", "source_id", "valid_until"],
    )
    try:
        for path in node_files:
            label = path.stem.replace("nodes-", "").replace("nodes_", "").replace("nodes", "node") or "node"
            with path.open(newline="", encoding="utf-8-sig") as handle:
                for row in csv.DictReader(handle):
                    node_id = first_value(row, "node_id", "id", "_id")
                    if not node_id:
                        continue
                    node_ids.add(node_id)
                    node_count += 1
                    node_writer.writerow(
                        {
                            "node_id": node_id,
                            "label": label,
                            "name": first_value(row, "name"),
                            "countries": first_value(row, "countries"),
                            "country_codes": first_value(row, "country_codes"),
                            "source_id": first_value(row, "sourceID", "source_id"),
                            "valid_until": first_value(row, "valid_until"),
                        }
                    )
    finally:
        node_handle.close()

    edge_count = 0
    seed_start = ""
    seed_end = ""
    edge_handle, edge_writer = csv_writer(
        normalized_dir / "edges.csv",
        ["start_id", "end_id", "rel_type", "link", "start_date", "end_date"],
    )
    try:
        for path in edge_files:
            with path.open(newline="", encoding="utf-8-sig") as handle:
                for row in csv.DictReader(handle):
                    start_id = first_value(row, "START_ID", "start_id", "node_id_start")
                    end_id = first_value(row, "END_ID", "end_id", "node_id_end")
                    if not start_id or not end_id or start_id not in node_ids or end_id not in node_ids:
                        continue
                    if not seed_start:
                        seed_start, seed_end = start_id, end_id
                    edge_count += 1
                    edge_writer.writerow(
                        {
                            "start_id": start_id,
                            "end_id": end_id,
                            "rel_type": first_value(row, "TYPE", "type", "rel_type") or "related_to",
                            "link": first_value(row, "link"),
                            "start_date": first_value(row, "start_date"),
                            "end_date": first_value(row, "end_date"),
                        }
                    )
    finally:
        edge_handle.close()

    metadata = {
        "node_count": node_count,
        "edge_count": edge_count,
        "seed_start": seed_start,
        "seed_end": seed_end,
        "raw_files": [str(path.relative_to(raw_dir)) for path in node_files + edge_files],
    }
    metadata_path.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    return metadata


def extract_ldbc(archive_path: Path, work_dir: Path) -> Path:
    extract_dir = work_dir / "raw"
    marker = extract_dir / ".extracted"
    if marker.exists():
        return extract_dir
    if extract_dir.exists():
        shutil.rmtree(extract_dir)
    extract_dir.mkdir(parents=True)

    attempts = [
        ["tar", "--zstd", "-xf", str(archive_path), "-C", str(extract_dir)],
        ["tar", "-I", "zstd", "-xf", str(archive_path), "-C", str(extract_dir)],
    ]
    errors: list[str] = []
    for cmd in attempts:
        completed = run(cmd, timeout=None)
        if completed.returncode == 0:
            marker.write_text(datetime.now(timezone.utc).isoformat() + "\n", encoding="utf-8")
            return extract_dir
        errors.append(completed.stderr.strip())

    if shutil.which("zstd") is None:
        raise RuntimeError("Cannot extract LDBC .tar.zst: zstd is not installed. Install zstd, then rerun the script.")
    raise RuntimeError("Cannot extract LDBC .tar.zst:\n" + "\n".join(errors))


def transform_ldbc(archive_path: Path, work_dir: Path) -> dict[str, object]:
    normalized_dir = work_dir / "normalized"
    metadata_path = normalized_dir / "metadata.json"
    schema_version = "full-snb-v1"
    if metadata_path.exists():
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        if metadata.get("schema_version") == schema_version:
            return metadata
        shutil.rmtree(normalized_dir)

    raw_dir = extract_ldbc(archive_path, work_dir)
    normalized_dir.mkdir(parents=True, exist_ok=True)

    node_specs = [
        ("person_", "person", "persons.csv", "person_id", ["first_name", "last_name", "gender", "birthday", "creation_date", "location_ip", "browser_used"], {"first_name": "firstName", "last_name": "lastName", "gender": "gender", "birthday": "birthday", "creation_date": "creationDate", "location_ip": "locationIP", "browser_used": "browserUsed"}),
        ("forum_", "forum", "forums.csv", "forum_id", ["title", "creation_date"], {"title": "title", "creation_date": "creationDate"}),
        ("post_", "post", "posts.csv", "post_id", ["image_file", "creation_date", "language", "content", "length"], {"image_file": "imageFile", "creation_date": "creationDate", "language": "language", "content": "content", "length": "length"}),
        ("comment_", "comment", "comments.csv", "comment_id", ["creation_date", "content", "length"], {"creation_date": "creationDate", "content": "content", "length": "length"}),
        ("place_", "place", "places.csv", "place_id", ["name", "url", "kind"], {"name": "name", "url": "url", "kind": "type"}),
        ("organisation_", "organisation", "organisations.csv", "organisation_id", ["kind", "name", "url"], {"kind": "type", "name": "name", "url": "url"}),
        ("tag_", "tag", "tags.csv", "tag_id", ["name", "url"], {"name": "name", "url": "url"}),
        ("tagclass_", "tagclass", "tagclasses.csv", "tagclass_id", ["name", "url"], {"name": "name", "url": "url"}),
    ]

    node_counts: dict[str, int] = {}
    node_ids: dict[str, set[str]] = {}
    node_source_files: dict[str, str] = {}
    for file_prefix, id_prefix, out_file, id_col, out_cols, col_map in node_specs:
        source = find_one_file(raw_dir, file_prefix)
        count, ids = stream_node_file(source, normalized_dir / out_file, id_prefix, id_col, out_cols, col_map)
        node_counts[id_prefix] = count
        node_ids[id_prefix] = ids
        node_source_files[id_prefix] = str(source.relative_to(raw_dir))

    edge_specs = [
        ("person_knows_person_", "person_knows_person.csv", ["src_person_id", "dst_person_id", "creationDate"], "src_person_id", "dst_person_id", "person", "person", ["creationDate"]),
        ("person_hasInterest_tag_", "person_has_interest_tag.csv", ["person_id", "tag_id"], "person_id", "tag_id", "person", "tag", []),
        ("person_isLocatedIn_place_", "person_is_located_in_place.csv", ["person_id", "place_id"], "person_id", "place_id", "person", "place", []),
        ("person_likes_comment_", "person_likes_comment.csv", ["person_id", "comment_id", "creationDate"], "person_id", "comment_id", "person", "comment", ["creationDate"]),
        ("person_likes_post_", "person_likes_post.csv", ["person_id", "post_id", "creationDate"], "person_id", "post_id", "person", "post", ["creationDate"]),
        ("person_studyAt_organisation_", "person_study_at_organisation.csv", ["person_id", "organisation_id", "classYear"], "person_id", "organisation_id", "person", "organisation", ["classYear"]),
        ("person_workAt_organisation_", "person_work_at_organisation.csv", ["person_id", "organisation_id", "workFrom"], "person_id", "organisation_id", "person", "organisation", ["workFrom"]),
        ("forum_containerOf_post_", "forum_container_of_post.csv", ["forum_id", "post_id"], "forum_id", "post_id", "forum", "post", []),
        ("forum_hasMember_person_", "forum_has_member_person.csv", ["forum_id", "person_id", "joinDate"], "forum_id", "person_id", "forum", "person", ["joinDate"]),
        ("forum_hasModerator_person_", "forum_has_moderator_person.csv", ["forum_id", "person_id"], "forum_id", "person_id", "forum", "person", []),
        ("forum_hasTag_tag_", "forum_has_tag_tag.csv", ["forum_id", "tag_id"], "forum_id", "tag_id", "forum", "tag", []),
        ("post_hasCreator_person_", "post_has_creator_person.csv", ["post_id", "person_id"], "post_id", "person_id", "post", "person", []),
        ("post_hasTag_tag_", "post_has_tag_tag.csv", ["post_id", "tag_id"], "post_id", "tag_id", "post", "tag", []),
        ("post_isLocatedIn_place_", "post_is_located_in_place.csv", ["post_id", "place_id"], "post_id", "place_id", "post", "place", []),
        ("comment_hasCreator_person_", "comment_has_creator_person.csv", ["comment_id", "person_id"], "comment_id", "person_id", "comment", "person", []),
        ("comment_hasTag_tag_", "comment_has_tag_tag.csv", ["comment_id", "tag_id"], "comment_id", "tag_id", "comment", "tag", []),
        ("comment_isLocatedIn_place_", "comment_is_located_in_place.csv", ["comment_id", "place_id"], "comment_id", "place_id", "comment", "place", []),
        ("comment_replyOf_comment_", "comment_reply_of_comment.csv", ["comment_id", "parent_comment_id"], "comment_id", "parent_comment_id", "comment", "comment", []),
        ("comment_replyOf_post_", "comment_reply_of_post.csv", ["comment_id", "post_id"], "comment_id", "post_id", "comment", "post", []),
        ("organisation_isLocatedIn_place_", "organisation_is_located_in_place.csv", ["organisation_id", "place_id"], "organisation_id", "place_id", "organisation", "place", []),
        ("place_isPartOf_place_", "place_is_part_of_place.csv", ["place_id", "parent_place_id"], "place_id", "parent_place_id", "place", "place", []),
        ("tag_hasType_tagclass_", "tag_has_type_tagclass.csv", ["tag_id", "tagclass_id"], "tag_id", "tagclass_id", "tag", "tagclass", []),
        ("tagclass_isSubclassOf_tagclass_", "tagclass_is_subclass_of_tagclass.csv", ["tagclass_id", "parent_tagclass_id"], "tagclass_id", "parent_tagclass_id", "tagclass", "tagclass", []),
    ]

    edge_counts: dict[str, int] = {}
    edge_source_files: dict[str, str] = {}
    seed_start = ""
    seed_end = ""
    for file_prefix, out_file, fieldnames, src_col, dst_col, src_prefix, dst_prefix, extra_cols in edge_specs:
        source = find_one_file(raw_dir, file_prefix)
        count = stream_edge_file(source, normalized_dir / out_file, fieldnames, src_col, dst_col, src_prefix, dst_prefix, node_ids[src_prefix], node_ids[dst_prefix], extra_cols)
        edge_name = out_file.removesuffix(".csv")
        edge_counts[edge_name] = count
        edge_source_files[edge_name] = str(source.relative_to(raw_dir))
        if file_prefix == "person_knows_person_":
            with (normalized_dir / out_file).open(newline="", encoding="utf-8") as handle:
                first = next(csv.DictReader(handle), None)
                if first:
                    seed_start = first[src_col]
                    seed_end = first[dst_col]

    language_count = 0
    language_handle, language_writer = csv_writer(normalized_dir / "person_speaks_language.csv", ["person_id", "language"])
    try:
        source = find_one_file(raw_dir, "person_speaks_language_")
        with source.open(newline="", encoding="utf-8-sig") as raw:
            reader = csv.reader(raw, delimiter="|")
            next(reader, [])
            for values in reader:
                if len(values) < 2:
                    continue
                person_id = prefixed("person", values[0])
                if person_id not in node_ids["person"]:
                    continue
                language_writer.writerow({"person_id": person_id, "language": values[1].strip()})
                language_count += 1
    finally:
        language_handle.close()

    email_count = 0
    email_handle, email_writer = csv_writer(normalized_dir / "person_email.csv", ["person_id", "email"])
    try:
        source = find_one_file(raw_dir, "person_email_emailaddress_")
        with source.open(newline="", encoding="utf-8-sig") as raw:
            reader = csv.reader(raw, delimiter="|")
            next(reader, [])
            for values in reader:
                if len(values) < 2:
                    continue
                person_id = prefixed("person", values[0])
                if person_id not in node_ids["person"]:
                    continue
                email_writer.writerow({"person_id": person_id, "email": values[1].strip()})
                email_count += 1
    finally:
        email_handle.close()

    metadata = {
        "schema_version": schema_version,
        "node_counts": node_counts,
        "edge_counts": edge_counts,
        "attribute_counts": {"person_speaks_language": language_count, "person_email": email_count},
        "total_node_count": sum(node_counts.values()),
        "total_edge_count": sum(edge_counts.values()),
        "person_count": node_counts["person"],
        "knows_count": edge_counts["person_knows_person"],
        "seed_start": seed_start,
        "seed_end": seed_end,
        "node_files": node_source_files,
        "edge_files": edge_source_files,
    }
    metadata_path.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    return metadata


def clean_graph_catalog(container: str) -> None:
    psql(
        container,
        """
        SELECT graph.reset();
        TRUNCATE graph._registered_filter_columns,
                 graph._registered_edges,
                 graph._registered_tables,
                 graph._build_jobs,
                 graph._maintenance_jobs,
                 graph._sync_log,
                 graph._sync_buffer
        RESTART IDENTITY;
        """,
        timeout=None,
        progress_label="Resetting pgGraph catalog",
    )


def load_panama(container: str, normalized_dir: Path) -> None:
    remote_dir = f"/tmp/pggraph-sandbox-panama-{int(time.time())}"
    docker_cp(normalized_dir, container, remote_dir)
    psql(
        container,
        f"""
        DROP SCHEMA IF EXISTS panama CASCADE;
        CREATE SCHEMA panama;
        CREATE TABLE panama.nodes (
          node_id text PRIMARY KEY,
          label text NOT NULL,
          name text,
          countries text,
          country_codes text,
          source_id text,
          valid_until text
        );
        CREATE TABLE panama.edges (
          edge_id bigserial PRIMARY KEY,
          start_id text NOT NULL REFERENCES panama.nodes(node_id),
          end_id text NOT NULL REFERENCES panama.nodes(node_id),
          rel_type text NOT NULL,
          link text,
          start_date text,
          end_date text
        );
        COPY panama.nodes(node_id, label, name, countries, country_codes, source_id, valid_until)
          FROM '{remote_dir}/nodes.csv' WITH (FORMAT csv, HEADER true);
        COPY panama.edges(start_id, end_id, rel_type, link, start_date, end_date)
          FROM '{remote_dir}/edges.csv' WITH (FORMAT csv, HEADER true);
        CREATE INDEX panama_edges_start_idx ON panama.edges(start_id);
        CREATE INDEX panama_edges_end_idx ON panama.edges(end_id);
        CREATE INDEX panama_nodes_name_idx ON panama.nodes(name);
        SELECT graph.add_table('panama.nodes'::regclass, 'node_id', ARRAY['name', 'countries', 'country_codes', 'label']);
        SELECT graph.add_edge(
          from_table := 'panama.edges'::regclass,
          from_column := 'start_id',
          to_table := 'panama.nodes'::regclass,
          to_column := 'end_id',
          label := 'related_to',
          bidirectional := true,
          label_column := 'rel_type'
        );
        """,
        timeout=None,
        progress_label="Loading Panama into PostgreSQL",
    )


def load_ldbc(container: str, normalized_dir: Path) -> None:
    remote_dir = f"/tmp/pggraph-sandbox-ldbc-{int(time.time())}"
    docker_cp(normalized_dir, container, remote_dir)
    psql(
        container,
        f"""
        DROP SCHEMA IF EXISTS ldbc CASCADE;
        CREATE SCHEMA ldbc;

        CREATE TABLE ldbc.persons (
          person_id text PRIMARY KEY,
          first_name text,
          last_name text,
          gender text,
          birthday text,
          creation_date text,
          location_ip text,
          browser_used text
        );
        CREATE TABLE ldbc.forums (
          forum_id text PRIMARY KEY,
          title text,
          creation_date text
        );
        CREATE TABLE ldbc.posts (
          post_id text PRIMARY KEY,
          image_file text,
          creation_date text,
          language text,
          content text,
          length text
        );
        CREATE TABLE ldbc.comments (
          comment_id text PRIMARY KEY,
          creation_date text,
          content text,
          length text
        );
        CREATE TABLE ldbc.places (
          place_id text PRIMARY KEY,
          name text,
          url text,
          kind text
        );
        CREATE TABLE ldbc.organisations (
          organisation_id text PRIMARY KEY,
          kind text,
          name text,
          url text
        );
        CREATE TABLE ldbc.tags (
          tag_id text PRIMARY KEY,
          name text,
          url text
        );
        CREATE TABLE ldbc.tagclasses (
          tagclass_id text PRIMARY KEY,
          name text,
          url text
        );

        COPY ldbc.persons(person_id, first_name, last_name, gender, birthday, creation_date, location_ip, browser_used)
          FROM '{remote_dir}/persons.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.forums(forum_id, title, creation_date)
          FROM '{remote_dir}/forums.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.posts(post_id, image_file, creation_date, language, content, length)
          FROM '{remote_dir}/posts.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.comments(comment_id, creation_date, content, length)
          FROM '{remote_dir}/comments.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.places(place_id, name, url, kind)
          FROM '{remote_dir}/places.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.organisations(organisation_id, kind, name, url)
          FROM '{remote_dir}/organisations.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.tags(tag_id, name, url)
          FROM '{remote_dir}/tags.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.tagclasses(tagclass_id, name, url)
          FROM '{remote_dir}/tagclasses.csv' WITH (FORMAT csv, HEADER true);

        CREATE TABLE ldbc.person_knows_person (
          src_person_id text NOT NULL REFERENCES ldbc.persons(person_id),
          dst_person_id text NOT NULL REFERENCES ldbc.persons(person_id),
          creation_date text
        );
        CREATE TABLE ldbc.person_has_interest_tag (person_id text NOT NULL REFERENCES ldbc.persons(person_id), tag_id text NOT NULL REFERENCES ldbc.tags(tag_id));
        CREATE TABLE ldbc.person_is_located_in_place (person_id text NOT NULL REFERENCES ldbc.persons(person_id), place_id text NOT NULL REFERENCES ldbc.places(place_id));
        CREATE TABLE ldbc.person_likes_comment (person_id text NOT NULL REFERENCES ldbc.persons(person_id), comment_id text NOT NULL REFERENCES ldbc.comments(comment_id), creation_date text);
        CREATE TABLE ldbc.person_likes_post (person_id text NOT NULL REFERENCES ldbc.persons(person_id), post_id text NOT NULL REFERENCES ldbc.posts(post_id), creation_date text);
        CREATE TABLE ldbc.person_study_at_organisation (person_id text NOT NULL REFERENCES ldbc.persons(person_id), organisation_id text NOT NULL REFERENCES ldbc.organisations(organisation_id), class_year text);
        CREATE TABLE ldbc.person_work_at_organisation (person_id text NOT NULL REFERENCES ldbc.persons(person_id), organisation_id text NOT NULL REFERENCES ldbc.organisations(organisation_id), work_from text);
        CREATE TABLE ldbc.forum_container_of_post (forum_id text NOT NULL REFERENCES ldbc.forums(forum_id), post_id text NOT NULL REFERENCES ldbc.posts(post_id));
        CREATE TABLE ldbc.forum_has_member_person (forum_id text NOT NULL REFERENCES ldbc.forums(forum_id), person_id text NOT NULL REFERENCES ldbc.persons(person_id), join_date text);
        CREATE TABLE ldbc.forum_has_moderator_person (forum_id text NOT NULL REFERENCES ldbc.forums(forum_id), person_id text NOT NULL REFERENCES ldbc.persons(person_id));
        CREATE TABLE ldbc.forum_has_tag_tag (forum_id text NOT NULL REFERENCES ldbc.forums(forum_id), tag_id text NOT NULL REFERENCES ldbc.tags(tag_id));
        CREATE TABLE ldbc.post_has_creator_person (post_id text NOT NULL REFERENCES ldbc.posts(post_id), person_id text NOT NULL REFERENCES ldbc.persons(person_id));
        CREATE TABLE ldbc.post_has_tag_tag (post_id text NOT NULL REFERENCES ldbc.posts(post_id), tag_id text NOT NULL REFERENCES ldbc.tags(tag_id));
        CREATE TABLE ldbc.post_is_located_in_place (post_id text NOT NULL REFERENCES ldbc.posts(post_id), place_id text NOT NULL REFERENCES ldbc.places(place_id));
        CREATE TABLE ldbc.comment_has_creator_person (comment_id text NOT NULL REFERENCES ldbc.comments(comment_id), person_id text NOT NULL REFERENCES ldbc.persons(person_id));
        CREATE TABLE ldbc.comment_has_tag_tag (comment_id text NOT NULL REFERENCES ldbc.comments(comment_id), tag_id text NOT NULL REFERENCES ldbc.tags(tag_id));
        CREATE TABLE ldbc.comment_is_located_in_place (comment_id text NOT NULL REFERENCES ldbc.comments(comment_id), place_id text NOT NULL REFERENCES ldbc.places(place_id));
        CREATE TABLE ldbc.comment_reply_of_comment (comment_id text NOT NULL REFERENCES ldbc.comments(comment_id), parent_comment_id text NOT NULL REFERENCES ldbc.comments(comment_id));
        CREATE TABLE ldbc.comment_reply_of_post (comment_id text NOT NULL REFERENCES ldbc.comments(comment_id), post_id text NOT NULL REFERENCES ldbc.posts(post_id));
        CREATE TABLE ldbc.organisation_is_located_in_place (organisation_id text NOT NULL REFERENCES ldbc.organisations(organisation_id), place_id text NOT NULL REFERENCES ldbc.places(place_id));
        CREATE TABLE ldbc.place_is_part_of_place (place_id text NOT NULL REFERENCES ldbc.places(place_id), parent_place_id text NOT NULL REFERENCES ldbc.places(place_id));
        CREATE TABLE ldbc.tag_has_type_tagclass (tag_id text NOT NULL REFERENCES ldbc.tags(tag_id), tagclass_id text NOT NULL REFERENCES ldbc.tagclasses(tagclass_id));
        CREATE TABLE ldbc.tagclass_is_subclass_of_tagclass (tagclass_id text NOT NULL REFERENCES ldbc.tagclasses(tagclass_id), parent_tagclass_id text NOT NULL REFERENCES ldbc.tagclasses(tagclass_id));
        CREATE TABLE ldbc.person_speaks_language (person_id text NOT NULL REFERENCES ldbc.persons(person_id), language text);
        CREATE TABLE ldbc.person_email (person_id text NOT NULL REFERENCES ldbc.persons(person_id), email text);

        COPY ldbc.person_knows_person(src_person_id, dst_person_id, creation_date) FROM '{remote_dir}/person_knows_person.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.person_has_interest_tag(person_id, tag_id) FROM '{remote_dir}/person_has_interest_tag.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.person_is_located_in_place(person_id, place_id) FROM '{remote_dir}/person_is_located_in_place.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.person_likes_comment(person_id, comment_id, creation_date) FROM '{remote_dir}/person_likes_comment.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.person_likes_post(person_id, post_id, creation_date) FROM '{remote_dir}/person_likes_post.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.person_study_at_organisation(person_id, organisation_id, class_year) FROM '{remote_dir}/person_study_at_organisation.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.person_work_at_organisation(person_id, organisation_id, work_from) FROM '{remote_dir}/person_work_at_organisation.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.forum_container_of_post(forum_id, post_id) FROM '{remote_dir}/forum_container_of_post.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.forum_has_member_person(forum_id, person_id, join_date) FROM '{remote_dir}/forum_has_member_person.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.forum_has_moderator_person(forum_id, person_id) FROM '{remote_dir}/forum_has_moderator_person.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.forum_has_tag_tag(forum_id, tag_id) FROM '{remote_dir}/forum_has_tag_tag.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.post_has_creator_person(post_id, person_id) FROM '{remote_dir}/post_has_creator_person.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.post_has_tag_tag(post_id, tag_id) FROM '{remote_dir}/post_has_tag_tag.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.post_is_located_in_place(post_id, place_id) FROM '{remote_dir}/post_is_located_in_place.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.comment_has_creator_person(comment_id, person_id) FROM '{remote_dir}/comment_has_creator_person.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.comment_has_tag_tag(comment_id, tag_id) FROM '{remote_dir}/comment_has_tag_tag.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.comment_is_located_in_place(comment_id, place_id) FROM '{remote_dir}/comment_is_located_in_place.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.comment_reply_of_comment(comment_id, parent_comment_id) FROM '{remote_dir}/comment_reply_of_comment.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.comment_reply_of_post(comment_id, post_id) FROM '{remote_dir}/comment_reply_of_post.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.organisation_is_located_in_place(organisation_id, place_id) FROM '{remote_dir}/organisation_is_located_in_place.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.place_is_part_of_place(place_id, parent_place_id) FROM '{remote_dir}/place_is_part_of_place.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.tag_has_type_tagclass(tag_id, tagclass_id) FROM '{remote_dir}/tag_has_type_tagclass.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.tagclass_is_subclass_of_tagclass(tagclass_id, parent_tagclass_id) FROM '{remote_dir}/tagclass_is_subclass_of_tagclass.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.person_speaks_language(person_id, language) FROM '{remote_dir}/person_speaks_language.csv' WITH (FORMAT csv, HEADER true);
        COPY ldbc.person_email(person_id, email) FROM '{remote_dir}/person_email.csv' WITH (FORMAT csv, HEADER true);

        CREATE INDEX ldbc_persons_name_idx ON ldbc.persons(first_name, last_name);

        SELECT graph.add_table('ldbc.persons'::regclass, 'person_id', ARRAY['first_name', 'last_name', 'gender']);
        SELECT graph.add_table('ldbc.forums'::regclass, 'forum_id', ARRAY['title']);
        SELECT graph.add_table('ldbc.posts'::regclass, 'post_id', ARRAY['language', 'content']);
        SELECT graph.add_table('ldbc.comments'::regclass, 'comment_id', ARRAY['content']);
        SELECT graph.add_table('ldbc.places'::regclass, 'place_id', ARRAY['name', 'kind']);
        SELECT graph.add_table('ldbc.organisations'::regclass, 'organisation_id', ARRAY['name', 'kind']);
        SELECT graph.add_table('ldbc.tags'::regclass, 'tag_id', ARRAY['name']);
        SELECT graph.add_table('ldbc.tagclasses'::regclass, 'tagclass_id', ARRAY['name']);

        SELECT graph.add_edge('ldbc.person_knows_person'::regclass, 'src_person_id', 'ldbc.persons'::regclass, 'dst_person_id', 'knows', true);
        SELECT graph.add_edge('ldbc.person_has_interest_tag'::regclass, 'person_id', 'ldbc.tags'::regclass, 'tag_id', 'has_interest', true);
        SELECT graph.add_edge('ldbc.person_is_located_in_place'::regclass, 'person_id', 'ldbc.places'::regclass, 'place_id', 'person_is_located_in', true);
        SELECT graph.add_edge('ldbc.person_likes_comment'::regclass, 'person_id', 'ldbc.comments'::regclass, 'comment_id', 'likes_comment', true);
        SELECT graph.add_edge('ldbc.person_likes_post'::regclass, 'person_id', 'ldbc.posts'::regclass, 'post_id', 'likes_post', true);
        SELECT graph.add_edge('ldbc.person_study_at_organisation'::regclass, 'person_id', 'ldbc.organisations'::regclass, 'organisation_id', 'study_at', true);
        SELECT graph.add_edge('ldbc.person_work_at_organisation'::regclass, 'person_id', 'ldbc.organisations'::regclass, 'organisation_id', 'work_at', true);
        SELECT graph.add_edge('ldbc.forum_container_of_post'::regclass, 'forum_id', 'ldbc.posts'::regclass, 'post_id', 'container_of', true);
        SELECT graph.add_edge('ldbc.forum_has_member_person'::regclass, 'forum_id', 'ldbc.persons'::regclass, 'person_id', 'has_member', true);
        SELECT graph.add_edge('ldbc.forum_has_moderator_person'::regclass, 'forum_id', 'ldbc.persons'::regclass, 'person_id', 'has_moderator', true);
        SELECT graph.add_edge('ldbc.forum_has_tag_tag'::regclass, 'forum_id', 'ldbc.tags'::regclass, 'tag_id', 'forum_has_tag', true);
        SELECT graph.add_edge('ldbc.post_has_creator_person'::regclass, 'post_id', 'ldbc.persons'::regclass, 'person_id', 'post_has_creator', true);
        SELECT graph.add_edge('ldbc.post_has_tag_tag'::regclass, 'post_id', 'ldbc.tags'::regclass, 'tag_id', 'post_has_tag', true);
        SELECT graph.add_edge('ldbc.post_is_located_in_place'::regclass, 'post_id', 'ldbc.places'::regclass, 'place_id', 'post_is_located_in', true);
        SELECT graph.add_edge('ldbc.comment_has_creator_person'::regclass, 'comment_id', 'ldbc.persons'::regclass, 'person_id', 'comment_has_creator', true);
        SELECT graph.add_edge('ldbc.comment_has_tag_tag'::regclass, 'comment_id', 'ldbc.tags'::regclass, 'tag_id', 'comment_has_tag', true);
        SELECT graph.add_edge('ldbc.comment_is_located_in_place'::regclass, 'comment_id', 'ldbc.places'::regclass, 'place_id', 'comment_is_located_in', true);
        SELECT graph.add_edge('ldbc.comment_reply_of_comment'::regclass, 'comment_id', 'ldbc.comments'::regclass, 'parent_comment_id', 'reply_of_comment', true);
        SELECT graph.add_edge('ldbc.comment_reply_of_post'::regclass, 'comment_id', 'ldbc.posts'::regclass, 'post_id', 'reply_of_post', true);
        SELECT graph.add_edge('ldbc.organisation_is_located_in_place'::regclass, 'organisation_id', 'ldbc.places'::regclass, 'place_id', 'organisation_is_located_in', true);
        SELECT graph.add_edge('ldbc.place_is_part_of_place'::regclass, 'place_id', 'ldbc.places'::regclass, 'parent_place_id', 'is_part_of', true);
        SELECT graph.add_edge('ldbc.tag_has_type_tagclass'::regclass, 'tag_id', 'ldbc.tagclasses'::regclass, 'tagclass_id', 'has_type', true);
        SELECT graph.add_edge('ldbc.tagclass_is_subclass_of_tagclass'::regclass, 'tagclass_id', 'ldbc.tagclasses'::regclass, 'parent_tagclass_id', 'is_subclass_of', true);
        """,
        timeout=None,
        progress_label="Loading full LDBC into PostgreSQL",
    )


def prepare_dataset(args: argparse.Namespace, dataset: str) -> dict[str, object]:
    spec = DATASETS[dataset]
    dataset_dir = args.datasets_dir / dataset
    dataset_dir.mkdir(parents=True, exist_ok=True)
    archive = download(spec, dataset_dir, args.yes)
    started = time.perf_counter()
    if dataset == "panama":
        metadata = transform_panama(archive, dataset_dir)
    else:
        metadata = transform_ldbc(archive, dataset_dir)
    transform_seconds = time.perf_counter() - started

    clean_graph_catalog(args.container)
    load_started = time.perf_counter()
    if dataset == "panama":
        load_panama(args.container, dataset_dir / "normalized")
    else:
        load_ldbc(args.container, dataset_dir / "normalized")
    load_seconds = time.perf_counter() - load_started

    build_started = time.perf_counter()
    build_output = psql(
        args.container,
        "SELECT row_to_json(b) FROM graph.build() b;",
        timeout=None,
        tuples_only=True,
        progress_label=f"Running graph.build() for {dataset}",
    ).strip()
    build_seconds = time.perf_counter() - build_started
    if dataset == "panama":
        seed_table = "panama.nodes"
        seed_id = metadata["seed_start"]
    else:
        seed_table = "ldbc.persons"
        seed_id = metadata["seed_start"]
    status = psql(
        args.container,
        f"""
        WITH warm AS MATERIALIZED (
          SELECT count(*)
          FROM graph.traverse('{seed_table}'::regclass, {sql_literal(str(seed_id))}, 1, hydrate := false, max_rows := 1)
        )
        SELECT row_to_json(s)
        FROM warm, graph.status() s;
        """,
        timeout=120,
        tuples_only=True,
    ).strip()

    return {
        "source_url": spec.url,
        "archive": str(archive),
        "archive_sha256": sha256(archive),
        "compressed_size": spec.compressed_size,
        "uncompressed_size": spec.uncompressed_size,
        "metadata": metadata,
        "transform_seconds": transform_seconds,
        "load_seconds": load_seconds,
        "build_seconds": build_seconds,
        "build_output": build_output,
        "status": json.loads(status) if status else None,
    }


def scalar(container: str, sql: str) -> str:
    return psql(container, sql, timeout=120, tuples_only=True).strip().splitlines()[0]


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def benchmark_dsn(args: argparse.Namespace) -> str:
    return (
        f"host={args.host} port={args.port} dbname={args.database} "
        f"user={args.user} password={args.password}"
    )


def connect_benchmark(args: argparse.Namespace):
    try:
        import psycopg
    except ImportError as exc:
        raise RuntimeError("psycopg is required for benchmark timing. Run through sandbox/run_benchmarks.sh so the benchmark venv is created.") from exc
    return psycopg.connect(benchmark_dsn(args), autocommit=True)


def workload(dataset: str, container: str) -> list[WorkloadQuery]:
    if dataset == "panama":
        seed = scalar(container, "SELECT start_id FROM panama.edges GROUP BY start_id ORDER BY count(*) DESC LIMIT 1;")
        target = scalar(container, f"SELECT end_id FROM panama.edges WHERE start_id = {sql_literal(seed)} LIMIT 1;")
        gql_params = json.dumps({"seed": seed})
        status_sql = f"""
WITH warm AS MATERIALIZED (
  SELECT count(*) AS rows_seen
  FROM graph.traverse('panama.nodes'::regclass, {sql_literal(seed)}, 1, hydrate := false, max_rows := 1)
)
SELECT s.*
FROM warm, graph.status() s
"""
        return [
            WorkloadQuery("status", "Is the Panama graph loaded in this backend, and how large is it?", status_sql),
            WorkloadQuery("entity_search", "Which Panama entities mention Mossack in a registered searchable field?", "SELECT * FROM graph.search('name', 'Mossack', table_filter := 'panama.nodes'::regclass, mode := 'contains', max_rows := 25, hydrate := false)"),
            WorkloadQuery("traverse_depth_2", "What is the two-hop neighborhood around a high-degree Panama node?", f"SELECT * FROM graph.traverse('panama.nodes'::regclass, {sql_literal(seed)}, 2, hydrate := false, max_rows := 500)"),
            WorkloadQuery("shortest_path", "Can pgGraph find the direct path between a high-degree Panama seed and one adjacent target?", f"SELECT * FROM graph.shortest_path('panama.nodes'::regclass, {sql_literal(seed)}, 'panama.nodes'::regclass, {sql_literal(target)}, max_depth := 4, hydrate := false)"),
            WorkloadQuery(
                "gql_one_hop_scalar",
                "What is the GQL overhead for a one-hop scalar projection over the built graph?",
                f"""SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:related_to]->(target:nodes)
   WHERE source.node_id = $seed
   RETURN source.node_id AS source_id,
          target.node_id AS target_id,
          target.name AS target_name
   ORDER BY target_id
   LIMIT 50',
  params := {sql_literal(gql_params)}::jsonb
)""",
            ),
            WorkloadQuery(
                "gql_one_hop_hydrated_nodes",
                "What is the GQL overhead when returning hydrated source-row node objects?",
                f"""SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:related_to]->(target:nodes)
   WHERE source.node_id = $seed
   RETURN source, target
   ORDER BY target.node_id
   LIMIT 50',
  params := {sql_literal(gql_params)}::jsonb
)""",
            ),
            WorkloadQuery(
                "gql_one_hop_coordinates",
                "What is the GQL overhead when returning coordinate-only node objects?",
                f"""SELECT row
FROM graph.gql(
  'MATCH (source:nodes)-[:related_to]->(target:nodes)
   WHERE source.node_id = $seed
   RETURN source, target
   ORDER BY target.node_id
   LIMIT 50',
  params := {sql_literal(gql_params)}::jsonb,
  hydrate := false
)""",
            ),
            WorkloadQuery(
                "sql_join_one_hop_equivalent",
                "How fast is the equivalent PostgreSQL join over the source tables?",
                f"""SELECT source.node_id AS source_id,
       target.node_id AS target_id,
       target.name AS target_name
FROM panama.nodes source
JOIN panama.edges edge ON edge.start_id = source.node_id
JOIN panama.nodes target ON target.node_id = edge.end_id
WHERE source.node_id = {sql_literal(seed)}
ORDER BY target.node_id
LIMIT 50""",
            ),
            WorkloadQuery(
                "traverse_one_hop_coordinates",
                "How fast is the native pgGraph traversal API for the same seed with coordinate output?",
                f"""SELECT node_table_name, node_id, depth
FROM graph.traverse(
  'panama.nodes'::regclass,
  {sql_literal(seed)},
  1,
  hydrate := false,
  max_rows := 50
)
ORDER BY depth, node_id""",
            ),
            WorkloadQuery("component_stats", "How many connected components does the Panama graph have, and how large are they?", "SELECT * FROM graph.component_stats()"),
            WorkloadQuery("largest_component", "Which nodes are in the first page of the largest Panama connected component?", "SELECT * FROM graph.largest_component()"),
        ]
    seed = scalar(container, "SELECT src_person_id FROM ldbc.person_knows_person GROUP BY src_person_id ORDER BY count(*) DESC LIMIT 1;")
    target = scalar(container, f"SELECT dst_person_id FROM ldbc.person_knows_person WHERE src_person_id = {sql_literal(seed)} LIMIT 1;")
    gql_params = json.dumps({"seed": seed})
    forum_seed = scalar(container, "SELECT forum_id FROM ldbc.forum_has_member_person GROUP BY forum_id ORDER BY count(*) DESC LIMIT 1;")
    post_seed = scalar(container, "SELECT post_id FROM ldbc.post_has_tag_tag LIMIT 1;")
    tag_target = scalar(container, f"SELECT tag_id FROM ldbc.post_has_tag_tag WHERE post_id = {sql_literal(post_seed)} LIMIT 1;")
    tag_seed = scalar(container, "SELECT tag_id FROM ldbc.tag_has_type_tagclass LIMIT 1;")
    tagclass_target = scalar(container, f"SELECT tagclass_id FROM ldbc.tag_has_type_tagclass WHERE tag_id = {sql_literal(tag_seed)} LIMIT 1;")
    status_sql = f"""
WITH warm AS MATERIALIZED (
  SELECT count(*) AS rows_seen
  FROM graph.traverse('ldbc.persons'::regclass, {sql_literal(seed)}, 1, hydrate := false, max_rows := 1)
)
SELECT s.*
FROM warm, graph.status() s
"""
    return [
        WorkloadQuery("status", "Is the full modeled LDBC graph loaded in this backend, and how large is it?", status_sql),
        WorkloadQuery("person_search", "Which people have John in their first name?", "SELECT * FROM graph.search('first_name', 'John', table_filter := 'ldbc.persons'::regclass, mode := 'contains', max_rows := 25, hydrate := false)"),
        WorkloadQuery("friend_traversal_depth_1", "Who is directly connected to a high-degree person through the social graph?", f"SELECT * FROM graph.traverse('ldbc.persons'::regclass, {sql_literal(seed)}, 1, hydrate := false, max_rows := 500)"),
        WorkloadQuery(
            "gql_knows_scalar",
            "What is the GQL overhead for a one-hop knows projection over the built graph?",
            f"""SELECT row
FROM graph.gql(
  'MATCH (person:persons)-[:knows]->(friend:persons)
   WHERE person.person_id = $seed
   RETURN person.person_id AS person_id,
          friend.person_id AS friend_id,
          friend.first_name AS friend_first_name
   ORDER BY friend_id
   LIMIT 100',
  params := {sql_literal(gql_params)}::jsonb
)""",
        ),
        WorkloadQuery(
            "gql_knows_hydrated_nodes",
            "What is the GQL overhead when returning hydrated person node objects?",
            f"""SELECT row
FROM graph.gql(
  'MATCH (person:persons)-[:knows]->(friend:persons)
   WHERE person.person_id = $seed
   RETURN person, friend
   ORDER BY friend.person_id
   LIMIT 100',
  params := {sql_literal(gql_params)}::jsonb
)""",
        ),
        WorkloadQuery(
            "gql_knows_coordinates",
            "What is the GQL overhead when returning coordinate-only person node objects?",
            f"""SELECT row
FROM graph.gql(
  'MATCH (person:persons)-[:knows]->(friend:persons)
   WHERE person.person_id = $seed
   RETURN person, friend
   ORDER BY friend.person_id
   LIMIT 100',
  params := {sql_literal(gql_params)}::jsonb,
  hydrate := false
)""",
        ),
        WorkloadQuery(
            "sql_join_knows_equivalent",
            "How fast is the equivalent PostgreSQL join over the LDBC source tables?",
            f"""SELECT person.person_id,
       friend.person_id AS friend_id,
       friend.first_name AS friend_first_name
FROM ldbc.persons person
JOIN ldbc.person_knows_person edge ON edge.src_person_id = person.person_id
JOIN ldbc.persons friend ON friend.person_id = edge.dst_person_id
WHERE person.person_id = {sql_literal(seed)}
ORDER BY friend.person_id
LIMIT 100""",
        ),
        WorkloadQuery("person_content_neighborhood", "What people, posts, comments, forums, tags, places, and organisations are near a high-degree person within two hops?", f"SELECT * FROM graph.traverse('ldbc.persons'::regclass, {sql_literal(seed)}, 2, hydrate := false, max_rows := 1000)"),
        WorkloadQuery("forum_neighborhood", "What members, posts, tags, and related nodes are near a busy forum?", f"SELECT * FROM graph.traverse('ldbc.forums'::regclass, {sql_literal(forum_seed)}, 2, hydrate := false, max_rows := 1000)"),
        WorkloadQuery("post_to_tag_path", "Can pgGraph find the path between a post and one of its tags?", f"SELECT * FROM graph.shortest_path('ldbc.posts'::regclass, {sql_literal(post_seed)}, 'ldbc.tags'::regclass, {sql_literal(tag_target)}, max_depth := 4, hydrate := false)"),
        WorkloadQuery("tag_to_tagclass_path", "Can pgGraph connect a tag to its tag class?", f"SELECT * FROM graph.shortest_path('ldbc.tags'::regclass, {sql_literal(tag_seed)}, 'ldbc.tagclasses'::regclass, {sql_literal(tagclass_target)}, max_depth := 4, hydrate := false)"),
        WorkloadQuery("component_stats", "How many connected components does the full modeled LDBC graph have, and how large are they?", "SELECT * FROM graph.component_stats()"),
    ]


def query_hash(sql: str) -> str:
    return hashlib.sha256(sql.encode("utf-8")).hexdigest()


def checksum_sql(sql: str) -> str:
    body = sql.rstrip(";")
    return f"""
SELECT
  count(*)::bigint AS row_count,
  md5(coalesce(string_agg(row_hash, '' ORDER BY row_hash), '')) AS result_checksum
FROM (
  SELECT md5(row_to_json(benchmark_query)::text) AS row_hash
  FROM ({body}) AS benchmark_query
) AS benchmark_rows
"""


def server_execution_ms(conn, sql: str) -> float | None:
    explain_sql = f"EXPLAIN (ANALYZE, FORMAT JSON, TIMING ON) {checksum_sql(sql)}"
    with conn.cursor() as cur:
        cur.execute(explain_sql)
        row = cur.fetchone()
    if not row:
        return None
    plan = row[0]
    if isinstance(plan, str):
        plan = json.loads(plan)
    return float(plan[0]["Execution Time"])


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = (len(ordered) - 1) * pct
    lower = int(rank)
    upper = min(lower + 1, len(ordered) - 1)
    weight = rank - lower
    return ordered[lower] * (1 - weight) + ordered[upper] * weight


def run_query(conn, query: WorkloadQuery, phase: str, iteration: int) -> dict[str, object]:
    name = query.name
    sql = query.sql
    measured_sql = checksum_sql(sql)
    started = time.perf_counter()
    try:
        server_ms = server_execution_ms(conn, sql)
        with conn.cursor() as cur:
            cur.execute(measured_sql)
            row_count, checksum = cur.fetchone()
        elapsed = time.perf_counter() - started
        return {
            "name": name,
            "question": query.question,
            "sql": sql,
            "phase": phase,
            "iteration": iteration,
            "sql_sha256": query_hash(sql),
            "wall_ms": round(elapsed * 1000, 3),
            "server_execution_ms": round(server_ms, 3) if server_ms is not None else None,
            "row_count": int(row_count or 0),
            "result_checksum": checksum,
            "ok": True,
        }
    except Exception as exc:
        elapsed = time.perf_counter() - started
        return {
            "name": name,
            "question": query.question,
            "sql": sql,
            "phase": phase,
            "iteration": iteration,
            "sql_sha256": query_hash(sql),
            "wall_ms": round(elapsed * 1000, 3),
            "server_execution_ms": None,
            "ok": False,
            "error": str(exc),
        }


def summarize(results: list[dict[str, object]]) -> dict[str, object]:
    summary: dict[str, object] = {}
    for result in results:
        if not result.get("ok"):
            continue
        key = f"{result['phase']}:{result['name']}"
        summary.setdefault(key, []).append(float(result["wall_ms"]))
    return {
        key: {
            "iterations": len(values),
            "min_ms": min(values),
            "median_ms": median(values),
            "p95_ms": percentile(values, 0.95),
            "max_ms": max(values),
        }
        for key, values in summary.items()
    }


def benchmark_methodology() -> dict[str, object]:
    return {
        "measurement_unit": "milliseconds",
        "phases": {
            "build": "Dataset load and graph.build() are measured separately in prepared.load_seconds and prepared.build_seconds.",
            "cold": "The Docker container is restarted before each cold query. This measures first query behavior in a fresh PostgreSQL process/container against the already-loaded SQL dataset and persisted pgGraph build artifacts. It does not include graph.build(). It does not drop host OS page cache.",
            "warmup": "One unrecorded pass over every query runs in a persistent PostgreSQL backend after cold measurements.",
            "hot": "Measured iterations after the warmup pass in that same persistent PostgreSQL backend.",
        },
        "cache_methodology": {
            "backend_cache": "Cold starts a fresh PostgreSQL process by restarting the container before each cold query; hot reuses one persistent PostgreSQL backend after warmup.",
            "postgres_shared_buffers": "Cold restart clears PostgreSQL shared buffers inside the container.",
            "host_os_page_cache": "Not explicitly dropped. On macOS/Docker Desktop, host and VM filesystem cache may remain warm.",
            "pggraph_artifact": "The benchmark separates graph.build() from query timing. Query cold/hot timings use the built graph state/artifact rather than rebuilding for every cold query.",
        },
        "timing_methodology": {
            "wall_ms": "Host-side elapsed time for EXPLAIN ANALYZE plus result checksum execution over a psycopg connection.",
            "server_execution_ms": "PostgreSQL server-side execution time from EXPLAIN (ANALYZE, FORMAT JSON) of the checksum query.",
            "result_checksum": "MD5 of sorted per-row JSON hashes from the measured query result.",
        },
    }


def run_benchmark(args: argparse.Namespace, dataset: str, prepared: dict[str, object]) -> dict[str, object]:
    queries = workload(dataset, args.container)
    sql_manifest = {query.name: {"question": query.question, "sql": query.sql, "sha256": query_hash(query.sql)} for query in queries}
    results: list[dict[str, object]] = []

    for query in queries:
        restart_container(args.container)
        with connect_benchmark(args) as conn:
            results.append(run_query(conn, query, "cold", 1))

    with connect_benchmark(args) as hot_conn:
        for query in queries:
            run_query(hot_conn, query, "warmup", 0)

        for iteration in range(1, args.hot_iterations + 1):
            for query in queries:
                results.append(run_query(hot_conn, query, "hot", iteration))

    return {
        "dataset": dataset,
        "prepared": prepared,
        "boundary": {
            "cold": "Docker container restart before each cold query; excludes graph.build(); OS cache may remain warm depending on host",
            "hot": "one unrecorded warm-up pass, then repeated measured SQL in one persistent psycopg PostgreSQL backend",
        },
        "methodology": benchmark_methodology(),
        "sql_manifest": sql_manifest,
        "results": results,
        "summary": summarize(results),
    }


def capture_machine_specs() -> dict[str, object]:
    specs: dict[str, object] = {
        "captured_at": datetime.now(timezone.utc).isoformat(),
        "hostname": socket.gethostname(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "python": platform.python_version(),
        "cpu_count": os.cpu_count(),
    }
    for cmd_name, cmd in {
        "docker_version": ["docker", "version", "--format", "{{json .}}"],
        "docker_info": ["docker", "info", "--format", "{{json .}}"],
        "git_commit": ["git", "rev-parse", "HEAD"],
        "git_status": ["git", "status", "--short"],
    }.items():
        completed = run(cmd, timeout=30)
        if cmd_name.startswith("docker") and completed.returncode == 0 and completed.stdout.strip().startswith("{"):
            specs[cmd_name] = json.loads(completed.stdout)
        else:
            specs[cmd_name] = completed.stdout.strip() if completed.returncode == 0 else {"error": completed.stderr.strip()}
    return specs


def print_summary(report: dict[str, object]) -> None:
    for dataset_report in report["datasets"]:  # type: ignore[index]
        if not dataset_report.get("summary"):
            continue
        print(f"\n{dataset_report['dataset']} benchmark summary")
        for key, values in dataset_report.get("summary", {}).items():
            print(
                f"  {key}: median={values['median_ms']:.3f} ms "
                f"p95={values['p95_ms']:.3f} ms "
                f"min={values['min_ms']:.3f} ms max={values['max_ms']:.3f} ms n={values['iterations']}"
            )


def main() -> int:
    parser = argparse.ArgumentParser(description="Prepare and run pgGraph SQL benchmarks.")
    parser.add_argument("--dataset", default="all", choices=["all", "panama", "ldbc"])
    parser.add_argument("--container", default="pggraph-sandbox")
    parser.add_argument("--image", default="pggraph-postgres:17")
    parser.add_argument("--datasets-dir", type=Path, required=True)
    parser.add_argument("--results-dir", type=Path, required=True)
    parser.add_argument("--benchmark-venv", type=Path, default=Path("sandbox/benchmark/.venv"))
    parser.add_argument("--playground-venv", type=Path, default=Path("sandbox/playground/.venv"))
    parser.add_argument("--hot-iterations", type=int, default=10)
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument(
        "--cleanup",
        nargs="+",
        choices=["datasets", "results", "venv", "docker", "all"],
        help="Remove generated sandbox artifacts and exit.",
    )
    parser.add_argument("--dry-run", action="store_true", help="Show cleanup targets without deleting them.")
    parser.add_argument("--remove-image", action="store_true", help="With --cleanup docker, also remove the pgGraph Docker image.")
    parser.add_argument("--yes", action="store_true", help="Approve dataset downloads after printing their sizes.")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=55432)
    parser.add_argument("--database", default="postgres")
    parser.add_argument("--user", default="postgres")
    parser.add_argument("--password", default="postgres")
    args = parser.parse_args()

    if args.cleanup:
        return cleanup_generated(args)

    require_docker()
    args.datasets_dir.mkdir(parents=True, exist_ok=True)
    args.results_dir.mkdir(parents=True, exist_ok=True)

    selected = list(DATASETS) if args.dataset == "all" else [args.dataset]
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = args.results_dir / run_id
    run_dir.mkdir(parents=True, exist_ok=False)

    dataset_reports: list[dict[str, object]] = []
    for dataset in selected:
        print(f"\nPreparing {DATASETS[dataset].name}...")
        prepared = prepare_dataset(args, dataset)
        if args.prepare_only:
            dataset_reports.append({"dataset": dataset, "prepared": prepared})
        else:
            dataset_reports.append(run_benchmark(args, dataset, prepared))

    report = {
        "run_id": run_id,
        "status": "prepared" if args.prepare_only else "completed",
        "methodology": benchmark_methodology(),
        "machine": capture_machine_specs(),
        "datasets": dataset_reports,
    }
    report_path = run_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    print_summary(report)
    print(f"\nWrote {report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
