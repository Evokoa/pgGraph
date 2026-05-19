//! `cyrs_schema::SchemaProvider` implementation backed by the
//! pgGraph catalog (`_registered_labels`, `_registered_rel_types`,
//! and friends).
//!
//! Each `graph.cypher(...)` invocation snapshots the catalog once and
//! constructs a `PgGraphSchema` over the snapshot. The snapshot is
//! immutable for the duration of one query, which matches cyrs's
//! Salsa expectation that `schema_digest()` is stable across calls
//! within a single check (spec 0001 §11.2).
//!
//! Methods that depend on cyrs feature-request items
//! (`label_unique_props`, `rel_type_unique_props`, `labels_compatible`)
//! are written but rely on the trait defaults until upstream cyrs
//! ships them — see
//! `docs/contributor_guide/cypher-frontend/080-open-questions.md`.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use pgrx::prelude::*;
use smol_str::SmolStr;

use cyrs_schema::{
    EndpointDecl, FunctionSignature, ProcedureSignature, PropertyDecl, PropertyType,
    SchemaProvider, StandardLibrary,
};

use crate::safety::{GraphError, GraphResult};

/// Immutable in-memory view of the Cypher catalog at the moment a
/// query begins. Built via [`PgGraphSchema::snapshot`].
#[derive(Debug)]
pub(crate) struct PgGraphSchema {
    labels: BTreeMap<SmolStr, LabelEntry>,
    rel_types: BTreeMap<SmolStr, RelTypeEntry>,
    unique_props_by_label: BTreeMap<SmolStr, Vec<Vec<SmolStr>>>,
    unique_props_by_rel_type: BTreeMap<SmolStr, Vec<Vec<SmolStr>>>,
    label_sets: Vec<Vec<SmolStr>>,
    digest: [u8; 32],
}

#[derive(Debug, Clone)]
struct LabelEntry {
    #[allow(dead_code, reason = "consumed in M1 read-side translation")]
    table_name: String,
    #[allow(dead_code, reason = "consumed in M1 read-side translation")]
    discriminator: Option<(String, String)>,
    properties: IndexMap<SmolStr, PropertyDecl>,
}

#[derive(Debug, Clone)]
struct RelTypeEntry {
    #[allow(dead_code, reason = "consumed in M1/M2 read-side translation")]
    from_table: String,
    #[allow(dead_code, reason = "consumed in M1/M2 read-side translation")]
    from_column: String,
    #[allow(dead_code, reason = "consumed in M1/M2 read-side translation")]
    to_table: String,
    #[allow(dead_code, reason = "consumed in M1/M2 read-side translation")]
    to_column: String,
    #[allow(dead_code, reason = "consumed in M1/M2 read-side translation")]
    edge_label: String,
    properties: IndexMap<SmolStr, PropertyDecl>,
}

impl PgGraphSchema {
    /// Read the Cypher catalog via SPI and build a snapshot.
    pub(crate) fn snapshot() -> GraphResult<Self> {
        let labels = load_labels()?;
        let rel_types = load_rel_types()?;
        let (unique_props_by_label, unique_props_by_rel_type) = load_unique_props()?;
        let label_sets = load_label_sets()?;
        let digest = compute_digest(&labels, &rel_types, &unique_props_by_label,
                                    &unique_props_by_rel_type, &label_sets);
        Ok(Self {
            labels,
            rel_types,
            unique_props_by_label,
            unique_props_by_rel_type,
            label_sets,
            digest,
        })
    }
}

// ──────────────────────────────────────────────────────────────────
// SchemaProvider impl
// ──────────────────────────────────────────────────────────────────

impl SchemaProvider for PgGraphSchema {
    fn labels(&self) -> Vec<SmolStr> {
        self.labels.keys().cloned().collect()
    }

    fn relationship_types(&self) -> Vec<SmolStr> {
        self.rel_types.keys().cloned().collect()
    }

    fn node_properties(&self, label: &str) -> Option<Vec<PropertyDecl>> {
        self.labels
            .get(label)
            .map(|entry| entry.properties.values().cloned().collect())
    }

    fn relationship_properties(&self, rel_type: &str) -> Option<Vec<PropertyDecl>> {
        self.rel_types
            .get(rel_type)
            .map(|entry| entry.properties.values().cloned().collect())
    }

