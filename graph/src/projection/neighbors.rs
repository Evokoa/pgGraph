//! Overlay-aware neighbor iteration for graph algorithms.
//!
//! The committed CSR remains the fast path. Pending sync and future mutable
//! projection deltas are layered as insert/delete maps without materializing a
//! per-node neighbor vector for clean reads.

use std::collections::{HashMap, HashSet};

use crate::edge_store::{EdgeStore, EdgeTypeSlice, RelationshipId, NO_RELATIONSHIP_ID};
use crate::types::EdgeTypeId;

/// Pending edge inserts keyed by source node.
pub(crate) type OverlayInsert = (u32, EdgeTypeId, bool, Option<RelationshipId>);
/// Pending edge inserts keyed by source node.
pub(crate) type OverlayInserts = HashMap<u32, Vec<OverlayInsert>>;
/// One pending edge tombstone. `relationship_id = None` is a legacy
/// topology-wide tombstone; identified tombstones remove only one source row.
pub(crate) type OverlayDelete = (u32, EdgeTypeId, bool, Option<RelationshipId>);
/// Pending edge deletes keyed by source node.
pub(crate) type OverlayDeletes = HashMap<u32, HashSet<OverlayDelete>>;
/// Insert and delete overlay maps for one edge orientation.
pub(crate) type EdgeOverlay = (OverlayInserts, OverlayDeletes);

/// Source of graph neighbors for algorithms that must work over clean CSR and
/// overlay-augmented projections.
pub(crate) trait NeighborSource {
    /// Iterate neighbors in base CSR order followed by non-duplicate inserts.
    fn neighbors(&self, node_idx: u32) -> NeighborIter<'_>;

    /// Iterate neighbors in reverse expansion order for DFS stack pushes.
    fn neighbors_reversed(&self, node_idx: u32) -> NeighborIter<'_>;

    /// Fill a bounded forward-order page beginning at an owned logical cursor.
    /// The default keeps layered sources compatible; CSR/overlay sources
    /// override it to seek without replaying earlier neighbors.
    fn fill_neighbors(
        &self,
        node_idx: u32,
        cursor: &mut OwnedNeighborCursor,
        limit: usize,
        output: &mut Vec<Neighbor>,
    ) -> bool {
        let pos = cursor.logical_position();
        let mut iter = self.neighbors(node_idx).skip(pos).peekable();
        output.extend(iter.by_ref().take(limit));
        cursor.advance_logical(output.len());
        iter.peek().is_none()
    }

