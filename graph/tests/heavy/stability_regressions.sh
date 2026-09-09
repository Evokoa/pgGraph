#!/usr/bin/env bash
set -euo pipefail

# Uses the installed extension and fresh databases on the connected cluster.
PGGRAPH_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$PGGRAPH_ROOT/scripts/lib/pggraph-common.sh"
DB_PREFIX="${DB_PREFIX:-pggraph_stability_$$}"
databases=()
for suffix in p q empty replay; do
  database="${DB_PREFIX}_${suffix}"
  pggraph_validate_database_name "$database"
  if (( ${#database} > 63 )); then
    pggraph_die "database name exceeds PostgreSQL's identifier limit"
    exit 2
  fi
  # createdb fails rather than deleting an existing database.
  createdb "$database"
  databases+=("$database")
done
printf 'Regression databases: %s\n' "${databases[*]}"

sql() {
  local database="$1"
  shift
  psql -X -qAt -v ON_ERROR_STOP=1 -d "$database" "$@"
}

expect() {
  local actual
  actual="$(sql "$1" -c "$2")"
  if [[ "$actual" != "$3" ]]; then
    printf 'Assertion failed in %s\nExpected: %s\nActual: %s\n' "$1" "$3" "$actual" >&2
    exit 1
  fi
}

# Each call opens a fresh backend. Both databases use the default graph ID.
for suffix in p q; do
  sql "${DB_PREFIX}_${suffix}" >/dev/null <<SQL
CREATE EXTENSION graph;
CREATE TABLE n (id text PRIMARY KEY);
CREATE TABLE e (id serial PRIMARY KEY, src text, dst text, rel text);
INSERT INTO n VALUES ('${suffix}1'), ('${suffix}2');
INSERT INTO e (src, dst, rel) VALUES ('${suffix}1', '${suffix}2', '${suffix}_rel');
SELECT graph.add_table('n'::regclass, 'id');
SELECT graph.add_edge('e'::regclass, 'src', 'n'::regclass, 'dst', 'fallback', false, NULL, 'rel');
SELECT * FROM graph.build();
SQL
done
expect "${DB_PREFIX}_p" "SELECT string_agg(label, ',') FROM graph.edge_types()" p_rel
expect "${DB_PREFIX}_q" "SELECT string_agg(label, ',') FROM graph.edge_types()" q_rel
expect "${DB_PREFIX}_p" "SELECT string_agg(node_id, ',' ORDER BY depth) FROM graph.traverse('n'::regclass, 'p1', 3)" p1,p2
sql "${DB_PREFIX}_empty" -c 'CREATE EXTENSION graph' >/dev/null
sql "${DB_PREFIX}_empty" >/dev/null <<'SQL'
DO $$
DECLARE detail text;
BEGIN
  BEGIN
    PERFORM * FROM graph.edge_types();
    RAISE EXCEPTION 'unbuilt database unexpectedly loaded a graph';
  EXCEPTION WHEN SQLSTATE '55000' THEN
    GET STACKED DIAGNOSTICS detail = PG_EXCEPTION_DETAIL;
    IF detail IS DISTINCT FROM 'pgGraph diagnostic: PG003' THEN
      RAISE EXCEPTION 'wrong unbuilt-graph diagnostic: %', detail;
    END IF;
  END;
END $$;
SQL
sql "${DB_PREFIX}_q" -c 'SELECT graph.reset()' >/dev/null
expect "${DB_PREFIX}_p" "SELECT string_agg(node_id, ',' ORDER BY depth) FROM graph.traverse('n'::regclass, 'p1', 3)" p1,p2

# Absolute roots must retain the database namespace too.
shared_root="$(sql "${DB_PREFIX}_p" -c 'SHOW data_directory')/${DB_PREFIX}_absolute"
for suffix in p q; do
  sql "${DB_PREFIX}_${suffix}" -v database="${DB_PREFIX}_${suffix}" -v root="$shared_root" >/dev/null <<'SQL'
ALTER DATABASE :"database" SET graph.data_dir TO :'root';
SQL
  sql "${DB_PREFIX}_${suffix}" -c 'SELECT * FROM graph.build()' >/dev/null
done
expect "${DB_PREFIX}_p" "SELECT string_agg(label, ',') FROM graph.edge_types()" p_rel
expect "${DB_PREFIX}_q" "SELECT string_agg(label, ',') FROM graph.edge_types()" q_rel

# Empty dictionary, committed rows, automatic and explicit replay, and reload.
sql "${DB_PREFIX}_replay" >/dev/null <<'SQL'
CREATE EXTENSION graph;
CREATE TABLE n (id text PRIMARY KEY, props jsonb);
CREATE TABLE e (id serial PRIMARY KEY, src text, dst text, rel text);
INSERT INTO n VALUES ('a', '{"w":1}'), ('b', NULL), ('c', NULL);
SELECT graph.add_table('n'::regclass, 'id', ARRAY['props.w']);
SELECT graph.add_edge('e'::regclass, 'src', 'n'::regclass, 'dst', 'fallback', false, NULL, 'rel');
SELECT * FROM graph.build();
INSERT INTO e (src, dst, rel) VALUES ('a', 'b', 'y');
SQL
expect "${DB_PREFIX}_replay" "SELECT edge_path::text FROM graph.traverse('n'::regclass, 'a', 2) WHERE node_id = 'b'" '["y"]'
sql "${DB_PREFIX}_replay" -c "INSERT INTO e (src, dst, rel) VALUES ('b', 'c', 'z'); SELECT * FROM graph.apply_sync()" >/dev/null
expect "${DB_PREFIX}_replay" "SELECT edge_path::text FROM graph.traverse('n'::regclass, 'a', 2) WHERE node_id = 'c'" '["y", "z"]'
sql "${DB_PREFIX}_replay" >/dev/null <<'SQL'
BEGIN;
INSERT INTO e (src, dst, rel) VALUES ('a', 'c', 'aborted');
SELECT * FROM graph.traverse('n'::regclass, 'a', 1);
ROLLBACK;
DO $$ BEGIN
  PERFORM * FROM graph.traverse('n'::regclass, 'a', 2);
  IF EXISTS (SELECT FROM graph.edge_types() WHERE label = 'aborted') THEN
    RAISE EXCEPTION 'rolled-back label survived in the same backend';
  END IF;
END $$;
BEGIN;
SAVEPOINT replay_savepoint;
INSERT INTO e (src, dst, rel) VALUES ('a', 'c', 'savepoint-aborted');
SELECT * FROM graph.traverse('n'::regclass, 'a', 1);
ROLLBACK TO SAVEPOINT replay_savepoint;
DO $$ BEGIN
  PERFORM * FROM graph.traverse('n'::regclass, 'a', 2);
  IF EXISTS (SELECT FROM graph.edge_types() WHERE label = 'savepoint-aborted') THEN
    RAISE EXCEPTION 'savepoint rollback retained a replayed label';
  END IF;
END $$;
COMMIT;
SQL
expect "${DB_PREFIX}_replay" "SELECT edge_path::text FROM graph.traverse('n'::regclass, 'a', 2) WHERE node_id = 'c'" '["y", "z"]'
sql "${DB_PREFIX}_replay" -c "UPDATE e SET rel = 'updated' WHERE rel = 'y'; DELETE FROM e WHERE rel = 'z'; SELECT * FROM graph.apply_sync()" >/dev/null
expect "${DB_PREFIX}_replay" "SELECT edge_path::text FROM graph.traverse('n'::regclass, 'a', 2) WHERE node_id = 'b'" '["updated"]'
expect "${DB_PREFIX}_replay" "SELECT count(*) FROM graph.traverse('n'::regclass, 'a', 2) WHERE node_id = 'c'" 0
sql "${DB_PREFIX}_replay" -c 'SELECT * FROM graph.build()' >/dev/null
expect "${DB_PREFIX}_replay" "SELECT string_agg(label, ',') FROM graph.edge_types()" updated

# Registered JSONB paths must keep ordinary DML and GQL REMOVE writable.
sql "${DB_PREFIX}_replay" >/dev/null <<'SQL'
CREATE FUNCTION public.old_jsonb_sync() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN PERFORM NEW."props.w"::text; RETURN NEW; END $$;
CREATE OR REPLACE TRIGGER graph_sync_insert AFTER INSERT ON n
  FOR EACH ROW EXECUTE FUNCTION public.old_jsonb_sync();
DO $$ BEGIN
  BEGIN
    INSERT INTO n VALUES ('d', '{"w":2}');
    RAISE EXCEPTION 'old trigger unexpectedly accepted a dotted property';
  EXCEPTION WHEN undefined_column THEN NULL;
  END;
END $$;
SELECT * FROM graph.build();
INSERT INTO n VALUES ('d', '{"w":2}');
UPDATE n SET props = '{"w":3}' WHERE id = 'd';
DELETE FROM n WHERE id = 'd';
SET graph.mutable_enabled = on;
SELECT * FROM graph.build(mode := 'mutable_overlay');
SELECT * FROM graph.gql('MATCH (x:n {id: ''a''}) REMOVE x.props.w RETURN x.id');
DO $$
DECLARE column_name text; state text; detail text;
BEGIN
  FOREACH column_name IN ARRAY ARRAY['missing', 'props.w', 'props'] LOOP
    BEGIN
      PERFORM graph.add_filter_column('n'::regclass, column_name, 'numeric');
      RAISE EXCEPTION 'invalid filter column unexpectedly accepted';
    EXCEPTION WHEN OTHERS THEN
      GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, detail = PG_EXCEPTION_DETAIL;
      IF state <> '22023' OR detail IS DISTINCT FROM 'pgGraph diagnostic: PG005' THEN
        RAISE EXCEPTION 'wrong filter diagnostic: % / %', state, detail;
      END IF;
    END;
  END LOOP;
END $$;
SQL
expect "${DB_PREFIX}_replay" "SELECT props::text FROM n WHERE id = 'a'" '{}'

# Keep failed fixtures for inspection; remove only databases this run created.
for database in "${databases[@]}"; do
  dropdb "$database"
done
echo 'Stability regressions passed.'
