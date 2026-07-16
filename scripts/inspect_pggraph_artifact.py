#!/usr/bin/env python3
"""Inspect a current pgGraph `.pggraph` artifact and print section-size JSON.

Usage:
  python3 scripts/inspect_pggraph_artifact.py /path/to/main.pggraph
  python3 scripts/inspect_pggraph_artifact.py -g <graph-id> --pgdata /var/lib/postgresql/data
  python3 scripts/inspect_pggraph_artifact.py --resolve-only /path/to/main.pggraph

When the requested path is the logical `main.pggraph` path, the checker first
resolves `projection-current.json` to its checksummed generation manifest and
then resolves that manifest's relative `base_artifact_path`. If no current
pointer exists, the logical path remains available as a compatibility fallback.

The checker validates the v6 header, aligned section descriptors, section
bounds, zero padding, and header/body CRC32 values. It does not require a
running PostgreSQL server.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import struct
import sys
import zlib
import uuid

MAGIC = b"PGGH"
VERSION = 6
HEADER_SIZE = 512
SECTION_ALIGNMENT = 64
KNOWN_FLAGS = 0b111
SECTION_DESCRIPTOR_OFFSET = 64
SECTION_DESCRIPTOR_SIZE = 16
BODY_CRC_OFFSET = 40
HEADER_CRC_OFFSET = 44
CURRENT_POINTER_VERSION = 1
MANIFEST_VERSION = 2
MAX_CURRENT_POINTER_BYTES = 4 * 1024
SECTION_NAMES = [
    "is_active",
    "table_oids",
    "primary_key_offsets",
    "primary_key_bytes",
    "forward_offsets",
    "forward_targets",
    "forward_type_ids",
    "forward_directions",
    "forward_weights",
    "forward_relationship_ids",
    "inbound_offsets",
    "inbound_targets",
    "inbound_type_ids",
    "inbound_directions",
    "inbound_weights",
    "inbound_relationship_ids",
    "resolution_index",
    "filter_catalog",
    "filter_data",
    "filter_text_dictionaries",
    "edge_type_registry",
    "relationship_identity_descriptors",
    "relationship_identity_key_bytes",
    "tenanted_table_oids",
    "tenant_dictionary",
    "node_tenant_tokens",
]


def _read_json_object(
    path: pathlib.Path, max_bytes: int | None = None
) -> dict[str, object]:
    data = path.read_bytes()
    if max_bytes is not None and len(data) > max_bytes:
        raise ValueError(f"{path} exceeds the {max_bytes}-byte safety limit")
    try:
        decoded = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"{path} is not valid JSON: {exc}") from exc
    if not isinstance(decoded, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return decoded


def _relative_artifact_path(root: pathlib.Path, reference: object) -> pathlib.Path:
    if not isinstance(reference, str) or not reference.strip():
        raise ValueError("generation manifest base_artifact_path must be a non-empty string")
    relative = pathlib.PurePath(reference)
    if relative.is_absolute() or any(part in {"..", "."} for part in relative.parts):
        raise ValueError(f"manifest reference {reference!r} must stay inside {root}")
    return root.joinpath(*relative.parts)


def resolve_artifact(path: pathlib.Path) -> pathlib.Path:
    """Resolve a graph root or logical main path to the published base artifact."""
    root = path if path.is_dir() else path.parent
    logical_path = root / "main.pggraph" if path.is_dir() else path
    pointer_path = root / "projection-current.json"
    if not pointer_path.exists():
        if logical_path.is_file():
            return logical_path
        raise ValueError(f"no current projection pointer or compatibility artifact under {root}")

    pointer = _read_json_object(pointer_path, MAX_CURRENT_POINTER_BYTES)
    if set(pointer) != {"version", "generation_id", "manifest_checksum"}:
        raise ValueError(f"{pointer_path} has unsupported or missing fields")
    generation_id = pointer["generation_id"]
    if (
        pointer["version"] != CURRENT_POINTER_VERSION
        or not isinstance(generation_id, int)
        or isinstance(generation_id, bool)
        or generation_id <= 0
    ):
        raise ValueError(f"{pointer_path} has an invalid version or generation_id")
    manifest_checksum = pointer["manifest_checksum"]
    if not isinstance(manifest_checksum, str) or not manifest_checksum:
        raise ValueError(f"{pointer_path} has an invalid manifest_checksum")

    manifest_path = root / f"projection-generation-{generation_id:020}.json"
    try:
        manifest_bytes = manifest_path.read_bytes()
    except FileNotFoundError as exc:
        raise ValueError(f"current pointer references missing {manifest_path}") from exc
    actual_checksum = f"crc32:{zlib.crc32(manifest_bytes) & 0xFFFFFFFF:08x}"
    if manifest_checksum != actual_checksum:
        raise ValueError(
            f"current pointer checksum mismatch: expected {manifest_checksum}, got {actual_checksum}"
        )

    manifest = _read_json_object(manifest_path)
    if (
        manifest.get("version") != MANIFEST_VERSION
        or manifest.get("generation_id") != generation_id
    ):
        raise ValueError(f"{manifest_path} does not describe current generation {generation_id}")
    artifact = _relative_artifact_path(root, manifest.get("base_artifact_path"))
    if not artifact.is_file():
        raise ValueError(f"current generation base artifact is missing: {artifact}")
    return artifact


def inspect(path: pathlib.Path) -> dict[str, object]:
    data = path.read_bytes()
    if len(data) < HEADER_SIZE:
        raise ValueError(f"{path} is too small to be a v{VERSION} .pggraph artifact")
    if data[:4] != MAGIC:
        raise ValueError(f"{path} has invalid magic bytes")

    (
        version,
        header_size,
        flags,
        node_count,
        forward_edges,
        inbound_edges,
        section_count,
    ) = struct.unpack_from("<7I", data, 4)
    if version != VERSION:
        raise ValueError(f"{path} has version {version}, expected {VERSION}; rebuild the graph")
    if header_size != HEADER_SIZE or section_count != len(SECTION_NAMES):
        raise ValueError(f"{path} has an invalid v{VERSION} header or section count")
    if flags & ~KNOWN_FLAGS:
        raise ValueError(f"{path} has unsupported flags {flags:#x}")
    if forward_edges != inbound_edges:
        raise ValueError("forward and inbound edge counts differ")
    if bool(flags & 0b001) != bool(flags & 0b010):
        raise ValueError("forward and inbound weight flags differ")
    if any(data[48:64]) or any(data[480:HEADER_SIZE]):
        raise ValueError("reserved header bytes are not zero")

    body_length = struct.unpack_from("<Q", data, 32)[0]
    if body_length != len(data) - HEADER_SIZE:
        raise ValueError("body length does not match the artifact size")
    expected_body_crc = struct.unpack_from("<I", data, BODY_CRC_OFFSET)[0]
    actual_body_crc = zlib.crc32(data[HEADER_SIZE:]) & 0xFFFFFFFF
    header = bytearray(data[:HEADER_SIZE])
    expected_header_crc = struct.unpack_from("<I", header, HEADER_CRC_OFFSET)[0]
    header[HEADER_CRC_OFFSET : HEADER_CRC_OFFSET + 4] = b"\0" * 4
    actual_header_crc = zlib.crc32(header) & 0xFFFFFFFF
    if expected_header_crc != actual_header_crc:
        raise ValueError(
            f"header CRC32 mismatch: stored={expected_header_crc:#x}, computed={actual_header_crc:#x}"
        )
    if expected_body_crc != actual_body_crc:
        raise ValueError(
            f"body CRC32 mismatch: stored={expected_body_crc:#x}, computed={actual_body_crc:#x}"
        )

    sections = []
    previous_end = HEADER_SIZE
    for index, name in enumerate(SECTION_NAMES):
        descriptor = SECTION_DESCRIPTOR_OFFSET + index * SECTION_DESCRIPTOR_SIZE
        offset, size = struct.unpack_from("<QQ", data, descriptor)
        end = offset + size
        if offset % SECTION_ALIGNMENT != 0 or offset < previous_end or end > len(data):
            raise ValueError(
                f"invalid section {index} ({name}): offset={offset}, size={size}"
            )
        if any(data[previous_end:offset]):
            raise ValueError(f"alignment padding before section {index} ({name}) is not zero")
        sections.append({"name": name, "offset": offset, "size_bytes": size})
        previous_end = end
    if previous_end != len(data):
        raise ValueError("artifact contains trailing bytes after the final section")

    return {
        "path": str(path),
        "file_size_bytes": len(data),
        "version": version,
        "header_size_bytes": header_size,
        "flags": flags,
        "node_count": node_count,
        "edge_count": forward_edges,
        "forward_edge_count": forward_edges,
        "inbound_edge_count": inbound_edges,
        "section_count": section_count,
        "header_crc32_valid": True,
        "body_crc32_valid": True,
        "crc32_valid": True,
        "sections": sections,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "artifact",
        type=pathlib.Path,
        nargs="?",
        help="Logical main.pggraph, generation artifact, or graph root",
    )
    parser.add_argument("-g", "--graph-id", help="Graph ID under PGDATA/<graph-data-dir>")
    parser.add_argument(
        "--pgdata",
        type=pathlib.Path,
        help="PostgreSQL data directory (overrides PGDATA)",
    )
    parser.add_argument(
        "--graph-data-dir",
        default=os.environ.get("GRAPH_DATA_DIR", "graph"),
        help="Graph data directory under PGDATA",
    )
    parser.add_argument(
        "--resolve-only",
        action="store_true",
        help="Print the resolved current artifact path without inspecting it",
    )
    args = parser.parse_args()

    if args.artifact and args.graph_id:
        print("error: provide either artifact path or --graph-id, not both", file=sys.stderr)
        return 1
    artifact_path = args.artifact
    if artifact_path is None:
        if not args.graph_id:
            print("error: must provide either artifact path or --graph-id", file=sys.stderr)
            return 1
        try:
            graph_id = str(uuid.UUID(args.graph_id))
        except ValueError:
            print(f"error: invalid graph id {args.graph_id!r}", file=sys.stderr)
            return 1
        graph_data_dir = pathlib.PurePath(args.graph_data_dir)
        if graph_data_dir.is_absolute() or any(
            part in {"..", "."} for part in graph_data_dir.parts
        ):
            print(f"error: invalid graph data dir {args.graph_data_dir!r}", file=sys.stderr)
            return 1
        pgdata = args.pgdata or pathlib.Path(os.environ.get("PGDATA", ""))
        if str(pgdata) in {"", "."}:
            print("error: PGDATA is not set; use --pgdata or set PGDATA", file=sys.stderr)
            return 1
        artifact_path = pgdata.joinpath(*graph_data_dir.parts, graph_id, "main.pggraph")

    try:
        if artifact_path.is_dir() or artifact_path.name == "main.pggraph":
            artifact_path = resolve_artifact(artifact_path)
        if args.resolve_only:
            print(artifact_path)
            return 0
        report = inspect(artifact_path)
    except (OSError, ValueError, struct.error) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