    /// Fill a bounded page in the exact order returned by
    /// [`NeighborSource::neighbors_reversed`].
    fn fill_neighbors_reversed(
        &self,
        node_idx: u32,
        cursor: &mut OwnedNeighborCursor,
        limit: usize,
        output: &mut Vec<Neighbor>,
    ) -> bool {
        let pos = cursor.logical_position();
        let mut iter = self.neighbors_reversed(node_idx).skip(pos).peekable();
        output.extend(iter.by_ref().take(limit));
        cursor.advance_logical(output.len());
        iter.peek().is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum OwnedNeighborCursor {
    #[default]
    Start,
    Csr {
        pos: usize,
    },
    CsrReverse {
        consumed: usize,
    },
    Overlay {
        base_pos: usize,
        insert_pos: usize,
        inserts_phase: bool,
        duplicate_base_pos: usize,
        duplicate_base_end: usize,
        duplicate_insert_pos: usize,
        duplicate_check_initialized: bool,
    },
    OverlayReverse {
        base_consumed: usize,
        insert_consumed: usize,
        base_phase: bool,
        duplicate_base_pos: usize,
        duplicate_base_end: usize,
        duplicate_insert_pos: usize,
        duplicate_check_initialized: bool,
    },
    Layered {
        base_pos: usize,
        chunk_pos: usize,
        durable_pos: usize,
        overlay_pos: usize,
        last_key: Option<(u32, EdgeTypeId, bool, Option<RelationshipId>)>,
    },
    LayeredReverse {
        base_consumed: usize,
        chunk_consumed: usize,
        durable_consumed: usize,
        overlay_consumed: usize,
        last_key: Option<(u32, EdgeTypeId, bool, Option<RelationshipId>)>,
    },
    LayeredAny {
        out_base_pos: usize,
        in_base_pos: usize,
        out_chunk_pos: usize,
        in_chunk_pos: usize,
        out_durable_pos: usize,
        in_durable_pos: usize,
        out_overlay_pos: usize,
        in_overlay_pos: usize,
        last_key: Option<(u32, EdgeTypeId, bool, Option<RelationshipId>)>,
    },
    LayeredAnyReverse {
        out_base_consumed: usize,
        in_base_consumed: usize,
        out_chunk_consumed: usize,
        in_chunk_consumed: usize,
        out_durable_consumed: usize,
        in_durable_consumed: usize,
        out_overlay_consumed: usize,
        in_overlay_consumed: usize,
        last_key: Option<(u32, EdgeTypeId, bool, Option<RelationshipId>)>,
    },
    Logical {
        pos: usize,
    },
}

impl OwnedNeighborCursor {
    fn logical_position(&self) -> usize {
        match self {
            Self::Start => 0,
            Self::Csr { pos } | Self::Logical { pos } => *pos,
            Self::CsrReverse { consumed } => *consumed,
            Self::Overlay { .. }
            | Self::OverlayReverse { .. }
            | Self::Layered { .. }
            | Self::LayeredReverse { .. }
            | Self::LayeredAny { .. }
            | Self::LayeredAnyReverse { .. } => 0,
        }
    }

    fn advance_logical(&mut self, count: usize) {
        let next = self.logical_position().saturating_add(count);
        *self = Self::Logical { pos: next };
    }
}

/// Clean CSR neighbor source.
pub(crate) struct CsrNeighbors<'a> {
    edge_store: &'a EdgeStore,
}

impl<'a> CsrNeighbors<'a> {
    /// Borrow an [`EdgeStore`] as a clean neighbor source.
    pub(crate) fn new(edge_store: &'a EdgeStore) -> Self {
        Self { edge_store }
    }
}

impl NeighborSource for CsrNeighbors<'_> {
    fn neighbors(&self, node_idx: u32) -> NeighborIter<'_> {
        let (targets, type_ids, schema_reversed, relationship_ids) = self
            .edge_store
            .neighbors_with_schema_and_relationship_ids(node_idx);
        NeighborIter::Csr(CsrNeighborIter::forward(
            targets,
            type_ids,
            schema_reversed,
            relationship_ids,
        ))
    }

    fn neighbors_reversed(&self, node_idx: u32) -> NeighborIter<'_> {
        let (targets, type_ids, schema_reversed, relationship_ids) = self
            .edge_store
            .neighbors_with_schema_and_relationship_ids(node_idx);
        NeighborIter::Csr(CsrNeighborIter::reversed(
            targets,
            type_ids,
            schema_reversed,
            relationship_ids,
        ))
    }

    fn fill_neighbors(
        &self,
        node_idx: u32,
        cursor: &mut OwnedNeighborCursor,
        limit: usize,
        output: &mut Vec<Neighbor>,
    ) -> bool {
        let (targets, type_ids, schema_reversed, relationship_ids) = self
            .edge_store
            .neighbors_with_schema_and_relationship_ids(node_idx);
        let pos = match cursor {
            OwnedNeighborCursor::Start => 0,
            OwnedNeighborCursor::Csr { pos } => *pos,
            _ => cursor.logical_position(),
        };
        let end = pos.saturating_add(limit).min(targets.len());
        output.extend((pos.min(targets.len())..end).map(|pos| {
            Neighbor {
                target: targets[pos],
                type_id: type_ids.at(pos),
                schema_reversed: schema_reversed[pos] != 0,
                relationship_id: relationship_ids
                    .get(pos)
                    .copied()
                    .filter(|id| *id != NO_RELATIONSHIP_ID),
            }
        }));
        *cursor = OwnedNeighborCursor::Csr { pos: end };
        end == targets.len()
    }

    fn fill_neighbors_reversed(
        &self,
        node_idx: u32,
        cursor: &mut OwnedNeighborCursor,
        limit: usize,
        output: &mut Vec<Neighbor>,
    ) -> bool {
        let (targets, type_ids, schema_reversed, relationship_ids) = self
            .edge_store
            .neighbors_with_schema_and_relationship_ids(node_idx);
        let consumed = match cursor {
            OwnedNeighborCursor::CsrReverse { consumed } => *consumed,
            _ => 0,
        };
        let end = consumed.saturating_add(limit).min(targets.len());
        output.extend((consumed..end).map(|offset| {
            let pos = targets.len() - 1 - offset;
            Neighbor {
                target: targets[pos],
                type_id: type_ids.at(pos),
                schema_reversed: schema_reversed[pos] != 0,
                relationship_id: relationship_ids
                    .get(pos)
                    .copied()
                    .filter(|id| *id != NO_RELATIONSHIP_ID),
            }
        }));
        *cursor = OwnedNeighborCursor::CsrReverse { consumed: end };
        end == targets.len()
    }
}

/// CSR plus pending edge overlay source.
pub(crate) struct OverlayNeighbors<'a> {
    edge_store: &'a EdgeStore,
    inserts: &'a OverlayInserts,
    deletes: &'a OverlayDeletes,
}

impl<'a> OverlayNeighbors<'a> {
    /// Borrow a base CSR and orientation-specific overlay maps.
    pub(crate) fn new(
        edge_store: &'a EdgeStore,
        inserts: &'a OverlayInserts,
        deletes: &'a OverlayDeletes,
    ) -> Self {
        Self {
            edge_store,
            inserts,
            deletes,
        }
    }
}

