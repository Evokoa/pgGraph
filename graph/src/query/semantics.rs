//! Semantic binding for the supported GQL subset.

use crate::gql::ast::{
    self, CmpOp, Direction, Expr, Literal, LiteralValue, MatchClause, NodePat, Operand, Pattern,
    RelPat, ReturnExpr, ReturnItem, SortItem, SortKey, WithClause,
};
use crate::gql::errors::{GqlError, Span};

use super::catalog_snapshot::{CatalogSnapshot, EdgeMappingInfo, NodeLabelInfo, RelTypeInfo};
use super::logical_plan::{
    AggregateArg, AggregateFunc, BindingSide, BoundCmpOp, BoundDirection, BoundIncidentEdge,
    BoundMappedEdge, BoundNode, BoundRel, CreateProperty, CreateReturnBinding, CreateValue,
    HopBounds, LogicalCreateNode, LogicalDeleteEdge, LogicalDetachDeleteNode, LogicalJoinNodeSlot,
    LogicalJoinPathSlot, LogicalJoinPattern, LogicalJoinPlan, LogicalJoinRelSlot, LogicalMergeNode,
    LogicalNodeScan, LogicalPlan, LogicalRemoveProperty, LogicalSetProperty, LogicalStatement,
    LogicalWildcardPathPlan, LogicalWildcardPathSegment, PathFunc, Predicate, ReturnBinding,
    SortBinding, SortBindingKey, ValueExpr,
};
use super::physical_plan::MAX_GQL_RESULT_ROWS;

const MAX_BOUND_PREDICATE_DEPTH: usize = 512;
const MAX_BOUND_PREDICATE_COUNT: usize = 512;
const MAX_BOUND_HOPS: u32 = 64;

