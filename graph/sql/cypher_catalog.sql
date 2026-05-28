-- ─── Cypher frontend catalog tables (feat/cypher-frontend, M0) ─────
--
-- Maps Cypher labels and relationship types onto pgGraph's registered
-- tables and edges. The cypher_facade module reads from these tables
-- to build a cyrs_schema::SchemaProvider snapshot per query.
--
-- See: docs/contributor_guide/cypher-frontend/020-catalog-extensions.md

-- ──────────────────────────────────────────────────────────────────
-- _registered_labels: Cypher label → Postgres table mapping.
-- A label maps to exactly one table. When the same table stores
-- multiple labels, the discriminator_col + discriminator_val pair
-- selects rows for this label.
-- ──────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS graph._registered_labels (
    label             TEXT PRIMARY KEY,
    table_name        TEXT NOT NULL REFERENCES graph._registered_tables(table_name)
                                     ON DELETE CASCADE,
    discriminator_col TEXT NULL,
    discriminator_val TEXT NULL,
    CHECK ((discriminator_col IS NULL) = (discriminator_val IS NULL))
);

CREATE INDEX IF NOT EXISTS _registered_labels_table_idx
    ON graph._registered_labels(table_name);

-- ──────────────────────────────────────────────────────────────────
-- _registered_label_properties: Cypher property → table column.
-- One row per (label, property). Missing rows = "property not
-- stored on this label" → sema diagnostic + NULL reads.
-- ──────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS graph._registered_label_properties (
    label        TEXT NOT NULL,
    property     TEXT NOT NULL,
    column_name  TEXT NOT NULL,
    column_type  TEXT NOT NULL,
    required     BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (label, property),
    FOREIGN KEY (label) REFERENCES graph._registered_labels(label)
        ON DELETE CASCADE
);

-- ──────────────────────────────────────────────────────────────────
-- _registered_rel_types: Cypher rel type → existing pgGraph edge.
-- Binds a Cypher relationship type name to a registered edge row
-- (one of _registered_edges).
-- ──────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS graph._registered_rel_types (
    rel_type     TEXT PRIMARY KEY,
    from_table   TEXT NOT NULL,
    from_column  TEXT NOT NULL,
    to_table     TEXT NOT NULL,
    to_column    TEXT NOT NULL,
    label        TEXT NOT NULL,
    FOREIGN KEY (from_table, from_column, to_table, to_column, label)
        REFERENCES graph._registered_edges
            (from_table, from_column, to_table, to_column, label)
        ON DELETE CASCADE
);

-- ──────────────────────────────────────────────────────────────────
-- _registered_rel_properties: Cypher rel property → column (for
-- junction-table edges that carry their own properties).
-- ──────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS graph._registered_rel_properties (
    rel_type     TEXT NOT NULL,
    property     TEXT NOT NULL,
    column_name  TEXT NOT NULL,
    column_type  TEXT NOT NULL,
    required     BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (rel_type, property),
    FOREIGN KEY (rel_type) REFERENCES graph._registered_rel_types(rel_type)
        ON DELETE CASCADE
);

-- ──────────────────────────────────────────────────────────────────
-- _registered_unique_props: Declared uniqueness tuples (required
-- for MERGE atomicity — sema requires a matching tuple here before
-- allowing INSERT ... ON CONFLICT lowering).
-- ──────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS graph._registered_unique_props (
    kind     TEXT NOT NULL CHECK (kind IN ('label', 'rel_type')),
    name     TEXT NOT NULL,
    props    TEXT[] NOT NULL,
    PRIMARY KEY (kind, name, props)
);

-- ──────────────────────────────────────────────────────────────────
-- _registered_label_sets: Declared multi-label compatibility. Each
-- row is a sorted-distinct label tuple permitted to co-exist on a
-- single node. Empty table = single-label-only.
-- ──────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS graph._registered_label_sets (
    labels   TEXT[] PRIMARY KEY
);

-- ──────────────────────────────────────────────────────────────────
-- Permissions: same posture as the base catalog tables.
-- ──────────────────────────────────────────────────────────────────
REVOKE ALL ON TABLE graph._registered_labels           FROM PUBLIC;
REVOKE ALL ON TABLE graph._registered_label_properties FROM PUBLIC;
REVOKE ALL ON TABLE graph._registered_rel_types        FROM PUBLIC;
REVOKE ALL ON TABLE graph._registered_rel_properties   FROM PUBLIC;
REVOKE ALL ON TABLE graph._registered_unique_props     FROM PUBLIC;
REVOKE ALL ON TABLE graph._registered_label_sets       FROM PUBLIC;

GRANT SELECT ON TABLE graph._registered_labels           TO PUBLIC;
GRANT SELECT ON TABLE graph._registered_label_properties TO PUBLIC;
GRANT SELECT ON TABLE graph._registered_rel_types        TO PUBLIC;
GRANT SELECT ON TABLE graph._registered_rel_properties   TO PUBLIC;
GRANT SELECT ON TABLE graph._registered_unique_props     TO PUBLIC;
GRANT SELECT ON TABLE graph._registered_label_sets       TO PUBLIC;

-- Mark these as configuration tables so pg_dump preserves their contents.
SELECT pg_catalog.pg_extension_config_dump('graph._registered_labels',           '');
SELECT pg_catalog.pg_extension_config_dump('graph._registered_label_properties', '');
SELECT pg_catalog.pg_extension_config_dump('graph._registered_rel_types',        '');
SELECT pg_catalog.pg_extension_config_dump('graph._registered_rel_properties',   '');
SELECT pg_catalog.pg_extension_config_dump('graph._registered_unique_props',     '');
SELECT pg_catalog.pg_extension_config_dump('graph._registered_label_sets',       '');
