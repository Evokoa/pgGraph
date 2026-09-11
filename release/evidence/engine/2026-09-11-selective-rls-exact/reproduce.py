"""Retain exact caller-RLS differentials on an explicitly disposable PG17 host.

Run with the existing OSS virtualenv. This external harness imports only the
repository's inspected psql client and Python standard library. It never builds,
installs, drops a database/role, cleans output, or changes installed libraries.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import uuid


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def literal(value):
    return "'" + value.replace("'", "''") + "'"


def save(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def scalars(value):
    if isinstance(value, dict):
        for child in value.values():
            yield from scalars(child)
    elif isinstance(value, list):
        for child in value:
            yield from scalars(child)
    else:
        yield value


class Harness:
    def __init__(self, args, session_class, output):
        self.args = args
        self.session_class = session_class
        self.output = output
        self.sequence = 0
        self.results = []

    def execute(self, session, label, sql):
        self.sequence += 1
        stem = self.output / f"{self.sequence:03d}-{label}"
        stem.with_suffix(".sql").write_text(sql + "\n")
        try:
            result = session.execute(sql, timeout=self.args.timeout)
            stem.with_suffix(".out").write_text(result + "\n")
            return result
        except Exception as error:
            stem.with_suffix(".error.txt").write_text(str(error) + "\n")
            raise

    def settings(self, session):
        self.execute(session, "settings", """
SET statement_timeout = '120s';
SET graph.auto_load = on;
SET graph.query_freshness = 'off';
SET graph.persist_on_build = on;
SET graph.sync_mode = 'trigger';
""")

    def pair(self, session, role, name, function, *, expected="lazy", count=None,
             minimum=1, ids=None, path=False, weighted=False, forbidden=None,
             truncated=False, no_rls=False):
        forbidden = forbidden if forbidden is not None else ["hidden-node", "behind-hidden", "edge-hidden", "secret-sc", "secret-ct"]
        pair = {}
        for strategy in ["eager", "auto"]:
            # WITH ORDINALITY records emitted order. No row sorting or column
            # projection can hide a row-shape or ordering mismatch here.
            sql = f"""
SET ROLE {role};
SELECT graph._test_set_visibility_strategy('{strategy}');
SELECT jsonb_build_object('kind', 'rows', 'value',
  COALESCE(jsonb_agg(to_jsonb(r) - '__ordinal' ORDER BY __ordinal), '[]'::jsonb))
FROM {function} WITH ORDINALITY AS r;
SELECT jsonb_build_object('kind', 'metrics', 'value', graph._test_visibility_metrics());
RESET ROLE;
"""
            # PostgreSQL names the additional column ordinality by default.
            sql = sql.replace("__ordinal", "ordinality")
            raw = self.execute(session, name + "-" + strategy, sql)
            records = [json.loads(line) for line in raw.splitlines() if line.startswith('{')]
            require(len(records) == 2 and [r.get("kind") for r in records] == ["rows", "metrics"],
                    f"{name}/{strategy}: missing rows or immediate metrics")
            rows, metrics = records[0]["value"], records[1]["value"]
            require(isinstance(rows, list) and isinstance(metrics, dict), f"{name}: malformed output")
            selected = "eager" if strategy == "eager" else expected
            require(metrics.get("selected_strategy") == selected,
                    f"{name}/{strategy}: selected {metrics.get('selected_strategy')!r}, expected {selected}")
            if no_rls:
                require(metrics.get("spi_calls") == 0 and metrics.get("source_rows") == 0,
                        f"{name}/{strategy}: no-RLS path performed visibility work")
            elif selected == "eager":
                # Eager SPI scans record fetched source rows, while spi_calls
                # counts only the bounded lazy probes (sql_visibility.rs).
                require(isinstance(metrics.get("source_rows"), int) and metrics["source_rows"] > 0,
                        f"{name}/{strategy}: eager caller-RLS scan did no source work")
            else:
                require(isinstance(metrics.get("spi_calls"), int) and metrics["spi_calls"] > 0,
                        f"{name}/{strategy}: no actual caller-RLS probes")
            if count is not None:
                require(len(rows) == count, f"{name}: expected {count} rows, observed {len(rows)}")
            else:
                require(len(rows) >= minimum, f"{name}: unexpectedly empty/short result")
            values = list(scalars(rows))
            for denied in forbidden:
                require(denied not in values, f"{name}: exposed forbidden identity {denied}")
            if ids is not None:
                require({row.get("node_id") for row in rows} == set(ids), f"{name}: wrong visible node set")
            if path:
                require([row.get("step") for row in rows] == list(range(len(rows))), f"{name}: wrong step order")
                require(rows[0].get("node_id") == "s" and rows[-1].get("node_id") == "t", f"{name}: wrong endpoints")
            if weighted:
                require([row.get("node_id") for row in rows] == ["s", "a", "t"], f"{name}: weighted tie winner changed")
                require([row.get("step_cost") for row in rows] == [0, 2, 4], f"{name}: wrong accumulated costs")
                require(all(row.get("total_cost") == 4 for row in rows), f"{name}: wrong total cost")
            if truncated:
                require(any(row.get("truncated") is True for row in rows), f"{name}: missing truncation flag")
            pair[strategy] = {"rows": rows, "metrics": metrics}
        require(pair["eager"]["rows"] == pair["auto"]["rows"], f"{name}: full ordered eager/auto output mismatch")
        record = {"case": name, "expected_auto_strategy": expected, "status": "pass", **pair}
        save(self.output / (name + ".json"), record)
        self.results.append(record)
        save(self.output / "results.json", self.results)
        return record

    def setup(self, database, role, mode):
        session = self.session_class(database)
        try:
            self.execute(session, mode + "-extension", "CREATE EXTENSION graph;")
            metadata = json.loads(self.execute(session, mode + "-runtime", """
