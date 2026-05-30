//! Semantic binding for the read-only GQL subset.

use crate::gql::ast::{
    CmpOp, Direction, Expr, Literal, LiteralValue, MatchClause, NodePat, Operand, Pattern, RelPat,
    ReturnExpr, ReturnItem,
};
use crate::gql::errors::{GqlError, Span};

use super::catalog_snapshot::CatalogSnapshot;
use super::logical_plan::{
    BindingSide, BoundCmpOp, BoundNode, BoundRel, LogicalPlan, Predicate, ReturnBinding, ValueExpr,
};

const MAX_BOUND_PREDICATE_DEPTH: usize = 512;
const MAX_BOUND_PREDICATE_COUNT: usize = 512;

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
    let (source_pat, rel_pat, target_pat) = single_outbound_hop(&query.match_)?;
    let source = bind_node(source_pat, catalog)?;
    let target = bind_node(target_pat, catalog)?;
    let rel_type = rel_pat.rel_type.as_ref().ok_or_else(|| {
        GqlError::unsupported(
            rel_pat.span,
            "anonymous relationship types require a later phase",
        )
    })?;
    let rel = catalog.resolve_rel_type(
        &rel_type.text,
        source.table_oid,
        target.table_oid,
        rel_type.span,
    )?;
    let predicate = bind_predicates(
        query.where_.as_ref(),
        source_pat,
        rel_pat,
        target_pat,
        &source,
        &target,
    )?;
    let returns = bind_returns(&query.return_.items, &source, &target)?;
    Ok(LogicalPlan {
        source,
        relationship: BoundRel {
            rel_type: rel.rel_type,
        },
        target,
        returns,
        predicate,
    })
}

fn reject_later_clauses(query: &crate::gql::ast::Query) -> Result<(), GqlError> {
    if query.return_.distinct {
        return Err(GqlError::unsupported(
            query.return_.span,
            "RETURN DISTINCT is implemented in a later phase",
        ));
    }
    if !query.order_by.is_empty() {
        return Err(GqlError::unsupported(
            query.order_by[0].span,
            "ORDER BY is implemented in a later read phase",
        ));
    }
    if query.skip.is_some() {
        return Err(GqlError::unsupported(
            query.return_.span,
            "SKIP is implemented in a later read phase",
        ));
    }
    if query.limit.is_some() {
        return Err(GqlError::unsupported(
            query.return_.span,
            "LIMIT is implemented in a later read phase",
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
    if rel.direction != Direction::Out {
        return Err(GqlError::unsupported(
            rel.span,
            "Phase 1B supports only outbound directed relationships",
        ));
    }
    if rel.var_len.is_some() {
        return Err(GqlError::unsupported(
            rel.var_len.map_or(rel.span, |var_len| var_len.span),
            "variable-length relationships are implemented in a later read phase",
        ));
    }
    if !rel.props.is_empty() {
        return Err(GqlError::unsupported(
            rel.span,
            "relationship property maps are implemented in a later read phase",
        ));
    }
    Ok((start, rel, target))
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
