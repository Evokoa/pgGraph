//! Lowering from logical GQL plans to executable physical plans.

use super::logical_plan::{
    CreateReturnBinding, CreateValue, LogicalCreateNode, LogicalDeleteEdge,
    LogicalDetachDeleteNode, LogicalJoinNodeSlot, LogicalJoinPathSlot, LogicalJoinPattern,
    LogicalJoinPlan, LogicalJoinRelSlot, LogicalMergeNode, LogicalNodeScan, LogicalPlan,
    LogicalRemoveProperty, LogicalSetProperty, LogicalStatement, LogicalWildcardPathPlan,
    LogicalWildcardPathSegment, ReturnBinding,
};
use super::physical_plan::{
    CreatePropertySlot, CreateReturnSlot, CreateValueSlot, PhysicalCreateNode, PhysicalDeleteEdge,
    PhysicalDetachDeleteNode, PhysicalIncidentEdge, PhysicalJoinNodeSlot, PhysicalJoinPathSlot,
    PhysicalJoinPattern, PhysicalJoinPlan, PhysicalJoinRelSlot, PhysicalMergeNode,
    PhysicalNodeScan, PhysicalPlan, PhysicalRemoveProperty, PhysicalSetProperty, PhysicalStatement,
    PhysicalWildcardPathPlan, PhysicalWildcardPathSegment, ReturnSlot,
};

/// Lower a bound logical statement into an executable physical statement.
pub(crate) fn lower_statement(statement: LogicalStatement) -> PhysicalStatement {
    match statement {
        LogicalStatement::Read(plan) => PhysicalStatement::Read(lower(plan)),
        LogicalStatement::NodeScan(plan) => PhysicalStatement::NodeScan(lower_node_scan(plan)),
        LogicalStatement::JoinRead(plan) => PhysicalStatement::JoinRead(lower_join(plan)),
        LogicalStatement::WildcardPathRead(plan) => {
            PhysicalStatement::WildcardPathRead(lower_wildcard_path(plan))
        }
        LogicalStatement::CreateNode(plan) => {
            PhysicalStatement::CreateNode(lower_create_node(plan))
        }
        LogicalStatement::SetProperty(plan) => {
            PhysicalStatement::SetProperty(lower_set_property(plan))
        }
        LogicalStatement::RemoveProperty(plan) => {
            PhysicalStatement::RemoveProperty(lower_remove_property(plan))
        }
        LogicalStatement::DeleteEdge(plan) => {
            PhysicalStatement::DeleteEdge(lower_delete_edge(plan))
        }
        LogicalStatement::DetachDeleteNode(plan) => {
            PhysicalStatement::DetachDeleteNode(lower_detach_delete_node(plan))
        }
        LogicalStatement::MergeNode(plan) => PhysicalStatement::MergeNode(lower_merge_node(plan)),
    }
}

fn lower_join(plan: LogicalJoinPlan) -> PhysicalJoinPlan {
    PhysicalJoinPlan {
        node_slots: plan
            .node_slots
            .into_iter()
            .map(lower_join_node_slot)
            .collect(),
        rel_slots: plan
            .rel_slots
            .into_iter()
            .map(lower_join_rel_slot)
            .collect(),
        path_slots: plan
            .path_slots
            .into_iter()
            .map(lower_join_path_slot)
            .collect(),
        patterns: plan.patterns.into_iter().map(lower_join_pattern).collect(),
        returns: lower_returns(plan.returns),
        distinct: plan.distinct,
        predicate: plan.predicate,
        order_by: plan.order_by,
        required_table_oids: plan.required_table_oids,
        skip: plan.skip,
        limit: plan.limit,
    }
}

fn lower_join_node_slot(slot: LogicalJoinNodeSlot) -> PhysicalJoinNodeSlot {
    PhysicalJoinNodeSlot {
        var: slot.var,
        table_oid: slot.table_oid,
        label: slot.label,
    }
}

fn lower_join_rel_slot(slot: LogicalJoinRelSlot) -> PhysicalJoinRelSlot {
    PhysicalJoinRelSlot {
        var: slot.var,
        pattern_slot: slot.pattern_slot,
    }
}

