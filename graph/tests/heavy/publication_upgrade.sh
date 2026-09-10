#!/usr/bin/env bash
set -euo pipefail

# Requires the released 1.2.0 install SQL and candidate 1.2.1 update SQL.
PGGRAPH_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$PGGRAPH_ROOT/scripts/lib/pggraph-common.sh"
database="${DB_PREFIX:-pggraph_publication_upgrade_$$}"
pggraph_validate_database_name "$database"
createdb "$database"
psql -X -v ON_ERROR_STOP=1 -d "$database" <<'SQL'
CREATE EXTENSION graph VERSION '1.2.0';
DO $$ BEGIN
  IF to_regclass('graph._projection_heads') IS NOT NULL THEN
    RAISE EXCEPTION 'upgrade test requires the released 1.2.0 schema';
  END IF;
END $$;
REVOKE EXECUTE ON FUNCTION graph.test_enabled() FROM PUBLIC;
CREATE TEMP TABLE original_functions AS
SELECT oid, proowner, proacl FROM pg_proc WHERE pronamespace = 'graph'::regnamespace;
CREATE TABLE n(id text PRIMARY KEY);
INSERT INTO n VALUES ('a'), ('b');
SELECT graph.add_table('n'::regclass, 'id');
ALTER EXTENSION graph UPDATE TO '1.2.1';
DO $$ BEGIN
  IF EXISTS (
    SELECT 1 FROM original_functions original
    LEFT JOIN pg_proc current ON current.oid = original.oid
    WHERE current.oid IS NULL OR current.proowner <> original.proowner
       OR current.proacl IS DISTINCT FROM original.proacl
  ) THEN
    RAISE EXCEPTION 'upgrade changed an existing function owner, grant or identity';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'graph'
             AND 'graph._projection_heads'::regclass = ANY(extconfig)) THEN
    RAISE EXCEPTION 'derived publication metadata must not be dumped';
  END IF;
  IF EXISTS (SELECT 1 FROM graph._projection_heads) THEN
    RAISE EXCEPTION 'upgrade must not adopt filesystem artifacts';
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_proc
    WHERE oid = 'graph.sync_retention()'::regprocedure
      AND pg_catalog.pg_get_function_result(oid) =
          'TABLE(eligible_prune_floor bigint, prune_blocker text, retained_graph_rows bigint, database_sync_log_bytes bigint, active_sync_watermark_backends integer)'
      AND prosecdef
      AND proconfig = ARRAY['search_path=pg_catalog, pg_temp']::text[]
  ) THEN
    RAISE EXCEPTION 'upgraded sync_retention has an incorrect result shape or security boundary';
  END IF;
  IF (SELECT count(*) FROM graph.sync_retention()) <> 1 THEN
    RAISE EXCEPTION 'upgraded sync_retention did not return one diagnostic row';
  END IF;
END $$;
SELECT * FROM graph.build();
DO $$ BEGIN
  IF (SELECT count(*) FROM graph.traverse('n'::regclass, 'a', 1)) <> 1 THEN
    RAISE EXCEPTION 'upgraded graph did not rebuild';
  END IF;
END $$;
SQL
printf 'Transactional publication upgrade passed (%s)\n' "$database"
