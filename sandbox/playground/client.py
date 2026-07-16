"""Health-checked PostgreSQL connection lifecycle for the playground."""

from __future__ import annotations

from typing import Any, Callable

from config import PlaygroundConfig


def _connect(**kwargs: object) -> Any:
    import psycopg
    from psycopg.rows import dict_row

    return psycopg.connect(row_factory=dict_row, **kwargs)


class DatabaseClient:
    """Own a reconnectable autocommit connection instead of caching a socket."""

    def __init__(self, config: PlaygroundConfig, connector: Callable[..., Any] = _connect) -> None:
        self.config = config
        self._connector = connector
        self._connection: Any | None = None

    def connection(self) -> Any:
        connection = self._connection
        if connection is not None and not connection.closed:
            try:
                with connection.cursor() as cursor:
                    cursor.execute("SELECT 1;")
                return connection
            except Exception:
                connection.close()
        self._connection = self._connector(
            conninfo=self.config.dsn,
            autocommit=True,
            connect_timeout=10,
        )
        return self._connection

    def close(self) -> None:
        if self._connection is not None and not self._connection.closed:
            self._connection.close()
        self._connection = None
