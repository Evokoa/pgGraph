//! JSON value projection and hydrated predicate evaluation for GQL rows.

use std::collections::{btree_map::Entry, BTreeMap, HashMap, HashSet};

use crate::safety::{GraphError, GraphResult};

use super::execute::{GqlNodeCoordinate, GqlNodeRow, GqlRow};
use super::logical_plan::{
    AggregateArg, AggregateFunc, BindingSide, BoundCmpOp, PathFunc, Predicate, SortBindingKey,
    ValueExpr,
};
use super::physical_plan::{
    PhysicalJoinPlan, PhysicalNodeScan, PhysicalPlan, PhysicalWildcardPathPlan, ReturnSlot,
    MAX_GQL_DISTINCT_KEYS,
};

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
    let mut rows = collect_projectable_rows(rows, plan, hydrated, params)?;
    apply_distinct_stages_to_rows(&mut rows, plan, hydrated, hydrate_nodes)?;
    if plan.returns.iter().any(ReturnSlot::is_aggregate) {
        let mut projected = aggregate_rows(&rows, &plan.returns, plan, hydrated, hydrate_nodes)?;
        if plan.distinct {
            dedup_projected_rows(&mut projected)?;
        }
        sort_and_window(&mut projected, plan.skip, plan.limit);
        return Ok(projected.into_iter().map(|row| row.row).collect());
    }

    let mut projected = Vec::with_capacity(rows.len());
    for row in rows {
        projected.push(ProjectedRow {
            row: project_row(&row, plan, hydrated, hydrate_nodes)?,
            sort_values: sort_values(&row, plan, hydrated, params)?,
        });
    }
    if plan.distinct {
        dedup_projected_rows(&mut projected)?;
    }
    sort_and_window(&mut projected, plan.skip, plan.limit);
    Ok(projected.into_iter().map(|row| row.row).collect())
}

/// Project wildcard path matches into canonical JSON rows.
///
/// # Errors
///
/// Returns [`GraphError::GqlExecution`] when a wildcard path references table
/// metadata absent from the physical plan or hydrated row lookup.
pub(crate) fn project_wildcard_path_rows(
    rows: Vec<GqlRow>,
    plan: &PhysicalWildcardPathPlan,
    hydrated: &HydratedRows,
    params: &QueryParams,
    hydrate_nodes: bool,
) -> GraphResult<Vec<serde_json::Value>> {
    let mut projected = Vec::with_capacity(rows.len());
    for row in rows {
        if predicate_matches(plan.predicate.as_ref(), &row, hydrated, params)? {
            projected.push(project_wildcard_path_row(
                &row,
                plan,
                hydrated,
                hydrate_nodes,
            )?);
        }
    }
    apply_value_window(&mut projected, plan.skip, plan.limit);
    Ok(projected)
}

