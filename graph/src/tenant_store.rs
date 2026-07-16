//! Validated immutable tenant membership backed by mapped artifact bytes.
//!
//! The index stores one dense tenant token per graph node. Token zero denotes
//! no tenant; nonzero tokens address a strictly lexical UTF-8 dictionary. The
//! constructor validates both sections before exposing borrowed string views.

use std::cmp::Ordering;
use std::ops::Range;

use crate::mapped_bytes::MappedBytes;
use crate::safety::{GraphError, GraphResult};

const DICTIONARY_COUNT_BYTES: usize = std::mem::size_of::<u32>();
const DICTIONARY_OFFSET_BYTES: usize = std::mem::size_of::<u64>();
const TENANT_TOKEN_BYTES: usize = std::mem::size_of::<u32>();

/// Byte ranges used to construct a mapped tenant index.
#[derive(Clone, Debug)]
pub(crate) struct MappedTenantIndexParts {
    /// Read-only artifact bytes retained by the validated index.
    pub(crate) bytes: MappedBytes,
    /// Dictionary section: count, cumulative offsets, then UTF-8 bytes.
    pub(crate) dictionary_range: Range<usize>,
    /// Dense little-endian tenant tokens, one per graph node.
    pub(crate) tokens_range: Range<usize>,
}

/// Validated immutable tenant membership for one graph snapshot.
///
/// Dictionary entry ranges are the only heap metadata. They are allocated
/// fallibly after the serialized count and complete offset table have passed
/// bounds checks. Tenant strings and dense tokens remain in mapped storage.
#[derive(Clone, Debug)]
pub(crate) struct MappedTenantIndex {
    bytes: MappedBytes,
    tokens_range: Range<usize>,
    entries: Vec<Range<usize>>,
    node_count: u32,
}

