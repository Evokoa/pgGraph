//! Stable explain strings for read-oriented GQL inspection.

use super::physical_plan::{PhysicalNodeScan, PhysicalPlan, ReturnSlot};

/// Render a compact physical plan explanation.
pub(crate) fn explain(plan: &PhysicalPlan) -> String {
    let returns = plan
        .returns
        .iter()
        .map(ReturnSlot::name)
        .collect::<Vec<_>>()
        .join(", ");
    let op = if plan.optional {
        "OptionalExpand"
    } else {
        "Expand"
    };
    format!(
        "{op}(source={}:{}, rel={}, hops={}..{}, target={}:{}, return=[{}])",
        plan.source_var,
        plan.source_label,
        plan.rel_type,
        plan.hops.min,
        plan.hops.max,
        plan.target_var,
        plan.target_label,
        returns
    )
}

/// Render a compact node-scan plan explanation.
pub(crate) fn explain_node_scan(plan: &PhysicalNodeScan) -> String {
    let returns = plan
        .returns
        .iter()
        .map(ReturnSlot::name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "NodeScan(node={}:{}, return=[{}])",
        plan.var, plan.label, returns
    )
}
