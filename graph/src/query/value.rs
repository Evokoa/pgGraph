//! JSON value projection and hydrated predicate evaluation for GQL rows.

use std::collections::HashMap;

use crate::safety::{GraphError, GraphResult};

use super::execute::{GqlNodeCoordinate, GqlNodeRow, GqlRow};
use super::logical_plan::{BindingSide, BoundCmpOp, Predicate, SortBindingKey, ValueExpr};
use super::physical_plan::{PhysicalNodeScan, PhysicalPlan, ReturnSlot};

/// Hydrated source rows keyed by graph coordinate.
pub(crate) type HydratedRows = HashMap<(u32, String), serde_json::Value>;

/// Query parameters supplied by SQL callers.
pub(crate) type QueryParams = serde_json::Map<String, serde_json::Value>;

/// Project coordinate matches into canonical JSON rows.
///
/// # Errors
///
/// Returns [`GraphError::GqlParameter`] when a required parameter is missing
/// and [`GraphError::GqlExecution`] when predicate evaluation cannot be
/// completed safely.
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
            projected.push(ProjectedRow {
                row: project_row(&row, plan, hydrated, hydrate_nodes),
                sort_values: sort_values(&row, plan, hydrated, params)?,
            });
        }
    }
    if !plan.order_by.is_empty() {
        projected.sort_by(compare_projected_rows);
    }
    let skip = usize::try_from(plan.skip.unwrap_or(0)).unwrap_or(usize::MAX);
    let limit = plan
        .limit
        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX);
    Ok(projected
        .into_iter()
        .skip(skip)
        .take(limit)
        .map(|row| row.row)
        .collect())
}

/// Project node-only matches into canonical JSON rows.
///
/// # Errors
///
/// Returns [`GraphError::GqlParameter`] when a required parameter is missing
/// and [`GraphError::GqlExecution`] when predicate evaluation cannot be
/// completed safely.
pub(crate) fn project_node_rows(
    rows: Vec<GqlNodeRow>,
    plan: &PhysicalNodeScan,
    hydrated: &HydratedRows,
    params: &QueryParams,
    hydrate_nodes: bool,
) -> GraphResult<Vec<serde_json::Value>> {
    let mut projected = Vec::new();
    for row in rows {
        let fake = node_row_as_gql_row(&row);
        if predicate_matches(plan.predicate.as_ref(), &fake, hydrated, params)? {
            projected.push(ProjectedRow {
                row: project_node_row(&row, plan, hydrated, hydrate_nodes)?,
                sort_values: sort_values_for_node(&row, plan, hydrated, params)?,
            });
        }
    }
    if !plan.order_by.is_empty() {
        projected.sort_by(compare_projected_rows);
    }
    let skip = usize::try_from(plan.skip.unwrap_or(0)).unwrap_or(usize::MAX);
    let limit = plan
        .limit
        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX);
    Ok(projected
        .into_iter()
        .skip(skip)
        .take(limit)
        .map(|row| row.row)
        .collect())
}

