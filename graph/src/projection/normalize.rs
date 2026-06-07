//! Deterministic committed-mutation normalization for durable segments.
//!
//! Normalization is the boundary between committed sync-log rows and immutable
//! segment contents. It sorts rows into a stable order, cancels net-neutral
//! insert/delete pairs, preserves delete precedence for conflicting rows, and
//! enforces bounded ingestion buffers before segment writers see data.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::safety::{GraphError, GraphResult};
use crate::types::TraversalDirection;

const NORMALIZED_ROW_BYTES: usize = 40;

/// Committed mutation operation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MutationOperation {
    /// Insert or reactivate an edge.
    InsertEdge,
    /// Delete an edge.
    DeleteEdge,
    /// Insert or reactivate a node.
    UpsertNode,
    /// Delete or tombstone a node.
    DeleteNode,
}

impl MutationOperation {
    pub(crate) fn is_insert(self) -> bool {
        matches!(self, Self::InsertEdge | Self::UpsertNode)
    }

    pub(crate) fn is_delete(self) -> bool {
        matches!(self, Self::DeleteEdge | Self::DeleteNode)
    }

    pub(crate) fn is_edge(self) -> bool {
        matches!(self, Self::InsertEdge | Self::DeleteEdge)
    }
}

/// Raw committed mutation row ready for normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommittedMutation {
    /// Sync-log identifier.
    pub(crate) sync_id: u64,
    /// Projection generation that will own this mutation.
    pub(crate) generation_id: u64,
    /// Traversal direction for edge deltas.
    pub(crate) direction: TraversalDirection,
    /// Source node index.
    pub(crate) source: u32,
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: u8,
    /// Optional edge weight.
    pub(crate) weight: Option<u32>,
    /// Operation kind.
    pub(crate) operation: MutationOperation,
}

/// Normalized mutation consumed by durable segment writers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedMutation {
    /// Projection generation that owns this mutation.
    pub(crate) generation_id: u64,
    /// Latest sync-log row represented by this normalized mutation.
    pub(crate) sync_id: u64,
    /// Traversal direction for edge deltas.
    pub(crate) direction: TraversalDirection,
    /// Source node index.
    pub(crate) source: u32,
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: u8,
    /// Optional edge weight.
    pub(crate) weight: Option<u32>,
    /// Operation represented by this normalized row.
    pub(crate) operation: MutationOperation,
    /// Whether this mutation is a tombstone.
    pub(crate) tombstone: bool,
}

/// Normalized mutation batch plus ingestion-buffer accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedMutationBatch {
    /// Normalized rows in deterministic order.
    pub(crate) rows: Vec<NormalizedMutation>,
    /// Estimated bytes used by the normalized rows.
    pub(crate) estimated_bytes: usize,
}

/// Bounded ingestion buffer for committed projection mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MutationBufferLimits {
    /// Maximum rows allowed in the normalization buffer.
    pub(crate) max_rows: usize,
    /// Maximum estimated bytes allowed in the normalization buffer.
    pub(crate) max_bytes: usize,
}

impl MutationBufferLimits {
    /// Construct row/byte limits.
    pub(crate) fn new(max_rows: usize, max_bytes: usize) -> Self {
        Self {
            max_rows,
            max_bytes,
        }
    }
}

/// Normalize committed mutation rows into deterministic segment input.
///
/// # Errors
///
/// Returns [`GraphError::OverlayLimit`] when the row or byte limits are
/// exceeded.
pub(crate) fn normalize_committed_mutations(
    rows: &[CommittedMutation],
    limits: MutationBufferLimits,
) -> GraphResult<NormalizedMutationBatch> {
    validate_limits(rows.len(), limits)?;
    let mut groups: BTreeMap<MutationKey, Vec<CommittedMutation>> = BTreeMap::new();
    for row in rows {
        groups
            .entry(MutationKey::from(row))
            .or_default()
            .push(row.clone());
    }

    let mut normalized = Vec::with_capacity(groups.len());
    for group in groups.values() {
        if let Some(row) = normalize_group(group) {
            normalized.push(row);
        }
    }
    normalized.sort_by(compare_normalized_mutations);

    Ok(NormalizedMutationBatch {
        estimated_bytes: normalized
            .len()
            .checked_mul(NORMALIZED_ROW_BYTES)
            .ok_or_else(|| {
                GraphError::Internal("normalized mutation byte estimate overflowed".into())
            })?,
        rows: normalized,
    })
}

fn validate_limits(row_count: usize, limits: MutationBufferLimits) -> GraphResult<()> {
    if row_count > limits.max_rows {
        return Err(GraphError::OverlayLimit {
            kind: "projection_ingest_rows".to_string(),
            requested: row_count,
            limit: limits.max_rows,
        });
    }
    let requested_bytes = row_count
        .checked_mul(NORMALIZED_ROW_BYTES)
        .ok_or_else(|| GraphError::Internal("projection ingest byte estimate overflowed".into()))?;
    if requested_bytes > limits.max_bytes {
        return Err(GraphError::OverlayLimit {
            kind: "projection_ingest_bytes".to_string(),
            requested: requested_bytes,
            limit: limits.max_bytes,
        });
    }
    Ok(())
}

