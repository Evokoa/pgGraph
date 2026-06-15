-- graph extension bootstrap SQL
-- Creates catalog tables for storing registered tables and edges.

DO $$
BEGIN
    CREATE TYPE graph.node_ref AS (
        node_table REGCLASS,
        node_id    TEXT
    );
EXCEPTION WHEN duplicate_object THEN
    NULL;
END
$$;

CREATE OR REPLACE FUNCTION graph.node_ref(node_table REGCLASS, node_id TEXT)
RETURNS graph.node_ref
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT (node_table, node_id)::graph.node_ref
$$;

CREATE OR REPLACE FUNCTION graph.traverse(
    starts        graph.node_ref[],
    max_depth     INTEGER DEFAULT (current_setting('graph.default_max_depth'))::INTEGER,
    edge_types    TEXT[] DEFAULT NULL,
    direction     TEXT DEFAULT 'any',
    node_tables   OID[] DEFAULT NULL,
    filter        JSONB DEFAULT NULL,
    tenant        TEXT DEFAULT NULL,
    strategy      TEXT DEFAULT 'bfs',
    uniqueness    TEXT DEFAULT 'node_global',
    include_start BOOLEAN DEFAULT true,
    hydrate       BOOLEAN DEFAULT true,
    max_rows      INTEGER DEFAULT 1000,
    row_offset    INTEGER DEFAULT 0,
    max_nodes     INTEGER DEFAULT (current_setting('graph.max_nodes'))::INTEGER,
    max_frontier  INTEGER DEFAULT (current_setting('graph.max_frontier'))::INTEGER
)
RETURNS TABLE (
    root_table OID,
    root_id    TEXT,
    node_table OID,
    node_id    TEXT,
    depth      INTEGER,
    path       JSONB,
    edge_path  JSONB,
    node       JSONB,
    root_table_name TEXT,
    node_table_name TEXT
)
LANGUAGE sql
STABLE
COST 1000
ROWS 1000
AS $$
    SELECT t.root_table,
           t.root_id,
           t.node_table,
           t.node_id,
           t.depth,
           t.path,
           t.edge_path,
           t.node,
           t.root_table_name,
           t.node_table_name
    FROM graph.traverse(
        ARRAY(SELECT start_ref.node_table::oid FROM unnest($1) AS start_ref),
        ARRAY(SELECT start_ref.node_id FROM unnest($1) AS start_ref),
        $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15
    ) AS t;
$$;

CREATE TABLE IF NOT EXISTS graph._registered_tables (
    table_name TEXT PRIMARY KEY,
    id_column  TEXT NOT NULL,
    columns    TEXT DEFAULT '',
    tenant_column TEXT
);

CREATE TABLE IF NOT EXISTS graph._registered_edges (
    from_table    TEXT NOT NULL,
    from_column   TEXT NOT NULL,
    to_table      TEXT NOT NULL,
    to_column     TEXT NOT NULL,
    label         TEXT NOT NULL,
    bidirectional BOOLEAN DEFAULT true,
    weight_column TEXT,
    label_column  TEXT,
    UNIQUE (from_table, from_column, to_table, to_column, label)
);

ALTER TABLE graph._registered_edges
    ADD COLUMN IF NOT EXISTS label_column TEXT;

ALTER TABLE graph._registered_tables
    ADD COLUMN IF NOT EXISTS tenant_column TEXT;

CREATE TABLE IF NOT EXISTS graph._registered_filter_columns (
    table_name  TEXT NOT NULL,
    column_name TEXT NOT NULL,
    column_type TEXT NOT NULL DEFAULT 'numeric',
    UNIQUE (table_name, column_name)
);

ALTER TABLE graph._registered_filter_columns
    ADD COLUMN IF NOT EXISTS column_type TEXT NOT NULL DEFAULT 'numeric';

