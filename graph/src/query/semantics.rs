//! Semantic binding for the supported GQL subset.

use crate::gql::ast::{
    CmpOp, Direction, Expr, Literal, LiteralValue, MatchClause, NodePat, Operand, Pattern, RelPat,
    ReturnExpr, ReturnItem, SortItem, SortKey,
};
use crate::gql::errors::{GqlError, Span};

use super::catalog_snapshot::CatalogSnapshot;
use super::logical_plan::{
    BindingSide, BoundCmpOp, BoundDirection, BoundMappedEdge, BoundNode, BoundRel, CreateProperty,
    CreateReturnBinding, CreateValue, HopBounds, LogicalCreateNode, LogicalDeleteEdge,
    LogicalNodeScan, LogicalPlan, LogicalSetProperty, LogicalStatement, Predicate, ReturnBinding,
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
    let returns = bind_returns(&query.return_.items, &source, rel_pat, &target)?;
    let order_by = bind_sort_items(&query.order_by, &returns, &source, &target)?;
    Ok(LogicalPlan {
        source,
        relationship: BoundRel {
            var: rel_pat.var.as_ref().map(|var| var.text.clone()),
            rel_type: rel_info.rel_type,
            direction: bind_direction(rel_pat.direction),
            hops: bind_hops(rel_pat)?,
        },
        target,
        returns,
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
    let node = bind_node(start, catalog)?;
    let predicate = bind_node_predicates(query.where_.as_ref(), start, &node)?;
    let returns = bind_node_returns(&query.return_.items, &node)?;
    let order_by = bind_sort_items(&query.order_by, &returns, &node, &node)?;
    Ok(LogicalNodeScan {
        node,
        returns,
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
        crate::gql::ast::Statement::Delete(query) => {
            bind_delete_edge(query, catalog).map(LogicalStatement::DeleteEdge)
        }
    }
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
    let returns = bind_create_returns(&query.return_.items, &node)?;
    Ok(LogicalCreateNode {
        node,
        properties,
        returns,
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

fn bind_create_returns(
    items: &[ReturnItem],
    node: &BoundNode,
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
    let Pattern { start, tail, .. } = &query.match_.pattern;
    if !tail.is_empty() {
        return Err(GqlError::unsupported(
            query.match_.pattern.span,
            "SET supports a single-node MATCH pattern in this release",
        ));
    }
    let node = bind_node(start, catalog)?;
    let writable_properties = start
        .label
        .as_ref()
        .map(|label| catalog.resolve_node_label(&label.text, label.span))
        .transpose()?
        .map(|info| info.writable_properties)
        .unwrap_or_default();
    if query.set.target.var.text != node.var {
        return Err(GqlError::bind(
            query.set.target.var.span,
            format!("unknown SET variable `{}`", query.set.target.var.text),
        ));
    }
    let property = query.set.target.property.text.clone();
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
    let returns = bind_create_returns(&query.return_.items, &node)?;
    Ok(LogicalSetProperty {
        node,
        predicate,
        property,
        value,
        returns,
    })
}

fn bind_delete_edge(
    query: &crate::gql::ast::DeleteQuery,
    catalog: &impl CatalogSnapshot,
) -> Result<LogicalDeleteEdge, GqlError> {
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
    let source = bind_node(source_pat, catalog)?;
    let target = bind_node(target_pat, catalog)?;
    let rel_type = rel_pat.rel_type.as_ref().ok_or_else(|| {
        GqlError::unsupported(
            rel_pat.span,
            "anonymous relationship types require a later phase",
        )
    })?;
    let rel_info = resolve_relationship(catalog, rel_pat, rel_type, &source, &target)?;
    let edge_mapping = rel_info.edge_mapping.ok_or_else(|| {
        GqlError::unsupported(
            rel_pat.span,
            "DELETE requires a relationship backed by a registered edge row table",
        )
    })?;
    let predicate = bind_predicates(
        query.where_.as_ref(),
        source_pat,
        rel_pat,
        target_pat,
        &source,
        &target,
    )?;
    let returns = bind_returns(&query.return_.items, &source, rel_pat, &target)?;
    Ok(LogicalDeleteEdge {
        source,
        relationship: BoundRel {
            var: Some(rel_var.text.clone()),
            rel_type: rel_info.rel_type,
            direction: bind_direction(rel_pat.direction),
            hops: HopBounds { min: 1, max: 1 },
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

fn reject_later_clauses(query: &crate::gql::ast::Query) -> Result<(), GqlError> {
    if query.return_.distinct {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN DISTINCT is implemented in a later phase",
        ));
    }
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
    let hops = rel
        .var_len
        .map_or(HopBounds { min: 1, max: 1 }, |var_len| HopBounds {
            min: var_len.min,
            max: var_len.max,
        });
    if hops.min == 0 {
        return Err(GqlError::unsupported(
            rel.var_len.map_or(rel.span, |var_len| var_len.span),
            "zero-hop variable-length relationships are not implemented yet",
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
    if let Some(property) = info
        .properties
        .iter()
        .find(|property| property.starts_with('_'))
    {
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

fn bind_returns(
    items: &[ReturnItem],
    source: &BoundNode,
    rel_pat: &RelPat,
    target: &BoundNode,
) -> Result<Vec<ReturnBinding>, GqlError> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut bindings = Vec::with_capacity(items.len());
    for item in items {
        let binding = match &item.expr {
            ReturnExpr::Var { var, .. } if var.text == source.var => Ok(ReturnBinding::Node {
                side: BindingSide::Source,
                name: item
                    .alias
                    .as_ref()
                    .map_or_else(|| var.text.clone(), |alias| alias.text.clone()),
            }),
            ReturnExpr::Var { var, .. } if var.text == target.var => Ok(ReturnBinding::Node {
                side: BindingSide::Target,
                name: item
                    .alias
                    .as_ref()
                    .map_or_else(|| var.text.clone(), |alias| alias.text.clone()),
            }),
            ReturnExpr::Var { var, .. }
                if rel_pat
                    .var
                    .as_ref()
                    .is_some_and(|rel_var| rel_var.text == var.text) =>
            {
                if rel_pat.var_len.is_some() {
                    return Err(GqlError::unsupported(
                        var.span,
                        "RETURN relationship variables over variable-length patterns require path support",
                    ));
                }
                Ok(ReturnBinding::Relationship {
                    name: item
                        .alias
                        .as_ref()
                        .map_or_else(|| var.text.clone(), |alias| alias.text.clone()),
                })
            }
            ReturnExpr::Var { var, span } => Err(GqlError::bind(
                *span,
                format!("unknown return variable `{}`", var.text),
            )),
            ReturnExpr::Property {
                var,
                property,
                span: _,
            } => {
                let side = binding_side(&var.text, source, target, var.span)?;
                validate_property(side, &property.text, source, target, property.span)?;
                Ok(ReturnBinding::Property {
                    side,
                    property: property.text.clone(),
                    name: item.alias.as_ref().map_or_else(
                        || format!("{}.{}", var.text, property.text),
                        |alias| alias.text.clone(),
                    ),
                })
            }
            ReturnExpr::Func { span, .. } => Err(GqlError::unsupported(
                *span,
                "RETURN functions are implemented in a later read phase",
            )),
        }?;
        let name = binding.name();
        if !seen.insert(name.to_string()) {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{name}`"),
            ));
        }
        bindings.push(binding);
    }
    Ok(bindings)
}

fn bind_node_returns(
    items: &[ReturnItem],
    node: &BoundNode,
) -> Result<Vec<ReturnBinding>, GqlError> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut bindings = Vec::with_capacity(items.len());
    for item in items {
        let binding = match &item.expr {
            ReturnExpr::Var { var, .. } if var.text == node.var => Ok(ReturnBinding::Node {
                side: BindingSide::Source,
                name: item
                    .alias
                    .as_ref()
                    .map_or_else(|| var.text.clone(), |alias| alias.text.clone()),
            }),
            ReturnExpr::Property {
                var,
                property,
                span: _,
            } if var.text == node.var => {
                validate_property(
                    BindingSide::Source,
                    &property.text,
                    node,
                    node,
                    property.span,
                )?;
                Ok(ReturnBinding::Property {
                    side: BindingSide::Source,
                    property: property.text.clone(),
                    name: item.alias.as_ref().map_or_else(
                        || format!("{}.{}", var.text, property.text),
                        |alias| alias.text.clone(),
                    ),
                })
            }
            ReturnExpr::Var { var, span } => Err(GqlError::bind(
                *span,
                format!("unknown return variable `{}`", var.text),
            )),
            ReturnExpr::Property { var, span, .. } => Err(GqlError::bind(
                *span,
                format!("unknown return variable `{}`", var.text),
            )),
            ReturnExpr::Func { span, .. } => Err(GqlError::unsupported(
                *span,
                "RETURN functions are implemented in a later read phase",
            )),
        }?;
        let name = binding.name();
        if !seen.insert(name.to_string()) {
            return Err(GqlError::bind(
                item.span,
                format!("duplicate return name `{name}`"),
            ));
        }
        bindings.push(binding);
    }
    Ok(bindings)
}

fn bind_sort_items(
    items: &[SortItem],
    returns: &[ReturnBinding],
    source: &BoundNode,
    target: &BoundNode,
) -> Result<Vec<SortBinding>, GqlError> {
    items
        .iter()
        .map(|item| {
            let key = match &item.key {
                SortKey::Alias { alias, .. } => {
                    if returns
                        .iter()
                        .any(|binding| binding.name() == alias.text && binding.is_property())
                    {
                        SortBindingKey::ReturnName(alias.text.clone())
                    } else if returns.iter().any(|binding| binding.name() == alias.text) {
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
                    let side = binding_side(&var.text, source, target, var.span)?;
                    validate_property(side, &property.text, source, target, property.span)?;
                    SortBindingKey::Property {
                        side,
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
    if property.starts_with('_') {
        return Err(GqlError::bind(
            span,
            format!("reserved GQL property key `{property}`"),
        ));
    }
    let properties = match side {
        BindingSide::Source => &source.properties,
        BindingSide::Target => &target.properties,
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
