//! # FilterIndex — hybrid storage for traversal filtering
//!
//! Registered filter columns are indexed by internal `node_idx` so BFS can
//! evaluate traversal predicates without routing each neighbor back through SQL.

use crate::types::{FilterCondition, FilterOp};
use crate::{safety::GraphError, safety::GraphResult};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SPARSE_THRESHOLD_NUMERATOR: usize = 15;
const SPARSE_THRESHOLD_DENOMINATOR: usize = 100;

/// Metadata for a registered filter column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterColumnMeta {
    /// Source table OID that owns the column.
    pub table_oid: u32,
    /// Source column name.
    pub column_name: String,
    /// Encoded value domain used for hot-loop comparisons.
    pub column_type: FilterColumnType,
}

/// Supported encoded domains for traversal filter pushdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterColumnType {
    /// Integral numeric comparison domain.
    Numeric,
    /// Boolean equality domain.
    Boolean,
    /// Interned text equality domain.
    Text,
    /// Date domain encoded as days from the Unix epoch.
    Date,
    /// Timestamp-with-time-zone domain encoded as microseconds from the Unix epoch.
    Timestamptz,
    /// UUID equality domain encoded as a 128-bit integer.
    Uuid,
}

/// Value encoded for hot-loop filter comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EncodedFilterValue {
    /// Numeric, date, or timestamp value encoded as a signed integer.
    Numeric(i64),
    /// Boolean value.
    Boolean(bool),
    /// Interned text dictionary identifier.
    Text(u32),
    /// Date value encoded as days from the Unix epoch.
    Date(i64),
    /// Timestamp-with-time-zone value encoded as microseconds from the Unix epoch.
    Timestamptz(i64),
    /// UUID value encoded in canonical byte order.
    Uuid(u128),
}

/// Self-contained filter value stored in durable projection segments.
///
/// Unlike [`EncodedFilterValue::Text`], text values are stored directly rather
/// than as backend-local dictionary identifiers. `Null` is distinct from a
/// segment-row tombstone so an update to SQL `NULL` survives restart.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PersistedFilterValue {
    /// SQL `NULL` for a present source row.
    Null,
    /// Signed integral numeric value.
    Numeric(i64),
    /// Boolean value.
    Boolean(bool),
    /// Source text value, before backend-local dictionary interning.
    Text(String),
    /// Date encoded as days from the Unix epoch.
    Date(i64),
    /// Timestamp with time zone encoded as microseconds from the Unix epoch.
    Timestamptz(i64),
    /// UUID encoded in canonical byte order.
    Uuid(u128),
}

impl PersistedFilterValue {
    /// Return heap bytes owned by this value for ingest-budget accounting.
    pub(crate) fn heap_bytes(&self) -> usize {
        match self {
            Self::Text(value) => value.len(),
            Self::Null
            | Self::Numeric(_)
            | Self::Boolean(_)
            | Self::Date(_)
            | Self::Timestamptz(_)
            | Self::Uuid(_) => 0,
        }
    }
}

impl FilterColumnType {
    /// Parse a SQL-facing filter column type name.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not one of the supported filter
    /// domains: `numeric`, `boolean`, `text`, `date`, `timestamptz`, or `uuid`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "numeric" => Ok(Self::Numeric),
            "boolean" => Ok(Self::Boolean),
            "text" => Ok(Self::Text),
            "date" => Ok(Self::Date),
            "timestamptz" => Ok(Self::Timestamptz),
            "uuid" => Ok(Self::Uuid),
            other => Err(format!("unsupported filter column_type '{}'", other)),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterStorageKind {
    Dense,
    SparseBool,
    SparseLookup,
    SparseOrdered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum FilterColumnStorage {
    Dense {
        values: Vec<EncodedFilterValue>,
        present_bitmap: RoaringBitmap,
    },
    SparseBool {
        true_bitmap: RoaringBitmap,
        false_bitmap: RoaringBitmap,
        present_bitmap: RoaringBitmap,
    },
    SparseLookup {
        value_bitmaps: HashMap<EncodedFilterValue, RoaringBitmap>,
        present_bitmap: RoaringBitmap,
    },
    SparseOrdered {
        entries: Vec<(u32, EncodedFilterValue)>,
        present_bitmap: RoaringBitmap,
    },
}

/// Hybrid per-column storage for filtering during BFS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterIndex {
    /// Metadata for each registered column.
    pub columns: Vec<FilterColumnMeta>,
    storage: Vec<FilterColumnStorage>,
    text_dictionaries: Vec<HashMap<String, u32>>,
    reverse_text_dictionaries: Vec<Vec<String>>,
}

impl FilterIndex {
    /// Create an empty filter index.
    pub fn new() -> Self {
        Self {
            columns: Vec::new(),
            storage: Vec::new(),
            text_dictionaries: Vec::new(),
            reverse_text_dictionaries: Vec::new(),
        }
    }

    /// Register a new filter column. Returns the column index.
    pub fn register_column(
        &mut self,
        table_oid: u32,
        column_name: String,
        node_count: usize,
    ) -> usize {
        self.register_typed_column(
            table_oid,
            column_name,
            FilterColumnType::Numeric,
            node_count,
        )
    }

    /// Register a typed filter column and allocate per-node storage.
    ///
    /// Returns the new column index. All node slots start as SQL NULL until
    /// [`FilterIndex::set_value`] or [`FilterIndex::set_encoded_value`] writes
    /// a value.
    pub fn register_typed_column(
        &mut self,
        table_oid: u32,
        column_name: String,
        column_type: FilterColumnType,
        node_count: usize,
    ) -> usize {
        self.register_typed_column_with_populated_count(
            table_oid,
            column_name,
            column_type,
            node_count,
            node_count,
        )
    }

