//! Owned and mapped storage for durable relationship source identities.
//!
//! Persisted identities use fixed-width little-endian descriptors plus one
//! UTF-8 key-byte section. Mapped stores borrow those validated sections;
//! mutable identities added after load remain in a compact owned overlay until
//! an explicit materialization or rebuild.

use std::collections::HashSet;
use std::ops::Range;

use crate::edge_store::{RelationshipId, RelationshipIdentity};
use crate::mapped_bytes::MappedBytes;
use crate::safety::{GraphError, GraphResult};

const DESCRIPTOR_SIZE: usize = 16;

/// Borrowed view of one durable relationship source identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RelationshipIdentityRef<'a> {
    pub(crate) mapping_id: u64,
    pub(crate) source_key: &'a str,
}

impl RelationshipIdentityRef<'_> {
    /// Clone this view into the existing owned identity representation.
    pub(crate) fn to_owned(self) -> RelationshipIdentity {
        RelationshipIdentity {
            mapping_id: self.mapping_id,
            source_key: self.source_key.to_owned(),
        }
    }
}

impl<'a> From<&'a RelationshipIdentity> for RelationshipIdentityRef<'a> {
    fn from(identity: &'a RelationshipIdentity) -> Self {
        Self {
            mapping_id: identity.mapping_id,
            source_key: &identity.source_key,
        }
    }
}

/// Byte ranges for a mapped relationship-identity table.
#[derive(Clone, Debug)]
pub(crate) struct MappedRelationshipIdentityParts {
    /// Read-only mapping retained by the validated table.
    pub(crate) mmap: MappedBytes,
    /// Fixed 16-byte descriptors `(mapping_id, cumulative_key_end)`.
    pub(crate) descriptors_range: Range<usize>,
    /// Concatenated UTF-8 source keys addressed by the descriptors.
    pub(crate) key_bytes_range: Range<usize>,
}

/// Validated mapped relationship-identity descriptors and key bytes.
#[derive(Clone, Debug)]
pub(crate) struct MappedRelationshipIdentities {
    mmap: MappedBytes,
    descriptors_range: Range<usize>,
    key_bytes_range: Range<usize>,
    len: usize,
}

impl MappedRelationshipIdentities {
    /// Validate mapped descriptors and their UTF-8 key payload.
    ///
    /// Descriptor zero must be `(0, 0)`. Remaining descriptors must have a
    /// nonzero mapping ID and cumulative key ends within the key section.
    /// Descriptor order is preserved because relationship IDs remain stable
    /// across projection manifests. This boundary deliberately does not build
    /// a graph-sized duplicate index; owned construction and interning reject
    /// duplicates, while mapped lookup returns the first stable ID.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] when ranges, descriptors, ordering,
    /// or UTF-8 are invalid.
    pub(crate) fn new(parts: MappedRelationshipIdentityParts) -> GraphResult<Self> {
        if !range_in_mapping(&parts.descriptors_range, parts.mmap.len())
            || !range_in_mapping(&parts.key_bytes_range, parts.mmap.len())
            || ranges_overlap(&parts.descriptors_range, &parts.key_bytes_range)
            || parts.descriptors_range.len() < DESCRIPTOR_SIZE
            || !parts
                .descriptors_range
                .len()
                .is_multiple_of(DESCRIPTOR_SIZE)
        {
            return Err(corrupt("invalid relationship identity section layout"));
        }
        let len = parts.descriptors_range.len() / DESCRIPTOR_SIZE;
        if len > RelationshipId::MAX as usize {
            return Err(corrupt(
                "relationship identity descriptor count exceeds u32",
            ));
        }
        let descriptors = &parts.mmap.as_slice()[parts.descriptors_range.clone()];
        let key_bytes = &parts.mmap.as_slice()[parts.key_bytes_range.clone()];
        if descriptor_value(descriptors, 0)? != (0, 0) {
            return Err(corrupt(
                "relationship identity descriptor zero must be empty",
            ));
        }

        let mut previous_end = 0usize;
        let mut seen = HashSet::new();
        seen.try_reserve(len.saturating_sub(1))
            .map_err(allocation_error)?;
        for index in 1..len {
            let (mapping_id, key_end) = descriptor_value(descriptors, index)?;
            let key_end = usize::try_from(key_end)
                .map_err(|_| corrupt("relationship identity key end exceeds usize"))?;
            if mapping_id == 0 {
                return Err(corrupt(format!(
                    "relationship identity descriptor {index} has mapping ID 0"
                )));
            }
            if key_end < previous_end || key_end > key_bytes.len() {
                return Err(corrupt(format!(
                    "relationship identity descriptor {index} is invalid"
                )));
            }
            let source_key = std::str::from_utf8(&key_bytes[previous_end..key_end])
                .map_err(|_| corrupt(format!("relationship identity key {index} is not UTF-8")))?;
            if !seen.insert((mapping_id, source_key)) {
                return Err(corrupt(format!(
                    "relationship identity descriptor {index} duplicates an earlier identity"
                )));
            }
            previous_end = key_end;
        }
        if previous_end != key_bytes.len() {
            return Err(corrupt(
                "relationship identity descriptors do not span the key section",
            ));
        }

        Ok(Self {
            mmap: parts.mmap,
            descriptors_range: parts.descriptors_range,
            key_bytes_range: parts.key_bytes_range,
            len,
        })
    }

