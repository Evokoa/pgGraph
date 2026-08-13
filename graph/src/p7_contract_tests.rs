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
#[ignore = "P7.2 versioned adaptive base artifact checkpoint"]
fn p7_base_artifact_declares_validates_and_maps_each_edge_type_width() {
    let persistence = crate_source("src/persistence.rs");
    for seam in [
        "const VERSION: u32 = 7",
        "fn decode_edge_type_width",
        "fn write_edge_type_section",
        "v7_adaptive_edge_type_width_roundtrips_boundaries",
        "v7_rejects_invalid_width_reserved_id_alignment_and_truncation",
        "v6_artifact_remains_loadable_after_v7_activation",
        "v7_forward_and_inbound_widths_must_match_registry_capacity",
        "v7_checksum_and_recovery_preserve_last_valid_generation",
    ] {
        assert!(persistence.contains(seam), "P7.2 is missing `{seam}`");
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
