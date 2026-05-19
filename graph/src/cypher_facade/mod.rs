//! Cypher / openCypher v9 frontend for pgGraph.
//!
//! Entry point: the `graph.cypher(text, jsonb)` SQL function (added in
//! M1). This module owns the translation from a Cypher query string to
//! pgGraph operations, using the cyrs frontend (`cyrs-hir`,
//! `cyrs-plan`, `cyrs-schema`, `cyrs-sema`, `cyrs-diag`) for parsing,
//! resolution, sema, and logical plan IR.
//!
//! See: `docs/contributor_guide/cypher-frontend/000-overview.md`
//!
//! Milestone status: **M0 — skeleton & catalog**. The catalog tables
//! and registration SQL functions are wired; `execute()` returns the
//! `NotYetImplemented` error.

pub(crate) mod registration;
pub(crate) mod schema_provider;

use crate::safety::GraphError;

/// Error surface for the Cypher facade.
///
/// Maps onto `ereport` SQLSTATE per
/// `docs/contributor_guide/cypher-frontend/060-diagnostics-and-errors.md`.
#[derive(Debug, thiserror::Error)]
pub enum FacadeError {
    /// Feature is part of the documented milestone plan but not yet
    /// implemented on this branch. Surfaces as SQLSTATE `0A000`
    /// (`feature_not_supported`).
    #[error("graph.cypher: {0}")]
    NotYetImplemented(&'static str),

    /// Catalog read or write failure. Propagates the underlying
    /// `GraphError` SQLSTATE.
    #[error(transparent)]
    Catalog(#[from] GraphError),
}

impl FacadeError {
    /// SQLSTATE for `ereport`.
    pub(crate) fn sqlstate(&self) -> &'static str {
        match self {
            FacadeError::NotYetImplemented(_) => "0A000",
            FacadeError::Catalog(err) => err.sqlstate(),
        }
    }
}
