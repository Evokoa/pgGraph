//! Overlay-aware neighbor iteration for graph algorithms.
//!
//! The committed CSR remains the fast path. Pending sync and future mutable
//! projection deltas are layered as insert/delete maps without materializing a
//! per-node neighbor vector for clean reads.

use std::collections::{HashMap, HashSet};

use crate::edge_store::EdgeStore;

/// Pending edge inserts keyed by source node.
pub(crate) type OverlayInserts = HashMap<u32, Vec<(u32, u8, bool)>>;
/// Pending edge deletes keyed by source node.
pub(crate) type OverlayDeletes = HashMap<u32, HashSet<(u32, u8)>>;
/// Insert and delete overlay maps for one edge orientation.
pub(crate) type EdgeOverlay = (OverlayInserts, OverlayDeletes);

/// Source of graph neighbors for algorithms that must work over clean CSR and
/// overlay-augmented projections.
pub(crate) trait NeighborSource {
    /// Iterate neighbors in base CSR order followed by non-duplicate inserts.
    fn neighbors(&self, node_idx: u32) -> NeighborIter<'_>;

    /// Iterate neighbors in reverse expansion order for DFS stack pushes.
    fn neighbors_reversed(&self, node_idx: u32) -> NeighborIter<'_>;
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
        let (targets, type_ids, schema_reversed) = self.edge_store.neighbors_with_schema(node_idx);
        NeighborIter::Csr(CsrNeighborIter::forward(targets, type_ids, schema_reversed))
    }

    fn neighbors_reversed(&self, node_idx: u32) -> NeighborIter<'_> {
        let (targets, type_ids, schema_reversed) = self.edge_store.neighbors_with_schema(node_idx);
        NeighborIter::Csr(CsrNeighborIter::reversed(
            targets,
            type_ids,
            schema_reversed,
        ))
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
        let (targets, type_ids, schema_reversed) = self.edge_store.neighbors_with_schema(node_idx);
        NeighborIter::Overlay(OverlayNeighborIter::forward(
            targets,
            type_ids,
            schema_reversed,
            self.inserts.get(&node_idx).map(Vec::as_slice),
            self.deletes.get(&node_idx),
        ))
    }

    fn neighbors_reversed(&self, node_idx: u32) -> NeighborIter<'_> {
        let (targets, type_ids, schema_reversed) = self.edge_store.neighbors_with_schema(node_idx);
        NeighborIter::Overlay(OverlayNeighborIter::reversed(
            targets,
            type_ids,
            schema_reversed,
            self.inserts.get(&node_idx).map(Vec::as_slice),
            self.deletes.get(&node_idx),
        ))
    }
}

/// Neighbor stream item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Neighbor {
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: u8,
    /// Whether this edge row is a synthetic reverse of the schema edge.
    pub(crate) schema_reversed: bool,
}

/// Weighted neighbor stream item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WeightedNeighbor {
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: u8,
    /// Edge weight.
    pub(crate) weight: u32,
    /// Whether this edge row is a synthetic reverse of the schema edge.
    pub(crate) schema_reversed: bool,
}

/// Source of weighted graph neighbors for shortest-path algorithms.
pub(crate) trait WeightedNeighborSource {
    /// Whether this source can expose weighted edges.
    fn has_weighted_edges(&self) -> bool;

    /// Return weighted neighbors for `node_idx`.
    fn weighted_neighbors(&self, node_idx: u32) -> Vec<WeightedNeighbor>;
}

impl WeightedNeighborSource for EdgeStore {
    fn has_weighted_edges(&self) -> bool {
        self.has_weights()
    }

    fn weighted_neighbors(&self, node_idx: u32) -> Vec<WeightedNeighbor> {
        let (targets, type_ids, schema_reversed, weights) =
            self.neighbors_weighted_with_schema(node_idx);
        targets
            .iter()
            .zip(type_ids.iter())
            .zip(schema_reversed.iter())
            .zip(weights.iter())
            .map(
                |(((&target, &type_id), &schema_reversed), &weight)| WeightedNeighbor {
                    target,
                    type_id,
                    weight,
                    schema_reversed: schema_reversed != 0,
                },
            )
            .collect()
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
    type_ids: &'a [u8],
    schema_reversed: &'a [u8],
    pos: usize,
    reversed: bool,
}

impl<'a> CsrNeighborIter<'a> {
    fn forward(targets: &'a [u32], type_ids: &'a [u8], schema_reversed: &'a [u8]) -> Self {
        Self {
            targets,
            type_ids,
            schema_reversed,
            pos: 0,
            reversed: false,
        }
    }

    fn reversed(targets: &'a [u32], type_ids: &'a [u8], schema_reversed: &'a [u8]) -> Self {
        Self {
            targets,
            type_ids,
            schema_reversed,
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
            type_id: self.type_ids[pos],
            schema_reversed: self.schema_reversed[pos] != 0,
        })
    }
}