/// Project multi-pattern join matches into canonical JSON rows.
///
/// # Errors
///
/// Returns [`GraphError::GqlExecution`] when a joined row is missing a required
/// node slot or hydration needed for a projected value is absent.
pub(crate) fn project_join_rows(
    rows: Vec<GqlRow>,
    plan: &PhysicalJoinPlan,
    hydrated: &HydratedRows,
    params: &QueryParams,
    hydrate_nodes: bool,
) -> GraphResult<Vec<serde_json::Value>> {
    let mut rows = rows
        .into_iter()
        .filter_map(|row| {
            match predicate_matches(plan.predicate.as_ref(), &row, hydrated, params) {
                Ok(true) => Some(Ok(row)),
                Ok(false) => None,
                Err(err) => Some(Err(err)),
            }
        })
        .collect::<GraphResult<Vec<_>>>()?;
    apply_distinct_stages_to_join_rows(&mut rows, plan, hydrated, hydrate_nodes)?;
    if plan.returns.iter().any(ReturnSlot::is_aggregate) {
        let mut projected =
            aggregate_join_rows(&rows, &plan.returns, plan, hydrated, hydrate_nodes)?;
        if plan.distinct {
            dedup_projected_rows(&mut projected)?;
        }
        sort_and_window(&mut projected, plan.skip, plan.limit);
        return Ok(projected.into_iter().map(|row| row.row).collect());
    }

    let mut projected = Vec::with_capacity(rows.len());
    for row in rows {
        projected.push(ProjectedRow {
            row: project_join_row(&row, plan, hydrated, hydrate_nodes)?,
            sort_values: sort_values_for_join(&row, plan, hydrated, params)?,
        });
    }
    if plan.distinct {
        dedup_projected_rows(&mut projected)?;
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
            path_nodes: Vec::new(),
            path_relationships: Vec::new(),
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
    apply_distinct_stages_to_node_rows(&mut rows, plan, hydrated, hydrate_nodes)?;
    if plan.returns.iter().any(ReturnSlot::is_aggregate) {
        let mut projected =
            aggregate_node_rows(&rows, &plan.returns, plan, hydrated, hydrate_nodes)?;
        if plan.distinct {
            dedup_projected_rows(&mut projected)?;
        }
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
    if plan.distinct {
        dedup_projected_rows(&mut projected)?;
    }
    sort_and_window(&mut projected, plan.skip, plan.limit);
    Ok(projected.into_iter().map(|row| row.row).collect())
}

fn apply_distinct_stages_to_rows(
    rows: &mut Vec<GqlRow>,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<()> {
    for stage in &plan.distinct_stages {
        dedup_by_slots(rows, stage, |row, slot| {
            project_slot_value(row, slot, plan, hydrated, hydrate_nodes)
        })?;
    }
    Ok(())
}

fn apply_distinct_stages_to_node_rows(
    rows: &mut Vec<GqlNodeRow>,
    plan: &PhysicalNodeScan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<()> {
    for stage in &plan.distinct_stages {
        dedup_by_slots(rows, stage, |row, slot| {
            project_node_slot_value(row, slot, plan, hydrated, hydrate_nodes)
        })?;
    }
    Ok(())
}

fn apply_distinct_stages_to_join_rows(
    rows: &mut Vec<GqlRow>,
    plan: &PhysicalJoinPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<()> {
    for stage in &plan.distinct_stages {
        dedup_by_slots(rows, stage, |row, slot| {
            project_join_slot_value(row, slot, plan, hydrated, hydrate_nodes)
        })?;
    }
    Ok(())
}

fn dedup_by_slots<Row, KeyValue>(
    rows: &mut Vec<Row>,
    slots: &[ReturnSlot],
    key_value: KeyValue,
) -> GraphResult<()>
where
    KeyValue: Fn(&Row, &ReturnSlot) -> GraphResult<serde_json::Value>,
{
    let mut seen = HashSet::with_capacity(rows.len().min(MAX_GQL_DISTINCT_KEYS));
    let mut deduped = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        let key_values = slots
            .iter()
            .map(|slot| key_value(&row, slot))
            .collect::<GraphResult<Vec<_>>>()?;
        if insert_distinct_key(&mut seen, serde_json::Value::Array(key_values))? {
            deduped.push(row);
        }
    }
    *rows = deduped;
    Ok(())
}

fn dedup_projected_rows(projected: &mut Vec<ProjectedRow>) -> GraphResult<()> {
    let mut seen = HashSet::with_capacity(projected.len().min(MAX_GQL_DISTINCT_KEYS));
    let mut deduped = Vec::with_capacity(projected.len());
    for row in projected.drain(..) {
        if insert_distinct_key(&mut seen, row.row.clone())? {
            deduped.push(row);
        }
    }
    *projected = deduped;
    Ok(())
}

fn insert_distinct_key(seen: &mut HashSet<String>, value: serde_json::Value) -> GraphResult<bool> {
    let key = value.to_string();
    if seen.contains(&key) {
        return Ok(false);
    }
    if seen.len() >= MAX_GQL_DISTINCT_KEYS {
        return Err(GraphError::GqlExecution {
            reason: format!("GQL DISTINCT key cap exceeded ({MAX_GQL_DISTINCT_KEYS})"),
        });
    }
    seen.insert(key);
    Ok(true)
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

fn apply_value_window(values: &mut Vec<serde_json::Value>, skip: Option<u64>, limit: Option<u64>) {
    let skip = usize::try_from(skip.unwrap_or(0)).unwrap_or(usize::MAX);
    let limit = limit
        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX);
    if skip > 0 {
        let drain = skip.min(values.len());
        values.drain(0..drain);
    }
    values.truncate(limit.min(values.len()));
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

fn aggregate_join_rows(
    rows: &[GqlRow],
    returns: &[ReturnSlot],
    plan: &PhysicalJoinPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<Vec<ProjectedRow>> {
    aggregate_by(
        rows,
        returns,
        |row, slot| project_join_slot_value(row, slot, plan, hydrated, hydrate_nodes),
        |row, arg| aggregate_arg_value_for_join(row, arg, plan, hydrated, hydrate_nodes),
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
        let group = match groups.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let states = aggregate_slots
                    .iter()
                    .map(|slot| AggregateState::new(slot))
                    .collect::<GraphResult<Vec<_>>>()?;
                entry.insert(AggregateGroup {
                    group_values,
                    states,
                })
            }
        };
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
                    .collect::<GraphResult<Vec<_>>>()?,
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
    Count {
        count: u64,
        distinct: Option<HashSet<String>>,
    },
    Sum {
        sum: f64,
        seen: bool,
        distinct: Option<HashSet<String>>,
    },
    Avg {
        sum: f64,
        count: u64,
        distinct: Option<HashSet<String>>,
    },
    Min {
        value: Option<serde_json::Value>,
        distinct: Option<HashSet<String>>,
    },
    Max {
        value: Option<serde_json::Value>,
        distinct: Option<HashSet<String>>,
    },
    Collect {
        values: Vec<serde_json::Value>,
        distinct: Option<HashSet<String>>,
    },
}

impl AggregateState {
    fn new(slot: &ReturnSlot) -> GraphResult<Self> {
        let state = match slot {
            ReturnSlot::Aggregate {
                func: AggregateFunc::Count,
                distinct,
                ..
            } => Self::Count {
                count: 0,
                distinct: distinct_set(*distinct),
            },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Sum,
                distinct,
                ..
            } => Self::Sum {
                sum: 0.0,
                seen: false,
                distinct: distinct_set(*distinct),
            },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Avg,
                distinct,
                ..
            } => Self::Avg {
                sum: 0.0,
                count: 0,
                distinct: distinct_set(*distinct),
            },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Min,
                distinct,
                ..
            } => Self::Min {
                value: None,
                distinct: distinct_set(*distinct),
            },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Max,
                distinct,
                ..
            } => Self::Max {
                value: None,
                distinct: distinct_set(*distinct),
            },
            ReturnSlot::Aggregate {
                func: AggregateFunc::Collect,
                distinct,
                ..
            } => Self::Collect {
                values: Vec::new(),
                distinct: distinct_set(*distinct),
            },
            _ => {
                return Err(GraphError::Internal(
                    "aggregate state requires aggregate return slot".to_string(),
                ));
            }
        };
        Ok(state)
    }

    fn accumulate(&mut self, value: serde_json::Value) -> GraphResult<()> {
        match self {
            Self::Count { count, distinct } => {
                if accept_distinct_aggregate_value(distinct, &value, true)? {
                    *count += 1;
                }
                Ok(())
            }
            Self::Sum {
                sum,
                seen,
                distinct,
            } => {
                if accept_distinct_aggregate_value(distinct, &value, true)? {
                    let Some(number) = numeric_value(&value)? else {
                        return Ok(());
                    };
                    *sum += number;
                    *seen = true;
                }
                Ok(())
            }
            Self::Avg {
                sum,
                count,
                distinct,
            } => {
                if accept_distinct_aggregate_value(distinct, &value, true)? {
                    let Some(number) = numeric_value(&value)? else {
                        return Ok(());
                    };
                    *sum += number;
                    *count += 1;
                }
                Ok(())
            }
            Self::Min {
                value: current,
                distinct,
            } => {
                if accept_distinct_aggregate_value(distinct, &value, true)? {
                    update_extreme(current, value, false)?;
                }
                Ok(())
            }
            Self::Max {
                value: current,
                distinct,
            } => {
                if accept_distinct_aggregate_value(distinct, &value, true)? {
                    update_extreme(current, value, true)?;
                }
                Ok(())
            }
            Self::Collect { values, distinct } => {
                if accept_distinct_aggregate_value(distinct, &value, true)? {
                    values.push(value);
                }
                Ok(())
            }
        }
    }

    fn finish(&self) -> GraphResult<serde_json::Value> {
        match self {
            Self::Count { count, .. } => Ok(serde_json::Value::from(*count)),
            Self::Sum { sum, seen, .. } => number_or_null(*sum, *seen),
            Self::Avg { sum, count, .. } => number_or_null(*sum / (*count as f64), *count > 0),
            Self::Min { value, .. } | Self::Max { value, .. } => {
                Ok(value.clone().unwrap_or(serde_json::Value::Null))
            }
            Self::Collect { values, .. } => Ok(serde_json::Value::Array(values.clone())),
        }
    }
}

fn distinct_set(enabled: bool) -> Option<HashSet<String>> {
    enabled.then(HashSet::new)
}

fn accept_distinct_aggregate_value(
    distinct: &mut Option<HashSet<String>>,
    value: &serde_json::Value,
    skip_null: bool,
) -> GraphResult<bool> {
    if skip_null && value.is_null() {
        return Ok(false);
    }
    let Some(seen) = distinct else {
        return Ok(true);
    };
    insert_distinct_key(seen, value.clone())
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
        || plan
            .distinct_stages
            .iter()
            .flatten()
            .any(return_slot_requires_hydration)
}

/// Return whether this node-scan plan requires SQL row hydration.
pub(crate) fn node_scan_requires_hydration(plan: &PhysicalNodeScan, hydrate_nodes: bool) -> bool {
    hydrate_nodes
        || plan.predicate.is_some()
        || !plan.order_by.is_empty()
        || plan.returns.iter().any(return_slot_requires_hydration)
        || plan
            .distinct_stages
            .iter()
            .flatten()
            .any(return_slot_requires_hydration)
}

/// Return whether this wildcard path plan requires SQL row hydration.
pub(crate) fn wildcard_path_requires_hydration(
    plan: &PhysicalWildcardPathPlan,
    hydrate_nodes: bool,
) -> bool {
    plan.predicate.is_some()
        || (hydrate_nodes
            && plan.returns.iter().any(|slot| {
                matches!(
                    slot,
                    ReturnSlot::Node { .. }
                        | ReturnSlot::Path { .. }
                        | ReturnSlot::PathFunction {
                            func: PathFunc::Nodes,
                            ..
                        }
                )
            }))
}

/// Return whether this multi-pattern join plan requires SQL row hydration.
pub(crate) fn join_requires_hydration(plan: &PhysicalJoinPlan, hydrate_nodes: bool) -> bool {
    hydrate_nodes
        || plan.returns.iter().any(return_slot_requires_hydration)
        || plan
            .distinct_stages
            .iter()
            .flatten()
            .any(return_slot_requires_hydration)
        || plan.order_by.iter().any(sort_binding_requires_hydration)
        || plan
            .predicate
            .as_ref()
            .is_some_and(predicate_requires_hydration)
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

fn sort_binding_requires_hydration(sort: &super::logical_plan::SortBinding) -> bool {
    matches!(sort.key, SortBindingKey::Property { .. })
}

fn predicate_requires_hydration(predicate: &Predicate) -> bool {
    match predicate {
        Predicate::And(lhs, rhs) | Predicate::Or(lhs, rhs) => {
            predicate_requires_hydration(lhs) || predicate_requires_hydration(rhs)
        }
        Predicate::Not(expr) => predicate_requires_hydration(expr),
        Predicate::Compare { lhs, rhs, .. } => {
            value_expr_requires_hydration(lhs)
                || rhs.as_ref().is_some_and(value_expr_requires_hydration)
        }
    }
}

fn value_expr_requires_hydration(expr: &ValueExpr) -> bool {
    matches!(expr, ValueExpr::Property { .. })
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

#[derive(Debug, Clone, PartialEq)]
enum EvalValue {
    Value(serde_json::Value),
    Missing,
}

impl EvalValue {
    fn into_json_or_null(self) -> serde_json::Value {
        match self {
            Self::Value(value) => value,
            Self::Missing => serde_json::Value::Null,
        }
    }
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
                SortBindingKey::ReturnName(name) => plan
                    .returns
                    .iter()
                    .find(|slot| slot.name() == name)
                    .map(|slot| project_slot_value(row, slot, plan, hydrated, true))
                    .transpose()?
                    .unwrap_or(serde_json::Value::Null),
                SortBindingKey::Property { side, property } => eval_value(
                    &ValueExpr::Property {
                        side: *side,
                        property: property.clone(),
                    },
                    row,
                    hydrated,
                    params,
                )?
                .into_json_or_null(),
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
                SortBindingKey::ReturnName(name) => plan
                    .returns
                    .iter()
                    .find(|slot| slot.name() == name)
                    .map(|slot| project_node_slot_value(row, slot, plan, hydrated, true))
                    .transpose()?
                    .unwrap_or(serde_json::Value::Null),
                SortBindingKey::Property { side, property } => eval_value(
                    &ValueExpr::Property {
                        side: *side,
                        property: property.clone(),
                    },
                    &fake,
                    hydrated,
                    params,
                )?
                .into_json_or_null(),
            };
            Ok(SortValue {
                value,
                desc: sort.desc,
            })
        })
        .collect()
}