fn normalize_group(group: &[CommittedMutation]) -> Option<NormalizedMutation> {
    let has_insert = group.iter().any(|row| row.operation.is_insert());
    let has_delete = group.iter().any(|row| row.operation.is_delete());
    if has_insert && has_delete && cancels_pair(group) {
        return None;
    }
    let selected = group
        .iter()
        .max_by(|left, right| compare_precedence(left, right))?;
    Some(NormalizedMutation {
        generation_id: selected.generation_id,
        sync_id: selected.sync_id,
        direction: selected.direction,
        source: selected.source,
        target: selected.target,
        type_id: selected.type_id,
        weight: selected.weight,
        operation: selected.operation,
        tombstone: selected.operation.is_delete(),
    })
}

fn cancels_pair(group: &[CommittedMutation]) -> bool {
    group.len() == 2
        && group.iter().any(|row| row.operation.is_insert())
        && group.iter().any(|row| row.operation.is_delete())
}

fn compare_precedence(left: &CommittedMutation, right: &CommittedMutation) -> Ordering {
    left.operation
        .is_delete()
        .cmp(&right.operation.is_delete())
        .then_with(|| left.sync_id.cmp(&right.sync_id))
        .then_with(|| left.operation.cmp(&right.operation))
        .then_with(|| left.weight.cmp(&right.weight))
}

fn compare_normalized_mutations(left: &NormalizedMutation, right: &NormalizedMutation) -> Ordering {
    (
        left.generation_id,
        left.sync_id,
        left.source,
        direction_sort_key(left.direction),
        left.type_id,
        left.target,
        left.operation,
        left.tombstone,
    )
        .cmp(&(
            right.generation_id,
            right.sync_id,
            right.source,
            direction_sort_key(right.direction),
            right.type_id,
            right.target,
            right.operation,
            right.tombstone,
        ))
}

fn direction_sort_key(direction: TraversalDirection) -> u8 {
    match direction {
        TraversalDirection::Any => 0,
        TraversalDirection::Out => 1,
        TraversalDirection::In => 2,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MutationKey {
    generation_id: u64,
    entity_kind: MutationEntityKind,
    direction: TraversalDirection,
    source: u32,
    target: u32,
    type_id: u8,
}

impl PartialOrd for MutationKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MutationKey {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            self.generation_id,
            self.entity_kind,
            direction_sort_key(self.direction),
            self.source,
            self.target,
            self.type_id,
        )
            .cmp(&(
                other.generation_id,
                other.entity_kind,
                direction_sort_key(other.direction),
                other.source,
                other.target,
                other.type_id,
            ))
    }
}

