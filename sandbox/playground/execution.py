"""Bounded execution and result formatting for playground examples."""

from __future__ import annotations

import time
from typing import Any, Iterable

from config import PlaygroundConfig
from results import format_elapsed


def run_statements(connection: Any, statements: Iterable[str], config: PlaygroundConfig) -> dict:
    started = time.perf_counter()
    result_sets: list[dict] = []
    messages: list[str] = []
    with connection.cursor() as cursor:
        previous_timeout: str | None = None
        timeout_changed = False
        try:
            cursor.execute(
                "SELECT current_setting('statement_timeout') AS statement_timeout;"
            )
            previous_timeout = cursor.fetchone()["statement_timeout"]
            cursor.execute("SELECT set_config('statement_timeout', %s, false);", (str(config.statement_timeout_ms),))
            timeout_changed = True
            cursor.fetchone()
            for statement in statements:
                cursor.execute(statement)
                while True:
                    if cursor.description:
                        rows = [dict(row) for row in cursor.fetchmany(config.max_result_rows + 1)]
                        truncated = len(rows) > config.max_result_rows
                        rows = rows[: config.max_result_rows]
                        result_sets.append({"index": len(result_sets) + 1, "row_count": len(rows), "rows": rows, "truncated": truncated})
                    elif cursor.statusmessage:
                        messages.append(cursor.statusmessage)
                    if not cursor.nextset():
                        break
        finally:
            if timeout_changed and previous_timeout is not None:
                cursor.execute(
                    "SELECT set_config('statement_timeout', %s, false);",
                    (previous_timeout,),
                )
                cursor.fetchone()
    elapsed = time.perf_counter() - started
    return {"ok": True, "elapsed_seconds": elapsed, "elapsed": format_elapsed(elapsed), "result_sets": result_sets, "messages": messages or ["Query completed."]}


def run_with_error_handling(connection: Any, statements: Iterable[str], config: PlaygroundConfig) -> dict:
    started = time.perf_counter()
    try:
        return run_statements(connection, statements, config)
    except Exception as exc:
        elapsed = time.perf_counter() - started
        return {"ok": False, "elapsed_seconds": elapsed, "elapsed": format_elapsed(elapsed), "error": f"{type(exc).__name__}: {exc}", "result_sets": [], "messages": []}
