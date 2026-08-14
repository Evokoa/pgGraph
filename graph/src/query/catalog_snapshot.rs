//! Catalog snapshots used by the GQL binder.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::builder::{PrimaryKeySpec, RegisteredEdge, RegisteredTable};
use crate::catalog::{foreign_key_target_table_oid, read_catalog};
use crate::gql::errors::{GqlError, Span};
use crate::safety::GraphError;
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
    /// Registered primary-key columns in catalog order.
    pub(crate) primary_key_columns: Vec<String>,
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
    /// Durable source-row mapping used to identify and hydrate relationships.
    /// Node-backed foreign-key relationships carry read metadata here but are
    /// not writable as standalone edge rows.
    pub(crate) edge_mapping: Option<EdgeMappingInfo>,
}

/// Source-table details required to identify a mapped relationship row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeMappingInfo {
    /// Durable catalog identity for this relationship mapping.
    pub(crate) mapping_id: u64,
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
    /// Primary-key columns that identify one edge source row.
    pub(crate) source_key_columns: PrimaryKeySpec,
    /// Whether the edge was registered as bidirectional.
    pub(crate) bidirectional: bool,
    /// Dynamic relationship label column, when the edge registration uses one.
    pub(crate) label_column: Option<String>,
}

impl EdgeMappingInfo {
    /// Return whether the relationship is stored in a separate edge-row table.
    ///
    /// Node-backed foreign-key mappings use the source node table as their
    /// relationship table. They are readable through this metadata, but graph
    /// writes would need to update an existing source row rather than insert or
    /// delete a standalone edge row.
    pub(crate) fn has_standalone_edge_row(&self) -> bool {
        self.edge_table_oid != self.source_table_oid
    }
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

