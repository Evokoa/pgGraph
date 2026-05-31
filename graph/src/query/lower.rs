//! Lowering from logical GQL plans to executable physical plans.

use super::logical_plan::{
    CreateReturnBinding, CreateValue, LogicalCreateNode, LogicalDeleteEdge, LogicalNodeScan,
    LogicalPlan, LogicalSetProperty, LogicalStatement, ReturnBinding,
};
use super::physical_plan::{
    CreatePropertySlot, CreateReturnSlot, CreateValueSlot, PhysicalCreateNode, PhysicalDeleteEdge,
    PhysicalNodeScan, PhysicalPlan, PhysicalSetProperty, PhysicalStatement, ReturnSlot,
};

/// Lower a bound logical statement into an executable physical statement.
pub(crate) fn lower_statement(statement: LogicalStatement) -> PhysicalStatement {
    match statement {
        LogicalStatement::Read(plan) => PhysicalStatement::Read(lower(plan)),
        LogicalStatement::NodeScan(plan) => PhysicalStatement::NodeScan(lower_node_scan(plan)),
        LogicalStatement::CreateNode(plan) => {
            PhysicalStatement::CreateNode(lower_create_node(plan))
        }
        LogicalStatement::SetProperty(plan) => {
            PhysicalStatement::SetProperty(lower_set_property(plan))
        }
        LogicalStatement::DeleteEdge(plan) => {
            PhysicalStatement::DeleteEdge(lower_delete_edge(plan))
        }
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

fn lower_create_node(plan: LogicalCreateNode) -> PhysicalCreateNode {
    PhysicalCreateNode {
        var: plan.node.var,
        table_oid: plan.node.table_oid,
        label: plan.node.label,
        properties: plan
            .properties
            .into_iter()
            .map(|property| CreatePropertySlot {
                property: property.property,
                value: match property.value {
                    CreateValue::Literal(value) => {
                        CreateValueSlot::Literal(literal_value_json(value))
                    }
                    CreateValue::Param(name) => CreateValueSlot::Param(name),
                },
            })
            .collect(),
        returns: plan
            .returns
            .into_iter()
            .map(|slot| match slot {
                CreateReturnBinding::Node { name } => CreateReturnSlot::Node { name },
                CreateReturnBinding::Property { property, name } => {
                    CreateReturnSlot::Property { property, name }
                }
            })
            .collect(),
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
        returns: plan
            .returns
            .into_iter()
            .map(|binding| match binding {
                CreateReturnBinding::Node { name } => CreateReturnSlot::Node { name },
                CreateReturnBinding::Property { property, name } => {
                    CreateReturnSlot::Property { property, name }
                }
            })
            .collect(),
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
