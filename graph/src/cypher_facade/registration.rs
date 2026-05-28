//! SQL-callable Cypher catalog registration functions.
//!
//! These functions write to the `_registered_labels`,
//! `_registered_label_properties`, `_registered_rel_types`,
//! `_registered_rel_properties`, `_registered_unique_props`, and
//! `_registered_label_sets` tables. They are the public
//! administration surface users invoke to bind Cypher names to
//! pgGraph storage before issuing Cypher queries.
//!
//! Mirrors the auth and panic-boundary pattern from
//! `sql_facade/admin.rs`. See:
//! `docs/contributor_guide/cypher-frontend/020-catalog-extensions.md`.

use pgrx::prelude::*;

use crate::safety::{self, GraphError, GraphResult};

// ──────────────────────────────────────────────────────────────────
// Admin-permission gate (mirrors sql_facade::admin so we don't have
// to widen visibility on the shared helper).
// ──────────────────────────────────────────────────────────────────

fn require_graph_admin_result() -> GraphResult<()> {
    let allowed = Spi::connect(|client| {
        let result = client.select(
            "SELECT
                COALESCE((SELECT rolsuper FROM pg_roles WHERE rolname = current_user), false)
                OR has_schema_privilege(current_user, 'graph', 'CREATE')",
            None,
            &[],
        )?;
        Ok::<_, pgrx::spi::SpiError>(
            result
                .first()
                .get::<bool>(1)
                .ok()
                .flatten()
                .unwrap_or(false),
        )
    })
    .map_err(|e| GraphError::Internal(format!("admin-check SPI failed: {e}")))?;

    if allowed {
        Ok(())
    } else {
        Err(GraphError::AclDenied {
            table: "graph (cypher registration)".to_string(),
        })
    }
}

// ──────────────────────────────────────────────────────────────────
// register_label
// ──────────────────────────────────────────────────────────────────

/// Register a Cypher label against a previously registered table.
///
/// `discriminator_col` / `discriminator_val` are jointly NULL or
/// jointly non-NULL; when set, they select which rows of
/// `table_name` carry this label.
#[pg_extern(schema = "graph")]
fn register_label(
    label: &str,
    table_name: &str,
    discriminator_col: default!(Option<String>, "NULL"),
    discriminator_val: default!(Option<String>, "NULL"),
) {
    let result = (|| -> GraphResult<()> {
        require_graph_admin_result()?;
        validate_label_args(label, &discriminator_col, &discriminator_val)?;
        ensure_table_registered(table_name)?;
        insert_registered_label(
            label,
            table_name,
            discriminator_col.as_deref(),
            discriminator_val.as_deref(),
        )
    })();
    if let Err(err) = result {
        err.report();
    }
}

fn validate_label_args(
    label: &str,
    disc_col: &Option<String>,
    disc_val: &Option<String>,
) -> GraphResult<()> {
    if label.is_empty() {
        return Err(GraphError::Internal("label must not be empty".to_string()));
    }
    if disc_col.is_some() != disc_val.is_some() {
        return Err(GraphError::Internal(
            "discriminator_col and discriminator_val must both be NULL or both be set".to_string(),
        ));
    }
    Ok(())
}

fn ensure_table_registered(table_name: &str) -> GraphResult<()> {
    let found: bool = Spi::connect(|client| {
        let result = client.select(
            "SELECT EXISTS (
                SELECT 1 FROM graph._registered_tables WHERE table_name = $1
            )",
            None,
            &[table_name.into()],
        )?;
        Ok::<_, pgrx::spi::SpiError>(
            result
                .first()
                .get::<bool>(1)
                .ok()
                .flatten()
                .unwrap_or(false),
        )
    })
    .map_err(|e| GraphError::Internal(format!("table-lookup SPI failed: {e}")))?;

    if found {
        Ok(())
    } else {
        Err(GraphError::Internal(format!(
            "table '{table_name}' is not registered with graph; call graph.add_table() first",
        )))
    }
}