    fn get(&self, id: RelationshipId) -> Option<RelationshipIdentityRef<'_>> {
        let index = id as usize;
        if index == 0 || index >= self.len {
            return None;
        }
        let descriptors = &self.mmap.as_slice()[self.descriptors_range.clone()];
        let key_bytes = &self.mmap.as_slice()[self.key_bytes_range.clone()];
        let (mapping_id, key_end) = descriptor_value(descriptors, index).ok()?;
        let (_, key_start) = descriptor_value(descriptors, index - 1).ok()?;
        let key_start = usize::try_from(key_start).ok()?;
        let key_end = usize::try_from(key_end).ok()?;
        let source_key = std::str::from_utf8(key_bytes.get(key_start..key_end)?).ok()?;
        Some(RelationshipIdentityRef {
            mapping_id,
            source_key,
        })
    }

    fn find_id(&self, mapping_id: u64, source_key: &str) -> Option<RelationshipId> {
        (1..self.len).find_map(|index| {
            let id = RelationshipId::try_from(index).ok()?;
            let identity = self.get(id)?;
            (identity.mapping_id == mapping_id && identity.source_key == source_key).then_some(id)
        })
    }

    fn mapped_bytes(&self) -> usize {
        self.descriptors_range
            .len()
            .saturating_add(self.key_bytes_range.len())
    }
}

/// Relationship identities in build-time, mapped, or mapped-plus-overlay form.
#[derive(Clone, Debug)]
pub(crate) enum RelationshipIdentityStore {
    /// Fully owned dense identity vector with reserved empty slot zero.
    Owned(Vec<Option<RelationshipIdentity>>),
    /// Immutable identities borrowed from a validated artifact.
    Mapped(MappedRelationshipIdentities),
    /// Immutable mapped base plus dense identities added since that build.
    Layered {
        base: MappedRelationshipIdentities,
        overlay: Vec<RelationshipIdentity>,
    },
}

impl RelationshipIdentityStore {
    /// Validate and adopt a dense owned identity vector.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] for a missing reserved slot, empty
    /// nonzero slot, zero mapping ID, or duplicate identity. Returns
    /// [`GraphError::Oom`] if the validation index cannot be allocated.
    pub(crate) fn try_from_owned(
        identities: Vec<Option<RelationshipIdentity>>,
    ) -> GraphResult<Self> {
        if identities.first().is_none_or(Option::is_some) {
            return Err(corrupt(
                "relationship identity store must reserve empty slot zero",
            ));
        }
        if identities.len() > RelationshipId::MAX as usize {
            return Err(corrupt("relationship identity count exceeds u32"));
        }
        let mut seen = HashSet::new();
        seen.try_reserve(identities.len().saturating_sub(1))
            .map_err(allocation_error)?;
        for (index, identity) in identities.iter().enumerate().skip(1) {
            let identity = identity
                .as_ref()
                .ok_or_else(|| corrupt(format!("relationship identity slot {index} is empty")))?;
            if identity.mapping_id == 0 || !seen.insert(identity) {
                return Err(corrupt(format!(
                    "relationship identity slot {index} is invalid or duplicated"
                )));
            }
        }
        Ok(Self::Owned(identities))
    }

