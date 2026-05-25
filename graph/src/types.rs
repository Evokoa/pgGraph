//! # Types — Newtypes for type safety
//!
//! Newtypes prevent parameter mixups between `u32` values that represent
//! different concepts (table OIDs vs node indices vs edge counts).
//!
//! See: `docs/contributor_guide/engine-internals.mdx`

use pgrx::prelude::TimestampWithTimeZone;
use std::fmt;

/// PostgreSQL table OID. Wraps the raw `u32` OID from `pg_class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TableOid(pub u32);

impl fmt::Display for TableOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TableOid({})", self.0)
    }
}

/// Node index into the SoA arrays. Range: `0..node_count`.
#[cfg(any(test, feature = "development"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeIdx(pub u32);

#[cfg(any(test, feature = "development"))]
impl fmt::Display for NodeIdx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeIdx({})", self.0)
    }
}

/// Edge type ID. Range: `1..=254`. 0 = untyped, 255 = reserved sentinel.
#[cfg(any(test, feature = "development"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeTypeId(pub u8);

#[cfg(any(test, feature = "development"))]
impl EdgeTypeId {
    /// Reserved: untyped/null edge.
    pub const UNTYPED: Self = Self(0);
    /// Reserved: internal sentinel (never used in user-facing edges).
    pub const SENTINEL: Self = Self(255);
    /// Maximum number of user-defined edge types.
    pub const MAX_USER_TYPES: u8 = 254;
}

#[cfg(any(test, feature = "development"))]
impl fmt::Display for EdgeTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EdgeTypeId({})", self.0)
    }
}

/// Result of a single node discovered during BFS traversal.
#[derive(Debug, Clone)]
pub struct TraversalResult {
    pub node_table: TableOid,
    pub node_id: String,
    pub depth: i32,
    pub path: Vec<PathCoordinate>,
    pub edge_path: Vec<String>,
}

/// A node coordinate in a traversal path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathCoordinate {
    pub table_oid: TableOid,
    pub node_id: String,
}

/// Traversal algorithm selected by the SQL `strategy` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalStrategy {
    Bfs,
    Dfs,
}

impl TraversalStrategy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bfs" => Some(Self::Bfs),
            "dfs" => Some(Self::Dfs),
            _ => None,
        }
    }
}

/// Direction selected by the SQL `direction` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalDirection {
    Any,
    Out,
    In,
}

impl TraversalDirection {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "any" => Some(Self::Any),
            "out" => Some(Self::Out),
            "in" => Some(Self::In),
            _ => None,
        }
    }
}

/// Node visit uniqueness selected by the SQL `uniqueness` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalUniqueness {
    NodeGlobal,
    NodePerRoot,
}

impl TraversalUniqueness {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "node_global" => Some(Self::NodeGlobal),
            "node_per_root" => Some(Self::NodePerRoot),
            _ => None,
        }
    }
}

/// Result of a single step in a shortest path.
#[derive(Debug, Clone)]
pub struct PathStep {
    pub step: i32,
    pub node_table: TableOid,
    pub node_id: String,
    pub edge_label: Option<String>,
}

/// Result of a single step in a weighted shortest path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeightedPathStep {
    pub step: i32,
    pub node_table: TableOid,
    pub node_id: String,
    pub edge_label: Option<String>,
    pub edge_weight: Option<u32>,
    pub step_cost: u64,
    pub total_cost: u64,
}

/// Search matching strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Contains,
    Exact,
    Prefix,
    Token,
}

impl SearchMode {
    /// Parse a SQL-facing search mode.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "contains" => Some(Self::Contains),
            "exact" => Some(Self::Exact),
            "prefix" => Some(Self::Prefix),
            "token" => Some(Self::Token),
            _ => None,
        }
    }

    /// Stable match label returned to SQL callers.
    pub fn as_match_type(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::Token => "token",
        }
    }
}

/// Build result summary returned by `graph.build()`.
#[cfg(feature = "development")]
#[derive(Debug, Clone)]
pub struct BuildResult {
    pub nodes_loaded: i64,
    pub edges_loaded: i64,
    pub build_time_ms: f64,
    pub memory_used_mb: f64,
    pub sync_mode: String,
}

