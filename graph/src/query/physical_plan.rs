//! Physical plans executable against immutable CSR stores.

use super::logical_plan::{BindingSide, Predicate};

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
}

/// Physical return slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReturnSlot {
    /// Whole node value.
    Node { side: BindingSide, name: String },
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
}