CREATE TABLE IF NOT EXISTS graph._graphs (
    graph_id        UUID PRIMARY KEY,
    graph_name      TEXT NOT NULL,
    owner_role      OID NOT NULL,
    created_by      OID NOT NULL,
    tenant          TEXT,
    namespace       TEXT,
    graph_kind      TEXT NOT NULL CHECK (graph_kind IN ('global', 'user', 'tenant', 'workspace', 'subgraph')),
    residency       TEXT NOT NULL CHECK (residency IN ('hot', 'warm', 'cold')),
    materialization TEXT NOT NULL CHECK (materialization IN ('shared', 'dedicated')),
    projection_mode TEXT NOT NULL CHECK (projection_mode IN ('csr_readonly', 'mutable_overlay')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE graph._graphs
    ADD COLUMN IF NOT EXISTS tenant TEXT,
    ADD COLUMN IF NOT EXISTS namespace TEXT,
    ADD COLUMN IF NOT EXISTS graph_kind TEXT,
    ADD COLUMN IF NOT EXISTS residency TEXT,
    ADD COLUMN IF NOT EXISTS materialization TEXT,
    ADD COLUMN IF NOT EXISTS projection_mode TEXT,
    ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ;

UPDATE graph._graphs
SET namespace = COALESCE(namespace, 'public'),
    graph_kind = COALESCE(graph_kind, 'global'),
    residency = COALESCE(residency, 'hot'),
    materialization = COALESCE(materialization, 'shared'),
    projection_mode = COALESCE(projection_mode, 'csr_readonly'),
    created_at = COALESCE(created_at, now()),
    updated_at = COALESCE(updated_at, now());

ALTER TABLE graph._graphs
    ALTER COLUMN graph_kind SET NOT NULL,
    ALTER COLUMN residency SET NOT NULL,
    ALTER COLUMN materialization SET NOT NULL,
    ALTER COLUMN projection_mode SET NOT NULL,
    ALTER COLUMN created_at SET DEFAULT now(),
    ALTER COLUMN created_at SET NOT NULL,
    ALTER COLUMN updated_at SET DEFAULT now(),
    ALTER COLUMN updated_at SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._graphs'::regclass
          AND conname = '_graphs_graph_kind_check'
    ) THEN
        ALTER TABLE graph._graphs
            ADD CONSTRAINT _graphs_graph_kind_check
            CHECK (graph_kind IN ('global', 'user', 'tenant', 'workspace', 'subgraph'));
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._graphs'::regclass
          AND conname = '_graphs_residency_check'
    ) THEN
        ALTER TABLE graph._graphs
            ADD CONSTRAINT _graphs_residency_check
            CHECK (residency IN ('hot', 'warm', 'cold'));
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._graphs'::regclass
          AND conname = '_graphs_materialization_check'
    ) THEN
        ALTER TABLE graph._graphs
            ADD CONSTRAINT _graphs_materialization_check
            CHECK (materialization IN ('shared', 'dedicated'));
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._graphs'::regclass
          AND conname = '_graphs_projection_mode_check'
    ) THEN
        ALTER TABLE graph._graphs
            ADD CONSTRAINT _graphs_projection_mode_check
            CHECK (projection_mode IN ('csr_readonly', 'mutable_overlay'));
    END IF;
END $$;

INSERT INTO graph._graphs (
    graph_id,
    graph_name,
    owner_role,
    created_by,
    tenant,
    namespace,
    graph_kind,
    residency,
    materialization,
    projection_mode
)
SELECT '00000000-0000-0000-0000-000000000001'::uuid,
       'default',
       current_user::regrole::oid,
       current_user::regrole::oid,
       NULL,
       'public',
       'global',
       'hot',
       'shared',
       'csr_readonly'
WHERE NOT EXISTS (
    SELECT 1
    FROM graph._graphs
    WHERE graph_id = '00000000-0000-0000-0000-000000000001'::uuid
);

CREATE UNIQUE INDEX IF NOT EXISTS _graphs_identity_idx
    ON graph._graphs (
        COALESCE(tenant, ''),
        owner_role,
        COALESCE(namespace, ''),
        graph_name
    );

ALTER TABLE graph._registered_tables
    ADD COLUMN IF NOT EXISTS graph_id UUID;

ALTER TABLE graph._registered_edges
    ADD COLUMN IF NOT EXISTS graph_id UUID;

ALTER TABLE graph._registered_filter_columns
    ADD COLUMN IF NOT EXISTS graph_id UUID;

UPDATE graph._registered_tables
SET graph_id = '00000000-0000-0000-0000-000000000001'::uuid
WHERE graph_id IS NULL;

UPDATE graph._registered_edges
SET graph_id = '00000000-0000-0000-0000-000000000001'::uuid
WHERE graph_id IS NULL;

UPDATE graph._registered_filter_columns
SET graph_id = '00000000-0000-0000-0000-000000000001'::uuid
WHERE graph_id IS NULL;

ALTER TABLE graph._registered_tables
    ALTER COLUMN graph_id SET NOT NULL;

