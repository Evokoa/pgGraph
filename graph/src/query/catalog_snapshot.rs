//! Catalog snapshots used by the GQL binder.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::builder::{RegisteredEdge, RegisteredTable};
use crate::catalog::{catalog_fingerprint, read_catalog, table_oid_from_name};
use crate::gql::errors::{GqlError, Span};
use crate::safety::GraphResult;

#[derive(Debug, Clone)]
enum LabelEntry {
    Unique(NodeLabelInfo),
    Ambiguous,
}

/// Bound metadata for a node label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeLabelInfo {
    /// GQL label text.
    pub(crate) label: String,
    /// Source table OID backing this label.
    pub(crate) table_oid: u32,
    /// Registered property column names for later predicate/property phases.
    pub(crate) properties: BTreeSet<String>,
}

/// Bound metadata for a relationship type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelTypeInfo {
    /// GQL relationship type text.
    pub(crate) rel_type: String,
    /// Source node table OID.
    pub(crate) from_table_oid: u32,
    /// Target node table OID.
    pub(crate) to_table_oid: u32,
}

/// Catalog lookup port for semantic binding.
pub(crate) trait CatalogSnapshot {
    /// Resolve a node label to its registered source table.
    fn resolve_node_label(&self, label: &str, span: Span) -> Result<NodeLabelInfo, GqlError>;

    /// Resolve a relationship type between two concrete table OIDs.
    fn resolve_rel_type(
        &self,
        rel_type: &str,
        from_table_oid: u32,
        to_table_oid: u32,
        span: Span,
    ) -> Result<RelTypeInfo, GqlError>;
}

/// SPI-backed catalog snapshot.
#[derive(Debug, Clone)]
pub(crate) struct CatalogSnapshotImpl {
    labels: HashMap<String, LabelEntry>,
    rels: Vec<RelTypeInfo>,
    fingerprint: u64,
}

impl CatalogSnapshotImpl {
    /// Load registered graph catalog rows through SPI.
    ///
    /// # Errors
    ///
    /// Returns [`crate::safety::GraphError`] when catalog reads or relation OID
    /// resolution fail.
    pub(crate) fn load() -> GraphResult<Self> {
        let (tables, edges, filter_columns) = read_catalog()?;
        let fingerprint = catalog_fingerprint(&tables, &edges, &filter_columns);
        let labels = load_labels(&tables)?;
        let rels = load_rels(&tables, &edges)?;
        Ok(Self {
            labels,
            rels,
            fingerprint,
        })
    }

    /// Catalog fingerprint captured when this snapshot was loaded.
    pub(crate) fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

impl CatalogSnapshot for CatalogSnapshotImpl {
    fn resolve_node_label(&self, label: &str, span: Span) -> Result<NodeLabelInfo, GqlError> {
        self.labels
            .get(label)
            .ok_or_else(|| GqlError::bind(span, format!("unknown node label `{label}`")))
            .and_then(|entry| match entry {
                LabelEntry::Unique(info) => Ok(info.clone()),
                LabelEntry::Ambiguous => Err(GqlError::bind(
                    span,
                    format!("ambiguous node label `{label}`"),
                )),
            })
    }

