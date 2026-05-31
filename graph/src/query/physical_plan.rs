//! Physical plans executable against engine topology stores.

use super::logical_plan::{BindingSide, BoundDirection, HopBounds, Predicate, SortBinding};

/// Maximum GQL matches collected before sorting/projection.
pub(crate) const MAX_GQL_RESULT_ROWS: usize = 10_000;

/// Single-hop physical plan for Phase 1B.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhysicalPlan {
    /// Source node variable.
    pub(crate) source_var: String,
    /// Source table OID.
    pub(crate) source_table_oid: u32,
    /// Source label.
    pub(crate) source_label: String,
    /// Relationship type label.
    pub(crate) rel_type: String,
    /// Optional relationship variable.
    pub(crate) rel_var: Option<String>,
    /// Traversal direction.
    pub(crate) direction: BoundDirection,
    /// Hop bounds.
    pub(crate) hops: HopBounds,
    /// Target node variable.
    pub(crate) target_var: String,
    /// Target table OID.
    pub(crate) target_table_oid: u32,
    /// Target label.
    pub(crate) target_label: String,
    /// Return slots in requested order.
    pub(crate) returns: Vec<ReturnSlot>,
    /// Optional hydrated-row predicate.
    pub(crate) predicate: Option<Predicate>,
    /// Sort keys in requested order.
    pub(crate) order_by: Vec<SortBinding>,
    /// Number of rows to skip after ordering.
    pub(crate) skip: Option<u64>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u64>,
}

/// Physical return slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReturnSlot {
    /// Whole node value.
    Node { side: BindingSide, name: String },
    /// Whole relationship value.
    Relationship { name: String },
    /// Node property value.
    Property {
        /// Source or target binding.
        side: BindingSide,
        /// Source property name.
        property: String,
        /// Return column name.
        name: String,
    },
}

impl PhysicalPlan {
    /// Table OIDs whose rows must be visible to the current SQL role.
    pub(crate) fn required_table_oids(&self) -> [u32; 2] {
        [self.source_table_oid, self.target_table_oid]
    }

    /// Maximum matches the executor should collect for this plan.
    pub(crate) fn execution_row_cap(&self) -> usize {
        if self.order_by.is_empty() {
            if let Some(limit) = self.limit {
                let requested = self.skip.unwrap_or(0).saturating_add(limit);
                return usize::try_from(requested)
                    .unwrap_or(usize::MAX)
                    .min(MAX_GQL_RESULT_ROWS);
            }
        }
        MAX_GQL_RESULT_ROWS
    }

    /// Whether hitting the execution cap means results would be incomplete.
    pub(crate) fn cap_exhaustion_is_error(&self) -> bool {
        !self.order_by.is_empty() || self.limit.is_none()
    }
}