impl NeighborSource for OverlayNeighbors<'_> {
    fn neighbors(&self, node_idx: u32) -> NeighborIter<'_> {
        let (targets, type_ids, schema_reversed, relationship_ids) = self
            .edge_store
            .neighbors_with_schema_and_relationship_ids(node_idx);
        NeighborIter::Overlay(OverlayNeighborIter::forward(
            targets,
            type_ids,
            schema_reversed,
            relationship_ids,
            self.inserts.get(&node_idx).map(Vec::as_slice),
            self.deletes.get(&node_idx),
        ))
    }

    fn neighbors_reversed(&self, node_idx: u32) -> NeighborIter<'_> {
        let (targets, type_ids, schema_reversed, relationship_ids) = self
            .edge_store
            .neighbors_with_schema_and_relationship_ids(node_idx);
        NeighborIter::Overlay(OverlayNeighborIter::reversed(
            targets,
            type_ids,
            schema_reversed,
            relationship_ids,
            self.inserts.get(&node_idx).map(Vec::as_slice),
            self.deletes.get(&node_idx),
        ))
    }

    fn fill_neighbors(
        &self,
        node_idx: u32,
        cursor: &mut OwnedNeighborCursor,
        limit: usize,
        output: &mut Vec<Neighbor>,
    ) -> bool {
        // Classic overlays are bounded by query mutation caps. Preserve their
        // exact merged order; clean CSR takes the constant-time slice path.
        if self.inserts.get(&node_idx).is_none() && self.deletes.get(&node_idx).is_none() {
            return CsrNeighbors::new(self.edge_store)
                .fill_neighbors(node_idx, cursor, limit, output);
        }
        let (targets, type_ids, schema_reversed, relationship_ids) = self
            .edge_store
            .neighbors_with_schema_and_relationship_ids(node_idx);
        let inserted = self.inserts.get(&node_idx).map(Vec::as_slice);
        let deleted = self.deletes.get(&node_idx);
        let (
            mut base_pos,
            mut insert_pos,
            mut inserts_phase,
            mut duplicate_base_pos,
            mut duplicate_base_end,
            mut duplicate_insert_pos,
            mut duplicate_check_initialized,
        ) = match cursor {
            OwnedNeighborCursor::Overlay {
                base_pos,
                insert_pos,
                inserts_phase,
                duplicate_base_pos,
                duplicate_base_end,
                duplicate_insert_pos,
                duplicate_check_initialized,
            } => (
                *base_pos,
                *insert_pos,
                *inserts_phase,
                *duplicate_base_pos,
                *duplicate_base_end,
                *duplicate_insert_pos,
                *duplicate_check_initialized,
            ),
            _ => (0, 0, false, 0, 0, 0, false),
        };
        let mut examined = 0usize;
        while examined < limit && !inserts_phase {
            let Some(pos) = (base_pos < targets.len()).then_some(base_pos) else {
                inserts_phase = true;
                break;
            };
            base_pos += 1;
            examined += 1;
            let relationship_id = relationship_ids
                .get(pos)
                .copied()
                .filter(|id| *id != NO_RELATIONSHIP_ID);
            let candidate = Neighbor {
                target: targets[pos],
                type_id: type_ids.at(pos),
                schema_reversed: schema_reversed[pos] != 0,
                relationship_id,
            };
            if deleted.is_some_and(|set| {
                set.contains(&(
                    candidate.target,
                    candidate.type_id,
                    candidate.schema_reversed,
                    None,
                )) || set.contains(&(
                    candidate.target,
                    candidate.type_id,
                    candidate.schema_reversed,
                    relationship_id,
                ))
            }) {
                continue;
            }
            output.push(candidate);
        }
        while examined < limit && inserts_phase {
            let Some(values) = inserted else { break };
            let Some(&(target, type_id, reversed, relationship_id)) = values.get(insert_pos) else {
                break;
            };
            if !duplicate_check_initialized {
                let key_before = |idx: usize| {
                    (targets[idx], type_ids.at(idx), schema_reversed[idx] != 0)
                        < (target, type_id, reversed)
                };
                let key_after = |idx: usize| {
                    (targets[idx], type_ids.at(idx), schema_reversed[idx] != 0)
                        <= (target, type_id, reversed)
                };
                let mut low = 0usize;
                let mut high = targets.len();
                while low < high {
                    let mid = low + (high - low) / 2;
                    if key_before(mid) {
                        low = mid + 1;
                    } else {
                        high = mid;
                    }
                }
                duplicate_base_pos = low;
                high = targets.len();
                while low < high {
                    let mid = low + (high - low) / 2;
                    if key_after(mid) {
                        low = mid + 1;
                    } else {
                        high = mid;
                    }
                }
                duplicate_base_end = low;
                duplicate_insert_pos = 0;
                duplicate_check_initialized = true;
            }

            let mut duplicate = false;
            while examined < limit && duplicate_base_pos < duplicate_base_end {
                let idx = duplicate_base_pos;
                duplicate_base_pos += 1;
                examined += 1;
                if relationship_ids
                    .get(idx)
                    .copied()
                    .filter(|id| *id != NO_RELATIONSHIP_ID)
                    == relationship_id
                {
                    duplicate = true;
                    break;
                }
            }
            while !duplicate && examined < limit && duplicate_insert_pos < insert_pos {
                duplicate =
                    values[duplicate_insert_pos] == (target, type_id, reversed, relationship_id);
                duplicate_insert_pos += 1;
                examined += 1;
            }
            let checks_complete = duplicate
                || (duplicate_base_pos == duplicate_base_end && duplicate_insert_pos == insert_pos);
            if !checks_complete {
                break;
            }
            if !duplicate {
                if examined == limit {
                    break;
                }
                output.push(Neighbor {
                    target,
                    type_id,
                    schema_reversed: reversed,
                    relationship_id,
                });
                examined += 1;
            }
            insert_pos += 1;
            duplicate_base_pos = 0;
            duplicate_base_end = 0;
            duplicate_insert_pos = 0;
            duplicate_check_initialized = false;
        }
        *cursor = OwnedNeighborCursor::Overlay {
            base_pos,
            insert_pos,
            inserts_phase,
            duplicate_base_pos,
            duplicate_base_end,
            duplicate_insert_pos,
            duplicate_check_initialized,
        };
        inserts_phase && insert_pos >= inserted.map_or(0, <[_]>::len)
    }

    fn fill_neighbors_reversed(
        &self,
        node_idx: u32,
        cursor: &mut OwnedNeighborCursor,
        limit: usize,
        output: &mut Vec<Neighbor>,
    ) -> bool {
        if self.inserts.get(&node_idx).is_none() && self.deletes.get(&node_idx).is_none() {
            return CsrNeighbors::new(self.edge_store)
                .fill_neighbors_reversed(node_idx, cursor, limit, output);
        }
        let (targets, type_ids, schema_reversed, relationship_ids) = self
            .edge_store
            .neighbors_with_schema_and_relationship_ids(node_idx);
        let inserted = self
            .inserts
            .get(&node_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let deleted = self.deletes.get(&node_idx);
        let (
            mut base_consumed,
            mut insert_consumed,
            mut base_phase,
            mut duplicate_base_pos,
            mut duplicate_base_end,
            mut duplicate_insert_pos,
            mut duplicate_check_initialized,
        ) = match cursor {
            OwnedNeighborCursor::OverlayReverse {
                base_consumed,
                insert_consumed,
                base_phase,
                duplicate_base_pos,
                duplicate_base_end,
                duplicate_insert_pos,
                duplicate_check_initialized,
            } => (
                *base_consumed,
                *insert_consumed,
                *base_phase,
                *duplicate_base_pos,
                *duplicate_base_end,
                *duplicate_insert_pos,
                *duplicate_check_initialized,
            ),
            _ => (0, 0, false, 0, 0, 0, false),
        };
        let mut examined = 0usize;
        while examined < limit && !base_phase {
            if insert_consumed >= inserted.len() {
                base_phase = true;
                break;
            }
            let pos = inserted.len() - 1 - insert_consumed;
            let &(target, type_id, reversed, relationship_id) = &inserted[pos];
            if !duplicate_check_initialized {
                let key_before = |idx: usize| {
                    (targets[idx], type_ids.at(idx), schema_reversed[idx] != 0)
                        < (target, type_id, reversed)
                };
                let key_after = |idx: usize| {
                    (targets[idx], type_ids.at(idx), schema_reversed[idx] != 0)
                        <= (target, type_id, reversed)
                };
                let mut low = 0usize;
                let mut high = targets.len();
                while low < high {
                    let mid = low + (high - low) / 2;
                    if key_before(mid) {
                        low = mid + 1
                    } else {
                        high = mid
                    }
                }
                duplicate_base_pos = low;
                high = targets.len();
                while low < high {
                    let mid = low + (high - low) / 2;
                    if key_after(mid) {
                        low = mid + 1
                    } else {
                        high = mid
                    }
                }
                duplicate_base_end = low;
                duplicate_insert_pos = 0;
                duplicate_check_initialized = true;
            }
            let mut duplicate = false;
            while examined < limit && duplicate_base_pos < duplicate_base_end {
                let idx = duplicate_base_pos;
                duplicate_base_pos += 1;
                examined += 1;
                if relationship_ids
                    .get(idx)
                    .copied()
                    .filter(|id| *id != NO_RELATIONSHIP_ID)
                    == relationship_id
                {
                    duplicate = true;
                    break;
                }
            }
            while !duplicate && examined < limit && duplicate_insert_pos < pos {
                duplicate =
                    inserted[duplicate_insert_pos] == (target, type_id, reversed, relationship_id);
                duplicate_insert_pos += 1;
                examined += 1;
            }
            let checks_complete = duplicate
                || (duplicate_base_pos == duplicate_base_end && duplicate_insert_pos == pos);
            if !checks_complete {
                break;
            }
            if !duplicate {
                if examined == limit {
                    break;
                }
                output.push(Neighbor {
                    target,
                    type_id,
                    schema_reversed: reversed,
                    relationship_id,
                });
                examined += 1;
            }
            insert_consumed += 1;
            duplicate_base_pos = 0;
            duplicate_base_end = 0;
            duplicate_insert_pos = 0;
            duplicate_check_initialized = false;
        }
        while examined < limit && base_phase {
            if base_consumed >= targets.len() {
                break;
            }
            let pos = targets.len() - 1 - base_consumed;
            base_consumed += 1;
            examined += 1;
            let relationship_id = relationship_ids
                .get(pos)
                .copied()
                .filter(|id| *id != NO_RELATIONSHIP_ID);
            let candidate = Neighbor {
                target: targets[pos],
                type_id: type_ids.at(pos),
                schema_reversed: schema_reversed[pos] != 0,
                relationship_id,
            };
            if deleted.is_some_and(|set| {
                set.contains(&(
                    candidate.target,
                    candidate.type_id,
                    candidate.schema_reversed,
                    None,
                )) || set.contains(&(
                    candidate.target,
                    candidate.type_id,
                    candidate.schema_reversed,
                    relationship_id,
                ))
            }) {
                continue;
            }
            output.push(candidate);
        }
        *cursor = OwnedNeighborCursor::OverlayReverse {
            base_consumed,
            insert_consumed,
            base_phase,
            duplicate_base_pos,
            duplicate_base_end,
            duplicate_insert_pos,
            duplicate_check_initialized,
        };
        base_phase && base_consumed >= targets.len()
    }
}