    /// Register a typed filter column with the build-time sparsity heuristic.
    pub fn register_typed_column_with_populated_count(
        &mut self,
        table_oid: u32,
        column_name: String,
        column_type: FilterColumnType,
        node_count: usize,
        populated_count: usize,
    ) -> usize {
        let idx = self.columns.len();
        self.columns.push(FilterColumnMeta {
            table_oid,
            column_name,
            column_type,
        });
        self.storage
            .push(new_storage(column_type, node_count, populated_count));
        self.text_dictionaries.push(HashMap::new());
        self.reverse_text_dictionaries.push(Vec::new());
        idx
    }

    /// Set the value for a specific node in a specific column.
    pub fn set_value(&mut self, column_idx: usize, node_idx: u32, value: u32) {
        self.set_encoded_value(
            column_idx,
            node_idx,
            Some(EncodedFilterValue::Numeric(value as i64)),
        );
    }

    /// Set or clear the typed value for one node in one registered column.
    ///
    /// Passing `None` marks the value as SQL NULL. Out-of-range column or node
    /// indexes are ignored so sync replay can tolerate rows that were removed
    /// by a concurrent rebuild.
    pub fn set_encoded_value(
        &mut self,
        column_idx: usize,
        node_idx: u32,
        value: Option<EncodedFilterValue>,
    ) {
        let Some(storage) = self.storage.get_mut(column_idx) else {
            return;
        };
        storage.set(node_idx, value);
    }

    /// Apply a self-contained value decoded from a durable projection segment.
    ///
    /// Text values are interned into this backend's dictionary. SQL `NULL` and
    /// tombstones clear the indexed value. A persisted type that does not match
    /// the registered column is treated as artifact corruption.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] for an unknown column identifier or
    /// a value whose tag does not match the registered filter column domain.
    pub(crate) fn apply_persisted_value(
        &mut self,
        column_idx: usize,
        node_idx: u32,
        node_count: u32,
        value: &PersistedFilterValue,
        tombstone: bool,
    ) -> GraphResult<()> {
        if node_idx >= node_count {
            return Err(GraphError::CorruptFile {
                reason: format!(
                    "projection filter node id {node_idx} is outside projected node range 0..{node_count}"
                ),
            });
        }
        let column_type = self.column_type(column_idx).ok_or_else(|| {
            GraphError::CorruptFile {
                reason: format!(
                    "projection filter column id {column_idx} is not registered in the base artifact"
                ),
            }
        })?;
        let encoded = match (column_type, value) {
            (_, PersistedFilterValue::Null) => None,
            (FilterColumnType::Numeric, PersistedFilterValue::Numeric(value)) => {
                Some(EncodedFilterValue::Numeric(*value))
            }
            (FilterColumnType::Boolean, PersistedFilterValue::Boolean(value)) => {
                Some(EncodedFilterValue::Boolean(*value))
            }
            (FilterColumnType::Text, PersistedFilterValue::Text(value)) => {
                let token = self.intern_persisted_text_value(column_idx, value)?;
                Some(EncodedFilterValue::Text(token))
            }
            (FilterColumnType::Date, PersistedFilterValue::Date(value)) => {
                Some(EncodedFilterValue::Date(*value))
            }
            (FilterColumnType::Timestamptz, PersistedFilterValue::Timestamptz(value)) => {
                Some(EncodedFilterValue::Timestamptz(*value))
            }
            (FilterColumnType::Uuid, PersistedFilterValue::Uuid(value)) => {
                Some(EncodedFilterValue::Uuid(*value))
            }
            (expected, actual) => {
                return Err(GraphError::CorruptFile {
                    reason: format!(
                        "projection filter value {actual:?} does not match registered column {column_idx} type {expected:?}"
                    ),
                });
            }
        };
        self.set_encoded_value(column_idx, node_idx, if tombstone { None } else { encoded });
        Ok(())
    }

    /// Validate the parallel arrays and text dictionaries decoded from a base
    /// graph artifact before the index can be installed.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] when column metadata, storage, or
    /// text dictionaries are structurally inconsistent with `node_count`.
    pub(crate) fn validate_persisted_layout(&self, node_count: u32) -> GraphResult<()> {
        let column_count = self.columns.len();
        if self.storage.len() != column_count
            || self.text_dictionaries.len() != column_count
            || self.reverse_text_dictionaries.len() != column_count
        {
            return Err(filter_index_corrupt(
                "filter index parallel column arrays have different lengths",
            ));
        }
        let node_count = usize::try_from(node_count)
            .map_err(|_| filter_index_corrupt("filter index node count exceeds usize"))?;
        for column_idx in 0..column_count {
            self.storage[column_idx]
                .validate_persisted_layout(self.columns[column_idx].column_type, node_count)?;
            self.validate_text_dictionary(column_idx)?;
        }
        Ok(())
    }

    fn validate_text_dictionary(&self, column_idx: usize) -> GraphResult<()> {
        let forward = &self.text_dictionaries[column_idx];
        let reverse = &self.reverse_text_dictionaries[column_idx];
        if self.columns[column_idx].column_type != FilterColumnType::Text {
            if !forward.is_empty() || !reverse.is_empty() {
                return Err(filter_index_corrupt(format!(
                    "non-text filter column {column_idx} has a text dictionary"
                )));
            }
            return Ok(());
        }
        if forward.len() != reverse.len() {
            return Err(filter_index_corrupt(format!(
                "text filter column {column_idx} dictionary directions differ in length"
            )));
        }
        for (token, value) in reverse.iter().enumerate() {
            let token = u32::try_from(token).map_err(|_| {
                filter_index_corrupt(format!(
                    "text filter column {column_idx} dictionary exceeds u32 tokens"
                ))
            })?;
            if forward.get(value) != Some(&token) {
                return Err(filter_index_corrupt(format!(
                    "text filter column {column_idx} dictionary token {token} is inconsistent"
                )));
            }
        }
        Ok(())
    }

