//! # graph — Sub-millisecond graph traversal for PostgreSQL
//!
//! `graph` is a PostgreSQL extension written in Rust (via pgrx) that lets you
//! query your existing relational tables as a graph. No external services.
//! No ETL pipelines. No separate graph database. The current public API is
//! PostgreSQL SQL functions, including a GQL-compatible subset exposed through
//! `graph.gql()`.
//!
//! See: `docs/user_guide/index.mdx` and `docs/contributor_guide/index.mdx`

#![cfg_attr(
    not(any(test, feature = "pg_test")),
    deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)
)]

use pgrx::prelude::*;
use std::cell::RefCell;

// Module declarations ordered by dependency layer.
mod acl;
mod api_types;
mod bfs;
mod builder;
mod catalog;
mod config;
mod connected_components;
mod cypher;
mod discover;
mod edge_store;
mod engine;
mod filter_index;
mod gql;
mod node_store;
mod path_finder;
mod persistence;
mod projection;
mod query;
mod quote;
mod resolution_index;
mod safety;
mod sql_aggregation;
mod sql_build;
#[allow(
    dead_code,
    reason = "pgrx discovers SQL and background-worker entrypoints through attributes"
)]
mod sql_facade;
mod sql_filters;
mod sql_hydration;
mod sql_jobs;
mod sql_search;
mod sql_sync;
mod sql_traversal;
mod sync;
mod types;

use engine::Engine;

#[cfg(feature = "pg_test")]
use api_types::{BuildExecutionResult, MaintenanceExecutionResult};
#[cfg(feature = "pg_test")]
use catalog::{
    insert_registered_table, read_catalog, validate_numeric_column, validate_registered_table,
};
#[cfg(feature = "pg_test")]
use quote::quote_literal as sql_literal;
#[cfg(feature = "pg_test")]
use sql_facade::ensure_current_graph;
#[cfg(any(test, feature = "fuzzing"))]
use sql_filters::validate_structured_operator_shape;
#[cfg(feature = "pg_test")]
use sql_jobs::{
    create_build_job, create_maintenance_job, run_build_job, update_build_job_completed,
    update_build_job_failed, update_build_job_progress, update_build_job_started,
    update_maintenance_job_completed, update_maintenance_job_failed,
    update_maintenance_job_progress, update_maintenance_job_started,
};
#[cfg(feature = "pg_test")]
use sql_sync::current_sync_mode;
#[cfg(any(test, feature = "fuzzing"))]
use sql_sync::parse_sync_properties;
#[cfg(any(test, feature = "fuzzing"))]
use sql_traversal::parse_node_ref_json_parts;
#[cfg(any(test, feature = "fuzzing", feature = "pg_test"))]
use sql_traversal::validate_traverse_options;

/// Helpers exported only for fuzz targets and unit tests.
///
/// These wrappers expose parser and persistence boundaries that can run without
/// requiring a live PostgreSQL backend.
#[cfg(any(test, feature = "fuzzing"))]
pub mod fuzz_support {
    pub use crate::persistence::load_graph_file;

    /// Parse sync JSON properties through the same lossy boundary used by SQL
    /// sync replay. Intended for fuzz targets.
    pub fn parse_sync_properties(raw: Option<&str>) -> Vec<(String, String)> {
        crate::parse_sync_properties(raw)
    }

    /// Validate structured-filter operator shape without touching catalog
    /// state. Intended for fuzz targets.
    pub fn validate_structured_operator_shape(operator: &str, value: &serde_json::Value) -> bool {
        crate::validate_structured_operator_shape("fuzz_column", operator, value).is_ok()
    }

    /// Validate traversal direction, strategy, and uniqueness parsing without
    /// requiring a PostgreSQL backend. Intended for fuzz targets.
    pub fn validate_traverse_options(direction: &str, strategy: &str, uniqueness: &str) -> bool {
        crate::validate_traverse_options(direction, None, strategy, uniqueness).is_ok()
    }

    /// Parse a `graph.node_ref_string()` payload without resolving the table
    /// through Postgres. Intended for fuzz targets.
    pub fn parse_node_ref_json_parts(value: &serde_json::Value) -> bool {
        crate::parse_node_ref_json_parts(value).is_ok()
    }

