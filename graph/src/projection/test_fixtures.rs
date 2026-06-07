//! Test fixtures for durable projection development.
//!
//! These helpers define stable test inputs for manifest, segment,
//! normalization, ingestion, and layered-read phases. They are available only
//! to unit tests and do not affect the PostgreSQL extension surface.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::edge_store::{EdgeStore, RawEdge};
use crate::projection::neighbors::{Neighbor, NeighborSource};
use crate::types::TraversalDirection;

static NEXT_ARTIFACT_DIR_ID: AtomicU64 = AtomicU64::new(0);

/// Temporary projection artifact directory for unit tests.
#[derive(Debug)]
pub(crate) struct ProjectionArtifactDir {
    path: PathBuf,
}

impl ProjectionArtifactDir {
    /// Create a unique temporary projection artifact directory.
    ///
    /// # Panics
    ///
    /// Panics when the process cannot create the directory under the system
    /// temporary directory.
    pub(crate) fn new(test_name: &str) -> Self {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        for attempt in 0..128 {
            let id = NEXT_ARTIFACT_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pggraph-projection-{test_name}-{}-{created_at}-{id}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("test artifact directory can be created: {err}"),
            }
        }
        panic!("test artifact directory name kept colliding after bounded retries")
    }

    /// Return the root path for this artifact directory.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Return the path for a manifest generation file.
    pub(crate) fn manifest_path(&self, generation_id: u64) -> PathBuf {
        self.path
            .join(format!("projection-generation-{generation_id:020}.json"))
    }

    /// Return the path for a segment file.
    pub(crate) fn segment_path(&self, generation_id: u64, segment_id: u32) -> PathBuf {
        self.path.join(format!(
            "projection-generation-{generation_id:020}-segment-{segment_id:08}.pggraph-delta"
        ))
    }
}

impl Drop for ProjectionArtifactDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Synthetic sync operation for projection test fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyntheticSyncOperation {
    /// Insert or reactivate an edge.
    InsertEdge,
    /// Delete an edge.
    DeleteEdge,
    /// Insert or reactivate a node.
    UpsertNode,
    /// Delete or tombstone a node.
    DeleteNode,
}

/// Normalized mutation expected by segment-writer tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedMutation {
    /// Projection generation that owns the normalized mutation.
    pub(crate) generation_id: u64,
    /// Direction covered by the segment section.
    pub(crate) direction: TraversalDirection,
    /// Source node index.
    pub(crate) source: u32,
    /// Target node index.
    pub(crate) target: u32,
    /// Edge type identifier.
    pub(crate) type_id: u8,
    /// Optional edge weight.
    pub(crate) weight: Option<u32>,
    /// Whether this mutation is a tombstone.
    pub(crate) tombstone: bool,
}

/// Build an owned CSR store from fixture edge tuples.
pub(crate) fn edge_store_from_tuples(node_count: u32, edges: &[(u32, u32, u8)]) -> EdgeStore {
    EdgeStore::from_edges(
        node_count,
        edges
            .iter()
            .map(|&(source, target, type_id)| RawEdge {
                source,
                target,
                type_id,
                weight: None,
            })
            .collect(),
        false,
    )
}

/// Build an owned weighted CSR store from fixture edge tuples.
pub(crate) fn weighted_edge_store_from_tuples(
    node_count: u32,
    edges: &[(u32, u32, u8, u32)],
) -> EdgeStore {
    EdgeStore::from_edges(
        node_count,
        edges
            .iter()
            .map(|&(source, target, type_id, weight)| RawEdge {
                source,
                target,
                type_id,
                weight: Some(weight),
            })
            .collect(),
        true,
    )
}

/// Collect one source's neighbors into a deterministic vector.
pub(crate) fn collect_neighbors(source: &impl NeighborSource, node_idx: u32) -> Vec<Neighbor> {
    source.neighbors(node_idx).collect()
}

/// Assert that two neighbor sources expose the same full graph view.
pub(crate) fn assert_full_csr_equivalence(
    node_count: u32,
    expected: &impl NeighborSource,
    actual: &impl NeighborSource,
) {
    for node_idx in 0..node_count {
        assert_eq!(
            collect_neighbors(expected, node_idx),
            collect_neighbors(actual, node_idx),
            "neighbor mismatch at node {node_idx}"
        );
        assert_eq!(
            expected.neighbors_reversed(node_idx).collect::<Vec<_>>(),
            actual.neighbors_reversed(node_idx).collect::<Vec<_>>(),
            "reversed neighbor mismatch at node {node_idx}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::neighbors::CsrNeighbors;

    #[test]
    fn artifact_dir_builds_stable_projection_paths() {
        let dir = ProjectionArtifactDir::new("artifact_dir_builds_stable_projection_paths");

        assert!(dir.path().is_dir());
        assert!(dir
            .manifest_path(42)
            .ends_with("projection-generation-00000000000000000042.json"));
        assert!(dir.segment_path(42, 7).ends_with(
            "projection-generation-00000000000000000042-segment-00000007.pggraph-delta"
        ));
    }

    #[test]
    fn tuple_edges_build_expected_csr_neighbors() {
        let store = edge_store_from_tuples(3, &[(0, 1, 2), (0, 2, 3)]);
        let neighbors = CsrNeighbors::new(&store);

        assert_eq!(
            collect_neighbors(&neighbors, 0),
            vec![
                Neighbor {
                    target: 1,
                    type_id: 2,
                },
                Neighbor {
                    target: 2,
                    type_id: 3,
                },
            ]
        );
    }

    #[test]
    fn weighted_edges_and_equivalence_helpers_cover_future_surfaces() {
        let store = weighted_edge_store_from_tuples(3, &[(0, 1, 2, 10), (1, 2, 3, 20)]);
        let expected = CsrNeighbors::new(&store);
        let actual = CsrNeighbors::new(&store);

        assert_full_csr_equivalence(3, &expected, &actual);
    }

    #[test]
    fn synthetic_sync_operation_fixture_covers_all_mutation_kinds() {
        let operations = [
            SyntheticSyncOperation::InsertEdge,
            SyntheticSyncOperation::DeleteEdge,
            SyntheticSyncOperation::UpsertNode,
            SyntheticSyncOperation::DeleteNode,
        ];

        assert_eq!(operations.len(), 4);
    }
}