    fn intern_persisted_text_value(&mut self, column_idx: usize, value: &str) -> GraphResult<u32> {
        let forward = self.text_dictionaries.get_mut(column_idx).ok_or_else(|| {
            filter_index_corrupt(format!(
                "text filter column {column_idx} has no forward dictionary"
            ))
        })?;
        let reverse = self
            .reverse_text_dictionaries
            .get_mut(column_idx)
            .ok_or_else(|| {
                filter_index_corrupt(format!(
                    "text filter column {column_idx} has no reverse dictionary"
                ))
            })?;
        if forward.len() != reverse.len() {
            return Err(filter_index_corrupt(format!(
                "text filter column {column_idx} dictionary directions differ in length"
            )));
        }
        if let Some(existing) = forward.get(value) {
            if reverse.get(*existing as usize).map(String::as_str) != Some(value) {
                return Err(filter_index_corrupt(format!(
                    "text filter column {column_idx} dictionary token {existing} is inconsistent"
                )));
            }
            return Ok(*existing);
        }
        let token = u32::try_from(reverse.len())
            .map_err(|_| filter_index_corrupt("text filter dictionary exceeds u32 tokens"))?;
        forward.insert(value.to_string(), token);
        reverse.push(value.to_string());
        Ok(token)
    }

    /// Get the value for a specific node in a specific column.
    #[inline(always)]
    pub fn get_value(&self, column_idx: usize, node_idx: u32) -> u32 {
        self.storage
            .get(column_idx)
            .and_then(|storage| storage.value(node_idx))
            .and_then(|value| match value {
                EncodedFilterValue::Numeric(value)
                | EncodedFilterValue::Date(value)
                | EncodedFilterValue::Timestamptz(value) => {
                    Some(value.clamp(0, u32::MAX as i64) as u32)
                }
                _ => None,
            })
            .unwrap_or(0)
    }

    /// Check a node against a single filter operation.
    #[inline(always)]
    pub fn check_filter(&self, node_idx: u32, op: &FilterOp) -> bool {
        let column_idx = op.column_idx();
        if let Some(value) = crate::projection::tx_delta::filter_value_update(column_idx, node_idx)
        {
            return self.check_filter_value(column_idx, value, op);
        }
        let Some(storage) = self.storage.get(column_idx) else {
            return matches!(op.condition(), FilterCondition::IsNull);
        };
        self.check_filter_value(column_idx, storage.value(node_idx), op)
    }

    fn check_filter_value(
        &self,
        column_idx: usize,
        value: Option<EncodedFilterValue>,
        op: &FilterOp,
    ) -> bool {
        let Some(value) = value else {
            return matches!(op.condition(), FilterCondition::IsNull);
        };
        match op.condition() {
            FilterCondition::Gt(threshold) => encoded_u32(value) > *threshold,
            FilterCondition::Gte(threshold) => encoded_u32(value) >= *threshold,
            FilterCondition::Lt(threshold) => encoded_u32(value) < *threshold,
            FilterCondition::Lte(threshold) => encoded_u32(value) <= *threshold,
            FilterCondition::Eq(threshold) => encoded_u32(value) == *threshold,
            FilterCondition::Neq(threshold) => encoded_u32(value) != *threshold,
            FilterCondition::Between(lo, hi) => {
                let value = encoded_u32(value);
                value >= *lo && value <= *hi
            }
            FilterCondition::In(expected) => expected.contains(&encoded_u32(value)),
            FilterCondition::NotIn(expected) => !expected.contains(&encoded_u32(value)),
            FilterCondition::EqI64(expected) => encoded_i64(value) == Some(*expected),
            FilterCondition::NeqI64(expected) => encoded_i64(value) != Some(*expected),
            FilterCondition::GtI64(expected) => {
                encoded_i64(value).is_some_and(|value| value > *expected)
            }
            FilterCondition::GteI64(expected) => {
                encoded_i64(value).is_some_and(|value| value >= *expected)
            }
            FilterCondition::LtI64(expected) => {
                encoded_i64(value).is_some_and(|value| value < *expected)
            }
            FilterCondition::LteI64(expected) => {
                encoded_i64(value).is_some_and(|value| value <= *expected)
            }
            FilterCondition::BetweenI64(low, high) => {
                encoded_i64(value).is_some_and(|value| value >= *low && value <= *high)
            }
            FilterCondition::InI64(expected) => {
                encoded_i64(value).is_some_and(|value| expected.contains(&value))
            }
            FilterCondition::NotInI64(expected) => {
                encoded_i64(value).is_some_and(|value| !expected.contains(&value))
            }
            FilterCondition::EqBool(expected) => {
                matches!(value, EncodedFilterValue::Boolean(actual) if actual == *expected)
            }
            FilterCondition::NeqBool(expected) => {
                matches!(value, EncodedFilterValue::Boolean(actual) if actual != *expected)
            }
            FilterCondition::InBool(expected) => {
                matches!(value, EncodedFilterValue::Boolean(actual) if expected.contains(&actual))
            }
            FilterCondition::NotInBool(expected) => {
                matches!(value, EncodedFilterValue::Boolean(actual) if !expected.contains(&actual))
            }
            FilterCondition::EqToken(expected) => {
                matches!(value, EncodedFilterValue::Text(actual) if actual == *expected)
            }
            FilterCondition::NeqToken(expected) => {
                matches!(value, EncodedFilterValue::Text(actual) if actual != *expected)
            }
            FilterCondition::InToken(expected) => {
                matches!(value, EncodedFilterValue::Text(actual) if expected.contains(&actual))
            }
            FilterCondition::NotInToken(expected) => {
                matches!(value, EncodedFilterValue::Text(actual) if !expected.contains(&actual))
            }
            FilterCondition::ContainsToken(expected) => {
                matches!(value, EncodedFilterValue::Text(actual) if self.text_value(column_idx, actual).is_some_and(|actual| actual.contains(expected)))
            }
            FilterCondition::PrefixToken(expected) => {
                matches!(value, EncodedFilterValue::Text(actual) if self.text_value(column_idx, actual).is_some_and(|actual| actual.starts_with(expected)))
            }
            FilterCondition::EqUuid(expected) => {
                matches!(value, EncodedFilterValue::Uuid(actual) if actual == *expected)
            }
            FilterCondition::NeqUuid(expected) => {
                matches!(value, EncodedFilterValue::Uuid(actual) if actual != *expected)
            }
            FilterCondition::InUuid(expected) => {
                matches!(value, EncodedFilterValue::Uuid(actual) if expected.contains(&actual))
            }
            FilterCondition::NotInUuid(expected) => {
                matches!(value, EncodedFilterValue::Uuid(actual) if !expected.contains(&actual))
            }
            FilterCondition::IsNull => false,
            FilterCondition::IsNotNull => true,
        }
    }