    /// Return whether any GQL node label maps ambiguously to multiple tables.
    fn has_ambiguous_node_labels(&self) -> bool;
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
        Self::from_rows(&tables, &edges)
    }

    pub(crate) fn from_rows(
        tables: &[crate::builder::RegisteredTable],
        edges: &[crate::builder::RegisteredEdge],
    ) -> GraphResult<Self> {
        let labels = load_labels(tables)?;
        let rels = load_rels(tables, edges)?;
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
        let valid_dynamic_name = gql_identifier_from_text(rel_type).is_some();
        let mut candidates = Vec::<&RelTypeInfo>::new();
        for candidate in self.rels.iter().filter(|candidate| {
            candidate.from_table_oid == from_table_oid
                && candidate.to_table_oid == to_table_oid
                && (candidate.rel_type == rel_type
                    || (valid_dynamic_name
                        && candidate
                            .edge_mapping
                            .as_ref()
                            .is_some_and(|mapping| mapping.label_column.is_some())))
        }) {
            let candidate_mapping = candidate
                .edge_mapping
                .as_ref()
                .map(|mapping| mapping.mapping_id);
            if candidates.iter().any(|existing| {
                existing
                    .edge_mapping
                    .as_ref()
                    .map(|mapping| mapping.mapping_id)
                    == candidate_mapping
            }) {
                continue;
            }
            candidates.push(candidate);
        }
        match candidates.as_slice() {
            [candidate] => {
                let mut resolved = (*candidate).clone();
                resolved.rel_type = rel_type.to_string();
                return Ok(resolved);
            }
            [_, _, ..] => {
                return Err(GqlError::bind(
                    span,
                    format!(
                        "ambiguous relationship type `{rel_type}` from table {from_table_oid} to {to_table_oid}"
                    ),
                ));
            }
            [] => {}
        }
        Err(GqlError::bind(
            span,
            format!(
                "unknown relationship type `{rel_type}` from table {from_table_oid} to {to_table_oid}"
            ),
        ))
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

    fn has_ambiguous_node_labels(&self) -> bool {
        self.labels
            .values()
            .any(|entry| matches!(entry, LabelEntry::Ambiguous))
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
        let table_oid = table.table_oid;
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
                primary_key_columns: table.id_columns.columns().to_vec(),
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
        registered_table_oids.insert(table.table_oid, table.table_name.as_str());
    }
    let mut rels = Vec::with_capacity(edges.len());
    for edge in edges {
        let (from_node_table, relationship_source_table_oid) =
            if registered_tables.contains(edge.from_table.as_str()) {
                (Some(edge.from_table.as_str()), Some(edge.from_table_oid))
            } else {
                let edge_table_oid = edge.from_table_oid;
                let source_table_oid = edge_source_fk_table_oid(edge)?;
                let from_node_table =
                    source_table_oid.and_then(|oid| registered_table_oids.get(&oid).copied());
                (from_node_table, source_table_oid.map(|_| edge_table_oid))
            };
        let Some(from_node_table) = from_node_table else {
            continue;
        };
        let source_table_oid = tables
            .iter()
            .find(|table| table.table_name == from_node_table)
            .map(|table| table.table_oid)
            .ok_or_else(|| {
                GraphError::Internal(format!(
                    "registered source table disappeared: {from_node_table}"
                ))
            })?;
        let target_table_oid = edge.to_table_oid;
        let edge_mapping = relationship_source_table_oid.map(|edge_table_oid| EdgeMappingInfo {
            mapping_id: edge.mapping_id,
            edge_table_oid,
            source_table_oid,
            target_table_oid,
            source_column: edge.from_column.clone(),
            target_column: edge.to_column.clone(),
            source_key_columns: edge.source_key_columns.clone(),
            bidirectional: edge.bidirectional,
            label_column: edge.label_column.clone(),
        });
        for rel_type in relationship_type_names(edge) {
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

fn relationship_type_names(edge: &RegisteredEdge) -> Vec<String> {
    // Dynamic mappings are represented once by their registered fallback.
    // Explicit GQL/Cypher relationship names resolve structurally against the
    // mapping, so binding cost is independent of source vocabulary size.
    vec![edge.label.clone()]
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
    foreign_key_target_table_oid(edge.from_table_oid, &edge.from_column)
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
    ambiguous_node_labels: bool,
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
    /// Dynamic relationship label column, when present.
    pub(crate) label_column: Option<&'a str>,
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
                primary_key_columns: vec!["id".to_string()],
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
                primary_key_columns: vec!["id".to_string()],
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
                mapping_id: spec.edge_table_oid as u64,
                edge_table_oid: spec.edge_table_oid,
                source_table_oid: spec.from_table_oid,
                target_table_oid: spec.to_table_oid,
                source_column: spec.source_column.to_string(),
                target_column: spec.target_column.to_string(),
                source_key_columns: PrimaryKeySpec::from_columns(vec!["id".to_string()]),
                bidirectional: spec.bidirectional,
                label_column: spec.label_column.map(str::to_string),
            }),
        });
        self
    }

    /// Mark the fake catalog as containing at least one ambiguous node label.
    pub(crate) fn with_ambiguous_node_labels(mut self) -> Self {
        self.ambiguous_node_labels = true;
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
        let mut candidates = self.rels.iter().filter(|rel| {
            rel.from_table_oid == from_table_oid
                && rel.to_table_oid == to_table_oid
                && (rel.rel_type == rel_type
                    || rel
                        .edge_mapping
                        .as_ref()
                        .is_some_and(|mapping| mapping.label_column.is_some()))
        });
        let Some(candidate) = candidates.next() else {
            return Err(GqlError::bind(
                span,
                format!("unknown relationship type `{rel_type}`"),
            ));
        };
        let mapping_id = candidate
            .edge_mapping
            .as_ref()
            .map(|mapping| mapping.mapping_id);
        if candidates.any(|other| {
            other
                .edge_mapping
                .as_ref()
                .map(|mapping| mapping.mapping_id)
                != mapping_id
        }) {
            return Err(GqlError::bind(
                span,
                format!("ambiguous relationship type `{rel_type}`"),
            ));
        }
        let mut resolved = candidate.clone();
        resolved.rel_type = rel_type.to_string();
        Ok(resolved)
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

    fn has_ambiguous_node_labels(&self) -> bool {
        self.ambiguous_node_labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn dynamic_rel(mapping_id: u64) -> RelTypeInfo {
        RelTypeInfo {
            rel_type: "fallback".into(),
            from_table_oid: 10,
            to_table_oid: 20,
            edge_mapping: Some(EdgeMappingInfo {
                mapping_id,
                edge_table_oid: u32::try_from(mapping_id).unwrap(),
                source_table_oid: 10,
                target_table_oid: 20,
                source_column: "source_id".into(),
                target_column: "target_id".into(),
                source_key_columns: PrimaryKeySpec::from_columns(vec!["id".into()]),
                bidirectional: false,
                label_column: Some("rel_type".into()),
            }),
        }
    }

    #[test]
    fn unique_dynamic_mapping_resolves_named_type_without_vocabulary_enumeration() {
        let snapshot = CatalogSnapshotImpl {
            labels: HashMap::new(),
            rels: vec![dynamic_rel(1)],
        };
        let resolved = snapshot
            .resolve_rel_type("type_65536", 10, 20, Span::new(0, 10))
            .unwrap();
        assert_eq!(resolved.rel_type, "type_65536");
        assert_eq!(resolved.edge_mapping.unwrap().mapping_id, 1);
    }

    #[test]
    fn dynamic_mapping_resolution_rejects_ambiguous_or_invalid_names() {
        let snapshot = CatalogSnapshotImpl {
            labels: HashMap::new(),
            rels: vec![dynamic_rel(1), dynamic_rel(2)],
        };
        assert!(snapshot
            .resolve_rel_type("type_255", 10, 20, Span::new(0, 8))
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
        assert!(snapshot
            .resolve_rel_type("not-valid", 10, 20, Span::new(0, 9))
            .is_err());
    }

    #[test]
    fn exact_and_open_dynamic_mapping_collision_is_ambiguous() {
        let mut exact = dynamic_rel(1);
        exact.rel_type = "collision".into();
        exact.edge_mapping.as_mut().unwrap().label_column = None;
        let snapshot = CatalogSnapshotImpl {
            labels: HashMap::new(),
            rels: vec![exact, dynamic_rel(2)],
        };
        assert!(snapshot
            .resolve_rel_type("collision", 10, 20, Span::new(0, 9))
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
    }
}