    /// Decode a projection segment without touching PostgreSQL. Intended for
    /// fuzz targets and unit tests.
    pub fn load_projection_segment(bytes: &[u8]) -> bool {
        crate::projection::segment::DeltaSegment::from_bytes(bytes).is_ok()
    }

    /// Return valid projection segment seed bytes for named fuzz corpus tokens.
    pub fn projection_segment_seed_bytes(name: &str) -> Option<Vec<u8>> {
        crate::projection::segment::fuzz_seed_bytes(name)
    }

    /// Decode a projection manifest without touching PostgreSQL. Intended for
    /// fuzz targets and unit tests.
    pub fn load_projection_manifest(raw: &str) -> bool {
        crate::projection::manifest::ProjectionManifest::from_json(raw).is_ok()
    }

    /// Parse a GQL query through the pgrx-free frontend. Intended for fuzz
    /// targets and unit tests.
    pub fn parse_gql_query(query: &str) -> bool {
        crate::gql::parse(query).is_ok()
    }

    /// Parse an openCypher compatibility query without touching PostgreSQL.
    /// Intended for fuzz targets and unit tests.
    pub fn parse_cypher_query(query: &str) -> bool {
        crate::cypher::parse_statement(query).is_ok()
    }
}

/// Public re-exports for criterion benchmarks.
///
/// Benchmarks link against the `rlib` and need access to internal
/// data structures. This module is always available (bench targets
/// compile with `--lib`) but not part of the pgrx extension surface.
pub mod bench_support {
    use std::collections::{HashMap, HashSet};

    pub use crate::bfs::{execute as bfs_execute, BfsConfig, BfsResult};
    pub use crate::edge_store::{EdgeStore as EdgeStoreBuilder, RawEdge};
    pub use crate::filter_index::{FilterColumnType, FilterIndex as FilterIndexBuilder};
    pub use crate::node_store::NodeStore as NodeStoreBuilder;
    use crate::projection::layered::{LayeredNeighbors, SegmentProvider};
    use crate::projection::neighbors::NeighborSource;
    use crate::projection::segment::{DeltaSegment, SegmentEdge, SegmentEdgeWeight, SegmentKind};
    pub use crate::types::{EdgeTypeFilter, FilterCondition, FilterOp};
    use crate::types::{TraversalDirection, WeightedPathStep};

