"""Check publication with production features and independent live sessions.

Uses PGHOST, PGPORT and PGUSER. Creates fresh databases and retains them for
inspection. Run after installing the extension without pg_test features.
"""

import os
import re
import subprocess
import uuid

from psql_session import Session


QUERY = "SELECT string_agg(node_id, ',' ORDER BY node_id) FROM graph.traverse('n'::regclass, 'a', 1);"
UNBUILT = """
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph.traverse('n'::regclass, 'a', 1);
    RAISE EXCEPTION 'unpublished graph was served';
  EXCEPTION WHEN SQLSTATE '55000' THEN
    IF SQLERRM NOT LIKE 'Graph not built%' THEN RAISE; END IF;
  END;
END $$;
"""


def create_database(suffix):
    prefix = os.environ.get("DB_PREFIX", "pggraph_generation_" + uuid.uuid4().hex[:10])
    name = prefix + "_" + suffix
    if len(name) > 63 or not re.fullmatch(r"[a-zA-Z_][a-zA-Z0-9_]*", name):
        raise ValueError("invalid regression database name")
    subprocess.run(["createdb", name], check=True)
    print("Regression database:", name, flush=True)
    with Session(name) as setup:
        setup.execute("""
CREATE EXTENSION graph;
CREATE TABLE n(id text PRIMARY KEY);
CREATE TABLE e(id serial PRIMARY KEY, src text, dst text);
INSERT INTO n VALUES ('a'), ('b'), ('c');
INSERT INTO e(src, dst) VALUES ('a', 'b');
SELECT graph.add_table('n'::regclass, 'id');
SELECT graph.add_edge('e'::regclass, 'src', 'n'::regclass, 'dst', 'link', false);
""")
    return name


def first_build_rollback():
    database = create_database("abort")
    with Session(database) as builder, Session(database) as reader:
        builder.execute("BEGIN; SELECT * FROM graph.build();")
        reader.execute(UNBUILT)
        builder.execute("ROLLBACK;")
        builder.execute(UNBUILT)
        reader.execute(UNBUILT)
        builder.execute("SELECT * FROM graph.build();")
        assert reader.execute(QUERY) == "a,b"
    with Session(database) as fresh:
        assert fresh.execute(QUERY) == "a,b"


def resident_generation_refresh():
    database = create_database("refresh")
    with Session(database) as writer, Session(database) as reader:
        writer.execute("SET graph.mutable_enabled = on; SELECT * FROM graph.build(mode := 'mutable_overlay');")
        assert reader.execute(QUERY) == "a,b"
        reader.execute("SET graph.auto_load = off;")
        writer.execute("INSERT INTO e(src, dst) VALUES ('a', 'c');")
        writer.execute("SELECT * FROM graph.ingest_projection();")
        assert reader.execute(QUERY) == "a,b,c"
        writer.execute("DELETE FROM e WHERE dst = 'b';")
        writer.execute("SELECT * FROM graph.build();")
        assert reader.execute(QUERY) == "a,c"


def publication_cannot_be_forged():
    database = create_database("capability")
    with Session(database) as session:
        session.execute("""
DO $$ BEGIN
  BEGIN
    PERFORM graph._publish_generation_for_current_role();
    RAISE EXCEPTION 'publication accepted without a pending candidate';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
END $$;
""")
        session.execute(UNBUILT)


def old_snapshot_first_load():
    database = create_database("snapshot")
    with Session(database) as writer, Session(database) as reader:
        writer.execute("SELECT * FROM graph.build();")
        reader.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        assert reader.execute("SELECT count(*) FROM e;") == "1"
        writer.execute("DELETE FROM e;")
        writer.execute("SELECT * FROM graph.build();")
        writer.execute("SET graph.projection_retention_generations = 1;")
        writer.execute("SELECT * FROM graph.projection_gc();")
        assert reader.execute(QUERY) == "a,b"
        reader.execute("COMMIT;")
        assert reader.execute(QUERY) == "a"


