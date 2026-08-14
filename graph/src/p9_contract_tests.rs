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
fn p9_queries_filter_exactly_above_both_historical_width_boundaries() {
    let pg_tests = crate_source("src/pg_tests/p9_open_types.rs");
    for gate in [
        "open_type_255_traversal_paths_gql_and_cypher_filter_exactly",
        "open_type_parallel_relationships_preserve_identity_and_exact_output",
        "open_type_dynamic_label_equality_lowers_to_compact_type_filter",
        "open_type_absent_and_invalid_label_diagnostics_are_surface_stable",
        "open_type_acl_rls_force_bypass_and_transaction_matrix",
        "open_type_cancellation_cleans_query_state_and_same_backend_retries",
    ] {
        assert!(
            pg_tests.contains(&format!("fn {gate}")),
            "P9 high-cardinality PostgreSQL matrix is missing `{gate}`"
        );
    }

    for boundary in ["254", "255"] {
        assert!(
            pg_tests.contains(boundary),
            "P9 routine query matrix is missing explicit boundary `{boundary}`"
        );
    }

    let heavy = repo_source("graph/tests/heavy/open_type_high_cardinality_query.sh");
    for boundary in ["65534", "65535", "65536"] {
        assert!(
            heavy.contains(boundary),
            "P9 heavy query matrix is missing explicit boundary `{boundary}`"
        );
    }
    for surface in [
        "graph.traverse",
        "graph.shortest_path",
        "graph.gql",
        "graph.cypher",
    ] {
        assert!(
            heavy.contains(surface),
            "P9 heavy query matrix is missing `{surface}`"
        );
    }

    let semantics = crate_source("src/query/semantics.rs");
    assert!(
        semantics.contains("lower_dynamic_label_equality_filter"),
        "P9.4 must lower an eligible dynamic label-column equality to the existing compact type filter"
    );
    assert!(
        !semantics.contains("SELECT DISTINCT"),
        "P9.4 query binding must not enumerate dynamic source vocabulary"
    );
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
fn p9_migration_and_rollback_preserve_the_last_good_open_type_generation() {
    let persistence = crate_source("src/persistence.rs");
    for gate in [
        "v6_to_v7_rebuild_migration_keeps_v6_loadable",
        "v7_candidate_corruption_preserves_current_v6_generation",
    ] {
        assert!(
            persistence.contains(&format!("fn {gate}")),
            "P9 migration coverage is missing the pure artifact gate `{gate}`"
        );
    }

    let sync = crate_source("src/sql_sync.rs");
    let recovery = crate_source("src/projection/recovery.rs");
    for (owner, source, gate) in [
        (
            "sync",
            sync.as_str(),
            "sync_ingester_carries_actual_base_artifact_version",
        ),
        (
            "recovery",
            recovery.as_str(),
            "v7_recovery_uses_actual_version_and_width",
        ),
    ] {
        assert!(
            source.contains(&format!("fn {gate}")),
            "P9 {owner} compatibility coverage is missing `{gate}`"
        );
    }
    let maintenance = crate_source("src/pg_tests/maintenance_admin.rs");
    for gate in [
        "adaptive_edge_type_policy_limits_fail_atomically",
        "durable_sync_dictionary_corruption_fails_closed_without_advancing_generation",
    ] {
        assert!(
            maintenance.contains(&format!("fn {gate}")),
            "P9 diagnostic coverage is missing the real PostgreSQL gate `{gate}`"
        );
    }
}

#[test]
fn p9_open_type_diagnostics_and_packages_run_on_postgres_14_through_18() {
    let runner = repo_source("graph/tests/heavy/open_type_release_matrix.sh");
    for version in ["14", "15", "16", "17", "18"] {
        assert!(
            runner.contains(version),
            "P9 release matrix does not name PostgreSQL {version}"
        );
    }
    for requirement in [
        "RUN_PGRX_SQL=1",
        "RUN_PACKAGE_INSTALL_MATRIX=1",
        "open_type_255_traversal_paths_gql_and_cypher_filter_exactly",
        "adaptive_edge_type_policy_limits_fail_atomically",
        "durable_sync_dictionary_corruption_fails_closed_without_advancing_generation",
    ] {
        assert!(
            runner.contains(requirement),
            "P9 PostgreSQL package/diagnostic matrix is missing `{requirement}`"
        );
    }
    assert!(runner.contains(
        "PGRX_TEST_FILTER=\"open_type_ adaptive_edge_type_policy_limits_fail_atomically durable_sync_dictionary_corruption_fails_closed_without_advancing_generation\""
    ));

    let dockerfile = repo_source("graph/tests/heavy/Dockerfile.pg-matrix");
    for requirement in [
        "set -euo pipefail; \\\n    export RUSTFLAGS=\"${RUSTFLAGS:-} -C link-arg=-Wl,--unresolved-symbols=ignore-all\"; \\\n    for pg in ${PG_VERSIONS}",
        "for test_filter in ${PGRX_TEST_FILTER}",
        "cargo pgrx test --features development \"${feature}\" \"${test_filter}\"",
        "RUN_OPEN_TYPE_PACKAGE_SMOKE",
    ] {
        assert!(
            dockerfile.contains(requirement),
            "P9 Docker matrix wiring is missing `{requirement}`"
        );
    }

    let package_matrix = repo_source("graph/tests/heavy/package_install_matrix.sh");
    assert!(package_matrix.contains("RUN_OPEN_TYPE_PACKAGE_SMOKE"));
    assert!(package_matrix.contains("open_type_package_smoke.sh"));
    let package_smoke = repo_source("graph/tests/heavy/open_type_package_smoke.sh");
    for requirement in [
        "graph.unload_graph",
        "graph.load_graph",
        "graph.edge_types",
        "graph.traverse",
        "graph.shortest_path",
        "graph.gql",
        "graph.cypher",
        "PG004",
    ] {
        assert!(
            package_smoke.contains(requirement),
            "P9 installed-package smoke is missing `{requirement}`"
        );
    }
}

#[test]
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
#[ignore = "P9.5c/d activates after benchmark budgets and retained evidence land"]
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