    /// Durable projection shape exercised by release-readiness benchmarks.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LayeredProjectionBenchScenario {
        /// Base CSR routed through the layered projection source without deltas.
        BaseOnly,
        /// One small L0 segment with sparse insert/delete rows.
        SmallL0,
        /// Many L0 segments, each with a sparse mutation slice.
        ManyL0,
        /// One compacted L1 segment carrying the equivalent sparse mutation set.
        CompactedL1,
        /// One compacted L2 segment carrying the equivalent sparse mutation set.
        CompactedL2,
        /// One compacted L2 segment plus a rewritten dirty base chunk range.
        DirtyChunkRewrite,
        /// Durable segment plus Engine-owned committed overlay maps.
        TxDeltaOverlay,
        /// Durable weighted edges used by Dijkstra.
        WeightedPath,
        /// Relationship-expansion-shaped segment fanout for GQL path matching.
        GqlRelationshipExpansion,
    }

    /// Execute BFS over a deterministic durable layered projection scenario.
    pub fn bfs_layered_projection_execute(
        node_store: &NodeStoreBuilder,
        edge_store: &EdgeStoreBuilder,
        filter_index: &FilterIndexBuilder,
        config: &BfsConfig,
        scenario: LayeredProjectionBenchScenario,
    ) -> BfsResult {
        let layered = layered_neighbors(edge_store, scenario);
        crate::bfs::execute_with_neighbors(node_store, &layered, filter_index, config)
    }

    /// Execute a weighted shortest path over a durable layered projection.
    pub fn weighted_layered_projection_path(
        node_store: &NodeStoreBuilder,
        edge_store: &EdgeStoreBuilder,
        source: u32,
        target: u32,
    ) -> Option<Vec<WeightedPathStep>> {
        let layered = layered_neighbors(edge_store, LayeredProjectionBenchScenario::WeightedPath);
        let registry = ["".to_string(), "weighted".to_string()];
        crate::path_finder::weighted_shortest_path_with_neighbors(
            node_store, &layered, source, target, &registry,
        )
    }

    /// Count the GQL-shaped relationship expansion fanout for one source node.
    pub fn gql_layered_relationship_expansion_count(
        edge_store: &EdgeStoreBuilder,
        source: u32,
    ) -> usize {
        let layered = layered_neighbors(
            edge_store,
            LayeredProjectionBenchScenario::GqlRelationshipExpansion,
        );
        layered
            .for_direction(TraversalDirection::Out)
            .neighbors(source)
            .count()
    }

    fn layered_neighbors(
        edge_store: &EdgeStoreBuilder,
        scenario: LayeredProjectionBenchScenario,
    ) -> LayeredNeighbors<'_> {
        match scenario {
            LayeredProjectionBenchScenario::BaseOnly => LayeredNeighbors::new(edge_store, vec![]),
            LayeredProjectionBenchScenario::SmallL0 => {
                LayeredNeighbors::new(edge_store, sparse_segments(edge_store.node_count(), 1, 0))
            }
            LayeredProjectionBenchScenario::ManyL0 => {
                LayeredNeighbors::new(edge_store, sparse_segments(edge_store.node_count(), 8, 0))
            }
            LayeredProjectionBenchScenario::CompactedL1 => {
                LayeredNeighbors::new(edge_store, sparse_segments(edge_store.node_count(), 1, 1))
            }
            LayeredProjectionBenchScenario::CompactedL2 => {
                LayeredNeighbors::new(edge_store, sparse_segments(edge_store.node_count(), 1, 2))
            }
            LayeredProjectionBenchScenario::DirtyChunkRewrite => dirty_chunk_neighbors(edge_store),
            LayeredProjectionBenchScenario::TxDeltaOverlay => {
                let durable = sparse_segments(edge_store.node_count(), 1, 0);
                let overlays = committed_overlay(edge_store.node_count());
                LayeredNeighbors::new_with_options(
                    edge_store,
                    None,
                    durable,
                    None,
                    Some(overlays),
                    None,
                )
            }
            LayeredProjectionBenchScenario::WeightedPath => {
                LayeredNeighbors::new(edge_store, weighted_path_segments(edge_store.node_count()))
            }
            LayeredProjectionBenchScenario::GqlRelationshipExpansion => LayeredNeighbors::new(
                edge_store,
                relationship_expansion_segments(edge_store.node_count()),
            ),
        }
    }

    fn sparse_segments(node_count: u32, segment_count: u32, level: u8) -> Vec<DeltaSegment> {
        (0..segment_count)
            .map(|segment_idx| {
                let mut segment = edge_segment(node_count, level, i64::from(segment_idx + 1));
                let stride = 257 + segment_idx.saturating_mul(17);
                for source in (segment_idx..node_count).step_by(stride as usize) {
                    let target = source.wrapping_add(17 + segment_idx) % node_count;
                    segment.edge_inserts.push(SegmentEdge {
                        source,
                        target,
                        type_id: 1,
                    });
                    if source + 1 < node_count {
                        segment.edge_deletes.push(SegmentEdge {
                            source,
                            target: source + 1,
                            type_id: 1,
                        });
                    }
                }
                segment
            })
            .collect()
    }

    fn dirty_chunk_segments(node_count: u32) -> Vec<DeltaSegment> {
        if node_count == 0 {
            return vec![edge_segment(0, 2, 1)];
        }
        let range_end = node_count.min(2_048);
        let mut segment = DeltaSegment::new(
            SegmentKind::Edge,
            2,
            TraversalDirection::Out,
            0,
            range_end,
            1,
        )
        .expect("benchmark dirty chunk segment range is valid");
        for source in 0..range_end {
            segment.edge_inserts.push(SegmentEdge {
                source,
                target: source.wrapping_add(3) % node_count,
                type_id: 1,
            });
        }
        vec![segment]
    }

    fn dirty_chunk_neighbors(edge_store: &EdgeStoreBuilder) -> LayeredNeighbors<'_> {
        let provider = BenchSegmentProvider {
            segments: Vec::new(),
            base_chunks: dirty_chunk_segments(edge_store.node_count()),
        };
        LayeredNeighbors::from_provider(edge_store, &provider)
            .expect("benchmark dirty chunk provider is valid")
    }

    fn weighted_path_segments(node_count: u32) -> Vec<DeltaSegment> {
        let mut segment = edge_segment(node_count, 0, 1);
        let chain_end = node_count.min(128);
        for source in 0..chain_end.saturating_sub(1) {
            let target = source + 1;
            segment.edge_inserts.push(SegmentEdge {
                source,
                target,
                type_id: 1,
            });
            segment.edge_weights.push(SegmentEdgeWeight {
                source,
                target,
                type_id: 1,
                weight: 1,
            });
        }
        vec![segment]
    }

    fn relationship_expansion_segments(node_count: u32) -> Vec<DeltaSegment> {
        let mut segment = edge_segment(node_count, 0, 1);
        let fanout = node_count.min(256);
        for target in 1..fanout {
            segment.edge_inserts.push(SegmentEdge {
                source: 0,
                target,
                type_id: 1,
            });
        }
        vec![segment]
    }

    fn committed_overlay(
        node_count: u32,
    ) -> (
        HashMap<u32, Vec<(u32, u8)>>,
        HashMap<u32, HashSet<(u32, u8)>>,
    ) {
        let mut inserts = HashMap::new();
        let mut deletes = HashMap::new();
        for source in (0..node_count).step_by(509) {
            inserts.insert(source, vec![(source.wrapping_add(23) % node_count, 1)]);
            deletes.insert(
                source,
                HashSet::from([(source.wrapping_add(1) % node_count, 1)]),
            );
        }
        (inserts, deletes)
    }

    fn edge_segment(node_count: u32, level: u8, sync_watermark: i64) -> DeltaSegment {
        DeltaSegment::new(
            SegmentKind::Edge,
            level,
            TraversalDirection::Out,
            0,
            node_count,
            sync_watermark,
        )
        .expect("benchmark segment range is valid")
    }

    struct BenchSegmentProvider {
        segments: Vec<DeltaSegment>,
        base_chunks: Vec<DeltaSegment>,
    }

    impl SegmentProvider for BenchSegmentProvider {
        fn load_segments(&self) -> crate::safety::GraphResult<Vec<DeltaSegment>> {
            Ok(self.segments.clone())
        }

        fn load_base_chunks(&self) -> crate::safety::GraphResult<Vec<DeltaSegment>> {
            Ok(self.base_chunks.clone())
        }
    }

    #[cfg(test)]
    mod tests {
        use std::fs;
        use std::path::Path;
        use std::time::{Duration, Instant};

        use super::*;
        use crate::projection::chunk::EdgeStoreChunkSource;
        use crate::projection::compact::{compact_generation, CompactionBudgets};
        use crate::projection::gc::{collect_projection_garbage_with_config, ProjectionGcConfig};
        use crate::projection::ingest::{ProjectionIngester, ProjectionSyncRow};
        use crate::projection::manifest::{
            ManifestChunkRef, ManifestFileRef, ManifestSegmentRef, ProjectionManifest,
            ProjectionManifestStore,
        };
        use crate::projection::normalize::{MutationBufferLimits, MutationOperation};
        use crate::projection::recovery::repair_active_base_chunks;
        use crate::projection::test_fixtures::ProjectionArtifactDir;

        const RELEASE_CONTRACT_LIMIT: Duration = Duration::from_millis(250);

        fn release_fixture() -> (NodeStoreBuilder, EdgeStoreBuilder, FilterIndexBuilder) {
            let mut nodes = NodeStoreBuilder::new();
            for idx in 0..1_024 {
                nodes.add_node(100, format!("PK-{idx}"));
            }
            let mut edges = Vec::new();
            for source in 0..1_023 {
                edges.push(RawEdge {
                    source,
                    target: source + 1,
                    type_id: 1,
                    weight: None,
                });
            }
            let edge_store = EdgeStoreBuilder::try_from_edges(nodes.node_count(), edges, false)
                .expect("release fixture edges are in range");
            (nodes, edge_store, FilterIndexBuilder::new())
        }

        fn release_bfs_config() -> BfsConfig {
            BfsConfig {
                seed_node: 0,
                max_depth: 8,
                max_nodes: 10_000,
                max_frontier: 10_000,
                edge_type_filter: EdgeTypeFilter::All,
                filter_ops: Vec::new(),
                tenant: None,
                tenanted_table_oids: HashSet::new(),
                tenant_membership: HashMap::new(),
                overlay_insert_edges: HashMap::new(),
                overlay_deleted_edges: HashMap::new(),
            }
        }

        #[test]
        fn bfs_layered_projection_no_unbounded_regression() {
            let (nodes, edges, filters) = release_fixture();
            let config = release_bfs_config();
            let started = Instant::now();
            let result = bfs_layered_projection_execute(
                &nodes,
                &edges,
                &filters,
                &config,
                LayeredProjectionBenchScenario::ManyL0,
            );
            assert!(result.visited.len() >= 9);
            assert!(started.elapsed() < RELEASE_CONTRACT_LIMIT);
        }

        #[test]
        fn gql_layered_relationship_expansion_no_unbounded_regression() {
            let (_, edges, _) = release_fixture();
            let started = Instant::now();
            let count = gql_layered_relationship_expansion_count(&edges, 0);
            assert!(count >= 255);
            assert!(started.elapsed() < RELEASE_CONTRACT_LIMIT);
        }

        #[test]
        fn weighted_path_layered_projection_no_unbounded_regression() {
            let (nodes, edges, _) = release_fixture();
            let started = Instant::now();
            let path = weighted_layered_projection_path(&nodes, &edges, 0, 127)
                .expect("durable weighted segment should connect the chain");
            assert_eq!(path.len(), 128);
            assert!(started.elapsed() < RELEASE_CONTRACT_LIMIT);
        }

        #[test]
        fn projection_ingest_publish_latency_under_threshold() {
            let dir = ProjectionArtifactDir::new("projection_ingest_publish_latency");
            write_base_artifact(dir.path());
            let ingester = ProjectionIngester::new(dir.path(), "base.pggraph", "crc32:base", 1);
            let rows = (0..64)
                .map(|idx| ProjectionSyncRow {
                    sync_id: u64::from(idx + 1),
                    generation_id: 1,
                    committed: true,
                    operation: MutationOperation::InsertEdge,
                    direction: TraversalDirection::Out,
                    source: idx,
                    target: (idx + 1) % 64,
                    type_id: 1,
                    weight: Some(1),
                    table_oid: None,
                    pk_hash: None,
                    node_idx: None,
                    filter_column_id: None,
                    filter_value: None,
                    tenant_hash: None,
                })
                .collect::<Vec<_>>();
            let started = Instant::now();
            let result = ingester
                .ingest_committed_rows(&rows, MutationBufferLimits::new(1_000, 1_000_000))
                .expect("projection ingest publishes");
            assert_eq!(result.rows_ingested, 64);
            assert!(result.segments_published >= 1);
            assert!(started.elapsed() < RELEASE_CONTRACT_LIMIT);
        }

        #[test]
        fn projection_compaction_latency_under_threshold() {
            let dir = ProjectionArtifactDir::new("projection_compaction_latency");
            let base = release_edge_store();
            let previous =
                publish_manifest_with_segments(&dir, 1, sparse_segments(base.node_count(), 8, 0));
            let budgets = CompactionBudgets {
                max_rows: 10_000,
                max_bytes: 10_000_000,
                max_segments: 1_000,
                max_elapsed: Duration::from_secs(60),
                dirty_chunk_segment_threshold: None,
            };
            let started = Instant::now();
            let result = compact_generation(dir.path(), &previous, &base, budgets)
                .expect("projection compaction publishes");
            assert_eq!(result.segments_compacted, 8);
            assert!(result.manifest.generation_id > previous.generation_id);
            assert!(started.elapsed() < RELEASE_CONTRACT_LIMIT);
        }

        #[test]
        fn projection_gc_latency_under_threshold() {
            let dir = ProjectionArtifactDir::new("projection_gc_latency");
            write_base_artifact(dir.path());
            let obsolete = dir.path().join("obsolete.pggraph-delta");
            fs::write(&obsolete, b"obsolete").expect("obsolete file writes");
            let store = ProjectionManifestStore::new(dir.path());
            let mut first = ProjectionManifest::base_only(1, "base.pggraph", "crc32:base", 1, 1, 1);
            first.obsolete_files.push(ManifestFileRef {
                path: "obsolete.pggraph-delta".to_string(),
                bytes: 8,
            });
            store.publish(&first).expect("first manifest publishes");
            let second = ProjectionManifest::base_only(2, "base.pggraph", "crc32:base", 1, 2, 2);
            store.publish(&second).expect("second manifest publishes");
            let started = Instant::now();
            let summary = collect_projection_garbage_with_config(
                dir.path(),
                ProjectionGcConfig {
                    retained_generation_floor: 1,
                },
            )
            .expect("projection GC collects");
            assert_eq!(summary.deleted_files, 1);
            assert!(!obsolete.exists());
            assert!(started.elapsed() < RELEASE_CONTRACT_LIMIT);
        }

        #[test]
        fn projection_repair_latency_under_threshold() {
            let dir = ProjectionArtifactDir::new("projection_repair_latency");
            let base = release_edge_store();
            let source = EdgeStoreChunkSource::new(&base);
            let manifest = publish_manifest_with_base_chunk(&dir, &base);
            let chunk_path = dir.path().join(&manifest.base_chunks[0].path);
            fs::write(&chunk_path, b"corrupt chunk").expect("chunk corruption writes");
            let started = Instant::now();
            let result = repair_active_base_chunks(dir.path(), &source)
                .expect("projection repair runs")
                .expect("corrupt chunk is repaired");
            assert_eq!(result.chunks_rewritten, 1);
            assert!(result.manifest.generation_id > manifest.generation_id);
            assert!(started.elapsed() < RELEASE_CONTRACT_LIMIT);
        }

        fn write_base_artifact(root: &Path) {
            fs::write(root.join("base.pggraph"), b"base").expect("base artifact writes");
        }

        fn release_edge_store() -> EdgeStoreBuilder {
            EdgeStoreBuilder::from_edges(
                128,
                (0..127)
                    .map(|source| RawEdge {
                        source,
                        target: source + 1,
                        type_id: 1,
                        weight: Some(1),
                    })
                    .collect(),
                true,
            )
        }

        fn publish_manifest_with_segments(
            dir: &ProjectionArtifactDir,
            generation_id: u64,
            segments: Vec<DeltaSegment>,
        ) -> ProjectionManifest {
            write_base_artifact(dir.path());
            let mut manifest = ProjectionManifest::base_only(
                generation_id,
                "base.pggraph",
                "crc32:base",
                1,
                10,
                1,
            );
            for (idx, segment) in segments.iter().enumerate() {
                let relative = format!(
                    "projection-generation-{generation_id:020}-segment-{idx:08}.pggraph-delta"
                );
                let path = dir.path().join(&relative);
                segment.write_to_path(&path).expect("segment writes");
                manifest
                    .segments
                    .push(segment_ref(&relative, &path, segment));
            }
            ProjectionManifestStore::new(dir.path())
                .publish(&manifest)
                .expect("manifest publishes");
            manifest
        }

        fn publish_manifest_with_base_chunk(
            dir: &ProjectionArtifactDir,
            base: &EdgeStoreBuilder,
        ) -> ProjectionManifest {
            let mut manifest = publish_manifest_with_segments(dir, 1, Vec::new());
            let chunk = dirty_chunk_segments(base.node_count())
                .into_iter()
                .next()
                .expect("chunk fixture exists");
            let relative =
                "projection-generation-00000000000000000001-chunk-00000000.pggraph-delta";
            let path = dir.path().join(relative);
            chunk.write_to_path(&path).expect("chunk writes");
            manifest.base_chunks.push(ManifestChunkRef {
                path: relative.to_string(),
                checksum: checksum(&path),
                source_start: chunk.header.source_start,
                source_end: chunk.header.source_end,
                dirty_source_count: chunk.header.source_end - chunk.header.source_start,
                dirty_edge_count: chunk.edge_inserts.len() as u32,
            });
            let store = ProjectionManifestStore::new(dir.path());
            let mut chunk_manifest = manifest.clone();
            chunk_manifest.generation_id = 2;
            chunk_manifest.previous_generation_id = Some(1);
            store
                .publish(&chunk_manifest)
                .expect("chunk manifest publishes");
            chunk_manifest
        }

        fn segment_ref(relative: &str, path: &Path, segment: &DeltaSegment) -> ManifestSegmentRef {
            ManifestSegmentRef {
                path: relative.to_string(),
                checksum: checksum(path),
                level: segment.header.level,
                source_start: segment.header.source_start,
                source_end: segment.header.source_end,
                sync_watermark: segment.header.sync_watermark,
            }
        }

        fn checksum(path: &Path) -> String {
            let bytes = fs::read(path).expect("artifact checksum reads");
            format!("crc32:{:08x}", crc32fast::hash(&bytes))
        }
    }
}

