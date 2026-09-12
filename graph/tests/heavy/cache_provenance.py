"""Exercise resident-cache snapshot provenance with real production SQL.

Uses PGHOST, PGPORT and PGUSER, an already installed production extension, and
fresh retained fixture databases. No server or installation is changed. Run
with --prepared when the disposable cluster has max_prepared_transactions > 0.
The separate gql_isolation_matrix.sh retains the full mapped-write profile.
"""

import argparse
import re
import uuid

from generation_transactions import QUERY, UNBUILT, create_database
from psql_session import Session


CONFIGURE = """
SET statement_timeout = '15s';
SET lock_timeout = '5s';
SET graph.persist_on_build = off;
SET graph.sync_mode = 'trigger';
SET graph.query_freshness = 'apply_pending_sync';
SET graph.mutable_enabled = on;
"""
BUILD = "SELECT * FROM graph.build(mode := 'mutable_overlay');"
SOURCE = "SELECT string_agg(id, ',' ORDER BY id) FROM n WHERE id = 'a' OR id IN (SELECT dst FROM e WHERE src = 'a');"


def configure(session):
    session.execute(CONFIGURE)


def imported_snapshot_rejects_newer_cache(replay):
    database = create_database("cache_import_replay" if replay else "cache_import_build")
    with Session(database) as resident, Session(database) as exporter, Session(database) as writer:
        configure(resident)
        if replay:
            resident.execute(BUILD)
            assert resident.execute(QUERY) == "a,b"
        exporter.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        snapshot = exporter.execute("SELECT pg_export_snapshot();")
        assert re.fullmatch(r"[0-9A-Fa-f-]+", snapshot), snapshot
        assert exporter.execute(SOURCE) == "a,b"
        writer.execute("INSERT INTO e(src, dst) VALUES ('a', 'c');")
        if replay:
            assert resident.execute(QUERY) == "a,b,c"
        else:
            resident.execute(BUILD)
            assert resident.execute(QUERY) == "a,b,c"
        resident.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        resident.execute("SET TRANSACTION SNAPSHOT '" + snapshot + "';")
        assert resident.execute(SOURCE) == "a,b"
        resident.execute(UNBUILT)
        resident.execute("ROLLBACK;")
        exporter.execute("COMMIT;")
        resident.execute(BUILD)
        assert resident.execute(QUERY) == resident.execute(SOURCE) == "a,b,c"


def fixed_replay_cannot_skip_a_lower_sync_id():
    database = create_database("cache_sync_gap")
    with Session(database) as resident, Session(database) as lower, Session(database) as higher:
        configure(resident)
        resident.execute("INSERT INTO n VALUES ('d');")
        resident.execute(BUILD)
        assert resident.execute(QUERY) == "a,b"

        # The writers hold the shared transaction fence concurrently. The lower
        # ID remains active when the higher ID commits and the reader snapshots.
        lower.execute("BEGIN; INSERT INTO e(src, dst) VALUES ('a', 'c');")
        low_id = int(lower.execute("SELECT max(id) FROM graph._sync_log;"))
        higher.execute("INSERT INTO e(src, dst) VALUES ('a', 'd');")
        high_id = int(higher.execute("SELECT max(id) FROM graph._sync_log;"))
        assert low_id < high_id
        resident.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        assert resident.execute(SOURCE) == "a,b,d"
        lower.execute("COMMIT;")
        assert higher.execute(SOURCE) == "a,b,c,d"
        assert resident.execute(
            "SELECT string_agg(id::text, ',' ORDER BY id) FROM graph._sync_log "
            f"WHERE id IN ({low_id}, {high_id});"
        ) == str(high_id)
        assert resident.execute(QUERY) == "a,b,d"
        resident.execute("COMMIT;")
        # Reuse would incorrectly skip low_id forever. No persisted baseline
        # exists here, so invalidation must report unbuilt, then permit rebuild.
        resident.execute(UNBUILT)
        resident.execute(BUILD)
        assert resident.execute(QUERY) == resident.execute(SOURCE) == "a,b,c,d"


def imported_snapshot_excludes_an_active_cache_transaction():
    database = create_database("cache_import_active")
    with Session(database) as resident, Session(database) as exporter:
        configure(resident)
        resident.execute("BEGIN; " + BUILD)
        xid = resident.execute("SELECT pg_catalog.pg_current_xact_id()::pg_catalog.text;")
        assert xid.isdecimal()
        # Snapshot xmax follows completed XIDs. Commit a newer one so the still
        # active resident is represented in xip, even on an otherwise idle server.
        completed_xid = exporter.execute("SELECT pg_catalog.pg_current_xact_id()::pg_catalog.text;")
        assert int(completed_xid) > int(xid)
        exporter.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        snapshot = exporter.execute("SELECT pg_export_snapshot();")
        assert re.fullmatch(r"[0-9A-Fa-f-]+", snapshot), snapshot
        assert exporter.execute(
            "SELECT EXISTS (SELECT FROM pg_catalog.pg_snapshot_xip("
            "pg_catalog.pg_current_snapshot()) AS active(xid) "
            f"WHERE xid = '{xid}'::pg_catalog.xid8);"
        ) == "t"
        resident.execute("COMMIT;")
        resident.execute("BEGIN ISOLATION LEVEL REPEATABLE READ;")
        resident.execute("SET TRANSACTION SNAPSHOT '" + snapshot + "';")
        # Commit alone is insufficient: this snapshot explicitly recorded the
        # cache transaction as active, rather than merely preceding its XID.
        resident.execute(UNBUILT)
        resident.execute("ROLLBACK;")
        exporter.execute("COMMIT;")
        resident.execute(BUILD)
        assert resident.execute(QUERY) == "a,b"