fn sort_values_for_join(
    row: &GqlRow,
    plan: &PhysicalJoinPlan,
    hydrated: &HydratedRows,
    params: &QueryParams,
) -> GraphResult<Vec<SortValue>> {
    plan.order_by
        .iter()
        .map(|sort| {
            let value = match &sort.key {
                SortBindingKey::ReturnName(name) => plan
                    .returns
                    .iter()
                    .find(|slot| slot.name() == name)
                    .map(|slot| project_join_slot_value(row, slot, plan, hydrated, true))
                    .transpose()?
                    .unwrap_or(serde_json::Value::Null),
                SortBindingKey::Property { side, property } => eval_value(
                    &ValueExpr::Property {
                        side: *side,
                        property: property.clone(),
                    },
                    row,
                    hydrated,
                    params,
                )?
                .into_json_or_null(),
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
) -> GraphResult<EvalValue> {
    match expr {
        ValueExpr::Property { side, property } => coordinate(row, *side)
            .map(|coordinate| lookup_property_value(coordinate, hydrated, property))
            .unwrap_or(Ok(EvalValue::Value(serde_json::Value::Null))),
        ValueExpr::Literal(value) => Ok(EvalValue::Value(value.clone())),
        ValueExpr::Param(name) => {
            params
                .get(name)
                .cloned()
                .map(EvalValue::Value)
                .ok_or_else(|| GraphError::GqlParameter {
                    reason: format!("missing GQL parameter `{name}`"),
                })
        }
        ValueExpr::List(values) => Ok(EvalValue::Value(serde_json::Value::Array(values.clone()))),
    }
}

fn compare_values(lhs: &EvalValue, op: BoundCmpOp, rhs: Option<&EvalValue>) -> GraphResult<bool> {
    if matches!(lhs, EvalValue::Missing) || matches!(rhs, Some(EvalValue::Missing)) {
        return Ok(false);
    }
    let EvalValue::Value(lhs) = lhs else {
        return Err(GraphError::Internal(
            "GQL comparison received non-value left-hand side".to_string(),
        ));
    };
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

fn required_rhs(op: BoundCmpOp, rhs: Option<&EvalValue>) -> GraphResult<&serde_json::Value> {
    let value = rhs.ok_or_else(|| GraphError::GqlExecution {
        reason: format!("GQL comparison {op:?} requires a right-hand side"),
    })?;
    let EvalValue::Value(value) = value else {
        return Err(GraphError::Internal(
            "GQL comparison received non-value right-hand side".to_string(),
        ));
    };
    Ok(value)
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
) -> GraphResult<serde_json::Value> {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            ReturnSlot::Node { side, name } => {
                let value = coordinate(row, *side)
                    .map(|coordinate| {
                        node_value(coordinate, hydrated, label(plan, *side), hydrate_nodes)
                    })
                    .transpose()?
                    .unwrap_or(serde_json::Value::Null);
                output.insert(name.clone(), value);
            }
            ReturnSlot::Relationship { name } => {
                output.insert(name.clone(), relationship_value(row, plan));
            }
            ReturnSlot::Path { name } => {
                output.insert(
                    name.clone(),
                    path_value(row, plan, hydrated, hydrate_nodes)?,
                );
            }
            ReturnSlot::PathFunction { func, name, .. } => {
                output.insert(
                    name.clone(),
                    path_function_value(*func, row, plan, hydrated, hydrate_nodes)?,
                );
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
                        .transpose()?
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            ReturnSlot::Aggregate { name, .. } => {
                output.insert(name.clone(), serde_json::Value::Null);
            }
        }
    }
    Ok(serde_json::Value::Object(output))
}

fn project_wildcard_path_row(
    row: &GqlRow,
    plan: &PhysicalWildcardPathPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            ReturnSlot::Path { name } => {
                output.insert(
                    name.clone(),
                    wildcard_path_value(row, plan, hydrated, hydrate_nodes)?,
                );
            }
            ReturnSlot::PathFunction { func, name, .. } => {
                output.insert(
                    name.clone(),
                    wildcard_path_function_value(*func, row, plan, hydrated, hydrate_nodes)?,
                );
            }
            ReturnSlot::Node { side, name } => {
                let coordinate = coordinate(row, *side);
                output.insert(
                    name.clone(),
                    coordinate
                        .map(|coordinate| {
                            node_value(
                                coordinate,
                                hydrated,
                                wildcard_label_for_table(plan, coordinate.table_oid)?,
                                hydrate_nodes,
                            )
                        })
                        .transpose()?
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            ReturnSlot::Relationship { name } => {
                output.insert(name.clone(), wildcard_relationship_value(row, plan)?);
            }
            ReturnSlot::Property { .. } | ReturnSlot::Aggregate { .. } => {
                return Err(GraphError::GqlExecution {
                    reason: "wildcard path plans cannot project properties or aggregates"
                        .to_string(),
                });
            }
        }
    }
    Ok(serde_json::Value::Object(output))
}

fn project_join_row(
    row: &GqlRow,
    plan: &PhysicalJoinPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    let mut output = serde_json::Map::new();
    for slot in &plan.returns {
        match slot {
            ReturnSlot::Node { side, name } => {
                output.insert(
                    name.clone(),
                    coordinate(row, *side)
                        .map(|coordinate| {
                            node_value(
                                coordinate,
                                hydrated,
                                join_label(plan, *side)?,
                                hydrate_nodes,
                            )
                        })
                        .transpose()?
                        .unwrap_or(serde_json::Value::Null),
                );
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
                        .transpose()?
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            ReturnSlot::Relationship { name } => {
                output.insert(name.clone(), join_relationship_value(row, plan, name)?);
            }
            ReturnSlot::Path { name } => {
                output.insert(
                    name.clone(),
                    join_path_value(row, plan, name, hydrated, hydrate_nodes)?,
                );
            }
            ReturnSlot::PathFunction {
                func,
                path_var: Some(path_var),
                name,
            } => {
                output.insert(
                    name.clone(),
                    join_path_function_value(*func, row, plan, path_var, hydrated, hydrate_nodes)?,
                );
            }
            ReturnSlot::PathFunction { path_var: None, .. } | ReturnSlot::Aggregate { .. } => {
                return Err(GraphError::GqlExecution {
                    reason: "unsupported multi-pattern join return slot".to_string(),
                });
            }
        }
    }
    Ok(serde_json::Value::Object(output))
}

fn project_slot_value(
    row: &GqlRow,
    slot: &ReturnSlot,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    match slot {
        ReturnSlot::Node { side, .. } => coordinate(row, *side)
            .map(|coordinate| node_value(coordinate, hydrated, label(plan, *side), hydrate_nodes))
            .transpose()
            .map(|value| value.unwrap_or(serde_json::Value::Null)),
        ReturnSlot::Relationship { .. } => Ok(relationship_value(row, plan)),
        ReturnSlot::Path { .. } => path_value(row, plan, hydrated, hydrate_nodes),
        ReturnSlot::PathFunction { func, .. } => {
            path_function_value(*func, row, plan, hydrated, hydrate_nodes)
        }
        ReturnSlot::Property { side, property, .. } => coordinate(row, *side)
            .map(|coordinate| property_value(coordinate, hydrated, property))
            .transpose()
            .map(|value| value.unwrap_or(serde_json::Value::Null)),
        ReturnSlot::Aggregate { .. } => Err(GraphError::GqlExecution {
            reason: "aggregate slots cannot be used as grouping values".to_string(),
        }),
    }
}

fn project_join_slot_value(
    row: &GqlRow,
    slot: &ReturnSlot,
    plan: &PhysicalJoinPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    match slot {
        ReturnSlot::Node { side, .. } => match coordinate(row, *side) {
            Some(coordinate) => node_value(
                coordinate,
                hydrated,
                join_label(plan, *side)?,
                hydrate_nodes,
            ),
            None => Ok(serde_json::Value::Null),
        },
        ReturnSlot::Relationship { name } => join_relationship_value(row, plan, name),
        ReturnSlot::Path { name } => join_path_value(row, plan, name, hydrated, hydrate_nodes),
        ReturnSlot::PathFunction {
            func,
            path_var: Some(path_var),
            ..
        } => join_path_function_value(*func, row, plan, path_var, hydrated, hydrate_nodes),
        ReturnSlot::PathFunction { path_var: None, .. } => Err(GraphError::GqlExecution {
            reason: "single-pattern path functions cannot group multi-pattern join rows"
                .to_string(),
        }),
        ReturnSlot::Property { side, property, .. } => coordinate(row, *side)
            .map(|coordinate| property_value(coordinate, hydrated, property))
            .transpose()
            .map(|value| value.unwrap_or(serde_json::Value::Null)),
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
                    node_value(&row.node, hydrated, &plan.label, hydrate_nodes)?,
                );
            }
            ReturnSlot::Property { property, name, .. } => {
                output.insert(name.clone(), property_value(&row.node, hydrated, property)?);
            }
            ReturnSlot::Relationship { .. } => {
                return Err(GraphError::GqlExecution {
                    reason: "node-only MATCH cannot return relationship values".to_string(),
                });
            }
            ReturnSlot::Path { .. } | ReturnSlot::PathFunction { .. } => {
                return Err(GraphError::GqlExecution {
                    reason: "node-only MATCH cannot return path values".to_string(),
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
        ReturnSlot::Node { .. } => node_value(&row.node, hydrated, &plan.label, hydrate_nodes),
        ReturnSlot::Property { property, .. } => property_value(&row.node, hydrated, property),
        ReturnSlot::Relationship { .. } => Err(GraphError::GqlExecution {
            reason: "node-only MATCH cannot group by relationship values".to_string(),
        }),
        ReturnSlot::Path { .. } | ReturnSlot::PathFunction { .. } => {
            Err(GraphError::GqlExecution {
                reason: "node-only MATCH cannot group by path values".to_string(),
            })
        }
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
        AggregateArg::Node(side) => coordinate(row, *side)
            .map(|coordinate| node_value(coordinate, hydrated, label(plan, *side), hydrate_nodes))
            .transpose()
            .map(|value| value.unwrap_or(serde_json::Value::Null)),
        AggregateArg::Relationship => Ok(relationship_value(row, plan)),
        AggregateArg::JoinRelationship(_) | AggregateArg::JoinPath(_) => {
            Err(GraphError::GqlExecution {
                reason: "single-pattern MATCH cannot aggregate multi-pattern join values"
                    .to_string(),
            })
        }
        AggregateArg::Property { side, property } => coordinate(row, *side)
            .map(|coordinate| property_value(coordinate, hydrated, property))
            .transpose()
            .map(|value| value.unwrap_or(serde_json::Value::Null)),
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
        AggregateArg::Node(_) => node_value(&row.source, hydrated, label, hydrate_nodes),
        AggregateArg::Property { property, .. } => property_value(&row.source, hydrated, property),
        AggregateArg::Relationship
        | AggregateArg::JoinRelationship(_)
        | AggregateArg::JoinPath(_) => Err(GraphError::GqlExecution {
            reason: "node-only MATCH cannot aggregate relationship values".to_string(),
        }),
    }
}

fn aggregate_arg_value_for_join(
    row: &GqlRow,
    arg: &AggregateArg,
    plan: &PhysicalJoinPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    match arg {
        AggregateArg::All => Ok(serde_json::Value::Bool(true)),
        AggregateArg::Node(side) => match coordinate(row, *side) {
            Some(coordinate) => node_value(
                coordinate,
                hydrated,
                join_label(plan, *side)?,
                hydrate_nodes,
            ),
            None => Ok(serde_json::Value::Null),
        },
        AggregateArg::Property { side, property } => coordinate(row, *side)
            .map(|coordinate| property_value(coordinate, hydrated, property))
            .transpose()
            .map(|value| value.unwrap_or(serde_json::Value::Null)),
        AggregateArg::Relationship => Err(GraphError::GqlExecution {
            reason: "multi-pattern joins require a relationship slot for aggregation".to_string(),
        }),
        AggregateArg::JoinRelationship(slot) => join_relationship_value_by_slot(row, plan, *slot),
        AggregateArg::JoinPath(slot) => {
            join_path_value_by_slot(row, plan, *slot, hydrated, hydrate_nodes)
        }
    }
}

fn node_row_as_gql_row(row: &GqlNodeRow) -> GqlRow {
    GqlRow {
        source: row.node.clone(),
        target: Some(row.node.clone()),
        rel_start: Some(row.node.clone()),
        rel_end: Some(row.node.clone()),
        path_nodes: vec![row.node.clone()],
        path_relationships: Vec::new(),
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

fn path_value(
    row: &GqlRow,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    if row.path_nodes.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::json!({
        "_path": {
            "nodes": path_nodes_value(row, plan, hydrated, hydrate_nodes)?,
            "relationships": path_relationships_value(row, plan),
        }
    }))
}

fn path_function_value(
    func: PathFunc,
    row: &GqlRow,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    if row.path_nodes.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    match func {
        PathFunc::Nodes => Ok(serde_json::Value::Array(path_nodes_value(
            row,
            plan,
            hydrated,
            hydrate_nodes,
        )?)),
        PathFunc::Relationships => Ok(serde_json::Value::Array(path_relationships_value(
            row, plan,
        ))),
        PathFunc::Length => Ok(serde_json::Value::from(row.path_relationships.len())),
    }
}

fn wildcard_path_value(
    row: &GqlRow,
    plan: &PhysicalWildcardPathPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    Ok(serde_json::json!({
        "_path": {
            "nodes": wildcard_path_nodes_value(row, plan, hydrated, hydrate_nodes)?,
            "relationships": wildcard_path_relationships_value(row, plan)?,
        }
    }))
}

fn wildcard_path_function_value(
    func: PathFunc,
    row: &GqlRow,
    plan: &PhysicalWildcardPathPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    match func {
        PathFunc::Nodes => Ok(serde_json::Value::Array(wildcard_path_nodes_value(
            row,
            plan,
            hydrated,
            hydrate_nodes,
        )?)),
        PathFunc::Relationships => Ok(serde_json::Value::Array(wildcard_path_relationships_value(
            row, plan,
        )?)),
        PathFunc::Length => Ok(serde_json::Value::from(row.path_relationships.len())),
    }
}

fn wildcard_path_nodes_value(
    row: &GqlRow,
    plan: &PhysicalWildcardPathPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<Vec<serde_json::Value>> {
    row.path_nodes
        .iter()
        .map(|coordinate| {
            node_value(
                coordinate,
                hydrated,
                wildcard_label_for_table(plan, coordinate.table_oid)?,
                hydrate_nodes,
            )
        })
        .collect()
}

fn wildcard_path_relationships_value(
    row: &GqlRow,
    plan: &PhysicalWildcardPathPlan,
) -> GraphResult<Vec<serde_json::Value>> {
    row.path_relationships
        .iter()
        .map(|relationship| {
            Ok(serde_json::json!({
                "_type": &relationship.rel_type,
                "_start": wildcard_relationship_endpoint(&relationship.start, plan)?,
                "_end": wildcard_relationship_endpoint(&relationship.end, plan)?,
            }))
        })
        .collect()
}

fn wildcard_relationship_value(
    row: &GqlRow,
    plan: &PhysicalWildcardPathPlan,
) -> GraphResult<serde_json::Value> {
    let Some(relationship) = row.path_relationships.first() else {
        return Ok(serde_json::Value::Null);
    };
    Ok(serde_json::json!({
        "_type": &relationship.rel_type,
        "_start": wildcard_relationship_endpoint(&relationship.start, plan)?,
        "_end": wildcard_relationship_endpoint(&relationship.end, plan)?,
    }))
}

fn join_relationship_value(
    row: &GqlRow,
    plan: &PhysicalJoinPlan,
    name: &str,
) -> GraphResult<serde_json::Value> {
    let slot_index = plan
        .rel_slots
        .iter()
        .position(|slot| slot.var == name)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("unknown multi-pattern relationship slot `{name}`"),
        })?;
    join_relationship_value_by_slot(row, plan, slot_index)
}

fn join_relationship_value_by_slot(
    row: &GqlRow,
    plan: &PhysicalJoinPlan,
    slot_index: usize,
) -> GraphResult<serde_json::Value> {
    let slot = plan
        .rel_slots
        .get(slot_index)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("unknown multi-pattern relationship slot {slot_index}"),
        })?;
    let Some(relationship) = row.path_relationships.get(slot.pattern_slot) else {
        return Err(GraphError::GqlExecution {
            reason: format!(
                "multi-pattern relationship slot `{}` points at missing pattern {}",
                slot.var, slot.pattern_slot
            ),
        });
    };
    Ok(serde_json::json!({
        "_type": &relationship.rel_type,
        "_start": join_relationship_endpoint(&relationship.start, plan)?,
        "_end": join_relationship_endpoint(&relationship.end, plan)?,
    }))
}

fn join_path_value(
    row: &GqlRow,
    plan: &PhysicalJoinPlan,
    name: &str,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    let slot_index = plan
        .path_slots
        .iter()
        .position(|slot| slot.var == name)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("unknown multi-pattern path slot `{name}`"),
        })?;
    join_path_value_by_slot(row, plan, slot_index, hydrated, hydrate_nodes)
}

fn join_path_value_by_slot(
    row: &GqlRow,
    plan: &PhysicalJoinPlan,
    slot_index: usize,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    let slot = plan
        .path_slots
        .get(slot_index)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("unknown multi-pattern path slot {slot_index}"),
        })?;
    let pattern = plan
        .patterns
        .get(slot.pattern_slot)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!(
                "multi-pattern path slot `{}` points at missing pattern {}",
                slot.var, slot.pattern_slot
            ),
        })?;
    let source =
        row.path_nodes
            .get(pattern.source_slot)
            .ok_or_else(|| GraphError::GqlExecution {
                reason: format!(
                    "multi-pattern path slot `{}` source node slot {} is missing",
                    slot.var, pattern.source_slot
                ),
            })?;
    let target =
        row.path_nodes
            .get(pattern.target_slot)
            .ok_or_else(|| GraphError::GqlExecution {
                reason: format!(
                    "multi-pattern path slot `{}` target node slot {} is missing",
                    slot.var, pattern.target_slot
                ),
            })?;
    let relationship = row
        .path_relationships
        .get(slot.pattern_slot)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!(
                "multi-pattern path slot `{}` relationship pattern {} is missing",
                slot.var, slot.pattern_slot
            ),
        })?;

    Ok(serde_json::json!({
        "_path": {
            "nodes": [
                node_value(source, hydrated, join_label(plan, BindingSide::PathNode(pattern.source_slot))?, hydrate_nodes)?,
                node_value(target, hydrated, join_label(plan, BindingSide::PathNode(pattern.target_slot))?, hydrate_nodes)?,
            ],
            "relationships": [
                {
                    "_type": &relationship.rel_type,
                    "_start": join_relationship_endpoint(&relationship.start, plan)?,
                    "_end": join_relationship_endpoint(&relationship.end, plan)?,
                }
            ],
        }
    }))
}

