//! Shared policy vocabulary for graph identity and lifecycle defaults.
//!
//! This module is the single source of truth for named-graph constants that
//! will be written into catalogs, exposed through SQL, and used by runtime
//! selection. The current extension still has one implicit graph; these values
//! make that default explicit before catalog scoping is introduced.

#![allow(
    dead_code,
    reason = "Named-graph policy vocabulary is introduced before every later consumer exists"
)]

use std::fmt;

/// Built-in graph name used by compatibility SQL APIs.
pub(crate) const DEFAULT_GRAPH_NAME: &str = "default";
/// Built-in namespace used when callers do not provide a graph namespace.
pub(crate) const DEFAULT_GRAPH_NAMESPACE: &str = "public";
/// Stable graph identity reserved for the compatibility default graph.
pub(crate) const DEFAULT_GRAPH_ID_TEXT: &str = "00000000-0000-0000-0000-000000000001";

/// Accepted graph ownership and scope classes.
pub(crate) const GRAPH_KINDS: &[&str] = &["global", "user", "tenant", "workspace", "subgraph"];
/// Accepted backend residency policies.
pub(crate) const RESIDENCY_POLICIES: &[&str] = &["hot", "warm", "cold"];
/// Accepted physical materialization policies.
pub(crate) const MATERIALIZATION_POLICIES: &[&str] = &["shared", "dedicated"];
/// Accepted projection modes for catalog defaults and build jobs.
pub(crate) const PROJECTION_MODES: &[&str] = &["csr_readonly", "mutable_overlay"];

/// Durable job statuses used by build, maintenance, and future sync workers.
pub(crate) const JOB_STATUSES: &[&str] = &["queued", "running", "completed", "failed"];
/// Progress phases reported by SQL-visible long-running graph jobs.
pub(crate) const JOB_PROGRESS_PHASES: &[&str] = &[
    "queued",
    "starting",
    "reading_catalog",
    "scanning_source",
    "building_projection",
    "persisting",
    "completed",
    "failed",
];
/// Failure statuses used by graph and projection validation surfaces.
pub(crate) const FAILURE_STATUSES: &[&str] = &["missing", "stale", "invalid", "corrupt", "blocked"];

/// Default interval, in seconds, for future durable scheduler wakeups.
pub(crate) const DEFAULT_SCHEDULER_WAKE_INTERVAL_SECS: i32 = 60;
/// Default number of durable jobs one scheduler run may claim.
pub(crate) const DEFAULT_SCHEDULER_BATCH_SIZE: i32 = 64;
/// Default maximum retry attempts for idempotent graph jobs.
pub(crate) const DEFAULT_JOB_MAX_ATTEMPTS: i32 = 3;
/// Default graph count quota per owner before explicit quota configuration.
pub(crate) const DEFAULT_OWNER_GRAPH_QUOTA: i32 = 128;
/// Default graph count quota per tenant before explicit quota configuration.
pub(crate) const DEFAULT_TENANT_GRAPH_QUOTA: i32 = 512;
/// Default loaded graph slots per backend before explicit residency tuning.
pub(crate) const DEFAULT_BACKEND_LOADED_GRAPH_LIMIT: i32 = 1;

/// Internal graph identifier passed through catalog and runtime APIs.
///
/// The SQL catalog representation is `uuid`. Rust stores the canonical UUID
/// text to avoid adding a dependency before the catalog exists; callers must
/// construct values through [`GraphId::parse`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct GraphId(String);

impl GraphId {
    /// Parses and validates a canonical UUID string.
    ///
    /// # Errors
    ///
    /// Returns [`GraphIdentityError::InvalidUuid`] when the value is not in
    /// canonical PostgreSQL UUID text form.
    pub(crate) fn parse(value: &str) -> Result<Self, GraphIdentityError> {
        if is_canonical_uuid(value) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(GraphIdentityError::InvalidUuid)
        }
    }

    /// Returns the canonical UUID text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Graph identity validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphIdentityError {
    /// The supplied id was not canonical UUID text.
    InvalidUuid,
}

