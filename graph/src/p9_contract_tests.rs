//! P9 release contracts for complete open-vocabulary query behavior.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("graph crate must live under the repository root")
        .join(relative)
}

fn crate_source(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)).unwrap_or_default()
}

fn repo_source(relative: &str) -> String {
    fs::read_to_string(repo_path(relative)).unwrap_or_default()
}

#[test]
fn p9_mutable_wide_build_uses_adaptive_base_and_segment_formats() {
    let persisted = crate_source("src/persisted_build.rs");
    let pg_tests = crate_source("src/pg_tests/maintenance_admin.rs");
    assert!(!persisted.contains("wide relationship types require csr_readonly"));
    assert!(persisted.contains("direct_build_wide_mutable_mode_roundtrips_adaptive_types"));
    assert!(pg_tests.contains("adaptive_edge_types_above_v6_roundtrip_and_filter_exactly"));
    assert!(pg_tests.contains("graph.build(mode := 'mutable_overlay')"));
}

#[test]
#[ignore = "P9.4 high-cardinality query matrix"]
fn p9_queries_filter_exactly_above_both_historical_width_boundaries() {
    let pg_tests = crate_source("src/pg_tests/p9_open_types.rs");
    for gate in [
        "open_type_255_traversal_paths_and_gql_filter_exactly",
        "open_type_65536_traversal_paths_and_gql_filter_exactly",
        "open_type_parallel_relationships_preserve_identity_and_exact_output",
        "open_type_dynamic_label_equality_lowers_to_compact_type_filter",
        "open_type_unknown_label_diagnostics_match_across_query_surfaces",
        "open_type_acl_rls_force_bypass_and_transaction_matrix",
        "open_type_cancellation_cleans_query_state_and_same_backend_retries",
    ] {
        assert!(
            pg_tests.contains(&format!("fn {gate}")),
            "P9 high-cardinality PostgreSQL matrix is missing `{gate}`"
        );
    }

    for boundary in ["254", "255", "65_535", "65_536"] {
        assert!(
            pg_tests.contains(boundary),
            "P9 query matrix is missing explicit boundary `{boundary}`"
        );
    }
}

#[test]
fn p9_relationship_type_inventory_and_status_are_explicitly_bounded() {
    let admin = crate_source("src/sql_facade/admin.rs");
    let pg_tests = crate_source("src/pg_tests/maintenance_admin.rs");
    assert!(
        admin.contains("relationship_type_page") || admin.contains("edge_type_page"),
        "P9 needs a bounded or paginated relationship-type inventory owner"
    );
    let engine = crate_source("src/engine.rs");
    let registry = crate_source("src/edge_type_registry.rs");
    assert!(registry.contains("MAX_QUERY_EDGE_TYPE_FILTERS"));
    assert!(registry.contains("MAX_QUERY_EDGE_TYPE_FILTER_BYTES"));
    assert!(registry.contains("STATUS_EDGE_TYPE_PREVIEW"));
    assert!(engine.contains("fn edge_type_page"));
    assert!(engine.contains("take(EdgeTypeRegistry::STATUS_EDGE_TYPE_PREVIEW)"));
    assert!(pg_tests.contains("open_type_inventory_pages_have_stable_order_without_duplicates"));
    assert!(pg_tests.contains("open_type_inventory_rejects_unbounded_windows_before_allocation"));
    assert!(pg_tests.contains("open_type_status_does_not_materialize_the_complete_dictionary"));
}

#[test]
fn p9_dynamic_gql_and_cypher_binding_is_independent_of_vocabulary_size() {
    let catalog = crate_source("src/query/catalog_snapshot.rs");
    let execute = crate_source("src/query/execute.rs");
    let pg_tests = crate_source("src/pg_tests/gql.rs");

    assert!(catalog
        .contains("unique_dynamic_mapping_resolves_named_type_without_vocabulary_enumeration"));
    assert!(!catalog.contains("SELECT DISTINCT COALESCE"));
    assert!(!catalog.contains("ORDER BY 1"));
    assert!(catalog.contains("mapping.label_column.is_some()"));
    assert!(
        execute.contains("absent_dynamic_label_uses_no_match_sentinel_without_registry_mutation")
    );
    assert!(execute.contains("EdgeTypeId::SENTINEL"));
    assert!(pg_tests.contains("gql_binds_dynamic_edge_labels_from_registered_label_column"));
    assert!(pg_tests.contains("gql_rejects_ambiguous_open_dynamic_relationship_mappings"));
    assert!(pg_tests.contains("gql_dynamic_hidden_and_absent_types_have_eager_lazy_parity"));
    assert!(pg_tests.contains("MATCH p=(u:graph_test_users_pgtest)-[:acquaintance]"));
    assert!(pg_tests.contains("graph.cypher("));
    assert!(pg_tests.contains(":not_loaded"));
    let semantics = crate_source("src/query/semantics.rs");
    let query_tests = crate_source("src/query/tests.rs");
    assert!(query_tests.contains("binder_accepts_structural_dynamic_type_in_wildcard_path"));
    assert!(semantics.contains("per-row type tombstones"));
}

