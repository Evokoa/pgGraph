"""Exercise RSS gate failures without requiring a PostgreSQL installation."""

import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class BuildRssGateTests(unittest.TestCase):
    def run_gate(self, reading, *, limit=1024, existing=False, stress=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / "bin"
            tools.mkdir()
            output = root / "evidence"
            if existing:
                output.mkdir()
                (output / "rss.tsv").write_text("previous evidence\n")
            marker = root / "external-command"
            for name in ("cargo", "createdb", "dropdb"):
                path = tools / name
                path.write_text("#!/bin/sh\nprintf called >> \"$COMMAND_MARKER\"\n")
                path.chmod(0o755)
            helper = root / "psql.py"
            helper.write_text(
                "import pathlib, re, sys, time\n"
                "if '-Atc' in sys.argv:\n"
                "    print(0)\n"
                "else:\n"
                "    sql = sys.stdin.read()\n"
                "    match = re.search(r'^\\\\o (.+backend.pid)$', sql, re.M)\n"
                "    if match:\n"
                "        pathlib.Path(match[1]).write_text('99999\\n')\n"
                "        time.sleep(0.8)\n"
            )
            psql = tools / "psql"
            psql.write_text(
                f"#!/bin/sh\nexec {shlex.quote(sys.executable)} "
                f"{shlex.quote(str(helper))} \"$@\"\n"
            )
            psql.chmod(0o755)
            ps = tools / "ps"
            ps.write_text('#!/bin/sh\nprintf "%s\\n" "$RSS_READING"\n')
            ps.chmod(0o755)
            script = "build_memory_stress.sh" if stress else "measure_build_rss.sh"
            environment = dict(
                os.environ,
                PATH=str(tools) + os.pathsep + os.environ["PATH"],
                COMMAND_MARKER=str(marker),
                RSS_READING=reading,
                PG_CONFIG="/bin/true",
                OUTPUT_DIR=str(output),
                TMPDIR=str(root),
                MAX_RSS_MB=str(limit),
            )
            result = subprocess.run(
                ["bash", str(ROOT / "graph/tests/heavy" / script)],
                env=environment, text=True, capture_output=True, timeout=20,
            )
            retained = {p.name: p.read_text() for p in output.glob("*") if p.is_file()}
            return result, retained, marker.exists()

    def test_missing_or_zero_samples_fail_and_retain_build_output(self):
        for reading in ("", "0"):
            with self.subTest(reading=reading):
                result, retained, _ = self.run_gate(reading)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("no positive backend RSS sample", result.stderr)
                self.assertIn("build.out", retained)

    def test_positive_samples_pass_only_within_the_rss_limit(self):
        result, retained, _ = self.run_gate("2048")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Peak backend RSS: 2MB", result.stdout)
        self.assertIn("\t2048\n", retained["rss.tsv"])
        result, retained, _ = self.run_gate("2048", limit=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exceeded MAX_RSS_MB=1MB", result.stdout)
        self.assertIn("\t2048\n", retained["rss.tsv"])

    def test_existing_evidence_is_rejected_before_external_work(self):
        for stress in (False, True):
            with self.subTest(stress=stress):
                result, retained, called = self.run_gate("2048", existing=True, stress=stress)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(called)
                self.assertEqual(retained, {"rss.tsv": "previous evidence\n"})
