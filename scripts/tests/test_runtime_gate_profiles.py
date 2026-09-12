"""Exercise gate profile boundaries with synthetic external commands only."""
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "graph/tests/heavy"


def executable(path, body):
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)


class RuntimeGateProfileTests(unittest.TestCase):
    def test_isolation_scope_defaults_and_explicit_persisted_full_profile(self):
        for persist, requested, expected in (("off", None, "true"), ("on", None, "false"),
                                              ("on", "true", "true"), ("off", "false", "false")):
            with self.subTest(persist=persist, requested=requested), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                executable(root / "cargo", "exit 97\n")
                environment = {"PATH": str(root) + ":/usr/bin:/bin", "TMPDIR": str(root),
                               "PERSIST_ON_BUILD": persist, "PG_CONFIG": "/bin/false"}
                if requested is not None:
                    environment["FULL_PROFILE"] = requested
                result = subprocess.run(["bash", "-x", str(HEAVY / "gql_isolation_matrix.sh")],
                                        env=environment, capture_output=True, text=True, timeout=5)
                # The real shell resolves the profile before reaching the mock
                # install barrier. No PostgreSQL or Cargo operation executes.
                self.assertEqual(result.returncode, 97, result.stderr)
                resolved = re.findall(r"^\+ FULL_PROFILE=(true|false)$", result.stderr, re.M)
                self.assertEqual(resolved[-1], expected)
                self.assertFalse(list(root.glob("pggraph-gql-isolation.*")))

    def test_invalid_isolation_profile_refuses_before_external_work(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable(root / "cargo", 'touch "$TMPDIR/cargo-called"; exit 97\n')
            for field, value in (("FULL_PROFILE", "yes"), ("FULL_PROFILE", "1"),
                                 ("PERSIST_ON_BUILD", "yes")):
                with self.subTest(field=field, value=value):
                    environment = {"PATH": str(root) + ":/usr/bin:/bin", "TMPDIR": str(root),
                                   "PG_CONFIG": "/bin/false", field: value}
                    result = subprocess.run(["bash", str(HEAVY / "gql_isolation_matrix.sh")],
                                            env=environment, capture_output=True, text=True, timeout=5)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn(field + " must be", result.stderr)
                    self.assertFalse((root / "cargo-called").exists())
                    self.assertFalse(list(root.glob("pggraph-gql-isolation.*")))

    def pgbench_fixture(self, *, apply_ms=1, oracle_failure=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            calls = root / "calls.jsonl"
            helper = root / "psql.py"
            helper.write_text('''import json, os, pathlib, sys
sql = sys.stdin.read()
with pathlib.Path(os.environ['CALLS']).open('a') as calls:
    calls.write(json.dumps({'argv': sys.argv[1:], 'sql': sql}) + '\\n')
if 'FROM pgbench_measurement;' in sql:
    print(os.environ['APPLY_MS'] + '|1|1|1|1|4|0')
elif any('SELECT count(*) FROM graph._sync_log' in arg for arg in sys.argv):
    print(4)
elif 'post-stress graph retained' in sql and os.environ['ORACLE_FAILURE'] == '1':
    print('synthetic oracle PG007', file=sys.stderr)
    raise SystemExit(1)
''')
            executable(root / "psql", f"exec {shlex.quote(sys.executable)} {shlex.quote(str(helper))} \"$@\"\n")
            executable(root / "pgbench", 'printf "%s\\n" "$*" > "$TMPDIR/pgbench-arguments"\n')
            environment = {"PATH": str(root) + ":/usr/bin:/bin", "TMPDIR": str(root),
                           "CREATE_DB": "0", "DBNAME": "synthetic_fixture", "CALLS": str(calls),
                           "APPLY_MS": str(apply_ms), "ORACLE_FAILURE": str(int(oracle_failure))}
            result = subprocess.run(["bash", str(HEAVY / "run_pgbench_sync.sh")],
                                    env=environment, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=10)
            return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def test_pgbench_oracle_budget_is_local_and_after_timed_checks(self):
        result, calls = self.pgbench_fixture()
        self.assertEqual(result.returncode, 0, result.stderr)
        oracle = calls[-1]["sql"]
        self.assertIn("BEGIN;", oracle)
        self.assertIn("SET LOCAL graph.query_memory_mb = 256;", oracle)
        self.assertIn("COMMIT;", oracle)
        self.assertLess(oracle.index("BEGIN;"), oracle.index("SET LOCAL"))
        self.assertLess(oracle.index("post-stress graph retained"), oracle.index("COMMIT;"))
        self.assertTrue(any("FROM pgbench_measurement;" in call["sql"] for call in calls[:-1]))
        self.assertFalse(any("query_memory_mb" in call["sql"] for call in calls[:-1]))
        self.assertIn("<> 10000", oracle)
        self.assertIn("<> 9999", oracle)
        self.assertIn(">= 10000001", oracle)

    def test_pgbench_timed_failure_prevents_oracle_and_oracle_failure_stays_failed(self):
        result, calls = self.pgbench_fixture(apply_ms=6000)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("apply_sync exceeded threshold", result.stdout)
        self.assertFalse(any("SET LOCAL graph.query_memory_mb" in call["sql"] for call in calls))
        result, _ = self.pgbench_fixture(oracle_failure=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PG007", result.stderr)
        self.assertNotIn("stress passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
