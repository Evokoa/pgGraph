//! Lexer for the supported GQL subset.

use super::errors::{GqlError, Span};

const MAX_QUERY_BYTES: usize = 256 * 1024;
const MAX_TOKENS: usize = 32 * 1024;

/// Token category recognized by the GQL lexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokKind {
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `:`
    Colon,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `$`
    Dollar,
    /// `*`
    Star,
    /// `..`
    DotDot,
    /// `-`
    Dash,
    /// `->`
    ArrowRight,
    /// `<-`
    ArrowLeft,
    /// `=`
    Eq,
    /// `<>`
    Neq,
    /// `<`
    Lt,
    /// `<=`
    Lte,
    /// `>`
    Gt,
    /// `>=`
    Gte,
    /// Identifier.
    Ident,
    /// Quoted string literal.
    String,
    /// Integer literal.
    Int,
    /// Floating-point literal.
    Float,
    /// `MATCH`
    Match,
    /// `CREATE`
    Create,
    /// `SET`
    Set,
    /// `DELETE`
    Delete,
    /// `WHERE`
    Where,
    /// `RETURN`
    Return,
    /// `DISTINCT`
    Distinct,
    /// `ORDER`
    Order,
    /// `BY`
    By,
    /// `ASC`
    Asc,
    /// `DESC`
    Desc,
    /// `SKIP`
    Skip,
    /// `LIMIT`
    Limit,
    /// `AND`
    And,
    /// `OR`
    Or,
    /// `NOT`
    Not,
    /// `IN`
    In,
    /// `IS`
    Is,
    /// `NULL`
    Null,
    /// `TRUE`
    True,
    /// `FALSE`
    False,
    /// `AS`
    As,
    /// End of input.
    Eof,
}

/// Token with its source text and byte span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    /// Token category.
    pub(crate) kind: TokKind,
    /// Byte span in the original query.
    pub(crate) span: Span,
    /// Exact source text for identifiers and literals.
    pub(crate) text: String,
}

/// Convert query text into tokens ending with [`TokKind::Eof`].
///
/// # Errors
///
/// Returns [`GqlErrorKind::Syntax`](super::errors::GqlErrorKind::Syntax) when
/// the query is too large, a string is unterminated, a number is malformed, an
/// unsupported byte appears, or the token budget is exceeded.
pub(crate) fn tokenize(input: &str) -> Result<Vec<Token>, GqlError> {
    if input.len() > MAX_QUERY_BYTES {
        return Err(GqlError::syntax(
            Span::new(MAX_QUERY_BYTES, input.len()),
            "query exceeds the maximum GQL length",
        ));
    }

    let mut lexer = Lexer { input, pos: 0 };
    let mut tokens = Vec::new();
    loop {
        if tokens.len() >= MAX_TOKENS {
            return Err(GqlError::syntax(
                Span::new(lexer.pos, lexer.pos),
                "query exceeds the maximum GQL token count",
            ));
        }
        let token = lexer.next_token()?;
        let done = token.kind == TokKind::Eof;
        tokens.push(token);
        if done {
            return Ok(tokens);
        }
    }
}

struct Lexer<'a> {
    input: &'a str,
    pos: usize,
}

