//! # FilterIndex — hybrid storage for traversal filtering
//!
//! Registered filter columns are indexed by internal `node_idx` so BFS can
//! evaluate traversal predicates without routing each neighbor back through SQL.

use crate::types::FilterOp;
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
        let Some(storage) = self.storage.get(column_idx) else {
            return matches!(op, FilterOp::IsNull(_));
        };
        if !storage.is_present(node_idx) {
            return matches!(op, FilterOp::IsNull(_));
        }
        match op {
            FilterOp::Gt(_, threshold) => self.get_value(column_idx, node_idx) > *threshold,
            FilterOp::Gte(_, threshold) => self.get_value(column_idx, node_idx) >= *threshold,
            FilterOp::Lt(_, threshold) => self.get_value(column_idx, node_idx) < *threshold,
            FilterOp::Lte(_, threshold) => self.get_value(column_idx, node_idx) <= *threshold,
            FilterOp::Eq(_, threshold) => self.get_value(column_idx, node_idx) == *threshold,
            FilterOp::Neq(_, threshold) => self.get_value(column_idx, node_idx) != *threshold,
            FilterOp::Between(_, lo, hi) => {
                let value = self.get_value(column_idx, node_idx);
                value >= *lo && value <= *hi
            }
            FilterOp::EqI64(_, expected) => storage.encoded_i64(node_idx) == Some(*expected),
            FilterOp::NeqI64(_, expected) => storage.encoded_i64(node_idx) != Some(*expected),
            FilterOp::GtI64(_, expected) => self
                .storage
                .get(column_idx)
                .and_then(|storage| storage.encoded_i64(node_idx))
                .is_some_and(|value| value > *expected),
            FilterOp::GteI64(_, expected) => self
                .storage
                .get(column_idx)
                .and_then(|storage| storage.encoded_i64(node_idx))
                .is_some_and(|value| value >= *expected),
            FilterOp::LtI64(_, expected) => self
                .storage
                .get(column_idx)
                .and_then(|storage| storage.encoded_i64(node_idx))
                .is_some_and(|value| value < *expected),
            FilterOp::LteI64(_, expected) => self
                .storage
                .get(column_idx)
                .and_then(|storage| storage.encoded_i64(node_idx))
                .is_some_and(|value| value <= *expected),
            FilterOp::BetweenI64(_, low, high) => self
                .storage
                .get(column_idx)
                .and_then(|storage| storage.encoded_i64(node_idx))
                .is_some_and(|value| value >= *low && value <= *high),
            FilterOp::EqBool(_, expected) => {
                matches!(storage.value(node_idx), Some(EncodedFilterValue::Boolean(value)) if value == *expected)
            }
            FilterOp::NeqBool(_, expected) => {
                matches!(storage.value(node_idx), Some(EncodedFilterValue::Boolean(value)) if value != *expected)
            }
            FilterOp::EqToken(_, expected) => {
                matches!(storage.value(node_idx), Some(EncodedFilterValue::Text(value)) if value == *expected)
            }
            FilterOp::NeqToken(_, expected) => {
                matches!(storage.value(node_idx), Some(EncodedFilterValue::Text(value)) if value != *expected)
            }
            FilterOp::EqUuid(_, expected) => {
                matches!(storage.value(node_idx), Some(EncodedFilterValue::Uuid(value)) if value == *expected)
            }
            FilterOp::NeqUuid(_, expected) => {
                matches!(storage.value(node_idx), Some(EncodedFilterValue::Uuid(value)) if value != *expected)
            }
            FilterOp::IsNull(_) => false,
            FilterOp::IsNotNull(_) => true,
        }
    }

    /// Check a node against multiple AND'd filter operations.
    #[inline]
    pub fn check_filters(&self, node_idx: u32, ops: &[FilterOp]) -> bool {
        ops.iter().all(|op| self.check_filter(node_idx, op))
    }

    /// Find the column index for a given column name.
    pub fn find_column(&self, column_name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.column_name == column_name)
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

    /// Number of registered filter columns.
    pub fn column_count(&self) -> usize {
        self.columns.len()
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

    /// Parse a legacy unsigned filter condition string into FilterOp(s).
    ///
    /// Supported formats:
    /// - `"amount > 10000"`
    /// - `"amount >= 10000"`
    /// - `"risk_score BETWEEN 50 AND 90"`
    /// - `"amount > 1000 AND risk_score > 50"`
    ///
    /// Returns `Err` with a reason if parsing fails.
    #[cfg(any(test, feature = "development", feature = "fuzzing"))]
    pub fn parse_condition(&self, condition: &str) -> Result<Vec<FilterOp>, String> {
        let mut ops = Vec::new();
        let mut tokens = condition.split_whitespace().peekable();

        while tokens.peek().is_some() {
            let col_name = tokens
                .next()
                .ok_or_else(|| "missing filter column".to_string())?;
            let op_str = tokens
                .next()
                .ok_or_else(|| format!("missing operator after '{}'", col_name))?;

            let op = if op_str.eq_ignore_ascii_case("BETWEEN") {
                let lo_str = tokens
                    .next()
                    .ok_or_else(|| format!("missing lower bound for '{}'", col_name))?;
                let and_token = tokens
                    .next()
                    .ok_or_else(|| format!("missing AND in BETWEEN for '{}'", col_name))?;
                if !and_token.eq_ignore_ascii_case("AND") {
                    return Err(format!("missing AND in BETWEEN for '{}'", col_name));
                }
                let hi_str = tokens
                    .next()
                    .ok_or_else(|| format!("missing upper bound for '{}'", col_name))?;
                let lo = parse_u32_value(lo_str)?;
                let hi = parse_u32_value(hi_str)?;
                let col_idx = self.find_column(col_name).ok_or_else(|| {
                    format!("Column '{}' not registered as a filter column", col_name)
                })?;
                FilterOp::Between(col_idx, lo, hi)
            } else {
                let value_str = tokens
                    .next()
                    .ok_or_else(|| format!("missing value after '{} {}'", col_name, op_str))?;
                let value = parse_u32_value(value_str)?;
                let col_idx = self.find_column(col_name).ok_or_else(|| {
                    format!("Column '{}' not registered as a filter column", col_name)
                })?;
                match op_str {
                    ">=" => FilterOp::Gte(col_idx, value),
                    "<=" => FilterOp::Lte(col_idx, value),
                    "!=" => FilterOp::Neq(col_idx, value),
                    ">" => FilterOp::Gt(col_idx, value),
                    "<" => FilterOp::Lt(col_idx, value),
                    "=" => FilterOp::Eq(col_idx, value),
                    _ => return Err(format!("unsupported filter operator '{}'", op_str)),
                }
            };
            ops.push(op);

            if let Some(separator) = tokens.next() {
                if !separator.eq_ignore_ascii_case("AND") {
                    return Err(format!(
                        "expected AND between filter clauses, got '{}'",
                        separator
                    ));
                }
                if tokens.peek().is_none() {
                    return Err("trailing AND in filter condition".to_string());
                }
            }
        }

        Ok(ops)
    }
}

