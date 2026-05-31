//! Lowering from logical GQL plans to executable physical plans.

use super::logical_plan::{LogicalPlan, ReturnBinding};
use super::physical_plan::{PhysicalPlan, ReturnSlot};

/// Lower a bound logical plan into the executable Phase 1B physical plan.
pub(crate) fn lower(plan: LogicalPlan) -> PhysicalPlan {
    PhysicalPlan {
        source_var: plan.source.var,
        source_table_oid: plan.source.table_oid,
        source_label: plan.source.label,
        rel_type: plan.relationship.rel_type,
        rel_var: plan.relationship.var,
        direction: plan.relationship.direction,
        hops: plan.relationship.hops,
        target_var: plan.target.var,
        target_table_oid: plan.target.table_oid,
        target_label: plan.target.label,
        predicate: plan.predicate,
        order_by: plan.order_by,
        skip: plan.skip,
        limit: plan.limit,
        returns: plan
            .returns
            .into_iter()
            .map(|slot| match slot {
                ReturnBinding::Node { side, name } => ReturnSlot::Node { side, name },
                ReturnBinding::Relationship { name } => ReturnSlot::Relationship { name },
                ReturnBinding::Property {
                    side,
                    property,
                    name,
                } => ReturnSlot::Property {
                    side,
                    property,
                    name,
                },
            })
            .collect(),
    }
}