impl Lexer<'_> {
    fn next_token(&mut self) -> Result<Token, GqlError> {
        self.skip_whitespace();
        let start = self.pos;
        let Some(ch) = self.peek_char() else {
            return Ok(Token {
                kind: TokKind::Eof,
                span: Span::new(start, start),
                text: String::new(),
            });
        };

        match ch {
            '(' => Ok(self.single(start, TokKind::LParen)),
            ')' => Ok(self.single(start, TokKind::RParen)),
            '[' => Ok(self.single(start, TokKind::LBracket)),
            ']' => Ok(self.single(start, TokKind::RBracket)),
            '{' => Ok(self.single(start, TokKind::LBrace)),
            '}' => Ok(self.single(start, TokKind::RBrace)),
            ':' => Ok(self.single(start, TokKind::Colon)),
            ',' => Ok(self.single(start, TokKind::Comma)),
            '$' => Ok(self.single(start, TokKind::Dollar)),
            '*' => Ok(self.single(start, TokKind::Star)),
            '=' => Ok(self.single(start, TokKind::Eq)),
            '-' => {
                self.bump_char();
                if self.consume_char('>') {
                    Ok(self.token(start, TokKind::ArrowRight))
                } else {
                    Ok(self.token(start, TokKind::Dash))
                }
            }
            '<' => {
                self.bump_char();
                if self.consume_char('-') {
                    Ok(self.token(start, TokKind::ArrowLeft))
                } else if self.consume_char('=') {
                    Ok(self.token(start, TokKind::Lte))
                } else if self.consume_char('>') {
                    Ok(self.token(start, TokKind::Neq))
                } else {
                    Ok(self.token(start, TokKind::Lt))
                }
            }
            '>' => {
                self.bump_char();
                if self.consume_char('=') {
                    Ok(self.token(start, TokKind::Gte))
                } else {
                    Ok(self.token(start, TokKind::Gt))
                }
            }
            '.' => {
                self.bump_char();
                if self.consume_char('.') {
                    Ok(self.token(start, TokKind::DotDot))
                } else {
                    Ok(self.token(start, TokKind::Dot))
                }
            }
            '\'' | '"' => self.string(start, ch),
            c if c.is_ascii_digit() => self.number(start),
            c if is_ident_start(c) => Ok(self.ident_or_keyword(start)),
            _ => Err(GqlError::syntax(
                Span::new(start, start + ch.len_utf8()),
                format!("unexpected character {ch:?}"),
            )),
        }
    }

    fn single(&mut self, start: usize, kind: TokKind) -> Token {
        self.bump_char();
        self.token(start, kind)
    }

    fn token(&self, start: usize, kind: TokKind) -> Token {
        Token {
            kind,
            span: Span::new(start, self.pos),
            text: self.input[start..self.pos].to_string(),
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek_char(), Some(ch) if ch.is_whitespace()) {
            self.bump_char();
        }
    }

    fn string(&mut self, start: usize, quote: char) -> Result<Token, GqlError> {
        self.bump_char();
        let mut escaped = false;
        while let Some(ch) = self.peek_char() {
            self.bump_char();
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                return Ok(self.token(start, TokKind::String));
            }
        }
        Err(GqlError::syntax(
            Span::new(start, self.pos),
            "unterminated string literal",
        ))
    }

    fn number(&mut self, start: usize) -> Result<Token, GqlError> {
        self.take_while(|ch| ch.is_ascii_digit());
        let mut kind = TokKind::Int;
        if self.peek_char() == Some('.') && self.peek_next_char() != Some('.') {
            kind = TokKind::Float;
            self.bump_char();
            let digits_start = self.pos;
            self.take_while(|ch| ch.is_ascii_digit());
            if digits_start == self.pos {
                return Err(GqlError::syntax(
                    Span::new(start, self.pos),
                    "float literal requires digits after decimal point",
                ));
            }
        }
        Ok(self.token(start, kind))
    }

    fn ident_or_keyword(&mut self, start: usize) -> Token {
        self.bump_char();
        self.take_while(is_ident_continue);
        let text = &self.input[start..self.pos];
        let kind = match text.to_ascii_uppercase().as_str() {
            "MATCH" => TokKind::Match,
            "CREATE" => TokKind::Create,
            "SET" => TokKind::Set,
            "DELETE" => TokKind::Delete,
            "WHERE" => TokKind::Where,
            "RETURN" => TokKind::Return,
            "DISTINCT" => TokKind::Distinct,
            "ORDER" => TokKind::Order,
            "BY" => TokKind::By,
            "ASC" => TokKind::Asc,
            "DESC" => TokKind::Desc,
            "SKIP" => TokKind::Skip,
            "LIMIT" => TokKind::Limit,
            "AND" => TokKind::And,
            "OR" => TokKind::Or,
            "NOT" => TokKind::Not,
            "IN" => TokKind::In,
            "IS" => TokKind::Is,
            "NULL" => TokKind::Null,
            "TRUE" => TokKind::True,
            "FALSE" => TokKind::False,
            "AS" => TokKind::As,
            _ => TokKind::Ident,
        };
        self.token(start, kind)
    }

    fn take_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while matches!(self.peek_char(), Some(ch) if predicate(ch)) {
            self.bump_char();
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn peek_next_char(&self) -> Option<char> {
        let mut chars = self.input[self.pos..].chars();
        chars.next()?;
        chars.next()
    }

    fn bump_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::{tokenize, TokKind};

    #[test]
    fn tokenizes_keywords_case_insensitively() {
        let tokens =
            tokenize("match (n)-[r:knows]->(m) delete r return n").expect("lexing succeeds");
        let kinds = tokens.iter().map(|token| token.kind).collect::<Vec<_>>();

        assert_eq!(
            kinds,
            vec![
                TokKind::Match,
                TokKind::LParen,
                TokKind::Ident,
                TokKind::RParen,
                TokKind::Dash,
                TokKind::LBracket,
                TokKind::Ident,
                TokKind::Colon,
                TokKind::Ident,
                TokKind::RBracket,
                TokKind::ArrowRight,
                TokKind::LParen,
                TokKind::Ident,
                TokKind::RParen,
                TokKind::Delete,
                TokKind::Ident,
                TokKind::Return,
                TokKind::Ident,
                TokKind::Eof,
            ]
        );
    }

    #[test]
    fn rejects_unterminated_strings() {
        let err = tokenize("MATCH (n {name: 'Ada}) RETURN n").expect_err("lexing should fail");

        assert!(err.to_string().contains("unterminated string"));
    }
}
