"""Environment-backed playground configuration."""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

from queries import normalize_playground_mode


@dataclass(frozen=True)
class PlaygroundConfig:
    """Validated runtime limits and connection settings."""

    dsn: str
    mode: str
    assets_dir: Path
    statement_timeout_ms: int = 30_000
    max_result_rows: int = 1_000
    maintenance_memory_mb: int = 1_024
    build_wait_seconds: int = 600

    @property
    def build_mode(self) -> str:
        return "mutable_overlay" if self.mode == "mutable" else "csr_readonly"

    @classmethod
    def from_environment(cls) -> "PlaygroundConfig":
        dsn = os.environ.get("PGGRAPH_DSN")
        if not dsn:
            raise RuntimeError("PGGRAPH_DSN is required; start the playground with sandbox/start_playground.sh")
        timeout = int(os.environ.get("PGGRAPH_STATEMENT_TIMEOUT_MS", "30000"))
        row_limit = int(os.environ.get("PGGRAPH_MAX_RESULT_ROWS", "1000"))
        maintenance_memory = int(os.environ.get("PGGRAPH_MAINTENANCE_MEMORY_MB", "1024"))
        if timeout < 1 or row_limit < 1 or maintenance_memory < 1:
            raise ValueError("playground timeout, row limit, and maintenance memory must be positive")
        return cls(
            dsn=dsn,
            mode=normalize_playground_mode(os.environ.get("PGGRAPH_PLAYGROUND_MODE")),
            assets_dir=Path(os.environ.get("PGGRAPH_ASSETS_DIR", "assets")),
            statement_timeout_ms=timeout,
            max_result_rows=row_limit,
            maintenance_memory_mb=maintenance_memory,
        )
