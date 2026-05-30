//! JSON value projection and hydrated predicate evaluation for GQL rows.

use std::collections::HashMap;

use crate::safety::{GraphError, GraphResult};

use super::execute::{GqlNodeCoordinate, GqlRow};
use super::logical_plan::{BindingSide, BoundCmpOp, Predicate, ValueExpr};
use super::physical_plan::{PhysicalPlan, ReturnSlot};

/// Hydrated source rows keyed by graph coordinate.
pub(crate) type HydratedRows = HashMap<(u32, String), serde_json::Value>;

/// Query parameters supplied by SQL callers.
pub(crate) type QueryParams = serde_json::Map<String, serde_json::Value>;

/// Project coordinate matches into canonical JSON rows.
///
/// # Errors
///
/// Returns [`GraphError::InvalidFilter`] when a required parameter is missing
/// or a predicate comparison cannot be evaluated safely.
pub(crate) fn project_rows(
    rows: Vec<GqlRow>,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    params: &QueryParams,
    hydrate_nodes: bool,
) -> GraphResult<Vec<serde_json::Value>> {
    let mut projected = Vec::new();
    for row in rows {
        if predicate_matches(plan.predicate.as_ref(), &row, hydrated, params)? {
            projected.push(project_row(&row, plan, hydrated, hydrate_nodes));
        }
    }
    Ok(projected)
}

/// Return whether this plan requires SQL row hydration.
pub(crate) fn requires_hydration(plan: &PhysicalPlan, hydrate_nodes: bool) -> bool {
    hydrate_nodes
        || plan.predicate.is_some()
        || plan
            .returns
            .iter()
            .any(|slot| matches!(slot, ReturnSlot::Property { .. }))
}