#[cfg(any(test, feature = "development", feature = "fuzzing"))]
fn parse_u32_value(value: &str) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("Cannot parse '{}' as a number", value))
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

impl FilterColumnStorage {
    #[cfg(test)]
    fn kind(&self) -> FilterStorageKind {
        match self {
            Self::Dense { .. } => FilterStorageKind::Dense,
            Self::SparseBool { .. } => FilterStorageKind::SparseBool,
            Self::SparseLookup { .. } => FilterStorageKind::SparseLookup,
            Self::SparseOrdered { .. } => FilterStorageKind::SparseOrdered,
        }
    }

    fn is_present(&self, node_idx: u32) -> bool {
        match self {
            Self::Dense { present_bitmap, .. }
            | Self::SparseBool { present_bitmap, .. }
            | Self::SparseLookup { present_bitmap, .. }
            | Self::SparseOrdered { present_bitmap, .. } => present_bitmap.contains(node_idx),
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

    fn encoded_i64(&self, node_idx: u32) -> Option<i64> {
        match self.value(node_idx)? {
            EncodedFilterValue::Numeric(value)
            | EncodedFilterValue::Date(value)
            | EncodedFilterValue::Timestamptz(value) => Some(value),
            _ => None,
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
    bincode::serialized_size(bitmap)
        .map(|size| size as usize)
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
    fn parse_and_check_gt() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "amount".to_string(), 3);
        fi.set_value(0, 0, 5000);
        fi.set_value(0, 1, 15000);

        let ops = fi.parse_condition("amount > 10000").unwrap();
        assert!(!fi.check_filters(0, &ops)); // 5000 > 10000 = false
        assert!(fi.check_filters(1, &ops)); // 15000 > 10000 = true
    }

    #[test]
    fn parse_between() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "amount".to_string(), 3);
        fi.set_value(0, 0, 500);
        fi.set_value(0, 1, 1500);
        fi.set_value(0, 2, 6000);

        let ops = fi.parse_condition("amount BETWEEN 1000 AND 5000").unwrap();
        assert!(!fi.check_filters(0, &ops)); // 500 not in range
        assert!(fi.check_filters(1, &ops)); // 1500 in range
        assert!(!fi.check_filters(2, &ops)); // 6000 not in range
    }

    #[test]
    fn parse_invalid_column_returns_error() {
        let fi = FilterIndex::new();
        let result = fi.parse_condition("nonexistent > 100");
        assert!(result.is_err());
    }