    /// Return the number of identity slots, including reserved slot zero.
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Owned(identities) => identities.len(),
            Self::Mapped(mapped) => mapped.len,
            Self::Layered { base, overlay } => base.len.saturating_add(overlay.len()),
        }
    }

    /// Borrow the dense owned slots after an explicit materialization.
    pub(crate) fn as_owned_slice(&self) -> Option<&[Option<RelationshipIdentity>]> {
        match self {
            Self::Owned(identities) => Some(identities),
            Self::Mapped(_) | Self::Layered { .. } => None,
        }
    }

    /// Borrow one nonzero relationship identity.
    pub(crate) fn get(&self, id: RelationshipId) -> Option<RelationshipIdentityRef<'_>> {
        let index = id as usize;
        match self {
            Self::Owned(identities) => identities
                .get(index)
                .and_then(Option::as_ref)
                .map(RelationshipIdentityRef::from),
            Self::Mapped(mapped) => mapped.get(id),
            Self::Layered { base, overlay } => {
                if index < base.len {
                    base.get(id)
                } else {
                    overlay
                        .get(index - base.len)
                        .map(RelationshipIdentityRef::from)
                }
            }
        }
    }

    /// Iterate over every slot in relationship-ID order.
    pub(crate) fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = Option<RelationshipIdentityRef<'_>>> + '_ {
        (0..self.len()).map(|index| self.get(index as RelationshipId))
    }

    /// Find a relationship ID by canonical PostgreSQL source identity.
    pub(crate) fn find_id(&self, mapping_id: u64, source_key: &str) -> Option<RelationshipId> {
        match self {
            Self::Owned(identities) => identities
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, identity)| {
                    identity.as_ref().is_some_and(|identity| {
                        identity.mapping_id == mapping_id && identity.source_key == source_key
                    })
                })
                .and_then(|(index, _)| RelationshipId::try_from(index).ok()),
            Self::Mapped(mapped) => mapped.find_id(mapping_id, source_key),
            Self::Layered { base, overlay } => base.find_id(mapping_id, source_key).or_else(|| {
                overlay
                    .iter()
                    .position(|identity| {
                        identity.mapping_id == mapping_id && identity.source_key == source_key
                    })
                    .and_then(|offset| base.len.checked_add(offset))
                    .and_then(|index| RelationshipId::try_from(index).ok())
            }),
        }
    }

    /// Return an existing ID or append one identity to mutable owned storage.
    ///
    /// A mapped store becomes layered on its first new identity. Mapped base
    /// bytes are never copied by this operation.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] for mapping ID zero, an internal
    /// error if the ID space is exhausted, or [`GraphError::Oom`] if the owned
    /// vector cannot grow.
    pub(crate) fn push_or_intern(
        &mut self,
        identity: RelationshipIdentity,
    ) -> GraphResult<RelationshipId> {
        if identity.mapping_id == 0 {
            return Err(corrupt("relationship identity mapping ID must be nonzero"));
        }
        if let Some(id) = self.find_id(identity.mapping_id, &identity.source_key) {
            return Ok(id);
        }
        let id = RelationshipId::try_from(self.len())
            .map_err(|_| GraphError::Internal("relationship identity ID space exhausted".into()))?;
        match self {
            Self::Owned(identities) => {
                identities.try_reserve(1).map_err(allocation_error)?;
                identities.push(Some(identity));
            }
            Self::Mapped(base) => {
                let base = base.clone();
                let mut overlay = Vec::new();
                overlay.try_reserve(1).map_err(allocation_error)?;
                overlay.push(identity);
                *self = Self::Layered { base, overlay };
            }
            Self::Layered { overlay, .. } => {
                overlay.try_reserve(1).map_err(allocation_error)?;
                overlay.push(identity);
            }
        }
        Ok(id)
    }

    /// Copy mapped identities into the dense owned representation.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Oom`] if the destination or a source-key clone
    /// cannot be allocated.
    pub(crate) fn materialize(&mut self) -> GraphResult<()> {
        if matches!(self, Self::Owned(_)) {
            return Ok(());
        }
        let mut identities = Vec::new();
        identities
            .try_reserve_exact(self.len())
            .map_err(allocation_error)?;
        for identity in self.iter() {
            identities.push(match identity {
                None => None,
                Some(identity) => Some(try_clone_identity(identity)?),
            });
        }
        *self = Self::Owned(identities);
        Ok(())
    }

    /// Install a validated cumulative projection dictionary without copying a
    /// mapped base.
    ///
    /// The supplied vector must begin with every currently visible identity in
    /// the same relationship-ID order. Only the suffix beyond a mapped base is
    /// retained as owned overlay state.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CorruptFile`] when the dictionary does not extend
    /// this store, and [`GraphError::Oom`] if the replacement overlay cannot be
    /// allocated.
    pub(crate) fn install_cumulative_owned(
        &mut self,
        identities: &[Option<RelationshipIdentity>],
    ) -> GraphResult<()> {
        if identities.len() < self.len()
            || self.iter().enumerate().any(|(index, current)| {
                current.map(|identity| (identity.mapping_id, identity.source_key))
                    != identities[index]
                        .as_ref()
                        .map(|identity| (identity.mapping_id, identity.source_key.as_str()))
            })
        {
            return Err(corrupt(
                "relationship identity dictionary does not extend the active store",
            ));
        }

        match self {
            Self::Owned(_) => {
                *self = Self::try_from_owned(try_clone_owned_slots(identities)?)?;
            }
            Self::Mapped(base) => {
                let base = base.clone();
                let overlay = try_clone_dense_suffix(identities, base.len)?;
                if overlay.is_empty() {
                    *self = Self::Mapped(base);
                } else {
                    *self = Self::Layered { base, overlay };
                }
            }
            Self::Layered { base, .. } => {
                let base = base.clone();
                let overlay = try_clone_dense_suffix(identities, base.len)?;
                if overlay.is_empty() {
                    *self = Self::Mapped(base);
                } else {
                    *self = Self::Layered { base, overlay };
                }
            }
        }
        Ok(())
    }

    /// Estimate heap bytes retained by owned identity vectors and strings.
    pub(crate) fn estimated_heap_bytes(&self) -> usize {
        match self {
            Self::Owned(identities) => identity_vec_heap_bytes(identities.capacity(), identities),
            Self::Mapped(_) => 0,
            Self::Layered { overlay, .. } => overlay
                .capacity()
                .saturating_mul(std::mem::size_of::<RelationshipIdentity>())
                .saturating_add(
                    overlay
                        .iter()
                        .map(|identity| identity.source_key.capacity())
                        .sum::<usize>(),
                ),
        }
    }

    /// Estimate bytes viewed from immutable mapped identity sections.
    pub(crate) fn estimated_mmap_bytes(&self) -> usize {
        match self {
            Self::Owned(_) => 0,
            Self::Mapped(mapped) | Self::Layered { base: mapped, .. } => mapped.mapped_bytes(),
        }
    }
}

