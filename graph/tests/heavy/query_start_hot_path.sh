#!/usr/bin/env bash
set -euo pipefail

DBNAME="${DBNAME:-pggraph_query_start_hot_path}"
PG_VERSION_FEATURE="${PG_VERSION_FEATURE:-pg17}"
PG_MAJOR="${PG_VERSION_FEATURE#pg}"
PG_CONFIG="${PG_CONFIG:-}"

if [[ -z "$PG_CONFIG" ]]; then
  if [[ -x "/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config"
  elif [[ -x "/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config" ]]; then
    PG_CONFIG="/opt/homebrew/opt/postgresql@${PG_MAJOR}/bin/pg_config"
  else
    echo "PG_CONFIG is required for $PG_VERSION_FEATURE"
    exit 2
  fi
fi

if [[ "${SKIP_INSTALL:-0}" != "1" ]]; then
  cargo pgrx install \
    --pg-config "$PG_CONFIG" \
    --features "$PG_VERSION_FEATURE" \
    --no-default-features
fi
dropdb --if-exists "$DBNAME" >/dev/null 2>&1 || true
createdb "$DBNAME"

psql -X -v ON_ERROR_STOP=1 -d "$DBNAME" <<'SQL'
CREATE EXTENSION graph;
SET graph.persist_on_build = off;
SET graph.sync_mode = 'manual';

CREATE TABLE public.query_start_nodes (
    id text PRIMARY KEY,
    parent_id text REFERENCES public.query_start_nodes(id),
    name text NOT NULL
);
INSERT INTO public.query_start_nodes (id, parent_id, name)
VALUES ('root', NULL, 'Root'), ('child', 'root', 'Child');

SELECT graph.add_table(
    'public.query_start_nodes'::regclass,
    'id',
    ARRAY['name', 'parent_id']::text[],
    NULL
);
SELECT graph.add_edge(
    'public.query_start_nodes'::regclass,
    'parent_id',
    'public.query_start_nodes'::regclass,
    'id',
    'parent',
    false
);
SELECT * FROM graph.build();

CREATE TEMP TABLE query_start_samples (
    metric text NOT NULL,
    elapsed_ms double precision NOT NULL,
    row_count bigint NOT NULL
);

DO $benchmark$
DECLARE
    iteration integer;
    started_at timestamptz;
    observed_rows bigint;
BEGIN
    FOR iteration IN 1..45 LOOP
        started_at := clock_timestamp();
        SELECT count(*) INTO observed_rows
        FROM graph.traverse(
            'public.query_start_nodes'::regclass,
            'root',
            0,
            hydrate := false
        );
        IF iteration > 5 THEN
            INSERT INTO query_start_samples
            VALUES (
                'traverse_depth_zero',
                extract(epoch FROM clock_timestamp() - started_at) * 1000.0,
                observed_rows
            );
        END IF;

        started_at := clock_timestamp();
        SELECT count(*) INTO observed_rows FROM (VALUES (1)) AS control(value);
        IF iteration > 5 THEN
            INSERT INTO query_start_samples
            VALUES (
                'sql_negative_control',
                extract(epoch FROM clock_timestamp() - started_at) * 1000.0,
                observed_rows
            );
        END IF;
    END LOOP;
END
$benchmark$;

SELECT metric,
       count(*) AS samples,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY elapsed_ms)::numeric, 3)
           AS median_ms,
       round(percentile_cont(0.95) WITHIN GROUP (ORDER BY elapsed_ms)::numeric, 3)
           AS p95_ms,
       min(row_count) AS min_rows,
       max(row_count) AS max_rows
FROM query_start_samples
GROUP BY metric
ORDER BY metric;

DO $assertions$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM query_start_samples
        WHERE row_count <> 1
    ) THEN
        RAISE EXCEPTION 'query-start benchmark row counts changed';
    END IF;
END
$assertions$;
SQL

echo "Query-start hot-path benchmark passed on database: $DBNAME"