enum OverlayPhase {
    Base,
    Inserts,
}

pub(crate) struct OverlayNeighborIter<'a> {
    targets: &'a [u32],
    type_ids: &'a [u8],
    deleted: Option<&'a HashSet<(u32, u8)>>,
    inserted: Option<&'a [(u32, u8, bool)]>,
    base: CsrNeighborIter<'a>,
    insert_pos: usize,
    phase: OverlayPhase,
    reversed: bool,
}

impl<'a> OverlayNeighborIter<'a> {
    fn forward(
        targets: &'a [u32],
        type_ids: &'a [u8],
        schema_reversed: &'a [u8],
        inserted: Option<&'a [(u32, u8, bool)]>,
        deleted: Option<&'a HashSet<(u32, u8)>>,
    ) -> Self {
        Self {
            targets,
            type_ids,
            deleted,
            inserted,
            base: CsrNeighborIter::forward(targets, type_ids, schema_reversed),
            insert_pos: 0,
            phase: OverlayPhase::Base,
            reversed: false,
        }
    }

    fn reversed(
        targets: &'a [u32],
        type_ids: &'a [u8],
        schema_reversed: &'a [u8],
        inserted: Option<&'a [(u32, u8, bool)]>,
        deleted: Option<&'a HashSet<(u32, u8)>>,
    ) -> Self {
        Self {
            targets,
            type_ids,
            deleted,
            inserted,
            base: CsrNeighborIter::reversed(targets, type_ids, schema_reversed),
            insert_pos: inserted.map_or(0, <[_]>::len),
            phase: OverlayPhase::Inserts,
            reversed: true,
        }
    }

    fn base_contains(&self, target: u32, type_id: u8, schema_reversed: bool) -> bool {
        self.targets
            .iter()
            .zip(self.type_ids.iter())
            .zip(self.base.schema_reversed.iter())
            .any(|((&base_target, &base_type), &base_schema_reversed)| {
                base_target == target
                    && base_type == type_id
                    && (base_schema_reversed != 0) == schema_reversed
            })
    }

    fn inserted_duplicate(
        &self,
        pos: usize,
        target: u32,
        type_id: u8,
        schema_reversed: bool,
    ) -> bool {
        self.inserted.is_some_and(|inserted| {
            inserted[..pos].iter().any(
                |&(inserted_target, inserted_type, inserted_schema_reversed)| {
                    inserted_target == target
                        && inserted_type == type_id
                        && inserted_schema_reversed == schema_reversed
                },
            )
        })
    }

    fn next_base(&mut self) -> Option<Neighbor> {
        for neighbor in self.base.by_ref() {
            if self
                .deleted
                .is_some_and(|deleted| deleted.contains(&(neighbor.target, neighbor.type_id)))
            {
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
            let (target, type_id, schema_reversed) = inserted[pos];
            if self.base_contains(target, type_id, schema_reversed)
                || self.inserted_duplicate(pos, target, type_id, schema_reversed)
            {
                continue;
            }
            return Some(Neighbor {
                target,
                type_id,
                schema_reversed,
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
                type_id: 1,
                weight: None,
                schema_reversed: false,
            },
            RawEdge {
                source: 0,
                target: 2,
                type_id: 2,
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
                    type_id: 1,
                    schema_reversed: false,
                },
                Neighbor {
                    target: 2,
                    type_id: 2,
                    schema_reversed: false,
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
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
                RawEdge {
                    source: 0,
                    target: 2,
                    type_id: 1,
                    weight: None,
                    schema_reversed: false,
                },
            ],
            false,
        );
        let mut inserts = OverlayInserts::new();
        inserts.insert(0, vec![(3, 1, false), (2, 1, false), (3, 1, false)]);
        let mut deletes = OverlayDeletes::new();
        deletes.insert(0, HashSet::from([(1, 1)]));
        let neighbors = OverlayNeighbors::new(&store, &inserts, &deletes);

        let actual = neighbors.neighbors(0).collect::<Vec<_>>();

        assert_eq!(
            actual,
            vec![
                Neighbor {
                    target: 2,
                    type_id: 1,
                    schema_reversed: false,
                },
                Neighbor {
                    target: 3,
                    type_id: 1,
                    schema_reversed: false,
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
                type_id: 1,
                weight: None,
                schema_reversed: false,
            }],
            false,
        );
        let mut inserts = OverlayInserts::new();
        inserts.insert(0, vec![(1, 1, true)]);
        let deletes = OverlayDeletes::new();
        let neighbors = OverlayNeighbors::new(&store, &inserts, &deletes);

        let actual = neighbors.neighbors(0).collect::<Vec<_>>();

        assert_eq!(
            actual,
            vec![
                Neighbor {
                    target: 1,
                    type_id: 1,
                    schema_reversed: false,
                },
                Neighbor {
                    target: 1,
                    type_id: 1,
                    schema_reversed: true,
                },
            ]
        );
    }

    proptest! {
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
                    type_id,
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