impl Default for RelationshipIdentityStore {
    fn default() -> Self {
        Self::Owned(vec![None])
    }
}

fn descriptor_value(descriptors: &[u8], index: usize) -> GraphResult<(u64, u64)> {
    let start = index
        .checked_mul(DESCRIPTOR_SIZE)
        .ok_or_else(|| corrupt("relationship identity descriptor offset overflowed"))?;
    let end = start
        .checked_add(DESCRIPTOR_SIZE)
        .ok_or_else(|| corrupt("relationship identity descriptor end overflowed"))?;
    let descriptor = descriptors
        .get(start..end)
        .ok_or_else(|| corrupt("relationship identity descriptor is out of bounds"))?;
    let mapping_id = u64::from_le_bytes(
        descriptor[0..8]
            .try_into()
            .map_err(|_| corrupt("invalid relationship identity mapping ID"))?,
    );
    let key_end = u64::from_le_bytes(
        descriptor[8..16]
            .try_into()
            .map_err(|_| corrupt("invalid relationship identity key end"))?,
    );
    Ok((mapping_id, key_end))
}

fn try_clone_identity(identity: RelationshipIdentityRef<'_>) -> GraphResult<RelationshipIdentity> {
    let mut source_key = String::new();
    source_key
        .try_reserve_exact(identity.source_key.len())
        .map_err(allocation_error)?;
    source_key.push_str(identity.source_key);
    Ok(RelationshipIdentity {
        mapping_id: identity.mapping_id,
        source_key,
    })
}

