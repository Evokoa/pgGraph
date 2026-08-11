//! Pure caller-visibility policy consumed by graph algorithms.
//!
//! PostgreSQL policy evaluation and SPI scans live in `sql_visibility`; this
//! module only owns compact query-scoped admission state.

use roaring::RoaringBitmap;

use crate::edge_store::RelationshipId;
use crate::safety::{GraphError, GraphResult};

/// Caller-visible intersection of the loaded projection and source-table RLS.
#[derive(Debug, Clone, Default)]
pub(crate) enum VisibilityScope {
    /// No registered source relation applies RLS to the outer caller.
    #[default]
    Unrestricted,
    /// Hidden projection identities for relations whose RLS policies apply.
    Enforced {
        hidden_nodes: RoaringBitmap,
        hidden_relationships: RoaringBitmap,
        relationship_rls_edge_types: RoaringBitmap,
    },
}

impl VisibilityScope {
    pub(crate) fn enforced(
        hidden_nodes: RoaringBitmap,
        hidden_relationships: RoaringBitmap,
        relationship_rls_edge_types: RoaringBitmap,
    ) -> Self {
        Self::Enforced {
            hidden_nodes,
            hidden_relationships,
            relationship_rls_edge_types,
        }
    }

    #[inline(always)]
    pub(crate) fn allows_node(&self, node_idx: u32) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Enforced { hidden_nodes, .. } => !hidden_nodes.contains(node_idx),
        }
    }

    #[inline(always)]
    pub(crate) fn allows_relationship(
        &self,
        edge_type: u8,
        relationship_id: Option<RelationshipId>,
    ) -> GraphResult<bool> {
        match self {
            Self::Unrestricted => Ok(true),
            Self::Enforced {
                hidden_relationships: _,
                relationship_rls_edge_types,
                ..
            } if !relationship_rls_edge_types.contains(u32::from(edge_type)) => Ok(true),
            Self::Enforced {
                hidden_relationships,
                ..
            } => relationship_id
                .map(|id| !hidden_relationships.contains(id))
                .ok_or(GraphError::RlsRelationshipIdentityMissing),
        }
    }

    pub(crate) fn hide_nodes(&mut self, nodes: &RoaringBitmap) {
        if let Self::Enforced { hidden_nodes, .. } = self {
            *hidden_nodes |= nodes;
        }
    }

    pub(crate) fn hide_node(&mut self, node_idx: u32) {
        if let Self::Enforced { hidden_nodes, .. } = self {
            hidden_nodes.insert(node_idx);
        }
    }

    pub(crate) fn reveal_node(&mut self, node_idx: u32) {
        if let Self::Enforced { hidden_nodes, .. } = self {
            hidden_nodes.remove(node_idx);
        }
    }

    pub(crate) fn hide_relationship(&mut self, relationship_id: RelationshipId) {
        if let Self::Enforced {
            hidden_relationships,
            ..
        } = self
        {
            hidden_relationships.insert(relationship_id);
        }
    }

    pub(crate) fn reveal_relationship(&mut self, relationship_id: RelationshipId) {
        if let Self::Enforced {
            hidden_relationships,
            ..
        } = self
        {
            hidden_relationships.remove(relationship_id);
        }
    }

    #[cfg(feature = "development")]
    pub(crate) fn hidden_counts(&self) -> (u64, u64) {
        match self {
            Self::Unrestricted => (0, 0),
            Self::Enforced {
                hidden_nodes,
                hidden_relationships,
                ..
            } => (hidden_nodes.len(), hidden_relationships.len()),
        }
    }
}

/// Borrowed operation state shared by topology algorithms.
pub(crate) struct QueryExecutionContext<'a> {
    pub(crate) governor: &'a crate::resource::ResourceGovernor,
    pub(crate) visibility: &'a VisibilityScope,
}

impl<'a> QueryExecutionContext<'a> {
    pub(crate) const fn new(
        governor: &'a crate::resource::ResourceGovernor,
        visibility: &'a VisibilityScope,
    ) -> Self {
        Self {
            governor,
            visibility,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrestricted_scope_allows_nodes_and_identityless_relationships() {
        let scope = VisibilityScope::Unrestricted;
        assert!(scope.allows_node(7));
        assert!(scope.allows_relationship(3, None).unwrap());
    }

    #[test]
    fn enforced_scope_hides_exact_node_and_relationship_ids() {
        let mut hidden_nodes = RoaringBitmap::new();
        hidden_nodes.insert(7);
        let mut hidden_relationships = RoaringBitmap::new();
        hidden_relationships.insert(11);
        let mut rls_edge_types = RoaringBitmap::new();
        rls_edge_types.insert(3);
        let scope = VisibilityScope::enforced(hidden_nodes, hidden_relationships, rls_edge_types);

        assert!(!scope.allows_node(7));
        assert!(scope.allows_node(8));
        assert!(!scope.allows_relationship(3, Some(11)).unwrap());
        assert!(scope.allows_relationship(3, Some(12)).unwrap());
        assert!(scope.allows_relationship(4, None).unwrap());
        assert!(matches!(
            scope.allows_relationship(3, None),
            Err(GraphError::RlsRelationshipIdentityMissing)
        ));
    }

    #[test]
    fn node_only_rls_does_not_require_relationship_identity() {
        let scope = VisibilityScope::enforced(
            RoaringBitmap::new(),
            RoaringBitmap::new(),
            RoaringBitmap::new(),
        );
        assert!(scope.allows_relationship(3, None).unwrap());
    }
}
