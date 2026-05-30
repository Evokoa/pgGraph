//! Stable explain strings for development-only GQL inspection.

use super::physical_plan::{PhysicalPlan, ReturnSlot};

/// Render a compact physical plan explanation.
pub(crate) fn explain(plan: &PhysicalPlan) -> String {
    let returns = plan
        .returns
        .iter()
        .map(|slot| match slot {
            ReturnSlot::Node { name, .. } | ReturnSlot::Property { name, .. } => name.as_str(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "OneHopExpand(source={}:{}, rel={}, target={}:{}, return=[{}])",
        plan.source_var,
        plan.source_table_oid,
        plan.rel_type,
        plan.target_var,
        plan.target_table_oid,
        returns
    )
}