/// Neighbor stream item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Neighbor {
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: EdgeTypeId,
    /// Whether this edge row is a synthetic reverse of the schema edge.
    pub(crate) schema_reversed: bool,
    /// Durable relationship identity for the source row when available.
    pub(crate) relationship_id: Option<RelationshipId>,
}

/// Weighted neighbor stream item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WeightedNeighbor {
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: EdgeTypeId,
    /// Edge weight.
    pub(crate) weight: u32,
    /// Whether this edge row is a synthetic reverse of the schema edge.
    pub(crate) schema_reversed: bool,
    /// Durable relationship identity for the weighted source row when available.
    pub(crate) relationship_id: Option<RelationshipId>,
}

/// Source of weighted graph neighbors for shortest-path algorithms.
pub(crate) trait WeightedNeighborSource {
    /// Whether this source can expose weighted edges.
    fn has_weighted_edges(&self) -> bool;

    /// Return weighted neighbors for `node_idx`.
    fn weighted_neighbors(&self, node_idx: u32) -> Vec<WeightedNeighbor>;

    /// Fill at most `limit` examined weighted adjacency rows and advance an
    /// owned cursor. Returns true when the node's weighted adjacency is fully
    /// exhausted.
    fn fill_weighted_neighbors(
        &self,
        node_idx: u32,
        cursor: &mut OwnedNeighborCursor,
        limit: usize,
        output: &mut Vec<WeightedNeighbor>,
    ) -> bool;
}