ALTER TABLE graph._registered_edges
    ALTER COLUMN graph_id SET NOT NULL;

ALTER TABLE graph._registered_filter_columns
    ALTER COLUMN graph_id SET NOT NULL;

ALTER TABLE graph._registered_tables
    DROP CONSTRAINT IF EXISTS _registered_tables_pkey;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._registered_tables'::regclass
          AND conname = '_registered_tables_pkey'
    ) THEN
        ALTER TABLE graph._registered_tables
            ADD CONSTRAINT _registered_tables_pkey PRIMARY KEY (graph_id, table_name);
    END IF;
END $$;

DO $$
DECLARE
    constraint_name name;
BEGIN
    FOR constraint_name IN
        SELECT conname
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._registered_edges'::regclass
          AND contype = 'u'
    LOOP
        EXECUTE format('ALTER TABLE graph._registered_edges DROP CONSTRAINT %I', constraint_name);
    END LOOP;

    FOR constraint_name IN
        SELECT conname
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._registered_filter_columns'::regclass
          AND contype = 'u'
    LOOP
        EXECUTE format('ALTER TABLE graph._registered_filter_columns DROP CONSTRAINT %I', constraint_name);
    END LOOP;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS _registered_edges_graph_identity_idx
    ON graph._registered_edges (
        graph_id,
        from_table,
        from_column,
        to_table,
        to_column,
        label
    );

CREATE UNIQUE INDEX IF NOT EXISTS _registered_filter_columns_graph_identity_idx
    ON graph._registered_filter_columns (
        graph_id,
        table_name,
        column_name
    );

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._registered_tables'::regclass
          AND conname = '_registered_tables_graph_id_fkey'
    ) THEN
        ALTER TABLE graph._registered_tables
            ADD CONSTRAINT _registered_tables_graph_id_fkey
            FOREIGN KEY (graph_id) REFERENCES graph._graphs(graph_id) ON DELETE RESTRICT;
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._registered_edges'::regclass
          AND conname = '_registered_edges_graph_id_fkey'
    ) THEN
        ALTER TABLE graph._registered_edges
            ADD CONSTRAINT _registered_edges_graph_id_fkey
            FOREIGN KEY (graph_id) REFERENCES graph._graphs(graph_id) ON DELETE RESTRICT;
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._registered_filter_columns'::regclass
          AND conname = '_registered_filter_columns_graph_id_fkey'
    ) THEN
        ALTER TABLE graph._registered_filter_columns
            ADD CONSTRAINT _registered_filter_columns_graph_id_fkey
            FOREIGN KEY (graph_id) REFERENCES graph._graphs(graph_id) ON DELETE RESTRICT;
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS graph._build_jobs (
    build_id       TEXT PRIMARY KEY,
    status         TEXT NOT NULL CHECK (status IN ('queued', 'running', 'completed', 'failed')),
    nodes_loaded   BIGINT,
    edges_loaded   BIGINT,
    build_time_ms  DOUBLE PRECISION,
    memory_used_mb DOUBLE PRECISION,
    sync_mode      TEXT NOT NULL DEFAULT 'trigger',
    projection_mode TEXT NOT NULL DEFAULT 'csr_readonly'
        CHECK (projection_mode IN ('csr_readonly', 'mutable_overlay')),
    progress_phase TEXT NOT NULL DEFAULT 'queued',
    progress_message TEXT,
    started_at     TIMESTAMPTZ,
    finished_at    TIMESTAMPTZ,
    error          TEXT,
    worker_pid     INTEGER,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE graph._build_jobs
    ADD COLUMN IF NOT EXISTS projection_mode TEXT,
    ADD COLUMN IF NOT EXISTS progress_phase TEXT,
    ADD COLUMN IF NOT EXISTS progress_message TEXT;

UPDATE graph._build_jobs
SET projection_mode = 'csr_readonly'
WHERE projection_mode IS NULL
   OR projection_mode NOT IN ('csr_readonly', 'mutable_overlay');

UPDATE graph._build_jobs
SET progress_phase = CASE status
    WHEN 'running' THEN 'building'
    ELSE status
END
WHERE progress_phase IS NULL;

UPDATE graph._build_jobs
SET progress_message = CASE status
    WHEN 'queued' THEN 'queued for background build'
    WHEN 'running' THEN 'building graph from registered source tables'
    WHEN 'completed' THEN 'build completed'
    WHEN 'failed' THEN COALESCE(error, 'build failed')
    ELSE progress_message
END
WHERE progress_message IS NULL;

ALTER TABLE graph._build_jobs
    ALTER COLUMN projection_mode SET DEFAULT 'csr_readonly',
    ALTER COLUMN projection_mode SET NOT NULL,
    ALTER COLUMN progress_phase SET DEFAULT 'queued',
    ALTER COLUMN progress_phase SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint
        WHERE conrelid = 'graph._build_jobs'::regclass
          AND conname = '_build_jobs_projection_mode_check'
    ) THEN
        ALTER TABLE graph._build_jobs
            ADD CONSTRAINT _build_jobs_projection_mode_check
            CHECK (projection_mode IN ('csr_readonly', 'mutable_overlay'));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS _build_jobs_status_idx
    ON graph._build_jobs (status, created_at);

