//! JSON value projection and hydrated predicate evaluation for GQL rows.

use std::collections::{BTreeMap, HashMap};

use crate::safety::{GraphError, GraphResult};

use super::execute::{GqlNodeCoordinate, GqlNodeRow, GqlRow};
use super::logical_plan::{
    AggregateArg, AggregateFunc, BindingSide, BoundCmpOp, Predicate, SortBindingKey, ValueExpr,
};
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
    let rows = collect_projectable_rows(rows, plan, hydrated, params)?;
    if plan.returns.iter().any(ReturnSlot::is_aggregate) {
        let mut projected = aggregate_rows(&rows, &plan.returns, plan, hydrated, hydrate_nodes)?;
        sort_and_window(&mut projected, plan.skip, plan.limit);
        return Ok(projected.into_iter().map(|row| row.row).collect());
    }

    let mut projected = Vec::with_capacity(rows.len());
    for row in rows {
        projected.push(ProjectedRow {
            row: project_row(&row, plan, hydrated, hydrate_nodes),
            sort_values: sort_values(&row, plan, hydrated, params)?,
        });
    }
    sort_and_window(&mut projected, plan.skip, plan.limit);
    Ok(projected.into_iter().map(|row| row.row).collect())
}

fn collect_projectable_rows(
    rows: Vec<GqlRow>,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<Vec<GqlRow>> {
    let mut projectable = Vec::new();
    if plan.optional {
        let mut current_source: Option<GqlNodeCoordinate> = None;
        let mut fallback: Option<GqlRow> = None;
        let mut matched_current_source = false;
        for row in rows {
            if current_source.as_ref() != Some(&row.source) {
                flush_optional_source(
                    &mut projectable,
                    plan,
                    hydrated,
                    params,
                    &mut fallback,
                    matched_current_source,
                )?;
                current_source = Some(row.source.clone());
                matched_current_source = false;
            }
            if fallback.is_none() || row.target.is_none() {
                fallback = Some(row.clone());
            }
            if predicate_matches(plan.predicate.as_ref(), &row, hydrated, params)? {
                projectable.push(row);
                matched_current_source = true;
            }
        }
        flush_optional_source(
            &mut projectable,
            plan,
            hydrated,
            params,
            &mut fallback,
            matched_current_source,
        )?;
    } else {
        for row in rows {
            if predicate_matches(plan.predicate.as_ref(), &row, hydrated, params)? {
                projectable.push(row);
            }
        }
    }
    Ok(projectable)
}

fn flush_optional_source(
    projectable: &mut Vec<GqlRow>,
    _plan: &PhysicalPlan,
    _hydrated: &HydratedRows,
    _params: &QueryParams,
    fallback: &mut Option<GqlRow>,
    matched_current_source: bool,
) -> GraphResult<()> {
    let Some(row) = fallback.take() else {
        return Ok(());
    };
    if !matched_current_source {
        let null_row = GqlRow {
            source: row.source,
            target: None,
            rel_start: None,
            rel_end: None,
        };
        projectable.push(null_row);
    }
    Ok(())
}

/// Project node-only matches into canonical JSON rows.
///
/// # Errors
///
/// Returns [`GraphError::GqlParameter`] when a required parameter is missing
/// and [`GraphError::GqlExecution`] when predicate evaluation cannot be
/// completed safely.
pub(crate) fn project_node_rows(
    input_rows: Vec<GqlNodeRow>,
    plan: &PhysicalNodeScan,
    hydrated: &HydratedRows,
    params: &QueryParams,
    hydrate_nodes: bool,
) -> GraphResult<Vec<serde_json::Value>> {
    let mut rows = Vec::new();
    for row in input_rows {
        let fake = node_row_as_gql_row(&row);
        if predicate_matches(plan.predicate.as_ref(), &fake, hydrated, params)? {
            rows.push(row);
        }
    }
    if plan.returns.iter().any(ReturnSlot::is_aggregate) {
        let mut projected =
            aggregate_node_rows(&rows, &plan.returns, plan, hydrated, hydrate_nodes)?;
        sort_and_window(&mut projected, plan.skip, plan.limit);
        return Ok(projected.into_iter().map(|row| row.row).collect());
    }

    let mut projected = Vec::with_capacity(rows.len());
    for row in rows {
        projected.push(ProjectedRow {
            row: project_node_row(&row, plan, hydrated, hydrate_nodes)?,
            sort_values: sort_values_for_node(&row, plan, hydrated, params)?,
        });
    }
    sort_and_window(&mut projected, plan.skip, plan.limit);
    Ok(projected.into_iter().map(|row| row.row).collect())
}

fn sort_and_window(projected: &mut Vec<ProjectedRow>, skip: Option<u64>, limit: Option<u64>) {
    if projected
        .first()
        .is_some_and(|row| !row.sort_values.is_empty())
    {
        projected.sort_by(compare_projected_rows);
    }
    let skip = usize::try_from(skip.unwrap_or(0)).unwrap_or(usize::MAX);
    let limit = limit
        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX);
    if skip > 0 {
        let drain = skip.min(projected.len());
        projected.drain(0..drain);
    }
    projected.truncate(limit.min(projected.len()));
}