fn predicate_matches(
    predicate: Option<&Predicate>,
    row: &GqlRow,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<bool> {
    match predicate {
        Some(predicate) => eval_predicate(predicate, row, hydrated, params),
        None => Ok(true),
    }
}

fn eval_predicate(
    predicate: &Predicate,
    row: &GqlRow,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<bool> {
    match predicate {
        Predicate::And(lhs, rhs) => Ok(eval_predicate(lhs, row, hydrated, params)?
            && eval_predicate(rhs, row, hydrated, params)?),
        Predicate::Or(lhs, rhs) => Ok(eval_predicate(lhs, row, hydrated, params)?
            || eval_predicate(rhs, row, hydrated, params)?),
        Predicate::Not(expr) => Ok(!eval_predicate(expr, row, hydrated, params)?),
        Predicate::Compare { lhs, op, rhs } => {
            let lhs = eval_value(lhs, row, hydrated, params)?;
            let rhs = rhs
                .as_ref()
                .map(|expr| eval_value(expr, row, hydrated, params))
                .transpose()?;
            compare_values(&lhs, *op, rhs.as_ref())
        }
    }
}

fn eval_value(
    expr: &ValueExpr,
    row: &GqlRow,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<serde_json::Value> {
    match expr {
        ValueExpr::Property { side, property } => {
            Ok(property_value(coordinate(row, *side), hydrated, property))
        }
        ValueExpr::Literal(value) => Ok(value.clone()),
        ValueExpr::Param(name) => {
            params
                .get(name)
                .cloned()
                .ok_or_else(|| GraphError::InvalidFilter {
                    reason: format!("missing GQL parameter `{name}`"),
                })
        }
        ValueExpr::List(values) => Ok(serde_json::Value::Array(values.clone())),
    }
}

fn compare_values(
    lhs: &serde_json::Value,
    op: BoundCmpOp,
    rhs: Option<&serde_json::Value>,
) -> GraphResult<bool> {
    match op {
        BoundCmpOp::Eq => Ok(lhs == required_rhs(op, rhs)?),
        BoundCmpOp::Neq => Ok(lhs != required_rhs(op, rhs)?),
        BoundCmpOp::Lt => ordered(lhs, required_rhs(op, rhs)?).map(|ordering| ordering.is_lt()),
        BoundCmpOp::Lte => ordered(lhs, required_rhs(op, rhs)?).map(|ordering| !ordering.is_gt()),
        BoundCmpOp::Gt => ordered(lhs, required_rhs(op, rhs)?).map(|ordering| ordering.is_gt()),
        BoundCmpOp::Gte => ordered(lhs, required_rhs(op, rhs)?).map(|ordering| !ordering.is_lt()),
        BoundCmpOp::In => match required_rhs(op, rhs)? {
            serde_json::Value::Array(values) => Ok(values.iter().any(|value| value == lhs)),
            _ => Err(GraphError::InvalidFilter {
                reason: "GQL IN requires a list right-hand side".to_string(),
            }),
        },
        BoundCmpOp::IsNull => Ok(lhs.is_null()),
        BoundCmpOp::IsNotNull => Ok(!lhs.is_null()),
    }
}

fn required_rhs<'a>(
    op: BoundCmpOp,
    rhs: Option<&'a serde_json::Value>,
) -> GraphResult<&'a serde_json::Value> {
    rhs.ok_or_else(|| GraphError::InvalidFilter {
        reason: format!("GQL comparison {op:?} requires a right-hand side"),
    })
}

fn ordered(lhs: &serde_json::Value, rhs: &serde_json::Value) -> GraphResult<std::cmp::Ordering> {
    match (lhs, rhs) {
        (serde_json::Value::Number(lhs), serde_json::Value::Number(rhs)) => order_numbers(lhs, rhs),
        (serde_json::Value::String(lhs), serde_json::Value::String(rhs)) => Ok(lhs.cmp(rhs)),
        _ => Err(non_orderable()),
    }
}

fn non_orderable() -> GraphError {
    GraphError::InvalidFilter {
        reason: "GQL ordered comparisons require both operands to be numbers or strings"
            .to_string(),
    }
}

fn order_numbers(
    lhs: &serde_json::Number,
    rhs: &serde_json::Number,
) -> GraphResult<std::cmp::Ordering> {
    if let (Some(lhs), Some(rhs)) = (lhs.as_i64(), rhs.as_i64()) {
        return Ok(lhs.cmp(&rhs));
    }
    if let (Some(lhs), Some(rhs)) = (lhs.as_u64(), rhs.as_u64()) {
        return Ok(lhs.cmp(&rhs));
    }
    if let (Some(lhs), Some(rhs)) = (lhs.as_i64(), rhs.as_u64()) {
        return Ok(if lhs < 0 {
            std::cmp::Ordering::Less
        } else {
            (lhs as u64).cmp(&rhs)
        });
    }
    if let (Some(lhs), Some(rhs)) = (lhs.as_u64(), rhs.as_i64()) {
        return Ok(if rhs < 0 {
            std::cmp::Ordering::Greater
        } else {
            lhs.cmp(&(rhs as u64))
        });
    }
    let lhs = lhs.as_f64().ok_or_else(non_orderable)?;
    let rhs = rhs.as_f64().ok_or_else(non_orderable)?;
    lhs.partial_cmp(&rhs).ok_or_else(non_orderable)
}

fn project_row(
    row: &GqlRow,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            ReturnSlot::Node { side, name } => {
                output.insert(
                    name.clone(),
                    node_value(
                        coordinate(row, *side),
                        hydrated,
                        label(plan, *side),
                        hydrate_nodes,
                    ),
                );
            }
            ReturnSlot::Property {
                side,
                property,
                name,
            } => {
                output.insert(
                    name.clone(),
                    property_value(coordinate(row, *side), hydrated, property),
                );
            }
        }
    }
    serde_json::Value::Object(output)
}

fn node_value(
    coordinate: &GqlNodeCoordinate,
    hydrated: &HydratedRows,
    label: &str,
    hydrate: bool,
) -> serde_json::Value {
    let mut node = if hydrate {
        hydrated
            .get(&(coordinate.table_oid, coordinate.node_id.clone()))
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    node.insert(
        "_id".to_string(),
        serde_json::json!({
            "table": label,
            "id": coordinate.node_id,
        }),
    );
    node.insert(
        "_labels".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(label.to_string())]),
    );
    serde_json::Value::Object(node)
}

fn property_value(
    coordinate: &GqlNodeCoordinate,
    hydrated: &HydratedRows,
    property: &str,
) -> serde_json::Value {
    hydrated
        .get(&(coordinate.table_oid, coordinate.node_id.clone()))
        .and_then(|row| row.get(property))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn coordinate(row: &GqlRow, side: BindingSide) -> &GqlNodeCoordinate {
    match side {
        BindingSide::Source => &row.source,
        BindingSide::Target => &row.target,
    }
}

fn label(plan: &PhysicalPlan, side: BindingSide) -> &str {
    match side {
        BindingSide::Source => &plan.source_label,
        BindingSide::Target => &plan.target_label,
    }
}
