//! P7 contracts for adaptive physical edge-type storage and base artifacts.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("graph crate must live under the repository root")
        .join(relative)
}

fn crate_source(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

fn repo_source(relative: &str) -> String {
    fs::read_to_string(repo_path(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn p7_owned_csr_selects_one_two_or_four_byte_edge_type_storage() {
    let store = crate_source("src/edge_store.rs");
    for seam in [
        "enum EdgeTypeWidth",
        "enum EdgeTypeStorage",
        "enum EdgeTypeSlice",
        "fn select_for_max_id",
        "fn edge_type_width",
        "fn edge_type_at",
        "adaptive_edge_type_storage_selects_reserved_boundaries",
        "adaptive_edge_type_storage_preserves_forward_reverse_and_weighted_neighbors",
        "adaptive_edge_type_storage_rejects_all_ones_physical_sentinels",
        "adaptive_edge_type_storage_accounts_exact_owned_and_mapped_widths",
    ] {
        assert!(store.contains(seam), "P7.1 is missing `{seam}`");
    }
    assert!(
        !store.contains("type_ids: Vec<u8>"),
        "owned CSR must not remain fixed to the v6 byte width"
    );
}

#[test]
fn p7_dual_parser_keeps_the_writer_on_v6_and_reports_validated_artifact_metadata() {
    let persistence = crate_source("src/persistence.rs");
    assert!(
        persistence.contains("const V6_VERSION: u32 = 6;")
            && persistence.contains("const VERSION: u32 = V6_VERSION;"),
        "P7.2 must not switch the production writer away from v6"
    );
    for seam in [
        "const V7_VERSION: u32 = 7",
        "struct GraphArtifactMetadata",
        "version: u32",
        "edge_type_width: EdgeTypeWidth",
        "artifact_version: u32",
        "fn decode_edge_type_width",
        "fn graph_artifact_metadata_for_path",
        "parsed_artifact_metadata_reports_actual_version_and_widths",
        "v6_artifact_remains_loadable_after_v7_activation",
    ] {
        assert!(
            persistence.contains(seam),
            "P7.2 dual-parser metadata is missing `{seam}`"
        );
    }
}

#[test]
fn p7_v7_fixture_validation_covers_every_width_and_corruption_boundary() {
    let persistence = crate_source("src/persistence.rs");
    for gate in [
        "v7_test_fixture_widths_one_two_four_roundtrip",
        "v7_rejects_invalid_edge_type_width",
        "v7_rejects_noncanonical_edge_type_width",
        "v7_rejects_physical_edge_type_sentinel",
        "v7_rejects_edge_type_section_range_or_alignment",
        "v7_rejects_truncated_edge_type_section",
        "v7_rejects_forward_inbound_edge_type_width_mismatch",
    ] {
        assert!(
            persistence.contains(gate),
            "P7.2 corruption matrix is missing `{gate}`"
        );
    }
}

#[test]
fn p7_manifest_recovery_and_sync_use_the_parsed_base_artifact_version() {
    let persistence = crate_source("src/persistence.rs");
    let recovery = crate_source("src/projection/recovery.rs");
    let sync = crate_source("src/sql_sync.rs");

    for (owner, source, seams) in [
        (
            "persistence",
            persistence.as_str(),
            [
                "manifest_uses_parsed_base_artifact_version",
                "graph_artifact_metadata_for_path",
            ],
        ),
        (
            "recovery",
            recovery.as_str(),
            [
                "recovery_validates_actual_base_artifact_version",
                "graph_artifact_metadata_for_path",
            ],
        ),
        (
            "sync",
            sync.as_str(),
            [
                "sync_ingester_carries_actual_base_artifact_version",
                "graph_artifact_metadata_for_path",
            ],
        ),
    ] {
        for seam in seams {
            assert!(
                source.contains(seam),
                "P7.2 {owner} actual-version flow is missing `{seam}`"
            );
        }
    }
}

#[test]
#[ignore = "P7.3 direct build, migration, evidence, and documentation checkpoint"]
fn p7_direct_build_and_public_contract_support_more_than_254_types() {
    let persisted = crate_source("src/persisted_build.rs");
    let scanner = crate_source("src/persisted_edge_scanner.rs");
    let tests = crate_source("src/pg_tests/maintenance_admin.rs");
    let roadmap = repo_source("todo/post-v1-1/README.md");
    for seam in [
        "direct_build_adaptive_type_width_matches_owned_build_bytes",
        "direct_build_roundtrips_more_than_65534_relationship_types",
    ] {
        assert!(
            persisted.contains(seam) || scanner.contains(seam),
            "P7.3 is missing `{seam}`"
        );
    }
    for gate in [
        "adaptive_edge_types_above_v6_roundtrip_and_filter_exactly",
        "adaptive_edge_type_corruption_fails_without_replacing_current_generation",
    ] {
        assert!(
            tests.contains(gate),
            "P7.3 is missing PostgreSQL gate `{gate}`"
        );
    }
    assert!(roadmap.contains("| P7 | Complete |"));
}