SELECT jsonb_build_object('server_version_num', current_setting('server_version_num')::int,
 'extension_version', (SELECT extversion FROM pg_extension WHERE extname = 'graph'),
 'development_metrics', to_regprocedure('graph._test_visibility_metrics()') IS NOT NULL,
 'pg_test_schema', EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'tests'));
"""))
            require(170000 <= metadata["server_version_num"] < 180000, "PG17 required")
            require(metadata["extension_version"] == self.args.extension_version, "installed extension version mismatch")
            require(metadata["development_metrics"] and not metadata["pg_test_schema"], "development without pg_test required")
            self.settings(session)
            mutable = mode not in ["directed", "bidirectional"]
            edges = "('sa','s','a',2),('sb','s','b',2),('secret-sc','s','edge-hidden',1),('secret-ct','edge-hidden','t',1),('sh','s','hidden-node',1),('hx','hidden-node','behind-hidden',1)"
            if not mutable:
                edges += ",('at','a','t',2),('bt','b','t',2)"
            self.execute(session, mode + "-fixture", f"""
CREATE TABLE public.p4_nodes(id text PRIMARY KEY, name text NOT NULL);
CREATE TABLE public.p4_edges(id text PRIMARY KEY,
 src text NOT NULL REFERENCES public.p4_nodes(id),
 dst text NOT NULL REFERENCES public.p4_nodes(id),
 cost integer NOT NULL DEFAULT 2 CHECK(cost >= 0));
INSERT INTO public.p4_nodes VALUES ('s','Alice'),('a','Alice'),('b','Beta'),
 ('t','Target'),('hidden-node','Hidden'),('behind-hidden','Unreachable'),('edge-hidden','Denied edge target');
INSERT INTO public.p4_edges VALUES {edges};
SELECT graph.add_table('public.p4_nodes'::regclass, 'id', ARRAY['name']);
SELECT graph.add_edge('public.p4_edges'::regclass, 'src', 'public.p4_nodes'::regclass,
 'dst', 'friend', bidirectional := {'true' if mode == 'bidirectional' else 'false'}, weight_column := 'cost');
SET graph.mutable_enabled = {'on' if mutable else 'off'};
SET graph.persist_on_build = {'off' if mode == 'overlay' else 'on'};
SELECT * FROM graph.build({"mode := 'mutable_overlay'" if mutable else ""});
ALTER TABLE public.p4_nodes ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.p4_edges ENABLE ROW LEVEL SECURITY;
CREATE POLICY node_reader ON public.p4_nodes FOR SELECT TO {role} USING(id <> 'hidden-node');
CREATE POLICY edge_reader ON public.p4_edges FOR SELECT TO {role} USING(id NOT IN ('secret-sc','secret-ct'));
GRANT USAGE ON SCHEMA public, graph TO {role};
GRANT SELECT ON public.p4_nodes, public.p4_edges TO {role};
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA graph TO {role};
""")
            # Verify this role really sees the policy-filtered source rows.
            actual = json.loads(self.execute(session, mode + "-policy-check", f"""
