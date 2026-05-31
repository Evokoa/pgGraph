//! Recursive-descent parser for the supported GQL subset.

use super::ast::{
    AggregateArg, AggregateFunc, CmpOp, CreateClause, CreateQuery, DeleteClause, DeleteQuery,
    Direction, Expr, Ident, Literal, LiteralValue, MatchClause, NodePat, Operand, Pattern,
    PropertyRef, Query, RelPat, ReturnClause, ReturnExpr, ReturnItem, SetClause, SetQuery,
    SortItem, SortKey, Statement, VarLen, WithClause,
};
use super::errors::{GqlError, Span};
use super::lexer::{tokenize, TokKind, Token};

const MAX_PREFIX_NOT: usize = 512;

/// Parse a GQL read query into an AST.
///
/// # Errors
///
/// Returns [`GqlError`] when the input is not valid syntax for the supported
/// subset or uses a clause reserved for a later compatibility phase.
#[cfg(test)]
pub(crate) fn parse(input: &str) -> Result<Query, GqlError> {
    Parser::new(input)?.parse_query()
}

/// Parse a supported GQL statement into an AST.
///
/// # Errors
///
/// Returns [`GqlError`] when the input is not valid syntax for the supported
/// subset or uses a clause reserved for a later compatibility phase.
pub(crate) fn parse_statement(input: &str) -> Result<Statement, GqlError> {
    Parser::new(input)?.parse_statement()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(input: &str) -> Result<Self, GqlError> {
        Ok(Self {
            tokens: tokenize(input)?,
            pos: 0,
        })
    }

    fn parse_statement(&mut self) -> Result<Statement, GqlError> {
        match self.peek() {
            TokKind::Create => self.parse_create_query().map(Statement::Create),
            TokKind::Match | TokKind::Optional if self.statement_contains_set_before_return() => {
                self.parse_set_query().map(Statement::Set)
            }
            TokKind::Match | TokKind::Optional
                if self.statement_contains_delete_before_return() =>
            {
                self.parse_delete_query().map(Statement::Delete)
            }
            _ => self.parse_query().map(Statement::Read),
        }
    }

    fn statement_contains_set_before_return(&self) -> bool {
        self.tokens
            .iter()
            .skip(self.pos)
            .take_while(|token| token.kind != TokKind::Return && token.kind != TokKind::Eof)
            .any(|token| token.kind == TokKind::Set)
    }

    fn statement_contains_delete_before_return(&self) -> bool {
        self.tokens
            .iter()
            .skip(self.pos)
            .take_while(|token| token.kind != TokKind::Return && token.kind != TokKind::Eof)
            .any(|token| token.kind == TokKind::Delete)
    }

    fn parse_query(&mut self) -> Result<Query, GqlError> {
        let start = self.current().span.start as usize;
        let match_ = self.parse_match_clause()?;
        let where_ = if self.consume(TokKind::Where).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let mut with_ = Vec::new();
        while self.peek_ident_keyword("WITH") {
            with_.push(self.parse_with_clause()?);
        }
        let return_ = self.parse_return_clause()?;
        let order_by = if self.consume(TokKind::Order).is_some() {
            self.expect(TokKind::By, "expected BY after ORDER")?;
            self.parse_order_by()?
        } else {
            Vec::new()
        };
        let skip = if self.consume(TokKind::Skip).is_some() {
            Some(self.parse_u64("SKIP")?)
        } else {
            None
        };
        let limit = if self.consume(TokKind::Limit).is_some() {
            Some(self.parse_u64("LIMIT")?)
        } else {
            None
        };
        self.reject_known_later_clauses()?;
        let end = self.expect(TokKind::Eof, "expected end of query")?.span.end as usize;

        Ok(Query {
            match_,
            where_,
            with_,
            return_,
            order_by,
            skip,
            limit,
            span: Span::new(start, end),
        })
    }

    fn parse_create_query(&mut self) -> Result<CreateQuery, GqlError> {
        let start = self.current().span.start as usize;
        let create = self.parse_create_clause()?;
        let return_ = self.parse_return_clause()?;
        self.reject_known_later_clauses()?;
        let end = self.expect(TokKind::Eof, "expected end of query")?.span.end as usize;

        Ok(CreateQuery {
            create,
            return_,
            span: Span::new(start, end),
        })
    }

    fn parse_set_query(&mut self) -> Result<SetQuery, GqlError> {
        let start = self.current().span.start as usize;
        let match_ = self.parse_match_clause()?;
        let where_ = if self.consume(TokKind::Where).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let set = self.parse_set_clause()?;
        let return_ = self.parse_return_clause()?;
        self.reject_known_later_clauses()?;
        let end = self.expect(TokKind::Eof, "expected end of query")?.span.end as usize;

        Ok(SetQuery {
            match_,
            where_,
            set,
            return_,
            span: Span::new(start, end),
        })
    }

    fn parse_delete_query(&mut self) -> Result<DeleteQuery, GqlError> {
        let start = self.current().span.start as usize;
        let match_ = self.parse_match_clause()?;
        let where_ = if self.consume(TokKind::Where).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let delete = self.parse_delete_clause()?;
        let return_ = self.parse_return_clause()?;
        self.reject_known_later_clauses()?;
        let end = self.expect(TokKind::Eof, "expected end of query")?.span.end as usize;

        Ok(DeleteQuery {
            match_,
            where_,
            delete,
            return_,
            span: Span::new(start, end),
        })
    }

    fn parse_create_clause(&mut self) -> Result<CreateClause, GqlError> {
        let start = self
            .expect(TokKind::Create, "expected CREATE clause")?
            .span
            .start as usize;
        let node = self.parse_node_pat()?;
        Ok(CreateClause {
            span: Span::new(start, node.span.end as usize),
            node,
        })
    }

    fn parse_set_clause(&mut self) -> Result<SetClause, GqlError> {
        let start = self.expect(TokKind::Set, "expected SET clause")?.span.start as usize;
        let target = self.parse_property_ref()?;
        self.expect(TokKind::Eq, "expected '=' in SET clause")?;
        let value = self.parse_literal_or_param()?;
        let end = match &value {
            Operand::Literal(Literal::Value { span, .. }) | Operand::Param { span, .. } => {
                span.end as usize
            }
            Operand::Property { span, .. } | Operand::List { span, .. } => span.end as usize,
        };
        Ok(SetClause {
            target,
            value,
            span: Span::new(start, end),
        })
    }

    fn parse_delete_clause(&mut self) -> Result<DeleteClause, GqlError> {
        let start = self
            .expect(TokKind::Delete, "expected DELETE clause")?
            .span
            .start as usize;
        let var = self.parse_ident()?;
        Ok(DeleteClause {
            span: Span::new(start, var.span.end as usize),
            var,
        })
    }

    fn parse_property_ref(&mut self) -> Result<PropertyRef, GqlError> {
        let var = self.parse_ident()?;
        self.expect(TokKind::Dot, "expected '.' in property reference")?;
        let property = self.parse_property_path()?;
        Ok(PropertyRef {
            span: Span::new(var.span.start as usize, property.span.end as usize),
            var,
            property,
        })
    }

    fn parse_match_clause(&mut self) -> Result<MatchClause, GqlError> {
        let optional = self.consume(TokKind::Optional).is_some();
        let start = self
            .expect(TokKind::Match, "expected MATCH clause")?
            .span
            .start as usize;
        let pattern = self.parse_pattern()?;
        let end = pattern
            .tail
            .last()
            .map_or(pattern.start.span.end, |(_, node)| node.span.end) as usize;
        Ok(MatchClause {
            optional,
            pattern,
            span: Span::new(start, end),
        })
    }

    fn parse_pattern(&mut self) -> Result<Pattern, GqlError> {
        let start = self.parse_node_pat()?;
        let mut tail = Vec::new();
        while matches!(self.peek(), TokKind::Dash | TokKind::ArrowLeft) {
            let rel = self.parse_rel_pat()?;
            let node = self.parse_node_pat()?;
            tail.push((rel, node));
        }
        let end = tail
            .last()
            .map_or(start.span.end, |(_, node)| node.span.end);
        let span = Span::new(start.span.start as usize, end as usize);
        Ok(Pattern { start, tail, span })
    }

    fn parse_node_pat(&mut self) -> Result<NodePat, GqlError> {
        let start = self
            .expect(TokKind::LParen, "expected node pattern")?
            .span
            .start as usize;
        let var = if self.peek() == TokKind::Ident {
            Some(self.parse_ident()?)
        } else {
            None
        };
        let label = if self.consume(TokKind::Colon).is_some() {
            Some(self.parse_ident()?)
        } else {
            None
        };
        let props = if self.consume(TokKind::LBrace).is_some() {
            self.parse_prop_map()?
        } else {
            Vec::new()
        };
        let end = self
            .expect(TokKind::RParen, "expected ')' after node pattern")?
            .span
            .end as usize;
        Ok(NodePat {
            var,
            label,
            props,
            span: Span::new(start, end),
        })
    }

    fn parse_rel_pat(&mut self) -> Result<RelPat, GqlError> {
        if self.peek() == TokKind::ArrowLeft {
            let start = self.advance().span.start as usize;
            let (var, rel_type, var_len, props) = self.parse_optional_rel_detail()?;
            let end = self
                .expect(TokKind::Dash, "expected '-' after inbound relationship")?
                .span
                .end as usize;
            return Ok(RelPat {
                var,
                rel_type,
                direction: Direction::In,
                var_len,
                props,
                span: Span::new(start, end),
            });
        }

        let start = self
            .expect(TokKind::Dash, "expected relationship pattern")?
            .span
            .start as usize;
        let (var, rel_type, var_len, props) = self.parse_optional_rel_detail()?;
        let direction = if self.consume(TokKind::ArrowRight).is_some() {
            Direction::Out
        } else {
            self.expect(TokKind::Dash, "expected '-' or '->' after relationship")?;
            Direction::Undirected
        };
        let end = self.previous().span.end as usize;
        Ok(RelPat {
            var,
            rel_type,
            direction,
            var_len,
            props,
            span: Span::new(start, end),
        })
    }

    fn parse_optional_rel_detail(
        &mut self,
    ) -> Result<
        (
            Option<Ident>,
            Option<Ident>,
            Option<VarLen>,
            Vec<(Ident, Operand)>,
        ),
        GqlError,
    > {
        if self.consume(TokKind::LBracket).is_none() {
            return Ok((None, None, None, Vec::new()));
        }
        let var = if self.peek() == TokKind::Ident {
            Some(self.parse_ident()?)
        } else {
            None
        };
        let rel_type = if self.consume(TokKind::Colon).is_some() {
            Some(self.parse_ident()?)
        } else {
            None
        };
        let var_len = if self.consume(TokKind::Star).is_some() {
            Some(self.parse_var_len()?)
        } else {
            None
        };
        let props = if self.consume(TokKind::LBrace).is_some() {
            self.parse_prop_map()?
        } else {
            Vec::new()
        };
        self.expect(TokKind::RBracket, "expected ']' after relationship detail")?;
        Ok((var, rel_type, var_len, props))
    }

    fn parse_var_len(&mut self) -> Result<VarLen, GqlError> {
        let span = self.previous().span;
        let mut min = 1;
        if self.peek() == TokKind::Int {
            min = self.parse_u32("variable-length lower bound")?;
        }
        if self.consume(TokKind::DotDot).is_none() {
            return Err(GqlError::unsupported(
                span,
                "variable-length relationships require an explicit upper bound",
            ));
        }
        if self.peek() != TokKind::Int {
            return Err(GqlError::unsupported(
                self.current().span,
                "variable-length relationships require an explicit upper bound",
            ));
        }
        let max = self.parse_u32("variable-length upper bound")?;
        let var_len_span = Span::new(span.start as usize, self.previous().span.end as usize);
        if min > max {
            return Err(GqlError::syntax(
                span,
                "variable-length lower bound cannot exceed upper bound",
            ));
        }
        Ok(VarLen {
            min,
            max,
            span: var_len_span,
        })
    }

    fn parse_prop_map(&mut self) -> Result<Vec<(Ident, Operand)>, GqlError> {
        let mut props = Vec::new();
        if self.consume(TokKind::RBrace).is_some() {
            return Ok(props);
        }
        loop {
            let key = self.parse_property_path()?;
            self.expect(TokKind::Colon, "expected ':' after property name")?;
            let value = self.parse_literal_or_param()?;
            props.push((key, value));
            if self.consume(TokKind::Comma).is_none() {
                break;
            }
        }
        self.expect(TokKind::RBrace, "expected '}' after property map")?;
        Ok(props)
    }

    fn parse_return_clause(&mut self) -> Result<ReturnClause, GqlError> {
        let start = self
            .expect(TokKind::Return, "expected RETURN clause")?
            .span
            .start as usize;
        let distinct = self.consume(TokKind::Distinct).is_some();
        let mut items = Vec::new();
        loop {
            items.push(self.parse_return_item()?);
            if self.consume(TokKind::Comma).is_none() {
                break;
            }
        }
        let end = items.last().map_or(start, |item| item.span.end as usize);
        Ok(ReturnClause {
            distinct,
            items,
            span: Span::new(start, end),
        })
    }

    fn parse_with_clause(&mut self) -> Result<WithClause, GqlError> {
        let start = self
            .expect_ident_keyword("WITH", "expected WITH clause")?
            .span
            .start as usize;
        let distinct = self.consume(TokKind::Distinct).is_some();
        let mut items = Vec::new();
        loop {
            items.push(self.parse_return_item()?);
            if self.consume(TokKind::Comma).is_none() {
                break;
            }
        }
        let end = items.last().map_or(start, |item| item.span.end as usize);
        Ok(WithClause {
            distinct,
            items,
            span: Span::new(start, end),
        })
    }

    fn parse_return_item(&mut self) -> Result<ReturnItem, GqlError> {
        let start = self.current().span.start as usize;
        let expr = self.parse_return_expr()?;
        let alias = if self.consume(TokKind::As).is_some() {
            Some(self.parse_ident()?)
        } else {
            None
        };
        let end = alias.as_ref().map_or_else(
            || self.previous().span.end as usize,
            |ident| ident.span.end as usize,
        );
        Ok(ReturnItem {
            expr,
            alias,
            span: Span::new(start, end),
        })
    }

    fn parse_return_expr(&mut self) -> Result<ReturnExpr, GqlError> {
        let ident = self.parse_ident()?;
        if self.consume(TokKind::LParen).is_some() {
            if let Some(func) = aggregate_func(&ident.text) {
                let distinct = self.consume(TokKind::Distinct).is_some();
                let arg = self.parse_aggregate_arg()?;
                self.expect(TokKind::RParen, "expected ')' after aggregate argument")?;
                let end = self.previous().span.end as usize;
                return Ok(ReturnExpr::Aggregate {
                    func,
                    distinct,
                    arg,
                    name: ident.clone(),
                    span: Span::new(ident.span.start as usize, end),
                });
            }
            let mut args = Vec::new();
            if self.consume(TokKind::RParen).is_none() {
                loop {
                    args.push(self.parse_ident()?);
                    if self.consume(TokKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect(TokKind::RParen, "expected ')' after function arguments")?;
            }
            let end = self.previous().span.end as usize;
            return Ok(ReturnExpr::Func {
                span: Span::new(ident.span.start as usize, end),
                name: ident,
                args,
            });
        }
        if self.consume(TokKind::Dot).is_some() {
            let property = self.parse_property_path()?;
            Ok(ReturnExpr::Property {
                span: Span::new(ident.span.start as usize, property.span.end as usize),
                var: ident,
                property,
            })
        } else {
            Ok(ReturnExpr::Var {
                span: ident.span,
                var: ident,
            })
        }
    }

    fn parse_aggregate_arg(&mut self) -> Result<AggregateArg, GqlError> {
        if self.consume(TokKind::Star).is_some() {
            let span = self.previous().span;
            return Ok(AggregateArg::All { span });
        }
        let var = self.parse_ident()?;
        if self.consume(TokKind::Dot).is_some() {
            let property = self.parse_property_path()?;
            Ok(AggregateArg::Property {
                span: Span::new(var.span.start as usize, property.span.end as usize),
                var,
                property,
            })
        } else {
            Ok(AggregateArg::Var {
                span: var.span,
                var,
            })
        }
    }

    fn parse_order_by(&mut self) -> Result<Vec<SortItem>, GqlError> {
        let mut items = Vec::new();
        loop {
            items.push(self.parse_sort_item()?);
            if self.consume(TokKind::Comma).is_none() {
                break;
            }
        }
        Ok(items)
    }

    fn parse_sort_item(&mut self) -> Result<SortItem, GqlError> {
        let start = self.current().span.start as usize;
        let ident = self.parse_ident()?;
        let key = if self.consume(TokKind::Dot).is_some() {
            let property = self.parse_property_path()?;
            SortKey::Property {
                span: Span::new(ident.span.start as usize, property.span.end as usize),
                var: ident,
                property,
            }
        } else {
            SortKey::Alias {
                span: ident.span,
                alias: ident,
            }
        };
        let desc = if self.consume(TokKind::Desc).is_some() {
            true
        } else {
            let _asc = self.consume(TokKind::Asc);
            false
        };
        let end = self.previous().span.end as usize;
        Ok(SortItem {
            key,
            desc,
            span: Span::new(start, end),
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, GqlError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, GqlError> {
        let mut expr = self.parse_and()?;
        while self.consume(TokKind::Or).is_some() {
            let rhs = self.parse_and()?;
            let span = join_expr_span(&expr, &rhs);
            expr = Expr::Or {
                lhs: Box::new(expr),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<Expr, GqlError> {
        let mut expr = self.parse_not()?;
        while self.consume(TokKind::And).is_some() {
            let rhs = self.parse_not()?;
            let span = join_expr_span(&expr, &rhs);
            expr = Expr::And {
                lhs: Box::new(expr),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_not(&mut self) -> Result<Expr, GqlError> {
        let mut not_spans = Vec::new();
        while self.peek() == TokKind::Not {
            if not_spans.len() >= MAX_PREFIX_NOT {
                return Err(GqlError::syntax(
                    self.current().span,
                    "too many nested NOT operators",
                ));
            }
            not_spans.push(self.advance().span);
        }
        let mut expr = self.parse_comparison()?;
        while let Some(not_span) = not_spans.pop() {
            let span = Span::new(not_span.start as usize, expr_span(&expr).end as usize);
            expr = Expr::Not {
                expr: Box::new(expr),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_comparison(&mut self) -> Result<Expr, GqlError> {
        let start = self.current().span.start as usize;
        let lhs = self.parse_operand()?;
        let (op, rhs) = match self.peek() {
            TokKind::Eq => {
                self.advance();
                (CmpOp::Eq, Some(self.parse_operand()?))
            }
            TokKind::Neq => {
                self.advance();
                (CmpOp::Neq, Some(self.parse_operand()?))
            }
            TokKind::Lt => {
                self.advance();
                (CmpOp::Lt, Some(self.parse_operand()?))
            }
            TokKind::Lte => {
                self.advance();
                (CmpOp::Lte, Some(self.parse_operand()?))
            }
            TokKind::Gt => {
                self.advance();
                (CmpOp::Gt, Some(self.parse_operand()?))
            }
            TokKind::Gte => {
                self.advance();
                (CmpOp::Gte, Some(self.parse_operand()?))
            }
            TokKind::In => {
                self.advance();
                (CmpOp::In, Some(self.parse_operand()?))
            }
            TokKind::Is => {
                self.advance();
                let op = if self.consume(TokKind::Not).is_some() {
                    CmpOp::IsNotNull
                } else {
                    CmpOp::IsNull
                };
                self.expect(TokKind::Null, "expected NULL after IS predicate")?;
                (op, None)
            }
            _ => {
                return Err(GqlError::syntax(
                    self.current().span,
                    "expected comparison operator",
                ));
            }
        };
        let end = self.previous().span.end as usize;
        Ok(Expr::Compare {
            lhs,
            op,
            rhs,
            span: Span::new(start, end),
        })
    }

    fn parse_operand(&mut self) -> Result<Operand, GqlError> {
        match self.peek() {
            TokKind::Ident => {
                let var = self.parse_ident()?;
                self.expect(TokKind::Dot, "expected property reference")?;
                let property = self.parse_property_path()?;
                let span = Span::new(var.span.start as usize, property.span.end as usize);
                Ok(Operand::Property {
                    var,
                    property,
                    span,
                })
            }
            TokKind::Dollar => {
                let start = self.advance().span.start as usize;
                let name = self.parse_ident()?;
                Ok(Operand::Param {
                    span: Span::new(start, name.span.end as usize),
                    name,
                })
            }
            TokKind::LBracket => self.parse_literal_list(),
            TokKind::String
            | TokKind::Int
            | TokKind::Float
            | TokKind::True
            | TokKind::False
            | TokKind::Null => Ok(Operand::Literal(self.parse_literal()?)),
            _ => Err(GqlError::syntax(self.current().span, "expected operand")),
        }
    }

    fn parse_literal_or_param(&mut self) -> Result<Operand, GqlError> {
        if self.peek() == TokKind::Dollar {
            let start = self.advance().span.start as usize;
            let name = self.parse_ident()?;
            Ok(Operand::Param {
                span: Span::new(start, name.span.end as usize),
                name,
            })
        } else if self.peek() == TokKind::LBracket {
            self.parse_literal_list()
        } else {
            Ok(Operand::Literal(self.parse_literal()?))
        }
    }

    fn parse_literal_list(&mut self) -> Result<Operand, GqlError> {
        let start = self.expect(TokKind::LBracket, "expected '['")?.span.start as usize;
        let mut values = Vec::new();
        if let Some(end) = self.consume(TokKind::RBracket) {
            return Ok(Operand::List {
                values,
                span: Span::new(start, end.span.end as usize),
            });
        }
        loop {
            values.push(self.parse_literal()?);
            if self.consume(TokKind::Comma).is_none() {
                break;
            }
        }
        let end = self
            .expect(TokKind::RBracket, "expected ']' after list")?
            .span
            .end as usize;
        Ok(Operand::List {
            values,
            span: Span::new(start, end),
        })
    }

    fn parse_literal(&mut self) -> Result<Literal, GqlError> {
        let token = self.advance();
        match token.kind {
            TokKind::String => decode_string_literal(&token.text)
                .map(LiteralValue::Str)
                .map(|value| Literal::Value {
                    value,
                    span: token.span,
                })
                .map_err(|message| GqlError::syntax(token.span, message)),
            TokKind::Int => token
                .text
                .parse::<i64>()
                .map(LiteralValue::Int)
                .map(|value| Literal::Value {
                    value,
                    span: token.span,
                })
                .map_err(|err| GqlError::syntax(token.span, format!("invalid integer: {err}"))),
            TokKind::Float => token
                .text
                .parse::<f64>()
                .map(LiteralValue::Float)
                .map(|value| Literal::Value {
                    value,
                    span: token.span,
                })
                .map_err(|err| GqlError::syntax(token.span, format!("invalid float: {err}"))),
            TokKind::True => Ok(Literal::Value {
                value: LiteralValue::Bool(true),
                span: token.span,
            }),
            TokKind::False => Ok(Literal::Value {
                value: LiteralValue::Bool(false),
                span: token.span,
            }),
            TokKind::Null => Ok(Literal::Value {
                value: LiteralValue::Null,
                span: token.span,
            }),
            _ => Err(GqlError::syntax(token.span, "expected literal")),
        }
    }

    fn parse_ident(&mut self) -> Result<Ident, GqlError> {
        let token = self.advance();
        if token.kind != TokKind::Ident {
            return Err(GqlError::syntax(token.span, "expected identifier"));
        }
        Ok(Ident {
            text: token.text,
            span: token.span,
        })
    }

    fn parse_property_path(&mut self) -> Result<Ident, GqlError> {
        let first = self.parse_ident()?;
        let mut text = first.text;
        let mut end = first.span.end;
        while self.consume(TokKind::Dot).is_some() {
            let part = self.parse_ident()?;
            text.push('.');
            text.push_str(&part.text);
            end = part.span.end;
        }
        Ok(Ident {
            text,
            span: Span::new(first.span.start as usize, end as usize),
        })
    }

    fn parse_u64(&mut self, label: &str) -> Result<u64, GqlError> {
        let token = self.advance();
        if token.kind != TokKind::Int {
            return Err(GqlError::syntax(
                token.span,
                format!("{label} requires an integer"),
            ));
        }
        token
            .text
            .parse::<u64>()
            .map_err(|err| GqlError::syntax(token.span, format!("invalid {label}: {err}")))
    }

    fn parse_u32(&mut self, label: &str) -> Result<u32, GqlError> {
        let token = self.advance();
        if token.kind != TokKind::Int {
            return Err(GqlError::syntax(
                token.span,
                format!("{label} requires an integer"),
            ));
        }
        token
            .text
            .parse::<u32>()
            .map_err(|err| GqlError::syntax(token.span, format!("invalid {label}: {err}")))
    }

    fn reject_known_later_clauses(&self) -> Result<(), GqlError> {
        Ok(())
    }

    fn peek_ident_keyword(&self, keyword: &str) -> bool {
        let token = self.current();
        token.kind == TokKind::Ident && token.text.eq_ignore_ascii_case(keyword)
    }

    fn expect_ident_keyword(&mut self, keyword: &str, message: &str) -> Result<Token, GqlError> {
        let token = self.advance();
        if token.kind == TokKind::Ident && token.text.eq_ignore_ascii_case(keyword) {
            Ok(token)
        } else {
            Err(GqlError::syntax(token.span, message))
        }
    }

    fn expect(&mut self, kind: TokKind, message: &str) -> Result<Token, GqlError> {
        let token = self.advance();
        if token.kind == kind {
            Ok(token)
        } else {
            Err(GqlError::syntax(token.span, message))
        }
    }

    fn consume(&mut self, kind: TokKind) -> Option<Token> {
        if self.peek() == kind {
            Some(self.advance())
        } else {
            None
        }
    }

    fn peek(&self) -> TokKind {
        self.current().kind
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.pos.saturating_sub(1)]
    }

    fn advance(&mut self) -> Token {
        let token = self.current().clone();
        if token.kind != TokKind::Eof {
            self.pos += 1;
        }
        token
    }
}

fn join_expr_span(lhs: &Expr, rhs: &Expr) -> Span {
    let lhs = expr_span(lhs);
    let rhs = expr_span(rhs);
    Span::new(lhs.start as usize, rhs.end as usize)
}

fn aggregate_func(name: &str) -> Option<AggregateFunc> {
    match name.to_ascii_lowercase().as_str() {
        "count" => Some(AggregateFunc::Count),
        "sum" => Some(AggregateFunc::Sum),
        "avg" => Some(AggregateFunc::Avg),
        "min" => Some(AggregateFunc::Min),
        "max" => Some(AggregateFunc::Max),
        "collect" => Some(AggregateFunc::Collect),
        _ => None,
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

fn decode_string_literal(raw: &str) -> Result<String, String> {
    let quote = raw
        .chars()
        .next()
        .ok_or_else(|| "empty string literal".to_string())?;
    let inner = raw
        .strip_prefix(quote)
        .and_then(|value| value.strip_suffix(quote))
        .ok_or_else(|| "invalid string literal".to_string())?;
    let mut decoded = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let escaped = chars
            .next()
            .ok_or_else(|| "unterminated string escape".to_string())?;
        match escaped {
            '\\' => decoded.push('\\'),
            '\'' => decoded.push('\''),
            '"' => decoded.push('"'),
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            other => {
                return Err(format!("unsupported string escape \\{other}"));
            }
        }
    }
    Ok(decoded)
}