impl MappedTenantIndex {
    /// Validate and adopt mapped tenant dictionary and token sections.
    ///
    /// The dictionary must be strictly lexical UTF-8 with an offset table that
    /// spans its string payload exactly. The token section must contain exactly
    /// `node_count` little-endian `u32` values in `0..=dictionary_count`.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] for an invalid range, section length,
    /// offset, UTF-8 value, dictionary order, or tenant token. Returns
    /// [`GraphError::Oom`] when bounded dictionary metadata cannot be allocated.
    pub(crate) fn new(parts: MappedTenantIndexParts, node_count: u32) -> GraphResult<Self> {
        validate_section_ranges(&parts)?;

        let expected_tokens_len = (node_count as usize)
            .checked_mul(TENANT_TOKEN_BYTES)
            .ok_or_else(|| corrupt("tenant token section length overflowed"))?;
        if parts.tokens_range.len() != expected_tokens_len {
            return Err(corrupt(
                "tenant token section does not contain exactly one token per node",
            ));
        }

        let dictionary = &parts.bytes.as_slice()[parts.dictionary_range.clone()];
        let count = read_u32(dictionary, 0)?;
        let count =
            usize::try_from(count).map_err(|_| corrupt("tenant dictionary count exceeds usize"))?;
        let offset_count = count
            .checked_add(1)
            .ok_or_else(|| corrupt("tenant dictionary offset count overflowed"))?;
        let offsets_bytes = offset_count
            .checked_mul(DICTIONARY_OFFSET_BYTES)
            .ok_or_else(|| corrupt("tenant dictionary offset table overflowed"))?;
        let strings_start = DICTIONARY_COUNT_BYTES
            .checked_add(offsets_bytes)
            .ok_or_else(|| corrupt("tenant dictionary header length overflowed"))?;
        let strings = dictionary
            .get(strings_start..)
            .ok_or_else(|| corrupt("tenant dictionary offset table is truncated"))?;

        if read_u64(dictionary, DICTIONARY_COUNT_BYTES)? != 0 {
            return Err(corrupt("tenant dictionary must begin at offset zero"));
        }
        let terminal_offset = DICTIONARY_COUNT_BYTES
            .checked_add(
                count
                    .checked_mul(DICTIONARY_OFFSET_BYTES)
                    .ok_or_else(|| corrupt("tenant dictionary terminal offset overflowed"))?,
            )
            .ok_or_else(|| corrupt("tenant dictionary terminal offset overflowed"))?;
        if usize::try_from(read_u64(dictionary, terminal_offset)?)
            .ok()
            .filter(|end| *end == strings.len())
            .is_none()
        {
            return Err(corrupt(
                "tenant dictionary terminal offset does not span its payload",
            ));
        }

        let mut entries = Vec::new();
        entries.try_reserve_exact(count).map_err(allocation_error)?;
        let mut previous_end = 0usize;
        let mut previous_value: Option<&str> = None;
        for token in 0..count {
            let start = read_dictionary_offset(dictionary, token)?;
            let end = read_dictionary_offset(dictionary, token + 1)?;
            if start != previous_end || start > end || end > strings.len() {
                return Err(corrupt(format!(
                    "tenant dictionary entry {} has invalid offsets",
                    token + 1
                )));
            }
            let value = std::str::from_utf8(&strings[start..end]).map_err(|_| {
                corrupt(format!(
                    "tenant dictionary entry {} is not UTF-8",
                    token + 1
                ))
            })?;
            if previous_value.is_some_and(|previous| previous >= value) {
                return Err(corrupt(format!(
                    "tenant dictionary entry {} is not strictly lexical",
                    token + 1
                )));
            }
            let absolute_start = parts
                .dictionary_range
                .start
                .checked_add(strings_start)
                .and_then(|offset| offset.checked_add(start))
                .ok_or_else(|| corrupt("tenant dictionary entry range overflowed"))?;
            let absolute_end = parts
                .dictionary_range
                .start
                .checked_add(strings_start)
                .and_then(|offset| offset.checked_add(end))
                .ok_or_else(|| corrupt("tenant dictionary entry range overflowed"))?;
            entries.push(absolute_start..absolute_end);
            previous_end = end;
            previous_value = Some(value);
        }

        let tokens = &parts.bytes.as_slice()[parts.tokens_range.clone()];
        let dictionary_count =
            u32::try_from(count).map_err(|_| corrupt("tenant dictionary count exceeds u32"))?;
        for (node_idx, token_bytes) in tokens.chunks_exact(TENANT_TOKEN_BYTES).enumerate() {
            let token = read_u32(token_bytes, 0)?;
            if token > dictionary_count {
                return Err(corrupt(format!(
                    "tenant token for node {node_idx} is outside the dictionary"
                )));
            }
        }

        Ok(Self {
            bytes: parts.bytes,
            tokens_range: parts.tokens_range,
            entries,
            node_count,
        })
    }

    /// Return whether `node_idx` belongs to `tenant`.
    pub(crate) fn contains(&self, tenant: &str, node_idx: u32) -> bool {
        let Some(expected_token) = self.token_for_tenant(tenant) else {
            return false;
        };
        self.token_for_node(node_idx) == Some(expected_token)
    }

    /// Return the tenant assigned to `node_idx`, or `None` for NULL/out-of-range.
    pub(crate) fn tenant_for_node(&self, node_idx: u32) -> Option<&str> {
        let token = self.token_for_node(node_idx)?;
        if token == 0 {
            return None;
        }
        let index = usize::try_from(token.checked_sub(1)?).ok()?;
        self.entry(index)
    }

    /// Return retained heap bytes for validated dictionary entry metadata.
    pub(crate) fn estimated_heap_bytes(&self) -> usize {
        self.entries
            .capacity()
            .saturating_mul(std::mem::size_of::<Range<usize>>())
    }

    fn token_for_tenant(&self, tenant: &str) -> Option<u32> {
        let mut low = 0usize;
        let mut high = self.entries.len();
        while low < high {
            let middle = low + (high - low) / 2;
            match self.entry(middle)?.cmp(tenant) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return u32::try_from(middle).ok()?.checked_add(1),
            }
        }
        None
    }

    fn token_for_node(&self, node_idx: u32) -> Option<u32> {
        if node_idx >= self.node_count {
            return None;
        }
        let relative = (node_idx as usize).checked_mul(TENANT_TOKEN_BYTES)?;
        let start = self.tokens_range.start.checked_add(relative)?;
        read_u32(self.bytes.as_slice(), start).ok()
    }

    fn entry(&self, index: usize) -> Option<&str> {
        let range = self.entries.get(index)?;
        std::str::from_utf8(&self.bytes.as_slice()[range.clone()]).ok()
    }
}