fn insert_registered_label(
    label: &str,
    table_name: &str,
    disc_col: Option<&str>,
    disc_val: Option<&str>,
) -> GraphResult<()> {
    Spi::run_with_args(
        "INSERT INTO graph._registered_labels
            (label, table_name, discriminator_col, discriminator_val)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (label) DO UPDATE SET
            table_name = EXCLUDED.table_name,
            discriminator_col = EXCLUDED.discriminator_col,
            discriminator_val = EXCLUDED.discriminator_val",
        &[
            label.into(),
            table_name.into(),
            disc_col.map(str::to_string).into(),
            disc_val.map(str::to_string).into(),
        ],
    )
    .map_err(|e| GraphError::Internal(format!("register_label insert failed: {e}")))
}

// ──────────────────────────────────────────────────────────────────
// register_label_property
// ──────────────────────────────────────────────────────────────────

/// Map a Cypher property name onto a Postgres column on the label's
/// backing table. `column_type` is the textual pg type (e.g. `text`,
/// `int8`, `jsonb`); it informs sema's type lattice but does not
/// itself enforce a cast.
#[pg_extern(schema = "graph")]
fn register_label_property(
    label: &str,
    property: &str,
    column_name: &str,
    column_type: &str,
    required: default!(bool, false),
) {
    let result = (|| -> GraphResult<()> {
        require_graph_admin_result()?;
        if label.is_empty() || property.is_empty() || column_name.is_empty() || column_type.is_empty() {
            return Err(GraphError::Internal(
                "register_label_property: all string args required".to_string(),
            ));
        }
        Spi::run_with_args(
            "INSERT INTO graph._registered_label_properties
                (label, property, column_name, column_type, required)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (label, property) DO UPDATE SET
                column_name = EXCLUDED.column_name,
                column_type = EXCLUDED.column_type,
                required = EXCLUDED.required",
            &[
                label.into(),
                property.into(),
                column_name.into(),
                column_type.into(),
                required.into(),
            ],
        )
        .map_err(|e| GraphError::Internal(format!("register_label_property insert failed: {e}")))
    })();
    if let Err(err) = result {
        err.report();
    }
}

// ──────────────────────────────────────────────────────────────────
// register_rel_type
// ──────────────────────────────────────────────────────────────────

/// Register a Cypher relationship type against an already-registered
/// edge. The five-column foreign key matches `_registered_edges`'s
/// composite key.
#[pg_extern(schema = "graph")]
fn register_rel_type(
    rel_type: &str,
    from_table: &str,
    from_column: &str,
    to_table: &str,
    to_column: &str,
    label: &str,
) {
    let result = (|| -> GraphResult<()> {
        require_graph_admin_result()?;
        if rel_type.is_empty() {
            return Err(GraphError::Internal("rel_type must not be empty".to_string()));
        }
        Spi::run_with_args(
            "INSERT INTO graph._registered_rel_types
                (rel_type, from_table, from_column, to_table, to_column, label)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (rel_type) DO UPDATE SET
                from_table = EXCLUDED.from_table,
                from_column = EXCLUDED.from_column,
                to_table = EXCLUDED.to_table,
                to_column = EXCLUDED.to_column,
                label = EXCLUDED.label",
            &[
                rel_type.into(),
                from_table.into(),
                from_column.into(),
                to_table.into(),
                to_column.into(),
                label.into(),
            ],
        )
        .map_err(|e| GraphError::Internal(format!("register_rel_type insert failed: {e}")))
    })();
    if let Err(err) = result {
        err.report();
    }
}

// ──────────────────────────────────────────────────────────────────
// register_unique
// ──────────────────────────────────────────────────────────────────

