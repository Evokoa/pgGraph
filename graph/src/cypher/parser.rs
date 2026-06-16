//! Parser entrypoint for the openCypher compatibility subset.

use crate::gql::errors::{GqlError, Span};

use super::ast::CypherStatement;

/// Parse openCypher compatibility text.
///
/// The accepted syntax is intentionally the intersection that maps to pgGraph's
/// existing GQL AST. Recognized openCypher-only constructs are returned as
/// [`CypherStatement::Unsupported`] so semantic binding can report stable
/// compatibility diagnostics.
pub(crate) fn parse_statement(input: &str) -> Result<CypherStatement, GqlError> {
    if let Some((feature, span)) = unsupported_feature(input) {
        return Ok(CypherStatement::Unsupported { feature, span });
    }
    let statement = crate::gql::parse_statement(input)?;
    let span = statement_span(&statement);
    Ok(CypherStatement::Compatible {
        statement: Box::new(statement),
        span,
    })
}

fn unsupported_feature(input: &str) -> Option<(String, Span)> {
    let words = super::lexer::words(input);
    let first = words.first()?;
    if let Some(
        kind @ (super::lexer::CypherKeyword::Call
        | super::lexer::CypherKeyword::Foreach
        | super::lexer::CypherKeyword::Load
        | super::lexer::CypherKeyword::Start
        | super::lexer::CypherKeyword::Unwind),
    ) = first.keyword
    {
        return Some((kind.feature_name().to_string(), first.span));
    }

    if let Some((_, token)) = words.iter().enumerate().find(|(index, token)| {
        matches!(token.keyword, Some(super::lexer::CypherKeyword::Union))
            && union_starts_clause(&words, *index)
    }) {
        return Some((
            super::lexer::CypherKeyword::Union
                .feature_name()
                .to_string(),
            token.span,
        ));
    }

    let ddl_prefix = matches!(
        first.keyword,
        Some(
            super::lexer::CypherKeyword::Alter
                | super::lexer::CypherKeyword::Create
                | super::lexer::CypherKeyword::Drop
        )
    );
    let ddl_target = words.get(1).is_some_and(|token| {
        matches!(
            token.keyword,
            Some(
                super::lexer::CypherKeyword::Constraint
                    | super::lexer::CypherKeyword::Database
                    | super::lexer::CypherKeyword::Index
            )
        )
    });
    if ddl_prefix && ddl_target {
        return Some((
            super::lexer::CypherKeyword::Index
                .feature_name()
                .to_string(),
            Span {
                start: first.span.start,
                end: words[1].span.end,
            },
        ));
    }

    None
}

fn union_starts_clause(words: &[super::lexer::WordToken], union_index: usize) -> bool {
    words.get(union_index + 1).is_some_and(|token| {
        matches!(
            token.text.as_str(),
            "CALL" | "CREATE" | "MATCH" | "MERGE" | "OPTIONAL" | "RETURN" | "UNWIND"
        )
    })
}

fn statement_span(statement: &crate::gql::ast::Statement) -> Span {
    match statement {
        crate::gql::ast::Statement::Read(query) => query.span,
        crate::gql::ast::Statement::Create(query) => query.span,
        crate::gql::ast::Statement::CreateRelationship(query) => query.span,
        crate::gql::ast::Statement::Set(query) => query.span,
        crate::gql::ast::Statement::Remove(query) => query.span,
        crate::gql::ast::Statement::Delete(query) => query.span,
        crate::gql::ast::Statement::DetachDelete(query) => query.span,
        crate::gql::ast::Statement::Merge(query) => query.span,
    }
}