fn try_clone_owned_slots(
    identities: &[Option<RelationshipIdentity>],
) -> GraphResult<Vec<Option<RelationshipIdentity>>> {
    let mut cloned = Vec::new();
    cloned
        .try_reserve_exact(identities.len())
        .map_err(allocation_error)?;
    for identity in identities {
        cloned.push(match identity {
            Some(identity) => Some(try_clone_identity(RelationshipIdentityRef::from(identity))?),
            None => None,
        });
    }
    Ok(cloned)
}

fn try_clone_dense_suffix(
    identities: &[Option<RelationshipIdentity>],
    start: usize,
) -> GraphResult<Vec<RelationshipIdentity>> {
    let suffix = identities
        .get(start..)
        .ok_or_else(|| corrupt("relationship identity suffix starts outside dictionary"))?;
    let mut overlay = Vec::new();
    overlay
        .try_reserve_exact(suffix.len())
        .map_err(allocation_error)?;
    for (offset, identity) in suffix.iter().enumerate() {
        let identity = identity.as_ref().ok_or_else(|| {
            corrupt(format!(
                "relationship identity overlay slot {} is empty",
                start.saturating_add(offset)
            ))
        })?;
        overlay.push(try_clone_identity(RelationshipIdentityRef::from(identity))?);
    }
    Ok(overlay)
}