impl WeightedNeighborSource for EdgeStore {
    fn has_weighted_edges(&self) -> bool {
        self.has_weights()
    }

    fn weighted_neighbors(&self, node_idx: u32) -> Vec<WeightedNeighbor> {
        let (targets, type_ids, schema_reversed, weights) =
            self.neighbors_weighted_with_schema(node_idx);
        let (_, _, _, relationship_ids) = self.neighbors_with_schema_and_relationship_ids(node_idx);
        targets
            .iter()
            .zip(type_ids.iter())
            .zip(schema_reversed.iter())
            .zip(weights.iter())
            .enumerate()
            .map(
                |(idx, (((&target, type_id), &schema_reversed), &weight))| WeightedNeighbor {
                    target,
                    type_id,
                    weight,
                    schema_reversed: schema_reversed != 0,
                    relationship_id: relationship_ids
                        .get(idx)
                        .copied()
                        .filter(|&id| id != crate::edge_store::NO_RELATIONSHIP_ID),
                },
            )
            .collect()
    }

    fn fill_weighted_neighbors(
        &self,
        node_idx: u32,
        cursor: &mut OwnedNeighborCursor,
        limit: usize,
        output: &mut Vec<WeightedNeighbor>,
    ) -> bool {
        let (targets, type_ids, schema_reversed, weights) =
            self.neighbors_weighted_with_schema(node_idx);
        let (_, _, _, relationship_ids) = self.neighbors_with_schema_and_relationship_ids(node_idx);
        let mut position = match cursor {
            OwnedNeighborCursor::Csr { pos } => *pos,
            _ => 0,
        };
        let end = position.saturating_add(limit).min(targets.len());
        while position < end {
            if let Some(&weight) = weights.get(position) {
                output.push(WeightedNeighbor {
                    target: targets[position],
                    type_id: type_ids.at(position),
                    weight,
                    schema_reversed: schema_reversed[position] != 0,
                    relationship_id: relationship_ids
                        .get(position)
                        .copied()
                        .filter(|&id| id != crate::edge_store::NO_RELATIONSHIP_ID),
                });
            }
            position += 1;
        }
        *cursor = OwnedNeighborCursor::Csr { pos: position };
        position >= targets.len()
    }
}

/// Neighbor iterator for clean and overlay-backed sources.
pub(crate) enum NeighborIter<'a> {
    Csr(CsrNeighborIter<'a>),
    Overlay(OverlayNeighborIter<'a>),
    #[allow(
        dead_code,
        reason = "Layered runtime owns merged neighbor vectors until Engine read-path adoption uses it in production"
    )]
    Owned(std::vec::IntoIter<Neighbor>),
}

impl Iterator for NeighborIter<'_> {
    type Item = Neighbor;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Csr(iter) => iter.next(),
            Self::Overlay(iter) => iter.next(),
            Self::Owned(iter) => iter.next(),
        }
    }
}

pub(crate) struct CsrNeighborIter<'a> {
    targets: &'a [u32],
    type_ids: EdgeTypeSlice<'a>,
    schema_reversed: &'a [u8],
    relationship_ids: &'a [RelationshipId],
    pos: usize,
    reversed: bool,
}

impl<'a> CsrNeighborIter<'a> {
    fn forward(
        targets: &'a [u32],
        type_ids: EdgeTypeSlice<'a>,
        schema_reversed: &'a [u8],
        relationship_ids: &'a [RelationshipId],
    ) -> Self {
        Self {
            targets,
            type_ids,
            schema_reversed,
            relationship_ids,
            pos: 0,
            reversed: false,
        }
    }

    fn reversed(
        targets: &'a [u32],
        type_ids: EdgeTypeSlice<'a>,
        schema_reversed: &'a [u8],
        relationship_ids: &'a [RelationshipId],
    ) -> Self {
        Self {
            targets,
            type_ids,
            schema_reversed,
            relationship_ids,
            pos: targets.len(),
            reversed: true,
        }
    }
}

