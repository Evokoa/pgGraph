//! Error types and source spans for GQL frontend diagnostics.

use std::fmt;
use std::ops::Range;

/// Byte span into the original query text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Span {
    /// Inclusive byte offset where the item starts.
    pub(crate) start: u32,
    /// Exclusive byte offset where the item ends.
    pub(crate) end: u32,
}

impl Span {
    /// Construct a span from byte offsets.
    pub(crate) fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    /// Return this span as a standard range.
    pub(crate) fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// GQL frontend error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} at byte {start}..{end}", start = span.start, end = span.end)]
pub(crate) struct GqlError {
    /// Error category and message.
    pub(crate) kind: GqlErrorKind,
    /// Query byte range where the error was detected.
    pub(crate) span: Span,
}

impl GqlError {
    /// Create a syntax error at `span`.
    pub(crate) fn syntax(span: Span, message: impl Into<String>) -> Self {
        Self {
            kind: GqlErrorKind::Syntax {
                message: message.into(),
            },
            span,
        }
    }

    /// Create an unsupported-feature error at `span`.
    pub(crate) fn unsupported(span: Span, feature: impl Into<String>) -> Self {
        Self {
            kind: GqlErrorKind::Unsupported {
                feature: feature.into(),
            },
            span,
        }
    }
}

/// Stable frontend error categories used before SQLSTATE mapping exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GqlErrorKind {
    /// Query text does not match the supported grammar.
    Syntax { message: String },
    /// Query text uses valid GQL syntax outside the current supported subset.
    Unsupported { feature: String },
}

impl fmt::Display for GqlErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { message } => write!(f, "GQL syntax error: {message}"),
            Self::Unsupported { feature } => write!(f, "unsupported GQL feature: {feature}"),
        }
    }
}
