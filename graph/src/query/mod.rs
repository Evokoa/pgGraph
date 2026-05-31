//! Read-only GQL binding, planning, lowering, and execution.
//!
//! The query layer is intentionally pgrx-free except for the catalog adapter.
//! Parser output binds against a catalog snapshot, lowers into a physical plan,
//! and executes against [`crate::engine::Engine`] topology stores.

pub(crate) mod catalog_snapshot;
pub(crate) mod execute;
pub(crate) mod explain;
pub(crate) mod logical_plan;
pub(crate) mod lower;
pub(crate) mod physical_plan;
pub(crate) mod semantics;
pub(crate) mod value;

#[cfg(test)]
mod tests;