fn validate_section_ranges(parts: &MappedTenantIndexParts) -> GraphResult<()> {
    if !range_in_mapping(&parts.dictionary_range, parts.bytes.len())
        || !range_in_mapping(&parts.tokens_range, parts.bytes.len())
        || ranges_overlap(&parts.dictionary_range, &parts.tokens_range)
    {
        return Err(corrupt("invalid tenant index section layout"));
    }
    Ok(())
}

fn read_dictionary_offset(dictionary: &[u8], index: usize) -> GraphResult<usize> {
    let relative = index
        .checked_mul(DICTIONARY_OFFSET_BYTES)
        .and_then(|offset| offset.checked_add(DICTIONARY_COUNT_BYTES))
        .ok_or_else(|| corrupt("tenant dictionary offset position overflowed"))?;
    usize::try_from(read_u64(dictionary, relative)?)
        .map_err(|_| corrupt("tenant dictionary offset exceeds usize"))
}

fn read_u32(bytes: &[u8], offset: usize) -> GraphResult<u32> {
    let end = offset
        .checked_add(std::mem::size_of::<u32>())
        .ok_or_else(|| corrupt("tenant integer offset overflowed"))?;
    let raw: [u8; 4] = bytes
        .get(offset..end)
        .ok_or_else(|| corrupt("tenant u32 is truncated"))?
        .try_into()
        .map_err(|_| corrupt("tenant u32 is truncated"))?;
    Ok(u32::from_le_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> GraphResult<u64> {
    let end = offset
        .checked_add(std::mem::size_of::<u64>())
        .ok_or_else(|| corrupt("tenant integer offset overflowed"))?;
    let raw: [u8; 8] = bytes
        .get(offset..end)
        .ok_or_else(|| corrupt("tenant u64 is truncated"))?
        .try_into()
        .map_err(|_| corrupt("tenant u64 is truncated"))?;
    Ok(u64::from_le_bytes(raw))
}

fn range_in_mapping(range: &Range<usize>, mapping_len: usize) -> bool {
    range.start <= range.end && range.end <= mapping_len
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn allocation_error(_error: std::collections::TryReserveError) -> GraphError {
    GraphError::Oom {
        used_mb: 0,
        need_mb: 1,
        limit_mb: crate::config::MEMORY_LIMIT_MB.get().max(1) as u64,
    }
}

fn corrupt(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dictionary: &[&str], tokens: &[u32]) -> (MappedTenantIndexParts, u32) {
        let dictionary_len = DICTIONARY_COUNT_BYTES
            + (dictionary.len() + 1) * DICTIONARY_OFFSET_BYTES
            + dictionary.iter().map(|value| value.len()).sum::<usize>();
        let tokens_len = tokens.len() * TENANT_TOKEN_BYTES;
        let mut bytes = vec![0u8; dictionary_len + tokens_len];
        bytes[..4].copy_from_slice(&(dictionary.len() as u32).to_le_bytes());
        let mut cumulative = 0u64;
        for (index, value) in dictionary.iter().enumerate() {
            cumulative += value.len() as u64;
            let offset = DICTIONARY_COUNT_BYTES + (index + 1) * DICTIONARY_OFFSET_BYTES;
            bytes[offset..offset + 8].copy_from_slice(&cumulative.to_le_bytes());
        }
        let mut write = DICTIONARY_COUNT_BYTES + (dictionary.len() + 1) * DICTIONARY_OFFSET_BYTES;
        for value in dictionary {
            bytes[write..write + value.len()].copy_from_slice(value.as_bytes());
            write += value.len();
        }
        for (index, token) in tokens.iter().enumerate() {
            let start = dictionary_len + index * TENANT_TOKEN_BYTES;
            bytes[start..start + TENANT_TOKEN_BYTES].copy_from_slice(&token.to_le_bytes());
        }
        (
            MappedTenantIndexParts {
                bytes: MappedBytes::from_test_bytes(bytes),
                dictionary_range: 0..dictionary_len,
                tokens_range: dictionary_len..dictionary_len + tokens_len,
            },
            tokens.len() as u32,
        )
    }

    #[test]
    fn mapped_index_resolves_null_empty_and_nonempty_tenants() {
        let (parts, node_count) = fixture(&["", "alpha", "zeta"], &[0, 1, 2, 3]);
        let index = MappedTenantIndex::new(parts, node_count).expect("fixture validates");

        assert_eq!(index.tenant_for_node(0), None);
        assert_eq!(index.tenant_for_node(1), Some(""));
        assert_eq!(index.tenant_for_node(2), Some("alpha"));
        assert_eq!(index.tenant_for_node(3), Some("zeta"));
        assert_eq!(index.tenant_for_node(4), None);
        assert!(index.contains("", 1));
        assert!(index.contains("alpha", 2));
        assert!(!index.contains("alpha", 1));
        assert!(!index.contains("missing", 2));
        assert!(index.estimated_heap_bytes() >= 3 * std::mem::size_of::<Range<usize>>());
        assert_eq!(index.clone().tenant_for_node(3), Some("zeta"));
    }

    #[test]
    fn mapped_index_rejects_invalid_section_ranges_and_token_count() {
        let (mut overlapping, node_count) = fixture(&["a"], &[1]);
        overlapping.tokens_range = overlapping.dictionary_range.clone();
        assert!(MappedTenantIndex::new(overlapping, node_count).is_err());

        let (parts, _) = fixture(&["a"], &[1]);
        assert!(matches!(
            MappedTenantIndex::new(parts, 2),
            Err(GraphError::CorruptFile { reason }) if reason.contains("one token per node")
        ));
    }

    #[test]
    fn mapped_index_rejects_corrupt_dictionary_offsets() {
        let (mut parts, node_count) = fixture(&["alpha", "zeta"], &[1, 2]);
        let mut bytes = parts.bytes.as_slice().to_vec();
        bytes[DICTIONARY_COUNT_BYTES + DICTIONARY_OFFSET_BYTES
            ..DICTIONARY_COUNT_BYTES + 2 * DICTIONARY_OFFSET_BYTES]
            .copy_from_slice(&u64::MAX.to_le_bytes());
        parts.bytes = MappedBytes::from_test_bytes(bytes);

        assert!(matches!(
            MappedTenantIndex::new(parts, node_count),
            Err(GraphError::CorruptFile { reason }) if reason.contains("offset")
        ));
    }

    #[test]
    fn mapped_index_rejects_unknown_tokens() {
        let (parts, node_count) = fixture(&["alpha"], &[2]);
        assert!(matches!(
            MappedTenantIndex::new(parts, node_count),
            Err(GraphError::CorruptFile { reason }) if reason.contains("outside the dictionary")
        ));
    }

    #[test]
    fn mapped_index_rejects_nonlexical_and_duplicate_dictionary_values() {
        for dictionary in [&["zeta", "alpha"][..], &["alpha", "alpha"][..]] {
            let (parts, node_count) = fixture(dictionary, &[1]);
            assert!(matches!(
                MappedTenantIndex::new(parts, node_count),
                Err(GraphError::CorruptFile { reason }) if reason.contains("strictly lexical")
            ));
        }
    }

    #[test]
    fn mapped_index_rejects_invalid_utf8() {
        let (mut parts, node_count) = fixture(&["a"], &[1]);
        let mut bytes = parts.bytes.as_slice().to_vec();
        let string_start = DICTIONARY_COUNT_BYTES + 2 * DICTIONARY_OFFSET_BYTES;
        bytes[string_start] = 0xff;
        parts.bytes = MappedBytes::from_test_bytes(bytes);

        assert!(matches!(
            MappedTenantIndex::new(parts, node_count),
            Err(GraphError::CorruptFile { reason }) if reason.contains("not UTF-8")
        ));
    }

    #[test]
    fn empty_dictionary_accepts_only_null_tokens() {
        let (parts, node_count) = fixture(&[], &[0, 0]);
        let index = MappedTenantIndex::new(parts, node_count).expect("empty dictionary validates");
        assert_eq!(index.tenant_for_node(0), None);
        assert!(!index.contains("", 0));
    }
}
