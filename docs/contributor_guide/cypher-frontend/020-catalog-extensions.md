# Catalog extensions

cyrs is schema-aware via `SchemaProvider` (declared in
`cyrs-schema`). pgGraph already has a catalog of registered tables
and edges (`graph._registered_tables`, `graph._registered_edges`). To
serve Cypher we need a thin layer above this that maps Cypher
*labels* and *relationship types* onto Postgres tables, columns, and
unique constraints.

The invariant: **the catalog is the source of truth for the
label↔storage mapping.** Sema never guesses. If a label has no
mapping, sema produces a diagnostic with a host-range code (see
`feat-request.md` §3.1) and the query is rejected before any DML is
emitted.

## New catalog tables

```sql
-- A Cypher label, and which Postgres table stores nodes carrying it.
-- A label maps to exactly one table. (Multi-label compositions are
-- handled below via _registered_label_overlay.)
CREATE TABLE graph._registered_labels (
    label             text PRIMARY KEY,
    table_name        text NOT NULL REFERENCES graph._registered_tables(table_name),
    discriminator_col text NULL,     -- when the same table stores multiple labels,
                                     -- this column carries the label name.
    discriminator_val text NULL,     -- the value of that column for this label.
    CHECK ((discriminator_col IS NULL) = (discriminator_val IS NULL))
);

-- Cypher property → table column mapping. One row per (label, property).
-- Missing rows = "property not stored on this label" (sema emits a
-- diagnostic; reads return NULL).
CREATE TABLE graph._registered_label_properties (
    label        text NOT NULL,
    property     text NOT NULL,
    column_name  text NOT NULL,
    column_type  text NOT NULL,      -- pg type as text (e.g. 'text', 'int8', 'jsonb')
    required     bool NOT NULL DEFAULT false,
    PRIMARY KEY (label, property),
    FOREIGN KEY (label) REFERENCES graph._registered_labels(label) ON DELETE CASCADE
);

-- Cypher relationship type → (edge storage) mapping.
-- pgGraph already has _registered_edges which encodes the FK shape;
-- this table just gives the Cypher rel-type a name and binds it to one
-- of those edges. One row per Cypher rel type.
CREATE TABLE graph._registered_rel_types (
    rel_type     text PRIMARY KEY,
    from_table   text NOT NULL,
    from_column  text NOT NULL,
    to_table     text NOT NULL,
    to_column    text NOT NULL,
    label        text NOT NULL,      -- matches _registered_edges.label
    FOREIGN KEY (from_table, from_column, to_table, to_column, label)
        REFERENCES graph._registered_edges
            (from_table, from_column, to_table, to_column, label)
);

-- Relationship-type property storage. Same idea as label_properties,
-- only meaningful when the edge has its own row (junction table).
CREATE TABLE graph._registered_rel_properties (
    rel_type     text NOT NULL,
    property     text NOT NULL,
    column_name  text NOT NULL,
    column_type  text NOT NULL,
    required     bool NOT NULL DEFAULT false,
    PRIMARY KEY (rel_type, property),
    FOREIGN KEY (rel_type) REFERENCES graph._registered_rel_types(rel_type)
        ON DELETE CASCADE
);

-- Declared uniqueness — required for MERGE atomicity.
-- Each row is one uniqueness tuple. Multiple rows per label OK
-- (analogous to multiple unique indexes).
CREATE TABLE graph._registered_unique_props (
    kind         text NOT NULL CHECK (kind IN ('label', 'rel_type')),
    name         text NOT NULL,          -- label or rel_type
    props        text[] NOT NULL,        -- ordered tuple of property names
    PRIMARY KEY (kind, name, props)
);

-- Declared multi-label compatibility (which label sets may co-exist
-- on a single node). Empty table = no multi-label nodes allowed.
CREATE TABLE graph._registered_label_sets (
    labels       text[] PRIMARY KEY      -- sorted, distinct
);
```

## New registration SQL functions

```sql
-- Register a Cypher label against a registered table.
SELECT graph.register_label(
    label             := 'Person',
    table_name        := 'public.people',
    discriminator_col := NULL,
    discriminator_val := NULL
);

-- Map a property onto a column. column_type is informational for
-- sema; it does not enforce a cast.
SELECT graph.register_label_property(
    label       := 'Person',
    property    := 'name',
    column_name := 'full_name',
    column_type := 'text',
    required    := true
);

-- Register a Cypher rel type against an already-registered edge.
SELECT graph.register_rel_type(
    rel_type    := 'KNOWS',
    from_table  := 'public.people',
    from_column := 'id',
    to_table    := 'public.people',
    to_column   := 'id',
    label       := 'KNOWS'
);

-- Declare a uniqueness tuple. The function checks that a matching
-- Postgres UNIQUE / PRIMARY KEY constraint actually exists on the
-- underlying table; rejects otherwise.
SELECT graph.register_unique(
    kind  := 'label',
    name  := 'Person',
    props := ARRAY['email']
);

-- Allow specific multi-label compositions.
SELECT graph.allow_label_set(ARRAY['Person', 'Customer']);
```