fn join_path_function_value(
    func: PathFunc,
    row: &GqlRow,
    plan: &PhysicalJoinPlan,
    path_var: &str,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<serde_json::Value> {
    if matches!(func, PathFunc::Length) {
        return Ok(serde_json::Value::from(join_path_length(
            row, plan, path_var,
        )?));
    }
    let path = join_path_value(row, plan, path_var, hydrated, hydrate_nodes)?;
    let path = path.get("_path").ok_or_else(|| GraphError::GqlExecution {
        reason: format!("multi-pattern path `{path_var}` did not project _path"),
    })?;
    match func {
        PathFunc::Nodes => Ok(path
            .get("nodes")
            .cloned()
            .unwrap_or(serde_json::Value::Null)),
        PathFunc::Relationships => Ok(path
            .get("relationships")
            .cloned()
            .unwrap_or(serde_json::Value::Null)),
        PathFunc::Length => Ok(serde_json::Value::from(join_path_length(
            row, plan, path_var,
        )?)),
    }
}

fn join_path_length(row: &GqlRow, plan: &PhysicalJoinPlan, path_var: &str) -> GraphResult<i64> {
    let slot = plan
        .path_slots
        .iter()
        .find(|slot| slot.var == path_var)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("unknown multi-pattern path slot `{path_var}`"),
        })?;
    Ok(if row.path_relationships.get(slot.pattern_slot).is_some() {
        1
    } else {
        0
    })
}