    #[test]
    fn parse_all_comparison_operators() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "amount".to_string(), 3);
        fi.set_value(0, 0, 10);
        fi.set_value(0, 1, 20);
        fi.set_value(0, 2, 30);

        let cases = [
            ("amount >= 20", [false, true, true]),
            ("amount <= 20", [true, true, false]),
            ("amount != 20", [true, false, true]),
            ("amount = 20", [false, true, false]),
            ("amount < 20", [true, false, false]),
        ];

        for (condition, expected) in cases {
            let ops = fi.parse_condition(condition).unwrap();
            let actual = [
                fi.check_filters(0, &ops),
                fi.check_filters(1, &ops),
                fi.check_filters(2, &ops),
            ];
            assert_eq!(actual, expected, "condition: {}", condition);
        }
    }

    #[test]
    fn parse_multiple_and_conditions_across_columns() {
        let mut fi = FilterIndex::new();
        let amount = fi.register_column(100, "amount".to_string(), 3);
        let risk = fi.register_column(100, "risk".to_string(), 3);
        fi.set_value(amount, 0, 500);
        fi.set_value(risk, 0, 90);
        fi.set_value(amount, 1, 1500);
        fi.set_value(risk, 1, 20);
        fi.set_value(amount, 2, 2000);
        fi.set_value(risk, 2, 80);

        let ops = fi
            .parse_condition("amount >= 1000 AND risk BETWEEN 50 AND 100")
            .unwrap();

        assert!(!fi.check_filters(0, &ops));
        assert!(!fi.check_filters(1, &ops));
        assert!(fi.check_filters(2, &ops));
    }

    #[test]
    fn parse_invalid_numeric_value_returns_error() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "amount".to_string(), 1);

        let result = fi.parse_condition("amount > one_hundred");

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot parse"));
    }

    #[test]
    fn legacy_condition_parser_rejects_signed_numeric_literals() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "amount".to_string(), 1);

        let result = fi.parse_condition("amount > -1");

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot parse '-1'"));
    }

    #[test]
    fn parse_malformed_between_returns_error() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "amount".to_string(), 1);

        let result = fi.parse_condition("amount BETWEEN low AND 100");

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot parse"));
    }

    #[test]
    fn unsupported_or_condition_returns_error() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "amount".to_string(), 1);

        let result = fi.parse_condition("amount = 10 OR amount = 20");

        assert!(result.is_err());
    }

    #[test]
    fn unicode_filter_input_returns_error_without_panicking() {
        let fi = FilterIndex::new();
        let cases = [
            "🔥 > 1",
            "amount > 🔥",
            "straße BETWEEN 1 AND 2 AND amount > 3",
            "金额 > 100 AND",
            "amount BETWEEN 1 🔥 2",
        ];

        for condition in cases {
            let result = std::panic::catch_unwind(|| fi.parse_condition(condition));
            assert!(result.is_ok(), "condition panicked: {condition}");
            assert!(result.unwrap().is_err(), "condition parsed: {condition}");
        }
    }

    #[test]
    fn unicode_registered_identifier_is_char_boundary_safe() {
        let mut fi = FilterIndex::new();
        fi.register_column(100, "金额".to_string(), 1);
        fi.set_value(0, 0, 101);

        let ops = fi.parse_condition("金额 > 100").unwrap();

        assert!(fi.check_filters(0, &ops));
    }

    #[test]
    fn empty_condition_string_returns_empty_ops() {
        let fi = FilterIndex::new();
        let result = fi.parse_condition("");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
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
        assert!(fi.find_column("nonexistent").is_none());
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
        assert!(fi.check_filter(3, &FilterOp::EqBool(col, true)));
        assert!(fi.check_filter(7, &FilterOp::NeqBool(col, true)));
        assert!(!fi.check_filter(9, &FilterOp::NeqBool(col, true)));
        assert!(fi.check_filter(9, &FilterOp::IsNull(col)));
        assert!(fi.check_filter(3, &FilterOp::IsNotNull(col)));
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
        assert!(fi.check_filter(1, &FilterOp::EqToken(col, open)));
        assert!(fi.check_filter(2, &FilterOp::NeqToken(col, open)));
        assert!(!fi.check_filter(9, &FilterOp::NeqToken(col, open)));
        assert!(fi.check_filter(9, &FilterOp::IsNull(col)));
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
        assert!(fi.check_filter(9, &FilterOp::GtI64(col, 20)));
        assert!(fi.check_filter(3, &FilterOp::BetweenI64(col, 10, 30)));
        assert!(!fi.check_filter(99, &FilterOp::GtI64(col, 0)));
        assert!(fi.check_filter(99, &FilterOp::IsNull(col)));
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
        assert!(fi.check_filter(4, &FilterOp::EqI64(col, 42)));
    }
}