/// Engine status returned by `graph.status()`.
#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub node_count: i32,
    pub edge_count: i32,
    pub memory_used_mb: f64,
    pub memory_limit_mb: i32,
    pub sync_mode: String,
    pub sync_status: String,
    pub last_build: Option<TimestampWithTimeZone>,
    pub last_vacuum: Option<TimestampWithTimeZone>,
    pub edge_types: Vec<String>,
    pub edge_buffer_used: i32,
    pub has_unidirectional_edges: bool,
    pub applied_sync_id: i64,
    pub pending_sync_rows: i64,
    pub sync_lag: i64,
    pub needs_vacuum: bool,
    pub needs_rebuild: bool,
    pub schema_state: String,
    pub invalid_reason: Option<String>,
    pub disabled_trigger_count: i32,
    pub read_only: bool,
    pub read_only_reason: Option<String>,
}

/// Edge type filter for traversal.
#[derive(Debug, Clone)]
pub enum EdgeTypeFilter {
    /// Traverse every registered edge type.
    All,
    /// Traverse only the listed edge type identifiers.
    Only(std::collections::HashSet<u8>),
    /// Traverse no edges because the caller requested labels that do not exist.
    NoneMatched,
}

/// Filter operation used by traversal filter pushdown.
#[derive(Debug, Clone)]
pub enum FilterOp {
    /// Unsigned numeric column is greater than the threshold.
    Gt(usize, u32),
    /// Unsigned numeric column is greater than or equal to the threshold.
    Gte(usize, u32),
    /// Unsigned numeric column is less than the threshold.
    Lt(usize, u32),
    /// Unsigned numeric column is less than or equal to the threshold.
    Lte(usize, u32),
    /// Unsigned numeric column equals the value.
    Eq(usize, u32),
    /// Unsigned numeric column does not equal the value.
    Neq(usize, u32),
    /// Unsigned numeric column is within the inclusive range.
    Between(usize, u32, u32),
    /// Unsigned numeric column is one of the listed values.
    In(usize, Vec<u32>),
    /// Unsigned numeric column is not one of the listed values.
    NotIn(usize, Vec<u32>),
    /// Signed numeric column equals the value.
    EqI64(usize, i64),
    /// Signed numeric column does not equal the value.
    NeqI64(usize, i64),
    /// Signed numeric column is greater than the threshold.
    GtI64(usize, i64),
    /// Signed numeric column is greater than or equal to the threshold.
    GteI64(usize, i64),
    /// Signed numeric column is less than the threshold.
    LtI64(usize, i64),
    /// Signed numeric column is less than or equal to the threshold.
    LteI64(usize, i64),
    /// Signed numeric column is within the inclusive range.
    BetweenI64(usize, i64, i64),
    /// Signed numeric/date/timestamptz column is one of the listed values.
    InI64(usize, Vec<i64>),
    /// Signed numeric/date/timestamptz column is not one of the listed values.
    NotInI64(usize, Vec<i64>),
    /// Boolean column equals the value.
    EqBool(usize, bool),
    /// Boolean column does not equal the value.
    NeqBool(usize, bool),
    /// Boolean column is one of the listed values.
    InBool(usize, Vec<bool>),
    /// Boolean column is not one of the listed values.
    NotInBool(usize, Vec<bool>),
    /// Dictionary-encoded text/date/timestamptz token equals the value.
    EqToken(usize, u32),
    /// Dictionary-encoded text/date/timestamptz token does not equal the value.
    NeqToken(usize, u32),
    /// Dictionary-encoded text token is one of the listed values.
    InToken(usize, Vec<u32>),
    /// Dictionary-encoded text token is not one of the listed values.
    NotInToken(usize, Vec<u32>),
    /// UUID column equals the 128-bit UUID value.
    EqUuid(usize, u128),
    /// UUID column does not equal the 128-bit UUID value.
    NeqUuid(usize, u128),
    /// UUID column is one of the listed values.
    InUuid(usize, Vec<u128>),
    /// UUID column is not one of the listed values.
    NotInUuid(usize, Vec<u128>),
    /// Column value is SQL NULL.
    IsNull(usize),
    /// Column value is not SQL NULL.
    IsNotNull(usize),
}

