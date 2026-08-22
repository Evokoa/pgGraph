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
fn p7_dual_parser_reports_validated_artifact_metadata() {
    let persistence = crate_source("src/persistence.rs");
    assert!(
        persistence.contains("const V6_VERSION: u32 = 6;")
            && persistence.contains("const V7_VERSION: u32 = 7;"),
        "the dual loader must retain both artifact versions"
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
fn p7_direct_build_runs_use_logical_u32_before_the_v6_artifact_boundary() {
    let persisted = crate_source("src/persisted_build.rs");
    let scanner = crate_source("src/persisted_edge_scanner.rs");
    for seam in [
        "const LOGICAL_EDGE_TYPE_BYTES: usize = 4",
        "fn encode_logical_edge_type",
        "fn decode_logical_edge_type",
        "direct_run_codec_roundtrips_logical_edge_type_boundaries",
        "direct_run_codec_rejects_sentinel_truncation_and_key_value_mismatch",
        "direct_run_codec_accounts_for_widened_type_fields",
    ] {
        assert!(
            scanner.contains(seam),
            "P7.3 scanner/run codec is missing `{seam}`"
        );
    }

    for seam in [
        "direct_build_adaptive_type_width_matches_owned_build_bytes",
        "direct_build_v7_selects_width_boundaries_and_matches_owned_semantics",
        "direct_build_logical_run_accounting_matches_encoded_width",
    ] {
        assert!(
            persisted.contains(seam),
            "P7.3 direct assembly is missing `{seam}`"
        );
    }

    assert!(persisted.contains("to_v6_storage"));
    assert!(scanner.contains("EdgeTypeRegistry"));
}

#[test]
fn p7_v7_direct_build_and_public_contract_support_more_than_254_types() {
    let persistence = crate_source("src/persistence.rs");
    let persisted = crate_source("src/persisted_build.rs");
    let scanner = crate_source("src/persisted_edge_scanner.rs");
    assert!(
        persistence.contains("const VERSION: u32 = V7_VERSION;")
            && persistence.contains("edge_type_width: EdgeTypeWidth"),
        "P7.4 must activate v7 emission with an explicit direct-build width"
    );
    for seam in [
        "direct_build_adaptive_type_width_matches_owned_build_bytes",
        "direct_build_roundtrips_more_than_65534_relationship_types",
        "direct_build_v7_selects_width_boundaries_and_matches_owned_semantics",
        "direct_build_v7_accounting_matches_selected_physical_width",
    ] {
        assert!(
            persisted.contains(seam) || scanner.contains(seam),
            "P7.4 is missing `{seam}`"
        );
    }
    assert!(
        !scanner.contains("EdgeTypeRegistry::new_v6()")
            && !scanner.contains("registry.register_v6(label)"),
        "P7.4 source scanning must no longer enforce the historical v6 label ceiling"
    );
}

#[test]
fn p7_public_edge_type_policy_is_bounded_governed_and_tested() {
    let registry = crate_source("src/edge_type_registry.rs");
    let scanner = crate_source("src/persisted_edge_scanner.rs");
    let tests = crate_source("src/pg_tests/maintenance_admin.rs");
    for policy in [
        "MAX_USER_EDGE_TYPES",
        "MAX_EDGE_TYPE_LABEL_BYTES",
        "MAX_EDGE_TYPE_DICTIONARY_BYTES",
    ] {
        assert!(
            registry.contains(policy),
            "P7.4 public edge-type policy is missing `{policy}`"
        );
    }
    for gate in [
        "edge_type_policy_rejects_count_label_and_dictionary_limits",
        "direct_scan_accounts_registry_policy_before_interning",
    ] {
        assert!(
            registry.contains(gate) || scanner.contains(gate),
            "P7.4 policy unit coverage is missing `{gate}`"
        );
    }
    for gate in [
        "adaptive_edge_types_above_v6_roundtrip_and_filter_exactly",
        "adaptive_edge_type_policy_limits_fail_atomically",
        "edge_type_policy_limits_have_stable_sqlstate_and_detail",
    ] {
        assert!(
            tests.contains(gate),
            "P7.4 is missing PostgreSQL gate `{gate}`"
        );
    }
}

#[test]
fn p7_v7_migration_recovery_and_public_docs_ship_together() {
    let persistence = crate_source("src/persistence.rs");
    let recovery = crate_source("src/projection/recovery.rs");
    let tests = crate_source("src/pg_tests/maintenance_admin.rs");
    for gate in [
        "v6_to_v7_rebuild_migration_keeps_v6_loadable",
        "v7_candidate_corruption_preserves_current_v6_generation",
        "v7_recovery_uses_actual_version_and_width",
    ] {
        assert!(
            persistence.contains(gate) || recovery.contains(gate),
            "P7.4 migration/recovery coverage is missing `{gate}`"
        );
    }
    assert!(tests.contains("adaptive_edge_types_above_v6_roundtrip_and_filter_exactly"));

    let supported = repo_source("docs/user_guide/supported_features.mdx");
    let limits = repo_source("docs/user_guide/limitations-and-fit.mdx");
    let registration = repo_source("docs/user_guide/schema-registration.mdx");
    let api = repo_source("docs/user_guide/api-reference.mdx");
    let administration = repo_source("docs/user_guide/administration-and-security.mdx");
    let persistence_docs = repo_source("docs/user_guide/build-and-persistence.mdx");
    let internals = repo_source("docs/contributor_guide/engine-internals.mdx");
    let format = repo_source("docs/contributor_guide/persistence-format.mdx");
    assert!(
        supported.contains("Open-vocabulary relationship types")
            && supported.contains("relationship type limits"),
        "P7.4 supported features must advertise the bounded feature and its policy"
    );
    for phrase in [
        "Maximum distinct relationship types",
        "Maximum relationship type label bytes",
        "Maximum relationship type dictionary bytes",
    ] {
        assert!(
            limits.contains(phrase),
            "P7.4 canonical limitations policy is missing `{phrase}`"
        );
    }
    for (name, source) in [
        ("schema registration", registration.as_str()),
        ("API reference", api.as_str()),
        ("administration", administration.as_str()),
    ] {
        assert!(
            source.contains("relationship type limits"),
            "P7.4 {name} must direct users to the public relationship type limits"
        );
    }
    for (name, source) in [
        ("limitations", limits.as_str()),
        ("schema registration", registration.as_str()),
        ("API reference", api.as_str()),
        ("administration", administration.as_str()),
        ("engine internals", internals.as_str()),
    ] {
        for stale in [
            "limited to 254",
            "after 254 user labels",
            "up to 254 distinct",
            "max user labels are 254",
        ] {
            assert!(
                !source.contains(stale),
                "P7.4 {name} still publishes the obsolete statement `{stale}`"
            );
        }
    }
    assert!(
        persistence_docs.contains("v6")
            && persistence_docs.contains("v7")
            && persistence_docs.contains("rollback")
            && persistence_docs.contains("rebuild"),
        "P7.4 persistence docs must explain v6/v7 rebuild and rollback"
    );
    assert!(
        internals.contains("adaptive 1/2/4-byte")
            && format.contains("version 7")
            && format.contains("edge-type width"),
        "P7.4 contributor docs must describe the activated adaptive format"
    );

    let roadmap = repo_source("todo/post-v1-1/README.md");
    assert!(roadmap.contains("| P7 | Complete |"));
}