fn lower_join_path_slot(slot: LogicalJoinPathSlot) -> PhysicalJoinPathSlot {
    PhysicalJoinPathSlot {
        var: slot.var,
        pattern_slot: slot.pattern_slot,
    }
}

fn lower_join_pattern(pattern: LogicalJoinPattern) -> PhysicalJoinPattern {
    PhysicalJoinPattern {
        source_slot: pattern.source_slot,
        rel_type: pattern.rel_type,
        direction: pattern.direction,
        target_slot: pattern.target_slot,
    }
}

fn lower_wildcard_path(plan: LogicalWildcardPathPlan) -> PhysicalWildcardPathPlan {
    PhysicalWildcardPathPlan {
        path_var: plan.path_var,
        source_var: plan.source_var,
        rel_var: plan.rel_var,
        target_var: plan.target_var,
        direction: plan.direction,
        returns: lower_returns(plan.returns),
        source_table_filter: plan.source_table_filter,
        target_table_filter: plan.target_table_filter,
        rel_type_filter: plan.rel_type_filter,
        segments: plan
            .segments
            .into_iter()
            .map(lower_wildcard_path_segment)
            .collect(),
        required_node_table_oids: plan.required_node_table_oids,
        table_labels: plan.table_labels,
        rel_type_labels: plan.rel_type_labels,
        predicate: plan.predicate,
        skip: plan.skip,
        limit: plan.limit,
    }
}

fn lower_wildcard_path_segment(segment: LogicalWildcardPathSegment) -> PhysicalWildcardPathSegment {
    PhysicalWildcardPathSegment {
        rel_var: segment.rel_var,
        target_var: segment.target_var,
        direction: segment.direction,
        hops: segment.hops,
        target_table_filter: segment.target_table_filter,
        rel_type_filter: segment.rel_type_filter,
    }
}

fn lower_node_scan(plan: LogicalNodeScan) -> PhysicalNodeScan {
    PhysicalNodeScan {
        var: plan.node.var,
        table_oid: plan.node.table_oid,
        label: plan.node.label,
        predicate: plan.predicate,
        order_by: plan.order_by,
        skip: plan.skip,
        limit: plan.limit,
        distinct_stages: lower_return_stages(plan.distinct_stages),
        distinct: plan.distinct,
        returns: lower_returns(plan.returns),
    }
}

/// Lower a bound logical plan into the executable Phase 1B physical plan.
pub(crate) fn lower(plan: LogicalPlan) -> PhysicalPlan {
    PhysicalPlan {
        optional: plan.optional,
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
        distinct_stages: lower_return_stages(plan.distinct_stages),
        distinct: plan.distinct,
        returns: lower_returns(plan.returns),
    }
}

fn lower_create_node(plan: LogicalCreateNode) -> PhysicalCreateNode {
    PhysicalCreateNode {
        var: plan.node.var,
        table_oid: plan.node.table_oid,
        label: plan.node.label,
        properties: plan
            .properties
            .into_iter()
            .map(lower_create_property)
            .collect(),
        returns: lower_create_returns(plan.returns),
    }
}

fn lower_merge_node(plan: LogicalMergeNode) -> PhysicalMergeNode {
    PhysicalMergeNode {
        var: plan.node.var,
        table_oid: plan.node.table_oid,
        label: plan.node.label,
        properties: plan
            .properties
            .into_iter()
            .map(lower_create_property)
            .collect(),
        on_create: plan.on_create.map(lower_create_property),
        on_match: plan.on_match.map(lower_create_property),
        returns: lower_create_returns(plan.returns),
    }
}

fn lower_set_property(plan: LogicalSetProperty) -> PhysicalSetProperty {
    PhysicalSetProperty {
        var: plan.node.var,
        table_oid: plan.node.table_oid,
        label: plan.node.label,
        predicate: plan.predicate,
        property: plan.property,
        value: match plan.value {
            CreateValue::Literal(value) => CreateValueSlot::Literal(literal_value_json(value)),
            CreateValue::Param(name) => CreateValueSlot::Param(name),
        },
        returns: lower_create_returns(plan.returns),
    }
}

fn lower_remove_property(plan: LogicalRemoveProperty) -> PhysicalRemoveProperty {
    PhysicalRemoveProperty {
        var: plan.node.var,
        table_oid: plan.node.table_oid,
        label: plan.node.label,
        predicate: plan.predicate,
        property: plan.property,
        returns: lower_create_returns(plan.returns),
    }
}