fn aggregate_rows(
    rows: &[GqlRow],
    returns: &[ReturnSlot],
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<Vec<ProjectedRow>> {
    aggregate_by(
        rows,
        returns,
        |row, slot| project_slot_value(row, slot, plan, hydrated, hydrate_nodes),
        |row, arg| aggregate_arg_value(row, arg, plan, hydrated, hydrate_nodes),
        |output| aggregate_sort_values(output, plan.order_by.as_slice()),
    )
}

fn aggregate_node_rows(
    rows: &[GqlNodeRow],
    returns: &[ReturnSlot],
    plan: &PhysicalNodeScan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<Vec<ProjectedRow>> {
    aggregate_by(
        rows,
        returns,
        |row, slot| project_node_slot_value(row, slot, plan, hydrated, hydrate_nodes),
        |row, arg| {
            let fake = node_row_as_gql_row(row);
            aggregate_arg_value_for_node(&fake, arg, &plan.label, hydrated, hydrate_nodes)
        },
        |output| aggregate_sort_values(output, plan.order_by.as_slice()),
    )
}

fn aggregate_by<Row, ProjectValue, AggregateValue, SortValues>(
    rows: &[Row],
    returns: &[ReturnSlot],
    project_value: ProjectValue,
    aggregate_value: AggregateValue,
    sort_values: SortValues,
) -> GraphResult<Vec<ProjectedRow>>
where
    ProjectValue: Fn(&Row, &ReturnSlot) -> GraphResult<serde_json::Value>,
    AggregateValue: Fn(&Row, &AggregateArg) -> GraphResult<serde_json::Value>,
    SortValues: Fn(&serde_json::Value) -> Vec<SortValue>,
{
    let group_slots: Vec<&ReturnSlot> =
        returns.iter().filter(|slot| !slot.is_aggregate()).collect();
    let aggregate_slots: Vec<&ReturnSlot> =
        returns.iter().filter(|slot| slot.is_aggregate()).collect();
    let mut groups: BTreeMap<String, AggregateGroup> = BTreeMap::new();

    for row in rows {
        let group_values = group_slots
            .iter()
            .map(|slot| project_value(row, slot))
            .collect::<GraphResult<Vec<_>>>()?;
        let key = group_values
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\u{1f}");
        let group = groups.entry(key).or_insert_with(|| AggregateGroup {
            group_values,
            states: aggregate_slots
                .iter()
                .map(|slot| AggregateState::new(slot))
                .collect(),
        });
        for (state, slot) in group.states.iter_mut().zip(aggregate_slots.iter()) {
            let ReturnSlot::Aggregate { arg, .. } = slot else {
                continue;
            };
            state.accumulate(aggregate_value(row, arg)?)?;
        }
    }

    if rows.is_empty() && group_slots.is_empty() {
        groups.insert(
            String::new(),
            AggregateGroup {
                group_values: Vec::new(),
                states: aggregate_slots
                    .iter()
                    .map(|slot| AggregateState::new(slot))
                    .collect(),
            },
        );
    }

    groups
        .into_values()
        .map(|group| {
            let mut output = serde_json::Map::new();
            let mut group_index = 0;
            let mut aggregate_index = 0;
            for slot in returns {
                if slot.is_aggregate() {
                    output.insert(
                        slot.name().to_string(),
                        group.states[aggregate_index].finish()?,
                    );
                    aggregate_index += 1;
                } else {
                    output.insert(
                        slot.name().to_string(),
                        group.group_values[group_index].clone(),
                    );
                    group_index += 1;
                }
            }
            let row = serde_json::Value::Object(output);
            Ok(ProjectedRow {
                sort_values: sort_values(&row),
                row,
            })
        })
        .collect()
}

struct AggregateGroup {
    group_values: Vec<serde_json::Value>,
    states: Vec<AggregateState>,
}

#[derive(Debug)]
enum AggregateState {
    Count { count: u64 },
    Sum { sum: f64, seen: bool },
    Avg { sum: f64, count: u64 },
    Min { value: Option<serde_json::Value> },
    Max { value: Option<serde_json::Value> },
    Collect { values: Vec<serde_json::Value> },
}

impl AggregateState {
    fn new(slot: &ReturnSlot) -> Self {
        match slot {
            ReturnSlot::Aggregate {
                func: AggregateFunc::Count,
                ..
            } => Self::Count { count: 0 },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Sum,
                ..
            } => Self::Sum {
                sum: 0.0,
                seen: false,
            },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Avg,
                ..
            } => Self::Avg { sum: 0.0, count: 0 },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Min,
                ..
            } => Self::Min { value: None },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Max,
                ..
            } => Self::Max { value: None },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Collect,
                ..
            } => Self::Collect { values: Vec::new() },
            _ => unreachable!("aggregate state requires aggregate slot"),
        }
    }

    fn accumulate(&mut self, value: serde_json::Value) -> GraphResult<()> {
        match self {
            Self::Count { count } => {
                if !value.is_null() {
                    *count += 1;
                }
                Ok(())
            }
            Self::Sum { sum, seen } => {
                if let Some(number) = numeric_value(&value)? {
                    *sum += number;
                    *seen = true;
                }
                Ok(())
            }
            Self::Avg { sum, count } => {
                if let Some(number) = numeric_value(&value)? {
                    *sum += number;
                    *count += 1;
                }
                Ok(())
            }
            Self::Min { value: current } => update_extreme(current, value, false),
            Self::Max { value: current } => update_extreme(current, value, true),
            Self::Collect { values } => {
                values.push(value);
                Ok(())
            }
        }
    }

    fn finish(&self) -> GraphResult<serde_json::Value> {
        match self {
            Self::Count { count } => Ok(serde_json::Value::from(*count)),
            Self::Sum { sum, seen } => number_or_null(*sum, *seen),
            Self::Avg { sum, count } => number_or_null(*sum / (*count as f64), *count > 0),
            Self::Min { value } | Self::Max { value } => {
                Ok(value.clone().unwrap_or(serde_json::Value::Null))
            }
            Self::Collect { values } => Ok(serde_json::Value::Array(values.clone())),
        }
    }
}