/// Return node-only matches that satisfy the plan predicate.
///
/// # Errors
///
/// Returns [`GraphError::GqlParameter`] when a required parameter is missing
/// and [`GraphError::GqlExecution`] when predicate evaluation cannot be
/// completed safely.
pub(crate) fn filter_node_rows(
    rows: Vec<GqlNodeRow>,
    plan: &PhysicalNodeScan,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<Vec<GqlNodeRow>> {
    rows.into_iter()
        .filter_map(|row| {
            let fake = node_row_as_gql_row(&row);
            match predicate_matches(plan.predicate.as_ref(), &fake, hydrated, params) {
                Ok(true) => Some(Ok(row)),
                Ok(false) => None,
                Err(err) => Some(Err(err)),
            }
        })
        .collect()
}

/// Return whether this plan requires SQL row hydration.
pub(crate) fn requires_hydration(plan: &PhysicalPlan, hydrate_nodes: bool) -> bool {
    hydrate_nodes
        || plan.predicate.is_some()
        || !plan.order_by.is_empty()
        || plan
            .returns
            .iter()
            .any(|slot| matches!(slot, ReturnSlot::Property { .. }))
}

/// Return whether this node-scan plan requires SQL row hydration.
pub(crate) fn node_scan_requires_hydration(plan: &PhysicalNodeScan, hydrate_nodes: bool) -> bool {
    hydrate_nodes
        || plan.predicate.is_some()
        || !plan.order_by.is_empty()
        || plan
            .returns
            .iter()
            .any(|slot| matches!(slot, ReturnSlot::Property { .. }))
}

#[derive(Debug)]
struct ProjectedRow {
    row: serde_json::Value,
    sort_values: Vec<SortValue>,
}

#[derive(Debug)]
struct SortValue {
    value: serde_json::Value,
    desc: bool,
}

fn compare_projected_rows(left: &ProjectedRow, right: &ProjectedRow) -> std::cmp::Ordering {
    for (left, right) in left.sort_values.iter().zip(right.sort_values.iter()) {
        let ordering = total_json_order(&left.value, &right.value);
        if !ordering.is_eq() {
            return if left.desc {
                ordering.reverse()
            } else {
                ordering
            };
        }
    }
    std::cmp::Ordering::Equal
}

fn sort_values(
    row: &GqlRow,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<Vec<SortValue>> {
    plan.order_by
        .iter()
        .map(|sort| {
            let value = match &sort.key {
                SortBindingKey::ReturnName(name) => project_row(row, plan, hydrated, true)
                    .get(name)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                SortBindingKey::Property { side, property } => eval_value(
                    &ValueExpr::Property {
                        side: *side,
                        property: property.clone(),
                    },
                    row,
                    hydrated,
                    params,
                )?,
            };
            Ok(SortValue {
                value,
                desc: sort.desc,
            })
        })
        .collect()
}

fn sort_values_for_node(
    row: &GqlNodeRow,
    plan: &PhysicalNodeScan,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<Vec<SortValue>> {
    let fake = node_row_as_gql_row(row);
    plan.order_by
        .iter()
        .map(|sort| {
            let value = match &sort.key {
                SortBindingKey::ReturnName(name) => project_node_row(row, plan, hydrated, true)?
                    .get(name)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                SortBindingKey::Property { side, property } => eval_value(
                    &ValueExpr::Property {
                        side: *side,
                        property: property.clone(),
                    },
                    &fake,
                    hydrated,
                    params,
                )?,
            };
            Ok(SortValue {
                value,
                desc: sort.desc,
            })
        })
        .collect()
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
                .ok_or_else(|| GraphError::GqlParameter {
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
            _ => Err(GraphError::GqlExecution {
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
    rhs.ok_or_else(|| GraphError::GqlExecution {
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

fn total_json_order(lhs: &serde_json::Value, rhs: &serde_json::Value) -> std::cmp::Ordering {
    match ordered(lhs, rhs) {
        Ok(ordering) => ordering,
        Err(_) => json_rank(lhs)
            .cmp(&json_rank(rhs))
            .then_with(|| lhs.to_string().cmp(&rhs.to_string())),
    }
}

fn json_rank(value: &serde_json::Value) -> u8 {
    match value {
        serde_json::Value::Null => 0,
        serde_json::Value::Bool(_) => 1,
        serde_json::Value::Number(_) => 2,
        serde_json::Value::String(_) => 3,
        serde_json::Value::Array(_) => 4,
        serde_json::Value::Object(_) => 5,
    }
}

fn non_orderable() -> GraphError {
    GraphError::GqlExecution {
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
            ReturnSlot::Relationship { name } => {
                output.insert(name.clone(), relationship_value(row, plan));
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

fn project_node_row(
    row: &GqlNodeRow,
    plan: &PhysicalNodeScan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            ReturnSlot::Node { name, .. } => {
                output.insert(
                    name.clone(),
                    node_value(&row.node, hydrated, &plan.label, hydrate_nodes),
                );
            }
            ReturnSlot::Property { property, name, .. } => {
                output.insert(name.clone(), property_value(&row.node, hydrated, property));
            }
            ReturnSlot::Relationship { .. } => {
                return Err(GraphError::GqlExecution {
                    reason: "node-only MATCH cannot return relationship values".to_string(),
                });
            }
        }
    }
    Ok(serde_json::Value::Object(output))
}

fn node_row_as_gql_row(row: &GqlNodeRow) -> GqlRow {
    GqlRow {
        source: row.node.clone(),
        target: row.node.clone(),
        rel_start: row.node.clone(),
        rel_end: row.node.clone(),
    }
}

fn relationship_value(row: &GqlRow, plan: &PhysicalPlan) -> serde_json::Value {
    serde_json::json!({
        "_type": &plan.rel_type,
        "_start": relationship_endpoint(&row.rel_start, plan),
        "_end": relationship_endpoint(&row.rel_end, plan),
    })
}

fn relationship_endpoint(coordinate: &GqlNodeCoordinate, plan: &PhysicalPlan) -> serde_json::Value {
    serde_json::json!({
        "table": label_for_table(plan, coordinate.table_oid),
        "id": &coordinate.node_id,
    })
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

fn label_for_table(plan: &PhysicalPlan, table_oid: u32) -> &str {
    if table_oid == plan.source_table_oid {
        &plan.source_label
    } else {
        &plan.target_label
    }
}