/// Declare a uniqueness tuple for MERGE atomicity. `kind` must be
/// `'label'` or `'rel_type'`; `name` is the corresponding label or
/// rel-type name; `props` is the ordered property tuple.
///
/// **This function validates that a matching Postgres `UNIQUE` /
/// `PRIMARY KEY` constraint actually exists on the underlying
/// table.** Without that validation, MERGE's `INSERT ... ON CONFLICT`
/// lowering would not be atomic.
#[pg_extern(schema = "graph")]
fn register_unique(kind: &str, name: &str, props: Vec<String>) {
    let result = (|| -> GraphResult<()> {
        require_graph_admin_result()?;
        if kind != "label" && kind != "rel_type" {
            return Err(GraphError::Internal(format!(
                "register_unique: kind must be 'label' or 'rel_type' (got {kind:?})"
            )));
        }
        if props.is_empty() {
            return Err(GraphError::Internal(
                "register_unique: props must be non-empty".to_string(),
            ));
        }
        validate_pg_unique_exists(kind, name, &props)?;
        Spi::run_with_args(
            "INSERT INTO graph._registered_unique_props (kind, name, props)
             VALUES ($1, $2, $3)
             ON CONFLICT (kind, name, props) DO NOTHING",
            &[kind.into(), name.into(), props.into()],
        )
        .map_err(|e| GraphError::Internal(format!("register_unique insert failed: {e}")))
    })();
    if let Err(err) = result {
        err.report();
    }
}

/// Resolve `kind` + `name` to the underlying table + property→column
/// map, then check that the resulting column tuple is covered by a
/// real Postgres unique constraint.
fn validate_pg_unique_exists(kind: &str, name: &str, props: &[String]) -> GraphResult<()> {
    let (table_name, columns) = if kind == "label" {
        resolve_label_columns(name, props)?
    } else {
        resolve_rel_type_columns(name, props)?
    };

    let columns_array = columns;
    let exists: bool = Spi::connect(|client| {
        // `pg_constraint.conkey` is an int2[] of attribute numbers; we
        // compare against the requested column names via pg_attribute.
        let result = client.select(
            "WITH wanted AS (
                SELECT $2::text[] AS cols
            ),
            attnums AS (
                SELECT array_agg(a.attnum ORDER BY a.attnum) AS k
                FROM pg_attribute a
                WHERE a.attrelid = $1::regclass
                  AND a.attname = ANY((SELECT cols FROM wanted))
            )
            SELECT EXISTS (
                SELECT 1
                FROM pg_constraint c, attnums
                WHERE c.conrelid = $1::regclass
                  AND c.contype IN ('u', 'p')
                  AND c.conkey @> attnums.k
                  AND attnums.k @> c.conkey
            )",
            None,
            &[table_name.clone().into(), columns_array.into()],
        )?;
        Ok::<_, pgrx::spi::SpiError>(
            result
                .first()
                .get::<bool>(1)
                .ok()
                .flatten()
                .unwrap_or(false),
        )
    })
    .map_err(|e| GraphError::Internal(format!("unique-constraint check SPI failed: {e}")))?;

    if exists {
        Ok(())
    } else {
        Err(GraphError::Internal(format!(
            "no UNIQUE / PRIMARY KEY constraint on {} covering columns matching props {:?}; \
             create one before calling graph.register_unique()",
            table_name, props,
        )))
    }
}