fn path_nodes_value(
    row: &GqlRow,
    plan: &PhysicalPlan,
    hydrated: &HydratedRows,
    hydrate_nodes: bool,
) -> GraphResult<Vec<serde_json::Value>> {
    row.path_nodes
        .iter()
        .map(|coordinate| {
            node_value(
                coordinate,
                hydrated,
                label_for_table(plan, coordinate.table_oid),
                hydrate_nodes,
            )
        })
        .collect()
}

fn path_relationships_value(row: &GqlRow, plan: &PhysicalPlan) -> Vec<serde_json::Value> {
    row.path_relationships
        .iter()
        .map(|relationship| {
            serde_json::json!({
                "_type": &relationship.rel_type,
                "_start": relationship_endpoint(&relationship.start, plan),
                "_end": relationship_endpoint(&relationship.end, plan),
            })
        })
        .collect()
}

fn relationship_endpoint(coordinate: &GqlNodeCoordinate, plan: &PhysicalPlan) -> serde_json::Value {
    serde_json::json!({
        "table": label_for_table(plan, coordinate.table_oid),
        "id": &coordinate.node_id,
    })
}

fn join_relationship_endpoint(
    coordinate: &GqlNodeCoordinate,
    plan: &PhysicalJoinPlan,
) -> GraphResult<serde_json::Value> {
    let label = plan
        .node_slots
        .iter()
        .find(|slot| slot.table_oid == coordinate.table_oid)
        .map(|slot| slot.label.as_str())
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!(
                "multi-pattern relationship endpoint table OID {} is not bound",
                coordinate.table_oid
            ),
        })?;
    Ok(serde_json::json!({
        "table": label,
        "id": &coordinate.node_id,
    }))
}