::pgrx::pg_module_magic!(name, version);
::pgrx::extension_sql_file!(
    "../sql/bootstrap.sql",
    name = "graph_bootstrap_sql",
    requires = [auto_discover]
);

// Declare the 'graph' schema so pgrx can satisfy control-file schema checks.
#[pg_schema]
mod graph {}

// Thread-local engine instance (one per Postgres backend process)
thread_local! {
    static ENGINE: RefCell<Engine> = RefCell::new(Engine::new());
}

// ─────────────────────────────────────────────────────────────────────
// Extension lifecycle
// ─────────────────────────────────────────────────────────────────────

/// Called when the extension is loaded into a backend.
///
/// Registers GUC parameters and eagerly pre-warms the OS page cache for the
/// `.pggraph` file (if it exists). This does NOT load the graph into the engine —
/// that happens lazily on the first query via `maybe_auto_load()`. What this
/// does is call `madvise(MADV_WILLNEED)` to tell the kernel to prefetch the
/// file pages into RAM, so the subsequent mmap in `load_graph_file()` won't
/// block on disk I/O.
///
/// For best results, add to `postgresql.conf`:
/// ```text
/// shared_preload_libraries = 'graph'
/// ```
/// This runs `_PG_init()` at postmaster startup, giving later backend
/// processes a warm page-cache path when the kernel keeps those pages resident.
#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    config::register_gucs();
    projection::tx_delta::register_transaction_callbacks();

    // Eagerly pre-warm the OS page cache for the .pggraph file.
    let Ok(path) = persistence::graph_file_path() else {
        return;
    };
    if path.exists() {
        match std::fs::File::open(&path) {
            Ok(file) => {
                // SAFETY: The file descriptor stays alive for the duration of
                // this temporary mapping, and the mapping is only used for
                // read-only page-cache advice.
                if let Ok(mmap) = unsafe { memmap2::Mmap::map(&file) } {
                    // madvise(MADV_WILLNEED) — ask the kernel to page in the
                    // entire file. This is non-blocking: the kernel will
                    // asynchronously read pages from disk into the page cache.
                    #[cfg(unix)]
                    {
                        mmap.advise(memmap2::Advice::WillNeed).ok();
                    }
                    pgrx::log!(
                        "graph: pre-warmed page cache for {} ({:.1} MB)",
                        path.display(),
                        mmap.len() as f64 / 1_048_576.0
                    );
                    // mmap is dropped here — that's fine. The kernel keeps the
                    // pages in the page cache regardless.
                }
            }
            Err(_) => {
                // Not critical — auto-load will handle it later
            }
        }
    }

    pgrx::log!("graph: extension loaded (v{})", env!("CARGO_PKG_VERSION"));
}

// ─────────────────────────────────────────────────────────────────────
// Test module
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod pg_test;

/// Covers SQL API behavior through PostgreSQL, including registration,
/// discovery, build, search, traversal, path, component, and sync flows.
#[cfg(feature = "pg_test")]
#[pg_schema]
mod tests {
    include!("pg_tests/common.rs");
    include!("pg_tests/discovery.rs");
    include!("pg_tests/traversal_paths.rs");
    include!("pg_tests/filters.rs");
    include!("pg_tests/traversal_api.rs");
    include!("pg_tests/sync_config_build.rs");
    include!("pg_tests/registration_search.rs");
    include!("pg_tests/components_jobs.rs");
    include!("pg_tests/maintenance_admin.rs");
    include!("pg_tests/workflow_search_api.rs");
    include!("pg_tests/workflow_relationship_api.rs");
    include!("pg_tests/workflow_validation.rs");
    include!("pg_tests/synthetic_release.rs");
    include!("pg_tests/gql.rs");
    include!("pg_tests/cypher.rs");
}