/// Bind parsed GQL into a logical plan.
///
/// # Errors
///
/// Returns [`GqlError`] when the query uses valid syntax outside the current
/// Phase 1B execution slice or when labels/types cannot resolve in the catalog.
pub(crate) fn bind(
    query: &crate::gql::ast::Query,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalPlan, GqlError> {
    reject_later_clauses(query)?;
    validate_row_window(query)?;
    let (source_pat, rel_pat, target_pat) = single_outbound_hop(&query.match_)?;
    let source = bind_node(source_pat, catalog)?;
    let target = bind_node(target_pat, catalog)?;
    let rel_type = rel_pat.rel_type.as_ref().ok_or_else(|| {
        GqlError::unsupported(
            rel_pat.span,
            "anonymous relationship types require a later phase",
        )
    })?;
    let rel_info = resolve_relationship(catalog, rel_pat, rel_type, &source, &target)?;
    let predicate = bind_predicates(
        query.where_.as_ref(),
        source_pat,
        rel_pat,
        target_pat,
        &source,
        &target,
    )?;
    let initial_scope = initial_relationship_scope(rel_pat, target_pat, &source, &target)?;
    let BoundWith {
        scope,
        distinct_stages,
    } = bind_with_clauses(&query.with_, initial_scope, &source, &target)?;
    let returns = bind_scoped_returns(&query.return_.items, &scope, &source, &target)?;
    let order_by = bind_scoped_sort_items(
        &query.order_by,
        &returns,
        &scope,
        &source,
        &target,
        query.return_.distinct,
    )?;
    Ok(LogicalPlan {
        optional: query.match_.optional,
        source,
        relationship: BoundRel {
            var: rel_pat.var.as_ref().map(|var| var.text.clone()),
            rel_type: rel_info.rel_type,
            direction: bind_direction(rel_pat.direction),
            hops: bind_hops(rel_pat)?,
        },
        target,
        returns,
        distinct_stages,
        distinct: query.return_.distinct,
        predicate,
        order_by,
        skip: query.skip,
        limit: query.limit,
    })
}

fn bind_node_scan(
    query: &crate::gql::ast::Query,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalNodeScan, GqlError> {
    reject_later_clauses(query)?;
    validate_row_window(query)?;
    let Pattern { start, tail, .. } = &query.match_.pattern;
    if !tail.is_empty() {
        return Err(GqlError::unsupported(
            query.match_.pattern.span,
            "node scan binding requires a node-only MATCH pattern",
        ));
    }
    if query.match_.optional {
        return Err(GqlError::unsupported(
            query.match_.span,
            "node-only OPTIONAL MATCH requires multi-stage row semantics from a later read phase",
        ));
    }
    let node = bind_node(start, catalog)?;
    let predicate = bind_node_predicates(query.where_.as_ref(), start, &node)?;
    let initial_scope = initial_node_scope(&node);
    let BoundWith {
        scope,
        distinct_stages,
    } = bind_with_clauses(&query.with_, initial_scope, &node, &node)?;
    let returns = bind_scoped_returns(&query.return_.items, &scope, &node, &node)?;
    let order_by = bind_scoped_sort_items(
        &query.order_by,
        &returns,
        &scope,
        &node,
        &node,
        query.return_.distinct,
    )?;
    Ok(LogicalNodeScan {
        node,
        returns,
        distinct_stages,
        distinct: query.return_.distinct,
        predicate,
        order_by,
        skip: query.skip,
        limit: query.limit,
    })
}

/// Bind a parsed GQL statement into a logical statement.
///
/// # Errors
///
/// Returns [`GqlError`] when the statement uses valid syntax outside the
/// current execution slice or when labels/properties cannot resolve in the
/// catalog.
pub(crate) fn bind_statement(
    statement: &crate::gql::ast::Statement,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalStatement, GqlError> {
    match statement {
        crate::gql::ast::Statement::Read(query) if query.match_.patterns.len() > 1 => {
            bind_join_read(query, catalog).map(LogicalStatement::JoinRead)
        }
        crate::gql::ast::Statement::Read(query) if query.match_.pattern.path_var.is_some() => {
            bind_wildcard_path_read(query, catalog).map(LogicalStatement::WildcardPathRead)
        }
        crate::gql::ast::Statement::Read(query) if query.match_.pattern.tail.is_empty() => {
            bind_node_scan(query, catalog).map(LogicalStatement::NodeScan)
        }
        crate::gql::ast::Statement::Read(query) => bind(query, catalog).map(LogicalStatement::Read),
        crate::gql::ast::Statement::Create(query) => {
            bind_create_node(query, catalog).map(LogicalStatement::CreateNode)
        }
        crate::gql::ast::Statement::Set(query) => {
            bind_set_property(query, catalog).map(LogicalStatement::SetProperty)
        }
        crate::gql::ast::Statement::Remove(query) => {
            bind_remove_property(query, catalog).map(LogicalStatement::RemoveProperty)
        }
        crate::gql::ast::Statement::Delete(query) => {
            bind_delete_edge(query, catalog).map(LogicalStatement::DeleteEdge)
        }
        crate::gql::ast::Statement::DetachDelete(query) => {
            bind_detach_delete_node(query, catalog).map(LogicalStatement::DetachDeleteNode)
        }
        crate::gql::ast::Statement::Merge(query) => {
            bind_merge_node(query, catalog).map(LogicalStatement::MergeNode)
        }
    }
}

fn bind_join_read(
    query: &crate::gql::ast::Query,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalJoinPlan, GqlError> {
    validate_row_window(query)?;

    let mut node_slots = Vec::new();
    let mut slot_by_var = std::collections::HashMap::<String, usize>::new();
    let mut property_by_var = std::collections::HashMap::<String, (usize, String)>::new();
    let mut rel_slots = Vec::new();
    let mut rel_by_var = std::collections::HashMap::<String, usize>::new();
    let mut path_slots = Vec::new();
    let mut path_by_var = std::collections::HashMap::<String, usize>::new();
    let mut patterns = Vec::with_capacity(query.match_.patterns.len());
    for pattern in &query.match_.patterns {
        let [(rel, target)] = pattern.tail.as_slice() else {
            return Err(GqlError::unsupported(
                pattern.span,
                "multi-pattern joins currently support fixed single-relationship patterns",
            ));
        };
        if !rel.props.is_empty() {
            return Err(GqlError::unsupported(
                rel.span,
                "relationship properties over multi-pattern joins require a later phase",
            ));
        }
        let hops = bind_hops(rel)?;
        let rel_type = rel.rel_type.as_ref().ok_or_else(|| {
            GqlError::unsupported(
                rel.span,
                "anonymous relationship types in multi-pattern joins require a later phase",
            )
        })?;
        let source_slot = bind_join_node_slot(
            &pattern.start,
            catalog,
            &mut node_slots,
            &mut slot_by_var,
            &rel_by_var,
            &path_by_var,
        )?;
        let target_slot = bind_join_node_slot(
            target,
            catalog,
            &mut node_slots,
            &mut slot_by_var,
            &rel_by_var,
            &path_by_var,
        )?;
        validate_join_relationship(
            catalog,
            rel,
            rel_type,
            &node_slots[source_slot],
            &node_slots[target_slot],
        )?;
        bind_join_relationship_slot(
            rel,
            patterns.len(),
            &slot_by_var,
            &path_by_var,
            &mut rel_slots,
            &mut rel_by_var,
        )?;
        bind_join_path_slot(
            pattern,
            patterns.len(),
            &slot_by_var,
            &rel_by_var,
            &mut path_slots,
            &mut path_by_var,
        )?;
        patterns.push(LogicalJoinPattern {
            source_slot,
            rel_type: rel_type.text.clone(),
            direction: bind_direction(rel.direction),
            hops,
            target_slot,
        });
    }

    let predicate = bind_join_predicate(query.where_.as_ref(), &node_slots, &slot_by_var)?;
    let mut value_by_var = std::collections::HashMap::new();
    let with_clauses = bind_join_with_clauses(
        &query.with_,
        &node_slots,
        &mut slot_by_var,
        &mut property_by_var,
        &mut value_by_var,
        &mut rel_slots,
        &mut rel_by_var,
        &mut path_slots,
        &mut path_by_var,
    )?;
    let returns = bind_join_returns(
        &query.return_.items,
        &node_slots,
        &slot_by_var,
        &property_by_var,
        &value_by_var,
        &rel_by_var,
        &path_by_var,
        &mut rel_slots,
        &mut path_slots,
        with_clauses.has_aggregate_projection,
    )?;
    let order_by = bind_join_sort_items(
        &query.order_by,
        &returns,
        &node_slots,
        &slot_by_var,
        &property_by_var,
        query.return_.distinct,
    )?;
    let required_table_oids = node_slots.iter().map(|slot| slot.table_oid).collect();
    Ok(LogicalJoinPlan {
        optional: query.match_.optional,
        node_slots,
        rel_slots,
        path_slots,
        patterns,
        returns,
        aggregate_projection: with_clauses.aggregate_projection,
        aggregate_group_slots: with_clauses.aggregate_group_slots,
        distinct_stages: with_clauses.distinct_stages,
        post_aggregate_distinct_stages: with_clauses.post_aggregate_distinct_stages,
        distinct: query.return_.distinct,
        predicate,
        order_by,
        required_table_oids,
        skip: query.skip,
        limit: query.limit,
    })
}

fn bind_join_node_slot(
    node: &NodePat,
    catalog: &impl CatalogSnapshot,
    node_slots: &mut Vec<LogicalJoinNodeSlot>,
    slot_by_var: &mut std::collections::HashMap<String, usize>,
    rel_by_var: &std::collections::HashMap<String, usize>,
    path_by_var: &std::collections::HashMap<String, usize>,
) -> Result<usize, GqlError> {
    if !node.props.is_empty() {
        return Err(GqlError::unsupported(
            node.span,
            "node properties in multi-pattern joins require a later phase",
        ));
    }
    let var = node.var.as_ref().ok_or_else(|| {
        GqlError::unsupported(
            node.span,
            "anonymous nodes in multi-pattern joins require a later phase",
        )
    })?;
    if rel_by_var.contains_key(&var.text) || path_by_var.contains_key(&var.text) {
        return Err(GqlError::bind(
            var.span,
            format!("duplicate variable `{}` in MATCH scope", var.text),
        ));
    }
    if let Some(slot) = slot_by_var.get(&var.text).copied() {
        if let Some(label) = &node.label {
            let resolved = catalog.resolve_node_label(&label.text, label.span)?;
            if resolved.table_oid != node_slots[slot].table_oid {
                return Err(GqlError::bind(
                    label.span,
                    format!(
                        "conflicting label `{}` for previously bound variable `{}`",
                        label.text, var.text
                    ),
                ));
            }
        }
        return Ok(slot);
    }
    let label = node.label.as_ref().ok_or_else(|| {
        GqlError::unsupported(
            node.span,
            "new variables in multi-pattern joins require a concrete node label",
        )
    })?;
    let info = catalog.resolve_node_label(&label.text, label.span)?;
    let slot = node_slots.len();
    node_slots.push(LogicalJoinNodeSlot {
        var: var.text.clone(),
        table_oid: info.table_oid,
        label: info.label,
        properties: info.properties,
    });
    slot_by_var.insert(var.text.clone(), slot);
    Ok(slot)
}

fn bind_join_relationship_slot(
    rel: &RelPat,
    pattern_slot: usize,
    slot_by_var: &std::collections::HashMap<String, usize>,
    path_by_var: &std::collections::HashMap<String, usize>,
    rel_slots: &mut Vec<LogicalJoinRelSlot>,
    rel_by_var: &mut std::collections::HashMap<String, usize>,
) -> Result<(), GqlError> {
    let Some(var) = &rel.var else {
        return Ok(());
    };
    if rel.var_len.is_some() {
        return Err(GqlError::unsupported(
            var.span,
            "relationship variables on variable-length multi-pattern joins require path support",
        ));
    }
    if slot_by_var.contains_key(&var.text)
        || rel_by_var.contains_key(&var.text)
        || path_by_var.contains_key(&var.text)
    {
        return Err(GqlError::bind(
            var.span,
            format!("duplicate variable `{}` in MATCH scope", var.text),
        ));
    }
    let slot = rel_slots.len();
    rel_slots.push(LogicalJoinRelSlot {
        var: var.text.clone(),
        pattern_slot,
    });
    rel_by_var.insert(var.text.clone(), slot);
    Ok(())
}

fn bind_join_path_slot(
    pattern: &Pattern,
    pattern_slot: usize,
    slot_by_var: &std::collections::HashMap<String, usize>,
    rel_by_var: &std::collections::HashMap<String, usize>,
    path_slots: &mut Vec<LogicalJoinPathSlot>,
    path_by_var: &mut std::collections::HashMap<String, usize>,
) -> Result<(), GqlError> {
    let Some(var) = &pattern.path_var else {
        return Ok(());
    };
    if slot_by_var.contains_key(&var.text)
        || rel_by_var.contains_key(&var.text)
        || path_by_var.contains_key(&var.text)
    {
        return Err(GqlError::bind(
            var.span,
            format!("duplicate variable `{}` in MATCH scope", var.text),
        ));
    }
    let slot = path_slots.len();
    path_slots.push(LogicalJoinPathSlot {
        var: var.text.clone(),
        pattern_slot,
    });
    path_by_var.insert(var.text.clone(), slot);
    Ok(())
}

fn validate_join_relationship(
    catalog: &impl CatalogSnapshot,
    rel: &RelPat,
    rel_type: &ast::Ident,
    source: &LogicalJoinNodeSlot,
    target: &LogicalJoinNodeSlot,
) -> Result<(), GqlError> {
    let source = BoundNode {
        var: source.var.clone(),
        label: source.label.clone(),
        table_oid: source.table_oid,
        properties: source.properties.clone(),
    };
    let target = BoundNode {
        var: target.var.clone(),
        label: target.label.clone(),
        table_oid: target.table_oid,
        properties: target.properties.clone(),
    };
    resolve_relationship(catalog, rel, rel_type, &source, &target)?;
    Ok(())
}

fn bind_join_predicate(
    where_: Option<&Expr>,
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
) -> Result<Option<Predicate>, GqlError> {
    where_
        .map(|expr| bind_join_expr(expr, node_slots, slot_by_var, 0))
        .transpose()
}

fn bind_join_expr(
    expr: &Expr,
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
    depth: usize,
) -> Result<Predicate, GqlError> {
    if depth > MAX_BOUND_PREDICATE_DEPTH {
        return Err(GqlError::syntax(
            expr_span(expr),
            "predicate expression is too deeply nested",
        ));
    }
    match expr {
        Expr::And { lhs, rhs, .. } => Ok(Predicate::And(
            Box::new(bind_join_expr(lhs, node_slots, slot_by_var, depth + 1)?),
            Box::new(bind_join_expr(rhs, node_slots, slot_by_var, depth + 1)?),
        )),
        Expr::Or { lhs, rhs, .. } => Ok(Predicate::Or(
            Box::new(bind_join_expr(lhs, node_slots, slot_by_var, depth + 1)?),
            Box::new(bind_join_expr(rhs, node_slots, slot_by_var, depth + 1)?),
        )),
        Expr::Not { expr, .. } => Ok(Predicate::Not(Box::new(bind_join_expr(
            expr,
            node_slots,
            slot_by_var,
            depth + 1,
        )?))),
        Expr::Compare { lhs, op, rhs, .. } => Ok(Predicate::Compare {
            lhs: bind_join_operand(lhs, node_slots, slot_by_var)?,
            op: bind_cmp_op(*op),
            rhs: rhs
                .as_ref()
                .map(|operand| bind_join_operand(operand, node_slots, slot_by_var))
                .transpose()?,
        }),
    }
}

fn bind_join_operand(
    operand: &Operand,
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
) -> Result<ValueExpr, GqlError> {
    match operand {
        Operand::Property {
            var,
            property,
            span: _,
        } => {
            let slot = slot_by_var.get(&var.text).copied().ok_or_else(|| {
                GqlError::bind(var.span, format!("unknown variable `{}`", var.text))
            })?;
            if !node_slots[slot].properties.contains(&property.text) {
                return Err(GqlError::bind(
                    property.span,
                    format!(
                        "unknown property `{}` for label `{}`",
                        property.text, node_slots[slot].label
                    ),
                ));
            }
            Ok(ValueExpr::Property {
                side: BindingSide::PathNode(slot),
                property: property.text.clone(),
            })
        }
        Operand::Literal(literal) => Ok(ValueExpr::Literal(literal_json(literal))),
        Operand::Param { name, .. } => Ok(ValueExpr::Param(name.text.clone())),
        Operand::List { values, .. } => {
            Ok(ValueExpr::List(values.iter().map(literal_json).collect()))
        }
    }
}

fn bind_join_sort_items(
    items: &[SortItem],
    returns: &[ReturnBinding],
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
    property_by_var: &std::collections::HashMap<String, (usize, String)>,
    return_distinct: bool,
) -> Result<Vec<SortBinding>, GqlError> {
    let has_aggregate = returns.iter().any(ReturnBinding::is_aggregate);
    items
        .iter()
        .map(|item| {
            let key = match &item.key {
                SortKey::Alias { alias, .. } => {
                    if returns
                        .iter()
                        .any(|binding| binding.name() == alias.text && binding.is_sortable_scalar())
                    {
                        SortBindingKey::ReturnName(alias.text.clone())
                    } else if returns.iter().any(|binding| binding.name() == alias.text) {
                        return Err(GqlError::unsupported(
                            alias.span,
                            "ORDER BY aliases must refer to scalar property returns",
                        ));
                    } else if let Some((slot, property)) = property_by_var.get(&alias.text) {
                        if has_aggregate {
                            return Err(GqlError::unsupported(
                                alias.span,
                                "aggregate queries must ORDER BY returned property or aggregate aliases",
                            ));
                        }
                        if return_distinct {
                            return Err(GqlError::unsupported(
                                alias.span,
                                "DISTINCT queries must ORDER BY returned scalar expressions",
                            ));
                        }
                        SortBindingKey::Property {
                            side: BindingSide::PathNode(*slot),
                            property: property.clone(),
                        }
                    } else {
                        return Err(GqlError::bind(
                            alias.span,
                            format!("unknown ORDER BY alias `{}`", alias.text),
                        ));
                    }
                }
                SortKey::Property {
                    var,
                    property,
                    span: _,
                } => {
                    if has_aggregate {
                        if let Some(binding) = returned_join_property_binding(
                            returns,
                            slot_by_var,
                            &var.text,
                            &property.text,
                        ) {
                            return Ok(SortBinding {
                                key: SortBindingKey::ReturnName(binding.name().to_string()),
                                desc: item.desc,
                            });
                        }
                        return Err(GqlError::unsupported(
                            item.span,
                            "aggregate queries must ORDER BY returned property or aggregate aliases",
                        ));
                    }
                    if return_distinct {
                        if let Some(binding) = returned_join_property_binding(
                            returns,
                            slot_by_var,
                            &var.text,
                            &property.text,
                        ) {
                            return Ok(SortBinding {
                                key: SortBindingKey::ReturnName(binding.name().to_string()),
                                desc: item.desc,
                            });
                        }
                        return Err(GqlError::unsupported(
                            item.span,
                            "DISTINCT queries must ORDER BY returned scalar expressions",
                        ));
                    }
                    let slot = slot_by_var.get(&var.text).copied().ok_or_else(|| {
                        GqlError::bind(
                            var.span,
                            format!("unknown ORDER BY variable `{}`", var.text),
                        )
                    })?;
                    if !node_slots[slot].properties.contains(&property.text) {
                        return Err(GqlError::bind(
                            property.span,
                            format!(
                                "unknown property `{}` for label `{}`",
                                property.text, node_slots[slot].label
                            ),
                        ));
                    }
                    SortBindingKey::Property {
                        side: BindingSide::PathNode(slot),
                        property: property.text.clone(),
                    }
                }
            };
            Ok(SortBinding {
                key,
                desc: item.desc,
            })
        })
        .collect()
}

fn returned_join_property_binding<'a>(
    returns: &'a [ReturnBinding],
    slot_by_var: &std::collections::HashMap<String, usize>,
    var: &str,
    property: &str,
) -> Option<&'a ReturnBinding> {
    let slot = slot_by_var.get(var)?;
    returns.iter().find(|binding| {
        matches!(
            binding,
            ReturnBinding::Property {
                side: BindingSide::PathNode(binding_slot),
                property: binding_property,
                ..
            } if binding_slot == slot && binding_property == property
        )
    })
}

fn bind_join_with_clauses(
    clauses: &[WithClause],
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &mut std::collections::HashMap<String, usize>,
    property_by_var: &mut std::collections::HashMap<String, (usize, String)>,
    value_by_var: &mut std::collections::HashMap<String, ReturnBinding>,
    rel_slots: &mut Vec<LogicalJoinRelSlot>,
    rel_by_var: &mut std::collections::HashMap<String, usize>,
    path_slots: &mut Vec<LogicalJoinPathSlot>,
    path_by_var: &mut std::collections::HashMap<String, usize>,
) -> Result<JoinWithClauses, GqlError> {
    let mut distinct_stages = Vec::new();
    let mut post_aggregate_distinct_stages = Vec::new();
    let mut aggregate_projection = Vec::new();
    let mut aggregate_group_slots = Vec::new();
    let mut has_aggregate_projection = false;
    for clause in clauses {
        let has_direct_aggregate = clause
            .items
            .iter()
            .any(|item| matches!(&item.expr, ReturnExpr::Aggregate { .. }));
        if clause.distinct && !has_direct_aggregate {
            let stage = bind_join_distinct_stage(
                &clause.items,
                node_slots,
                slot_by_var,
                property_by_var,
                value_by_var,
                rel_slots,
                rel_by_var,
                path_slots,
                path_by_var,
            )?;
            if has_aggregate_projection {
                post_aggregate_distinct_stages.push(stage);
            } else {
                distinct_stages.push(stage);
            }
        }
        let next = bind_join_projection_scope(
            &clause.items,
            node_slots,
            slot_by_var,
            property_by_var,
            value_by_var,
            rel_slots,
            rel_by_var,
            path_slots,
            path_by_var,
        )?;
        if next.has_new_aggregate_projection {
            if has_aggregate_projection {
                return Err(GqlError::unsupported(
                    clause.span,
                    "aggregate WITH projections after aggregate WITH projections require a later projection phase",
                ));
            }
            has_aggregate_projection = true;
            aggregate_projection = next.projection_slots.clone();
            aggregate_group_slots.extend(next.aggregate_group_slots.clone());
        }
        if has_aggregate_projection {
            slot_by_var.clear();
            property_by_var.clear();
            *value_by_var = if next.has_new_aggregate_projection {
                projected_value_scope(&next.projection_slots)
            } else {
                projected_alias_scope(&next.projection_slots)
            };
            rel_by_var.clear();
            path_by_var.clear();
        } else {
            *slot_by_var = next.nodes;
            *property_by_var = next.properties;
            *value_by_var = next.values;
            *rel_by_var = next.relationships;
            *path_by_var = next.paths;
        }
    }
    Ok(JoinWithClauses {
        distinct_stages,
        post_aggregate_distinct_stages,
        aggregate_projection,
        aggregate_group_slots,
        has_aggregate_projection,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JoinWithClauses {
    distinct_stages: Vec<Vec<ReturnBinding>>,
    post_aggregate_distinct_stages: Vec<Vec<ReturnBinding>>,
    aggregate_projection: Vec<ReturnBinding>,
    aggregate_group_slots: Vec<ReturnBinding>,
    has_aggregate_projection: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JoinProjectionScope {
    nodes: std::collections::HashMap<String, usize>,
    properties: std::collections::HashMap<String, (usize, String)>,
    values: std::collections::HashMap<String, ReturnBinding>,
    relationships: std::collections::HashMap<String, usize>,
    paths: std::collections::HashMap<String, usize>,
    projection_slots: Vec<ReturnBinding>,
    aggregate_group_slots: Vec<ReturnBinding>,
    has_new_aggregate_projection: bool,
}

fn projected_value_scope(
    slots: &[ReturnBinding],
) -> std::collections::HashMap<String, ReturnBinding> {
    slots
        .iter()
        .map(|slot| {
            let name = slot.name().to_string();
            (
                name.clone(),
                ReturnBinding::Projected {
                    source: name.clone(),
                    name,
                },
            )
        })
        .collect()
}

fn projected_alias_scope(
    slots: &[ReturnBinding],
) -> std::collections::HashMap<String, ReturnBinding> {
    slots
        .iter()
        .map(|slot| (slot.name().to_string(), slot.clone()))
        .collect()
}

fn bind_join_distinct_stage(
    items: &[ReturnItem],
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
    property_by_var: &std::collections::HashMap<String, (usize, String)>,
    value_by_var: &std::collections::HashMap<String, ReturnBinding>,
    rel_slots: &mut Vec<LogicalJoinRelSlot>,
    rel_by_var: &std::collections::HashMap<String, usize>,
    path_slots: &mut Vec<LogicalJoinPathSlot>,
    path_by_var: &std::collections::HashMap<String, usize>,
) -> Result<Vec<ReturnBinding>, GqlError> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut stage = Vec::with_capacity(items.len());
    for item in items {
        if let ReturnExpr::Aggregate { span, .. } = &item.expr {
            return Err(GqlError::unsupported(
                *span,
                "aggregate WITH DISTINCT projections require grouped row streams from a later read phase",
            ));
        }
        let (name, binding) = bind_join_with_item(
            item,
            node_slots,
            slot_by_var,
            property_by_var,
            value_by_var,
            rel_by_var,
            path_by_var,
        )?;
        if binding.is_aggregate() {
            return Err(GqlError::unsupported(
                item.span,
                "aggregate WITH DISTINCT projections require grouped row streams from a later read phase",
            ));
        }
        if !seen.insert(name.clone()) {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{name}`"),
            ));
        }
        register_join_with_alias(&name, &binding, rel_slots, path_slots);
        stage.push(binding.into_return_binding(name));
    }
    Ok(stage)
}

fn bind_join_projection_scope(
    items: &[ReturnItem],
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
    property_by_var: &std::collections::HashMap<String, (usize, String)>,
    value_by_var: &std::collections::HashMap<String, ReturnBinding>,
    rel_slots: &mut Vec<LogicalJoinRelSlot>,
    rel_by_var: &std::collections::HashMap<String, usize>,
    path_slots: &mut Vec<LogicalJoinPathSlot>,
    path_by_var: &std::collections::HashMap<String, usize>,
) -> Result<JoinProjectionScope, GqlError> {
    let has_aggregate = items
        .iter()
        .any(|item| matches!(&item.expr, ReturnExpr::Aggregate { .. }));
    let mut nodes = std::collections::HashMap::with_capacity(items.len());
    let mut properties = std::collections::HashMap::with_capacity(items.len());
    let mut values = std::collections::HashMap::with_capacity(items.len());
    let mut relationships = std::collections::HashMap::with_capacity(items.len());
    let mut paths = std::collections::HashMap::with_capacity(items.len());
    let mut projection_slots = Vec::with_capacity(items.len());
    let mut aggregate_group_slots = Vec::new();
    for item in items {
        let (name, binding) = bind_join_with_item(
            item,
            node_slots,
            slot_by_var,
            property_by_var,
            value_by_var,
            rel_by_var,
            path_by_var,
        )?;
        if nodes.contains_key(&name)
            || properties.contains_key(&name)
            || values.contains_key(&name)
            || relationships.contains_key(&name)
            || paths.contains_key(&name)
        {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{name}`"),
            ));
        }
        register_join_with_alias(&name, &binding, rel_slots, path_slots);
        let projection_slot = binding.clone().into_return_binding(name.clone());
        if has_aggregate && !binding.is_aggregate() {
            aggregate_group_slots.push(projection_slot.clone());
        }
        match binding {
            JoinWithBinding::Node(slot) => {
                nodes.insert(name, slot);
            }
            JoinWithBinding::Property { slot, property } => {
                properties.insert(name, (slot, property));
            }
            JoinWithBinding::Value(binding) => {
                values.insert(name, binding);
            }
            JoinWithBinding::Relationship(slot) => {
                relationships.insert(name, slot);
            }
            JoinWithBinding::Path(slot) => {
                paths.insert(name, slot);
            }
        }
        projection_slots.push(projection_slot);
    }
    Ok(JoinProjectionScope {
        nodes,
        properties,
        values,
        relationships,
        paths,
        projection_slots,
        aggregate_group_slots,
        has_new_aggregate_projection: has_aggregate,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JoinWithBinding {
    Node(usize),
    Relationship(usize),
    Path(usize),
    Value(ReturnBinding),
    Property { slot: usize, property: String },
}

impl JoinWithBinding {
    fn is_aggregate(&self) -> bool {
        matches!(self, Self::Value(binding) if binding.is_aggregate())
    }

    fn into_return_binding(self, name: String) -> ReturnBinding {
        match self {
            Self::Node(slot) => ReturnBinding::Node {
                side: BindingSide::PathNode(slot),
                name,
            },
            Self::Property { slot, property } => ReturnBinding::Property {
                side: BindingSide::PathNode(slot),
                property,
                name,
            },
            Self::Value(binding) => renamed_return_binding(binding, name),
            Self::Relationship(_) => ReturnBinding::Relationship { name },
            Self::Path(_) => ReturnBinding::Path { name },
        }
    }
}

fn renamed_return_binding(binding: ReturnBinding, name: String) -> ReturnBinding {
    match binding {
        ReturnBinding::Node { side, .. } => ReturnBinding::Node { side, name },
        ReturnBinding::Relationship { .. } => ReturnBinding::Relationship { name },
        ReturnBinding::Path { .. } => ReturnBinding::Path { name },
        ReturnBinding::PathFunction { func, path_var, .. } => ReturnBinding::PathFunction {
            func,
            path_var,
            name,
        },
        ReturnBinding::Property { side, property, .. } => ReturnBinding::Property {
            side,
            property,
            name,
        },
        ReturnBinding::Projected { source, .. } => ReturnBinding::Projected { source, name },
        ReturnBinding::Aggregate {
            func,
            arg,
            distinct,
            ..
        } => ReturnBinding::Aggregate {
            func,
            arg,
            distinct,
            name,
        },
    }
}

fn register_join_with_alias(
    name: &str,
    binding: &JoinWithBinding,
    rel_slots: &mut Vec<LogicalJoinRelSlot>,
    path_slots: &mut Vec<LogicalJoinPathSlot>,
) {
    match binding {
        JoinWithBinding::Relationship(slot) => {
            bind_join_relationship_return_alias(name, *slot, rel_slots);
        }
        JoinWithBinding::Path(slot) => {
            bind_join_path_return_alias(name, *slot, path_slots);
        }
        JoinWithBinding::Node(_) | JoinWithBinding::Property { .. } | JoinWithBinding::Value(_) => {
        }
    }
}

fn bind_join_with_item(
    item: &ReturnItem,
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
    property_by_var: &std::collections::HashMap<String, (usize, String)>,
    value_by_var: &std::collections::HashMap<String, ReturnBinding>,
    rel_by_var: &std::collections::HashMap<String, usize>,
    path_by_var: &std::collections::HashMap<String, usize>,
) -> Result<(String, JoinWithBinding), GqlError> {
    let binding = match &item.expr {
        ReturnExpr::Var { var, .. } => {
            if let Some(slot) = slot_by_var.get(&var.text).copied() {
                JoinWithBinding::Node(slot)
            } else if let Some((slot, property)) = property_by_var.get(&var.text) {
                JoinWithBinding::Property {
                    slot: *slot,
                    property: property.clone(),
                }
            } else if let Some(binding) = value_by_var.get(&var.text) {
                JoinWithBinding::Value(binding.clone())
            } else if let Some(slot) = rel_by_var.get(&var.text).copied() {
                JoinWithBinding::Relationship(slot)
            } else if let Some(slot) = path_by_var.get(&var.text).copied() {
                JoinWithBinding::Path(slot)
            } else {
                return Err(GqlError::bind(
                    var.span,
                    format!("unknown WITH variable `{}`", var.text),
                ));
            }
        }
        ReturnExpr::Property { var, property, .. } => {
            let slot = slot_by_var.get(&var.text).copied().ok_or_else(|| {
                if rel_by_var.contains_key(&var.text) {
                    GqlError::unsupported(
                        property.span,
                        "relationship properties over multi-pattern joins require a later phase",
                    )
                } else if path_by_var.contains_key(&var.text) {
                    GqlError::unsupported(
                        property.span,
                        "path properties over multi-pattern joins require a later phase",
                    )
                } else if property_by_var.contains_key(&var.text) {
                    GqlError::bind(
                        var.span,
                        format!("variable `{}` does not bind a node", var.text),
                    )
                } else {
                    GqlError::bind(var.span, format!("unknown WITH variable `{}`", var.text))
                }
            })?;
            if !node_slots[slot].properties.contains(&property.text) {
                return Err(GqlError::bind(
                    property.span,
                    format!(
                        "unknown property `{}` for label `{}`",
                        property.text, node_slots[slot].label
                    ),
                ));
            }
            JoinWithBinding::Property {
                slot,
                property: property.text.clone(),
            }
        }
        ReturnExpr::Aggregate {
            func,
            distinct,
            arg,
            ..
        } => JoinWithBinding::Value(ReturnBinding::Aggregate {
            func: bind_aggregate_func(*func),
            arg: bind_join_aggregate_arg(
                *func,
                arg,
                node_slots,
                slot_by_var,
                value_by_var,
                rel_by_var,
                path_by_var,
            )?,
            distinct: *distinct,
            name: projection_name(item),
        }),
        ReturnExpr::Func {
            name: func_name,
            args,
            span,
        } => JoinWithBinding::Value(bind_join_path_function_return(
            func_name,
            args,
            *span,
            path_by_var,
            projection_name(item),
        )?),
    };
    Ok((projection_name(item), binding))
}

fn bind_join_path_function_return(
    func_name: &ast::Ident,
    args: &[ast::Ident],
    span: Span,
    path_by_var: &std::collections::HashMap<String, usize>,
    name: String,
) -> Result<ReturnBinding, GqlError> {
    let Some(func) = path_func(&func_name.text) else {
        return Err(GqlError::unsupported(
            span,
            "RETURN functions are implemented in a later read phase",
        ));
    };
    let [arg] = args else {
        return Err(GqlError::bind(
            span,
            format!(
                "path function `{}` requires exactly one path argument",
                func_name.text
            ),
        ));
    };
    if !path_by_var.contains_key(&arg.text) {
        return Err(GqlError::bind(
            arg.span,
            format!(
                "path function `{}` requires a path variable from the multi-pattern MATCH",
                func_name.text
            ),
        ));
    }
    Ok(ReturnBinding::PathFunction {
        func,
        path_var: Some(arg.text.clone()),
        name,
    })
}

fn bind_join_returns(
    items: &[ReturnItem],
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
    property_by_var: &std::collections::HashMap<String, (usize, String)>,
    value_by_var: &std::collections::HashMap<String, ReturnBinding>,
    rel_by_var: &std::collections::HashMap<String, usize>,
    path_by_var: &std::collections::HashMap<String, usize>,
    rel_slots: &mut Vec<LogicalJoinRelSlot>,
    path_slots: &mut Vec<LogicalJoinPathSlot>,
    _has_aggregate_with_projection: bool,
) -> Result<Vec<ReturnBinding>, GqlError> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut returns = Vec::with_capacity(items.len());
    for item in items {
        let binding = match &item.expr {
            ReturnExpr::Var { var, .. } => {
                let name = projection_name(item);
                if let Some(slot) = slot_by_var.get(&var.text).copied() {
                    ReturnBinding::Node {
                        side: BindingSide::PathNode(slot),
                        name,
                    }
                } else if let Some((slot, property)) = property_by_var.get(&var.text) {
                    ReturnBinding::Property {
                        side: BindingSide::PathNode(*slot),
                        property: property.clone(),
                        name,
                    }
                } else if let Some(binding) = value_by_var.get(&var.text) {
                    renamed_return_binding(binding.clone(), name)
                } else if let Some(rel_slot) = rel_by_var.get(&var.text).copied() {
                    bind_join_relationship_return_alias(&name, rel_slot, rel_slots);
                    ReturnBinding::Relationship { name }
                } else if let Some(path_slot) = path_by_var.get(&var.text).copied() {
                    bind_join_path_return_alias(&name, path_slot, path_slots);
                    ReturnBinding::Path { name }
                } else {
                    return Err(GqlError::bind(
                        var.span,
                        format!("unknown return variable `{}`", var.text),
                    ));
                }
            }
            ReturnExpr::Property { var, property, .. } => {
                let slot = slot_by_var.get(&var.text).copied().ok_or_else(|| {
                    if rel_by_var.contains_key(&var.text) {
                        GqlError::unsupported(
                            property.span,
                            "relationship properties over multi-pattern joins require a later phase",
                        )
                    } else if path_by_var.contains_key(&var.text) {
                        GqlError::unsupported(
                            property.span,
                            "path properties over multi-pattern joins require a later phase",
                        )
                    } else if property_by_var.contains_key(&var.text) {
                        GqlError::bind(
                            var.span,
                            format!("variable `{}` does not bind a node", var.text),
                        )
                    } else {
                        GqlError::bind(
                            var.span,
                            format!("unknown return variable `{}`", var.text),
                        )
                    }
                })?;
                if !node_slots[slot].properties.contains(&property.text) {
                    return Err(GqlError::bind(
                        property.span,
                        format!(
                            "unknown property `{}` for label `{}`",
                            property.text, node_slots[slot].label
                        ),
                    ));
                }
                ReturnBinding::Property {
                    side: BindingSide::PathNode(slot),
                    property: property.text.clone(),
                    name: projection_name(item),
                }
            }
            ReturnExpr::Aggregate {
                func,
                distinct,
                arg,
                span: _,
                ..
            } => ReturnBinding::Aggregate {
                func: bind_aggregate_func(*func),
                arg: bind_join_aggregate_arg(
                    *func,
                    arg,
                    node_slots,
                    slot_by_var,
                    value_by_var,
                    rel_by_var,
                    path_by_var,
                )?,
                distinct: *distinct,
                name: projection_name(item),
            },
            ReturnExpr::Func {
                name: func_name,
                args,
                span,
            } => bind_join_path_function_return(
                func_name,
                args,
                *span,
                path_by_var,
                projection_name(item),
            )?,
        };
        if !seen.insert(binding.name().to_string()) {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{}`", binding.name()),
            ));
        }
        returns.push(binding);
    }
    Ok(returns)
}

fn bind_join_aggregate_arg(
    func: ast::AggregateFunc,
    arg: &ast::AggregateArg,
    node_slots: &[LogicalJoinNodeSlot],
    slot_by_var: &std::collections::HashMap<String, usize>,
    value_by_var: &std::collections::HashMap<String, ReturnBinding>,
    rel_by_var: &std::collections::HashMap<String, usize>,
    path_by_var: &std::collections::HashMap<String, usize>,
) -> Result<AggregateArg, GqlError> {
    match arg {
        ast::AggregateArg::All { span } => {
            if func == ast::AggregateFunc::Count {
                Ok(AggregateArg::All)
            } else {
                Err(GqlError::bind(
                    *span,
                    "only count(*) may use '*' as an aggregate argument",
                ))
            }
        }
        ast::AggregateArg::Var { var, span } => {
            if let Some(slot) = slot_by_var.get(&var.text).copied() {
                if aggregate_accepts_value(func) {
                    Ok(AggregateArg::Node(BindingSide::PathNode(slot)))
                } else {
                    Err(GqlError::bind(
                        *span,
                        format!(
                            "aggregate `{}` requires a property argument",
                            aggregate_name(func)
                        ),
                    ))
                }
            } else if let Some(rel_slot) = rel_by_var.get(&var.text).copied() {
                if aggregate_accepts_value(func) {
                    Ok(AggregateArg::JoinRelationship(rel_slot))
                } else {
                    Err(GqlError::bind(
                        *span,
                        format!(
                            "aggregate `{}` requires a property argument",
                            aggregate_name(func)
                        ),
                    ))
                }
            } else if let Some(path_slot) = path_by_var.get(&var.text).copied() {
                if aggregate_accepts_value(func) {
                    Ok(AggregateArg::JoinPath(path_slot))
                } else {
                    Err(GqlError::bind(
                        *span,
                        format!(
                            "aggregate `{}` requires a property argument",
                            aggregate_name(func)
                        ),
                    ))
                }
            } else if let Some(ReturnBinding::Projected { source, .. }) =
                value_by_var.get(&var.text)
            {
                Ok(AggregateArg::Projected {
                    name: source.clone(),
                })
            } else {
                Err(GqlError::bind(
                    *span,
                    format!("unknown aggregate variable `{}`", var.text),
                ))
            }
        }
        ast::AggregateArg::Property {
            var,
            property,
            span: _,
        } => {
            let slot = slot_by_var.get(&var.text).copied().ok_or_else(|| {
                if rel_by_var.contains_key(&var.text) {
                    GqlError::unsupported(
                        property.span,
                        "relationship properties over multi-pattern joins require a later phase",
                    )
                } else if path_by_var.contains_key(&var.text) {
                    GqlError::unsupported(
                        property.span,
                        "path properties over multi-pattern joins require a later phase",
                    )
                } else {
                    GqlError::bind(
                        var.span,
                        format!("unknown aggregate variable `{}`", var.text),
                    )
                }
            })?;
            if !node_slots[slot].properties.contains(&property.text) {
                return Err(GqlError::bind(
                    property.span,
                    format!(
                        "unknown property `{}` for label `{}`",
                        property.text, node_slots[slot].label
                    ),
                ));
            }
            Ok(AggregateArg::Property {
                side: BindingSide::PathNode(slot),
                property: property.text.clone(),
            })
        }
    }
}

fn bind_join_relationship_return_alias(
    name: &str,
    rel_slot: usize,
    rel_slots: &mut Vec<LogicalJoinRelSlot>,
) {
    if rel_slots.iter().any(|slot| slot.var == name) {
        return;
    }
    let pattern_slot = rel_slots[rel_slot].pattern_slot;
    rel_slots.push(LogicalJoinRelSlot {
        var: name.to_string(),
        pattern_slot,
    });
}

fn bind_join_path_return_alias(
    name: &str,
    path_slot: usize,
    path_slots: &mut Vec<LogicalJoinPathSlot>,
) {
    if path_slots.iter().any(|slot| slot.var == name) {
        return;
    }
    let pattern_slot = path_slots[path_slot].pattern_slot;
    path_slots.push(LogicalJoinPathSlot {
        var: name.to_string(),
        pattern_slot,
    });
}

fn bind_wildcard_path_read(
    query: &crate::gql::ast::Query,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalWildcardPathPlan, GqlError> {
    validate_row_window(query)?;
    if query.match_.optional {
        return Err(GqlError::unsupported(
            query.match_.span,
            "OPTIONAL MATCH path variables require a later read phase",
        ));
    }
    if !query.with_.is_empty() || !query.order_by.is_empty() || query.return_.distinct {
        return Err(GqlError::unsupported(
            query.span,
            "wildcard path variables support only WHERE, RETURN, SKIP, and LIMIT in this phase",
        ));
    }
    let Pattern {
        path_var,
        start,
        tail,
        ..
    } = &query.match_.pattern;
    let path_var = path_var
        .as_ref()
        .expect("caller only routes path-variable patterns")
        .text
        .clone();
    if tail.is_empty() {
        return Err(GqlError::unsupported(
            query.match_.pattern.span,
            "wildcard path variables require at least one relationship",
        ));
    }
    if tail.len() > 1 && tail.iter().any(|(rel, _)| rel.var_len.is_some()) {
        return Err(GqlError::unsupported(
            query.match_.pattern.span,
            "variable-length wildcard paths with multiple segments require a later phase",
        ));
    }
    let source_table_filter = bind_wildcard_node_filter(start, catalog)?;
    let mut segments = Vec::with_capacity(tail.len());
    let mut previous_table_filter = source_table_filter;
    for (rel, target) in tail {
        let target_table_filter = bind_wildcard_node_filter(target, catalog)?;
        let rel_type_filter = bind_wildcard_relationship_filter(rel, catalog)?;
        let hops = bind_hops(rel)?;
        if tail.len() > 1 && rel.var.is_some() {
            return Err(GqlError::unsupported(
                rel.span,
                "relationship variables in multi-segment path variables require a later phase",
            ));
        }
        if rel.var_len.is_some() {
            if rel.var.is_some() {
                return Err(GqlError::unsupported(
                    rel.span,
                    "relationship variables on variable-length wildcard paths require a later phase",
                ));
            }
            if target.var.is_some() {
                return Err(GqlError::unsupported(
                    target.span,
                    "named target nodes on variable-length wildcard paths require a later phase",
                ));
            }
        }
        if let (Some(source_table_oid), Some(target_table_oid), Some(rel_type)) = (
            previous_table_filter,
            target_table_filter,
            rel_type_filter.as_ref(),
        ) {
            let source = BoundNode {
                var: String::new(),
                label: String::new(),
                table_oid: source_table_oid,
                properties: std::collections::BTreeSet::new(),
            };
            let target_node = BoundNode {
                var: target
                    .var
                    .as_ref()
                    .map_or_else(String::new, |var| var.text.clone()),
                label: target
                    .label
                    .as_ref()
                    .map_or_else(String::new, |label| label.text.clone()),
                table_oid: target_table_oid,
                properties: std::collections::BTreeSet::new(),
            };
            let rel_ident = rel
                .rel_type
                .as_ref()
                .expect("rel_type_filter came from rel");
            resolve_relationship(catalog, rel, rel_ident, &source, &target_node)?;
            debug_assert_eq!(rel_type, &rel_ident.text);
        }
        segments.push(LogicalWildcardPathSegment {
            rel_var: rel.var.as_ref().map(|var| var.text.clone()),
            target_var: target.var.as_ref().map(|var| var.text.clone()),
            direction: bind_direction(rel.direction),
            hops,
            target_table_filter,
            rel_type_filter,
        });
        previous_table_filter = target_table_filter;
    }
    let source_var = start.var.as_ref().map(|var| var.text.clone());
    let (rel_var, target_var, direction, target_table_filter, rel_type_filter) = {
        let first_segment = &segments[0];
        (
            first_segment.rel_var.clone(),
            first_segment.target_var.clone(),
            first_segment.direction,
            first_segment.target_table_filter,
            first_segment.rel_type_filter.clone(),
        )
    };
    let returns =
        bind_wildcard_path_returns(&query.return_.items, &path_var, start.var.as_ref(), tail)?;
    let predicate = bind_wildcard_path_predicate(query.where_.as_ref(), start, tail, catalog)?;
    let node_labels = catalog.node_labels();
    let table_labels = node_labels
        .iter()
        .map(|label| (label.table_oid, label.label.clone()))
        .collect();
    let required_node_table_oids = node_labels
        .iter()
        .map(|label| label.table_oid)
        .collect::<std::collections::BTreeSet<_>>();
    let rel_type_labels = catalog
        .rel_types()
        .into_iter()
        .map(|rel| rel.rel_type)
        .collect::<std::collections::BTreeSet<_>>();
    Ok(LogicalWildcardPathPlan {
        path_var,
        source_var,
        rel_var,
        target_var,
        direction,
        returns,
        source_table_filter,
        target_table_filter,
        rel_type_filter,
        segments,
        required_node_table_oids,
        table_labels,
        rel_type_labels,
        predicate,
        skip: query.skip,
        limit: query.limit,
    })
}

fn bind_wildcard_path_predicate(
    where_: Option<&Expr>,
    start: &NodePat,
    tail: &[(RelPat, NodePat)],
    catalog: &impl CatalogSnapshot,
) -> Result<Option<Predicate>, GqlError> {
    let Some(expr) = where_ else {
        return Ok(None);
    };
    let node_positions = wildcard_node_var_positions(start, tail)?;
    let possible_tables = possible_wildcard_node_tables(start, tail, catalog)?;
    let node_labels = catalog.node_labels();
    let properties_by_table = node_labels
        .iter()
        .map(|label| (label.table_oid, label.properties.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    bind_wildcard_expr(
        expr,
        &node_positions,
        &possible_tables,
        &properties_by_table,
        0,
    )
    .map(Some)
}

fn wildcard_node_var_positions(
    start: &NodePat,
    tail: &[(RelPat, NodePat)],
) -> Result<std::collections::HashMap<String, usize>, GqlError> {
    let mut positions = std::collections::HashMap::new();
    if let Some(var) = &start.var {
        positions.insert(var.text.clone(), 0);
    }
    for (idx, (_, target)) in tail.iter().enumerate() {
        if let Some(var) = &target.var {
            if positions.insert(var.text.clone(), idx + 1).is_some() {
                return Err(GqlError::bind(
                    var.span,
                    format!("duplicate variable `{}` in MATCH scope", var.text),
                ));
            }
        }
    }
    Ok(positions)
}

fn possible_wildcard_node_tables(
    start: &NodePat,
    tail: &[(RelPat, NodePat)],
    catalog: &impl CatalogSnapshot,
) -> Result<Vec<std::collections::BTreeSet<u32>>, GqlError> {
    let node_labels = catalog.node_labels();
    let all_tables = node_labels
        .iter()
        .map(|label| label.table_oid)
        .collect::<std::collections::BTreeSet<_>>();
    let mut allowed = Vec::with_capacity(tail.len() + 1);
    allowed.push(wildcard_allowed_tables_for_node(
        start,
        catalog,
        &all_tables,
    )?);
    for (_, target) in tail {
        allowed.push(wildcard_allowed_tables_for_node(
            target,
            catalog,
            &all_tables,
        )?);
    }

    let rels = catalog.rel_types();
    let segment_pairs = tail
        .iter()
        .map(|(rel, _)| wildcard_segment_table_pairs(rel, &rels))
        .collect::<Vec<_>>();
    let mut possible = vec![std::collections::BTreeSet::new(); tail.len() + 1];
    let mut path = Vec::with_capacity(tail.len() + 1);
    for table_oid in &allowed[0] {
        path.clear();
        path.push(*table_oid);
        collect_possible_wildcard_table_paths(
            0,
            *table_oid,
            &allowed,
            &segment_pairs,
            &mut path,
            &mut possible,
        );
    }
    Ok(possible)
}

fn wildcard_allowed_tables_for_node(
    node: &NodePat,
    catalog: &impl CatalogSnapshot,
    all_tables: &std::collections::BTreeSet<u32>,
) -> Result<std::collections::BTreeSet<u32>, GqlError> {
    node.label.as_ref().map_or_else(
        || Ok(all_tables.clone()),
        |label| {
            catalog
                .resolve_node_label(&label.text, label.span)
                .map(|info| [info.table_oid].into_iter().collect())
        },
    )
}

fn wildcard_segment_table_pairs(
    rel: &RelPat,
    rels: &[super::catalog_snapshot::RelTypeInfo],
) -> Vec<(u32, u32)> {
    let mut pairs = std::collections::BTreeSet::new();
    for rel_info in rels {
        if rel
            .rel_type
            .as_ref()
            .is_some_and(|filter| filter.text != rel_info.rel_type)
        {
            continue;
        }
        match rel.direction {
            Direction::Out => {
                pairs.insert((rel_info.from_table_oid, rel_info.to_table_oid));
            }
            Direction::In => {
                pairs.insert((rel_info.to_table_oid, rel_info.from_table_oid));
            }
            Direction::Undirected => {
                pairs.insert((rel_info.from_table_oid, rel_info.to_table_oid));
                pairs.insert((rel_info.to_table_oid, rel_info.from_table_oid));
            }
        }
    }
    pairs.into_iter().collect()
}

fn collect_possible_wildcard_table_paths(
    segment_idx: usize,
    current_table: u32,
    allowed: &[std::collections::BTreeSet<u32>],
    segment_pairs: &[Vec<(u32, u32)>],
    path: &mut Vec<u32>,
    possible: &mut [std::collections::BTreeSet<u32>],
) {
    let Some(pairs) = segment_pairs.get(segment_idx) else {
        for (idx, table_oid) in path.iter().enumerate() {
            possible[idx].insert(*table_oid);
        }
        return;
    };
    for (from_table_oid, to_table_oid) in pairs {
        if *from_table_oid != current_table || !allowed[segment_idx + 1].contains(to_table_oid) {
            continue;
        }
        path.push(*to_table_oid);
        collect_possible_wildcard_table_paths(
            segment_idx + 1,
            *to_table_oid,
            allowed,
            segment_pairs,
            path,
            possible,
        );
        path.pop();
    }
}

fn bind_wildcard_expr(
    expr: &Expr,
    node_positions: &std::collections::HashMap<String, usize>,
    possible_tables: &[std::collections::BTreeSet<u32>],
    properties_by_table: &std::collections::HashMap<u32, std::collections::BTreeSet<String>>,
    depth: usize,
) -> Result<Predicate, GqlError> {
    if depth > MAX_BOUND_PREDICATE_DEPTH {
        return Err(GqlError::syntax(
            expr_span(expr),
            "predicate expression is too deeply nested",
        ));
    }
    match expr {
        Expr::And { lhs, rhs, .. } => Ok(Predicate::And(
            Box::new(bind_wildcard_expr(
                lhs,
                node_positions,
                possible_tables,
                properties_by_table,
                depth + 1,
            )?),
            Box::new(bind_wildcard_expr(
                rhs,
                node_positions,
                possible_tables,
                properties_by_table,
                depth + 1,
            )?),
        )),
        Expr::Or { lhs, rhs, .. } => Ok(Predicate::Or(
            Box::new(bind_wildcard_expr(
                lhs,
                node_positions,
                possible_tables,
                properties_by_table,
                depth + 1,
            )?),
            Box::new(bind_wildcard_expr(
                rhs,
                node_positions,
                possible_tables,
                properties_by_table,
                depth + 1,
            )?),
        )),
        Expr::Not { expr, .. } => Ok(Predicate::Not(Box::new(bind_wildcard_expr(
            expr,
            node_positions,
            possible_tables,
            properties_by_table,
            depth + 1,
        )?))),
        Expr::Compare { lhs, op, rhs, .. } => Ok(Predicate::Compare {
            lhs: bind_wildcard_operand(lhs, node_positions, possible_tables, properties_by_table)?,
            op: bind_cmp_op(*op),
            rhs: rhs
                .as_ref()
                .map(|operand| {
                    bind_wildcard_operand(
                        operand,
                        node_positions,
                        possible_tables,
                        properties_by_table,
                    )
                })
                .transpose()?,
        }),
    }
}

fn bind_wildcard_operand(
    operand: &Operand,
    node_positions: &std::collections::HashMap<String, usize>,
    possible_tables: &[std::collections::BTreeSet<u32>],
    properties_by_table: &std::collections::HashMap<u32, std::collections::BTreeSet<String>>,
) -> Result<ValueExpr, GqlError> {
    match operand {
        Operand::Property {
            var,
            property,
            span: _,
        } => {
            let node_idx = node_positions.get(&var.text).copied().ok_or_else(|| {
                GqlError::bind(var.span, format!("unknown variable `{}`", var.text))
            })?;
            let possible = possible_tables.get(node_idx).ok_or_else(|| {
                GqlError::bind(var.span, format!("unknown variable `{}`", var.text))
            })?;
            if possible.is_empty() {
                return Err(GqlError::unsupported(
                    property.span,
                    "wildcard property predicates require at least one possible concrete node label",
                ));
            }
            if possible.iter().any(|table_oid| {
                !properties_by_table
                    .get(table_oid)
                    .is_some_and(|properties| properties.contains(&property.text))
            }) {
                return Err(GqlError::unsupported(
                    property.span,
                    format!(
                        "wildcard property `{}` must be available on every possible concrete node label",
                        property.text
                    ),
                ));
            }
            Ok(ValueExpr::Property {
                side: BindingSide::PathNode(node_idx),
                property: property.text.clone(),
            })
        }
        Operand::Literal(literal) => Ok(ValueExpr::Literal(literal_json(literal))),
        Operand::Param { name, .. } => Ok(ValueExpr::Param(name.text.clone())),
        Operand::List { values, .. } => {
            Ok(ValueExpr::List(values.iter().map(literal_json).collect()))
        }
    }
}

fn bind_wildcard_node_filter(
    node: &NodePat,
    catalog: &impl CatalogSnapshot,
) -> Result<Option<u32>, GqlError> {
    if !node.props.is_empty() {
        return Err(GqlError::unsupported(
            node.span,
            "wildcard path variables cannot bind node properties in this phase",
        ));
    }
    node.label
        .as_ref()
        .map(|label| {
            catalog
                .resolve_node_label(&label.text, label.span)
                .map(|info| info.table_oid)
        })
        .transpose()
}

fn bind_wildcard_relationship_filter(
    rel: &RelPat,
    catalog: &impl CatalogSnapshot,
) -> Result<Option<String>, GqlError> {
    if !rel.props.is_empty() {
        return Err(GqlError::unsupported(
            rel.span,
            "wildcard path variables cannot bind relationship properties in this phase",
        ));
    }
    let Some(rel_type) = &rel.rel_type else {
        return Ok(None);
    };
    if catalog
        .rel_types()
        .into_iter()
        .any(|info| info.rel_type == rel_type.text)
    {
        return Ok(Some(rel_type.text.clone()));
    }
    Err(GqlError::bind(
        rel_type.span,
        format!("unknown relationship type `{}`", rel_type.text),
    ))
}

fn bind_wildcard_path_returns(
    items: &[ReturnItem],
    path_var: &str,
    source_var: Option<&ast::Ident>,
    tail: &[(RelPat, NodePat)],
) -> Result<Vec<ReturnBinding>, GqlError> {
    let mut scope = BindingScope::from([(path_var.to_string(), ScopedBinding::Path)]);
    insert_wildcard_binding(
        &mut scope,
        path_var,
        source_var,
        ScopedBinding::Node(BindingSide::PathNode(0)),
    )?;
    for (idx, (rel, target)) in tail.iter().enumerate() {
        insert_wildcard_binding(
            &mut scope,
            path_var,
            rel.var.as_ref(),
            ScopedBinding::Relationship { var_len: false },
        )?;
        insert_wildcard_binding(
            &mut scope,
            path_var,
            target.var.as_ref(),
            ScopedBinding::Node(BindingSide::PathNode(idx + 1)),
        )?;
    }
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut bindings = Vec::with_capacity(items.len());
    for item in items {
        let name = projection_name(item);
        if !seen.insert(name.clone()) {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{name}`"),
            ));
        }
        let binding = match &item.expr {
            ReturnExpr::Var { var, span } => match scope.get(&var.text) {
                Some(ScopedBinding::Path) => ReturnBinding::Path { name },
                Some(ScopedBinding::Node(side)) => ReturnBinding::Node { side: *side, name },
                Some(ScopedBinding::Relationship { .. }) => ReturnBinding::Relationship { name },
                Some(_) => unreachable!("wildcard path scope stores only element bindings"),
                None => {
                    return Err(GqlError::bind(
                        *span,
                        format!("unknown return variable `{}`", var.text),
                    ));
                }
            },
            ReturnExpr::Func {
                name: func_name,
                args,
                span,
            } => match bind_path_function(func_name, args, *span, &scope)? {
                ScopedBinding::PathFunction(func) => ReturnBinding::PathFunction {
                    func,
                    path_var: None,
                    name,
                },
                _ => unreachable!("path functions bind only to path-function slots"),
            },
            ReturnExpr::Property { span, .. } => {
                return Err(GqlError::unsupported(
                    *span,
                    "wildcard path variables do not support property projection in this phase",
                ));
            }
            ReturnExpr::Aggregate { span, .. } => {
                return Err(GqlError::unsupported(
                    *span,
                    "wildcard path variables do not support aggregates in this phase",
                ));
            }
        };
        bindings.push(binding);
    }
    Ok(bindings)
}

fn insert_wildcard_binding(
    scope: &mut BindingScope,
    path_var: &str,
    var: Option<&ast::Ident>,
    binding: ScopedBinding,
) -> Result<(), GqlError> {
    let Some(var) = var else {
        return Ok(());
    };
    if var.text == path_var || scope.insert(var.text.clone(), binding).is_some() {
        return Err(GqlError::bind(
            var.span,
            format!("duplicate variable `{}` in MATCH scope", var.text),
        ));
    }
    Ok(())
}

fn bind_create_node(
    query: &crate::gql::ast::CreateQuery,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalCreateNode, GqlError> {
    if query.return_.distinct {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN DISTINCT is implemented in a later phase",
        ));
    }
    let node = bind_node(&query.create.node, catalog)?;
    if query.create.node.props.is_empty() {
        return Err(GqlError::unsupported(
            query.create.node.span,
            "CREATE requires a property map for mapped node writes",
        ));
    }
    let properties = bind_create_properties(&query.create.node, &node)?;
    let returns = bind_write_returns(&query.return_.items, &node, false)?;
    Ok(LogicalCreateNode {
        node,
        properties,
        returns,
    })
}

fn bind_merge_node(
    query: &crate::gql::ast::MergeQuery,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalMergeNode, GqlError> {
    if query.return_.distinct {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN DISTINCT over MERGE is implemented in a later phase",
        ));
    }
    let node = bind_node(&query.merge.node, catalog)?;
    if query.merge.node.props.is_empty() {
        return Err(GqlError::unsupported(
            query.merge.node.span,
            "MERGE requires a property map for mapped node identity",
        ));
    }
    let properties = bind_create_properties(&query.merge.node, &node)?;
    let writable_properties =
        writable_properties_for_label(&node.label, catalog, query.merge.node.span)?;
    let on_create = query
        .on_create
        .as_ref()
        .map(|set| bind_merge_set_property(set, &node, &writable_properties, "ON CREATE"))
        .transpose()?;
    let on_match = query
        .on_match
        .as_ref()
        .map(|set| bind_merge_set_property(set, &node, &writable_properties, "ON MATCH"))
        .transpose()?;
    if let Some(on_create) = &on_create {
        if properties
            .iter()
            .any(|property| property.property == on_create.property)
        {
            return Err(GqlError::bind(
                query
                    .on_create
                    .as_ref()
                    .map_or(query.merge.span, |set| set.target.property.span),
                format!("duplicate MERGE property `{}`", on_create.property),
            ));
        }
    }
    let returns = bind_write_returns(&query.return_.items, &node, false)?;
    Ok(LogicalMergeNode {
        node,
        properties,
        on_create,
        on_match,
        returns,
    })
}

fn bind_merge_set_property(
    set: &crate::gql::ast::SetClause,
    node: &BoundNode,
    writable_properties: &std::collections::BTreeSet<String>,
    branch: &str,
) -> Result<CreateProperty, GqlError> {
    if set.target.var.text != node.var {
        return Err(GqlError::bind(
            set.target.var.span,
            format!("unknown MERGE {branch} variable `{}`", set.target.var.text),
        ));
    }
    let property = set.target.property.text.clone();
    if property.contains('.') {
        return Err(GqlError::unsupported(
            set.target.property.span,
            "MERGE branch writes to jsonb property paths require a later write phase",
        ));
    }
    if property.starts_with('_') {
        return Err(GqlError::bind(
            set.target.property.span,
            format!("reserved GQL property key `{property}`"),
        ));
    }
    if !writable_properties.contains(&property) {
        return Err(GqlError::bind(
            set.target.property.span,
            format!("property `{property}` is not a writable mapped column"),
        ));
    }
    Ok(CreateProperty {
        property,
        value: bind_create_value(&set.value)?,
    })
}

fn bind_create_properties(
    node_pat: &NodePat,
    node: &BoundNode,
) -> Result<Vec<CreateProperty>, GqlError> {
    let mut seen = std::collections::HashSet::with_capacity(node_pat.props.len());
    let mut properties = Vec::with_capacity(node_pat.props.len());
    for (key, value) in &node_pat.props {
        if !node.properties.contains(&key.text) {
            return Err(GqlError::bind(
                key.span,
                format!("unknown property `{}`", key.text),
            ));
        }
        if key.text.contains('.') {
            return Err(GqlError::unsupported(
                key.span,
                "writes to jsonb property paths require the Phase 4 jsonb write path",
            ));
        }
        if !seen.insert(key.text.as_str()) {
            return Err(GqlError::bind(
                key.span,
                format!("duplicate CREATE property `{}`", key.text),
            ));
        }
        let value = bind_create_value(value)?;
        properties.push(CreateProperty {
            property: key.text.clone(),
            value,
        });
    }
    Ok(properties)
}

fn bind_create_value(value: &Operand) -> Result<CreateValue, GqlError> {
    match value {
        Operand::Literal(Literal::Value { value, .. }) => Ok(CreateValue::Literal(value.clone())),
        Operand::Param { name, .. } => Ok(CreateValue::Param(name.text.clone())),
        Operand::List { span, .. } => Err(GqlError::unsupported(
            *span,
            "write property lists are implemented in a later write phase",
        )),
        Operand::Property { span, .. } => Err(GqlError::unsupported(
            *span,
            "write property references require MATCH writes from a later phase",
        )),
    }
}

fn bind_write_returns(
    items: &[ReturnItem],
    node: &BoundNode,
    allow_jsonb_paths: bool,
) -> Result<Vec<CreateReturnBinding>, GqlError> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut bindings = Vec::with_capacity(items.len());
    for item in items {
        let binding = match &item.expr {
            ReturnExpr::Var { var, .. } if var.text == node.var => CreateReturnBinding::Node {
                name: item
                    .alias
                    .as_ref()
                    .map_or_else(|| var.text.clone(), |alias| alias.text.clone()),
            },
            ReturnExpr::Property { var, property, .. } if var.text == node.var => {
                validate_property(
                    BindingSide::Source,
                    &property.text,
                    node,
                    node,
                    property.span,
                )?;
                if property.text.contains('.') && !allow_jsonb_paths {
                    return Err(GqlError::unsupported(
                        property.span,
                        "write RETURN jsonb property paths require the Phase 4 jsonb write path",
                    ));
                }
                CreateReturnBinding::Property {
                    property: property.text.clone(),
                    name: item
                        .alias
                        .as_ref()
                        .map_or_else(|| property.text.clone(), |alias| alias.text.clone()),
                }
            }
            ReturnExpr::Var { var, span } => {
                return Err(GqlError::bind(
                    *span,
                    format!("unknown return variable `{}`", var.text),
                ));
            }
            ReturnExpr::Property { var, span, .. } => {
                return Err(GqlError::bind(
                    *span,
                    format!("unknown return variable `{}`", var.text),
                ));
            }
            ReturnExpr::Func { span, .. } => {
                return Err(GqlError::unsupported(
                    *span,
                    "RETURN functions over CREATE are implemented in a later phase",
                ));
            }
            ReturnExpr::Aggregate { span, .. } => {
                return Err(GqlError::unsupported(
                    *span,
                    "RETURN aggregates over CREATE are implemented in a later phase",
                ));
            }
        };
        if !seen.insert(binding.name().to_string()) {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{}`", binding.name()),
            ));
        }
        bindings.push(binding);
    }
    Ok(bindings)
}

fn bind_set_property(
    query: &crate::gql::ast::SetQuery,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalSetProperty, GqlError> {
    if query.return_.distinct {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN DISTINCT over SET is implemented in a later phase",
        ));
    }
    if query.match_.optional {
        return Err(GqlError::unsupported(
            query.match_.span,
            "OPTIONAL MATCH is only supported for read queries",
        ));
    }
    let Pattern { start, tail, .. } = &query.match_.pattern;
    if !tail.is_empty() {
        return Err(GqlError::unsupported(
            query.match_.pattern.span,
            "SET supports a single-node MATCH pattern in this release",
        ));
    }
    let node = bind_node(start, catalog)?;
    let writable_properties = writable_properties_for_match_start(start, catalog)?;
    if query.set.target.var.text != node.var {
        return Err(GqlError::bind(
            query.set.target.var.span,
            format!("unknown SET variable `{}`", query.set.target.var.text),
        ));
    }
    let property = query.set.target.property.text.clone();
    if property.contains('.') {
        return Err(GqlError::unsupported(
            query.set.target.property.span,
            "writes to jsonb property paths require the Phase 4 jsonb write path",
        ));
    }
    if property.starts_with('_') {
        return Err(GqlError::bind(
            query.set.target.property.span,
            format!("reserved GQL property key `{property}`"),
        ));
    }
    if !writable_properties.contains(&property) {
        return Err(GqlError::bind(
            query.set.target.property.span,
            format!("property `{property}` is not a writable mapped column"),
        ));
    }
    let value = bind_create_value(&query.set.value)?;
    let predicate = bind_node_predicates(query.where_.as_ref(), start, &node)?;
    let returns = bind_write_returns(&query.return_.items, &node, false)?;
    Ok(LogicalSetProperty {
        node,
        predicate,
        property,
        value,
        returns,
    })
}

fn bind_remove_property(
    query: &crate::gql::ast::RemoveQuery,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalRemoveProperty, GqlError> {
    if query.return_.distinct {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN DISTINCT over REMOVE is implemented in a later phase",
        ));
    }
    if query.match_.optional {
        return Err(GqlError::unsupported(
            query.match_.span,
            "OPTIONAL MATCH is only supported for read queries",
        ));
    }
    let Pattern { start, tail, .. } = &query.match_.pattern;
    if !tail.is_empty() {
        return Err(GqlError::unsupported(
            query.match_.pattern.span,
            "REMOVE supports a single-node MATCH pattern in this release",
        ));
    }
    let node = bind_node(start, catalog)?;
    let writable_properties = writable_properties_for_match_start(start, catalog)?;
    let property = match &query.remove.target {
        crate::gql::ast::RemoveTarget::Property(target) => {
            if target.var.text != node.var {
                return Err(GqlError::bind(
                    target.var.span,
                    format!("unknown REMOVE variable `{}`", target.var.text),
                ));
            }
            let property = target.property.text.clone();
            if property.starts_with('_') {
                return Err(GqlError::bind(
                    target.property.span,
                    format!("reserved GQL property key `{property}`"),
                ));
            }
            if !writable_properties.contains(&property) {
                return Err(GqlError::bind(
                    target.property.span,
                    format!("property `{property}` is not a writable mapped column"),
                ));
            }
            property
        }
        crate::gql::ast::RemoveTarget::Label { var, label, .. } => {
            if var.text != node.var {
                return Err(GqlError::bind(
                    var.span,
                    format!("unknown REMOVE variable `{}`", var.text),
                ));
            }
            return Err(GqlError::unsupported(
                label.span,
                "REMOVE label is not supported because labels map to registered source tables",
            ));
        }
    };
    let predicate = bind_node_predicates(query.where_.as_ref(), start, &node)?;
    let returns = bind_write_returns(&query.return_.items, &node, true)?;
    Ok(LogicalRemoveProperty {
        node,
        predicate,
        property,
        returns,
    })
}

fn writable_properties_for_match_start(
    start: &NodePat,
    catalog: &impl CatalogSnapshot,
) -> Result<std::collections::BTreeSet<String>, GqlError> {
    start
        .label
        .as_ref()
        .map(|label| catalog.resolve_node_label(&label.text, label.span))
        .transpose()
        .map(|info| {
            info.map(|info| info.writable_properties)
                .unwrap_or_default()
        })
}

fn writable_properties_for_label(
    label: &str,
    catalog: &impl CatalogSnapshot,
    span: Span,
) -> Result<std::collections::BTreeSet<String>, GqlError> {
    catalog
        .resolve_node_label(label, span)
        .map(|info| info.writable_properties)
}

fn bind_delete_edge(
    query: &crate::gql::ast::DeleteQuery,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalDeleteEdge, GqlError> {
    if query.match_.optional {
        return Err(GqlError::unsupported(
            query.match_.span,
            "OPTIONAL MATCH is only supported for read queries",
        ));
    }
    if query.return_.distinct {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN DISTINCT is implemented in a later phase",
        ));
    }
    let (source_pat, rel_pat, target_pat) = single_outbound_hop(&query.match_)?;
    if rel_pat.var_len.is_some() {
        return Err(GqlError::unsupported(
            rel_pat.span,
            "DELETE supports only a single matched relationship in this release",
        ));
    }
    if rel_pat.direction == Direction::Undirected {
        return Err(GqlError::unsupported(
            rel_pat.span,
            "DELETE requires a directed relationship pattern in this release",
        ));
    }
    let rel_var = rel_pat.var.as_ref().ok_or_else(|| {
        GqlError::bind(
            rel_pat.span,
            "DELETE requires a named relationship variable",
        )
    })?;
    if query.delete.var.text != rel_var.text {
        return Err(GqlError::bind(
            query.delete.var.span,
            format!("unknown DELETE variable `{}`", query.delete.var.text),
        ));
    }
    let DeleteEdgeBinding {
        source,
        target,
        rel_info,
        edge_mapping,
    } = bind_delete_edge_mapping(source_pat, rel_pat, target_pat, catalog)?;
    let predicate = bind_predicates(
        query.where_.as_ref(),
        source_pat,
        rel_pat,
        target_pat,
        &source,
        &target,
    )?;
    let scope = initial_relationship_scope(rel_pat, target_pat, &source, &target)?;
    let returns = bind_scoped_returns(&query.return_.items, &scope, &source, &target)?;
    if returns.iter().any(ReturnBinding::is_aggregate) {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN aggregates over DELETE are implemented in a later phase",
        ));
    }
    Ok(LogicalDeleteEdge {
        source,
        relationship: BoundRel {
            var: Some(rel_var.text.clone()),
            rel_type: rel_info.rel_type,
            direction: bind_direction(rel_pat.direction),
            hops: HopBounds {
                variable: false,
                min: 1,
                max: 1,
            },
        },
        rel_var: rel_var.text.clone(),
        target,
        edge: BoundMappedEdge {
            edge_table_oid: edge_mapping.edge_table_oid,
            source_table_oid: edge_mapping.source_table_oid,
            target_table_oid: edge_mapping.target_table_oid,
            source_column: edge_mapping.source_column,
            target_column: edge_mapping.target_column,
            bidirectional: edge_mapping.bidirectional,
        },
        predicate,
        returns,
    })
}

#[derive(Debug, Clone)]
struct DeleteEdgeBinding {
    source: BoundNode,
    target: BoundNode,
    rel_info: RelTypeInfo,
    edge_mapping: EdgeMappingInfo,
}

fn bind_delete_edge_mapping(
    source_pat: &NodePat,
    rel_pat: &RelPat,
    target_pat: &NodePat,
    catalog: &impl CatalogSnapshot,
) -> Result<DeleteEdgeBinding, GqlError> {
    if source_pat.label.is_some() && target_pat.label.is_some() && rel_pat.rel_type.is_some() {
        let source = bind_node(source_pat, catalog)?;
        let target = bind_node(target_pat, catalog)?;
        let rel_type = rel_pat.rel_type.as_ref().expect("checked rel type");
        let rel_info = resolve_relationship(catalog, rel_pat, rel_type, &source, &target)?;
        let edge_mapping = rel_info.edge_mapping.clone().ok_or_else(|| {
            GqlError::unsupported(
                rel_pat.span,
                "DELETE requires a relationship backed by a registered edge row table",
            )
        })?;
        return Ok(DeleteEdgeBinding {
            source,
            target,
            rel_info,
            edge_mapping,
        });
    }
    if source_pat.label.is_none() && !source_pat.props.is_empty() {
        return Err(GqlError::unsupported(
            source_pat.span,
            "wildcard DELETE node property predicates require concrete node labels",
        ));
    }
    if target_pat.label.is_none() && !target_pat.props.is_empty() {
        return Err(GqlError::unsupported(
            target_pat.span,
            "wildcard DELETE node property predicates require concrete node labels",
        ));
    }
    if (source_pat.label.is_none() || target_pat.label.is_none())
        && catalog.has_ambiguous_node_labels()
    {
        return Err(GqlError::unsupported(
            rel_pat.span,
            "wildcard DELETE endpoint labels are ambiguous",
        ));
    }
    let source_labels = delete_node_label_candidates(source_pat, catalog)?;
    let target_labels = delete_node_label_candidates(target_pat, catalog)?;
    let rel_types = catalog.rel_types();
    let mut candidates = Vec::new();
    let mut seen_candidates = std::collections::HashSet::new();
    for rel_info in rel_types {
        let Some(edge_mapping) = rel_info.edge_mapping.clone() else {
            continue;
        };
        if edge_mapping.label_column.is_some() {
            return Err(GqlError::unsupported(
                rel_pat.span,
                "wildcard DELETE over dynamic relationship labels requires concrete endpoint labels and relationship type",
            ));
        }
        if rel_pat
            .rel_type
            .as_ref()
            .is_some_and(|rel_type| rel_type.text != rel_info.rel_type)
        {
            continue;
        }
        let (source_table_oid, target_table_oid) = match rel_pat.direction {
            Direction::Out => (rel_info.from_table_oid, rel_info.to_table_oid),
            Direction::In => (rel_info.to_table_oid, rel_info.from_table_oid),
            Direction::Undirected => unreachable!("undirected DELETE rejected before binding"),
        };
        let Some(source_label) = source_labels
            .iter()
            .find(|label| label.table_oid == source_table_oid)
        else {
            continue;
        };
        let Some(target_label) = target_labels
            .iter()
            .find(|label| label.table_oid == target_table_oid)
        else {
            continue;
        };
        if !seen_candidates.insert((
            rel_info.rel_type.clone(),
            rel_info.from_table_oid,
            rel_info.to_table_oid,
            edge_mapping.edge_table_oid,
            edge_mapping.source_column.clone(),
            edge_mapping.target_column.clone(),
        )) {
            continue;
        }
        candidates.push(DeleteEdgeBinding {
            source: bound_delete_node(source_pat, source_label, "__pggraph_delete_source"),
            target: bound_delete_node(target_pat, target_label, "__pggraph_delete_target"),
            rel_info,
            edge_mapping,
        });
    }
    if candidates.len() != 1 {
        let reason = if candidates.is_empty() {
            "DELETE requires a relationship backed by a registered edge row table".to_string()
        } else {
            format!(
                "wildcard DELETE relationship pattern is ambiguous across {} mapped edge row registrations",
                candidates.len()
            )
        };
        return Err(GqlError::unsupported(rel_pat.span, reason));
    }
    Ok(candidates.remove(0))
}

fn delete_node_label_candidates(
    node_pat: &NodePat,
    catalog: &impl CatalogSnapshot,
) -> Result<Vec<NodeLabelInfo>, GqlError> {
    if let Some(label) = &node_pat.label {
        return catalog
            .resolve_node_label(&label.text, label.span)
            .map(|label| vec![label]);
    }
    Ok(catalog.node_labels())
}

fn bound_delete_node(node_pat: &NodePat, label: &NodeLabelInfo, fallback_var: &str) -> BoundNode {
    BoundNode {
        var: node_pat
            .var
            .as_ref()
            .map_or_else(|| fallback_var.to_string(), |var| var.text.clone()),
        label: label.label.clone(),
        table_oid: label.table_oid,
        properties: label.properties.clone(),
    }
}

fn bind_detach_delete_node(
    query: &crate::gql::ast::DetachDeleteQuery,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalDetachDeleteNode, GqlError> {
    if query.match_.optional {
        return Err(GqlError::unsupported(
            query.match_.span,
            "OPTIONAL MATCH is only supported for read queries",
        ));
    }
    if query.return_.distinct {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN DISTINCT over DETACH DELETE is implemented in a later phase",
        ));
    }
    let Pattern { start, tail, .. } = &query.match_.pattern;
    if !tail.is_empty() {
        return Err(GqlError::unsupported(
            query.match_.pattern.span,
            "DETACH DELETE supports a single-node MATCH pattern in this release",
        ));
    }
    let node = bind_node(start, catalog)?;
    if query.delete.var.text != node.var {
        return Err(GqlError::bind(
            query.delete.var.span,
            format!("unknown DETACH DELETE variable `{}`", query.delete.var.text),
        ));
    }
    let mut incident_edges = Vec::new();
    for rel in catalog.incident_rel_types(node.table_oid) {
        let edge = rel.edge_mapping.ok_or_else(|| {
            GqlError::unsupported(
                query.delete.span,
                "DETACH DELETE requires incident relationships backed by registered edge row tables",
            )
        })?;
        if !incident_edges.iter().any(|existing: &BoundIncidentEdge| {
            existing.rel_type == rel.rel_type
                && existing.edge.edge_table_oid == edge.edge_table_oid
                && existing.edge.source_column == edge.source_column
                && existing.edge.target_column == edge.target_column
        }) {
            incident_edges.push(BoundIncidentEdge {
                rel_type: rel.rel_type,
                edge: BoundMappedEdge {
                    edge_table_oid: edge.edge_table_oid,
                    source_table_oid: edge.source_table_oid,
                    target_table_oid: edge.target_table_oid,
                    source_column: edge.source_column,
                    target_column: edge.target_column,
                    bidirectional: edge.bidirectional,
                },
            });
        }
    }
    let predicate = bind_node_predicates(query.where_.as_ref(), start, &node)?;
    let returns = bind_write_returns(&query.return_.items, &node, true)?;
    Ok(LogicalDetachDeleteNode {
        node,
        predicate,
        incident_edges,
        returns,
    })
}

fn validate_row_window(query: &crate::gql::ast::Query) -> Result<(), GqlError> {
    let Some(limit) = query.limit else {
        return Ok(());
    };
    let window = query.skip.unwrap_or(0).saturating_add(limit);
    if window > MAX_GQL_RESULT_ROWS as u64 {
        return Err(GqlError::unsupported(
            query.return_.span,
            format!("GQL row window cannot exceed {MAX_GQL_RESULT_ROWS}"),
        ));
    }
    Ok(())
}

fn resolve_relationship(
    catalog: &impl CatalogSnapshot,
    rel_pat: &RelPat,
    rel_type: &crate::gql::ast::Ident,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<super::catalog_snapshot::RelTypeInfo, GqlError> {
    match rel_pat.direction {
        Direction::Out => catalog.resolve_rel_type(
            &rel_type.text,
            source.table_oid,
            target.table_oid,
            rel_type.span,
        ),
        Direction::In => catalog.resolve_rel_type(
            &rel_type.text,
            target.table_oid,
            source.table_oid,
            rel_type.span,
        ),
        Direction::Undirected => catalog
            .resolve_rel_type(
                &rel_type.text,
                source.table_oid,
                target.table_oid,
                rel_type.span,
            )
            .or_else(|_| {
                catalog.resolve_rel_type(
                    &rel_type.text,
                    target.table_oid,
                    source.table_oid,
                    rel_type.span,
                )
            }),
    }
}

fn reject_later_clauses(_query: &crate::gql::ast::Query) -> Result<(), GqlError> {
    Ok(())
}

fn single_outbound_hop(match_: &MatchClause) -> Result<(&NodePat, &RelPat, &NodePat), GqlError> {
    let Pattern { start, tail, .. } = &match_.pattern;
    let [(rel, target)] = tail.as_slice() else {
        return Err(GqlError::unsupported(
            match_.pattern.span,
            "Phase 1B supports exactly one relationship in MATCH",
        ));
    };
    if !rel.props.is_empty() {
        return Err(GqlError::unsupported(
            rel.span,
            "relationship property maps are implemented in a later read phase",
        ));
    }
    Ok((start, rel, target))
}

fn bind_direction(direction: Direction) -> BoundDirection {
    match direction {
        Direction::Out => BoundDirection::Out,
        Direction::In => BoundDirection::In,
        Direction::Undirected => BoundDirection::Undirected,
    }
}

fn bind_hops(rel: &RelPat) -> Result<HopBounds, GqlError> {
    let hops = rel.var_len.map_or(
        HopBounds {
            variable: false,
            min: 1,
            max: 1,
        },
        |var_len| HopBounds {
            variable: true,
            min: var_len.min,
            max: var_len.max,
        },
    );
    if hops.min == 0 {
        return Err(GqlError::unsupported(
            rel.var_len.map_or(rel.span, |var_len| var_len.span),
            "zero-hop variable-length relationships are outside the supported GQL subset",
        ));
    }
    if hops.max > MAX_BOUND_HOPS {
        return Err(GqlError::unsupported(
            rel.var_len.map_or(rel.span, |var_len| var_len.span),
            format!("variable-length upper bound cannot exceed {MAX_BOUND_HOPS}"),
        ));
    }
    Ok(hops)
}

fn bind_node(node: &NodePat, catalog: &impl CatalogSnapshot) -> Result<BoundNode, GqlError> {
    let var = node.var.as_ref().ok_or_else(|| {
        GqlError::unsupported(node.span, "anonymous node patterns require a later phase")
    })?;
    let label = node.label.as_ref().ok_or_else(|| {
        GqlError::unsupported(node.span, "unlabeled node patterns require a later phase")
    })?;
    let info = catalog.resolve_node_label(&label.text, label.span)?;
    if let Some(property) = info.properties.iter().find(|property| {
        property
            .split('.')
            .any(|segment| segment.is_empty() || segment.starts_with('_'))
    }) {
        return Err(GqlError::bind(
            label.span,
            format!("registered property `{property}` uses a reserved GQL key"),
        ));
    }
    Ok(BoundNode {
        var: var.text.clone(),
        label: info.label,
        table_oid: info.table_oid,
        properties: info.properties,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScopedBinding {
    Node(BindingSide),
    Relationship { var_len: bool },
    Path,
    Property { side: BindingSide, property: String },
    PathFunction(PathFunc),
}

type BindingScope = std::collections::HashMap<String, ScopedBinding>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundWith {
    scope: BindingScope,
    distinct_stages: Vec<Vec<ReturnBinding>>,
}

fn initial_relationship_scope(
    rel_pat: &RelPat,
    target_pat: &NodePat,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<BindingScope, GqlError> {
    let mut scope = BindingScope::with_capacity(3);
    scope.insert(source.var.clone(), ScopedBinding::Node(BindingSide::Source));
    if source.var == target.var {
        let span = target_pat
            .var
            .as_ref()
            .map_or(target_pat.span, |var| var.span);
        return Err(GqlError::bind(
            span,
            format!("duplicate variable `{}` in MATCH scope", target.var),
        ));
    }
    scope.insert(target.var.clone(), ScopedBinding::Node(BindingSide::Target));
    if let Some(rel_var) = &rel_pat.var {
        if rel_var.text == source.var || rel_var.text == target.var {
            return Err(GqlError::bind(
                rel_var.span,
                format!("duplicate variable `{}` in MATCH scope", rel_var.text),
            ));
        }
        scope.insert(
            rel_var.text.clone(),
            ScopedBinding::Relationship {
                var_len: rel_pat.var_len.is_some(),
            },
        );
    }
    Ok(scope)
}

fn initial_node_scope(node: &BoundNode) -> BindingScope {
    BindingScope::from([(node.var.clone(), ScopedBinding::Node(BindingSide::Source))])
}

fn bind_with_clauses(
    clauses: &[WithClause],
    mut scope: BindingScope,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<BoundWith, GqlError> {
    let mut distinct_stages = Vec::new();
    for clause in clauses {
        if clause.distinct {
            distinct_stages.push(bind_distinct_stage(&clause.items, &scope, source, target)?);
        }
        scope = bind_projection_scope(&clause.items, &scope, source, target)?;
    }
    Ok(BoundWith {
        scope,
        distinct_stages,
    })
}

fn bind_distinct_stage(
    items: &[ReturnItem],
    scope: &BindingScope,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<Vec<ReturnBinding>, GqlError> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut stage = Vec::with_capacity(items.len());
    for item in items {
        match item.expr {
            ReturnExpr::Aggregate { span, .. } => {
                return Err(GqlError::unsupported(
                    span,
                    "aggregate WITH projections require row-stream aggregation from a later read phase",
                ));
            }
            ReturnExpr::Func { span, .. } => {
                return Err(GqlError::unsupported(
                    span,
                    "path-function WITH projections require row-stream value projection from a later read phase",
                ));
            }
            _ => {}
        }
        let (name, scoped) = bind_scoped_item(item, scope, source, target)?;
        if !seen.insert(name.clone()) {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{name}`"),
            ));
        }
        let binding = match scoped {
            ScopedBinding::Node(side) => ReturnBinding::Node { side, name },
            ScopedBinding::Relationship { var_len: false } => ReturnBinding::Relationship { name },
            ScopedBinding::Relationship { var_len: true } => ReturnBinding::Path { name },
            ScopedBinding::Path => ReturnBinding::Path { name },
            ScopedBinding::PathFunction(func) => ReturnBinding::PathFunction {
                func,
                path_var: None,
                name,
            },
            ScopedBinding::Property { side, property } => ReturnBinding::Property {
                side,
                property,
                name,
            },
        };
        stage.push(binding);
    }
    Ok(stage)
}

