#!/usr/bin/env bash
set -euo pipefail

# Run against an installed production extension. Keep databases for inspection.
PGGRAPH_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$PGGRAPH_ROOT/scripts/lib/pggraph-common.sh"
DB_PREFIX="${DB_PREFIX:-pggraph_endpoints_$$}"
for persist in on off; do
  database="${DB_PREFIX}_${persist}"
  pggraph_validate_database_name "$database"
  if (( ${#database} > 63 )); then
    pggraph_die "database name exceeds PostgreSQL's identifier limit"
    exit 2
  fi
  createdb "$database"
  psql -X -qAt -v ON_ERROR_STOP=1 -d "$database" -v persist="$persist" <<'SQL'
CREATE EXTENSION graph;
SET graph.persist_on_build TO :'persist';
CREATE TABLE unrelated (id text PRIMARY KEY);
CREATE TABLE n (id text PRIMARY KEY);
CREATE TABLE typed_edge (id serial PRIMARY KEY, src text REFERENCES n(id), dst text);
CREATE TABLE untyped_edge (id serial PRIMARY KEY, src text, dst text);
INSERT INTO unrelated VALUES ('a'), ('missing');
INSERT INTO n VALUES ('a'), ('b'), ('unique');
INSERT INTO typed_edge(src, dst) VALUES ('a', 'b'), ('a', 'missing');
INSERT INTO untyped_edge(src, dst) VALUES ('a', 'unique'), ('unique', 'b');
SELECT graph.add_table('unrelated'::regclass, 'id');
SELECT graph.add_table('n'::regclass, 'id');
SELECT graph.add_edge('typed_edge'::regclass, 'src', 'n'::regclass, 'dst', 'typed', false);
SELECT graph.add_edge('untyped_edge'::regclass, 'src', 'n'::regclass, 'dst', 'untyped', false);
SELECT * FROM graph.build();
DO $$
BEGIN
  IF (SELECT string_agg(node_id, ',' ORDER BY depth, node_id)
      FROM graph.traverse('n'::regclass, 'a', 1)) IS DISTINCT FROM 'a,b' THEN
    RAISE EXCEPTION 'typed edge did not resolve to its exact source and target';
  END IF;
  IF (SELECT count(*) FROM graph.traverse('unrelated'::regclass, 'a', 1)) <> 1 THEN
    RAISE EXCEPTION 'ambiguous source resolved to an unrelated node';
  END IF;
  IF EXISTS (SELECT FROM graph.traverse('n'::regclass, 'a', 1)
             WHERE depth = 1 AND edge_path::text <> '["typed"]') THEN
    RAISE EXCEPTION 'ambiguous source attached an untyped edge to the typed source';
  END IF;
  IF (SELECT string_agg(node_id, ',' ORDER BY depth, node_id)
      FROM graph.traverse('n'::regclass, 'unique', 1)) IS DISTINCT FROM 'unique,b' THEN
    RAISE EXCEPTION 'unique untyped source failed to resolve';
  END IF;
END $$;
SQL
  printf 'Endpoint identity passed: persist_on_build=%s (%s)\n' "$persist" "$database"
done

for persist in on off; do
  for mapping in ambiguous alternate composite target_alternate node_target_alternate node_target_fk; do
    database="${DB_PREFIX}_${persist}_${mapping}"
    pggraph_validate_database_name "$database"
    if (( ${#database} > 63 )); then
      pggraph_die "database name exceeds PostgreSQL's identifier limit"
      exit 2
    fi
    createdb "$database"
    psql -X -qAt -v ON_ERROR_STOP=1 -d "$database" -v persist="$persist" -v mapping="$mapping" <<'SQL'
CREATE EXTENSION graph;
SET graph.persist_on_build TO :'persist';
CREATE TABLE n (id text PRIMARY KEY, alternative text UNIQUE, UNIQUE(id, alternative));
CREATE TABLE m (id text PRIMARY KEY);
CREATE TABLE e (id serial PRIMARY KEY, src text, second_key text, dst text);
SELECT set_config('endpoint_test.mapping', :'mapping', false);
DO $$ BEGIN
  CASE current_setting('endpoint_test.mapping')
    WHEN 'ambiguous' THEN
      ALTER TABLE e ADD FOREIGN KEY (src) REFERENCES n(id);
      ALTER TABLE e ADD FOREIGN KEY (src) REFERENCES m(id);
    WHEN 'alternate' THEN
      ALTER TABLE e ADD FOREIGN KEY (src) REFERENCES n(alternative);
    WHEN 'composite' THEN
      ALTER TABLE e ADD FOREIGN KEY (src, second_key) REFERENCES n(id, alternative);
    WHEN 'target_alternate' THEN
      ALTER TABLE e ADD FOREIGN KEY (dst) REFERENCES n(alternative);
    WHEN 'node_target_alternate' THEN
      ALTER TABLE e ADD FOREIGN KEY (src) REFERENCES n(alternative);
    WHEN 'node_target_fk' THEN
      ALTER TABLE e ADD FOREIGN KEY (src) REFERENCES n(alternative);
  END CASE;
END $$;
SELECT graph.add_table('n'::regclass, 'id');
INSERT INTO n VALUES ('a', 'b'), ('b', 'a');
INSERT INTO m VALUES ('a');
INSERT INTO e(src, second_key, dst) VALUES ('a', 'b', 'a');
SELECT graph.add_table('e'::regclass, 'id')
WHERE current_setting('endpoint_test.mapping') IN ('node_target_alternate', 'node_target_fk');
SELECT graph.add_edge('e'::regclass, 'src', 'n'::regclass,
  CASE WHEN current_setting('endpoint_test.mapping') = 'node_target_alternate'
       THEN 'alternative'
       WHEN current_setting('endpoint_test.mapping') = 'node_target_fk'
       THEN 'id' ELSE 'dst' END, 'invalid_mapping', false);
DO $$
DECLARE detail text;
BEGIN
  BEGIN
    PERFORM * FROM graph.build();
    RAISE EXCEPTION 'unsupported foreign key mapping was accepted';
  EXCEPTION WHEN SQLSTATE '22023' THEN
    GET STACKED DIAGNOSTICS detail = PG_EXCEPTION_DETAIL;
    IF detail IS DISTINCT FROM 'pgGraph diagnostic: PG005' THEN
      RAISE EXCEPTION 'wrong mapping diagnostic: %', detail;
    END IF;
  END;
END $$;
SQL
    printf 'Foreign key validation passed: %s, persist_on_build=%s\n' "$mapping" "$persist"
  done
done