    /// Check a node against multiple AND'd filter operations.
    #[inline]
    pub fn check_filters(&self, node_idx: u32, ops: &[FilterOp]) -> bool {
        ops.iter().all(|op| self.check_filter(node_idx, op))
    }

    /// Return the first column with this display name.
    ///
    /// This compatibility helper is intentionally unsuitable for graph query
    /// and synchronization paths because a display name can occur on more
    /// than one relation. Use [`Self::find_column_for_table`] whenever source
    /// relation identity is available.
    pub fn find_first_column_by_name(&self, column_name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.column_name == column_name)
    }

    /// Find a registered column by its owning relation and attribute name.
    ///
    /// Graphs may register the same attribute name on multiple tables. Build
    /// and synchronization paths must use this identity-aware lookup rather
    /// than treating the first matching display name as authoritative.
    pub fn find_column_for_table(&self, table_oid: u32, column_name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| column.table_oid == table_oid && column.column_name == column_name)
    }

    /// Return the encoded domain for a registered column.
    pub fn column_type(&self, column_idx: usize) -> Option<FilterColumnType> {
        self.columns
            .get(column_idx)
            .map(|column| column.column_type)
    }

    /// Intern a text value in the dictionary for `column_idx`.
    ///
    /// The returned token is stable for the lifetime of this [`FilterIndex`].
    pub fn intern_text_value(&mut self, column_idx: usize, value: &str) -> u32 {
        if let Some(existing) = self.text_dictionaries[column_idx].get(value) {
            return *existing;
        }
        let id = self.reverse_text_dictionaries[column_idx].len() as u32;
        self.text_dictionaries[column_idx].insert(value.to_string(), id);
        self.reverse_text_dictionaries[column_idx].push(value.to_string());
        id
    }

    /// Look up an already-interned text token for `column_idx`.
    ///
    /// Returns `None` when the value has never been indexed for that column.
    pub fn lookup_text_value(&self, column_idx: usize, value: &str) -> Option<u32> {
        self.text_dictionaries
            .get(column_idx)
            .and_then(|dictionary| dictionary.get(value))
            .copied()
    }

    /// Return an interned text value by token for `column_idx`.
    pub fn text_value(&self, column_idx: usize, token: u32) -> Option<&str> {
        self.reverse_text_dictionaries
            .get(column_idx)
            .and_then(|dictionary| dictionary.get(token as usize))
            .map(String::as_str)
    }

    /// Number of registered filter columns.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_reverse_text_dictionary_for_test(&mut self) {
        self.reverse_text_dictionaries.pop();
    }

    #[cfg(test)]
    pub(crate) fn storage_kind(&self, column_idx: usize) -> Option<FilterStorageKind> {
        self.storage.get(column_idx).map(FilterColumnStorage::kind)
    }

    /// Estimate bytes owned by the heap-resident hybrid index.
    pub fn estimated_heap_bytes(&self) -> usize {
        let columns = self.columns.len() * std::mem::size_of::<FilterColumnMeta>();
        let dictionaries: usize = self
            .reverse_text_dictionaries
            .iter()
            .flatten()
            .map(|value| value.len() + std::mem::size_of::<String>())
            .sum();
        columns.saturating_add(dictionaries).saturating_add(
            self.storage
                .iter()
                .map(FilterColumnStorage::estimated_bytes)
                .sum(),
        )
    }
}

fn new_storage(
    column_type: FilterColumnType,
    node_count: usize,
    populated_count: usize,
) -> FilterColumnStorage {
    if is_sparse(populated_count, node_count) {
        return match column_type {
            FilterColumnType::Boolean => FilterColumnStorage::SparseBool {
                true_bitmap: RoaringBitmap::new(),
                false_bitmap: RoaringBitmap::new(),
                present_bitmap: RoaringBitmap::new(),
            },
            FilterColumnType::Text | FilterColumnType::Uuid => FilterColumnStorage::SparseLookup {
                value_bitmaps: HashMap::new(),
                present_bitmap: RoaringBitmap::new(),
            },
            FilterColumnType::Numeric | FilterColumnType::Date | FilterColumnType::Timestamptz => {
                FilterColumnStorage::SparseOrdered {
                    entries: Vec::with_capacity(populated_count),
                    present_bitmap: RoaringBitmap::new(),
                }
            }
        };
    }

    FilterColumnStorage::Dense {
        values: vec![default_encoded_value(column_type); node_count],
        present_bitmap: RoaringBitmap::new(),
    }
}

