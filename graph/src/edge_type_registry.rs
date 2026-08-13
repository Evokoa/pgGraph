//! Validated ordered relationship-type dictionary.

use crate::safety::{GraphError, GraphResult};
use crate::types::EdgeTypeId;
use std::collections::HashMap;

/// Ordered labels plus an O(1) label-to-logical-ID lookup.
#[derive(Debug, Clone)]
pub(crate) struct EdgeTypeRegistry {
    labels: Vec<String>,
    ids_by_label: HashMap<String, EdgeTypeId>,
}

impl EdgeTypeRegistry {
    const HASH_ENTRY_OVERHEAD_BYTES: usize = 32;

    pub(crate) fn new_v6() -> Self {
        Self {
            labels: vec![String::new()],
            ids_by_label: HashMap::new(),
        }
    }

    pub(crate) fn try_from_v6_labels(labels: Vec<String>) -> GraphResult<Self> {
        if labels.len() > EdgeTypeId::V6_MAX_USER_ID as usize + 1 {
            return Err(GraphError::CorruptFile {
                reason: "edge type registry count exceeds v6 limits".into(),
            });
        }
        Self::try_from_labels(labels)
    }

    pub(crate) fn try_from_labels(labels: Vec<String>) -> GraphResult<Self> {
        if labels.first().is_none_or(|label| !label.is_empty())
            || labels.len() > u32::MAX as usize
            || labels.iter().skip(1).any(String::is_empty)
        {
            return Err(GraphError::CorruptFile {
                reason: "edge type registry contains invalid labels".into(),
            });
        }
        let mut ids_by_label = HashMap::new();
        ids_by_label
            .try_reserve(labels.len().saturating_sub(1))
            .map_err(|error| {
                GraphError::Internal(format!(
                    "edge type registry lookup allocation failed: {error}"
                ))
            })?;
        for (index, label) in labels.iter().enumerate().skip(1) {
            let logical = EdgeTypeId::try_from(u32::try_from(index).map_err(|_| {
                GraphError::Internal("edge type registry index exceeds u32".into())
            })?)
            .map_err(|_| GraphError::CorruptFile {
                reason: "edge type registry uses the logical sentinel".into(),
            })?;
            let lookup_label = try_clone_label(label)?;
            if ids_by_label.insert(lookup_label, logical).is_some() {
                return Err(GraphError::CorruptFile {
                    reason: "edge type registry contains duplicate labels".into(),
                });
            }
        }
        Ok(Self {
            labels,
            ids_by_label,
        })
    }

    pub(crate) fn register_v6(&mut self, label: &str) -> GraphResult<EdgeTypeId> {
        if let Some(id) = self.id(label) {
            return Ok(id);
        }
        if label.is_empty() || self.labels.len() > EdgeTypeId::V6_MAX_USER_ID as usize {
            return Err(GraphError::EdgeTypeLimit);
        }
        let id =
            EdgeTypeId::try_from(u32::try_from(self.labels.len()).map_err(|_| {
                GraphError::Internal("edge type registry index exceeds u32".into())
            })?)
            .map_err(|_| GraphError::EdgeTypeLimit)?;
        self.labels.try_reserve(1).map_err(|error| {
            GraphError::Internal(format!("edge type registry allocation failed: {error}"))
        })?;
        self.ids_by_label.try_reserve(1).map_err(|error| {
            GraphError::Internal(format!("edge type lookup allocation failed: {error}"))
        })?;
        let ordered_label = try_clone_label(label)?;
        let lookup_label = try_clone_label(label)?;
        self.labels.push(ordered_label);
        self.ids_by_label.insert(lookup_label, id);
        Ok(id)
    }

    pub(crate) fn id(&self, label: &str) -> Option<EdgeTypeId> {
        if label.is_empty() {
            return Some(EdgeTypeId::UNTYPED);
        }
        self.ids_by_label.get(label).copied()
    }

    pub(crate) fn as_slice(&self) -> &[String] {
        &self.labels
    }

    pub(crate) fn into_labels(self) -> Vec<String> {
        self.labels
    }

    pub(crate) fn ordered_heap_bytes(&self) -> usize {
        self.labels.capacity() * std::mem::size_of::<String>()
            + self.labels.iter().map(String::capacity).sum::<usize>()
    }

