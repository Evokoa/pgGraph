//! Contract tests for the durable projection build sequence.
//!
//! These tests record the required behavior and fixture call sites that durable
//! projection modules must turn green. Implemented contracts pass; future
//! contracts fail by default so phase progress is visible in the normal suite.

use super::test_fixtures::{edge_store_from_tuples, NormalizedMutation, ProjectionArtifactDir};
use crate::projection::ingest::{ProjectionIngester, ProjectionSyncRow};
use crate::projection::manifest::{
    ProjectionManifest, ProjectionManifestStore, VALIDATION_STATUS_VALID,
};
use crate::projection::neighbors::CsrNeighbors;
use crate::projection::normalize::{MutationBufferLimits, MutationOperation};
use crate::projection::segment::{
    DeltaSegment, SegmentEdge, SegmentEdgeWeight, SegmentFilterValue, SegmentKind,
    SegmentNodeState, SegmentResolution, SegmentTenant,
};
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
    let segment_path = dir.segment_path(1, 0);
    let weighted = NormalizedMutation {
        generation_id: 1,
        direction: TraversalDirection::Out,
        source: 0,
        target: 1,
        type_id: 2,
        weight: Some(7),
        tombstone: false,
    };
    let delete = NormalizedMutation {
        tombstone: true,
        ..weighted.clone()
    };
    let mut segment = DeltaSegment::new(SegmentKind::Edge, 0, TraversalDirection::Out, 0, 4, 42)
        .expect("segment constructs");
    segment.edge_inserts.push(SegmentEdge {
        source: weighted.source,
        target: weighted.target,
        type_id: weighted.type_id,
    });
    segment.edge_weights.push(SegmentEdgeWeight {
        source: weighted.source,
        target: weighted.target,
        type_id: weighted.type_id,
        weight: weighted.weight.expect("fixture has weight"),
    });
    segment.edge_deletes.push(SegmentEdge {
        source: delete.source,
        target: delete.target,
        type_id: delete.type_id,
    });

    segment
        .write_to_path(&segment_path)
        .expect("segment writes");
    let decoded = DeltaSegment::read_from_path(&segment_path).expect("segment reads");

    assert_eq!(decoded.edge_inserts, segment.edge_inserts);
    assert_eq!(decoded.edge_weights, segment.edge_weights);
    assert_eq!(decoded.edge_deletes, segment.edge_deletes);
}

#[test]
fn delta_segment_roundtrips_node_resolution_filter_tenant_sections() {
    let dir = ProjectionArtifactDir::new(
        "delta_segment_roundtrips_node_resolution_filter_tenant_sections",
    );
    let segment_path = dir.segment_path(1, 1);
    let mut segment = DeltaSegment::new(SegmentKind::Node, 0, TraversalDirection::Any, 0, 4, 43)
        .expect("segment constructs");
    segment.node_states.push(SegmentNodeState {
        node_idx: 1,
        active: true,
    });
    segment.resolutions.push(SegmentResolution {
        table_oid: 100,
        pk_hash: 7_001,
        node_idx: 1,
        tombstone: false,
    });
    segment.filters.push(SegmentFilterValue {
        node_idx: 1,
        column_id: 2,
        value: 99,
        tombstone: false,
    });
    segment.tenants.push(SegmentTenant {
        node_idx: 1,
        tenant_hash: 8_002,
        tombstone: true,
    });

    segment
        .write_to_path(&segment_path)
        .expect("segment writes");
    let decoded = DeltaSegment::read_from_path(&segment_path).expect("segment reads");

    assert_eq!(decoded.node_states, segment.node_states);
    assert_eq!(decoded.resolutions, segment.resolutions);
    assert_eq!(decoded.filters, segment.filters);
    assert_eq!(decoded.tenants, segment.tenants);
}

#[test]
fn projection_ingest_committed_edge_insert_publishes_l0_manifest() {
    let dir = ProjectionArtifactDir::new("projection_ingest_committed_edge_contract");
    std::fs::write(dir.path().join("base.pggraph"), b"base").expect("base artifact writes");
    ProjectionManifestStore::new(dir.path())
        .publish(&ProjectionManifest::base_only(
            1,
            "base.pggraph",
            "crc32:00000000",
            1,
            0,
            1,
        ))
        .expect("base manifest publishes");
    let ingester = ProjectionIngester::new(dir.path(), "base.pggraph", "crc32:00000000", 1);
    let row = ProjectionSyncRow {
        sync_id: 1,
        generation_id: 1,
        committed: true,
        operation: MutationOperation::InsertEdge,
        direction: TraversalDirection::Out,
        source: 0,
        target: 1,
        type_id: 2,
        weight: None,
        table_oid: None,
        pk_hash: None,
        node_idx: None,
        filter_column_id: None,
        filter_value: None,
        tenant_hash: None,
    };

    let result = ingester
        .ingest_committed_rows(&[row], MutationBufferLimits::new(10, 10_000))
        .expect("ingestion publishes");
    let manifest = result.manifest.expect("manifest published");
    let segment = DeltaSegment::read_from_path(&dir.path().join(&manifest.segments[0].path))
        .expect("segment reads");

    assert_eq!(manifest.sync_watermark, 1);
    assert_eq!(manifest.segments.len(), 1);
    assert_eq!(
        segment.edge_inserts,
        vec![SegmentEdge {
            source: 0,
            target: 1,
            type_id: 2,
        }]
    );
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