fn identity_vec_heap_bytes(capacity: usize, identities: &[Option<RelationshipIdentity>]) -> usize {
    capacity
        .saturating_mul(std::mem::size_of::<Option<RelationshipIdentity>>())
        .saturating_add(
            identities
                .iter()
                .filter_map(Option::as_ref)
                .map(|identity| identity.source_key.capacity())
                .sum::<usize>(),
        )
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

    fn mapped_fixture(entries: &[(u64, &str)]) -> MappedRelationshipIdentities {
        let descriptor_len = (entries.len() + 1) * DESCRIPTOR_SIZE;
        let key_len = entries.iter().map(|(_, key)| key.len()).sum::<usize>();
        let key_start = descriptor_len;
        let mut bytes = vec![0u8; descriptor_len + key_len];
        let mut key_end = 0u64;
        for (index, (mapping_id, key)) in entries.iter().enumerate() {
            key_end += key.len() as u64;
            let start = (index + 1) * DESCRIPTOR_SIZE;
            bytes[start..start + 8].copy_from_slice(&mapping_id.to_le_bytes());
            bytes[start + 8..start + 16].copy_from_slice(&key_end.to_le_bytes());
        }
        let mut write = key_start;
        for (_, key) in entries {
            bytes[write..write + key.len()].copy_from_slice(key.as_bytes());
            write += key.len();
        }
        MappedRelationshipIdentities::new(MappedRelationshipIdentityParts {
            mmap: MappedBytes::from_test_bytes(bytes),
            descriptors_range: 0..descriptor_len,
            key_bytes_range: key_start..key_start + key_len,
        })
        .expect("fixture validates")
    }

    #[test]
    fn mapped_store_borrows_empty_and_nonempty_keys_and_finds_stable_ids() {
        let mapped = mapped_fixture(&[(1, ""), (1, "alpha"), (2, "z")]);
        let store = RelationshipIdentityStore::Mapped(mapped);

        assert_eq!(store.len(), 4);
        assert!(store.len() > 1);
        assert_eq!(store.get(0), None);
        assert_eq!(store.get(1).expect("empty key").source_key, "");
        assert_eq!(store.get(2).expect("alpha").mapping_id, 1);
        assert_eq!(store.find_id(2, "z"), Some(3));
        assert_eq!(store.find_id(2, "missing"), None);
        assert_eq!(store.iter().filter(Option::is_some).count(), 3);
        assert_eq!(store.estimated_heap_bytes(), 0);
        assert!(store.estimated_mmap_bytes() > 0);
    }

    #[test]
    fn mapped_validation_rejects_structural_and_identity_corruption() {
        let valid = mapped_fixture(&[(1, "a")]);
        let mmap = valid.mmap.clone();
        assert!(
            MappedRelationshipIdentities::new(MappedRelationshipIdentityParts {
                mmap: mmap.clone(),
                descriptors_range: 0..0,
                key_bytes_range: 32..33,
            })
            .is_err()
        );
        assert!(
            MappedRelationshipIdentities::new(MappedRelationshipIdentityParts {
                mmap: mmap.clone(),
                descriptors_range: 0..17,
                key_bytes_range: 32..33,
            })
            .is_err()
        );
        assert!(
            MappedRelationshipIdentities::new(MappedRelationshipIdentityParts {
                mmap,
                descriptors_range: 0..32,
                key_bytes_range: 16..17,
            })
            .is_err()
        );

        let unsorted = mapped_fixture_bytes(&[(2, "z"), (1, "a")]);
        let unsorted = MappedRelationshipIdentities::new(unsorted)
            .expect("stable descriptor order need not be canonical");
        assert_eq!(unsorted.find_id(1, "a"), Some(2));
        let duplicate = mapped_fixture_bytes(&[(1, "a"), (1, "a")]);
        assert!(matches!(
            MappedRelationshipIdentities::new(duplicate),
            Err(GraphError::CorruptFile { reason }) if reason.contains("duplicates")
        ));

        let mut invalid_utf8 = mapped_fixture_bytes(&[(1, "a")]);
        let mut bytes = invalid_utf8.mmap.as_slice().to_vec();
        bytes[32] = 0xff;
        invalid_utf8.mmap = MappedBytes::from_test_bytes(bytes);
        assert!(MappedRelationshipIdentities::new(invalid_utf8).is_err());
    }

    #[test]
    fn mapped_store_layers_new_identities_and_materializes_on_request() {
        let mapped = mapped_fixture(&[(1, "a"), (2, "b")]);
        let mut store = RelationshipIdentityStore::Mapped(mapped);
        assert_eq!(
            store
                .push_or_intern(RelationshipIdentity {
                    mapping_id: 1,
                    source_key: "a".into(),
                })
                .expect("existing identity"),
            1
        );
        assert_eq!(
            store
                .push_or_intern(RelationshipIdentity {
                    mapping_id: 3,
                    source_key: "c".into(),
                })
                .expect("new identity"),
            3
        );
        assert!(matches!(store, RelationshipIdentityStore::Layered { .. }));
        assert_eq!(store.get(3).expect("overlay identity").source_key, "c");
        assert!(store.estimated_heap_bytes() > 0);
        assert!(store.estimated_mmap_bytes() > 0);

        store.materialize().expect("materializes");
        assert!(matches!(store, RelationshipIdentityStore::Owned(_)));
        assert_eq!(store.find_id(2, "b"), Some(2));
        assert_eq!(store.find_id(3, "c"), Some(3));
        assert_eq!(store.estimated_mmap_bytes(), 0);
    }

    #[test]
    fn owned_store_rejects_holes_duplicates_and_zero_mapping_ids() {
        assert!(RelationshipIdentityStore::try_from_owned(Vec::new()).is_err());
        assert!(RelationshipIdentityStore::try_from_owned(vec![None, None]).is_err());
        let identity = RelationshipIdentity {
            mapping_id: 1,
            source_key: "edge".into(),
        };
        assert!(RelationshipIdentityStore::try_from_owned(vec![
            None,
            Some(identity.clone()),
            Some(identity),
        ])
        .is_err());
        assert!(RelationshipIdentityStore::try_from_owned(vec![
            None,
            Some(RelationshipIdentity {
                mapping_id: 0,
                source_key: "edge".into(),
            }),
        ])
        .is_err());
    }

    fn mapped_fixture_bytes(entries: &[(u64, &str)]) -> MappedRelationshipIdentityParts {
        let descriptor_len = (entries.len() + 1) * DESCRIPTOR_SIZE;
        let key_len = entries.iter().map(|(_, key)| key.len()).sum::<usize>();
        let mut bytes = vec![0u8; descriptor_len + key_len];
        let mut key_end = 0u64;
        for (index, (mapping_id, key)) in entries.iter().enumerate() {
            key_end += key.len() as u64;
            let start = (index + 1) * DESCRIPTOR_SIZE;
            bytes[start..start + 8].copy_from_slice(&mapping_id.to_le_bytes());
            bytes[start + 8..start + 16].copy_from_slice(&key_end.to_le_bytes());
        }
        let mut write = descriptor_len;
        for (_, key) in entries {
            bytes[write..write + key.len()].copy_from_slice(key.as_bytes());
            write += key.len();
        }
        MappedRelationshipIdentityParts {
            mmap: MappedBytes::from_test_bytes(bytes),
            descriptors_range: 0..descriptor_len,
            key_bytes_range: descriptor_len..descriptor_len + key_len,
        }
    }
}
