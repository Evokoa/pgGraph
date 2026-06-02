//! Catalog snapshots used by the GQL binder.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::builder::{RegisteredEdge, RegisteredTable};
use crate::catalog::{read_catalog, sql_table_name_from_catalog, table_oid_from_name};
use crate::gql::errors::{GqlError, Span};
use crate::quote::quote_ident;
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
    /// Registered non-key property columns that writes may update.
    pub(crate) writable_properties: BTreeSet<String>,
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
    /// Registered edge-row mapping when this relationship is backed by a
    /// separate edge table.
    pub(crate) edge_mapping: Option<EdgeMappingInfo>,
}

/// Source-table details required for mapped edge writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeMappingInfo {
    /// Registered edge row table OID.
    pub(crate) edge_table_oid: u32,
    /// Registered source node table OID.
    pub(crate) source_table_oid: u32,
    /// Registered target node table OID.
    pub(crate) target_table_oid: u32,
    /// Edge row column containing the source node key.
    pub(crate) source_column: String,
    /// Edge row column containing the target node key.
    pub(crate) target_column: String,
    /// Whether the edge was registered as bidirectional.
    pub(crate) bidirectional: bool,
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

    /// Return registered relationships incident to the table OID.
    fn incident_rel_types(&self, table_oid: u32) -> Vec<RelTypeInfo>;

    /// Return all registered node labels visible to wildcard binding.
    fn node_labels(&self) -> Vec<NodeLabelInfo>;

    /// Return all registered relationship types visible to wildcard binding.
    fn rel_types(&self) -> Vec<RelTypeInfo>;
}

/// SPI-backed catalog snapshot.
#[derive(Debug, Clone)]
pub(crate) struct CatalogSnapshotImpl {
    labels: HashMap<String, LabelEntry>,
    rels: Vec<RelTypeInfo>,
}