fn is_sparse(populated_count: usize, node_count: usize) -> bool {
    node_count != 0
        && populated_count.saturating_mul(SPARSE_THRESHOLD_DENOMINATOR)
            < node_count.saturating_mul(SPARSE_THRESHOLD_NUMERATOR)
}

fn default_encoded_value(column_type: FilterColumnType) -> EncodedFilterValue {
    match column_type {
        FilterColumnType::Numeric => EncodedFilterValue::Numeric(0),
        FilterColumnType::Boolean => EncodedFilterValue::Boolean(false),
        FilterColumnType::Text => EncodedFilterValue::Text(0),
        FilterColumnType::Date => EncodedFilterValue::Date(0),
        FilterColumnType::Timestamptz => EncodedFilterValue::Timestamptz(0),
        FilterColumnType::Uuid => EncodedFilterValue::Uuid(0),
    }
}

fn filter_index_corrupt(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: reason.into(),
    }
}

fn encoded_value_matches(column_type: FilterColumnType, value: EncodedFilterValue) -> bool {
    matches!(
        (column_type, value),
        (FilterColumnType::Numeric, EncodedFilterValue::Numeric(_))
            | (FilterColumnType::Boolean, EncodedFilterValue::Boolean(_))
            | (FilterColumnType::Text, EncodedFilterValue::Text(_))
            | (FilterColumnType::Date, EncodedFilterValue::Date(_))
            | (
                FilterColumnType::Timestamptz,
                EncodedFilterValue::Timestamptz(_)
            )
            | (FilterColumnType::Uuid, EncodedFilterValue::Uuid(_))
    )
}

fn encoded_u32(value: EncodedFilterValue) -> u32 {
    encoded_i64(value)
        .map(|value| value.clamp(0, u32::MAX as i64) as u32)
        .unwrap_or(0)
}

fn encoded_i64(value: EncodedFilterValue) -> Option<i64> {
    match value {
        EncodedFilterValue::Numeric(value)
        | EncodedFilterValue::Date(value)
        | EncodedFilterValue::Timestamptz(value) => Some(value),
        _ => None,
    }
}