impl fmt::Display for GraphIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphIdentityError::InvalidUuid => f.write_str("graph id must be canonical UUID text"),
        }
    }
}

impl std::error::Error for GraphIdentityError {}

/// Returns whether `value` is one of the supported graph kinds.
pub(crate) fn is_graph_kind(value: &str) -> bool {
    contains_policy_value(GRAPH_KINDS, value)
}

/// Returns whether `value` is one of the supported residency policies.
pub(crate) fn is_residency_policy(value: &str) -> bool {
    contains_policy_value(RESIDENCY_POLICIES, value)
}

/// Returns whether `value` is one of the supported materialization policies.
pub(crate) fn is_materialization_policy(value: &str) -> bool {
    contains_policy_value(MATERIALIZATION_POLICIES, value)
}

/// Returns whether `value` is one of the supported projection modes.
pub(crate) fn is_projection_mode(value: &str) -> bool {
    contains_policy_value(PROJECTION_MODES, value)
}

fn contains_policy_value(allowed: &[&str], value: &str) -> bool {
    allowed.iter().any(|allowed| *allowed == value)
}

fn is_canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].into_iter().all(|idx| bytes[idx] == b'-')
        && bytes
            .iter()
            .enumerate()
            .filter(|(idx, _)| !matches!(*idx, 8 | 13 | 18 | 23))
            .all(|(_, byte)| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_graph_identity_is_canonical_uuid() {
        let default_graph_id = GraphId::parse(DEFAULT_GRAPH_ID_TEXT).expect("default id is valid");
        assert_eq!(default_graph_id.as_str(), DEFAULT_GRAPH_ID_TEXT);
        assert_eq!(DEFAULT_GRAPH_NAME, "default");
        assert_eq!(DEFAULT_GRAPH_NAMESPACE, "public");
    }

    #[test]
    fn policy_vocabularies_accept_only_known_values() {
        assert!(is_graph_kind("tenant"));
        assert!(is_residency_policy("hot"));
        assert!(is_materialization_policy("dedicated"));
        assert!(is_projection_mode("mutable_overlay"));

        assert!(!is_graph_kind("team"));
        assert!(!is_residency_policy("always_loaded"));
        assert!(!is_materialization_policy("physical"));
        assert!(!is_projection_mode("mutable"));
    }

    #[test]
    fn graph_id_parser_rejects_non_canonical_values() {
        assert_eq!(
            GraphId::parse("not-a-uuid"),
            Err(GraphIdentityError::InvalidUuid)
        );
        assert_eq!(
            GraphId::parse("00000000-0000-0000-0000-00000000000A"),
            Err(GraphIdentityError::InvalidUuid)
        );
        assert_eq!(
            GraphId::parse("00000000-0000-0000-0000-000000000001"),
            Ok(GraphId("00000000-0000-0000-0000-000000000001".to_string()))
        );
    }

    #[test]
    fn job_scheduler_and_quota_defaults_are_single_sourced() {
        assert_eq!(JOB_STATUSES, ["queued", "running", "completed", "failed"]);
        assert!(JOB_PROGRESS_PHASES.contains(&"reading_catalog"));
        assert!(FAILURE_STATUSES.contains(&"corrupt"));
        assert_eq!(DEFAULT_SCHEDULER_WAKE_INTERVAL_SECS, 60);
        assert_eq!(DEFAULT_SCHEDULER_BATCH_SIZE, 64);
        assert_eq!(DEFAULT_JOB_MAX_ATTEMPTS, 3);
        assert_eq!(DEFAULT_OWNER_GRAPH_QUOTA, 128);
        assert_eq!(DEFAULT_TENANT_GRAPH_QUOTA, 512);
        assert_eq!(DEFAULT_BACKEND_LOADED_GRAPH_LIMIT, 1);
    }
}