fn lower_delete_edge(plan: LogicalDeleteEdge) -> PhysicalDeleteEdge {
    PhysicalDeleteEdge {
        source_var: plan.source.var,
        source_table_oid: plan.source.table_oid,
        source_label: plan.source.label,
        rel_type: plan.relationship.rel_type,
        rel_var: plan.rel_var,
        direction: plan.relationship.direction,
        target_var: plan.target.var,
        target_table_oid: plan.target.table_oid,
        target_label: plan.target.label,
        edge_table_oid: plan.edge.edge_table_oid,
        edge_source_table_oid: plan.edge.source_table_oid,
        edge_target_table_oid: plan.edge.target_table_oid,
        source_column: plan.edge.source_column,
        target_column: plan.edge.target_column,
        bidirectional: plan.edge.bidirectional,
        predicate: plan.predicate,
        returns: lower_returns(plan.returns),
    }
}

fn lower_detach_delete_node(plan: LogicalDetachDeleteNode) -> PhysicalDetachDeleteNode {
    PhysicalDetachDeleteNode {
        var: plan.node.var,
        table_oid: plan.node.table_oid,
        label: plan.node.label,
        predicate: plan.predicate,
        incident_edges: plan
            .incident_edges
            .into_iter()
            .map(|incident| PhysicalIncidentEdge {
                rel_type: incident.rel_type,
                edge_table_oid: incident.edge.edge_table_oid,
                edge_source_table_oid: incident.edge.source_table_oid,
                edge_target_table_oid: incident.edge.target_table_oid,
                source_column: incident.edge.source_column,
                target_column: incident.edge.target_column,
                bidirectional: incident.edge.bidirectional,
            })
            .collect(),
        returns: lower_create_returns(plan.returns),
    }
}

fn lower_create_property(property: super::logical_plan::CreateProperty) -> CreatePropertySlot {
    CreatePropertySlot {
        property: property.property,
        value: match property.value {
            CreateValue::Literal(value) => CreateValueSlot::Literal(literal_value_json(value)),
            CreateValue::Param(name) => CreateValueSlot::Param(name),
        },
    }
}

fn lower_create_returns(returns: Vec<CreateReturnBinding>) -> Vec<CreateReturnSlot> {
    returns
        .into_iter()
        .map(|binding| match binding {
            CreateReturnBinding::Node { name } => CreateReturnSlot::Node { name },
            CreateReturnBinding::Property { property, name } => {
                CreateReturnSlot::Property { property, name }
            }
        })
        .collect()
}

fn lower_return_stages(stages: Vec<Vec<ReturnBinding>>) -> Vec<Vec<ReturnSlot>> {
    stages.into_iter().map(lower_returns).collect()
}

fn lower_returns(returns: Vec<ReturnBinding>) -> Vec<ReturnSlot> {
    returns
        .into_iter()
        .map(|slot| match slot {
            ReturnBinding::Node { side, name } => ReturnSlot::Node { side, name },
            ReturnBinding::Relationship { name } => ReturnSlot::Relationship { name },
            ReturnBinding::Path { name } => ReturnSlot::Path { name },
            ReturnBinding::PathFunction { func, name } => ReturnSlot::PathFunction { func, name },
            ReturnBinding::Property {
                side,
                property,
                name,
            } => ReturnSlot::Property {
                side,
                property,
                name,
            },
            ReturnBinding::Aggregate {
                func,
                arg,
                distinct,
                name,
            } => ReturnSlot::Aggregate {
                func,
                arg,
                distinct,
                name,
            },
        })
        .collect()
}

fn literal_value_json(value: crate::gql::ast::LiteralValue) -> serde_json::Value {
    match value {
        crate::gql::ast::LiteralValue::Str(value) => serde_json::Value::String(value),
        crate::gql::ast::LiteralValue::Int(value) => serde_json::Value::from(value),
        crate::gql::ast::LiteralValue::Float(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        crate::gql::ast::LiteralValue::Bool(value) => serde_json::Value::Bool(value),
        crate::gql::ast::LiteralValue::Null => serde_json::Value::Null,
    }
}