    fn resolve_rel_type(
        &self,
        rel_type: &str,
        from_table_oid: u32,
        to_table_oid: u32,
        span: Span,
    ) -> Result<RelTypeInfo, GqlError> {
        self.rels
            .iter()
            .find(|rel| {
                rel.rel_type == rel_type
                    && rel.from_table_oid == from_table_oid
                    && rel.to_table_oid == to_table_oid
            })
            .cloned()
            .ok_or_else(|| {
                GqlError::bind(
                    span,
                    format!(
                        "unknown relationship type `{rel_type}` from table {from_table_oid} to {to_table_oid}"
                    ),
                )
            })
    }
}

fn load_labels(tables: &[RegisteredTable]) -> GraphResult<HashMap<String, LabelEntry>> {
    let mut labels = HashMap::with_capacity(tables.len());
    for table in tables {
        let table_oid = table_oid_from_name(&table.table_name)?;
        if let Some(label) = gql_label_from_regclass(&table.table_name) {
            let mut properties = table.columns.iter().cloned().collect::<BTreeSet<_>>();
            properties.extend(table.id_columns.columns().iter().cloned());
            let info = NodeLabelInfo {
                label: label.clone(),
                table_oid,
                properties,
            };
            labels
                .entry(label)
                .and_modify(|entry| *entry = LabelEntry::Ambiguous)
                .or_insert(LabelEntry::Unique(info));
        }
    }
    Ok(labels)
}

fn load_rels(
    tables: &[RegisteredTable],
    edges: &[RegisteredEdge],
) -> GraphResult<Vec<RelTypeInfo>> {
    let registered_tables = tables
        .iter()
        .map(|table| table.table_name.as_str())
        .collect::<HashSet<_>>();
    let mut registered_table_oids = HashMap::with_capacity(tables.len());
    for table in tables {
        registered_table_oids.insert(
            table_oid_from_name(&table.table_name)?,
            table.table_name.as_str(),
        );
    }
    let mut rels = Vec::with_capacity(edges.len());
    for edge in edges {
        let from_node_table = if registered_tables.contains(edge.from_table.as_str()) {
            Some(edge.from_table.as_str())
        } else {
            edge_source_fk_table_oid(edge)?.and_then(|oid| registered_table_oids.get(&oid).copied())
        };
        let Some(from_node_table) = from_node_table else {
            continue;
        };
        rels.push(RelTypeInfo {
            rel_type: edge.label.clone(),
            from_table_oid: table_oid_from_name(from_node_table)?,
            to_table_oid: table_oid_from_name(&edge.to_table)?,
        });
        if edge.bidirectional {
            rels.push(RelTypeInfo {
                rel_type: edge.label.clone(),
                from_table_oid: table_oid_from_name(&edge.to_table)?,
                to_table_oid: table_oid_from_name(from_node_table)?,
            });
        }
    }
    Ok(rels)
}

fn edge_source_fk_table_oid(edge: &RegisteredEdge) -> GraphResult<Option<u32>> {
    let from_table_oid = table_oid_from_name(&edge.from_table)?;
    pgrx::Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT c.confrelid::oid::integer
                 FROM pg_constraint c
                 JOIN unnest(c.conkey) WITH ORDINALITY AS fk_from(attnum, n) ON true
                 JOIN pg_attribute from_attr
                   ON from_attr.attrelid = c.conrelid
                  AND from_attr.attnum = fk_from.attnum
                 WHERE c.contype = 'f'
                   AND c.conrelid = $1::oid
                   AND from_attr.attname = $2
                 ORDER BY c.oid
                 LIMIT 2",
                None,
                &[
                    pgrx::pg_sys::Oid::from_u32(from_table_oid).into(),
                    edge.from_column.clone().into(),
                ],
            )
            .map_err(|err| {
                crate::safety::GraphError::Internal(format!(
                    "edge source foreign-key lookup failed: {err}"
                ))
            })?;
        if rows.len() != 1 {
            return Ok(None);
        }
        rows.first()
            .get::<i32>(1)
            .map_err(|err| {
                crate::safety::GraphError::Internal(format!(
                    "edge source foreign-key target read failed: {err}"
                ))
            })
            .map(|oid| oid.map(|oid| oid as u32))
    })
}

fn gql_label_from_regclass(regclass: &str) -> Option<String> {
    let label = regclass.rsplit('.').next()?;
    let first = label.bytes().next()?;
    if label.is_empty()
        || label.starts_with('"')
        || !(first == b'_' || first.is_ascii_alphabetic())
        || !label
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(label.to_string())
}

/// In-memory catalog used by binder unit tests.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeCatalog {
    labels: HashMap<String, NodeLabelInfo>,
    rels: Vec<RelTypeInfo>,
}

#[cfg(test)]
impl FakeCatalog {
    /// Create an empty fake catalog.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add a node label backed by `table_oid`.
    pub(crate) fn with_label(
        mut self,
        label: &str,
        table_oid: u32,
        properties: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        self.labels.insert(
            label.to_string(),
            NodeLabelInfo {
                label: label.to_string(),
                table_oid,
                properties: properties
                    .into_iter()
                    .map(|property| property.as_ref().to_string())
                    .collect(),
            },
        );
        self
    }

    /// Add a directed relationship type between concrete source/target tables.
    pub(crate) fn with_edge(
        mut self,
        rel_type: &str,
        from_table_oid: u32,
        to_table_oid: u32,
    ) -> Self {
        self.rels.push(RelTypeInfo {
            rel_type: rel_type.to_string(),
            from_table_oid,
            to_table_oid,
        });
        self
    }
}

#[cfg(test)]
impl CatalogSnapshot for FakeCatalog {
    fn resolve_node_label(&self, label: &str, span: Span) -> Result<NodeLabelInfo, GqlError> {
        self.labels
            .get(label)
            .cloned()
            .ok_or_else(|| GqlError::bind(span, format!("unknown node label `{label}`")))
    }

    fn resolve_rel_type(
        &self,
        rel_type: &str,
        from_table_oid: u32,
        to_table_oid: u32,
        span: Span,
    ) -> Result<RelTypeInfo, GqlError> {
        self.rels
            .iter()
            .find(|rel| {
                rel.rel_type == rel_type
                    && rel.from_table_oid == from_table_oid
                    && rel.to_table_oid == to_table_oid
            })
            .cloned()
            .ok_or_else(|| GqlError::bind(span, format!("unknown relationship type `{rel_type}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::gql_label_from_regclass;

    #[test]
    fn gql_label_from_regclass_accepts_only_simple_unquoted_identifiers() {
        assert_eq!(gql_label_from_regclass("users").as_deref(), Some("users"));
        assert_eq!(
            gql_label_from_regclass("tenant_a.users").as_deref(),
            Some("users")
        );
        assert_eq!(gql_label_from_regclass("\"MixedCase\""), None);
        assert_eq!(
            gql_label_from_regclass("tenant-a.users").as_deref(),
            Some("users")
        );
        assert_eq!(gql_label_from_regclass("123users"), None);
    }
}
