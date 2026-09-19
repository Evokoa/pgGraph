"""Run PostgreSQL gate cleanup paths with controlled local shell commands."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def executable(path, body):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('#!/bin/bash\nset -euo pipefail\n' + body)
    path.chmod(0o755)


class ProcessCleanupTests(unittest.TestCase):
    def run_upgrade(self, root, stop=0, sql=0, start=0, signal='', fsync='on'):
        work = root / 'work'
        (work / 'old').mkdir(parents=True)
        (work / 'old/PG_VERSION').write_text('17\n')
        (work / 'sentinel').touch()
        tools = root / 'bin'
        executable(tools / 'python3', 'cat >/dev/null\necho "55001 55002"\n')
        for major in ('old', 'new'):
            bindir = root / major
            executable(bindir / 'pg_ctl', '''printf '%s %s\\n' "$(basename "$(dirname "$0")")" "${!#}" >> "$FIXTURE/calls"
printf '%s\\n' "$*" >> "$FIXTURE/start-options"
case "${!#}" in start) exit "$START_CODE";; stop) exit "$STOP_CODE";; esac
''')
            executable(bindir / 'createdb', ':\n')
            executable(bindir / 'psql', '''sql="$(cat)"
printf '%s\\n' "$sql" >> "$FIXTURE/sql-input"
if [[ "$sql" == *"current_setting('fsync')"* && "$FSYNC" != on ]]; then exit 1; fi
if [[ -n "$SIGNAL" ]]; then kill -"$SIGNAL" "$PPID"; fi
exit "$SQL_CODE"
''')
            executable(bindir / 'pg_controldata', 'echo "Data page checksum version: 0"\n')
            executable(bindir / 'initdb', '''if [[ "${1:-}" == --help ]]; then echo --no-data-checksums; exit; fi
mkdir -p "${!#}"
''')
            executable(bindir / 'pg_upgrade', ':\n')
        env = dict(os.environ, PATH=str(tools) + ':/usr/bin:/bin', FIXTURE=str(root),
                   OLD_BINDIR=str(root / 'old'), NEW_BINDIR=str(root / 'new'),
                   OLD_DATADIR=str(work / 'old'), NEW_DATADIR=str(work / 'new'),
                   PGGRAPH_UPGRADE_SENTINEL=str(work / 'sentinel'), STOP_CODE=str(stop),
                   START_CODE=str(start), SQL_CODE=str(sql), SIGNAL=signal, FSYNC=fsync)
        result = subprocess.run(['bash', str(ROOT / 'graph/tests/heavy/pg_upgrade_validate.sh')],
                                env=env, text=True, capture_output=True, timeout=10)
        return result, (root / 'calls').read_text().splitlines(), work

    def test_upgrade_preserves_command_error_and_failed_start_cleanup(self):
        for stop, sql, start, expected in ((0, 0, 0, 0), (1, 0, 0, 1), (1, 7, 0, 7), (1, 0, 5, 5)):
            with self.subTest(stop=stop, sql=sql, start=start), tempfile.TemporaryDirectory() as directory:
                result, calls, work = self.run_upgrade(Path(directory), stop, sql, start)
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertTrue(work.is_dir())
                self.assertIn('old stop', calls)
                if stop:
                    self.assertIn('shutdown failed', result.stderr)
                else:
                    self.assertEqual(calls, ['old start', 'old stop', 'new start', 'new stop'])

    def test_upgrade_signal_exits_once_and_preserves_failure_status(self):
        for signal, expected in (('INT', 130), ('TERM', 143)):
            with self.subTest(signal=signal), tempfile.TemporaryDirectory() as directory:
                result, calls, work = self.run_upgrade(Path(directory), stop=1, signal=signal)
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(calls, ['old start', 'old stop'])
                self.assertTrue(work.is_dir())

    def test_upgrade_matrix_cleanup_retains_failed_owned_directory(self):
        # Exercise the maintained cleanup/trap block without installing into
        # system PostgreSQL directories or running the matrix itself.
        script = (ROOT / 'graph/tests/heavy/run_pg_upgrade_matrix.sh').read_text()
        start = script.index('  cleanup() {')
        end = script.index('  "$old_bindir/initdb"', start)
        cleanup = script[start:end]
        for status in (0, 1, 7, 130, 143):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                work = Path(directory) / 'owned'
                work.mkdir()
                (work / 'PG_VERSION').write_text('17\n')
                result = subprocess.run(['bash', '-c', 'set -euo pipefail; workdir="$1";\n' +
                                         cleanup + '\nexit "$2"', 'cleanup', str(work), str(status)],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertEqual(work.exists(), status != 0)

    def run_sanitizer(self, root, stop, sql, fsync='on'):
        tools = root / 'bin'
        executable(tools / 'cargo', ':\n')
        executable(tools / 'pg_config', ':\n')
        executable(tools / 'createdb', ':\n')
        executable(tools / 'postgres', ':\n')
        executable(tools / 'initdb', '''while [[ "$1" != -D ]]; do shift; done
mkdir -p "$2"
printf 'valid' > "$2/artifact"
printf '%s\\n' "$2" > "$FIXTURE/data-path"
''')
        executable(tools / 'pg_isready', '[[ -f "$FIXTURE/server-pid" ]]\n')
        executable(tools / 'pg_ctl', '''printf 'stop\\n' >> "$FIXTURE/calls"
kill -TERM "$(cat "$FIXTURE/server-pid")"
exit "$STOP_CODE"
''')
        executable(tools / 'valgrind', '''printf '%s\\n' "$$" > "$FIXTURE/server-pid"
printf '%s\\n' "$*" > "$FIXTURE/start-options"
for arg in "$@"; do
  case "$arg" in --log-file=*) logfile="${arg#--log-file=}";; esac
done
printf 'ERROR SUMMARY: 0 errors\\n' > "${logfile//%p/$$}"
trap 'exit 0' TERM
while :; do sleep 0.05; done
''')
        executable(tools / 'python3', '''printf '%s/artifact\\n' "$(cat "$FIXTURE/data-path")"
''')
        executable(tools / 'psql', '''if [[ "$*" != *' -c '* && "$*" != *' -f '* ]]; then
  sql="$(cat)"
  printf '%s\\n' "$sql" >> "$FIXTURE/sql-input"
  if [[ "$sql" == *"current_setting('fsync')"* && "$FSYNC" != on ]]; then exit 1; fi
fi
if [[ "$SQL_CODE" != 0 ]]; then exit "$SQL_CODE"; fi
case "$*" in
*'graph.load_graph'*) [[ "$(od -An -tu1 -N1 "$(cat "$FIXTURE/data-path")/artifact" | tr -d ' ')" != 0 ]];;
*'graph.current_graph()'*|*'SELECT oid'*|*'SELECT count(*)'*) echo 1;;
esac
''')
        env = dict(os.environ, PATH=str(tools) + ':/usr/bin:/bin', FIXTURE=str(root),
                   PG_CONFIG=str(tools / 'pg_config'), PGGRAPH_TEST_TMPDIR=str(root),
                   STOP_CODE=str(stop), SQL_CODE=str(sql), FSYNC=fsync)
        result = subprocess.run(['bash', str(ROOT / 'graph/tests/heavy/run_postgres_process_sanitizer.sh')],
                                env=env, text=True, capture_output=True, timeout=15)
        return result, list(root.glob('pggraph-process-sanitizer.*')), (root / 'calls').read_text().splitlines()

    def test_live_fsync_check_refuses_disabled_durability(self):
        for gate in ('upgrade', 'sanitizer'):
            for fsync in ('on', 'off'):
                with self.subTest(gate=gate, fsync=fsync), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    if gate == 'upgrade':
                        result, _, _ = self.run_upgrade(root, fsync=fsync)
                    else:
                        result, _, _ = self.run_sanitizer(root, 0, 0, fsync=fsync)
                    self.assertEqual(result.returncode, int(fsync != 'on'), result.stderr)
                    self.assertNotIn('-F', (root / 'start-options').read_text().split())
                    sql = (root / 'sql-input').read_text()
                    self.assertIn("current_setting('fsync') <> 'on'", sql)
                    self.assertIn("RAISE EXCEPTION 'Gate requires fsync=on'", sql)

    def test_sanitizer_stop_failure_is_not_erased_after_pid_is_cleared(self):
        for stop, sql, expected in ((0, 0, 0), (1, 0, 1), (1, 7, 7), (0, 7, 7)):
            with self.subTest(stop=stop, sql=sql), tempfile.TemporaryDirectory() as directory:
                result, workdirs, calls = self.run_sanitizer(Path(directory), stop, sql)
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(len(workdirs), int(expected != 0))
                self.assertEqual(calls, ['stop'])
                if stop:
                    self.assertIn('shutdown failed', result.stderr)
                if expected:
                    self.assertTrue((workdirs[0] / 'data/artifact').exists())


if __name__ == '__main__':
    unittest.main()