def overlay_only_fixed_transactions_keep_baseline():
    database = create_database("cache_overlay")
    with Session(database) as resident, Session(database) as writer:
        configure(resident)
        resident.execute(BUILD)
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"]:
            for finish in ["COMMIT", "ROLLBACK"]:
                resident.execute(f"BEGIN ISOLATION LEVEL {isolation};")
                assert resident.execute(QUERY) == resident.execute(SOURCE) == "a,b"
                resident.execute("SAVEPOINT mapped_write;")
                resident.execute("SELECT * FROM graph.gql('CREATE (x:n {id: ''transient''}) RETURN x.id');")
                assert resident.execute("SELECT count(*) FROM n WHERE id = 'transient';") == "1"
                assert resident.execute("SELECT count(*) FROM graph.gql('MATCH (x:n {id: ''transient''}) RETURN x.id');") == "1"
                resident.execute("ROLLBACK TO mapped_write;")
                assert resident.execute("SELECT count(*) FROM n WHERE id = 'transient';") == "0"
                assert resident.execute("SELECT count(*) FROM graph.gql('MATCH (x:n {id: ''transient''}) RETURN x.id');") == "0"
                resident.execute(f"{finish};")
                assert resident.execute(QUERY) == "a,b"
        writer.execute("INSERT INTO e(src, dst) VALUES ('a', 'c');")
        assert resident.execute(QUERY) == resident.execute(SOURCE) == "a,b,c"


def aborts_discard_installed_or_replayed_base():
    database = create_database("cache_abort")
    with Session(database) as resident, Session(database) as writer:
        configure(resident)
        # An unpublished build has no publication callback to rescue its abort.
        resident.execute("BEGIN; " + BUILD + " ROLLBACK;")
        resident.execute(UNBUILT)
        resident.execute(BUILD)
        resident.execute("BEGIN; SAVEPOINT outer_scope; SAVEPOINT inner_scope; " + BUILD)
        resident.execute("RELEASE inner_scope; ROLLBACK TO outer_scope; COMMIT;")
        resident.execute(UNBUILT)
        resident.execute(BUILD)
        writer.execute("INSERT INTO e(src, dst) VALUES ('a', 'c');")
        resident.execute("BEGIN; SAVEPOINT replay;")
        assert resident.execute(QUERY) == "a,b,c"
        resident.execute("ROLLBACK TO replay; COMMIT;")
        resident.execute(UNBUILT)
        resident.execute(BUILD)
        assert resident.execute(QUERY) == resident.execute(SOURCE) == "a,b,c"


def prepared_base_is_not_a_committed_cache():
    database = create_database("cache_prepare")
    with Session(database) as resident, Session(database) as coordinator:
        configure(resident)
        assert int(coordinator.execute("SHOW max_prepared_transactions;")) > 0
        for finish in ["COMMIT", "ROLLBACK"]:
            gid = "pggraph_cache_" + uuid.uuid4().hex
            # Build uses PostgreSQL temporary spools, which cannot participate
            # in two-phase commit. Complete it first, then exercise real base
            # replay in the transaction that will be prepared.
            resident.execute("DELETE FROM e WHERE dst = 'c';")
            resident.execute(BUILD)
            try:
                resident.execute("BEGIN; INSERT INTO e(src, dst) VALUES ('a', 'c');")
                assert resident.execute(QUERY) == "a,b,c"
                resident.execute("PREPARE TRANSACTION '" + gid + "';")
                resident.execute(UNBUILT)
                coordinator.execute(f"{finish} PREPARED '{gid}';")
                resident.execute(UNBUILT)
            finally:
                # Only resolve this test's exact prepared transaction. Retain
                # every fixture database and all source data for inspection.
                if coordinator.execute(f"SELECT count(*) FROM pg_prepared_xacts WHERE gid = '{gid}';") == "1":
                    coordinator.execute(f"ROLLBACK PREPARED '{gid}';")
            resident.execute(BUILD)
            expected = "a,b,c" if finish == "COMMIT" else "a,b"
            assert resident.execute(QUERY) == resident.execute(SOURCE) == expected


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepared", action="store_true")
    args = parser.parse_args()
    for scenario in [
        lambda: imported_snapshot_rejects_newer_cache(False),
        lambda: imported_snapshot_rejects_newer_cache(True),
        imported_snapshot_excludes_an_active_cache_transaction,
        fixed_replay_cannot_skip_a_lower_sync_id,
        overlay_only_fixed_transactions_keep_baseline,
        aborts_discard_installed_or_replayed_base,
    ]:
        scenario()
    if args.prepared:
        prepared_base_is_not_a_committed_cache()
    print("Resident cache provenance regressions passed", flush=True)