fn numeric_value(value: &serde_json::Value) -> GraphResult<Option<f64>> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_f64()
        .map(Some)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: "GQL numeric aggregates require number inputs".to_string(),
        })
}

fn number_or_null(value: f64, seen: bool) -> GraphResult<serde_json::Value> {
    if !seen {
        return Ok(serde_json::Value::Null);
    }
    serde_json::Number::from_f64(value)
        .map(serde_json::Value::Number)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: "GQL aggregate produced a non-finite number".to_string(),
        })
}

fn update_extreme(
    current: &mut Option<serde_json::Value>,
    value: serde_json::Value,
    choose_max: bool,
) -> GraphResult<()> {
    if value.is_null() {
        return Ok(());
    }
    match current {
        Some(existing) => {
            let ordering = ordered(&value, existing)?;
            if (choose_max && ordering.is_gt()) || (!choose_max && ordering.is_lt()) {
                *existing = value;
            }
        }
        None => *current = Some(value),
    }
    Ok(())
}

fn aggregate_sort_values(
    output: &serde_json::Value,
    order_by: &[super::logical_plan::SortBinding],
) -> Vec<SortValue> {
    order_by
        .iter()
        .map(|sort| {
            let value = match &sort.key {
                SortBindingKey::ReturnName(name) => {
                    output.get(name).cloned().unwrap_or(serde_json::Value::Null)
                }
                SortBindingKey::Property { .. } => serde_json::Value::Null,
            };
            SortValue {
                value,
                desc: sort.desc,
            }
        })
        .collect()
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

/// Return relationship matches that satisfy the plan predicate.
///
/// # Errors
///
/// Returns [`GraphError::GqlParameter`] when a required parameter is missing
/// and [`GraphError::GqlExecution`] when predicate evaluation cannot be
/// completed safely.
pub(crate) fn filter_rows(
    rows: Vec<GqlRow>,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<Vec<GqlRow>> {
    rows.into_iter()
        .filter_map(|row| {
            match predicate_matches(plan.predicate.as_ref(), &row, hydrated, params) {
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
        || plan.returns.iter().any(return_slot_requires_hydration)
}

/// Return whether this node-scan plan requires SQL row hydration.
pub(crate) fn node_scan_requires_hydration(plan: &PhysicalNodeScan, hydrate_nodes: bool) -> bool {
    hydrate_nodes
        || plan.predicate.is_some()
        || !plan.order_by.is_empty()
        || plan.returns.iter().any(return_slot_requires_hydration)
}

fn return_slot_requires_hydration(slot: &ReturnSlot) -> bool {
    matches!(
        slot,
        ReturnSlot::Property { .. }
            | ReturnSlot::Aggregate {
                arg: AggregateArg::Property { .. },
                ..
            }
    )
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
        ValueExpr::Property { side, property } => Ok(coordinate(row, *side)
            .map(|coordinate| property_value(coordinate, hydrated, property))
            .unwrap_or(serde_json::Value::Null)),
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
                let value = coordinate(row, *side)
                    .map(|coordinate| {
                        node_value(coordinate, hydrated, label(plan, *side), hydrate_nodes)
                    })
                    .unwrap_or(serde_json::Value::Null);
                output.insert(name.clone(), value);
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
                    coordinate(row, *side)
                        .map(|coordinate| property_value(coordinate, hydrated, property))
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            ReturnSlot::Aggregate { name, .. } => {
                output.insert(name.clone(), serde_json::Value::Null);
            }
        }
    }
    serde_json::Value::Object(output)
}

fn project_slot_value(
    row: &GqlRow,
    slot: &ReturnSlot,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    match slot {
        ReturnSlot::Node { side, .. } => Ok(coordinate(row, *side)
            .map(|coordinate| node_value(coordinate, hydrated, label(plan, *side), hydrate_nodes))
            .unwrap_or(serde_json::Value::Null)),
        ReturnSlot::Relationship { .. } => Ok(relationship_value(row, plan)),
        ReturnSlot::Property { side, property, .. } => Ok(coordinate(row, *side)
            .map(|coordinate| property_value(coordinate, hydrated, property))
            .unwrap_or(serde_json::Value::Null)),
        ReturnSlot::Aggregate { .. } => Err(GraphError::GqlExecution {
            reason: "aggregate slots cannot be used as grouping values".to_string(),
        }),
    }
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
            ReturnSlot::Aggregate { name, .. } => {
                output.insert(name.clone(), serde_json::Value::Null);
            }
        }
    }
    Ok(serde_json::Value::Object(output))
}

fn project_node_slot_value(
    row: &GqlNodeRow,
    slot: &ReturnSlot,
    plan: &PhysicalNodeScan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    match slot {
        ReturnSlot::Node { .. } => Ok(node_value(&row.node, hydrated, &plan.label, hydrate_nodes)),
        ReturnSlot::Property { property, .. } => Ok(property_value(&row.node, hydrated, property)),
        ReturnSlot::Relationship { .. } => Err(GraphError::GqlExecution {
            reason: "node-only MATCH cannot group by relationship values".to_string(),
        }),
        ReturnSlot::Aggregate { .. } => Err(GraphError::GqlExecution {
            reason: "aggregate slots cannot be used as grouping values".to_string(),
        }),
    }
}

fn aggregate_arg_value(
    row: &GqlRow,
    arg: &AggregateArg,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    match arg {
        AggregateArg::All => Ok(serde_json::Value::Bool(true)),
        AggregateArg::Node(side) => Ok(coordinate(row, *side)
            .map(|coordinate| node_value(coordinate, hydrated, label(plan, *side), hydrate_nodes))
            .unwrap_or(serde_json::Value::Null)),
        AggregateArg::Relationship => Ok(relationship_value(row, plan)),
        AggregateArg::Property { side, property } => Ok(coordinate(row, *side)
            .map(|coordinate| property_value(coordinate, hydrated, property))
            .unwrap_or(serde_json::Value::Null)),
    }
}

fn aggregate_arg_value_for_node(
    row: &GqlRow,
    arg: &AggregateArg,
    label: &str,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    match arg {
        AggregateArg::All => Ok(serde_json::Value::Bool(true)),
        AggregateArg::Node(_) => Ok(node_value(&row.source, hydrated, label, hydrate_nodes)),
        AggregateArg::Property { property, .. } => {
            Ok(property_value(&row.source, hydrated, property))
        }
        AggregateArg::Relationship => Err(GraphError::GqlExecution {
            reason: "node-only MATCH cannot aggregate relationship values".to_string(),
        }),
    }
}

fn node_row_as_gql_row(row: &GqlNodeRow) -> GqlRow {
    GqlRow {
        source: row.node.clone(),
        target: Some(row.node.clone()),
        rel_start: Some(row.node.clone()),
        rel_end: Some(row.node.clone()),
    }
}

fn relationship_value(row: &GqlRow, plan: &PhysicalPlan) -> serde_json::Value {
    let (Some(rel_start), Some(rel_end)) = (&row.rel_start, &row.rel_end) else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "_type": &plan.rel_type,
        "_start": relationship_endpoint(rel_start, plan),
        "_end": relationship_endpoint(rel_end, plan),
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

fn coordinate(row: &GqlRow, side: BindingSide) -> Option<&GqlNodeCoordinate> {
    match side {
        BindingSide::Source => Some(&row.source),
        BindingSide::Target => row.target.as_ref(),
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