#[test]
#[ignore = "P9.5 migration and PostgreSQL release matrix"]
fn p9_migration_recovery_diagnostics_and_postgres_matrix_ship_together() {
    let pg_tests = crate_source("src/pg_tests/p9_open_types.rs");
    for gate in [
        "open_type_v6_to_v7_migration_preserves_last_good_generation",
        "open_type_rollback_keeps_v6_readable_and_rebuild_diagnostic_stable",
        "open_type_policy_and_corruption_errors_have_stable_sqlstate_and_detail",
    ] {
        assert!(
            pg_tests.contains(&format!("fn {gate}")),
            "P9 migration/diagnostic coverage is missing `{gate}`"
        );
    }

    let runner = repo_source("graph/tests/heavy/open_type_release_matrix.sh");
    for version in ["pg14", "pg15", "pg16", "pg17", "pg18"] {
        assert!(
            runner.contains(version),
            "P9 release matrix does not name PostgreSQL `{version}`"
        );
    }
    assert!(
        runner.contains("open_type_255_traversal_paths_and_gql_filter_exactly")
            && runner.contains("open_type_65536_traversal_paths_and_gql_filter_exactly"),
        "P9 PostgreSQL matrix must execute both high-cardinality query boundaries"
    );
}

#[test]
#[ignore = "P9.5 fuzz evidence"]
fn p9_open_type_codecs_and_query_filters_have_property_and_fuzz_evidence() {
    let registry = crate_source("src/edge_type_registry.rs");
    let segment = crate_source("src/projection/segment.rs");
    let query = crate_source("src/engine.rs");
    for (owner, source, gate) in [
        (
            "registry",
            registry.as_str(),
            "open_type_registry_property_preserves_order_spelling_and_lookup",
        ),
        (
            "segment",
            segment.as_str(),
            "open_type_segment_property_roundtrips_widths_and_rejects_sentinels",
        ),
        (
            "query",
            query.as_str(),
            "open_type_filter_property_matches_text_reference_model",
        ),
    ] {
        assert!(
            source.contains(&format!("fn {gate}")),
            "P9 {owner} property coverage is missing `{gate}`"
        );
    }

    let dictionary_fuzz = repo_source("graph/fuzz/fuzz_targets/load_edge_type_dictionary.rs");
    let segment_fuzz = repo_source("graph/fuzz/fuzz_targets/load_projection_segment.rs");
    assert!(
        dictionary_fuzz.contains("fuzz_target"),
        "P9 needs an arbitrary-byte cumulative-dictionary fuzz target"
    );
    assert!(
        segment_fuzz.contains("fuzz_target"),
        "P9 must retain arbitrary-byte mutable-segment fuzz coverage"
    );
}

#[test]
#[ignore = "P9.5 retained performance evidence"]
fn p9_retains_reproducible_high_cardinality_query_and_resource_evidence() {
    let cargo = crate_source("Cargo.toml");
    let benchmark = crate_source("benches/open_type_query_bench.rs");
    assert!(
        cargo.contains("name = \"open_type_query_bench\"")
            && cargo.contains("required-features = [\"benchmarks\"]"),
        "P9 query benchmark must be an explicit benchmark-only target"
    );
    for dimension in [
        "label_count",
        "selectivity",
        "degree",
        "depth",
        "backend_count",
        "artifact_bytes",
        "rss",
        "pss",
    ] {
        assert!(
            benchmark.contains(dimension),
            "P9 query benchmark is missing `{dimension}`"
        );
    }

    let evidence = repo_path("todo/measurements/2026-08-13-p9-open-type-query");
    let readme = fs::read_to_string(evidence.join("README.md"))
        .expect("P9 must retain reproducible open-type measurement instructions");
    let results = fs::read_to_string(evidence.join("results.csv"))
        .expect("P9 must retain machine-readable query/resource results");
    let budgets = fs::read_to_string(evidence.join("budgets.json"))
        .expect("P9 must retain predeclared query/resource acceptance budgets");
    for required in [
        "exact commit",
        "cargo bench",
        "254",
        "255",
        "65535",
        "65536",
        "PostgreSQL",
    ] {
        assert!(
            readme.contains(required),
            "P9 evidence README is missing `{required}`"
        );
    }
    let header = results.lines().next().expect("P9 results need a header");
    for column in [
        "label_count",
        "query_surface",
        "selectivity",
        "degree",
        "depth",
        "latency_median_ns",
        "throughput_per_second",
        "rss_bytes",
        "pss_bytes",
        "artifact_bytes",
        "backend_count",
    ] {
        assert!(
            header.split(',').any(|candidate| candidate == column),
            "P9 evidence results are missing `{column}`"
        );
    }
    let budget_json: serde_json::Value =
        serde_json::from_str(&budgets).expect("P9 budgets must be valid JSON");
    assert!(budget_json.get("low_cardinality_regression").is_some());
    assert!(budget_json
        .get("high_cardinality_resource_limits")
        .is_some());
}
