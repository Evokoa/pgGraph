"""Verify sealed base sharing with live Linux PostgreSQL backends.

Uses PGHOST, PGPORT and PGUSER. Pass --container when PostgreSQL runs in a
local Docker container. All source mutation is confined to a fresh fixture
database created by this test; the database is retained for inspection.
"""

import argparse
import os
import subprocess

from generation_transactions import create_database, QUERY
from psql_session import Session


class LinuxHost:
    def __init__(self, container):
        self.prefix = ["docker", "exec", "--user", "postgres", "-e", "LC_ALL=C", container] if container else []

    def run(self, *args, check=True):
        return subprocess.run(
            self.prefix + list(args), check=check, text=True, capture_output=True,
            env={**os.environ, "LC_ALL": "C"},
        )

    def snapshot(self, pid):
        mappings = self.run("cat", f"/proc/{pid}/maps").stdout
        matches = [line.split() for line in mappings.splitlines() if "memfd:pggraph" in line]
        assert len(matches) == 1, f"expected one sealed base mapping, found {matches}"
        mapping = matches[0]
        assert mapping[1].startswith("r--"), "base mapping is writable"
        inode = mapping[4]
        descriptors = self.run(
            "find", f"/proc/{pid}/fd", "-mindepth", "1", "-maxdepth", "1", "-printf", "%f\n",
        ).stdout.splitlines()
        assert len(descriptors) <= 1024, "unexpected fixture descriptor count"
        for descriptor in descriptors:
            assert descriptor.isdecimal()
            path = f"/proc/{pid}/fd/{descriptor}"
            result = self.run("stat", "-Lc", "%i", path, check=False)
            if result.returncode == 0 and result.stdout.strip() == inode:
                return inode, path
        raise AssertionError("sealed mapping has no retained discovery descriptor")

    def descriptor_count(self, pid):
        return len(self.run(
            "find", f"/proc/{pid}/fd", "-mindepth", "1", "-maxdepth", "1", "-printf", "%f\n",
        ).stdout.splitlines())


def sharing_and_lifetime(host):
    database = create_database("sealed_sharing")
    with Session(database) as creator, Session(database) as adopter:
        creator.execute("SELECT * FROM graph.build();")
        creator_pid = int(creator.execute("SELECT pg_backend_pid();"))
        assert creator.execute(QUERY) == "a,b"
        creator_inode, _ = host.snapshot(creator_pid)
        assert adopter.execute(QUERY) == "a,b"
        adopter_pid = int(adopter.execute("SELECT pg_backend_pid();"))
        adopter_inode, adopter_fd = host.snapshot(adopter_pid)
        assert adopter_inode == creator_inode, "backends copied the base separately"
        assert adopter.execute(
            "SELECT active_backend_shared_mb > 0 FROM graph.memory_profile();"
        ) == "t"

        truncated = host.run("truncate", "-s", "0", adopter_fd, check=False)
        assert truncated.returncode != 0, "sealed descriptor allowed shrinking"
        assert "Operation not permitted" in truncated.stderr, truncated.stderr
        punched = host.run(
            "fallocate", "--punch-hole", "--keep-size", "--offset", "0",
            "--length", "4096", adopter_fd, check=False,
        )
        assert punched.returncode != 0, "sealed descriptor allowed hole punching"
        assert "Operation not permitted" in punched.stderr, punched.stderr
        assert adopter.execute(QUERY) == "a,b"
        creator.close()

        with Session(database) as later:
            assert later.execute(QUERY) == "a,b"
            later_pid = int(later.execute("SELECT pg_backend_pid();"))
            later_inode, _ = host.snapshot(later_pid)
            assert later_inode == adopter_inode, "creator exit broke adopter discovery"

            root = later.execute("SELECT artifact_root FROM graph._projection_heads;")
            bases = host.run("find", root, "-maxdepth", "1", "-name", "*-base.pggraph").stdout.splitlines()
            assert len(bases) == 1, "unexpected fixture base inventory"
            source = bases[0]
            host.run("cp", source, source + ".test-original")
            host.run("truncate", "-s", "0", source)
            assert later.execute(QUERY) == "a,b"
            assert adopter.execute(QUERY) == "a,b"
            with Session(database) as fresh:
                fresh.execute("""
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph.traverse('n'::regclass, 'a', 1);
    RAISE EXCEPTION 'fresh loader accepted a truncated source';
  EXCEPTION WHEN SQLSTATE '55000' THEN
    IF SQLERRM NOT LIKE 'Graph not built%' THEN RAISE; END IF;
  END;
END $$;
""")
        print("Shared snapshot inode:", adopter_inode, flush=True)


def cancellation_cleanup(host):
    database = create_database("sealed_cancel")
    with Session(database) as reader:
        reader.execute("SELECT * FROM graph.build(); SELECT graph.unload_graph('default');")
        pid = int(reader.execute("SELECT pg_backend_pid();"))
        for registry in [False, True]:
            baseline = None
            for _ in range(12):
                reader.execute("SELECT graph._test_arm_snapshot_cancel(" + str(registry).lower() + ");")
                reader.execute("""
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph.traverse('n'::regclass, 'a', 1);
    RAISE EXCEPTION 'snapshot load did not cancel';
  EXCEPTION WHEN query_canceled THEN NULL;
  END;
END $$;
""")
                count = host.descriptor_count(pid)
                if baseline is None:
                    baseline = count
                assert count <= baseline, "canceled snapshot loads leaked descriptors"
        with Session(database) as other:
            assert other.execute(QUERY) == "a,b", "cancellation retained the registry lock"
            assert reader.execute(QUERY) == "a,b"
            other_pid = int(other.execute("SELECT pg_backend_pid();"))
            assert host.snapshot(pid)[0] == host.snapshot(other_pid)[0]
    print("Snapshot cancellation cleanup passed", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container")
    parser.add_argument("--cancellation", action="store_true", help="also test a development build's cancellation hooks")
    args = parser.parse_args()
    sharing_and_lifetime(LinuxHost(args.container))
    if args.cancellation:
        cancellation_cleanup(LinuxHost(args.container))
    print("Sealed snapshot sharing regressions passed", flush=True)