impl FilterColumnStorage {
    fn validate_persisted_layout(
        &self,
        column_type: FilterColumnType,
        node_count: usize,
    ) -> GraphResult<()> {
        let node_in_range = |node_idx: u32| (node_idx as usize) < node_count;
        match self {
            Self::Dense {
                values,
                present_bitmap,
            } => {
                if values.len() != node_count
                    || present_bitmap
                        .iter()
                        .any(|node_idx| !node_in_range(node_idx))
                    || values
                        .iter()
                        .any(|value| !encoded_value_matches(column_type, *value))
                {
                    return Err(filter_index_corrupt(
                        "dense filter storage does not match its column domain or node count",
                    ));
                }
            }
            Self::SparseBool {
                true_bitmap,
                false_bitmap,
                present_bitmap,
            } => {
                if column_type != FilterColumnType::Boolean
                    || present_bitmap
                        .iter()
                        .any(|node_idx| !node_in_range(node_idx))
                    || true_bitmap.iter().any(|node_idx| {
                        !node_in_range(node_idx) || !present_bitmap.contains(node_idx)
                    })
                    || false_bitmap.iter().any(|node_idx| {
                        !node_in_range(node_idx) || !present_bitmap.contains(node_idx)
                    })
                    || !true_bitmap.is_disjoint(false_bitmap)
                    || present_bitmap.len() != true_bitmap.len() + false_bitmap.len()
                {
                    return Err(filter_index_corrupt(
                        "sparse boolean storage is inconsistent with its column domain",
                    ));
                }
            }
            Self::SparseLookup {
                value_bitmaps,
                present_bitmap,
            } => {
                if !matches!(column_type, FilterColumnType::Text | FilterColumnType::Uuid)
                    || present_bitmap
                        .iter()
                        .any(|node_idx| !node_in_range(node_idx))
                {
                    return Err(filter_index_corrupt(
                        "sparse lookup storage is inconsistent with its column domain",
                    ));
                }
                let mut seen = RoaringBitmap::new();
                for (value, bitmap) in value_bitmaps {
                    if !encoded_value_matches(column_type, *value)
                        || !seen.is_disjoint(bitmap)
                        || bitmap.iter().any(|node_idx| {
                            !node_in_range(node_idx) || !present_bitmap.contains(node_idx)
                        })
                    {
                        return Err(filter_index_corrupt(
                            "sparse lookup storage is inconsistent with its column domain",
                        ));
                    }
                    seen |= bitmap;
                }
                if &seen != present_bitmap {
                    return Err(filter_index_corrupt(
                        "sparse lookup storage does not cover its present nodes exactly",
                    ));
                }
            }
            Self::SparseOrdered {
                entries,
                present_bitmap,
            } => {
                if !matches!(
                    column_type,
                    FilterColumnType::Numeric
                        | FilterColumnType::Date
                        | FilterColumnType::Timestamptz
                ) || present_bitmap
                    .iter()
                    .any(|node_idx| !node_in_range(node_idx))
                    || entries.iter().any(|(node_idx, value)| {
                        !node_in_range(*node_idx)
                            || !present_bitmap.contains(*node_idx)
                            || !encoded_value_matches(column_type, *value)
                    })
                    || entries.windows(2).any(|pair| pair[0].0 >= pair[1].0)
                    || entries.len() as u64 != present_bitmap.len()
                {
                    return Err(filter_index_corrupt(
                        "sparse ordered storage is inconsistent with its column domain",
                    ));
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn kind(&self) -> FilterStorageKind {
        match self {
            Self::Dense { .. } => FilterStorageKind::Dense,
            Self::SparseBool { .. } => FilterStorageKind::SparseBool,
            Self::SparseLookup { .. } => FilterStorageKind::SparseLookup,
            Self::SparseOrdered { .. } => FilterStorageKind::SparseOrdered,
        }
    }

    fn value(&self, node_idx: u32) -> Option<EncodedFilterValue> {
        match self {
            Self::Dense {
                values,
                present_bitmap,
            } => present_bitmap
                .contains(node_idx)
                .then(|| values.get(node_idx as usize).copied())
                .flatten(),
            Self::SparseBool {
                true_bitmap,
                false_bitmap,
                present_bitmap,
            } => {
                if !present_bitmap.contains(node_idx) {
                    None
                } else {
                    Some(EncodedFilterValue::Boolean(
                        true_bitmap.contains(node_idx) && !false_bitmap.contains(node_idx),
                    ))
                }
            }
            Self::SparseLookup {
                value_bitmaps,
                present_bitmap,
            } => {
                if !present_bitmap.contains(node_idx) {
                    return None;
                }
                value_bitmaps
                    .iter()
                    .find_map(|(value, bitmap)| bitmap.contains(node_idx).then_some(*value))
            }
            Self::SparseOrdered {
                entries,
                present_bitmap,
            } => {
                if !present_bitmap.contains(node_idx) {
                    return None;
                }
                entries
                    .binary_search_by_key(&node_idx, |(idx, _)| *idx)
                    .ok()
                    .map(|idx| entries[idx].1)
            }
        }
    }

    fn set(&mut self, node_idx: u32, value: Option<EncodedFilterValue>) {
        match self {
            Self::Dense {
                values,
                present_bitmap,
            } => {
                let idx = node_idx as usize;
                if idx >= values.len() {
                    return;
                }
                match value {
                    Some(value) => {
                        values[idx] = value;
                        present_bitmap.insert(node_idx);
                    }
                    None => {
                        present_bitmap.remove(node_idx);
                    }
                }
            }
            Self::SparseBool {
                true_bitmap,
                false_bitmap,
                present_bitmap,
            } => {
                true_bitmap.remove(node_idx);
                false_bitmap.remove(node_idx);
                match value {
                    Some(EncodedFilterValue::Boolean(true)) => {
                        true_bitmap.insert(node_idx);
                        present_bitmap.insert(node_idx);
                    }
                    Some(EncodedFilterValue::Boolean(false)) => {
                        false_bitmap.insert(node_idx);
                        present_bitmap.insert(node_idx);
                    }
                    Some(_) => {
                        present_bitmap.remove(node_idx);
                    }
                    None => {
                        present_bitmap.remove(node_idx);
                    }
                }
            }
            Self::SparseLookup {
                value_bitmaps,
                present_bitmap,
            } => {
                for bitmap in value_bitmaps.values_mut() {
                    bitmap.remove(node_idx);
                }
                match value {
                    Some(value @ (EncodedFilterValue::Text(_) | EncodedFilterValue::Uuid(_))) => {
                        value_bitmaps.entry(value).or_default().insert(node_idx);
                        present_bitmap.insert(node_idx);
                    }
                    Some(_) => {
                        present_bitmap.remove(node_idx);
                    }
                    None => {
                        present_bitmap.remove(node_idx);
                    }
                }
            }
            Self::SparseOrdered {
                entries,
                present_bitmap,
            } => match entries.binary_search_by_key(&node_idx, |(idx, _)| *idx) {
                Ok(idx) => match value {
                    Some(value) => {
                        entries[idx] = (node_idx, value);
                        present_bitmap.insert(node_idx);
                    }
                    None => {
                        entries.remove(idx);
                        present_bitmap.remove(node_idx);
                    }
                },
                Err(idx) => {
                    if let Some(value) = value {
                        entries.insert(idx, (node_idx, value));
                        present_bitmap.insert(node_idx);
                    }
                }
            },
        }
    }

    fn estimated_bytes(&self) -> usize {
        match self {
            Self::Dense {
                values,
                present_bitmap,
            } => values
                .len()
                .saturating_mul(std::mem::size_of::<EncodedFilterValue>())
                .saturating_add(serialized_bitmap_size(present_bitmap)),
            Self::SparseBool {
                true_bitmap,
                false_bitmap,
                present_bitmap,
            } => serialized_bitmap_size(true_bitmap)
                .saturating_add(serialized_bitmap_size(false_bitmap))
                .saturating_add(serialized_bitmap_size(present_bitmap)),
            Self::SparseLookup {
                value_bitmaps,
                present_bitmap,
            } => serialized_bitmap_size(present_bitmap).saturating_add(
                value_bitmaps
                    .values()
                    .map(|bitmap| {
                        std::mem::size_of::<EncodedFilterValue>()
                            .saturating_add(serialized_bitmap_size(bitmap))
                    })
                    .sum(),
            ),
            Self::SparseOrdered {
                entries,
                present_bitmap,
            } => entries
                .len()
                .saturating_mul(std::mem::size_of::<(u32, EncodedFilterValue)>())
                .saturating_add(serialized_bitmap_size(present_bitmap)),
        }
    }
}

fn serialized_bitmap_size(bitmap: &RoaringBitmap) -> usize {
    bincode::serde::encode_to_vec(bitmap, bincode::config::standard())
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

impl Default for FilterIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! Covers filter column registration and predicate evaluation boundaries so
    //! traversal filters preserve their typed comparison semantics.

    use crate::types::UnsignedFilterOp;

    use super::*;

    #[test]
    fn register_and_set_values() {
        let mut fi = FilterIndex::new();
        let col = fi.register_column(100, "amount".to_string(), 5);
        fi.set_value(col, 0, 5000);
        fi.set_value(col, 2, 15000);

        assert_eq!(fi.get_value(col, 0), 5000);
        assert_eq!(fi.get_value(col, 1), 0); // default
        assert_eq!(fi.get_value(col, 2), 15000);
    }

    #[test]
    fn u32_max_boundary_values() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "score".to_string(), 2);
        fi.set_value(0, 0, u32::MAX);
        fi.set_value(0, 1, 0);

        let op = UnsignedFilterOp::Gte(0, u32::MAX);
        assert!(op.check(fi.get_value(0, 0))); // u32::MAX >= u32::MAX
        assert!(!op.check(fi.get_value(0, 1))); // 0 >= u32::MAX

        let op = UnsignedFilterOp::Lte(0, 0);
        assert!(!op.check(fi.get_value(0, 0))); // u32::MAX <= 0
        assert!(op.check(fi.get_value(0, 1))); // 0 <= 0
    }

    #[test]
    fn find_column_returns_none_for_unregistered() {
        let fi = FilterIndex::new();
        assert!(fi.find_first_column_by_name("nonexistent").is_none());
    }

    #[test]
    fn table_qualified_lookup_keeps_same_named_columns_distinct() {
        let mut index = FilterIndex::new();
        let users_status = index.register_column(101, "status".to_string(), 2);
        let companies_status = index.register_column(202, "status".to_string(), 2);

        assert_eq!(
            index.find_column_for_table(101, "status"),
            Some(users_status)
        );
        assert_eq!(
            index.find_column_for_table(202, "status"),
            Some(companies_status)
        );
    }

    #[test]
    fn column_count_reflects_registrations() {
        let mut fi = FilterIndex::new();
        assert_eq!(fi.column_count(), 0);
        fi.register_column(100, "a".to_string(), 1);
        fi.register_column(100, "b".to_string(), 1);
        assert_eq!(fi.column_count(), 2);
    }

    #[test]
    fn sparse_boolean_filters_preserve_null_semantics() {
        let mut fi = FilterIndex::new();
        let col = fi.register_typed_column_with_populated_count(
            100,
            "active".to_string(),
            FilterColumnType::Boolean,
            100,
            2,
        );
        fi.set_encoded_value(col, 3, Some(EncodedFilterValue::Boolean(true)));
        fi.set_encoded_value(col, 7, Some(EncodedFilterValue::Boolean(false)));

        assert_eq!(fi.storage_kind(col), Some(FilterStorageKind::SparseBool));
        assert!(fi.check_filter(3, &FilterOp::new(col, FilterCondition::EqBool(true))));
        assert!(fi.check_filter(7, &FilterOp::new(col, FilterCondition::NeqBool(true))));
        assert!(!fi.check_filter(9, &FilterOp::new(col, FilterCondition::NeqBool(true))));
        assert!(fi.check_filter(9, &FilterOp::new(col, FilterCondition::IsNull)));
        assert!(fi.check_filter(3, &FilterOp::new(col, FilterCondition::IsNotNull)));
    }

    #[test]
    fn sparse_text_filters_do_not_treat_missing_as_neq() {
        let mut fi = FilterIndex::new();
        let col = fi.register_typed_column_with_populated_count(
            100,
            "status".to_string(),
            FilterColumnType::Text,
            100,
            2,
        );
        let open = fi.intern_text_value(col, "open");
        let closed = fi.intern_text_value(col, "closed");
        fi.set_encoded_value(col, 1, Some(EncodedFilterValue::Text(open)));
        fi.set_encoded_value(col, 2, Some(EncodedFilterValue::Text(closed)));

        assert_eq!(fi.storage_kind(col), Some(FilterStorageKind::SparseLookup));
        assert!(fi.check_filter(1, &FilterOp::new(col, FilterCondition::EqToken(open))));
        assert!(fi.check_filter(2, &FilterOp::new(col, FilterCondition::NeqToken(open))));
        assert!(fi.check_filter(
            1,
            &FilterOp::new(col, FilterCondition::ContainsToken("pe".to_string()))
        ));
        assert!(fi.check_filter(
            2,
            &FilterOp::new(col, FilterCondition::PrefixToken("cl".to_string()))
        ));
        assert!(!fi.check_filter(9, &FilterOp::new(col, FilterCondition::NeqToken(open))));
        assert!(fi.check_filter(9, &FilterOp::new(col, FilterCondition::IsNull)));
    }

    #[test]
    fn sparse_numeric_filters_use_sorted_binary_lookup() {
        let mut fi = FilterIndex::new();
        let col = fi.register_typed_column_with_populated_count(
            100,
            "amount".to_string(),
            FilterColumnType::Numeric,
            100,
            3,
        );
        fi.set_encoded_value(col, 20, Some(EncodedFilterValue::Numeric(50)));
        fi.set_encoded_value(col, 3, Some(EncodedFilterValue::Numeric(10)));
        fi.set_encoded_value(col, 9, Some(EncodedFilterValue::Numeric(30)));

        assert_eq!(fi.storage_kind(col), Some(FilterStorageKind::SparseOrdered));
        assert!(fi.check_filter(9, &FilterOp::new(col, FilterCondition::GtI64(20))));
        assert!(fi.check_filter(3, &FilterOp::new(col, FilterCondition::BetweenI64(10, 30))));
        assert!(!fi.check_filter(99, &FilterOp::new(col, FilterCondition::GtI64(0))));
        assert!(fi.check_filter(99, &FilterOp::new(col, FilterCondition::IsNull)));
    }

    #[test]
    fn sparsity_heuristic_switches_at_fifteen_percent() {
        let mut fi = FilterIndex::new();
        let sparse = fi.register_typed_column_with_populated_count(
            100,
            "sparse".to_string(),
            FilterColumnType::Numeric,
            100,
            14,
        );
        let dense = fi.register_typed_column_with_populated_count(
            100,
            "dense".to_string(),
            FilterColumnType::Numeric,
            100,
            15,
        );

        assert_eq!(
            fi.storage_kind(sparse),
            Some(FilterStorageKind::SparseOrdered)
        );
        assert_eq!(fi.storage_kind(dense), Some(FilterStorageKind::Dense));
    }

    #[test]
    fn dense_numeric_filters_keep_indexed_loads() {
        let mut fi = FilterIndex::new();
        let col = fi.register_typed_column_with_populated_count(
            100,
            "score".to_string(),
            FilterColumnType::Numeric,
            10,
            10,
        );
        fi.set_encoded_value(col, 4, Some(EncodedFilterValue::Numeric(42)));

        assert_eq!(fi.storage_kind(col), Some(FilterStorageKind::Dense));
        assert_eq!(fi.get_value(col, 4), 42);
        assert!(fi.check_filter(4, &FilterOp::new(col, FilterCondition::EqI64(42))));
    }

    #[test]
    fn transaction_filter_update_overrides_base_value() {
        crate::projection::tx_delta::clear_for_test();
        let mut fi = FilterIndex::new();
        let col = fi.register_typed_column(100, "score".to_string(), FilterColumnType::Numeric, 10);
        fi.set_encoded_value(col, 4, Some(EncodedFilterValue::Numeric(42)));
        crate::projection::tx_delta::record_filter_value_update(
            col,
            4,
            Some(EncodedFilterValue::Numeric(101)),
        )
        .expect("record filter update");

        assert!(fi.check_filter(4, &FilterOp::new(col, FilterCondition::GtI64(100))));
        assert!(!fi.check_filter(4, &FilterOp::new(col, FilterCondition::EqI64(42))));

        crate::projection::tx_delta::clear_for_test();
    }

    #[test]
    fn persisted_filter_values_restore_exact_backend_local_encodings() {
        let mut index = FilterIndex::new();
        let cases = [
            (
                FilterColumnType::Numeric,
                PersistedFilterValue::Numeric(i64::MIN + 1),
                EncodedFilterValue::Numeric(i64::MIN + 1),
            ),
            (
                FilterColumnType::Boolean,
                PersistedFilterValue::Boolean(true),
                EncodedFilterValue::Boolean(true),
            ),
            (
                FilterColumnType::Date,
                PersistedFilterValue::Date(-20_000),
                EncodedFilterValue::Date(-20_000),
            ),
            (
                FilterColumnType::Timestamptz,
                PersistedFilterValue::Timestamptz(4_102_444_800_123_456),
                EncodedFilterValue::Timestamptz(4_102_444_800_123_456),
            ),
            (
                FilterColumnType::Uuid,
                PersistedFilterValue::Uuid(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef),
                EncodedFilterValue::Uuid(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef),
            ),
        ];
        for (idx, (column_type, persisted, expected)) in cases.into_iter().enumerate() {
            let column = index.register_typed_column(100, format!("column_{idx}"), column_type, 1);
            index
                .apply_persisted_value(column, 0, 1, &persisted, false)
                .expect("persisted value applies");
            assert_eq!(index.storage[column].value(0), Some(expected));
        }

        let text_column =
            index.register_typed_column(100, "text".into(), FilterColumnType::Text, 1);
        index
            .apply_persisted_value(
                text_column,
                0,
                1,
                &PersistedFilterValue::Text("durable 🧪 filter".into()),
                false,
            )
            .expect("persisted text applies");
        let token = index
            .lookup_text_value(text_column, "durable 🧪 filter")
            .expect("text is interned in the restored backend");
        assert_eq!(
            index.storage[text_column].value(0),
            Some(EncodedFilterValue::Text(token))
        );

        index
            .apply_persisted_value(text_column, 0, 1, &PersistedFilterValue::Null, false)
            .expect("persisted null applies");
        assert_eq!(index.storage[text_column].value(0), None);
    }

    #[test]
    fn persisted_filter_value_rejects_registered_type_mismatch() {
        let mut index = FilterIndex::new();
        let column = index.register_typed_column(100, "score".into(), FilterColumnType::Numeric, 1);

        let err = index
            .apply_persisted_value(column, 0, 1, &PersistedFilterValue::Uuid(1), false)
            .expect_err("mismatched durable type must be rejected");

        assert!(matches!(err, GraphError::CorruptFile { .. }));
    }

    #[test]
    fn persisted_filter_value_rejects_out_of_range_dense_and_sparse_nodes() {
        let mut index = FilterIndex::new();
        let dense = index.register_typed_column_with_populated_count(
            100,
            "dense".into(),
            FilterColumnType::Numeric,
            1,
            1,
        );
        let sparse = index.register_typed_column_with_populated_count(
            100,
            "sparse".into(),
            FilterColumnType::Numeric,
            1,
            0,
        );

        for column in [dense, sparse] {
            let err = index
                .apply_persisted_value(column, 1, 1, &PersistedFilterValue::Numeric(42), false)
                .expect_err("out-of-range persisted node must be rejected");
            assert!(matches!(err, GraphError::CorruptFile { .. }));
        }
    }

    #[test]
    fn malformed_persisted_text_dictionary_fails_closed() {
        let mut index = FilterIndex::new();
        let column = index.register_typed_column(100, "text".into(), FilterColumnType::Text, 1);
        index.reverse_text_dictionaries.pop();

        assert!(matches!(
            index.validate_persisted_layout(1),
            Err(GraphError::CorruptFile { .. })
        ));
        assert!(matches!(
            index.apply_persisted_value(
                column,
                0,
                1,
                &PersistedFilterValue::Text("value".into()),
                false,
            ),
            Err(GraphError::CorruptFile { .. })
        ));
    }
}