fn wildcard_relationship_endpoint(
    coordinate: &GqlNodeCoordinate,
    plan: &PhysicalWildcardPathPlan,
) -> GraphResult<serde_json::Value> {
    Ok(serde_json::json!({
        "table": wildcard_label_for_table(plan, coordinate.table_oid)?,
        "id": &coordinate.node_id,
    }))
}

fn node_value(
    coordinate: &GqlNodeCoordinate,
    hydrated: &HydratedRows,
    label: &str,
    hydrate: bool,
) -> GraphResult<serde_json::Value> {
    let mut node = if hydrate {
        hydrated_node_object(coordinate, hydrated)?
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
    Ok(serde_json::Value::Object(node))
}

fn property_value(
    coordinate: &GqlNodeCoordinate,
    hydrated: &HydratedRows,
    property: &str,
) -> GraphResult<serde_json::Value> {
    lookup_property_value(coordinate, hydrated, property).map(EvalValue::into_json_or_null)
}

fn lookup_property_value(
    coordinate: &GqlNodeCoordinate,
    hydrated: &HydratedRows,
    property: &str,
) -> GraphResult<EvalValue> {
    let row = hydrated_required_row(coordinate, hydrated)?;
    let mut segments = property.split('.');
    let Some(first) = segments.next() else {
        return Ok(EvalValue::Missing);
    };
    let Some(mut value) = row.get(first) else {
        return Ok(EvalValue::Missing);
    };
    for segment in segments {
        let Some(next) = value.get(segment) else {
            return Ok(EvalValue::Missing);
        };
        value = next;
    }
    Ok(EvalValue::Value(value.clone()))
}

fn hydrated_node_object(
    coordinate: &GqlNodeCoordinate,
    hydrated: &HydratedRows,
) -> GraphResult<serde_json::Map<String, serde_json::Value>> {
    let row = hydrated_required_row(coordinate, hydrated)?;
    row.as_object()
        .cloned()
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!(
                "GQL could not hydrate node `{}` from table OID {}",
                coordinate.node_id, coordinate.table_oid
            ),
        })
}