CREATE TABLE IF NOT EXISTS graph._maintenance_jobs (
    job_id            TEXT PRIMARY KEY,
    status            TEXT NOT NULL CHECK (status IN ('queued', 'running', 'completed', 'failed')),
    sync_rows_applied BIGINT,
    nodes_after       BIGINT,
    edges_after       BIGINT,
    vacuum_time_ms    DOUBLE PRECISION,
    progress_phase    TEXT NOT NULL DEFAULT 'queued',
    progress_message  TEXT,
    started_at        TIMESTAMPTZ,
    finished_at       TIMESTAMPTZ,
    error             TEXT,
    worker_pid        INTEGER,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE graph._maintenance_jobs
    ADD COLUMN IF NOT EXISTS progress_phase TEXT,
    ADD COLUMN IF NOT EXISTS progress_message TEXT;

UPDATE graph._maintenance_jobs
SET progress_phase = CASE status
    WHEN 'running' THEN 'rebuilding'
    ELSE status
END
WHERE progress_phase IS NULL;

UPDATE graph._maintenance_jobs
SET progress_message = CASE status
    WHEN 'queued' THEN 'queued for background maintenance'
    WHEN 'running' THEN 'rebuilding graph for maintenance'
    WHEN 'completed' THEN 'maintenance completed'
    WHEN 'failed' THEN COALESCE(error, 'maintenance failed')
    ELSE progress_message
END
WHERE progress_message IS NULL;

ALTER TABLE graph._maintenance_jobs
    ALTER COLUMN progress_phase SET DEFAULT 'queued',
    ALTER COLUMN progress_phase SET NOT NULL;

CREATE INDEX IF NOT EXISTS _maintenance_jobs_status_idx
    ON graph._maintenance_jobs (status, created_at);

CREATE TABLE IF NOT EXISTS graph._sync_log (
    id             BIGSERIAL PRIMARY KEY,
    op             CHAR(1) NOT NULL,
    table_oid      OID,
    table_name     TEXT NOT NULL,
    pk             TEXT,
    old_pk         TEXT,
    new_pk         TEXT,
    properties     JSONB,
    old_row        JSONB,
    new_row        JSONB,
    xid            BIGINT,
    needs_vacuum   BOOLEAN DEFAULT false,
    error_message  TEXT,
    created_at     TIMESTAMPTZ DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_sync_log_id ON graph._sync_log (id);
CREATE INDEX IF NOT EXISTS idx_sync_log_created ON graph._sync_log (created_at);

CREATE TABLE IF NOT EXISTS graph._projection_generations (
    generation_id     BIGINT NOT NULL CHECK (generation_id > 0),
    backend_pid       INTEGER NOT NULL DEFAULT 0,
    database_oid      OID NOT NULL,
    heartbeat_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    sync_watermark    BIGINT NOT NULL DEFAULT 0 CHECK (sync_watermark >= 0),
    validation_status TEXT NOT NULL DEFAULT 'valid'
        CHECK (validation_status IN ('valid', 'corrupt', 'repairing')),
    repair_status     TEXT,
    is_current        BOOLEAN NOT NULL DEFAULT false,
    published_at      TIMESTAMPTZ,
    retained_until    TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (generation_id, backend_pid, database_oid)
);

CREATE INDEX IF NOT EXISTS idx_projection_generations_current
    ON graph._projection_generations (is_current, generation_id DESC);
CREATE INDEX IF NOT EXISTS idx_projection_generations_active
    ON graph._projection_generations (database_oid, expires_at)
    WHERE backend_pid <> 0;

CREATE TABLE IF NOT EXISTS graph._sync_buffer (
    id         BIGSERIAL PRIMARY KEY,
    op         CHAR(1) NOT NULL,
    table_name TEXT NOT NULL,
    pk         TEXT NOT NULL,
    old_pk     TEXT,
    new_pk     TEXT,
    properties JSONB,
    created_at TIMESTAMPTZ DEFAULT now()
);

ALTER TABLE graph._sync_buffer ADD COLUMN IF NOT EXISTS old_pk TEXT;
ALTER TABLE graph._sync_buffer ADD COLUMN IF NOT EXISTS new_pk TEXT;

CREATE INDEX IF NOT EXISTS idx_sync_buffer_created ON graph._sync_buffer (created_at);

-- Preserve extension-owned operational state across pg_dump/pg_restore.
-- Source tables remain authoritative for graph contents, but registered graph
-- catalogs, durable jobs, and unapplied sync rows are database state rather
-- than extension install metadata.
SELECT pg_catalog.pg_extension_config_dump('graph._registered_tables', '');
SELECT pg_catalog.pg_extension_config_dump('graph._registered_edges', '');
SELECT pg_catalog.pg_extension_config_dump('graph._registered_filter_columns', '');
SELECT pg_catalog.pg_extension_config_dump('graph._graphs', '');
SELECT pg_catalog.pg_extension_config_dump('graph._build_jobs', '');
SELECT pg_catalog.pg_extension_config_dump('graph._maintenance_jobs', '');
SELECT pg_catalog.pg_extension_config_dump('graph._sync_log', '');
SELECT pg_catalog.pg_extension_config_dump('graph._sync_log_id_seq', '');
SELECT pg_catalog.pg_extension_config_dump('graph._projection_generations', '');
SELECT pg_catalog.pg_extension_config_dump('graph._sync_buffer', '');
SELECT pg_catalog.pg_extension_config_dump('graph._sync_buffer_id_seq', '');

-- Do not run graph.auto_discover() during CREATE EXTENSION.
--
-- PostgreSQL records objects created while an extension script is running as
-- extension members. graph.auto_discover() calls graph.build(), and build uses
-- ON COMMIT DROP temp tables; if those temp tables are created inside the
-- extension transaction, PostgreSQL refuses to drop them because they are
-- marked as extension-owned. Users should run graph.auto_discover() after
-- CREATE EXTENSION completes.

-- ─── Privilege hardening ─────────────────────────────────────────────
-- Internal catalog tables should not be directly writable by non-admin
-- users. Access is mediated through the graph.* SQL API functions.
REVOKE ALL ON TABLE graph._registered_tables       FROM PUBLIC;
REVOKE ALL ON TABLE graph._registered_edges        FROM PUBLIC;
REVOKE ALL ON TABLE graph._registered_filter_columns FROM PUBLIC;
REVOKE ALL ON TABLE graph._graphs                 FROM PUBLIC;
REVOKE ALL ON TABLE graph._build_jobs             FROM PUBLIC;
REVOKE ALL ON TABLE graph._maintenance_jobs       FROM PUBLIC;
REVOKE ALL ON TABLE graph._sync_log               FROM PUBLIC;
REVOKE ALL ON TABLE graph._projection_generations FROM PUBLIC;
REVOKE ALL ON TABLE graph._sync_buffer            FROM PUBLIC;
GRANT SELECT ON TABLE graph._registered_tables       TO PUBLIC;
GRANT SELECT ON TABLE graph._registered_edges        TO PUBLIC;
GRANT SELECT ON TABLE graph._registered_filter_columns TO PUBLIC;
GRANT SELECT ON TABLE graph._graphs                 TO PUBLIC;
GRANT SELECT ON TABLE graph._build_jobs             TO PUBLIC;
GRANT SELECT ON TABLE graph._maintenance_jobs       TO PUBLIC;
GRANT SELECT ON TABLE graph._sync_log               TO PUBLIC;
GRANT SELECT ON TABLE graph._projection_generations TO PUBLIC;
GRANT SELECT ON TABLE graph._sync_buffer            TO PUBLIC;
GRANT SELECT ON SEQUENCE graph._sync_log_id_seq     TO PUBLIC;
GRANT SELECT ON SEQUENCE graph._sync_buffer_id_seq  TO PUBLIC;

-- Catalog mutation, build/vacuum, sync apply, reset, and global analytics are
-- protected in Rust by graph-admin checks. Production deployments should still
-- grant application roles only the reader functions they need.
