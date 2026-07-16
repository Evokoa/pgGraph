"""Unit tests for the dependency-free playground catalog and formatting."""

from __future__ import annotations

import unittest
from pathlib import Path

from catalog import query_examples
from client import DatabaseClient
from config import PlaygroundConfig
from execution import run_with_error_handling
from results import format_elapsed


class FakeCursor:
    description = None
    statusmessage = "SELECT 1"

    def __init__(self, *, fail: bool = False) -> None:
        self.fail = fail

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def execute(self, _sql, _params=None) -> None:
        if self.fail:
            raise RuntimeError("broken connection")

    def fetchone(self):
        return {"set_config": "30000"}

    def nextset(self) -> bool:
        return False


class FakeConnection:
    def __init__(self, *, fail_health: bool = False) -> None:
        self.closed = False
        self.fail_health = fail_health

    def cursor(self) -> FakeCursor:
        fail, self.fail_health = self.fail_health, False
        return FakeCursor(fail=fail)

    def close(self) -> None:
        self.closed = True


class CatalogTests(unittest.TestCase):
    def test_ids_are_stable_and_unique(self) -> None:
        examples = query_examples("csr")
        self.assertEqual(len(examples), len({example.id for example in examples}))
        self.assertTrue(all(example.statements for example in examples))

    def test_multi_statement_examples_have_explicit_boundaries(self) -> None:
        examples = {example.title: example for example in query_examples("csr")}
        self.assertEqual(len(examples["Status + Catalog"].statements), 3)
        self.assertEqual(len(examples["Component Stats"].statements), 2)

    def test_modes_filter_mutable_examples(self) -> None:
        csr = {example.title for example in query_examples("csr")}
        mutable = {example.title for example in query_examples("mutable")}
        self.assertNotIn("Mutable GQL Merge Node", csr)
        self.assertIn("Mutable GQL Merge Node", mutable)

    def test_elapsed_format(self) -> None:
        self.assertEqual(format_elapsed(0.012), "12 ms")
        self.assertEqual(format_elapsed(1.25), "1.25 s")

    def test_client_reconnects_after_failed_health_check(self) -> None:
        config = PlaygroundConfig("postgresql://example", "csr", Path("assets"))
        connections = [FakeConnection(fail_health=True), FakeConnection()]
        client = DatabaseClient(config, connector=lambda **_kwargs: connections.pop(0))
        first = client.connection()
        self.assertIsNot(first, client.connection())
        self.assertTrue(first.closed)

    def test_execution_returns_bounded_error_shape(self) -> None:
        config = PlaygroundConfig("postgresql://example", "csr", Path("assets"))
        result = run_with_error_handling(FakeConnection(fail_health=True), ("SELECT 1",), config)
        self.assertFalse(result["ok"])
        self.assertIn("broken connection", result["error"])


if __name__ == "__main__":
    unittest.main()