fn hydrated_required_row<'a>(
    coordinate: &GqlNodeCoordinate,
    hydrated: &'a HydratedRows,
) -> GraphResult<&'a serde_json::Value> {
    hydrated
        .get(&(coordinate.table_oid, coordinate.node_id.clone()))
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!(
                "GQL could not hydrate node `{}` from table OID {}",
                coordinate.node_id, coordinate.table_oid
            ),
        })
}

fn coordinate(row: &GqlRow, side: BindingSide) -> Option<&GqlNodeCoordinate> {
    match side {
        BindingSide::Source => Some(&row.source),
        BindingSide::Target => row.target.as_ref(),
        BindingSide::PathNode(idx) => row.path_nodes.get(idx),
    }
}

fn label(plan: &PhysicalPlan, side: BindingSide) -> &str {
    match side {
        BindingSide::Source => &plan.source_label,
        BindingSide::Target => &plan.target_label,
        BindingSide::PathNode(_) => {
            unreachable!("path-node bindings are not used by one-hop plans")
        }
    }
}

fn join_label(plan: &PhysicalJoinPlan, side: BindingSide) -> GraphResult<&str> {
    let BindingSide::PathNode(slot) = side else {
        return Err(GraphError::GqlExecution {
            reason: "multi-pattern join node binding used a non-slot side".to_string(),
        });
    };
    plan.node_slots
        .get(slot)
        .map(|node| node.label.as_str())
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("multi-pattern join node slot {slot} is out of range"),
        })
}

fn label_for_table(plan: &PhysicalPlan, table_oid: u32) -> &str {
    if table_oid == plan.source_table_oid {
        &plan.source_label
    } else {
        &plan.target_label
    }
}

fn wildcard_label_for_table(plan: &PhysicalWildcardPathPlan, table_oid: u32) -> GraphResult<&str> {
    plan.table_labels
        .get(&table_oid)
        .map(String::as_str)
        .ok_or_else(|| GraphError::GqlExecution {
            reason: format!("wildcard path plan has no label for table OID {table_oid}"),
        })
}
