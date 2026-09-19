"""Persistent psql sessions for production-feature concurrency regressions."""

import os
import selectors
import subprocess
import time
import uuid


class Session:
    def __init__(self, database):
        self.process = subprocess.Popen(
            ["psql", "-X", "-qAt", "-v", "ON_ERROR_STOP=1", "-d", database],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.pending = b""
        self.execute("SET client_min_messages = error;")

    def execute(self, sql, timeout=30):
        marker = ("PGGRAPH_DONE_" + uuid.uuid4().hex).encode()
        self.process.stdin.write(sql.encode() + b"\n\\echo " + marker + b"\n")
        self.process.stdin.flush()
        deadline = time.monotonic() + timeout
        lines = []
        while True:
            while b"\n" in self.pending:
                line, self.pending = self.pending.split(b"\n", 1)
                if line.strip() == marker:
                    return b"\n".join(lines).decode()
                lines.append(line)
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not self.selector.select(remaining):
                raise TimeoutError("psql regression command timed out")
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError(b"\n".join(lines).decode() + self.pending.decode())
            self.pending += chunk

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
        self.process.wait(timeout=10)
        self.selector.close()
        self.process.stdin.close()
        self.process.stdout.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