fn resolve_label_columns(label: &str, props: &[String]) -> GraphResult<(String, Vec<String>)> {
    let raw: Result<(Option<String>, Vec<Option<String>>), pgrx::spi::SpiError> =
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT table_name FROM graph._registered_labels WHERE label = $1",
                    None,
                    &[label.into()],
                )?
                .first()
                .get::<String>(1)?;
            let mut cols: Vec<Option<String>> = Vec::with_capacity(props.len());
            for p in props {
                let col = client
                    .select(
                        "SELECT column_name FROM graph._registered_label_properties
                         WHERE label = $1 AND property = $2",
                        None,
                        &[label.into(), p.clone().into()],
                    )?
                    .first()
                    .get::<String>(1)?;
                cols.push(col);
            }
            Ok((table, cols))
        });
    let (table, cols) = raw
        .map_err(|e| GraphError::Internal(format!("label-column resolution SPI failed: {e}")))?;

    let Some(table) = table else {
        return Err(GraphError::Internal(format!(
            "label {label:?} not registered"
        )));
    };
    let mut resolved = Vec::with_capacity(cols.len());
    for (prop, col) in props.iter().zip(cols) {
        match col {
            Some(c) => resolved.push(c),
            None => {
                return Err(GraphError::Internal(format!(
                    "property {prop:?} not registered on label {label:?}"
                )));
            }
        }
    }
    Ok((table, resolved))
}

fn resolve_rel_type_columns(rel_type: &str, props: &[String]) -> GraphResult<(String, Vec<String>)> {
    // For rel types we treat the from_table as the carrier of the
    // unique constraint when the edge has its own properties table
    // (junction model). Pure FK edges don't currently support
    // register_unique — MERGE atomicity on those is not in M0 scope.
    let raw: Result<(Option<String>, Vec<Option<String>>), pgrx::spi::SpiError> =
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT from_table FROM graph._registered_rel_types WHERE rel_type = $1",
                    None,
                    &[rel_type.into()],
                )?
                .first()
                .get::<String>(1)?;
            let mut cols: Vec<Option<String>> = Vec::with_capacity(props.len());
            for p in props {
                let col = client
                    .select(
                        "SELECT column_name FROM graph._registered_rel_properties
                         WHERE rel_type = $1 AND property = $2",
                        None,
                        &[rel_type.into(), p.clone().into()],
                    )?
                    .first()
                    .get::<String>(1)?;
                cols.push(col);
            }
            Ok((table, cols))
        });
    let (table, cols) = raw
        .map_err(|e| GraphError::Internal(format!("rel-type-column resolution SPI failed: {e}")))?;

    let Some(table) = table else {
        return Err(GraphError::Internal(format!(
            "rel_type {rel_type:?} not registered"
        )));
    };
    let mut resolved = Vec::with_capacity(cols.len());
    for (prop, col) in props.iter().zip(cols) {
        match col {
            Some(c) => resolved.push(c),
            None => {
                return Err(GraphError::Internal(format!(
                    "property {prop:?} not registered on rel_type {rel_type:?}"
                )));
            }
        }
    }
    Ok((table, resolved))
}

// ──────────────────────────────────────────────────────────────────
// allow_label_set
// ──────────────────────────────────────────────────────────────────

/// Declare a multi-label combination that may co-exist on a single
/// node. The slice is normalised (sorted, deduplicated) before
/// insert. A single-element slice is always permitted and inserting
/// it is a no-op.
#[pg_extern(schema = "graph")]
fn allow_label_set(labels: Vec<String>) {
    let result = (|| -> GraphResult<()> {
        require_graph_admin_result()?;
        if labels.is_empty() {
            return Err(GraphError::Internal(
                "allow_label_set: labels must be non-empty".to_string(),
            ));
        }
        let mut normalised = labels;
        normalised.sort();
        normalised.dedup();
        if normalised.len() == 1 {
            // Single-element sets are always permitted; no need to
            // record them.
            return Ok(());
        }
        Spi::run_with_args(
            "INSERT INTO graph._registered_label_sets (labels)
             VALUES ($1)
             ON CONFLICT (labels) DO NOTHING",
            &[normalised.into()],
        )
        .map_err(|e| GraphError::Internal(format!("allow_label_set insert failed: {e}")))
    })();
    if let Err(err) = result {
        err.report();
    }
}

/// Suppress unused-import lints; `safety` is consumed transitively
/// via re-exports above but the linter sometimes misses the chain.
#[allow(dead_code, reason = "module placeholder; expands in M1")]
fn _touch() -> safety::GraphResult<()> {
    Ok(())
}