def imported_snapshot_first_load():
    database = create_database("import")
    with Session(database) as writer, Session(database) as exporter, Session(database) as reader:
        writer.execute("SELECT * FROM graph.build();")
        exporter.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        snapshot = exporter.execute("SELECT pg_export_snapshot();")
        assert re.fullmatch(r"[0-9A-Fa-f-]+", snapshot)
        reader.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        reader.execute("SET TRANSACTION SNAPSHOT '" + snapshot + "';")
        exporter.execute("COMMIT;")
        writer.execute("DELETE FROM e; SELECT * FROM graph.build();")
        writer.execute("SET graph.projection_retention_generations = 1;")
        assert writer.execute("SELECT deleted_files FROM graph.projection_gc();") == "0"
        assert reader.execute(QUERY) == "a,b"
        reader.execute("COMMIT;")
        assert reader.execute(QUERY) == "a"


def statement_rollback():
    database = create_database("statement")
    with Session(database) as writer, Session(database) as reader:
        writer.execute("""
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph.build();
    RAISE EXCEPTION 'abort candidate';
  EXCEPTION WHEN raise_exception THEN NULL;
  END;
END $$;
""")
        writer.execute(UNBUILT)
        reader.execute(UNBUILT)
        writer.execute("SELECT * FROM graph.build();")
        writer.execute("SET graph.query_freshness = off; DELETE FROM e;")
        reader.execute("SET graph.query_freshness = off;")
        writer.execute("BEGIN; SAVEPOINT candidate; SELECT * FROM graph.build();")
        assert writer.execute(QUERY) == "a"
        assert reader.execute(QUERY) == "a,b"
        writer.execute("ROLLBACK TO candidate;")
        assert writer.execute(QUERY) == "a,b"
        writer.execute("COMMIT;")


def reclamation_after_commit():
    database = create_database("reclaim")
    with Session(database) as writer, Session(database) as old:
        writer.execute("SELECT * FROM graph.build();")
        old.execute("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT count(*) FROM e;")
        writer.execute("INSERT INTO e(src, dst) VALUES ('a', 'c'); SELECT * FROM graph.build();")
        writer.execute("SET graph.projection_retention_generations = 1;")
        assert writer.execute("SELECT count(*) > 0 FROM graph._sync_log;") == "t"
        assert writer.execute("SELECT deleted_files FROM graph.projection_gc();") == "0"
        assert writer.execute("SELECT count(*) > 0 FROM graph._sync_log;") == "t"
        old.execute("COMMIT;")
        assert int(writer.execute("SELECT deleted_files FROM graph.projection_gc();")) > 0
        assert writer.execute("SELECT count(*) FROM graph._sync_log;") == "0"
        assert writer.execute(QUERY) == "a,b,c"


def cold_resident_and_drop():
    database = create_database("cold_drop")
    status = "SELECT artifact_exists FROM graph.graph_runtime_status() WHERE graph_name = 'cold';"
    with Session(database) as writer, Session(database) as reader:
        writer.execute("""
SELECT graph.create_graph('cold', residency := 'cold');
SELECT graph.set_current_graph('cold');
SELECT graph.add_table('n'::regclass, 'id');
SELECT graph.add_edge('e'::regclass, 'src', 'n'::regclass, 'dst', 'link', false);
SELECT * FROM graph.build();
""")
        reader.execute("SELECT graph.set_current_graph('cold'); SELECT * FROM graph.load_graph('cold');")
        reader.execute("SET graph.query_freshness = off;")
        assert reader.execute(QUERY) == "a,b"
        writer.execute("DELETE FROM e; SELECT * FROM graph.build();")
        assert reader.execute(QUERY) == "a"
        writer.execute("SELECT graph.remove_edge('link'); SELECT graph.remove_table('n'::regclass);")
        reader.execute("SELECT graph.set_current_graph('default'); BEGIN ISOLATION LEVEL REPEATABLE READ;")
        assert reader.execute(status) == "t"
        writer.execute("BEGIN; SELECT * FROM graph.drop_graph('cold');")
        assert reader.execute(status) == "t"
        writer.execute("ROLLBACK;")
        assert writer.execute(status) == "t"
        writer.execute("SELECT * FROM graph.drop_graph('cold');")
        assert reader.execute(status) == "t"
        reader.execute("COMMIT;")
        assert reader.execute(status) == ""