/// Legacy unsigned numeric filter operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsignedFilterOp {
    /// Unsigned numeric column is greater than the threshold.
    Gt(usize, u32),
    /// Unsigned numeric column is greater than or equal to the threshold.
    Gte(usize, u32),
    /// Unsigned numeric column is less than the threshold.
    Lt(usize, u32),
    /// Unsigned numeric column is less than or equal to the threshold.
    Lte(usize, u32),
    /// Unsigned numeric column equals the value.
    Eq(usize, u32),
    /// Unsigned numeric column does not equal the value.
    Neq(usize, u32),
    /// Unsigned numeric column is within the inclusive range.
    Between(usize, u32, u32),
}

impl UnsignedFilterOp {
    /// Evaluate this unsigned numeric filter against a value.
    #[inline]
    pub fn check(&self, value: u32) -> bool {
        match self {
            UnsignedFilterOp::Gt(_, threshold) => value > *threshold,
            UnsignedFilterOp::Gte(_, threshold) => value >= *threshold,
            UnsignedFilterOp::Lt(_, threshold) => value < *threshold,
            UnsignedFilterOp::Lte(_, threshold) => value <= *threshold,
            UnsignedFilterOp::Eq(_, threshold) => value == *threshold,
            UnsignedFilterOp::Neq(_, threshold) => value != *threshold,
            UnsignedFilterOp::Between(_, lo, hi) => value >= *lo && value <= *hi,
        }
    }

    /// Get the column index this unsigned filter operates on.
    #[inline]
    pub fn column_idx(&self) -> usize {
        match self {
            UnsignedFilterOp::Gt(idx, _)
            | UnsignedFilterOp::Gte(idx, _)
            | UnsignedFilterOp::Lt(idx, _)
            | UnsignedFilterOp::Lte(idx, _)
            | UnsignedFilterOp::Eq(idx, _)
            | UnsignedFilterOp::Neq(idx, _)
            | UnsignedFilterOp::Between(idx, _, _) => *idx,
        }
    }
}

impl FilterOp {
    /// Convert this filter to the legacy unsigned numeric evaluator shape.
    #[inline]
    pub fn as_unsigned(&self) -> Option<UnsignedFilterOp> {
        match self {
            FilterOp::Gt(idx, threshold) => Some(UnsignedFilterOp::Gt(*idx, *threshold)),
            FilterOp::Gte(idx, threshold) => Some(UnsignedFilterOp::Gte(*idx, *threshold)),
            FilterOp::Lt(idx, threshold) => Some(UnsignedFilterOp::Lt(*idx, *threshold)),
            FilterOp::Lte(idx, threshold) => Some(UnsignedFilterOp::Lte(*idx, *threshold)),
            FilterOp::Eq(idx, threshold) => Some(UnsignedFilterOp::Eq(*idx, *threshold)),
            FilterOp::Neq(idx, threshold) => Some(UnsignedFilterOp::Neq(*idx, *threshold)),
            FilterOp::Between(idx, lo, hi) => Some(UnsignedFilterOp::Between(*idx, *lo, *hi)),
            FilterOp::EqI64(_, _)
            | FilterOp::NeqI64(_, _)
            | FilterOp::GtI64(_, _)
            | FilterOp::GteI64(_, _)
            | FilterOp::LtI64(_, _)
            | FilterOp::LteI64(_, _)
            | FilterOp::BetweenI64(_, _, _)
            | FilterOp::In(_, _)
            | FilterOp::NotIn(_, _)
            | FilterOp::InI64(_, _)
            | FilterOp::NotInI64(_, _)
            | FilterOp::EqBool(_, _)
            | FilterOp::NeqBool(_, _)
            | FilterOp::InBool(_, _)
            | FilterOp::NotInBool(_, _)
            | FilterOp::EqToken(_, _)
            | FilterOp::NeqToken(_, _)
            | FilterOp::InToken(_, _)
            | FilterOp::NotInToken(_, _)
            | FilterOp::EqUuid(_, _)
            | FilterOp::NeqUuid(_, _)
            | FilterOp::InUuid(_, _)
            | FilterOp::NotInUuid(_, _)
            | FilterOp::IsNull(_)
            | FilterOp::IsNotNull(_) => None,
        }
    }