    fn relationship_endpoints(&self, rel_type: &str) -> Vec<EndpointDecl> {
        // M0: we record only the from/to table names, not the labels
        // pgGraph would associate with those tables. The proper join
        // through `_registered_labels` lands in M1 once the read path
        // needs it.
        let Some(_entry) = self.rel_types.get(rel_type) else {
            return Vec::new();
        };
        Vec::new()
    }

    fn inverse_of(&self, _rel_type: &str) -> Option<SmolStr> {
        None
    }

    fn function(&self, name: &str) -> Option<FunctionSignature> {
        // The stdlib catalog is `static` inside cyrs-schema; this
        // construction is effectively free.
        StandardLibrary::default().function(name)
    }

    fn procedure(&self, _name: &str) -> Option<ProcedureSignature> {
        None
    }

    fn schema_digest(&self) -> [u8; 32] {
        self.digest
    }
}

// ──────────────────────────────────────────────────────────────────
// Catalog readers
// ──────────────────────────────────────────────────────────────────

fn load_labels() -> GraphResult<BTreeMap<SmolStr, LabelEntry>> {
    let mut labels: BTreeMap<SmolStr, LabelEntry> = BTreeMap::new();

    Spi::connect(|client| {
        let rows = client.select(
            "SELECT label, table_name, discriminator_col, discriminator_val
             FROM graph._registered_labels",
            None,
            &[],
        )?;
        for row in rows {
            let label: String = row.get::<String>(1)?.unwrap_or_default();
            let table_name: String = row.get::<String>(2)?.unwrap_or_default();
            let disc_col: Option<String> = row.get::<String>(3)?;
            let disc_val: Option<String> = row.get::<String>(4)?;
            let discriminator = match (disc_col, disc_val) {
                (Some(c), Some(v)) => Some((c, v)),
                _ => None,
            };
            labels.insert(
                SmolStr::from(label),
                LabelEntry {
                    table_name,
                    discriminator,
                    properties: IndexMap::new(),
                },
            );
        }
        Ok::<(), pgrx::spi::SpiError>(())
    })
    .map_err(|e| GraphError::Internal(format!("load_labels SPI failed: {e}")))?;

    Spi::connect(|client| {
        let rows = client.select(
            "SELECT label, property, column_name, column_type, required
             FROM graph._registered_label_properties",
            None,
            &[],
        )?;
        for row in rows {
            let label: String = row.get::<String>(1)?.unwrap_or_default();
            let property: String = row.get::<String>(2)?.unwrap_or_default();
            let _column: String = row.get::<String>(3)?.unwrap_or_default();
            let column_type: String = row.get::<String>(4)?.unwrap_or_default();
            let required: bool = row.get::<bool>(5)?.unwrap_or(false);
            if let Some(entry) = labels.get_mut(label.as_str()) {
                let ty = pg_type_to_property_type(&column_type);
                entry
                    .properties
                    .insert(SmolStr::from(property.clone()),
                            PropertyDecl::new(property, ty, required));
            }
        }
        Ok::<(), pgrx::spi::SpiError>(())
    })
    .map_err(|e| GraphError::Internal(format!("load_label_properties SPI failed: {e}")))?;

    Ok(labels)
}

fn load_rel_types() -> GraphResult<BTreeMap<SmolStr, RelTypeEntry>> {
    let mut rel_types: BTreeMap<SmolStr, RelTypeEntry> = BTreeMap::new();

    Spi::connect(|client| {
        let rows = client.select(
            "SELECT rel_type, from_table, from_column, to_table, to_column, label
             FROM graph._registered_rel_types",
            None,
            &[],
        )?;
        for row in rows {
            let rel_type: String = row.get::<String>(1)?.unwrap_or_default();
            let from_table: String = row.get::<String>(2)?.unwrap_or_default();
            let from_column: String = row.get::<String>(3)?.unwrap_or_default();
            let to_table: String = row.get::<String>(4)?.unwrap_or_default();
            let to_column: String = row.get::<String>(5)?.unwrap_or_default();
            let edge_label: String = row.get::<String>(6)?.unwrap_or_default();
            rel_types.insert(
                SmolStr::from(rel_type),
                RelTypeEntry {
                    from_table,
                    from_column,
                    to_table,
                    to_column,
                    edge_label,
                    properties: IndexMap::new(),
                },
            );
        }
        Ok::<(), pgrx::spi::SpiError>(())
    })
    .map_err(|e| GraphError::Internal(format!("load_rel_types SPI failed: {e}")))?;

    Spi::connect(|client| {
        let rows = client.select(
            "SELECT rel_type, property, column_name, column_type, required
             FROM graph._registered_rel_properties",
            None,
            &[],
        )?;
        for row in rows {
            let rel_type: String = row.get::<String>(1)?.unwrap_or_default();
            let property: String = row.get::<String>(2)?.unwrap_or_default();
            let _column: String = row.get::<String>(3)?.unwrap_or_default();
            let column_type: String = row.get::<String>(4)?.unwrap_or_default();
            let required: bool = row.get::<bool>(5)?.unwrap_or(false);
            if let Some(entry) = rel_types.get_mut(rel_type.as_str()) {
                let ty = pg_type_to_property_type(&column_type);
                entry.properties.insert(
                    SmolStr::from(property.clone()),
                    PropertyDecl::new(property, ty, required),
                );
            }
        }
        Ok::<(), pgrx::spi::SpiError>(())
    })
    .map_err(|e| GraphError::Internal(format!("load_rel_properties SPI failed: {e}")))?;

    Ok(rel_types)
}