impl Iterator for CsrNeighborIter<'_> {
    type Item = Neighbor;

    fn next(&mut self) -> Option<Self::Item> {
        let pos = if self.reversed {
            self.pos = self.pos.checked_sub(1)?;
            self.pos
        } else {
            if self.pos >= self.targets.len() {
                return None;
            }
            let pos = self.pos;
            self.pos += 1;
            pos
        };
        Some(Neighbor {
            target: self.targets[pos],
            type_id: self.type_ids.at(pos),
            schema_reversed: self.schema_reversed[pos] != 0,
            relationship_id: self
                .relationship_ids
                .get(pos)
                .copied()
                .filter(|id| *id != NO_RELATIONSHIP_ID),
        })
    }
}

enum OverlayPhase {
    Base,
    Inserts,
}

pub(crate) struct OverlayNeighborIter<'a> {
    targets: &'a [u32],
    type_ids: EdgeTypeSlice<'a>,
    deleted: Option<&'a HashSet<OverlayDelete>>,
    inserted: Option<&'a [OverlayInsert]>,
    base: CsrNeighborIter<'a>,
    insert_pos: usize,
    phase: OverlayPhase,
    reversed: bool,
}

impl<'a> OverlayNeighborIter<'a> {
    fn forward(
        targets: &'a [u32],
        type_ids: EdgeTypeSlice<'a>,
        schema_reversed: &'a [u8],
        relationship_ids: &'a [RelationshipId],
        inserted: Option<&'a [OverlayInsert]>,
        deleted: Option<&'a HashSet<OverlayDelete>>,
    ) -> Self {
        Self {
            targets,
            type_ids,
            deleted,
            inserted,
            base: CsrNeighborIter::forward(targets, type_ids, schema_reversed, relationship_ids),
            insert_pos: 0,
            phase: OverlayPhase::Base,
            reversed: false,
        }
    }

    fn reversed(
        targets: &'a [u32],
        type_ids: EdgeTypeSlice<'a>,
        schema_reversed: &'a [u8],
        relationship_ids: &'a [RelationshipId],
        inserted: Option<&'a [OverlayInsert]>,
        deleted: Option<&'a HashSet<OverlayDelete>>,
    ) -> Self {
        Self {
            targets,
            type_ids,
            deleted,
            inserted,
            base: CsrNeighborIter::reversed(targets, type_ids, schema_reversed, relationship_ids),
            insert_pos: inserted.map_or(0, <[_]>::len),
            phase: OverlayPhase::Inserts,
            reversed: true,
        }
    }

    fn base_contains(
        &self,
        target: u32,
        type_id: EdgeTypeId,
        schema_reversed: bool,
        relationship_id: Option<RelationshipId>,
    ) -> bool {
        self.targets
            .iter()
            .zip(self.type_ids.iter())
            .zip(self.base.schema_reversed.iter())
            .enumerate()
            .any(
                |(idx, ((&base_target, base_type), &base_schema_reversed))| {
                    base_target == target
                        && base_type == type_id
                        && (base_schema_reversed != 0) == schema_reversed
                        && self
                            .base
                            .relationship_ids
                            .get(idx)
                            .copied()
                            .filter(|id| *id != NO_RELATIONSHIP_ID)
                            == relationship_id
                },
            )
    }

    fn inserted_duplicate(
        &self,
        pos: usize,
        target: u32,
        type_id: EdgeTypeId,
        schema_reversed: bool,
        relationship_id: Option<RelationshipId>,
    ) -> bool {
        self.inserted.is_some_and(|inserted| {
            inserted[..pos].iter().any(
                |&(
                    inserted_target,
                    inserted_type,
                    inserted_schema_reversed,
                    inserted_relationship_id,
                )| {
                    inserted_target == target
                        && inserted_type == type_id
                        && inserted_schema_reversed == schema_reversed
                        && inserted_relationship_id == relationship_id
                },
            )
        })
    }

    fn next_base(&mut self) -> Option<Neighbor> {
        for neighbor in self.base.by_ref() {
            if self.deleted.is_some_and(|deleted| {
                deleted.contains(&(
                    neighbor.target,
                    neighbor.type_id,
                    neighbor.schema_reversed,
                    None,
                )) || deleted.contains(&(
                    neighbor.target,
                    neighbor.type_id,
                    neighbor.schema_reversed,
                    neighbor.relationship_id,
                ))
            }) {
                continue;
            }
            return Some(neighbor);
        }
        None
    }

    fn next_insert(&mut self) -> Option<Neighbor> {
        let inserted = self.inserted?;
        loop {
            let pos = if self.reversed {
                self.insert_pos = self.insert_pos.checked_sub(1)?;
                self.insert_pos
            } else {
                if self.insert_pos >= inserted.len() {
                    return None;
                }
                let pos = self.insert_pos;
                self.insert_pos += 1;
                pos
            };
            let (target, type_id, schema_reversed, relationship_id) = inserted[pos];
            if self.base_contains(target, type_id, schema_reversed, relationship_id)
                || self.inserted_duplicate(pos, target, type_id, schema_reversed, relationship_id)
            {
                continue;
            }
            return Some(Neighbor {
                target,
                type_id,
                schema_reversed,
                relationship_id,
            });
        }
    }
}