def fixed_snapshot_cannot_publish():
    database = create_database("fixed_publish")
    with Session(database) as writer:
        writer.execute("SELECT * FROM graph.build();")
        generation = writer.execute("SELECT manifest_generation FROM graph.projection_status();")
        writer.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        writer.execute("""
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph.build();
    RAISE EXCEPTION 'fixed snapshot published a durable generation';
  EXCEPTION WHEN feature_not_supported THEN NULL;
  END;
END $$;
""")
        assert writer.execute("SELECT manifest_generation FROM graph.projection_status();") == generation
        writer.execute("ROLLBACK; SELECT * FROM graph.build();")
        assert int(writer.execute("SELECT manifest_generation FROM graph.projection_status();")) > int(generation)


def shared_source_retention():
    database = create_database("shared")
    with Session(database) as writer, Session(database) as reader:
        writer.execute("DELETE FROM e; SELECT * FROM graph.build();")
        writer.execute("""
SELECT graph.create_graph('other');
SELECT graph.set_current_graph('other');
SELECT graph.add_table('n'::regclass, 'id');
SELECT graph.add_edge('e'::regclass, 'src', 'n'::regclass, 'dst', 'link', false);
SELECT * FROM graph.build();
SELECT graph.set_current_graph('default');
INSERT INTO e(src, dst) VALUES ('a', 'b');
SELECT * FROM graph.build();
SELECT * FROM graph.projection_gc();
""")
        reader.execute("SELECT graph.set_current_graph('other');")
        assert reader.execute(QUERY) == "a,b", "GC removed another graph's unapplied sync row"


def shared_root_retention():
    database = create_database("roots")
    alternate = "graph-regression-" + uuid.uuid4().hex
    with Session(database) as writer, Session(database) as reader:
        writer.execute("DELETE FROM e; SELECT * FROM graph.build();")
        writer.execute("SET graph.data_dir = '" + alternate + "'; SELECT * FROM graph.build();")
        writer.execute("RESET graph.data_dir; INSERT INTO e(src, dst) VALUES ('a', 'b');")
        writer.execute("SELECT * FROM graph.build(); SELECT * FROM graph.projection_gc();")
        reader.execute("SET graph.data_dir = '" + alternate + "';")
        assert reader.execute(QUERY) == "a,b", "GC removed another root's unapplied sync row"


def resident_root_switch():
    database = create_database("root_switch")
    alternate = "graph-regression-" + uuid.uuid4().hex
    with Session(database) as writer, Session(database) as reader:
        writer.execute("SELECT * FROM graph.build();")
        reader.execute("SET graph.query_freshness = off;")
        assert reader.execute(QUERY) == "a,b"
        writer.execute("DELETE FROM e; INSERT INTO e(src, dst) VALUES ('a', 'c');")
        writer.execute("SET graph.data_dir = '" + alternate + "'; SELECT * FROM graph.build();")
        reader.execute("SET graph.data_dir = '" + alternate + "';")
        assert reader.execute(QUERY) == "a,c", "equal generation IDs hid a root change"
        reader.execute("RESET graph.data_dir;")
        assert reader.execute(QUERY) == "a,b"


def reset_transaction():
    database = create_database("reset")
    with Session(database) as writer, Session(database) as reader:
        writer.execute("SELECT * FROM graph.build();")
        writer.execute("BEGIN; SELECT graph.reset();")
        assert reader.execute(QUERY) == "a,b"
        writer.execute("ROLLBACK;")
        assert writer.execute(QUERY) == "a,b"
        writer.execute("BEGIN; SAVEPOINT s; SELECT graph.reset(); ROLLBACK TO s;")
        assert writer.execute(QUERY) == "a,b"
        writer.execute("COMMIT; SELECT graph.reset();")
        writer.execute(UNBUILT)
        reader.execute(UNBUILT)
        writer.execute("SELECT * FROM graph.build();")
        assert reader.execute(QUERY) == "a,b"


if __name__ == "__main__":
    first_build_rollback()
    publication_cannot_be_forged()
    resident_generation_refresh()
    old_snapshot_first_load()
    imported_snapshot_first_load()
    statement_rollback()
    reclamation_after_commit()
    cold_resident_and_drop()
    fixed_snapshot_cannot_publish()
    shared_source_retention()
    shared_root_retention()
    resident_root_switch()
    reset_transaction()
    print("Transactional generation regressions passed", flush=True)
