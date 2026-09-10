"""Check replay commit order and lock lifetime with production features."""

from generation_transactions import create_database, QUERY
from psql_session import Session


def empty_graph(suffix, mode="csr_readonly"):
    database = create_database(suffix)
    with Session(database) as setup:
        setup.execute("DELETE FROM e; SET graph.mutable_enabled = on;")
        setup.execute("SELECT * FROM graph.build(mode := '" + mode + "');")
    return database


def expect_writer_busy(reader):
    reader.execute("""
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph.traverse('n'::regclass, 'a', 1);
    RAISE EXCEPTION 'replay advanced while an earlier writer was uncommitted';
  EXCEPTION WHEN lock_not_available THEN NULL;
  END;
END $$;
""")


def out_of_order_commits():
    database = empty_graph("order")
    with Session(database) as slow, Session(database) as fast, Session(database) as reader:
        slow.execute("BEGIN; INSERT INTO e(src,dst) VALUES ('a','b');")
        fast.execute("INSERT INTO e(src,dst) VALUES ('a','c');")
        expect_writer_busy(reader)
        slow.execute("COMMIT;")
        assert reader.execute(QUERY) == "a,b,c"
        assert reader.execute(QUERY) == "a,b,c"
    with Session(database) as fresh:
        assert fresh.execute(QUERY) == "a,b,c"


def fixed_snapshot_replay():
    database = empty_graph("fixed_replay")
    with Session(database) as slow, Session(database) as fast, Session(database) as reader:
        slow.execute("BEGIN; INSERT INTO e(src,dst) VALUES ('a','b');")
        fast.execute("INSERT INTO e(src,dst) VALUES ('a','c');")
        reader.execute("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT count(*) FROM e;")
        slow.execute("COMMIT;")
        assert reader.execute(QUERY) == "a,c"
        reader.execute("COMMIT;")
        assert reader.execute(QUERY) == "a,b,c", "fixed-snapshot watermark escaped its transaction"


def imported_snapshot_discards_future_overlay():
    database = empty_graph("import_replay")
    with Session(database) as exporter, Session(database) as writer, Session(database) as reader:
        exporter.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        snapshot = exporter.execute("SELECT pg_export_snapshot();")
        writer.execute("INSERT INTO e(src,dst) VALUES ('a','b');")
        assert reader.execute(QUERY) == "a,b"
        reader.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        reader.execute("SET TRANSACTION SNAPSHOT '" + snapshot + "';")
        assert reader.execute(QUERY) == "a", "resident overlay was newer than imported snapshot"
        reader.execute("COMMIT;")
        exporter.execute("COMMIT;")
        assert reader.execute(QUERY) == "a,b"


def catchup_releases_writer_fence():
    database = empty_graph("writer_progress", "mutable_overlay")
    with Session(database) as writer, Session(database) as reader:
        writer.execute("INSERT INTO e(src,dst) VALUES ('a','b');")
        reader.execute("BEGIN;")
        assert reader.execute(QUERY) == "a,b"
        writer.execute("SET lock_timeout = '1s'; INSERT INTO e(src,dst) VALUES ('a','c');")
        reader.execute("COMMIT;")
        assert reader.execute(QUERY) == "a,b,c"


def own_rows_rollback():
    database = empty_graph("own_replay")
    with Session(database) as writer:
        writer.execute("BEGIN; INSERT INTO e(src,dst) VALUES ('a','b');")
        assert writer.execute(QUERY) == "a,b"
        writer.execute("SAVEPOINT s; INSERT INTO e(src,dst) VALUES ('a','c');")
        assert writer.execute(QUERY) == "a,b,c"
        writer.execute("ROLLBACK TO s;")
        assert writer.execute(QUERY) == "a,b"
        writer.execute("ROLLBACK;")
        assert writer.execute(QUERY) == "a"


def cancellation_during_fetch():
    """Run separately with development enabled, without pg_test guard bypasses."""
    database = empty_graph("cancel_capture")
    with Session(database) as reader, Session(database) as writer:
        reader.execute("BEGIN;")
        reader.execute("""
DO $$ BEGIN
  BEGIN
    PERFORM graph._test_sync_capture_cancel_during_fetch();
    RAISE EXCEPTION 'capture did not cancel';
  EXCEPTION WHEN query_canceled THEN NULL;
  END;
END $$;
""")
        writer.execute("SET lock_timeout = '1s'; INSERT INTO e(src,dst) VALUES ('a','b');")
        assert reader.execute(QUERY) == "a,b"
        reader.execute("ROLLBACK;")
        writer.execute("INSERT INTO e(src,dst) VALUES ('a','c');")
        assert reader.execute(QUERY) == "a,b,c"


if __name__ == "__main__":
    out_of_order_commits()
    fixed_snapshot_replay()
    imported_snapshot_discards_future_overlay()
    catchup_releases_writer_fence()
    own_rows_rollback()
    print("Replay transaction regressions passed", flush=True)