SET ROLE {role};
SELECT jsonb_build_object('role',current_user,'hidden_nodes',(SELECT count(*) FROM public.p4_nodes WHERE id='hidden-node'),
 'hidden_edges',(SELECT count(*) FROM public.p4_edges WHERE id IN ('secret-sc','secret-ct')));
RESET ROLE;
"""))
            require(actual == {"role": role, "hidden_nodes": 0, "hidden_edges": 0}, "fixture RLS is not effective")
            return session
        except Exception:
            session.close()
            raise

    def eager_relationship_cleanup(self, session, role):
        self.execute(session, "edge-cleanup-fixture", f"""
ALTER TABLE public.p4_nodes DISABLE ROW LEVEL SECURITY;
CREATE FUNCTION public.p4_edge_policy_error() RETURNS boolean
LANGUAGE plpgsql SECURITY INVOKER AS $policy$
BEGIN
  RAISE EXCEPTION USING ERRCODE = 'P0001', MESSAGE = 'p4 injected eager edge policy error';
END;
$policy$;
GRANT EXECUTE ON FUNCTION public.p4_edge_policy_error() TO {role};
""")
        for state in ["57014", "P0001"]:
            # PostgreSQL catches only this expected backend error. Every other
            # SQLSTATE propagates to ON_ERROR_STOP and fails the harness.
            self.execute(session, "edge-cleanup-capture-" + state, f"""
CREATE FUNCTION public.p4_capture_{state}(query_sql text) RETURNS jsonb
LANGUAGE plpgsql SECURITY INVOKER AS $capture$
DECLARE error_state text; error_message text; error_detail text; error_context text;
BEGIN
  BEGIN
    EXECUTE query_sql;
  EXCEPTION WHEN SQLSTATE '{state}' THEN
    GET STACKED DIAGNOSTICS error_state = RETURNED_SQLSTATE,
      error_message = MESSAGE_TEXT, error_detail = PG_EXCEPTION_DETAIL,
      error_context = PG_EXCEPTION_CONTEXT;
    RETURN jsonb_build_object('sqlstate',error_state,'message',error_message,
      'detail',error_detail,'context',error_context);
  END;
  RAISE EXCEPTION USING ERRCODE = 'XX000', MESSAGE = 'expected SQLSTATE {state} was not raised';
END;
$capture$;
GRANT EXECUTE ON FUNCTION public.p4_capture_{state}(text) TO {role};
""")
        preconditions = json.loads(self.execute(session, "edge-cleanup-preconditions", f"""
SET ROLE {role};
SELECT jsonb_build_object('backend_pid',pg_backend_pid(),
 'node_rls',graph._test_rls_applies_to_outer_caller('public.p4_nodes'::regclass),
 'edge_rls',graph._test_rls_applies_to_outer_caller('public.p4_edges'::regclass));
RESET ROLE;
"""))
        require(preconditions.get("node_rls") is False and preconditions.get("edge_rls") is True,
                "cleanup must exercise only the active relationship policy scan")
        options = {"count": 6, "ids": ["s", "a", "b", "t", "hidden-node", "behind-hidden"],
                   "forbidden": ["edge-hidden", "secret-sc", "secret-ct"]}
        baseline = self.pair(session, role, "edge-cleanup-baseline", traversal("out"), **options)
        for state, label, expected_message in [
            ("57014", "scan-cancel", "injected RLS visibility scan cancellation"),
            ("P0001", "policy-error", "p4 injected eager edge policy error"),
        ]:
            if state == "P0001":
                self.execute(session, "edge-cleanup-install-error-policy", """
ALTER POLICY edge_reader ON public.p4_edges USING (public.p4_edge_policy_error());
""")
            arm = "SELECT graph._test_arm_visibility_scan_cancel(0);" if state == "57014" else ""
            observed = self.execute(session, "edge-cleanup-" + label, f"""
SET ROLE {role};
SELECT graph._test_set_visibility_strategy('eager');
{arm}
SELECT jsonb_build_object('kind','error','value',
 public.p4_capture_{state}({literal('SELECT * FROM ' + traversal('out'))}));
SELECT jsonb_build_object('kind','cleanup','backend_pid',pg_backend_pid(),
 'build_slot_empty',graph._test_visibility_build_slot_empty(),
 'resolution_state_empty',graph._test_visibility_resolution_state_empty(),
 'metrics',graph._test_visibility_metrics());
