#!/usr/bin/env bash
set -euo pipefail

# Both resident and fresh backends must reject a stale persisted catalog.
PGGRAPH_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$PGGRAPH_ROOT/scripts/lib/pggraph-common.sh"
database="${DB_PREFIX:-pggraph_provenance_$$}"
pggraph_validate_database_name "$database"
if (( ${#database} > 63 )); then
  pggraph_die "database name exceeds PostgreSQL's identifier limit"
  exit 2
fi
createdb "$database"
psql -X -qAt -v ON_ERROR_STOP=1 -d "$database" <<'SQL'
CREATE EXTENSION graph;
CREATE TABLE n (id text PRIMARY KEY);
INSERT INTO n VALUES ('a');
SELECT graph.add_table('n'::regclass, 'id');
SELECT * FROM graph.build();
SELECT count(*) FROM graph.traverse('n'::regclass, 'a', 1);
CREATE TEMP TABLE generation_before AS
SELECT manifest_generation, manifest_watermark FROM graph.projection_status();
CREATE FUNCTION assert_stale_catalog() RETURNS void LANGUAGE plpgsql AS $$
DECLARE detail text;
BEGIN
  BEGIN
    PERFORM * FROM graph.traverse('n'::regclass, 'a', 1);
    RAISE EXCEPTION 'stale graph catalog was accepted';
  EXCEPTION WHEN SQLSTATE 'XX000' THEN
    GET STACKED DIAGNOSTICS detail = PG_EXCEPTION_DETAIL;
    IF detail IS DISTINCT FROM 'pgGraph diagnostic: PG000' THEN
      RAISE EXCEPTION 'unexpected catalog diagnostic: %', detail;
    END IF;
  END;
END $$;
CREATE TABLE extra (id text PRIMARY KEY);
INSERT INTO extra VALUES ('x');
SELECT graph.add_table('extra'::regclass, 'id');
SELECT assert_stale_catalog();
INSERT INTO n VALUES ('b');
DO $$ BEGIN
  BEGIN
    PERFORM * FROM graph.ingest_projection();
    RAISE EXCEPTION 'ingestion accepted stale catalog';
  EXCEPTION WHEN SQLSTATE 'XX000' THEN NULL;
  END;
  IF EXISTS (
    SELECT manifest_generation, manifest_watermark FROM graph.projection_status()
    EXCEPT SELECT * FROM generation_before
  ) THEN
    RAISE EXCEPTION 'rejected ingestion changed durable generation or watermark';
  END IF;
END $$;
SQL
psql -X -qAt -v ON_ERROR_STOP=1 -d "$database" -c 'SELECT assert_stale_catalog()'
psql -X -qAt -v ON_ERROR_STOP=1 -d "$database" <<'SQL'
SELECT * FROM graph.build();
DO $$ BEGIN
  IF (SELECT count(*) FROM graph.traverse('extra'::regclass, 'x', 1)) <> 1 THEN
    RAISE EXCEPTION 'rebuild did not establish the new catalog';
  END IF;
END $$;
SQL
psql -X -qAt -v ON_ERROR_STOP=1 -d "$database" <<'SQL'
CREATE TABLE e (id serial PRIMARY KEY, src text, dst text,
  CONSTRAINT endpoint_source FOREIGN KEY (src) REFERENCES n(id));
INSERT INTO extra VALUES ('a'), ('b');
INSERT INTO e (src, dst) VALUES ('a', 'b');
SELECT graph.add_edge('e'::regclass, 'src', 'n'::regclass, 'dst', 'link', false);
SELECT * FROM graph.build();
ALTER TABLE e DROP CONSTRAINT endpoint_source;
ALTER TABLE e ADD CONSTRAINT endpoint_source FOREIGN KEY (src) REFERENCES extra(id);
SELECT assert_stale_catalog();
SQL
psql -X -qAt -v ON_ERROR_STOP=1 -d "$database" -c 'SELECT assert_stale_catalog()'
printf 'Persisted catalog provenance passed (%s)\n' "$database"