    pub(crate) fn registration_heap_upper_bound(&self, label: &str) -> GraphResult<usize> {
        if self.id(label).is_some() {
            return Ok(0);
        }
        let next_labels =
            self.labels.len().checked_add(1).ok_or_else(|| {
                GraphError::Internal("edge type registry length overflowed".into())
            })?;
        let next_lookup = self
            .ids_by_label
            .len()
            .checked_add(1)
            .ok_or_else(|| GraphError::Internal("edge type lookup length overflowed".into()))?;
        let label_slots = collection_capacity_upper_bound(self.labels.capacity(), next_labels)?;
        let lookup_slots =
            collection_capacity_upper_bound(self.ids_by_label.capacity(), next_lookup)?;
        let target = label_slots
            .checked_mul(std::mem::size_of::<String>())
            .and_then(|bytes| {
                bytes.checked_add(self.labels.iter().map(String::capacity).sum::<usize>())
            })
            .and_then(|bytes| bytes.checked_add(label.len()))
            .and_then(|bytes| {
                bytes.checked_add(lookup_slots.checked_mul(
                    std::mem::size_of::<(String, EdgeTypeId)>() + Self::HASH_ENTRY_OVERHEAD_BYTES,
                )?)
            })
            .and_then(|bytes| {
                bytes.checked_add(
                    self.ids_by_label
                        .keys()
                        .map(String::capacity)
                        .sum::<usize>(),
                )
            })
            .and_then(|bytes| bytes.checked_add(label.len()))
            .ok_or_else(|| GraphError::Internal("edge type registry growth overflowed".into()))?;
        Ok(target.saturating_sub(self.heap_bytes()))
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.ordered_heap_bytes()
            + self.ids_by_label.capacity()
                * (std::mem::size_of::<(String, EdgeTypeId)>() + Self::HASH_ENTRY_OVERHEAD_BYTES)
            + self
                .ids_by_label
                .keys()
                .map(String::capacity)
                .sum::<usize>()
    }

    /// Conservative heap bound for decoding one validated v6 registry section.
    pub(crate) fn v6_load_metadata_upper_bound(encoded: &[u8]) -> GraphResult<usize> {
        let count = registry_encoded_count(encoded)?;
        if count > EdgeTypeId::V6_MAX_USER_ID as usize + 1 {
            return Err(GraphError::CorruptFile {
                reason: "edge type registry count exceeds v6 limits".into(),
            });
        }
        Self::load_metadata_upper_bound(encoded)
    }

    pub(crate) fn load_metadata_upper_bound(encoded: &[u8]) -> GraphResult<usize> {
        let count = registry_encoded_count(encoded)?;
        if count > u32::MAX as usize {
            return Err(GraphError::CorruptFile {
                reason: "edge type registry count exceeds logical limits".into(),
            });
        }
        let header_bytes = count
            .checked_add(1)
            .and_then(|offsets| offsets.checked_mul(8))
            .and_then(|offsets| offsets.checked_add(4))
            .ok_or_else(|| GraphError::CorruptFile {
                reason: "edge type registry header overflows".into(),
            })?;
        let payload_bytes =
            encoded
                .len()
                .checked_sub(header_bytes)
                .ok_or_else(|| GraphError::CorruptFile {
                    reason: "edge type registry offset table exceeds its section".into(),
                })?;
        let lookup_count = count.saturating_sub(1);
        // `HashMap` keeps spare buckets and one control byte per bucket. Two
        // times the next power of two is deliberately above its maximum load
        // factor for this v6-bounded dictionary.
        let lookup_slots = lookup_count
            .max(1)
            .checked_next_power_of_two()
            .and_then(|slots| slots.checked_mul(2))
            .ok_or_else(|| GraphError::Internal("edge type lookup bound overflowed".into()))?;
        count
            .checked_mul(std::mem::size_of::<String>())
            .and_then(|bytes| bytes.checked_add(payload_bytes.checked_mul(2)?))
            .and_then(|bytes| {
                bytes.checked_add(lookup_slots.checked_mul(
                    std::mem::size_of::<(String, EdgeTypeId)>() + Self::HASH_ENTRY_OVERHEAD_BYTES,
                )?)
            })
            .ok_or_else(|| GraphError::Internal("edge type metadata bound overflowed".into()))
    }
}

