"""Unit tests for reproducible playground dataset preparation."""

from __future__ import annotations

import csv
import importlib.util
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
COMMON_RUNNER = ROOT / "sandbox" / "common" / "run_benchmarks.py"
SPEC = importlib.util.spec_from_file_location("pggraph_dataset_runner", COMMON_RUNNER)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"could not load {COMMON_RUNNER}")
RUNNER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUNNER
SPEC.loader.exec_module(RUNNER)

TEMPORARY_ROOT = ROOT / ".temporary_files"


def node_csv(name: str, node_id: str) -> str:
    return (
        "node_id,name,countries,country_codes,sourceID,valid_until\n"
        f"{node_id},{name},Taiwan,TWN,Panama Papers,The Panama Papers data is current through 2015\n"
    )


def write_fixture_archive(
    archive_path: Path,
    *,
    officer_name: str = "Peng Wan-Hsiung",
    intermediary_name: str | None = None,
) -> None:
    intermediary_name = intermediary_name or officer_name
    relationships = "START_ID,END_ID,TYPE,link,start_date,end_date\n51122,2,officer_of,,,\n"
    with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        # Write the lower-priority category first to prove archive member order
        # does not decide the canonical label.
        archive.writestr("nodes-intermediaries.csv", node_csv(intermediary_name, "51122"))
        archive.writestr("nodes-officers.csv", node_csv(officer_name, "51122"))
        archive.writestr("nodes-entities.csv", node_csv("Example Entity", "2"))
        archive.writestr("relationships.csv", relationships)


def normalized_nodes(work_dir: Path) -> dict[str, dict[str, str]]:
    with (work_dir / "normalized" / "nodes.csv").open(newline="", encoding="utf-8") as handle:
        return {row["node_id"]: row for row in csv.DictReader(handle)}


class DatasetTests(unittest.TestCase):
    """Verify the pinned snapshot and Panama normalization invariants."""

    @classmethod
    def setUpClass(cls) -> None:
        TEMPORARY_ROOT.mkdir(exist_ok=True)

    def test_panama_snapshot_uses_fixed_benchmark_release(self) -> None:
        dataset = RUNNER.DATASETS["panama"]
        self.assertEqual(
            dataset.url,
            "https://github.com/Evokoa/pgGraph/releases/download/"
            "benchmark-data-icij-2026-07-29/icij-offshore-leaks-2026-07-29.zip",
        )
        self.assertNotIn("LATEST", dataset.url)
        self.assertEqual(
            dataset.expected_sha256,
            "34475194b6a8c2d683fddc55cca02f88f08f0a538521fb13a324975221624380",
        )

    def test_transform_deduplicates_identical_cross_category_nodes(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            work_dir = Path(temporary_dir)
            archive_path = work_dir / "fixture.zip"
            write_fixture_archive(archive_path)

            metadata = RUNNER.transform_panama(archive_path, work_dir)
            nodes = normalized_nodes(work_dir)

            self.assertEqual(metadata["node_count"], 2)
            self.assertEqual(metadata["duplicate_node_id_count"], 1)
            self.assertEqual(metadata["duplicate_node_row_count"], 1)
            self.assertEqual(nodes["51122"]["label"], "officers")
            self.assertEqual(nodes["51122"]["name"], "Peng Wan-Hsiung")

    def test_transform_rejects_conflicting_duplicate_nodes(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            work_dir = Path(temporary_dir)
            archive_path = work_dir / "fixture.zip"
            write_fixture_archive(archive_path, intermediary_name="Conflicting Name")

            with self.assertRaisesRegex(RuntimeError, "node_id '51122' has conflicting values"):
                RUNNER.transform_panama(archive_path, work_dir)

    def test_transform_cache_is_keyed_by_archive_and_version(self) -> None:
        with tempfile.TemporaryDirectory(dir=TEMPORARY_ROOT) as temporary_dir:
            work_dir = Path(temporary_dir)
            archive_path = work_dir / "fixture.zip"
            write_fixture_archive(archive_path, officer_name="First Name")
            first = RUNNER.transform_panama(archive_path, work_dir)

            write_fixture_archive(archive_path, officer_name="Second Name")
            second = RUNNER.transform_panama(archive_path, work_dir)

            self.assertNotEqual(first["archive_sha256"], second["archive_sha256"])
            self.assertEqual(second["transform_version"], RUNNER.PANAMA_TRANSFORM_VERSION)
            self.assertEqual(normalized_nodes(work_dir)["51122"]["name"], "Second Name")


if __name__ == "__main__":
    unittest.main()
