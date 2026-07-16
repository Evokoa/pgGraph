//! # FilterIndex — hybrid storage for traversal filtering
//!
//! Registered filter columns are indexed by internal `node_idx` so BFS can
//! evaluate traversal predicates without routing each neighbor back through SQL.

use crate::mapped_bytes::MappedBytes;
use crate::types::{FilterCondition, FilterOp};
use crate::{safety::GraphError, safety::GraphResult};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Range;

const SPARSE_THRESHOLD_NUMERATOR: usize = 15;
const SPARSE_THRESHOLD_DENOMINATOR: usize = 100;
pub(crate) const FILTER_CATALOG_HEADER_SIZE: usize = 16;
pub(crate) const FILTER_DESCRIPTOR_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum PersistedStorageKind {
    Dense = 0,
    SparseBool = 1,
    SparseLookup = 2,
    SparseOrdered = 3,
}

impl PersistedStorageKind {
    fn parse(value: u8) -> GraphResult<Self> {
        match value {
            0 => Ok(Self::Dense),
            1 => Ok(Self::SparseBool),
            2 => Ok(Self::SparseLookup),
            3 => Ok(Self::SparseOrdered),
            _ => Err(filter_index_corrupt(format!(
                "unknown filter storage kind {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
struct MappedColumn {
    column_type: FilterColumnType,
    storage_kind: PersistedStorageKind,
    value_width: usize,
    row_count: u32,
    data_range: Range<usize>,
    dictionary_range: Range<usize>,
    dictionary_count: u32,
}

#[derive(Debug, Clone)]
struct MappedFilterBase {
    bytes: MappedBytes,
    columns: Vec<MappedColumn>,
    node_count: u32,
}

#[derive(Debug, Clone, Default)]
struct TextDelta {
    forward: HashMap<String, u32>,
    reverse: Vec<String>,
}

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
    fn persisted_tag(self) -> u8 {
        match self {
            Self::Numeric => 0,
            Self::Boolean => 1,
            Self::Text => 2,
            Self::Date => 3,
            Self::Timestamptz => 4,
            Self::Uuid => 5,
        }
    }

    fn from_persisted_tag(value: u8) -> GraphResult<Self> {
        match value {
            0 => Ok(Self::Numeric),
            1 => Ok(Self::Boolean),
            2 => Ok(Self::Text),
            3 => Ok(Self::Date),
            4 => Ok(Self::Timestamptz),
            5 => Ok(Self::Uuid),
            _ => Err(filter_index_corrupt(format!(
                "unknown filter column type {value}"
            ))),
        }
    }

    const fn persisted_width(self) -> usize {
        match self {
            Self::Boolean => 1,
            Self::Text => 4,
            Self::Numeric | Self::Date | Self::Timestamptz => 8,
            Self::Uuid => 16,
        }
    }

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
    #[serde(skip)]
    mapped_base: Option<MappedFilterBase>,
    #[serde(skip)]
    delta: Vec<HashMap<u32, Option<EncodedFilterValue>>>,
    #[serde(skip)]
    text_delta: Vec<TextDelta>,
}

impl FilterIndex {
    /// Create an empty filter index.
    pub fn new() -> Self {
        Self {
            columns: Vec::new(),
            storage: Vec::new(),
            text_dictionaries: Vec::new(),
            reverse_text_dictionaries: Vec::new(),
            mapped_base: None,
            delta: Vec::new(),
            text_delta: Vec::new(),
        }
    }

    /// Construct an immutable mapped base from the three validated v5 filter
    /// sections. Section-relative offsets in the catalog are translated into
    /// ranges owned by `bytes`; graph-sized values and dictionaries remain in
    /// the read-only mapping.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] when catalog descriptors, sparse
    /// ordering, typed values, or lexical dictionaries are inconsistent.
    pub(crate) fn from_mapped_sections(
        bytes: MappedBytes,
        catalog_range: Range<usize>,
        data_range: Range<usize>,
        dictionary_range: Range<usize>,
        node_count: u32,
    ) -> GraphResult<Self> {
        validate_range(&catalog_range, bytes.len(), "filter catalog")?;
        validate_range(&data_range, bytes.len(), "filter data")?;
        validate_range(&dictionary_range, bytes.len(), "filter dictionary")?;
        let catalog = &bytes.as_slice()[catalog_range.clone()];
        if catalog.len() < FILTER_CATALOG_HEADER_SIZE {
            return Err(filter_index_corrupt("filter catalog header is truncated"));
        }
        let column_count = read_u32(catalog, 0)? as usize;
        if read_u32(catalog, 4)? as usize != FILTER_DESCRIPTOR_SIZE {
            return Err(filter_index_corrupt("unexpected filter descriptor size"));
        }
        let name_bytes_len = usize::try_from(read_u64(catalog, 8)?)
            .map_err(|_| filter_index_corrupt("filter catalog names exceed usize"))?;
        let descriptor_bytes = column_count
            .checked_mul(FILTER_DESCRIPTOR_SIZE)
            .ok_or_else(|| filter_index_corrupt("filter descriptor length overflowed"))?;
        let names_start = FILTER_CATALOG_HEADER_SIZE
            .checked_add(descriptor_bytes)
            .ok_or_else(|| filter_index_corrupt("filter catalog length overflowed"))?;
        let catalog_end = names_start
            .checked_add(name_bytes_len)
            .ok_or_else(|| filter_index_corrupt("filter catalog length overflowed"))?;
        if catalog_end != catalog.len() {
            return Err(filter_index_corrupt(
                "filter catalog length does not match its header",
            ));
        }
        let names = &catalog[names_start..catalog_end];
        let mut columns = Vec::new();
        columns
            .try_reserve_exact(column_count)
            .map_err(allocation_error)?;
        let mut mapped_columns = Vec::new();
        mapped_columns
            .try_reserve_exact(column_count)
            .map_err(allocation_error)?;
        let mut expected_name_end = 0usize;
        let mut expected_data_end = 0usize;
        let mut expected_dictionary_end = 0usize;
        for column_idx in 0..column_count {
            let start = FILTER_CATALOG_HEADER_SIZE + column_idx * FILTER_DESCRIPTOR_SIZE;
            let descriptor = &catalog[start..start + FILTER_DESCRIPTOR_SIZE];
            if descriptor[7] != 0 || descriptor[60..64].iter().any(|byte| *byte != 0) {
                return Err(filter_index_corrupt(format!(
                    "filter column {column_idx} has nonzero reserved bytes"
                )));
            }
            let column_type = FilterColumnType::from_persisted_tag(descriptor[4])?;
            let storage_kind = PersistedStorageKind::parse(descriptor[5])?;
            let value_width = descriptor[6] as usize;
            if value_width != column_type.persisted_width() {
                return Err(filter_index_corrupt(format!(
                    "filter column {column_idx} has an invalid value width"
                )));
            }
            validate_storage_kind(column_idx, column_type, storage_kind)?;
            let name_offset = usize::try_from(read_u64(descriptor, 8)?)
                .map_err(|_| filter_index_corrupt("filter name offset exceeds usize"))?;
            let name_len = read_u32(descriptor, 16)? as usize;
            let name_end = name_offset
                .checked_add(name_len)
                .ok_or_else(|| filter_index_corrupt("filter name range overflowed"))?;
            if name_offset != expected_name_end {
                return Err(filter_index_corrupt(
                    "filter catalog names are not contiguous and canonical",
                ));
            }
            let column_name = std::str::from_utf8(
                names
                    .get(name_offset..name_end)
                    .ok_or_else(|| filter_index_corrupt("filter name is outside catalog"))?,
            )
            .map_err(|_| filter_index_corrupt("filter column name is not UTF-8"))?;
            let mut owned_column_name = String::new();
            owned_column_name
                .try_reserve_exact(column_name.len())
                .map_err(allocation_error)?;
            owned_column_name.push_str(column_name);
            expected_name_end = name_end;
            let row_count = read_u32(descriptor, 20)?;
            let local_data = relative_range(descriptor, 24, 32, data_range.len(), "filter data")?;
            let local_dictionary = relative_range(
                descriptor,
                40,
                48,
                dictionary_range.len(),
                "filter dictionary",
            )?;
            if local_data.start != expected_data_end
                || local_dictionary.start != expected_dictionary_end
            {
                return Err(filter_index_corrupt(
                    "filter value or dictionary ranges are not contiguous and canonical",
                ));
            }
            expected_data_end = local_data.end;
            expected_dictionary_end = local_dictionary.end;
            let dictionary_count = read_u32(descriptor, 56)?;
            let data = shift_range(&local_data, data_range.start)?;
            let dictionary = shift_range(&local_dictionary, dictionary_range.start)?;
            validate_mapped_column(
                &bytes,
                column_idx,
                column_type,
                storage_kind,
                value_width,
                row_count,
                &data,
                &dictionary,
                dictionary_count,
                node_count,
            )?;
            columns.push(FilterColumnMeta {
                table_oid: read_u32(descriptor, 0)?,
                column_name: owned_column_name,
                column_type,
            });
            mapped_columns.push(MappedColumn {
                column_type,
                storage_kind,
                value_width,
                row_count,
                data_range: data,
                dictionary_range: dictionary,
                dictionary_count,
            });
        }
        if expected_name_end != names.len()
            || expected_data_end != data_range.len()
            || expected_dictionary_end != dictionary_range.len()
        {
            return Err(filter_index_corrupt(
                "filter sections contain unreferenced trailing bytes",
            ));
        }
        let mut identities = std::collections::HashSet::new();
        identities
            .try_reserve(columns.len())
            .map_err(allocation_error)?;
        if columns
            .iter()
            .any(|column| !identities.insert((column.table_oid, column.column_name.as_str())))
        {
            return Err(filter_index_corrupt(
                "filter catalog contains duplicate table/column identities",
            ));
        }
        let mut text_dictionaries = Vec::new();
        text_dictionaries
            .try_reserve_exact(column_count)
            .map_err(allocation_error)?;
        text_dictionaries.resize_with(column_count, HashMap::new);
        let mut reverse_text_dictionaries = Vec::new();
        reverse_text_dictionaries
            .try_reserve_exact(column_count)
            .map_err(allocation_error)?;
        reverse_text_dictionaries.resize_with(column_count, Vec::new);
        let mut delta = Vec::new();
        delta
            .try_reserve_exact(column_count)
            .map_err(allocation_error)?;
        delta.resize_with(column_count, HashMap::new);
        let mut text_delta = Vec::new();
        text_delta
            .try_reserve_exact(column_count)
            .map_err(allocation_error)?;
        text_delta.resize_with(column_count, TextDelta::default);
        Ok(Self {
            storage: Vec::new(),
            text_dictionaries,
            reverse_text_dictionaries,
            mapped_base: Some(MappedFilterBase {
                bytes,
                columns: mapped_columns,
                node_count,
            }),
            delta,
            text_delta,
            columns,
        })
    }

    /// Compute a checked upper bound for mapped-filter metadata and validation
    /// scratch before allocating per-column containers.
    pub(crate) fn mapped_load_metadata_upper_bound(catalog: &[u8]) -> GraphResult<usize> {
        if catalog.len() < FILTER_CATALOG_HEADER_SIZE {
            return Err(filter_index_corrupt("filter catalog header is truncated"));
        }
        let column_count = read_u32(catalog, 0)? as usize;
        let names_len = usize::try_from(read_u64(catalog, 8)?)
            .map_err(|_| filter_index_corrupt("filter catalog names exceed usize"))?;
        let retained_per_column = [
            std::mem::size_of::<FilterColumnMeta>(),
            std::mem::size_of::<MappedColumn>(),
            std::mem::size_of::<HashMap<String, u32>>(),
            std::mem::size_of::<Vec<String>>(),
            std::mem::size_of::<HashMap<u32, Option<EncodedFilterValue>>>(),
            std::mem::size_of::<TextDelta>(),
        ]
        .into_iter()
        .try_fold(0usize, usize::checked_add)
        .ok_or_else(|| GraphError::Internal("filter metadata size overflowed".into()))?;
        let uniqueness_scratch = std::mem::size_of::<(u32, &str)>()
            .checked_add(18)
            .ok_or_else(|| GraphError::Internal("filter metadata size overflowed".into()))?;
        retained_per_column
            .checked_add(uniqueness_scratch)
            .and_then(|bytes| bytes.checked_mul(2))
            .and_then(|bytes| bytes.checked_mul(column_count))
            .and_then(|bytes| bytes.checked_add(names_len.checked_mul(2)?))
            .and_then(|bytes| bytes.checked_add(256))
            .ok_or_else(|| GraphError::Internal("filter metadata size overflowed".into()))
    }

    /// Encode the immutable v5 catalog, value, and dictionary sections.
    ///
    /// The persisted representation is canonical: columns use dense value
    /// arrays, text dictionaries are strictly lexical, and text tokens are
    /// rewritten to that lexical order. The loader can therefore retain all
    /// graph-sized values and dictionary bytes in the mapped artifact.
    ///
    /// # Errors
    ///
    /// Returns an error if a column contains a value from another domain or a
    /// section length cannot be represented by the v5 descriptor fields.
    pub(crate) fn encode_v5_sections(
        &self,
        node_count: u32,
    ) -> GraphResult<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let (catalog_capacity, data_capacity, dictionary_capacity) =
            self.v5_section_size_upper_bounds(node_count)?;
        let column_count = u32::try_from(self.columns.len())
            .map_err(|_| GraphError::Internal("filter catalog exceeds u32 columns".into()))?;
        let mut names = Vec::new();
        let mut data = Vec::new();
        let mut dictionaries = Vec::new();
        names
            .try_reserve_exact(
                self.columns
                    .iter()
                    .try_fold(0usize, |total, column| {
                        total.checked_add(column.column_name.len())
                    })
                    .ok_or_else(|| GraphError::Internal("filter names overflowed".into()))?,
            )
            .map_err(allocation_error)?;
        data.try_reserve_exact(data_capacity)
            .map_err(allocation_error)?;
        dictionaries
            .try_reserve_exact(dictionary_capacity)
            .map_err(allocation_error)?;
        let mut descriptors = Vec::new();
        descriptors
            .try_reserve_exact(self.columns.len())
            .map_err(allocation_error)?;

        for (column_idx, column) in self.columns.iter().enumerate() {
            let name_offset = u64::try_from(names.len())
                .map_err(|_| GraphError::Internal("filter names exceed u64".into()))?;
            let name_len = u32::try_from(column.column_name.len())
                .map_err(|_| GraphError::Internal("filter column name exceeds u32".into()))?;
            names.extend_from_slice(column.column_name.as_bytes());

            let mut lexical_text = if column.column_type == FilterColumnType::Text {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(node_count as usize)
                    .map_err(allocation_error)?;
                for node_idx in 0..node_count {
                    if let Some(EncodedFilterValue::Text(token)) =
                        self.persistent_value(column_idx, node_idx)
                    {
                        let value = self.text_value(column_idx, token).ok_or_else(|| {
                            filter_index_corrupt(format!(
                                "text filter column {column_idx} references unknown token {token}"
                            ))
                        })?;
                        values.push(value);
                    }
                }
                values.sort_unstable();
                values.dedup();
                values
            } else {
                Vec::new()
            };

            let dictionary_offset = u64::try_from(dictionaries.len())
                .map_err(|_| GraphError::Internal("filter dictionaries exceed u64".into()))?;
            if column.column_type == FilterColumnType::Text {
                let mut string_offset = 0u64;
                dictionaries.extend_from_slice(&string_offset.to_le_bytes());
                for value in &lexical_text {
                    string_offset = string_offset
                        .checked_add(u64::try_from(value.len()).map_err(|_| {
                            GraphError::Internal("filter dictionary value exceeds u64".into())
                        })?)
                        .ok_or_else(|| {
                            GraphError::Internal("filter dictionary bytes exceed u64".into())
                        })?;
                    dictionaries.extend_from_slice(&string_offset.to_le_bytes());
                }
                for value in &lexical_text {
                    dictionaries.extend_from_slice(value.as_bytes());
                }
            }
            let dictionary_len = u64::try_from(dictionaries.len())
                .ok()
                .and_then(|end| end.checked_sub(dictionary_offset))
                .ok_or_else(|| {
                    GraphError::Internal("filter dictionary length overflowed".into())
                })?;
            let dictionary_count = u32::try_from(lexical_text.len())
                .map_err(|_| GraphError::Internal("filter dictionary exceeds u32".into()))?;

            let data_offset = u64::try_from(data.len())
                .map_err(|_| GraphError::Internal("filter data exceeds u64".into()))?;
            let present_len = (node_count as usize).div_ceil(8);
            let presence_start = data.len();
            data.resize(
                data.len()
                    .checked_add(present_len)
                    .ok_or_else(|| GraphError::Internal("filter data length overflowed".into()))?,
                0,
            );
            let value_width = column.column_type.persisted_width();
            let values_len = (node_count as usize)
                .checked_mul(value_width)
                .ok_or_else(|| GraphError::Internal("filter value bytes overflowed".into()))?;
            let values_start = data.len();
            data.resize(
                values_start
                    .checked_add(values_len)
                    .ok_or_else(|| GraphError::Internal("filter data length overflowed".into()))?,
                0,
            );
            let mut row_count = 0u32;
            for node_idx in 0..node_count {
                let Some(value) = self.persistent_value(column_idx, node_idx) else {
                    continue;
                };
                if !encoded_value_matches(column.column_type, value) {
                    return Err(filter_index_corrupt(format!(
                        "filter column {column_idx} contains a value from another domain"
                    )));
                }
                data[presence_start + node_idx as usize / 8] |= 1 << (node_idx % 8);
                row_count = row_count.checked_add(1).ok_or_else(|| {
                    GraphError::Internal("filter populated row count exceeds u32".into())
                })?;
                let start = values_start + node_idx as usize * value_width;
                encode_v5_value(
                    &mut data[start..start + value_width],
                    value,
                    column_idx,
                    &lexical_text,
                    self,
                )?;
            }
            let data_len = u64::try_from(data.len())
                .ok()
                .and_then(|end| end.checked_sub(data_offset))
                .ok_or_else(|| GraphError::Internal("filter data length overflowed".into()))?;

            let mut descriptor = [0u8; FILTER_DESCRIPTOR_SIZE];
            descriptor[0..4].copy_from_slice(&column.table_oid.to_le_bytes());
            descriptor[4] = column.column_type.persisted_tag();
            descriptor[5] = PersistedStorageKind::Dense as u8;
            descriptor[6] = u8::try_from(value_width)
                .map_err(|_| GraphError::Internal("filter value width exceeds u8".into()))?;
            descriptor[8..16].copy_from_slice(&name_offset.to_le_bytes());
            descriptor[16..20].copy_from_slice(&name_len.to_le_bytes());
            descriptor[20..24].copy_from_slice(&row_count.to_le_bytes());
            descriptor[24..32].copy_from_slice(&data_offset.to_le_bytes());
            descriptor[32..40].copy_from_slice(&data_len.to_le_bytes());
            descriptor[40..48].copy_from_slice(&dictionary_offset.to_le_bytes());
            descriptor[48..56].copy_from_slice(&dictionary_len.to_le_bytes());
            descriptor[56..60].copy_from_slice(&dictionary_count.to_le_bytes());
            descriptors.push(descriptor);
            lexical_text.clear();
        }

        let names_len = u64::try_from(names.len())
            .map_err(|_| GraphError::Internal("filter names exceed u64".into()))?;
        let catalog_len = FILTER_CATALOG_HEADER_SIZE
            .checked_add(
                descriptors
                    .len()
                    .checked_mul(FILTER_DESCRIPTOR_SIZE)
                    .ok_or_else(|| GraphError::Internal("filter catalog overflowed".into()))?,
            )
            .and_then(|len| len.checked_add(names.len()))
            .ok_or_else(|| GraphError::Internal("filter catalog overflowed".into()))?;
        debug_assert_eq!(catalog_len, catalog_capacity);
        let mut catalog = Vec::new();
        catalog
            .try_reserve_exact(catalog_capacity)
            .map_err(allocation_error)?;
        catalog.extend_from_slice(&column_count.to_le_bytes());
        catalog.extend_from_slice(&(FILTER_DESCRIPTOR_SIZE as u32).to_le_bytes());
        catalog.extend_from_slice(&names_len.to_le_bytes());
        for descriptor in descriptors {
            catalog.extend_from_slice(&descriptor);
        }
        catalog.extend_from_slice(&names);
        Ok((catalog, data, dictionaries))
    }

    /// Return checked upper bounds for the three v5 filter sections.
    pub(crate) fn v5_section_size_upper_bounds(
        &self,
        node_count: u32,
    ) -> GraphResult<(usize, usize, usize)> {
        let catalog = FILTER_CATALOG_HEADER_SIZE
            .checked_add(
                self.columns
                    .len()
                    .checked_mul(FILTER_DESCRIPTOR_SIZE)
                    .ok_or_else(|| GraphError::Internal("filter catalog overflowed".into()))?,
            )
            .and_then(|bytes| {
                self.columns.iter().try_fold(bytes, |total, column| {
                    total.checked_add(column.column_name.len())
                })
            })
            .ok_or_else(|| GraphError::Internal("filter catalog overflowed".into()))?;
        let present_len = (node_count as usize).div_ceil(8);
        let data = self
            .columns
            .iter()
            .try_fold(0usize, |total, column| {
                (node_count as usize)
                    .checked_mul(column.column_type.persisted_width())
                    .and_then(|bytes| bytes.checked_add(present_len))
                    .and_then(|bytes| total.checked_add(bytes))
            })
            .ok_or_else(|| GraphError::Internal("filter data overflowed".into()))?;
        let mut dictionaries = 0usize;
        for (column_idx, column) in self.columns.iter().enumerate() {
            if column.column_type != FilterColumnType::Text {
                continue;
            }
            dictionaries = dictionaries
                .checked_add(8)
                .ok_or_else(|| GraphError::Internal("filter dictionaries overflowed".into()))?;
            for node_idx in 0..node_count {
                let Some(EncodedFilterValue::Text(token)) =
                    self.persistent_value(column_idx, node_idx)
                else {
                    continue;
                };
                let value = self.text_value(column_idx, token).ok_or_else(|| {
                    filter_index_corrupt(format!(
                        "text filter column {column_idx} references unknown token {token}"
                    ))
                })?;
                dictionaries = dictionaries
                    .checked_add(8)
                    .and_then(|bytes| bytes.checked_add(value.len()))
                    .ok_or_else(|| GraphError::Internal("filter dictionaries overflowed".into()))?;
            }
        }
        Ok((catalog, data, dictionaries))
    }

    /// Return a checked upper bound for transient v5 filter encoding memory.
    pub(crate) fn v5_encoding_workspace_upper_bound(&self, node_count: u32) -> GraphResult<usize> {
        let (catalog, data, dictionaries) = self.v5_section_size_upper_bounds(node_count)?;
        let names = self.columns.iter().try_fold(0usize, |total, column| {
            total.checked_add(column.column_name.len())
        });
        let descriptors = self.columns.len().checked_mul(FILTER_DESCRIPTOR_SIZE);
        let text_scratch = if self
            .columns
            .iter()
            .any(|column| column.column_type == FilterColumnType::Text)
        {
            node_count as usize
        } else {
            0
        }
        .checked_mul(std::mem::size_of::<&str>());
        [
            Some(catalog),
            Some(data),
            Some(dictionaries),
            names,
            descriptors,
            text_scratch,
        ]
        .into_iter()
        .try_fold(0usize, |total, bytes| total.checked_add(bytes?))
        .ok_or_else(|| GraphError::Internal("filter encoding workspace overflowed".into()))
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
        self.delta.push(HashMap::new());
        self.text_delta.push(TextDelta::default());
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
        if self.mapped_base.is_some() {
            let Some(delta) = self.delta.get_mut(column_idx) else {
                return;
            };
            delta.insert(node_idx, value);
            return;
        }
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
    #[cfg(test)]
    pub(crate) fn validate_persisted_layout(&self, node_count: u32) -> GraphResult<()> {
        if let Some(mapped) = &self.mapped_base {
            if mapped.node_count != node_count || mapped.columns.len() != self.columns.len() {
                return Err(filter_index_corrupt(
                    "mapped filter index does not match artifact node or column count",
                ));
            }
            return Ok(());
        }
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

    #[cfg(test)]
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
        if self.mapped_base.is_some() {
            return self.try_intern_text_value(column_idx, value);
        }
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
        self.persistent_value(column_idx, node_idx)
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
        if column_idx >= self.columns.len() {
            return matches!(op.condition(), FilterCondition::IsNull);
        }
        self.check_filter_value(column_idx, self.persistent_value(column_idx, node_idx), op)
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
    ///
    /// # Errors
    ///
    /// Returns an internal error when the dictionary exhausts the `u32` token
    /// space.
    pub fn intern_text_value(&mut self, column_idx: usize, value: &str) -> GraphResult<u32> {
        self.try_intern_text_value(column_idx, value)
    }

    fn try_intern_text_value(&mut self, column_idx: usize, value: &str) -> GraphResult<u32> {
        if let Some(existing) = self.lookup_text_value(column_idx, value) {
            return Ok(existing);
        }
        if self.mapped_base.is_some() {
            let base_count = self
                .mapped_base
                .as_ref()
                .and_then(|base| base.columns.get(column_idx))
                .map_or(0, |column| column.dictionary_count);
            let delta = &mut self.text_delta[column_idx];
            let id = base_count
                .checked_add(u32::try_from(delta.reverse.len()).map_err(|_| {
                    GraphError::Internal("text filter token space exhausted".to_string())
                })?)
                .ok_or_else(|| {
                    GraphError::Internal("text filter token space exhausted".to_string())
                })?;
            delta.forward.insert(value.to_string(), id);
            delta.reverse.push(value.to_string());
            return Ok(id);
        }
        let id = u32::try_from(self.reverse_text_dictionaries[column_idx].len())
            .map_err(|_| GraphError::Internal("text filter token space exhausted".to_string()))?;
        self.text_dictionaries[column_idx].insert(value.to_string(), id);
        self.reverse_text_dictionaries[column_idx].push(value.to_string());
        Ok(id)
    }

    /// Look up an already-interned text token for `column_idx`.
    ///
    /// Returns `None` when the value has never been indexed for that column.
    pub fn lookup_text_value(&self, column_idx: usize, value: &str) -> Option<u32> {
        if let Some(base) = &self.mapped_base {
            if let Some(token) = base.lookup_text(column_idx, value) {
                return Some(token);
            }
            return self
                .text_delta
                .get(column_idx)
                .and_then(|dictionary| dictionary.forward.get(value))
                .copied();
        }
        self.text_dictionaries
            .get(column_idx)
            .and_then(|dictionary| dictionary.get(value))
            .copied()
    }

    /// Return an interned text value by token for `column_idx`.
    pub fn text_value(&self, column_idx: usize, token: u32) -> Option<&str> {
        if let Some(base) = &self.mapped_base {
            let base_count = base.columns.get(column_idx)?.dictionary_count;
            if token < base_count {
                return base.text_value(column_idx, token);
            }
            return self
                .text_delta
                .get(column_idx)?
                .reverse
                .get((token - base_count) as usize)
                .map(String::as_str);
        }
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
    pub(crate) fn storage_kind(&self, column_idx: usize) -> Option<FilterStorageKind> {
        if let Some(base) = &self.mapped_base {
            return base
                .columns
                .get(column_idx)
                .map(|column| match column.storage_kind {
                    PersistedStorageKind::Dense => FilterStorageKind::Dense,
                    PersistedStorageKind::SparseBool => FilterStorageKind::SparseBool,
                    PersistedStorageKind::SparseLookup => FilterStorageKind::SparseLookup,
                    PersistedStorageKind::SparseOrdered => FilterStorageKind::SparseOrdered,
                });
        }
        self.storage.get(column_idx).map(FilterColumnStorage::kind)
    }

    /// Estimate bytes owned by the heap-resident hybrid index.
    pub fn estimated_heap_bytes(&self) -> usize {
        let columns = self
            .columns
            .capacity()
            .saturating_mul(std::mem::size_of::<FilterColumnMeta>())
            .saturating_add(
                self.columns
                    .iter()
                    .map(|column| column.column_name.capacity())
                    .sum(),
            );
        let storage = self
            .storage
            .capacity()
            .saturating_mul(std::mem::size_of::<FilterColumnStorage>())
            .saturating_add(
                self.storage
                    .iter()
                    .map(FilterColumnStorage::estimated_bytes)
                    .sum(),
            );
        let text_dictionaries = self
            .text_dictionaries
            .capacity()
            .saturating_mul(std::mem::size_of::<HashMap<String, u32>>())
            .saturating_add(
                self.text_dictionaries
                    .iter()
                    .fold(0usize, |bytes, dictionary| {
                        bytes
                            .saturating_add(hash_map_allocation_upper_bound::<String, u32>(
                                dictionary,
                            ))
                            .saturating_add(
                                dictionary
                                    .keys()
                                    .map(|value| value.capacity())
                                    .sum::<usize>(),
                            )
                    }),
            );
        let reverse_text_dictionaries = self
            .reverse_text_dictionaries
            .capacity()
            .saturating_mul(std::mem::size_of::<Vec<String>>())
            .saturating_add(self.reverse_text_dictionaries.iter().fold(
                0usize,
                |bytes, dictionary| {
                    bytes
                        .saturating_add(
                            dictionary
                                .capacity()
                                .saturating_mul(std::mem::size_of::<String>()),
                        )
                        .saturating_add(
                            dictionary
                                .iter()
                                .map(|value| value.capacity())
                                .sum::<usize>(),
                        )
                },
            ));
        let mapped_columns = self.mapped_base.as_ref().map_or(0, |base| {
            base.columns
                .capacity()
                .saturating_mul(std::mem::size_of::<MappedColumn>())
        });
        let delta = self
            .delta
            .capacity()
            .saturating_mul(std::mem::size_of::<HashMap<u32, Option<EncodedFilterValue>>>())
            .saturating_add(self.delta.iter().fold(0usize, |bytes, column| {
                bytes.saturating_add(hash_map_allocation_upper_bound::<
                    u32,
                    Option<EncodedFilterValue>,
                >(column))
            }));
        let text_delta = self
            .text_delta
            .capacity()
            .saturating_mul(std::mem::size_of::<TextDelta>())
            .saturating_add(self.text_delta.iter().fold(0usize, |bytes, dictionary| {
                bytes
                    .saturating_add(hash_map_allocation_upper_bound::<String, u32>(
                        &dictionary.forward,
                    ))
                    .saturating_add(
                        dictionary
                            .forward
                            .keys()
                            .map(|value| value.capacity())
                            .sum::<usize>(),
                    )
                    .saturating_add(
                        dictionary
                            .reverse
                            .capacity()
                            .saturating_mul(std::mem::size_of::<String>()),
                    )
                    .saturating_add(
                        dictionary
                            .reverse
                            .iter()
                            .map(|value| value.capacity())
                            .sum::<usize>(),
                    )
            }));
        columns
            .saturating_add(storage)
            .saturating_add(text_dictionaries)
            .saturating_add(reverse_text_dictionaries)
            .saturating_add(mapped_columns)
            .saturating_add(delta)
            .saturating_add(text_delta)
    }

    fn persistent_value(&self, column_idx: usize, node_idx: u32) -> Option<EncodedFilterValue> {
        if let Some(value) = self
            .delta
            .get(column_idx)
            .and_then(|column| column.get(&node_idx))
        {
            return *value;
        }
        if let Some(base) = &self.mapped_base {
            return base.value(column_idx, node_idx);
        }
        self.storage
            .get(column_idx)
            .and_then(|storage| storage.value(node_idx))
    }
}

impl MappedFilterBase {
    fn value(&self, column_idx: usize, node_idx: u32) -> Option<EncodedFilterValue> {
        if node_idx >= self.node_count {
            return None;
        }
        let column = self.columns.get(column_idx)?;
        let data = &self.bytes.as_slice()[column.data_range.clone()];
        match column.storage_kind {
            PersistedStorageKind::Dense => {
                let present_bytes = (self.node_count as usize).div_ceil(8);
                if data[node_idx as usize / 8] & (1 << (node_idx % 8)) == 0 {
                    return None;
                }
                let value_offset = present_bytes + node_idx as usize * column.value_width;
                decode_value(
                    &data[value_offset..value_offset + column.value_width],
                    column_idx,
                    self,
                )
                .ok()
            }
            PersistedStorageKind::SparseBool
            | PersistedStorageKind::SparseLookup
            | PersistedStorageKind::SparseOrdered => {
                let stride = 4 + column.value_width;
                let mut low = 0usize;
                let mut high = column.row_count as usize;
                while low < high {
                    let mid = low + (high - low) / 2;
                    let offset = mid * stride;
                    let candidate = read_u32(data, offset).ok()?;
                    match candidate.cmp(&node_idx) {
                        std::cmp::Ordering::Less => low = mid + 1,
                        std::cmp::Ordering::Greater => high = mid,
                        std::cmp::Ordering::Equal => {
                            return decode_value(
                                &data[offset + 4..offset + stride],
                                column_idx,
                                self,
                            )
                            .ok();
                        }
                    }
                }
                None
            }
        }
    }

    fn lookup_text(&self, column_idx: usize, value: &str) -> Option<u32> {
        let column = self.columns.get(column_idx)?;
        let mut low = 0u32;
        let mut high = column.dictionary_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let candidate = self.text_value(column_idx, mid)?;
            match candidate.cmp(value) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn text_value(&self, column_idx: usize, token: u32) -> Option<&str> {
        let column = self.columns.get(column_idx)?;
        if token >= column.dictionary_count {
            return None;
        }
        let dictionary = &self.bytes.as_slice()[column.dictionary_range.clone()];
        let offsets_bytes = (column.dictionary_count as usize + 1) * 8;
        let start = usize::try_from(read_u64(dictionary, token as usize * 8).ok()?).ok()?;
        let end = usize::try_from(read_u64(dictionary, (token as usize + 1) * 8).ok()?).ok()?;
        std::str::from_utf8(dictionary.get(offsets_bytes + start..offsets_bytes + end)?).ok()
    }
}

fn validate_range(range: &Range<usize>, len: usize, label: &str) -> GraphResult<()> {
    if range.start > range.end || range.end > len {
        return Err(filter_index_corrupt(format!(
            "{label} range is outside the artifact"
        )));
    }
    Ok(())
}

fn relative_range(
    descriptor: &[u8],
    offset_field: usize,
    length_field: usize,
    section_len: usize,
    label: &str,
) -> GraphResult<Range<usize>> {
    let start = usize::try_from(read_u64(descriptor, offset_field)?)
        .map_err(|_| filter_index_corrupt(format!("{label} offset exceeds usize")))?;
    let len = usize::try_from(read_u64(descriptor, length_field)?)
        .map_err(|_| filter_index_corrupt(format!("{label} length exceeds usize")))?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| filter_index_corrupt(format!("{label} range overflowed")))?;
    if end > section_len {
        return Err(filter_index_corrupt(format!(
            "{label} range exceeds its section"
        )));
    }
    Ok(start..end)
}

fn shift_range(range: &Range<usize>, start: usize) -> GraphResult<Range<usize>> {
    Ok(start
        .checked_add(range.start)
        .ok_or_else(|| filter_index_corrupt("filter section range overflowed"))?
        ..start
            .checked_add(range.end)
            .ok_or_else(|| filter_index_corrupt("filter section range overflowed"))?)
}

fn read_u32(bytes: &[u8], offset: usize) -> GraphResult<u32> {
    let raw: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| filter_index_corrupt("filter integer is truncated"))?
        .try_into()
        .map_err(|_| filter_index_corrupt("filter integer is truncated"))?;
    Ok(u32::from_le_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> GraphResult<u64> {
    let raw: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| filter_index_corrupt("filter integer is truncated"))?
        .try_into()
        .map_err(|_| filter_index_corrupt("filter integer is truncated"))?;
    Ok(u64::from_le_bytes(raw))
}

fn validate_storage_kind(
    column_idx: usize,
    column_type: FilterColumnType,
    storage_kind: PersistedStorageKind,
) -> GraphResult<()> {
    let valid = match storage_kind {
        PersistedStorageKind::Dense => true,
        PersistedStorageKind::SparseBool => column_type == FilterColumnType::Boolean,
        PersistedStorageKind::SparseLookup => {
            matches!(column_type, FilterColumnType::Text | FilterColumnType::Uuid)
        }
        PersistedStorageKind::SparseOrdered => matches!(
            column_type,
            FilterColumnType::Numeric | FilterColumnType::Date | FilterColumnType::Timestamptz
        ),
    };
    if valid {
        Ok(())
    } else {
        Err(filter_index_corrupt(format!(
            "filter column {column_idx} storage kind does not match its type"
        )))
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_mapped_column(
    bytes: &MappedBytes,
    column_idx: usize,
    column_type: FilterColumnType,
    storage_kind: PersistedStorageKind,
    value_width: usize,
    row_count: u32,
    data_range: &Range<usize>,
    dictionary_range: &Range<usize>,
    dictionary_count: u32,
    node_count: u32,
) -> GraphResult<()> {
    let data = &bytes.as_slice()[data_range.clone()];
    let expected_data_len = match storage_kind {
        PersistedStorageKind::Dense => {
            if row_count > node_count {
                return Err(filter_index_corrupt(format!(
                    "dense filter column {column_idx} row count exceeds node count"
                )));
            }
            (node_count as usize)
                .div_ceil(8)
                .checked_add(
                    (node_count as usize)
                        .checked_mul(value_width)
                        .ok_or_else(|| filter_index_corrupt("filter data length overflowed"))?,
                )
                .ok_or_else(|| filter_index_corrupt("filter data length overflowed"))?
        }
        PersistedStorageKind::SparseBool
        | PersistedStorageKind::SparseLookup
        | PersistedStorageKind::SparseOrdered => (row_count as usize)
            .checked_mul(4 + value_width)
            .ok_or_else(|| filter_index_corrupt("filter data length overflowed"))?,
    };
    if data.len() != expected_data_len {
        return Err(filter_index_corrupt(format!(
            "filter column {column_idx} data length is inconsistent"
        )));
    }
    match storage_kind {
        PersistedStorageKind::Dense => {
            let present_len = (node_count as usize).div_ceil(8);
            if !node_count.is_multiple_of(8)
                && data
                    .get(present_len.saturating_sub(1))
                    .is_some_and(|last| last & !((1u8 << (node_count % 8)) - 1) != 0)
            {
                return Err(filter_index_corrupt(format!(
                    "filter column {column_idx} has nonzero trailing presence bits"
                )));
            }
            let present_count = data[..present_len]
                .iter()
                .map(|byte| byte.count_ones())
                .sum::<u32>();
            if present_count != row_count {
                return Err(filter_index_corrupt(format!(
                    "filter column {column_idx} row count does not match presence bits"
                )));
            }
            for node_idx in 0..node_count as usize {
                if data[node_idx / 8] & (1 << (node_idx % 8)) == 0 {
                    continue;
                }
                let offset = present_len + node_idx * value_width;
                validate_value(
                    &data[offset..offset + value_width],
                    column_type,
                    dictionary_count,
                )?;
            }
        }
        PersistedStorageKind::SparseBool
        | PersistedStorageKind::SparseLookup
        | PersistedStorageKind::SparseOrdered => {
            let stride = 4 + value_width;
            let mut previous = None;
            for row in data.chunks_exact(stride) {
                let node_idx = read_u32(row, 0)?;
                if node_idx >= node_count || previous.is_some_and(|last| last >= node_idx) {
                    return Err(filter_index_corrupt(format!(
                        "filter column {column_idx} sparse nodes are not strict and in range"
                    )));
                }
                validate_value(&row[4..], column_type, dictionary_count)?;
                previous = Some(node_idx);
            }
        }
    }
    validate_dictionary(
        bytes,
        column_idx,
        column_type,
        dictionary_range,
        dictionary_count,
    )
}

fn validate_value(
    bytes: &[u8],
    column_type: FilterColumnType,
    dictionary_count: u32,
) -> GraphResult<()> {
    match column_type {
        FilterColumnType::Boolean if bytes != [0] && bytes != [1] => {
            Err(filter_index_corrupt("boolean filter value is not 0 or 1"))
        }
        FilterColumnType::Text if read_u32(bytes, 0)? >= dictionary_count => Err(
            filter_index_corrupt("text filter token is outside its dictionary"),
        ),
        _ => Ok(()),
    }
}

fn validate_dictionary(
    bytes: &MappedBytes,
    column_idx: usize,
    column_type: FilterColumnType,
    range: &Range<usize>,
    count: u32,
) -> GraphResult<()> {
    if column_type != FilterColumnType::Text {
        if !range.is_empty() || count != 0 {
            return Err(filter_index_corrupt(format!(
                "non-text filter column {column_idx} has a dictionary"
            )));
        }
        return Ok(());
    }
    let dictionary = &bytes.as_slice()[range.clone()];
    let offsets_len = (count as usize + 1)
        .checked_mul(8)
        .ok_or_else(|| filter_index_corrupt("filter dictionary length overflowed"))?;
    if dictionary.len() < offsets_len || read_u64(dictionary, 0)? != 0 {
        return Err(filter_index_corrupt(format!(
            "text filter column {column_idx} has an invalid dictionary offset table"
        )));
    }
    let strings = &dictionary[offsets_len..];
    if read_u64(dictionary, count as usize * 8)? != strings.len() as u64 {
        return Err(filter_index_corrupt(format!(
            "text filter column {column_idx} dictionary terminal offset is invalid"
        )));
    }
    let mut previous: Option<&str> = None;
    for token in 0..count as usize {
        let start = usize::try_from(read_u64(dictionary, token * 8)?)
            .map_err(|_| filter_index_corrupt("dictionary offset exceeds usize"))?;
        let end = usize::try_from(read_u64(dictionary, (token + 1) * 8)?)
            .map_err(|_| filter_index_corrupt("dictionary offset exceeds usize"))?;
        let value = std::str::from_utf8(
            strings
                .get(start..end)
                .ok_or_else(|| filter_index_corrupt("dictionary string range is invalid"))?,
        )
        .map_err(|_| filter_index_corrupt("filter dictionary value is not UTF-8"))?;
        if previous.is_some_and(|last| last >= value) {
            return Err(filter_index_corrupt(format!(
                "text filter column {column_idx} dictionary is not strictly lexical"
            )));
        }
        previous = Some(value);
    }
    Ok(())
}

fn decode_value(
    bytes: &[u8],
    column_idx: usize,
    base: &MappedFilterBase,
) -> GraphResult<EncodedFilterValue> {
    let column_type = base
        .columns
        .get(column_idx)
        .ok_or_else(|| filter_index_corrupt("filter column is outside mapped base"))?
        .column_type;
    Ok(match column_type {
        FilterColumnType::Boolean => EncodedFilterValue::Boolean(bytes[0] != 0),
        FilterColumnType::Text => EncodedFilterValue::Text(read_u32(bytes, 0)?),
        FilterColumnType::Numeric => EncodedFilterValue::Numeric(i64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| filter_index_corrupt("numeric filter width is invalid"))?,
        )),
        FilterColumnType::Date => EncodedFilterValue::Date(i64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| filter_index_corrupt("date filter width is invalid"))?,
        )),
        FilterColumnType::Timestamptz => EncodedFilterValue::Timestamptz(i64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| filter_index_corrupt("timestamp filter width is invalid"))?,
        )),
        FilterColumnType::Uuid => EncodedFilterValue::Uuid(u128::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| filter_index_corrupt("uuid filter width is invalid"))?,
        )),
    })
}

fn encode_v5_value(
    output: &mut [u8],
    value: EncodedFilterValue,
    column_idx: usize,
    lexical_text: &[&str],
    index: &FilterIndex,
) -> GraphResult<()> {
    match value {
        EncodedFilterValue::Numeric(value)
        | EncodedFilterValue::Date(value)
        | EncodedFilterValue::Timestamptz(value) => output.copy_from_slice(&value.to_le_bytes()),
        EncodedFilterValue::Boolean(value) => output[0] = u8::from(value),
        EncodedFilterValue::Uuid(value) => output.copy_from_slice(&value.to_le_bytes()),
        EncodedFilterValue::Text(token) => {
            let value = index.text_value(column_idx, token).ok_or_else(|| {
                filter_index_corrupt(format!(
                    "text filter column {column_idx} references unknown token {token}"
                ))
            })?;
            let lexical_token = lexical_text.binary_search(&value).map_err(|_| {
                filter_index_corrupt(format!(
                    "text filter column {column_idx} value is missing from its dictionary"
                ))
            })?;
            let lexical_token = u32::try_from(lexical_token)
                .map_err(|_| GraphError::Internal("filter dictionary exceeds u32".into()))?;
            output.copy_from_slice(&lexical_token.to_le_bytes());
        }
    }
    Ok(())
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

fn allocation_error(_error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
    }
}

fn hash_map_allocation_upper_bound<K, V>(map: &HashMap<K, V>) -> usize {
    if map.capacity() == 0 {
        return 0;
    }
    // Hashbrown stores one control byte per bucket plus a fixed SIMD tail.
    // Charging two control bytes and one 16-byte tail is conservative across
    // the supported Rust toolchains while still reflecting real capacity.
    map.capacity()
        .saturating_mul(std::mem::size_of::<(K, V)>().saturating_add(2))
        .saturating_add(16)
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
    #[cfg(test)]
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

    fn mapped_text_fixture(dictionary_values: &[&str]) -> GraphResult<FilterIndex> {
        let mut dictionary = vec![0u8; (dictionary_values.len() + 1) * 8];
        let mut string_bytes = Vec::new();
        for (idx, value) in dictionary_values.iter().enumerate() {
            string_bytes.extend_from_slice(value.as_bytes());
            dictionary[(idx + 1) * 8..(idx + 2) * 8]
                .copy_from_slice(&(string_bytes.len() as u64).to_le_bytes());
        }
        dictionary.extend_from_slice(&string_bytes);
        let mut data = Vec::new();
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        let name = b"status";
        let mut catalog = vec![0u8; FILTER_CATALOG_HEADER_SIZE + FILTER_DESCRIPTOR_SIZE];
        catalog[0..4].copy_from_slice(&1u32.to_le_bytes());
        catalog[4..8].copy_from_slice(&(FILTER_DESCRIPTOR_SIZE as u32).to_le_bytes());
        catalog[8..16].copy_from_slice(&(name.len() as u64).to_le_bytes());
        let descriptor = &mut catalog[16..80];
        descriptor[0..4].copy_from_slice(&100u32.to_le_bytes());
        descriptor[4] = FilterColumnType::Text.persisted_tag();
        descriptor[5] = PersistedStorageKind::SparseLookup as u8;
        descriptor[6] = 4;
        descriptor[16..20].copy_from_slice(&(name.len() as u32).to_le_bytes());
        descriptor[20..24].copy_from_slice(&1u32.to_le_bytes());
        descriptor[32..40].copy_from_slice(&(data.len() as u64).to_le_bytes());
        descriptor[48..56].copy_from_slice(&(dictionary.len() as u64).to_le_bytes());
        descriptor[56..60].copy_from_slice(&(dictionary_values.len() as u32).to_le_bytes());
        catalog.extend_from_slice(name);
        let catalog_range = 0..catalog.len();
        let data_range = catalog_range.end..catalog_range.end + data.len();
        let dictionary_range = data_range.end..data_range.end + dictionary.len();
        let mut bytes = catalog;
        bytes.extend_from_slice(&data);
        bytes.extend_from_slice(&dictionary);
        FilterIndex::from_mapped_sections(
            MappedBytes::from_test_bytes(bytes),
            catalog_range,
            data_range,
            dictionary_range,
            4,
        )
    }

    #[test]
    fn mapped_text_base_uses_lexical_dictionary_without_heap_materialization() {
        let index = mapped_text_fixture(&["closed", "open"]).expect("mapped filter fixture");

        assert_eq!(index.lookup_text_value(0, "closed"), Some(0));
        assert_eq!(index.lookup_text_value(0, "open"), Some(1));
        assert_eq!(index.text_value(0, 1), Some("open"));
        assert!(index.check_filter(2, &FilterOp::new(0, FilterCondition::EqToken(1))));
        assert!(index.check_filter(1, &FilterOp::new(0, FilterCondition::IsNull)));
    }

    #[test]
    fn mapped_filter_delta_overrides_null_and_clones_without_sharing_mutation() {
        let mut index = mapped_text_fixture(&["closed", "open"]).expect("mapped filter fixture");
        let pending = index.intern_text_value(0, "pending").unwrap();
        index.set_encoded_value(0, 2, None);
        index.set_encoded_value(0, 3, Some(EncodedFilterValue::Text(pending)));
        let mut clone = index.clone();
        clone.set_encoded_value(0, 3, None);

        assert_eq!(pending, 2);
        assert!(index.check_filter(2, &FilterOp::new(0, FilterCondition::IsNull)));
        assert!(index.check_filter(3, &FilterOp::new(0, FilterCondition::EqToken(2))));
        assert!(clone.check_filter(3, &FilterOp::new(0, FilterCondition::IsNull)));
    }

    #[test]
    fn mapped_filter_rejects_nonlexical_dictionary() {
        assert!(matches!(
            mapped_text_fixture(&["open", "closed"]),
            Err(GraphError::CorruptFile { .. })
        ));
    }

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
        let open = fi.intern_text_value(col, "open").unwrap();
        let closed = fi.intern_text_value(col, "closed").unwrap();
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