fn registry_encoded_count(encoded: &[u8]) -> GraphResult<usize> {
    let count_bytes: [u8; 4] = encoded
        .get(..4)
        .ok_or_else(|| GraphError::CorruptFile {
            reason: "edge type registry is too short".into(),
        })?
        .try_into()
        .map_err(|_| GraphError::CorruptFile {
            reason: "edge type registry count is malformed".into(),
        })?;
    let count = u32::from_le_bytes(count_bytes) as usize;
    if count == 0 {
        return Err(GraphError::CorruptFile {
            reason: "edge type registry must contain the untyped entry".into(),
        });
    }
    Ok(count)
}

fn collection_capacity_upper_bound(current: usize, required: usize) -> GraphResult<usize> {
    if current >= required {
        return Ok(current);
    }
    required
        .max(1)
        .checked_next_power_of_two()
        .and_then(|capacity| capacity.checked_mul(4))
        .ok_or_else(|| GraphError::Internal("edge type collection capacity overflowed".into()))
}

fn try_clone_label(label: &str) -> GraphResult<String> {
    let mut owned = String::new();
    owned.try_reserve_exact(label.len()).map_err(|error| {
        GraphError::Internal(format!("edge type label allocation failed: {error}"))
    })?;
    owned.push_str(label);
    Ok(owned)
}

impl std::ops::Deref for EdgeTypeRegistry {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_preserves_order_and_reuses_o1_ids() {
        let mut registry = EdgeTypeRegistry::new_v6();
        let before = registry.heap_bytes();
        let growth_bound = registry.registration_heap_upper_bound("friend").unwrap();
        let friend = registry.register_v6("friend").unwrap();
        assert!(registry.heap_bytes() <= before + growth_bound);
        assert_eq!(registry.registration_heap_upper_bound("friend").unwrap(), 0);
        assert_eq!(registry.register_v6("friend").unwrap(), friend);
        assert_eq!(registry.register_v6("works_at").unwrap().get(), 2);
        assert_eq!(registry.id("friend"), Some(friend));
        assert_eq!(registry.as_slice(), ["", "friend", "works_at"]);
    }

    #[test]
    fn registration_bound_covers_every_v6_capacity_transition() {
        let mut registry = EdgeTypeRegistry::new_v6();
        for index in 1..=EdgeTypeId::V6_MAX_USER_ID {
            let label = format!("type_{index}");
            let before = registry.heap_bytes();
            let growth = registry.registration_heap_upper_bound(&label).unwrap();
            registry.register_v6(&label).unwrap();
            assert!(
                registry.heap_bytes() <= before + growth,
                "registration {index} exceeded its preflight"
            );
        }
    }

    #[test]
    fn registry_rejects_invalid_loaded_labels_and_v6_overflow() {
        assert!(EdgeTypeRegistry::try_from_v6_labels(vec!["bad".into()]).is_err());
        assert!(
            EdgeTypeRegistry::try_from_v6_labels(vec!["".into(), "x".into(), "x".into()]).is_err()
        );
        let mut registry = EdgeTypeRegistry::new_v6();
        for index in 1..=EdgeTypeId::V6_MAX_USER_ID {
            assert_eq!(
                registry
                    .register_v6(&format!("type_{index}"))
                    .unwrap()
                    .get(),
                index
            );
        }
        assert!(matches!(
            registry.register_v6("overflow"),
            Err(GraphError::EdgeTypeLimit)
        ));
    }

    #[test]
    fn v6_load_bound_covers_short_label_lookup_storage() {
        let count = EdgeTypeId::V6_MAX_USER_ID as usize + 1;
        let labels = std::iter::once(String::new())
            .chain((1..count).map(|index| format!("t{index}")))
            .collect::<Vec<_>>();
        let payload_bytes = labels.iter().map(String::len).sum::<usize>();
        let encoded_len = 4 + (count + 1) * 8 + payload_bytes;
        let mut encoded = vec![0; encoded_len];
        encoded[..4].copy_from_slice(&(count as u32).to_le_bytes());
        let mut offset = 0u64;
        for (index, label) in labels.iter().enumerate() {
            encoded[4 + index * 8..12 + index * 8].copy_from_slice(&offset.to_le_bytes());
            offset += label.len() as u64;
        }
        encoded[4 + count * 8..12 + count * 8].copy_from_slice(&offset.to_le_bytes());
        let registry = EdgeTypeRegistry::try_from_v6_labels(labels).unwrap();
        let bound = EdgeTypeRegistry::v6_load_metadata_upper_bound(&encoded).unwrap();
        assert!(bound >= registry.heap_bytes());
    }
}