fn bind_projection_scope(
    items: &[ReturnItem],
    scope: &BindingScope,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<BindingScope, GqlError> {
    let mut next = BindingScope::with_capacity(items.len());
    for item in items {
        match item.expr {
            ReturnExpr::Aggregate { span, .. } => {
                return Err(GqlError::unsupported(
                    span,
                    "aggregate WITH projections require row-stream aggregation from a later read phase",
                ));
            }
            ReturnExpr::Func { span, .. } => {
                return Err(GqlError::unsupported(
                    span,
                    "path-function WITH projections require row-stream value projection from a later read phase",
                ));
            }
            _ => {}
        }
        let (name, binding) = bind_scoped_item(item, scope, source, target)?;
        if next.insert(name.clone(), binding).is_some() {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{name}`"),
            ));
        }
    }
    Ok(next)
}

fn bind_scoped_returns(
    items: &[ReturnItem],
    scope: &BindingScope,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<Vec<ReturnBinding>, GqlError> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut bindings = Vec::with_capacity(items.len());
    for item in items {
        let name = projection_name(item);
        if !seen.insert(name.clone()) {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{name}`"),
            ));
        }
        let binding = if let ReturnExpr::Aggregate {
            func,
            distinct,
            arg,
            span: _,
            ..
        } = &item.expr
        {
            ReturnBinding::Aggregate {
                func: bind_aggregate_func(*func),
                arg: bind_aggregate_arg(*func, arg, scope, source, target)?,
                distinct: *distinct,
                name,
            }
        } else {
            let (_, scoped) = bind_scoped_item(item, scope, source, target)?;
            match scoped {
                ScopedBinding::Node(side) => ReturnBinding::Node { side, name },
                ScopedBinding::Relationship { var_len: false } => {
                    ReturnBinding::Relationship { name }
                }
                ScopedBinding::Relationship { var_len: true } => ReturnBinding::Path { name },
                ScopedBinding::Path => ReturnBinding::Path { name },
                ScopedBinding::PathFunction(func) => ReturnBinding::PathFunction {
                    func,
                    path_var: None,
                    name,
                },
                ScopedBinding::Property { side, property } => ReturnBinding::Property {
                    side,
                    property,
                    name,
                },
            }
        };
        bindings.push(binding);
    }
    Ok(bindings)
}