#[allow(clippy::type_complexity, reason = "two parallel maps returned together")]
fn load_unique_props() -> GraphResult<(
    BTreeMap<SmolStr, Vec<Vec<SmolStr>>>,
    BTreeMap<SmolStr, Vec<Vec<SmolStr>>>,
)> {
    let mut by_label: BTreeMap<SmolStr, Vec<Vec<SmolStr>>> = BTreeMap::new();
    let mut by_rel: BTreeMap<SmolStr, Vec<Vec<SmolStr>>> = BTreeMap::new();
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT kind, name, props FROM graph._registered_unique_props",
            None,
            &[],
        )?;
        for row in rows {
            let kind: String = row.get::<String>(1)?.unwrap_or_default();
            let name: String = row.get::<String>(2)?.unwrap_or_default();
            let props: Vec<String> = row.get::<Vec<String>>(3)?.unwrap_or_default();
            let key = SmolStr::from(name);
            let tuple: Vec<SmolStr> = props.into_iter().map(SmolStr::from).collect();
            let target = if kind == "label" {
                by_label.entry(key).or_default()
            } else {
                by_rel.entry(key).or_default()
            };
            target.push(tuple);
        }
        Ok::<(), pgrx::spi::SpiError>(())
    })
    .map_err(|e| GraphError::Internal(format!("load_unique_props SPI failed: {e}")))?;
    Ok((by_label, by_rel))
}

fn load_label_sets() -> GraphResult<Vec<Vec<SmolStr>>> {
    let mut out: Vec<Vec<SmolStr>> = Vec::new();
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT labels FROM graph._registered_label_sets",
            None,
            &[],
        )?;
        for row in rows {
            let labels: Vec<String> = row.get::<Vec<String>>(1)?.unwrap_or_default();
            out.push(labels.into_iter().map(SmolStr::from).collect());
        }
        Ok::<(), pgrx::spi::SpiError>(())
    })
    .map_err(|e| GraphError::Internal(format!("load_label_sets SPI failed: {e}")))?;
    Ok(out)
}

// ──────────────────────────────────────────────────────────────────
// Type bridge (see 020-catalog-extensions.md)
// ──────────────────────────────────────────────────────────────────

fn pg_type_to_property_type(pg_type: &str) -> PropertyType {
    let normalised = pg_type.trim().to_lowercase();
    match normalised.as_str() {
        "text" | "varchar" | "bpchar" | "char" | "name" => PropertyType::String,
        "int2" | "int4" | "int8" | "smallint" | "integer" | "bigint" => PropertyType::Int,
        "float4" | "float8" | "numeric" | "real" | "double precision" => PropertyType::Float,
        "bool" | "boolean" => PropertyType::Bool,
        "date" => PropertyType::Date,
        "timestamp" | "timestamptz" | "timestamp with time zone"
            | "timestamp without time zone" => PropertyType::Datetime,
        // Array forms come back as either `_text` (internal) or `text[]`.
        other if other.starts_with('_') => {
            PropertyType::List(Box::new(pg_type_to_property_type(&other[1..])))
        }
        other if other.ends_with("[]") => {
            let inner = &other[..other.len() - 2];
            PropertyType::List(Box::new(pg_type_to_property_type(inner)))
        }
        "jsonb" | "json" => PropertyType::Any,
        other => PropertyType::Opaque(SmolStr::from(other)),
    }
}

// ──────────────────────────────────────────────────────────────────
// Digest
// ──────────────────────────────────────────────────────────────────

