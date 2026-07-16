"""Supported pgGraph registration and build coordination."""

from __future__ import annotations

import time

import psycopg

from config import PlaygroundConfig

GRAPH_BUSY_SQLSTATE = "PG006"


def _status(cursor: psycopg.Cursor) -> dict:
    cursor.execute("SELECT node_count, edge_count, projection_mode FROM graph.status();")
    row = cursor.fetchone()
    return dict(row) if row else {"node_count": 0, "edge_count": 0, "projection_mode": ""}


def _ensure_active_graph(cursor: psycopg.Cursor, config: PlaygroundConfig) -> None:
    deadline = time.monotonic() + config.build_wait_seconds
    attempt = 0
    while True:
        status = _status(cursor)
        if status["node_count"] > 0 and status["edge_count"] > 0 and status["projection_mode"] == config.build_mode:
            return
        try:
            if config.build_mode == "mutable_overlay":
                cursor.execute("SET graph.mutable_enabled = on;")
            cursor.execute("SELECT * FROM graph.build(%s);", (config.build_mode,))
            if cursor.description:
                cursor.fetchall()
            return
        except psycopg.Error as exc:
            if exc.sqlstate != GRAPH_BUSY_SQLSTATE:
                raise
            if time.monotonic() >= deadline:
                raise RuntimeError("Timed out waiting for graph build or vacuum to finish.") from exc
            time.sleep(min(5, 1 + attempt))
            attempt += 1


def ensure_graph_loaded(connection: psycopg.Connection, config: PlaygroundConfig) -> None:
    """Provision through public APIs and verify one bounded traversal."""
    with connection.cursor() as cursor:
        cursor.execute("CREATE EXTENSION IF NOT EXISTS graph;")
        cursor.execute("SELECT set_config('graph.query_memory_mb', '512', false);")
        cursor.execute("SELECT set_config('graph.maintenance_memory_mb', %s, false);", (str(config.maintenance_memory_mb),))
        if config.build_mode == "mutable_overlay":
            cursor.execute("SET graph.mutable_enabled = on;")
            cursor.execute("SET graph.query_freshness = off;")
        cursor.execute("SELECT count(*) AS nodes FROM panama.nodes;")
        if cursor.fetchone()["nodes"] == 0:
            raise RuntimeError("Panama tables are empty. Rerun sandbox/start_playground.sh to prepare the dataset.")
        cursor.execute("SELECT EXISTS (SELECT 1 FROM graph.registered_tables() WHERE table_name = 'panama.nodes') AS registered;")
        if not cursor.fetchone()["registered"]:
            cursor.execute("SELECT graph.reset();")
            cursor.execute("SELECT graph.add_table('panama.nodes'::regclass, 'node_id', ARRAY['name', 'countries', 'country_codes', 'label']);")
            cursor.execute("SELECT graph.add_edge(from_table := 'panama.edges'::regclass, from_column := 'start_id', to_table := 'panama.nodes'::regclass, to_column := 'end_id', label := 'related_to', bidirectional := true, label_column := 'rel_type');")
        _ensure_active_graph(cursor, config)
        cursor.execute("WITH seed AS (SELECT start_id FROM panama.edges GROUP BY start_id ORDER BY count(*) DESC LIMIT 1) SELECT count(*) FROM seed, LATERAL graph.traverse('panama.nodes'::regclass, seed.start_id, 1, hydrate := false, max_rows := 1);")
