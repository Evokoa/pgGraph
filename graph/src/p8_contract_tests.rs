//! P8 contracts for adaptive mutable relationship-type persistence.
//!
//! P8.1 activates only the segment-codec and cumulative-dictionary foundation.
//! The ignored P8.2 contracts freeze the following sync publication boundary
//! without requiring transaction-local dictionary work in the first checkpoint.

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
fn p8_segment_v7_uses_adaptive_checked_edge_type_storage_and_reads_v6() {
    let segment = crate_source("src/projection/segment.rs");
    for seam in [
        "const V6_VERSION: u32 = 6",
        "const V7_VERSION: u32 = 7",
        "edge_type_width: EdgeTypeWidth",
        "segment_v7_roundtrips_one_two_four_byte_edge_types",
        "segment_v6_remains_readable_after_v7_activation",
    ] {
        assert!(
            segment.contains(seam),
            "P8.1 adaptive segment codec is missing `{seam}`"
        );
    }
}

#[test]
fn p8_segment_v7_rejects_noncanonical_or_corrupt_type_sections() {
    let segment = crate_source("src/projection/segment.rs");
    for gate in [
        "segment_v7_rejects_invalid_or_noncanonical_edge_type_width",
        "segment_v7_rejects_edge_type_sentinel_and_registry_overflow",
        "segment_v7_rejects_misaligned_truncated_or_mismatched_type_sections",
        "segment_v7_decode_and_encode_account_selected_width_bytes",
    ] {
        assert!(
            segment.contains(gate),
            "P8.1 segment corruption/resource matrix is missing `{gate}`"
        );
    }
}

#[test]
fn p8_manifest_owns_a_checksummed_cumulative_edge_type_dictionary() {
    let manifest = crate_source("src/projection/manifest.rs");
    let dictionary = crate_source("src/projection/edge_type_dictionary.rs");
    for seam in [
        "struct ManifestEdgeTypeDictionaryRef",
        "edge_type_dictionary: Option<ManifestEdgeTypeDictionaryRef>",
        "write_edge_type_dictionary_artifact",
        "read_edge_type_dictionary_artifact",
        "edge_type_dictionary_roundtrips_boundaries_and_preserves_source_spelling",
        "edge_type_dictionary_rejects_count_offset_utf8_duplicate_and_checksum_corruption",
        "edge_type_dictionary_decode_is_resource_governed_before_allocation",
    ] {
        assert!(
            manifest.contains(seam) || dictionary.contains(seam),
            "P8.1 cumulative dictionary contract is missing `{seam}`"
        );
    }
}

#[test]
fn p8_recovery_and_gc_validate_and_retain_the_dictionary_reference() {
    let recovery = crate_source("src/projection/recovery.rs");
    for gate in [
        "recovery_rejects_missing_or_corrupt_edge_type_dictionary",
        "recovery_rejects_segment_type_ids_outside_cumulative_dictionary",
        "generation_gc_retains_referenced_edge_type_dictionaries",
        "failed_dictionary_candidate_preserves_the_current_generation",
    ] {
        assert!(
            recovery.contains(gate),
            "P8.1 recovery/retention contract is missing `{gate}`"
        );
    }
}

#[test]
fn p8_unseen_sync_labels_publish_dictionary_and_segments_atomically() {
    let sync = crate_source("src/sql_sync.rs");
    let ingest = crate_source("src/projection/ingest.rs");
    for seam in [
        "unseen_sync_labels_are_interned_in_deterministic_order_under_writer_lock",
        "duplicate_unseen_sync_labels_reuse_one_dictionary_slot",
        "unseen_sync_labels_publish_dictionary_and_segments_in_one_generation",
        "unseen_sync_label_failure_preserves_manifest_dictionary_engine_and_watermark",
        "unseen_sync_manifest_conflict_cleans_dictionary_and_segment_candidates",
        "sync_dictionary_growth_is_governed_before_label_or_segment_allocation",
    ] {
        assert!(
            sync.contains(seam) || ingest.contains(seam),
            "P8.2 unseen-label publication contract is missing `{seam}`"
        );
    }
}

#[test]
fn p8_unseen_sync_labels_survive_reload_and_fail_closed_on_corruption() {
    let pg_tests = crate_source("src/pg_tests/maintenance_admin.rs");
    for gate in [
        "durable_sync_unseen_edge_type_survives_reload_and_filters_exactly",
        "durable_sync_unseen_edge_type_policy_failure_is_atomic",
        "durable_sync_dictionary_corruption_fails_closed_without_advancing_generation",
    ] {
        assert!(
            pg_tests.contains(gate),
            "P8.2 PostgreSQL boundary is missing `{gate}`"
        );
    }

    let roadmap = repo_source("todo/post-v1-1/README.md");
    assert!(roadmap.contains("### P8: Persist dictionaries and incremental labels"));
}
