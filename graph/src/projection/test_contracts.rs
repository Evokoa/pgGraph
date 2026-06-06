//! Contract tests for the durable projection build sequence.
//!
//! These tests record the required behavior and fixture call sites that durable
//! projection modules must turn green. Implemented contracts pass; future
//! contracts fail by default so phase progress is visible in the normal suite.

use super::test_fixtures::{
    edge_store_from_tuples, NormalizedMutation, ProjectionArtifactDir, SyntheticSyncOperation,
    SyntheticSyncRow,
};
use crate::projection::manifest::{ProjectionManifest, VALIDATION_STATUS_VALID};
use crate::projection::neighbors::CsrNeighbors;
use crate::types::TraversalDirection;

fn production_feature_absent(feature: &str) -> ! {
    panic!("{feature} is not implemented yet")
}

#[test]
fn projection_manifest_roundtrips_base_only_generation() {
    let dir = ProjectionArtifactDir::new("projection_manifest_roundtrips_base_only_generation");
    let manifest_path = dir.manifest_path(1);
    let manifest = ProjectionManifest::base_only(
        1,
        manifest_path.to_string_lossy(),
        "xxh3:base",
        2,
        42,
        1_700_000,
    );

    let json = manifest.to_pretty_json().expect("manifest encodes");
    let decoded = ProjectionManifest::from_json(&json).expect("manifest decodes");

    assert_eq!(decoded, manifest);
    assert_eq!(decoded.validation_status, VALIDATION_STATUS_VALID);
}

#[test]
fn delta_segment_roundtrips_edge_topology_weight_and_delete_sections() {
    let dir = ProjectionArtifactDir::new(
        "delta_segment_roundtrips_edge_topology_weight_and_delete_sections",
    );
    let _segment_path = dir.segment_path(1, 0);
    let _weighted = NormalizedMutation {
        generation_id: 1,
        direction: TraversalDirection::Out,
        source: 0,
        target: 1,
        type_id: 2,
        weight: Some(7),
        tombstone: false,
    };
    let _delete = NormalizedMutation {
        tombstone: true,
        .._weighted.clone()
    };

    production_feature_absent("edge topology, weight, and delete segment sections");
}

#[test]
fn delta_segment_roundtrips_node_resolution_filter_tenant_sections() {
    let dir = ProjectionArtifactDir::new(
        "delta_segment_roundtrips_node_resolution_filter_tenant_sections",
    );
    let _segment_path = dir.segment_path(1, 1);

    production_feature_absent("node, resolution, filter, and tenant segment sections");
}

#[test]
fn projection_ingest_committed_edge_insert_publishes_l0_manifest() {
    let _row = SyntheticSyncRow {
        log_id: 1,
        generation_id: 1,
        table_oid: 100,
        source: 0,
        target: 1,
        type_id: 2,
        weight: None,
        operation: SyntheticSyncOperation::InsertEdge,
    };

    production_feature_absent("committed edge ingestion and L0 manifest publishing");
}

#[test]
fn layered_neighbors_equal_full_rebuild_for_insert_delete_sequence() {
    let full_rebuild = edge_store_from_tuples(4, &[(0, 2, 1), (0, 3, 1)]);
    let _expected = CsrNeighbors::new(&full_rebuild);

    production_feature_absent("layered neighbors over durable segment sequences");
}

#[test]
fn status_reports_manifest_watermark_segments_chunks_gc_and_repair() {
    production_feature_absent("durable projection status and diagnostics");
}