    /// Get the column index this filter operates on.
    #[inline]
    pub fn column_idx(&self) -> usize {
        match self {
            FilterOp::Gt(idx, _)
            | FilterOp::Gte(idx, _)
            | FilterOp::Lt(idx, _)
            | FilterOp::Lte(idx, _)
            | FilterOp::Eq(idx, _)
            | FilterOp::Neq(idx, _)
            | FilterOp::Between(idx, _, _)
            | FilterOp::In(idx, _)
            | FilterOp::NotIn(idx, _)
            | FilterOp::EqI64(idx, _)
            | FilterOp::NeqI64(idx, _)
            | FilterOp::GtI64(idx, _)
            | FilterOp::GteI64(idx, _)
            | FilterOp::LtI64(idx, _)
            | FilterOp::LteI64(idx, _)
            | FilterOp::BetweenI64(idx, _, _)
            | FilterOp::InI64(idx, _)
            | FilterOp::NotInI64(idx, _)
            | FilterOp::EqBool(idx, _)
            | FilterOp::NeqBool(idx, _)
            | FilterOp::InBool(idx, _)
            | FilterOp::NotInBool(idx, _)
            | FilterOp::EqToken(idx, _)
            | FilterOp::NeqToken(idx, _)
            | FilterOp::InToken(idx, _)
            | FilterOp::NotInToken(idx, _)
            | FilterOp::EqUuid(idx, _)
            | FilterOp::NeqUuid(idx, _)
            | FilterOp::InUuid(idx, _)
            | FilterOp::NotInUuid(idx, _)
            | FilterOp::IsNull(idx)
            | FilterOp::IsNotNull(idx) => *idx,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Covers SQL-facing domain types and parser helpers so filter and search
    //! mode semantics stay stable across API calls.

    use super::*;

    // Legacy unsigned filter evaluation boundary behavior.

    #[test]
    fn filter_gt_boundary() {
        let op = UnsignedFilterOp::Gt(0, 10);
        assert!(!op.check(9));
        assert!(!op.check(10));
        assert!(op.check(11));
    }

    #[test]
    fn filter_gt_zero_threshold() {
        let op = UnsignedFilterOp::Gt(0, 0);
        assert!(!op.check(0));
        assert!(op.check(1));
    }

    #[test]
    fn filter_gt_u32_max_threshold() {
        let op = UnsignedFilterOp::Gt(0, u32::MAX);
        assert!(!op.check(u32::MAX));
        assert!(!op.check(0));
    }

    #[test]
    fn filter_gte_boundary() {
        let op = UnsignedFilterOp::Gte(0, 10);
        assert!(!op.check(9));
        assert!(op.check(10));
        assert!(op.check(11));
    }

    #[test]
    fn filter_gte_zero() {
        // >= 0 is always true for u32
        let op = UnsignedFilterOp::Gte(0, 0);
        assert!(op.check(0));
        assert!(op.check(u32::MAX));
    }

    #[test]
    fn filter_lt_boundary() {
        let op = UnsignedFilterOp::Lt(0, 10);
        assert!(op.check(9));
        assert!(!op.check(10));
        assert!(!op.check(11));
    }

    #[test]
    fn filter_lt_zero_threshold() {
        // < 0 is always false for u32
        let op = UnsignedFilterOp::Lt(0, 0);
        assert!(!op.check(0));
        assert!(!op.check(1));
    }

    #[test]
    fn filter_lte_boundary() {
        let op = UnsignedFilterOp::Lte(0, 10);
        assert!(op.check(9));
        assert!(op.check(10));
        assert!(!op.check(11));
    }

    #[test]
    fn filter_eq_exact_match() {
        let op = UnsignedFilterOp::Eq(0, 42);
        assert!(!op.check(41));
        assert!(op.check(42));
        assert!(!op.check(43));
    }

    #[test]
    fn filter_neq_exact_mismatch() {
        let op = UnsignedFilterOp::Neq(0, 42);
        assert!(op.check(41));
        assert!(!op.check(42));
        assert!(op.check(43));
    }

    #[test]
    fn filter_between_inclusive_boundaries() {
        let op = UnsignedFilterOp::Between(0, 10, 20);
        assert!(!op.check(9));
        assert!(op.check(10)); // inclusive lower
        assert!(op.check(15));
        assert!(op.check(20)); // inclusive upper
        assert!(!op.check(21));
    }

    #[test]
    fn filter_between_single_value_range() {
        let op = UnsignedFilterOp::Between(0, 5, 5);
        assert!(!op.check(4));
        assert!(op.check(5));
        assert!(!op.check(6));
    }

    #[test]
    fn filter_between_full_u32_range() {
        let op = UnsignedFilterOp::Between(0, 0, u32::MAX);
        assert!(op.check(0));
        assert!(op.check(u32::MAX));
        assert!(op.check(u32::MAX / 2));
    }

    #[test]
    fn typed_filter_ops_do_not_convert_to_unsigned_evaluators() {
        assert_eq!(
            FilterOp::Gt(2, 9).as_unsigned(),
            Some(UnsignedFilterOp::Gt(2, 9))
        );

        for op in [
            FilterOp::EqI64(2, -1),
            FilterOp::EqBool(2, true),
            FilterOp::EqToken(2, 7),
            FilterOp::EqUuid(2, 7),
            FilterOp::IsNull(2),
            FilterOp::IsNotNull(2),
        ] {
            assert_eq!(op.as_unsigned(), None, "{op:?}");
        }
    }

    // ─── UnsignedFilterOp::column_idx() ───

    #[test]
    fn unsigned_column_idx_returns_correct_index_for_all_variants() {
        let cases: Vec<UnsignedFilterOp> = vec![
            UnsignedFilterOp::Gt(3, 0),
            UnsignedFilterOp::Gte(3, 0),
            UnsignedFilterOp::Lt(3, 0),
            UnsignedFilterOp::Lte(3, 0),
            UnsignedFilterOp::Eq(3, 0),
            UnsignedFilterOp::Neq(3, 0),
            UnsignedFilterOp::Between(3, 0, 0),
        ];
        for op in cases {
            assert_eq!(op.column_idx(), 3, "column_idx wrong for {:?}", op);
        }
    }

    // ─── EdgeTypeId constants ───

    #[test]
    fn edge_type_id_constants() {
        assert_eq!(EdgeTypeId::UNTYPED.0, 0);
        assert_eq!(EdgeTypeId::SENTINEL.0, 255);
        assert_eq!(EdgeTypeId::MAX_USER_TYPES, 254);
    }

    #[test]
    fn edge_type_id_untyped_and_sentinel_are_distinct() {
        assert_ne!(EdgeTypeId::UNTYPED, EdgeTypeId::SENTINEL);
    }

    // ─── SearchMode parsing ───

    #[test]
    fn search_mode_parse_accepts_supported_modes_case_insensitively() {
        assert_eq!(SearchMode::parse("contains"), Some(SearchMode::Contains));
        assert_eq!(SearchMode::parse("EXACT"), Some(SearchMode::Exact));
        assert_eq!(SearchMode::parse(" Prefix "), Some(SearchMode::Prefix));
        assert_eq!(SearchMode::parse("token"), Some(SearchMode::Token));
    }

    #[test]
    fn search_mode_parse_rejects_unknown_modes() {
        assert_eq!(SearchMode::parse("fuzzy"), None);
        assert_eq!(SearchMode::parse(""), None);
    }

    #[test]
    fn traversal_option_parsers_accept_supported_modes_case_insensitively() {
        assert_eq!(
            TraversalDirection::parse(" OUT "),
            Some(TraversalDirection::Out)
        );
        assert_eq!(
            TraversalStrategy::parse("dfs"),
            Some(TraversalStrategy::Dfs)
        );
        assert_eq!(
            TraversalUniqueness::parse("NODE_PER_ROOT"),
            Some(TraversalUniqueness::NodePerRoot)
        );
    }

    #[test]
    fn traversal_option_parsers_reject_unknown_modes() {
        assert_eq!(TraversalDirection::parse("sideways"), None);
        assert_eq!(TraversalStrategy::parse("weighted"), None);
        assert_eq!(TraversalUniqueness::parse("node_local"), None);
    }

    // ─── Display impls ───

    #[test]
    fn table_oid_display() {
        let oid = TableOid(12345);
        assert_eq!(format!("{}", oid), "TableOid(12345)");
    }

    #[test]
    fn node_idx_display() {
        let idx = NodeIdx(42);
        assert_eq!(format!("{}", idx), "NodeIdx(42)");
    }

    #[test]
    fn edge_type_id_display() {
        let et = EdgeTypeId(7);
        assert_eq!(format!("{}", et), "EdgeTypeId(7)");
    }

    // ─── Newtype identity and ordering ───

    #[test]
    fn table_oid_eq_and_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(TableOid(1));
        set.insert(TableOid(1));
        set.insert(TableOid(2));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn node_idx_ordering() {
        assert!(NodeIdx(0) < NodeIdx(1));
        assert_eq!(NodeIdx(5), NodeIdx(5));
    }
}