fn bind_scoped_item(
    item: &ReturnItem,
    scope: &BindingScope,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<(String, ScopedBinding), GqlError> {
    let binding = match &item.expr {
        ReturnExpr::Var { var, span } => scope.get(&var.text).cloned().ok_or_else(|| {
            GqlError::bind(*span, format!("unknown return variable `{}`", var.text))
        })?,
        ReturnExpr::Property { var, property, .. } => match scope.get(&var.text) {
            Some(ScopedBinding::Node(side)) => {
                validate_property(*side, &property.text, source, target, property.span)?;
                ScopedBinding::Property {
                    side: *side,
                    property: property.text.clone(),
                }
            }
            Some(_) => {
                return Err(GqlError::bind(
                    var.span,
                    format!("variable `{}` does not bind a node", var.text),
                ));
            }
            None => {
                return Err(GqlError::bind(
                    var.span,
                    format!("unknown return variable `{}`", var.text),
                ));
            }
        },
        ReturnExpr::Func { name, args, span } => {
            return bind_path_function(name, args, *span, scope)
                .map(|binding| (projection_name(item), binding));
        }
        ReturnExpr::Aggregate { span, .. } => {
            return Err(GqlError::unsupported(
                *span,
                "aggregate expressions are only supported in RETURN",
            ));
        }
    };
    Ok((projection_name(item), binding))
}

fn bind_path_function(
    name: &crate::gql::ast::Ident,
    args: &[crate::gql::ast::Ident],
    span: Span,
    scope: &BindingScope,
) -> Result<ScopedBinding, GqlError> {
    let Some(func) = path_func(&name.text) else {
        return Err(GqlError::unsupported(
            span,
            "RETURN functions are implemented in a later read phase",
        ));
    };
    let [arg] = args else {
        return Err(GqlError::bind(
            span,
            format!(
                "path function `{}` requires exactly one path argument",
                name.text
            ),
        ));
    };
    match scope.get(&arg.text) {
        Some(ScopedBinding::Relationship { .. }) => Ok(ScopedBinding::PathFunction(func)),
        Some(ScopedBinding::Path) => Ok(ScopedBinding::PathFunction(func)),
        Some(_) => Err(GqlError::bind(
            arg.span,
            format!(
                "path function `{}` requires a relationship path variable",
                name.text
            ),
        )),
        None => Err(GqlError::bind(
            arg.span,
            format!("unknown path variable `{}`", arg.text),
        )),
    }
}

fn bind_aggregate_func(func: ast::AggregateFunc) -> AggregateFunc {
    match func {
        ast::AggregateFunc::Count => AggregateFunc::Count,
        ast::AggregateFunc::Sum => AggregateFunc::Sum,
        ast::AggregateFunc::Avg => AggregateFunc::Avg,
        ast::AggregateFunc::Min => AggregateFunc::Min,
        ast::AggregateFunc::Max => AggregateFunc::Max,
        ast::AggregateFunc::Collect => AggregateFunc::Collect,
    }
}

fn path_func(name: &str) -> Option<PathFunc> {
    if name.eq_ignore_ascii_case("nodes") {
        Some(PathFunc::Nodes)
    } else if name.eq_ignore_ascii_case("relationships") {
        Some(PathFunc::Relationships)
    } else if name.eq_ignore_ascii_case("length") {
        Some(PathFunc::Length)
    } else {
        None
    }
}

fn bind_aggregate_arg(
    func: ast::AggregateFunc,
    arg: &ast::AggregateArg,
    scope: &BindingScope,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<AggregateArg, GqlError> {
    match arg {
        ast::AggregateArg::All { span } => {
            if func == ast::AggregateFunc::Count {
                Ok(AggregateArg::All)
            } else {
                Err(GqlError::bind(
                    *span,
                    "only count(*) may use '*' as an aggregate argument",
                ))
            }
        }
        ast::AggregateArg::Var { var, span } => match scope.get(&var.text) {
            Some(ScopedBinding::Node(side)) if aggregate_accepts_value(func) => {
                Ok(AggregateArg::Node(*side))
            }
            Some(ScopedBinding::Relationship { var_len: false })
                if aggregate_accepts_value(func) =>
            {
                Ok(AggregateArg::Relationship)
            }
            Some(ScopedBinding::Relationship { var_len: true }) => Err(GqlError::unsupported(
                *span,
                "aggregates over variable-length relationships require path support",
            )),
            Some(_) => Err(GqlError::bind(
                *span,
                format!(
                    "aggregate `{}` requires a property argument",
                    aggregate_name(func)
                ),
            )),
            None => Err(GqlError::bind(
                *span,
                format!("unknown aggregate variable `{}`", var.text),
            )),
        },
        ast::AggregateArg::Property {
            var,
            property,
            span: _,
        } => match scope.get(&var.text) {
            Some(ScopedBinding::Node(side)) => {
                validate_property(*side, &property.text, source, target, property.span)?;
                Ok(AggregateArg::Property {
                    side: *side,
                    property: property.text.clone(),
                })
            }
            Some(_) => Err(GqlError::bind(
                var.span,
                format!("variable `{}` does not bind a node", var.text),
            )),
            None => Err(GqlError::bind(
                var.span,
                format!("unknown aggregate variable `{}`", var.text),
            )),
        },
    }
}

fn aggregate_accepts_value(func: ast::AggregateFunc) -> bool {
    matches!(
        func,
        ast::AggregateFunc::Count | ast::AggregateFunc::Collect
    )
}

fn aggregate_name(func: ast::AggregateFunc) -> &'static str {
    match func {
        ast::AggregateFunc::Count => "count",
        ast::AggregateFunc::Sum => "sum",
        ast::AggregateFunc::Avg => "avg",
        ast::AggregateFunc::Min => "min",
        ast::AggregateFunc::Max => "max",
        ast::AggregateFunc::Collect => "collect",
    }
}

fn projection_name(item: &ReturnItem) -> String {
    if let Some(alias) = &item.alias {
        return alias.text.clone();
    }
    match &item.expr {
        ReturnExpr::Var { var, .. } => var.text.clone(),
        ReturnExpr::Property { var, property, .. } => format!("{}.{}", var.text, property.text),
        ReturnExpr::Func { name, .. } => name.text.clone(),
        ReturnExpr::Aggregate { name, .. } => name.text.clone(),
    }
}

fn bind_scoped_sort_items(
    items: &[SortItem],
    returns: &[ReturnBinding],
    scope: &BindingScope,
    source: &BoundNode,
    target: &BoundNode,
    return_distinct: bool,
) -> Result<Vec<SortBinding>, GqlError> {
    let has_aggregate = returns.iter().any(ReturnBinding::is_aggregate);
    items
        .iter()
        .map(|item| {
            let key = match &item.key {
                SortKey::Alias { alias, .. } => {
                    if returns
                        .iter()
                        .any(|binding| binding.name() == alias.text && binding.is_sortable_scalar())
                    {
                        SortBindingKey::ReturnName(alias.text.clone())
                    } else if returns.iter().any(|binding| binding.name() == alias.text) {
                        return Err(GqlError::unsupported(
                            alias.span,
                            "ORDER BY aliases must refer to scalar property returns",
                        ));
                    } else if let Some(ScopedBinding::Property { side, property }) =
                        scope.get(&alias.text)
                    {
                        if has_aggregate {
                            return Err(GqlError::unsupported(
                                alias.span,
                                "aggregate queries must ORDER BY returned property or aggregate aliases",
                            ));
                        }
                        if return_distinct {
                            return Err(GqlError::unsupported(
                                alias.span,
                                "DISTINCT queries must ORDER BY returned scalar expressions",
                            ));
                        }
                        SortBindingKey::Property {
                            side: *side,
                            property: property.clone(),
                        }
                    } else if scope.contains_key(&alias.text) {
                        return Err(GqlError::unsupported(
                            alias.span,
                            "ORDER BY aliases must refer to scalar property returns",
                        ));
                    } else {
                        return Err(GqlError::bind(
                            alias.span,
                            format!("unknown ORDER BY alias `{}`", alias.text),
                        ));
                    }
                }
                SortKey::Property {
                    var,
                    property,
                    span: _,
                } => {
                    if has_aggregate {
                        if let Some(binding) =
                            returned_property_binding(returns, scope, &var.text, &property.text)
                        {
                            SortBindingKey::ReturnName(binding.name().to_string())
                        } else {
                            return Err(GqlError::unsupported(
                                item.span,
                                "aggregate queries must ORDER BY returned property or aggregate aliases",
                            ));
                        }
                    } else {
                        if return_distinct {
                            if let Some(binding) =
                                returned_property_binding(returns, scope, &var.text, &property.text)
                            {
                                return Ok(SortBinding {
                                    key: SortBindingKey::ReturnName(binding.name().to_string()),
                                    desc: item.desc,
                                });
                            }
                            return Err(GqlError::unsupported(
                                item.span,
                                "DISTINCT queries must ORDER BY returned scalar expressions",
                            ));
                        }
                        match scope.get(&var.text) {
                            Some(ScopedBinding::Node(side)) => {
                                validate_property(
                                    *side,
                                    &property.text,
                                    source,
                                    target,
                                    property.span,
                                )?;
                                SortBindingKey::Property {
                                    side: *side,
                                    property: property.text.clone(),
                                }
                            }
                            Some(_) => {
                                return Err(GqlError::bind(
                                    var.span,
                                    format!("variable `{}` does not bind a node", var.text),
                                ));
                            }
                            None => {
                                return Err(GqlError::bind(
                                    var.span,
                                    format!("unknown ORDER BY variable `{}`", var.text),
                                ));
                            }
                        }
                    }
                }
            };
            Ok(SortBinding {
                key,
                desc: item.desc,
            })
        })
        .collect()
}

fn returned_property_binding<'a>(
    returns: &'a [ReturnBinding],
    scope: &BindingScope,
    var: &str,
    property: &str,
) -> Option<&'a ReturnBinding> {
    let Some(ScopedBinding::Node(side)) = scope.get(var) else {
        return None;
    };
    returns.iter().find(|binding| {
        matches!(
            binding,
            ReturnBinding::Property {
                side: binding_side,
                property: binding_property,
                ..
            } if binding_side == side && binding_property == property
        )
    })
}