RESET ROLE;
""")
            records = [json.loads(line) for line in observed.splitlines() if line.startswith('{')]
            require(len(records) == 2 and [r.get("kind") for r in records] == ["error", "cleanup"],
                    f"{label}: missing backend error or cleanup diagnostics")
            error, cleanup = records[0]["value"], records[1]
            require(error.get("sqlstate") == state and error.get("message") == expected_message,
                    f"{label}: unexpected backend error {error}")
            require(cleanup.get("backend_pid") == preconditions["backend_pid"], f"{label}: backend changed")
            require(cleanup.get("build_slot_empty") is True and cleanup.get("resolution_state_empty") is True,
                    f"{label}: backend visibility state survived the error")
            require(cleanup.get("metrics", {}).get("selected_strategy") == "eager",
                    f"{label}: error did not occur in the selected eager route")
            if state == "P0001":
                self.execute(session, "edge-cleanup-restore-policy", """
ALTER POLICY edge_reader ON public.p4_edges USING (id NOT IN ('secret-sc','secret-ct'));
""")
            retry = self.pair(session, role, "edge-cleanup-" + label + "-retry", traversal("out"), **options)
            require(retry["eager"]["rows"] == baseline["eager"]["rows"],
                    f"{label}: same-backend eager retry changed full ordered rows")
            retry_pid = int(self.execute(session, "edge-cleanup-" + label + "-retry-pid", "SELECT pg_backend_pid();"))
            require(retry_pid == preconditions["backend_pid"], f"{label}: retry used a different backend")
            record = {"case": "edge-cleanup-" + label, "status": "pass", "expected_sqlstate": state,
                      "error": error, "cleanup": cleanup, "baseline_rows": baseline["eager"]["rows"],
                      "retry_rows": retry["eager"]["rows"], "retry_metrics": retry["eager"]["metrics"],
                      "retry_backend_pid": retry_pid}
            save(self.output / ("edge-cleanup-" + label + ".json"), record)
            self.results.append(record)
            save(self.output / "results.json", self.results)
        self.execute(session, "edge-cleanup-restore-node-rls", "ALTER TABLE public.p4_nodes ENABLE ROW LEVEL SECURITY;")


def path_call(weighted=False):
    name = "weighted_shortest_path" if weighted else "shortest_path"
    options = "ARRAY['friend']" if weighted else "max_depth := 20, hydrate := true"
    return f"graph.{name}('public.p4_nodes'::regclass,'s','public.p4_nodes'::regclass,'t',{options})"


def traversal(direction, strategy="dfs"):
    start = "t" if direction == "in" else "s"
    return f"graph.traverse('public.p4_nodes'::regclass,'{start}',4,direction := '{direction}',strategy := '{strategy}',hydrate := true)"


def run_cases(h, database, role, mode):
    session = h.setup(database, role, mode)
    try:
        if mode in ["overlay", "durable"]:
            h.execute(session, mode + "-insert", "INSERT INTO public.p4_edges VALUES ('at','a','t',2);")
            if mode == "overlay":
                h.execute(session, "overlay-apply", "SELECT graph.apply_sync();")
                state = json.loads(h.execute(session, "overlay-state", "SELECT to_jsonb(s) FROM graph.status() s;"))
                require(state.get("pending_edge_deltas", 0) > 0, "classic overlay absent")
            else:
                published = h.execute(session, "durable-publish", "SELECT segments_published FROM graph.ingest_projection();")
                require(int(published) > 0, "durable segment absent")
                session.close()
                session = h.session_class(database)
                h.settings(session)
        elif mode in ["txedge", "txnode"]:
            if mode == "txnode":
                h.execute(session, "txnode-route", "INSERT INTO public.p4_edges VALUES ('at','a','t',2); SELECT graph.apply_sync();")
            h.execute(session, mode + "-begin", "BEGIN;")
            query = ("MATCH (a:p4_nodes {id: 'a'}), (t:p4_nodes {id: 't'}) CREATE (a)-[r:friend {id: 'at', cost: 2}]->(t) RETURN r"
                     if mode == "txedge" else "CREATE (n:p4_nodes {id: 'txnode', name: 'transaction-local'}) RETURN n")
            h.execute(session, mode + "-write", f"SELECT * FROM graph.gql({literal(query)},hydrate := false);")
            state = json.loads(h.execute(session, mode + "-state", "SELECT to_jsonb(s) FROM graph.status() s;"))
            field = "tx_delta_added_edges" if mode == "txedge" else "tx_delta_added_nodes"
            require(state.get(field, 0) > 0, f"{field} absent")

        h.pair(session, role, mode + "-path", path_call(), count=3, path=True,
               expected="eager" if mode == "txnode" else "lazy")
        if mode != "txnode":
            for direction in ["out", "in", "any"]:
                h.pair(session, role, mode + "-dfs-" + direction, traversal(direction),
                       count=4 if not (mode in ["overlay", "durable", "txedge"] and direction == "in") else 3,
                       ids=["s","a","t"] if mode in ["overlay", "durable", "txedge"] and direction == "in" else ["s","a","b","t"])
        if mode in ["directed", "bidirectional", "durable"]:
            h.pair(session, role, mode + "-weighted", path_call(True), count=3, path=True, weighted=True)
        if mode != "directed":
            if mode in ["txedge", "txnode"]:
                h.execute(session, mode + "-commit", "COMMIT;")
            return

        h.pair(session, role, "directed-bfs", traversal("out", "bfs"), count=4, ids=["s","a","b","t"])
        for weighted in [False, True]:
            for source, target, label in [("s", "hidden-node", "hidden-target"), ("hidden-node", "t", "hidden-source"), ("s", "behind-hidden", "hidden-intermediate")]:
                name = "weighted_shortest_path" if weighted else "shortest_path"
                options = "ARRAY['friend']" if weighted else "max_depth := 20, hydrate := true"
                function = f"graph.{name}('public.p4_nodes'::regclass,{literal(source)},'public.p4_nodes'::regclass,{literal(target)},{options})"
                h.pair(session, role, name + "-" + label, function, count=0)
        h.pair(session, role, "directed-dfs-multiseed",
               "graph.traverse(ARRAY['public.p4_nodes'::regclass::oid,'public.p4_nodes'::regclass::oid],ARRAY['s','a'],4,direction := 'out',strategy := 'dfs',hydrate := true)", minimum=4)
        h.pair(session, role, "workflow-expand",
               "graph.expand('public.p4_nodes'::regclass,'s',max_depth := 2,direction := 'out',max_rows := 1)", count=1,truncated=True)
        related = []
        for include_counts in [False, True]:
            related.append(h.pair(session, role, "workflow-find-related-" + str(include_counts).lower(),
               "graph.find_related('name','Alice',source_table := 'public.p4_nodes'::regclass,search_mode := 'exact',search_max_rows := 2,max_depth := 2,direction := 'out',max_rows := 10,include_counts := " + str(include_counts).lower() + ")", minimum=2))
        for metric in ["spi_calls", "requested_keys"]:
            require(related[0]["auto"]["metrics"].get(metric) == related[1]["auto"]["metrics"].get(metric),
                    f"find_related did not reuse its resolver for counts: {metric}")
        h.pair(session, role, "workflow-neighborhood",
               "graph.neighborhood('name','Alice',source_table := 'public.p4_nodes'::regclass,search_mode := 'exact',search_max_rows := 1,max_depth := 1,direction := 'out',sample_k := 1,node_limit := 1)", count=1,truncated=True)
        queries = [
            ("forward", "MATCH (u:p4_nodes {id: 's'})-[r:friend]->(v:p4_nodes) RETURN u.id AS source,r,v.id AS target ORDER BY target", 2, "lazy"),
            ("reverse", "MATCH (u:p4_nodes {id: 't'})<-[r:friend]-(v:p4_nodes) RETURN u.id AS source,r,v.id AS target ORDER BY target", 2, "lazy"),
            ("optional", "OPTIONAL MATCH (u:p4_nodes {id: 's'})<-[r:friend]-(v:p4_nodes) RETURN u.id AS source,r,v.id AS target ORDER BY target LIMIT 1", 1, "lazy"),
            ("multipattern", "MATCH (u:p4_nodes)-[r:friend]->(v:p4_nodes),(u)-[q:friend]->(w:p4_nodes) WHERE u.id = 's' RETURN v.id AS target,w.id AS peer,r,q ORDER BY target,peer LIMIT 1", 1, "eager"),
        ]
        for label, query, count, expected in queries:
            h.pair(session, role, "gql-" + label, f"graph.gql({literal(query)},hydrate := true)",count=count,expected=expected)
        h.pair(session, role, "cypher-forward", f"graph.cypher({literal(queries[0][1])},hydrate := true)", count=2)
        h.pair(session, role, "gql-global", "graph.gql('MATCH (u:p4_nodes) RETURN u ORDER BY u.id LIMIT 3',hydrate := true)", count=3,expected="eager",forbidden=["hidden-node"])
        h.eager_relationship_cleanup(session, role)
        h.execute(session, "disable-rls", "ALTER TABLE public.p4_nodes DISABLE ROW LEVEL SECURITY; ALTER TABLE public.p4_edges DISABLE ROW LEVEL SECURITY;")
        h.pair(session, role, "no-rls-bfs", traversal("out", "bfs"), count=7,expected="eager",forbidden=[],no_rls=True)
    finally:
        session.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--build-record", type=Path, required=True,
                        help="JSON attestation: source_commit, features, build_profile, library_sha256")
    parser.add_argument("--client-file", type=Path,
                        help="Optional frozen copy of the reviewed repository psql client")
    parser.add_argument("--admin-database", required=True)
    parser.add_argument("--prefix", default="pggraph_p4_" + uuid.uuid4().hex[:10])
    parser.add_argument("--extension-version", default="1.2.1")
    parser.add_argument("--timeout", type=int, default=150)
    parser.add_argument("--disposable", action="store_true", required=True)
    args = parser.parse_args()
    require(sys.prefix != sys.base_prefix, "Use the existing OSS virtual environment")
    require(all(os.environ.get(key) for key in ["PGHOST", "PGPORT", "PGUSER"]), "Explicit PGHOST, PGPORT and PGUSER required")
    require(re.fullmatch(r"pggraph_[a-z0-9_]{1,35}", args.prefix), "invalid fixture prefix")
    require(args.timeout >= 120, "client timeout must allow the 120s statement timeout")
    repository = args.repository.resolve()
    output = args.output.resolve()
    require(not output.is_relative_to(repository), "evidence must stay outside public repository")
    require(not output.exists(), "output collision; choose a new directory")
    build_record = json.loads(args.build_record.read_text())
    revision = subprocess.check_output(["git", "-C", str(repository), "rev-parse", "HEAD"], text=True).strip()
    require(build_record.get("source_commit") == revision, "build/source revision mismatch")
    require(set(build_record.get("features", [])) == {"pg17", "development"}, "build must use only pg17,development (no pg_test)")
    require(build_record.get("build_profile") == "release", "final qualification requires an attested release-profile artifact")
    require(re.fullmatch(r"[0-9a-f]{64}", build_record.get("library_sha256", "")), "retain installed-library SHA256 in build record")
    client_path = args.client_file or repository / "graph/tests/heavy/psql_session.py"
    require(client_path.read_bytes() == (repository / "graph/tests/heavy/psql_session.py").read_bytes(),
            "frozen psql client differs from reviewed source")
    spec = importlib.util.spec_from_file_location("pggraph_psql_session", client_path)
    module = importlib.util.module_from_spec(spec)
    sys.dont_write_bytecode = True
    spec.loader.exec_module(module)
    output.mkdir(parents=True)
    save(output / "build-record.json", build_record)
    (output / "commit.txt").write_text(revision + "\n")
    (output / "git-status.txt").write_text(subprocess.check_output(["git", "-C", str(repository), "status", "--short"], text=True))
    save(output / "harness.json", {"sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                                   "client_sha256": hashlib.sha256(client_path.read_bytes()).hexdigest(),
                                   "database_prefix": args.prefix, "status": "running"})
    h = Harness(args, module.Session, output)
    modes = ["directed", "bidirectional", "overlay", "durable", "txedge", "txnode"]
    roles = [args.prefix + "_" + mode + "_r" for mode in modes]
    databases = [args.prefix + "_" + mode for mode in modes]
    try:
        with module.Session(args.admin_database) as admin:
            names = ",".join(literal(name) for name in databases)
            role_names = ",".join(literal(name) for name in roles)
            collisions = h.execute(admin, "collision-check", f"SELECT datname FROM pg_database WHERE datname IN ({names}) UNION ALL SELECT rolname FROM pg_roles WHERE rolname IN ({role_names});")
            require(not collisions.strip(), "database or role collision; no fixtures created")
            for database, role in zip(databases, roles):
                h.execute(admin, "create-" + database, f"CREATE ROLE {role} NOLOGIN NOSUPERUSER NOBYPASSRLS;\nCREATE DATABASE {database};")
        for database, role, mode in zip(databases, roles, modes):
            run_cases(h, database, role, mode)
        save(output / "completion.json", {"status": "complete", "cases": len(h.results), "databases": databases, "roles": roles})
        print(f"Complete: {len(h.results)} exact differential cases. Evidence and fixtures retained at {output}")
    except Exception as error:
        save(output / "completion.json", {"status": "failed", "cases_passed": len(h.results), "error": str(error), "databases": databases, "roles": roles})
        raise


if __name__ == "__main__":
    main()