impl Iterator for OverlayNeighborIter<'_> {
    type Item = Neighbor;

    fn next(&mut self) -> Option<Self::Item> {
        match self.phase {
            OverlayPhase::Base => self.next_base().or_else(|| {
                self.phase = OverlayPhase::Inserts;
                self.next_insert()
            }),
            OverlayPhase::Inserts => self.next_insert().or_else(|| {
                self.phase = OverlayPhase::Base;
                self.next_base()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge_store::RawEdge;
    use proptest::prelude::*;

    #[test]
    fn clean_neighbors_match_csr_order() {
        let edges = vec![
            RawEdge {
                source: 0,
                target: 1,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: EdgeTypeId::from_v6_storage(2).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
            },
        ];
        let store = EdgeStore::from_edges(3, edges, false);
        let neighbors = CsrNeighbors::new(&store);

        let actual = neighbors.neighbors(0).collect::<Vec<_>>();

        assert_eq!(
            actual,
            vec![
                Neighbor {
                    target: 1,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    schema_reversed: false,
                    relationship_id: None,
                },
                Neighbor {
                    target: 2,
                    type_id: EdgeTypeId::from_v6_storage(2).expect("fixture type ID is valid v6"),
                    schema_reversed: false,
                    relationship_id: None,
                }
            ]
        );
    }

    #[test]
    fn overlay_neighbors_hide_deletes_and_append_inserts() {
        let store = EdgeStore::from_edges(
            4,
            vec![
                RawEdge {
                    source: 0,
                    target: 1,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    weight: None,
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    weight: None,
                    schema_reversed: false,
                },
            ],
            false,
        );
        let mut inserts = OverlayInserts::new();
        inserts.insert(
            0,
            vec![
                (3, crate::types::EdgeTypeId::test_v6(1), false, None),
                (2, crate::types::EdgeTypeId::test_v6(1), false, None),
                (3, crate::types::EdgeTypeId::test_v6(1), false, None),
            ],
        );
        let mut deletes = OverlayDeletes::new();
        deletes.insert(
            0,
            HashSet::from([(1, crate::types::EdgeTypeId::test_v6(1), false, None)]),
        );
        let neighbors = OverlayNeighbors::new(&store, &inserts, &deletes);

        let actual = neighbors.neighbors(0).collect::<Vec<_>>();

        assert_eq!(
            actual,
            vec![
                Neighbor {
                    target: 2,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    schema_reversed: false,
                    relationship_id: None,
                },
                Neighbor {
                    target: 3,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    schema_reversed: false,
                    relationship_id: None,
                }
            ]
        );
    }

    #[test]
    fn overlay_neighbors_keep_schema_reversed_inserts_distinct_from_base() {
        let store = EdgeStore::from_edges(
            2,
            vec![RawEdge {
                source: 0,
                target: 1,
                type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                weight: None,
                schema_reversed: false,
            }],
            false,
        );
        let mut inserts = OverlayInserts::new();
        inserts.insert(
            0,
            vec![(1, crate::types::EdgeTypeId::test_v6(1), true, None)],
        );
        let deletes = OverlayDeletes::new();
        let neighbors = OverlayNeighbors::new(&store, &inserts, &deletes);

        let actual = neighbors.neighbors(0).collect::<Vec<_>>();

        assert_eq!(
            actual,
            vec![
                Neighbor {
                    target: 1,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    schema_reversed: false,
                    relationship_id: None,
                },
                Neighbor {
                    target: 1,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    schema_reversed: true,
                    relationship_id: None,
                },
            ]
        );
    }

    #[test]
    fn clean_csr_cursor_pages_preserve_high_degree_order() {
        let degree = 4_096u32;
        let edges = (1..=degree)
            .map(|target| RawEdge {
                source: 0,
                target,
                type_id: EdgeTypeId::test_v6((target % 7) as u8),
                weight: None,
                schema_reversed: target % 2 == 0,
            })
            .collect::<Vec<_>>();
        let store = EdgeStore::from_edges(degree + 1, edges, false);
        let neighbors = CsrNeighbors::new(&store);
        let expected = neighbors.neighbors(0).collect::<Vec<_>>();
        let mut actual = Vec::new();
        let mut cursor = OwnedNeighborCursor::default();
        loop {
            let exhausted = neighbors.fill_neighbors(0, &mut cursor, 17, &mut actual);
            if exhausted {
                break;
            }
        }
        assert_eq!(actual, expected);

        let expected_reversed = neighbors.neighbors_reversed(0).collect::<Vec<_>>();
        for page_size in [1, 2, 17, 257, 4_096] {
            let mut actual_reversed = Vec::new();
            let mut cursor = OwnedNeighborCursor::default();
            loop {
                let exhausted = neighbors.fill_neighbors_reversed(
                    0,
                    &mut cursor,
                    page_size,
                    &mut actual_reversed,
                );
                if exhausted {
                    break;
                }
            }
            assert_eq!(actual_reversed, expected_reversed);
        }
    }

    #[test]
    fn owned_overlay_cursor_pages_without_replaying_prefixes() {
        let store = EdgeStore::from_edges(
            6,
            (1..=4)
                .map(|target| RawEdge {
                    source: 0,
                    target,
                    type_id: EdgeTypeId::from_v6_storage(1).expect("fixture type ID is valid v6"),
                    weight: None,
                    schema_reversed: false,
                })
                .collect(),
            false,
        );
        let inserts = OverlayInserts::from([(
            0,
            vec![(5, crate::types::EdgeTypeId::test_v6(1), false, None)],
        )]);
        let deletes = OverlayDeletes::from([(
            0,
            HashSet::from([(2, crate::types::EdgeTypeId::test_v6(1), false, None)]),
        )]);
        let neighbors = OverlayNeighbors::new(&store, &inserts, &deletes);
        let expected = neighbors.neighbors(0).collect::<Vec<_>>();
        let mut cursor = OwnedNeighborCursor::default();
        let mut actual = Vec::new();
        let mut pages = 0;
        loop {
            pages += 1;
            let mut page = Vec::new();
            let exhausted = neighbors.fill_neighbors(0, &mut cursor, 2, &mut page);
            actual.extend(page);
            if exhausted {
                break;
            }
        }
        assert_eq!(actual, expected);
        // Two raw base pages plus one bounded insert-deduplication page.
        assert_eq!(pages, 3);
        assert!(matches!(cursor, OwnedNeighborCursor::Overlay { .. }));
    }

    proptest! {
        #[test]
        fn owned_overlay_cursor_matches_iterator_for_bounded_pages(
            raw_edges in prop::collection::vec((1u32..12, 0u8..4, any::<bool>()), 0..48),
            raw_inserts in prop::collection::vec((1u32..12, 0u8..4, any::<bool>(), prop::option::of(1u32..32)), 0..32),
            raw_deletes in prop::collection::vec((1u32..12, 0u8..4, any::<bool>(), prop::option::of(1u32..32)), 0..32),
            page_size in 1usize..8,
        ) {
            let store = EdgeStore::from_edges(
                12,
                raw_edges
                    .into_iter()
                    .map(|(target, type_id, schema_reversed)| RawEdge {
                        source: 0,
                        target,
                        type_id: EdgeTypeId::test_v6(type_id),
                        weight: None,
                        schema_reversed,
                    })
                    .collect(),
                false,
            );
            let inserts = OverlayInserts::from([(
                0,
                raw_inserts
                    .into_iter()
                    .map(|(target, type_id, reversed, identity)| {
                        (target, EdgeTypeId::test_v6(type_id), reversed, identity)
                    })
                    .collect(),
            )]);
            let deletes = OverlayDeletes::from([(
                0,
                raw_deletes
                    .into_iter()
                    .map(|(target, type_id, reversed, identity)| {
                        (target, EdgeTypeId::test_v6(type_id), reversed, identity)
                    })
                    .collect(),
            )]);
            let neighbors = OverlayNeighbors::new(&store, &inserts, &deletes);
            let expected = neighbors.neighbors(0).collect::<Vec<_>>();
            let mut actual = Vec::new();
            let mut cursor = OwnedNeighborCursor::default();
            let mut pages = 0usize;
            loop {
                pages += 1;
                prop_assert!(pages <= 10_000, "cursor failed to make bounded progress");
                let mut page = Vec::new();
                let exhausted = neighbors.fill_neighbors(0, &mut cursor, page_size, &mut page);
                actual.extend(page);
                if exhausted {
                    break;
                }
            }
            prop_assert_eq!(actual, expected);

            let expected_reversed = neighbors.neighbors_reversed(0).collect::<Vec<_>>();
            let mut actual_reversed = Vec::new();
            let mut reverse_cursor = OwnedNeighborCursor::default();
            let mut reverse_pages = 0usize;
            loop {
                reverse_pages += 1;
                prop_assert!(reverse_pages <= 10_000, "reverse cursor failed to make bounded progress");
                let mut page = Vec::new();
                let exhausted = neighbors.fill_neighbors_reversed(
                    0,
                    &mut reverse_cursor,
                    page_size,
                    &mut page,
                );
                actual_reversed.extend(page);
                if exhausted { break; }
            }
            prop_assert_eq!(actual_reversed, expected_reversed);
        }

        #[test]
        fn clean_overlay_matches_csr_neighbors(
            node_count in 1u32..16,
            raw_edges in prop::collection::vec((0u32..16, 0u32..16, 0u8..4), 0..96),
            query_node in 0u32..16,
        ) {
            let edges = raw_edges
                .into_iter()
                .filter(|(source, target, _)| *source < node_count && *target < node_count)
                .map(|(source, target, type_id)| RawEdge {
                    source,
                    target,
                    type_id: EdgeTypeId::test_v6(type_id),
                    weight: None,
                schema_reversed: false,
                })
                .collect::<Vec<_>>();
            let store = EdgeStore::from_edges(node_count, edges, false);
            let inserts = OverlayInserts::new();
            let deletes = OverlayDeletes::new();
            let csr = CsrNeighbors::new(&store);
            let overlay = OverlayNeighbors::new(&store, &inserts, &deletes);

            prop_assert_eq!(
                csr.neighbors(query_node).collect::<Vec<_>>(),
                overlay.neighbors(query_node).collect::<Vec<_>>()
            );
            prop_assert_eq!(
                csr.neighbors_reversed(query_node).collect::<Vec<_>>(),
                overlay.neighbors_reversed(query_node).collect::<Vec<_>>()
            );
        }
    }
}