fn bind_predicates(
    where_: Option<&Expr>,
    source_pat: &NodePat,
    rel_pat: &RelPat,
    target_pat: &NodePat,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<Option<Predicate>, GqlError> {
    let mut predicates = Vec::new();
    if let Some(expr) = where_ {
        predicates.push(bind_expr(expr, source, target, 0)?);
    }
    for (property, value) in &source_pat.props {
        check_predicate_count(&predicates, property.span)?;
        validate_property(
            BindingSide::Source,
            &property.text,
            source,
            target,
            property.span,
        )?;
        predicates.push(Predicate::Compare {
            lhs: ValueExpr::Property {
                side: BindingSide::Source,
                property: property.text.clone(),
            },
            op: BoundCmpOp::Eq,
            rhs: Some(bind_operand(value, source, target)?),
        });
    }
    for (property, value) in &target_pat.props {
        check_predicate_count(&predicates, property.span)?;
        validate_property(
            BindingSide::Target,
            &property.text,
            source,
            target,
            property.span,
        )?;
        predicates.push(Predicate::Compare {
            lhs: ValueExpr::Property {
                side: BindingSide::Target,
                property: property.text.clone(),
            },
            op: BoundCmpOp::Eq,
            rhs: Some(bind_operand(value, source, target)?),
        });
    }
    if !rel_pat.props.is_empty() {
        return Err(GqlError::unsupported(
            rel_pat.span,
            "relationship property maps are implemented in a later read phase",
        ));
    }
    Ok(predicates
        .into_iter()
        .reduce(|lhs, rhs| Predicate::And(Box::new(lhs), Box::new(rhs))))
}

fn bind_node_predicates(
    where_: Option<&Expr>,
    node_pat: &NodePat,
    node: &BoundNode,
) -> Result<Option<Predicate>, GqlError> {
    let mut predicates = Vec::new();
    if let Some(expr) = where_ {
        predicates.push(bind_expr(expr, node, node, 0)?);
    }
    for (property, value) in &node_pat.props {
        check_predicate_count(&predicates, property.span)?;
        validate_property(
            BindingSide::Source,
            &property.text,
            node,
            node,
            property.span,
        )?;
        predicates.push(Predicate::Compare {
            lhs: ValueExpr::Property {
                side: BindingSide::Source,
                property: property.text.clone(),
            },
            op: BoundCmpOp::Eq,
            rhs: Some(bind_operand(value, node, node)?),
        });
    }
    Ok(predicates
        .into_iter()
        .reduce(|lhs, rhs| Predicate::And(Box::new(lhs), Box::new(rhs))))
}

fn check_predicate_count(predicates: &[Predicate], span: Span) -> Result<(), GqlError> {
    if predicates.len() >= MAX_BOUND_PREDICATE_COUNT {
        return Err(GqlError::syntax(span, "too many predicates in GQL query"));
    }
    Ok(())
}

fn bind_expr(
    expr: &Expr,
    source: &BoundNode,
    target: &BoundNode,
    depth: usize,
) -> Result<Predicate, GqlError> {
    if depth > MAX_BOUND_PREDICATE_DEPTH {
        return Err(GqlError::syntax(
            expr_span(expr),
            "predicate expression is too deeply nested",
        ));
    }
    match expr {
        Expr::And { lhs, rhs, .. } => Ok(Predicate::And(
            Box::new(bind_expr(lhs, source, target, depth + 1)?),
            Box::new(bind_expr(rhs, source, target, depth + 1)?),
        )),
        Expr::Or { lhs, rhs, .. } => Ok(Predicate::Or(
            Box::new(bind_expr(lhs, source, target, depth + 1)?),
            Box::new(bind_expr(rhs, source, target, depth + 1)?),
        )),
        Expr::Not { expr, .. } => Ok(Predicate::Not(Box::new(bind_expr(
            expr,
            source,
            target,
            depth + 1,
        )?))),
        Expr::Compare { lhs, op, rhs, .. } => Ok(Predicate::Compare {
            lhs: bind_operand(lhs, source, target)?,
            op: bind_cmp_op(*op),
            rhs: rhs
                .as_ref()
                .map(|operand| bind_operand(operand, source, target))
                .transpose()?,
        }),
    }
}

fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::And { span, .. }
        | Expr::Or { span, .. }
        | Expr::Not { span, .. }
        | Expr::Compare { span, .. } => *span,
    }
}