The check inside `graph.register_unique` is non-negotiable. It is
what lets us issue `INSERT ... ON CONFLICT (cols) DO UPDATE` for
`MERGE` and trust the atomicity guarantee.

## `SchemaProvider` implementation

```rust
// graph/src/cypher_facade/schema_provider.rs (sketch)

pub struct PgGraphSchema {
    snapshot: CatalogSnapshot,   // taken once per cypher() invocation
    digest:   [u8; 32],          // catalog_fingerprint() → digest input
}

impl cyrs_schema::SchemaProvider for PgGraphSchema {
    fn labels(&self) -> Vec<SmolStr> { ... }
    fn relationship_types(&self) -> Vec<SmolStr> { ... }

    fn node_properties(&self, label: &str) -> Option<Vec<PropertyDecl>> {
        // _registered_label_properties → PropertyDecl with PropertyType
        // mapped from column_type via the table below.
    }

    fn relationship_properties(&self, rel_type: &str) -> Option<Vec<PropertyDecl>> { ... }

    fn relationship_endpoints(&self, rel_type: &str) -> Vec<EndpointDecl> {
        // _registered_rel_types FROM/TO labels.
    }

    fn function(&self, name: &str) -> Option<FunctionSignature> {
        // delegate to cyrs_schema::StandardLibrary; pgGraph adds no
        // procs of its own in v1.
        StandardLibrary::default().function(name)
    }

    fn procedure(&self, name: &str) -> Option<ProcedureSignature> {
        None  // no CALL <proc> in v1
    }

    fn schema_digest(&self) -> [u8; 32] { self.digest }

    // From cyrs feat-request §2.2 — implement once cyrs ships them.
    fn label_unique_props(&self, label: &str) -> Vec<Vec<SmolStr>> { ... }
    fn rel_type_unique_props(&self, rel_type: &str) -> Vec<Vec<SmolStr>> { ... }

    // From cyrs feat-request §2.3.
    fn labels_compatible(&self, labels: &[SmolStr]) -> Option<bool> {
        // Look up the sorted-distinct slice in _registered_label_sets.
        // Single-element slice is always compatible.
    }
}
```

## `column_type` → `PropertyType` mapping

| pg type                          | `cyrs_schema::PropertyType` |
| -------------------------------- | --------------------------- |
| `text`, `varchar`, `bpchar`      | `String`                    |
| `int2`, `int4`, `int8`           | `Int`                       |
| `float4`, `float8`, `numeric`    | `Float`                     |
| `bool`                           | `Bool`                      |
| `date`                           | `Date`                      |
| `timestamp`, `timestamptz`       | `Datetime`                  |
| `_text`, `_int4`, …              | `List(...)` recursively     |
| `jsonb`, `json`                  | `Any` (and we'll handle structurally) |
| anything else                    | `Opaque("<pg type>")`       |

If the user wants typed handling of an opaque type, they can either
re-register the column with a closer-fitting type or extend this map
in a later milestone.

## Snapshot consistency

`schema_digest()` must change whenever any of the eight tables above
changes. The implementation:

```rust
fn compute_digest(snapshot: &CatalogSnapshot) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    snapshot.hash_into(&mut hasher);   // deterministic field order
    *hasher.finalize().as_bytes()
}
```

Combine with pgGraph's existing `catalog_fingerprint` (which already
covers the underlying `_registered_tables` / `_registered_edges`) so
a single key invalidates the per-statement-cache when either layer
changes.

## What we leave to a later milestone

- **Inferred label registration** — auto-mapping every registered
  table to a label of the same name. Sugar; out of scope for v1.
- **Computed properties** — labels that expose a `prop` derived from
  a SQL expression rather than a column. Out of scope.
- **Property aliases** — `register_label_property` with two
  `property` rows pointing at the same column. Out of scope.
- **Per-tenant catalog overlays** — pgGraph supports tenant scoping;
  v1 Cypher uses the tenant context set on the session, but the
  label↔table mapping is global. Per-tenant overlays are a v2 ask.