fn compute_digest(
    labels: &BTreeMap<SmolStr, LabelEntry>,
    rel_types: &BTreeMap<SmolStr, RelTypeEntry>,
    unique_by_label: &BTreeMap<SmolStr, Vec<Vec<SmolStr>>>,
    unique_by_rel: &BTreeMap<SmolStr, Vec<Vec<SmolStr>>>,
    label_sets: &[Vec<SmolStr>],
) -> [u8; 32] {
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    // Each section is prefixed by a tag byte so re-orderings between
    // sections can't collide. BTreeMap iteration is sorted by key.
    hasher.update(&[0x01]);
    for (label, entry) in labels {
        hasher.update(label.as_bytes());
        hasher.update(entry.table_name.as_bytes());
        if let Some((c, v)) = &entry.discriminator {
            hasher.update(c.as_bytes());
            hasher.update(v.as_bytes());
        }
        for (prop, decl) in &entry.properties {
            hasher.update(prop.as_bytes());
            hasher.update(&[u8::from(decl.required)]);
        }
    }
    hasher.update(&[0x02]);
    for (rel, entry) in rel_types {
        hasher.update(rel.as_bytes());
        hasher.update(entry.from_table.as_bytes());
        hasher.update(entry.from_column.as_bytes());
        hasher.update(entry.to_table.as_bytes());
        hasher.update(entry.to_column.as_bytes());
        hasher.update(entry.edge_label.as_bytes());
        for (prop, decl) in &entry.properties {
            hasher.update(prop.as_bytes());
            hasher.update(&[u8::from(decl.required)]);
        }
    }
    hasher.update(&[0x03]);
    for (key, tuples) in unique_by_label {
        hasher.update(key.as_bytes());
        for tuple in tuples {
            for p in tuple {
                hasher.update(p.as_bytes());
                hasher.update(b",");
            }
            hasher.update(b";");
        }
    }
    hasher.update(&[0x04]);
    for (key, tuples) in unique_by_rel {
        hasher.update(key.as_bytes());
        for tuple in tuples {
            for p in tuple {
                hasher.update(p.as_bytes());
                hasher.update(b",");
            }
            hasher.update(b";");
        }
    }
    hasher.update(&[0x05]);
    for set in label_sets {
        for label in set {
            hasher.update(label.as_bytes());
            hasher.update(b",");
        }
        hasher.update(b";");
    }
    // xxh3 is a 128-bit hash; we widen to 32 bytes by hashing twice
    // with different seeds. Adequate for an invalidation key; this
    // is not a cryptographic digest.
    let lo = hasher.digest128().to_le_bytes();
    let mut second = xxhash_rust::xxh3::Xxh3::with_seed(0xA5A5_5A5A);
    second.update(&lo);
    let hi = second.digest128().to_le_bytes();
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&lo);
    out[16..].copy_from_slice(&hi);
    out
}

// ──────────────────────────────────────────────────────────────────
// Accessors for future milestones (M1+ translator)
// ──────────────────────────────────────────────────────────────────

impl PgGraphSchema {
    /// Whether a multi-label combination is permitted. `None` = not
    /// declared in the catalog (we treat as "rejected" at execution
    /// time — see open question Q-IN-2 in 080-open-questions.md).
    #[allow(dead_code, reason = "consumed in M3 write-side translation")]
    pub(crate) fn labels_compatible_check(&self, labels: &[SmolStr]) -> Option<bool> {
        if labels.len() <= 1 {
            return Some(true);
        }
        let mut normalised: Vec<SmolStr> = labels.to_vec();
        normalised.sort();
        normalised.dedup();
        Some(self.label_sets.iter().any(|s| s == &normalised))
    }

    /// Declared unique-property tuples for a label. Used in M4 to
    /// validate MERGE pattern determinism against real Postgres
    /// uniqueness constraints.
    #[allow(dead_code, reason = "consumed in M4 MERGE lowering")]
    pub(crate) fn label_unique_props(&self, label: &str) -> Vec<Vec<SmolStr>> {
        self.unique_props_by_label
            .get(label)
            .cloned()
            .unwrap_or_default()
    }

    /// Declared unique-property tuples for a relationship type.
    #[allow(dead_code, reason = "consumed in M4 MERGE lowering")]
    pub(crate) fn rel_type_unique_props(&self, rel_type: &str) -> Vec<Vec<SmolStr>> {
        self.unique_props_by_rel_type
            .get(rel_type)
            .cloned()
            .unwrap_or_default()
    }
}