fn bind_operand(
    operand: &Operand,
    source: &BoundNode,
    target: &BoundNode,
) -> Result<ValueExpr, GqlError> {
    match operand {
        Operand::Property {
            var,
            property,
            span: _,
        } => {
            let side = binding_side(&var.text, source, target, var.span)?;
            validate_property(side, &property.text, source, target, property.span)?;
            Ok(ValueExpr::Property {
                side,
                property: property.text.clone(),
            })
        }
        Operand::Literal(literal) => Ok(ValueExpr::Literal(literal_json(literal))),
        Operand::Param { name, .. } => Ok(ValueExpr::Param(name.text.clone())),
        Operand::List { values, .. } => {
            Ok(ValueExpr::List(values.iter().map(literal_json).collect()))
        }
    }
}

fn binding_side(
    var: &str,
    source: &BoundNode,
    target: &BoundNode,
    span: Span,
) -> Result<BindingSide, GqlError> {
    if var == source.var {
        Ok(BindingSide::Source)
    } else if var == target.var {
        Ok(BindingSide::Target)
    } else {
        Err(GqlError::bind(span, format!("unknown variable `{var}`")))
    }
}

fn validate_property(
    side: BindingSide,
    property: &str,
    source: &BoundNode,
    target: &BoundNode,
    span: Span,
) -> Result<(), GqlError> {
    if property
        .split('.')
        .any(|segment| segment.is_empty() || segment.starts_with('_'))
    {
        return Err(GqlError::bind(
            span,
            format!("reserved GQL property key `{property}`"),
        ));
    }
    let properties = match side {
        BindingSide::Source => &source.properties,
        BindingSide::Target => &target.properties,
        BindingSide::PathNode(_) => {
            return Err(GqlError::unsupported(
                span,
                "property references over multi-segment path variables require a later phase",
            ));
        }
    };
    if properties.contains(property) {
        Ok(())
    } else {
        Err(GqlError::bind(
            span,
            format!("unknown property `{property}`"),
        ))
    }
}

fn bind_cmp_op(op: CmpOp) -> BoundCmpOp {
    match op {
        CmpOp::Eq => BoundCmpOp::Eq,
        CmpOp::Neq => BoundCmpOp::Neq,
        CmpOp::Lt => BoundCmpOp::Lt,
        CmpOp::Lte => BoundCmpOp::Lte,
        CmpOp::Gt => BoundCmpOp::Gt,
        CmpOp::Gte => BoundCmpOp::Gte,
        CmpOp::In => BoundCmpOp::In,
        CmpOp::IsNull => BoundCmpOp::IsNull,
        CmpOp::IsNotNull => BoundCmpOp::IsNotNull,
    }
}

fn literal_json(literal: &Literal) -> serde_json::Value {
    let Literal::Value { value, .. } = literal;
    match value {
        LiteralValue::Str(value) => serde_json::Value::String(value.clone()),
        LiteralValue::Int(value) => serde_json::Value::from(*value),
        LiteralValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        LiteralValue::Bool(value) => serde_json::Value::Bool(*value),
        LiteralValue::Null => serde_json::Value::Null,
    }
}