impl CatalogSnapshotImpl {
    /// Load registered graph catalog rows through SPI.
    ///
    /// # Errors
    ///
    /// Returns [`crate::safety::GraphError`] when catalog reads or relation OID
    /// resolution fail.
    pub(crate) fn load() -> GraphResult<Self> {
        let (tables, edges, _filter_columns) = read_catalog()?;
        let labels = load_labels(&tables)?;
        let rels = load_rels(&tables, &edges)?;
        Ok(Self { labels, rels })
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

    fn incident_rel_types(&self, table_oid: u32) -> Vec<RelTypeInfo> {
        incident_rel_types(&self.rels, table_oid)
    }

    fn node_labels(&self) -> Vec<NodeLabelInfo> {
        node_labels(&self.labels)
    }

    fn rel_types(&self) -> Vec<RelTypeInfo> {
        self.rels.clone()
    }
}

fn node_labels(labels: &HashMap<String, LabelEntry>) -> Vec<NodeLabelInfo> {
    labels
        .values()
        .filter_map(|entry| match entry {
            LabelEntry::Unique(info) => Some(info.clone()),
            LabelEntry::Ambiguous => None,
        })
        .collect()
}

fn incident_rel_types(rels: &[RelTypeInfo], table_oid: u32) -> Vec<RelTypeInfo> {
    let mut seen = HashSet::new();
    rels.iter()
        .filter(|rel| rel.from_table_oid == table_oid || rel.to_table_oid == table_oid)
        .filter(|rel| {
            seen.insert((
                rel.rel_type.clone(),
                rel.from_table_oid,
                rel.to_table_oid,
                rel.edge_mapping
                    .as_ref()
                    .map(|edge| edge.edge_table_oid)
                    .unwrap_or_default(),
            ))
        })
        .cloned()
        .collect()
}

fn load_labels(tables: &[RegisteredTable]) -> GraphResult<HashMap<String, LabelEntry>> {
    let mut labels = HashMap::with_capacity(tables.len());
    for table in tables {
        let table_oid = table_oid_from_name(&table.table_name)?;
        if let Some(label) = gql_label_from_regclass(&table.table_name) {
            let mut properties = table.columns.iter().cloned().collect::<BTreeSet<_>>();
            properties.extend(table.id_columns.columns().iter().cloned());
            let writable_properties = table
                .columns
                .iter()
                .filter(|column| table.tenant_column.as_deref() != Some(column.as_str()))
                .cloned()
                .collect::<BTreeSet<_>>();
            let info = NodeLabelInfo {
                label: label.clone(),
                table_oid,
                properties,
                writable_properties,
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
        let (from_node_table, edge_mapping) =
            if registered_tables.contains(edge.from_table.as_str()) {
                (Some(edge.from_table.as_str()), None)
            } else {
                let edge_table_oid = table_oid_from_name(&edge.from_table)?;
                let source_table_oid = edge_source_fk_table_oid(edge)?;
                let from_node_table =
                    source_table_oid.and_then(|oid| registered_table_oids.get(&oid).copied());
                let target_table_oid = table_oid_from_name(&edge.to_table)?;
                let edge_mapping = source_table_oid.map(|source_table_oid| EdgeMappingInfo {
                    edge_table_oid,
                    source_table_oid,
                    target_table_oid,
                    source_column: edge.from_column.clone(),
                    target_column: edge.to_column.clone(),
                    bidirectional: edge.bidirectional,
                });
                (from_node_table, edge_mapping)
            };
        let Some(from_node_table) = from_node_table else {
            continue;
        };
        let source_table_oid = table_oid_from_name(from_node_table)?;
        let target_table_oid = table_oid_from_name(&edge.to_table)?;
        for rel_type in relationship_type_names(edge)? {
            rels.push(RelTypeInfo {
                rel_type: rel_type.clone(),
                from_table_oid: source_table_oid,
                to_table_oid: target_table_oid,
                edge_mapping: edge_mapping.clone(),
            });
            if edge.bidirectional {
                rels.push(RelTypeInfo {
                    rel_type,
                    from_table_oid: target_table_oid,
                    to_table_oid: source_table_oid,
                    edge_mapping: edge_mapping.clone(),
                });
            }
        }
    }
    Ok(rels)
}

fn relationship_type_names(edge: &RegisteredEdge) -> GraphResult<Vec<String>> {
    let Some(label_column) = edge.label_column.as_deref() else {
        return Ok(vec![edge.label.clone()]);
    };
    let edge_table = sql_table_name_from_catalog(&edge.from_table)?;
    let label_expr = quote_ident(label_column);
    let query = format!(
        "SELECT DISTINCT COALESCE(NULLIF(BTRIM({label_expr}::text), ''), $1)
         FROM {}
         ORDER BY 1",
        edge_table.as_sql()
    );
    pgrx::Spi::connect(|client| {
        let rows = client
            .select(&query, None, &[edge.label.clone().into()])
            .map_err(|err| {
                crate::safety::GraphError::Internal(format!(
                    "dynamic relationship label read failed for {}.{}: {err}",
                    edge.from_table, label_column
                ))
            })?;
        let mut labels = Vec::with_capacity(rows.len());
        for row in rows {
            let label = row.get::<String>(1).map_err(|err| {
                crate::safety::GraphError::Internal(format!(
                    "dynamic relationship label value read failed: {err}"
                ))
            })?;
            if let Some(label) = label {
                if gql_identifier_from_text(&label).is_some() {
                    labels.push(label);
                }
            }
        }
        if labels.is_empty() && gql_identifier_from_text(&edge.label).is_some() {
            labels.push(edge.label.clone());
        }
        Ok(labels)
    })
}

fn gql_identifier_from_text(text: &str) -> Option<String> {
    let first = text.bytes().next()?;
    if text.is_empty()
        || text.starts_with('"')
        || !(first == b'_' || first.is_ascii_alphabetic())
        || !text
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(text.to_string())
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
    gql_identifier_from_text(label)
}

/// In-memory catalog used by binder unit tests.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeCatalog {
    labels: HashMap<String, NodeLabelInfo>,
    rels: Vec<RelTypeInfo>,
}

/// Test-only relationship mapping specification.
#[cfg(test)]
pub(crate) struct MappedEdgeSpec<'a> {
    /// Relationship type name.
    pub(crate) rel_type: &'a str,
    /// Source node table OID.
    pub(crate) from_table_oid: u32,
    /// Target node table OID.
    pub(crate) to_table_oid: u32,
    /// Edge row table OID.
    pub(crate) edge_table_oid: u32,
    /// Source-key column in the edge row table.
    pub(crate) source_column: &'a str,
    /// Target-key column in the edge row table.
    pub(crate) target_column: &'a str,
    /// Whether the registration is bidirectional.
    pub(crate) bidirectional: bool,
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
                writable_properties: BTreeSet::new(),
            },
        );
        self
    }

    /// Add a node label with distinct read and write property sets.
    pub(crate) fn with_writable_label(
        mut self,
        label: &str,
        table_oid: u32,
        properties: impl IntoIterator<Item = impl AsRef<str>>,
        writable_properties: impl IntoIterator<Item = impl AsRef<str>>,
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
                writable_properties: writable_properties
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
            edge_mapping: None,
        });
        self
    }

    /// Add a directed relationship type backed by a mapped edge row table.
    pub(crate) fn with_mapped_edge(mut self, spec: MappedEdgeSpec<'_>) -> Self {
        self.rels.push(RelTypeInfo {
            rel_type: spec.rel_type.to_string(),
            from_table_oid: spec.from_table_oid,
            to_table_oid: spec.to_table_oid,
            edge_mapping: Some(EdgeMappingInfo {
                edge_table_oid: spec.edge_table_oid,
                source_table_oid: spec.from_table_oid,
                target_table_oid: spec.to_table_oid,
                source_column: spec.source_column.to_string(),
                target_column: spec.target_column.to_string(),
                bidirectional: spec.bidirectional,
            }),
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

    fn incident_rel_types(&self, table_oid: u32) -> Vec<RelTypeInfo> {
        incident_rel_types(&self.rels, table_oid)
    }

    fn node_labels(&self) -> Vec<NodeLabelInfo> {
        self.labels.values().cloned().collect()
    }

    fn rel_types(&self) -> Vec<RelTypeInfo> {
        self.rels.clone()
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