impl From<&CommittedMutation> for MutationKey {
    fn from(row: &CommittedMutation) -> Self {
        Self {
            generation_id: row.generation_id,
            entity_kind: MutationEntityKind::from(row.operation),
            direction: row.direction,
            source: row.source,
            target: row.target,
            type_id: row.type_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MutationEntityKind {
    Edge,
    Node,
}

impl From<MutationOperation> for MutationEntityKind {
    fn from(operation: MutationOperation) -> Self {
        if operation.is_edge() {
            Self::Edge
        } else {
            Self::Node
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn delta_segment_normalization_is_deterministic() {
        let rows = vec![
            mutation(3, 1, 2, 1, MutationOperation::InsertEdge),
            mutation(1, 1, 1, 1, MutationOperation::InsertEdge),
            mutation(2, 1, 1, 2, MutationOperation::InsertEdge),
        ];
        let mut reversed = rows.clone();
        reversed.reverse();

        let left = normalize_committed_mutations(&rows, limits()).expect("rows normalize");
        let right = normalize_committed_mutations(&reversed, limits()).expect("rows normalize");

        assert_eq!(left, right);
        assert_eq!(
            left.rows.iter().map(|row| row.sync_id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn delta_segment_normalization_cancels_insert_delete_pairs() {
        let rows = vec![
            mutation(1, 1, 1, 2, MutationOperation::InsertEdge),
            mutation(2, 1, 1, 2, MutationOperation::DeleteEdge),
        ];

        let normalized = normalize_committed_mutations(&rows, limits()).expect("rows normalize");

        assert!(normalized.rows.is_empty());
    }

    #[test]
    fn delta_segment_normalization_preserves_delete_precedence() {
        let rows = vec![
            mutation(1, 1, 1, 2, MutationOperation::InsertEdge),
            mutation(2, 1, 1, 2, MutationOperation::InsertEdge),
            mutation(3, 1, 1, 2, MutationOperation::DeleteEdge),
        ];

        let normalized = normalize_committed_mutations(&rows, limits()).expect("rows normalize");

        assert_eq!(normalized.rows.len(), 1);
        assert!(normalized.rows[0].tombstone);
        assert_eq!(normalized.rows[0].sync_id, 3);
    }

    #[test]
    fn delta_segment_normalization_groups_direction_and_edge_type() {
        let rows = vec![
            mutation(1, 1, 1, 2, MutationOperation::InsertEdge),
            CommittedMutation {
                direction: TraversalDirection::In,
                ..mutation(2, 1, 1, 2, MutationOperation::InsertEdge)
            },
            CommittedMutation {
                type_id: 3,
                ..mutation(3, 1, 1, 2, MutationOperation::InsertEdge)
            },
        ];

        let normalized = normalize_committed_mutations(&rows, limits()).expect("rows normalize");

        assert_eq!(normalized.rows.len(), 3);
        assert_eq!(normalized.rows[0].direction, TraversalDirection::Out);
        assert_eq!(normalized.rows[1].direction, TraversalDirection::In);
        assert_eq!(normalized.rows[2].type_id, 3);
    }

    #[test]
    fn projection_ingest_buffer_limits_reject_oversized_batch() {
        let rows = vec![
            mutation(1, 1, 1, 2, MutationOperation::InsertEdge),
            mutation(2, 1, 2, 3, MutationOperation::InsertEdge),
        ];

        let row_err = normalize_committed_mutations(&rows, MutationBufferLimits::new(1, 1_000))
            .expect_err("row limit rejects");
        let byte_err = normalize_committed_mutations(&rows, MutationBufferLimits::new(10, 1))
            .expect_err("byte limit rejects");

        assert!(matches!(row_err, GraphError::OverlayLimit { .. }));
        assert!(matches!(byte_err, GraphError::OverlayLimit { .. }));
    }

    #[test]
    fn delta_segment_normalization_handles_node_operations() {
        let rows = vec![
            mutation(1, 1, 1, 1, MutationOperation::UpsertNode),
            mutation(2, 1, 1, 1, MutationOperation::DeleteNode),
            mutation(3, 1, 2, 2, MutationOperation::DeleteNode),
        ];

        let normalized = normalize_committed_mutations(&rows, limits()).expect("rows normalize");

        assert_eq!(normalized.rows.len(), 1);
        assert!(normalized.rows[0].tombstone);
        assert_eq!(normalized.rows[0].source, 2);
    }

    #[test]
    fn delta_segment_normalization_keeps_node_and_edge_domains_separate() {
        let rows = vec![
            mutation(1, 1, 1, 2, MutationOperation::InsertEdge),
            mutation(2, 1, 1, 2, MutationOperation::DeleteNode),
        ];

        let normalized = normalize_committed_mutations(&rows, limits()).expect("rows normalize");

        assert_eq!(normalized.rows.len(), 2);
        assert_eq!(normalized.rows[0].operation, MutationOperation::InsertEdge);
        assert_eq!(normalized.rows[1].operation, MutationOperation::DeleteNode);
    }

    #[test]
    fn delta_segment_normalization_ties_duplicate_sync_ids_deterministically() {
        let rows = vec![
            weighted_mutation(1, 1, 1, 2, Some(10), MutationOperation::InsertEdge),
            weighted_mutation(1, 1, 1, 2, Some(20), MutationOperation::InsertEdge),
        ];
        let mut reversed = rows.clone();
        reversed.reverse();

        let left = normalize_committed_mutations(&rows, limits()).expect("rows normalize");
        let right = normalize_committed_mutations(&reversed, limits()).expect("rows normalize");

        assert_eq!(left, right);
        assert_eq!(left.rows[0].weight, Some(20));
    }

    proptest! {
        #[test]
        fn normalization_proptest_is_deterministic(sync_ids in prop::collection::vec(1_u64..50, 0..16)) {
            let rows = sync_ids
                .iter()
                .copied()
                .enumerate()
                .map(|(idx, sync_id)| {
                    mutation(
                        sync_id,
                        1,
                        (idx % 4) as u32,
                        ((idx + 1) % 4) as u32,
                        if idx % 3 == 0 {
                            MutationOperation::DeleteEdge
                        } else {
                            MutationOperation::InsertEdge
                        },
                    )
                })
                .collect::<Vec<_>>();
            let mut reversed = rows.clone();
            reversed.reverse();

            let left = normalize_committed_mutations(&rows, limits()).expect("rows normalize");
            let right = normalize_committed_mutations(&reversed, limits()).expect("rows normalize");

            prop_assert_eq!(left, right);
        }
    }

    fn mutation(
        sync_id: u64,
        generation_id: u64,
        source: u32,
        target: u32,
        operation: MutationOperation,
    ) -> CommittedMutation {
        weighted_mutation(sync_id, generation_id, source, target, Some(10), operation)
    }

    fn weighted_mutation(
        sync_id: u64,
        generation_id: u64,
        source: u32,
        target: u32,
        weight: Option<u32>,
        operation: MutationOperation,
    ) -> CommittedMutation {
        CommittedMutation {
            sync_id,
            generation_id,
            direction: TraversalDirection::Out,
            source,
            target,
            type_id: 1,
            weight,
            operation,
        }
    }

    fn limits() -> MutationBufferLimits {
        MutationBufferLimits::new(1_000, 1_000_000)
    }
}
