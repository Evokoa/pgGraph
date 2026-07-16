"""Release-runner environment isolation tests."""

from __future__ import annotations

import os
import unittest
from unittest.mock import patch

import run_release


class GateEnvironmentTests(unittest.TestCase):
    def test_undeclared_release_controls_are_removed(self) -> None:
        caller = {
            "PATH": "/usr/bin",
            "HOME": "/tmp/home",
            "RUN_INSTALL": "0",
            "PG_VERSIONS": "17",
            "PGHOST": "unexpected",
            "MAX_RSS_MB": "1",
        }
        gate = {"environment": {"RUN_INSTALL": "1", "PG_VERSIONS": "14 15 16 17 18"}}
        with patch.dict(os.environ, caller, clear=True):
            environment = run_release.gate_environment(gate)
        self.assertEqual(environment["RUN_INSTALL"], "1")
        self.assertEqual(environment["PG_VERSIONS"], "14 15 16 17 18")
        self.assertNotIn("PGHOST", environment)
        self.assertNotIn("MAX_RSS_MB", environment)
        self.assertEqual(environment["PATH"], "/usr/bin")

    def test_undeclared_control_does_not_reach_gate(self) -> None:
        with patch.dict(os.environ, {"RUN_PLAYGROUND": "0", "PGPORT": "9999"}, clear=True):
            environment = run_release.gate_environment({})
        self.assertNotIn("RUN_PLAYGROUND", environment)
        self.assertNotIn("PGPORT", environment)


if __name__ == "__main__":
    unittest.main()
