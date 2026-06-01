//! Semantic binding for the supported GQL subset.

use crate::gql::ast::{
    self, CmpOp, Direction, Expr, Literal, LiteralValue, MatchClause, NodePat, Operand, Pattern,
    RelPat, ReturnExpr, ReturnItem, SortItem, SortKey, WithClause,
};
use crate::gql::errors::{GqlError, Span};

use super::catalog_snapshot::CatalogSnapshot;
use super::logical_plan::{
    AggregateArg, AggregateFunc, BindingSide, BoundCmpOp, BoundDirection, BoundIncidentEdge,
    BoundMappedEdge, BoundNode, BoundRel, CreateProperty, CreateReturnBinding, CreateValue,
    HopBounds, LogicalCreateNode, LogicalDeleteEdge, LogicalDetachDeleteNode, LogicalMergeNode,
    LogicalNodeScan, LogicalPlan, LogicalRemoveProperty, LogicalSetProperty, LogicalStatement,
    PathFunc, Predicate, ReturnBinding, SortBinding, SortBindingKey, ValueExpr,
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
    let hops = rel
        .var_len
        .map_or(HopBounds { min: 1, max: 1 }, |var_len| HopBounds {
            min: var_len.min,
            max: var_len.max,
        });
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
            ScopedBinding::PathFunction(func) => ReturnBinding::PathFunction { func, name },
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
                ScopedBinding::PathFunction(func) => ReturnBinding::PathFunction { func, name },
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
