//! P9 release contracts for complete open-vocabulary query behavior.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn retained_delimited_rows(
    path: &Path,
    delimiter: char,
    expected_header: &[&str],
) -> Vec<Vec<String>> {
    let contents = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "retained evidence {} is unreadable: {error}",
            path.display()
        )
    });
    let mut lines = contents.lines();
    let header = lines
        .next()
        .unwrap_or_else(|| panic!("retained evidence {} is empty", path.display()))
        .split(delimiter)
        .collect::<Vec<_>>();
    assert_eq!(
        header,
        expected_header,
        "unexpected header in {}",
        path.display()
    );
    lines
        .enumerate()
        .map(|(index, line)| {
            let fields = line
                .split(delimiter)
                .map(str::to_string)
                .collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                expected_header.len(),
                "row {} in {} has the wrong width",
                index + 2,
                path.display()
            );
            fields
        })
        .collect()
}

fn retained_u64(value: &str, field: &str) -> u64 {
    value
        .parse::<u64>()
        .unwrap_or_else(|error| panic!("retained `{field}` value `{value}` is invalid: {error}"))
}

fn retained_positive_f64(value: &str, field: &str) -> f64 {
    let parsed = value
        .parse::<f64>()
        .unwrap_or_else(|error| panic!("retained `{field}` value `{value}` is invalid: {error}"));
    assert!(
        parsed.is_finite() && parsed > 0.0,
        "retained `{field}` must be finite and positive"
    );
    parsed
}

fn retained_median(mut samples: Vec<f64>) -> f64 {
    assert!(
        !samples.is_empty(),
        "retained median needs at least one sample"
    );
    samples.sort_by(f64::total_cmp);
    let middle = samples.len() / 2;
    if samples.len().is_multiple_of(2) {
        (samples[middle - 1] + samples[middle]) / 2.0
    } else {
        samples[middle]
    }
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
fn p9_open_type_query_benchmark_and_linux_backend_resource_runner_are_declared() {
    let cargo = crate_source("Cargo.toml");
    let benchmark = crate_source("benches/open_type_query_bench.rs");
    assert!(
        cargo.contains(
            "[[bench]]\nname = \"open_type_query_bench\"\nharness = false\nrequired-features = [\"benchmarks\"]"
        ),
        "P9 query benchmark must be an explicit benchmark-only target"
    );
    for dimension in [
        "label_count",
        "query_surface",
        "selectivity",
        "degree",
        "depth",
        "csr_direction",
        "csr_type_bytes_one_direction",
        "adaptive_u8",
        "adaptive_u16",
        "adaptive_u32",
        "validate_case_manifest",
    ] {
        assert!(
            benchmark.contains(dimension),
            "P9 query benchmark is missing `{dimension}`"
        );
    }

    let runner = repo_source("graph/tests/heavy/open_type_query_resources.sh");
    for requirement in [
        "uname -s",
        "/proc/",
        "smaps_rollup",
        "pg_backend_pid()",
        "psql -X -v ON_ERROR_STOP=1",
        "trap cleanup EXIT",
        "^pggraph_",
        "MAX_RSS_BYTES",
        "MAX_PSS_BYTES",
        "STATEMENT_TIMEOUT_MS",
        "BACKEND_COUNT",
        "ARTIFACT_BYTES",
        "projection_artifact_bytes",
        "query-started",
        "query-done",
        "pg_stat_activity",
        "state = 'active'",
        "query LIKE '%graph.traverse%'",
        "max_query_total_pss_bytes",
        "query_minus_idle_total_pss_bytes",
        "label_count",
        "query_surface",
        "filter_shape",
        "degree",
        "depth",
    ] {
        assert!(
            runner.contains(requirement),
            "P9 Linux PostgreSQL resource runner is missing `{requirement}`"
        );
    }
    assert!(
        !runner.contains("\\! while"),
        "P9 resource workers must use bounded PostgreSQL waits, not orphanable shell loops"
    );
}

#[test]
fn p9_open_type_query_and_resource_budgets_are_predeclared() {
    let evidence = repo_path("todo/measurements/2026-08-13-p9-open-type-query");
    let cases = fs::read_to_string(evidence.join("cases.json"))
        .expect("P9 must retain the exact predeclared benchmark case matrix");
    let case_json: serde_json::Value =
        serde_json::from_str(&cases).expect("P9 cases must be valid JSON");
    let budgets = fs::read_to_string(evidence.join("budgets.json"))
        .expect("P9 must retain predeclared query/resource acceptance budgets");
    let budget_json: serde_json::Value =
        serde_json::from_str(&budgets).expect("P9 budgets must be valid JSON");
    assert_eq!(
        budget_json
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("predeclared"),
        "P9 acceptance budgets must be frozen before measurements run"
    );
    assert_eq!(
        case_json
            .get("criterion_expected_case_count")
            .and_then(serde_json::Value::as_u64),
        Some(65),
        "P9 must freeze the exact Criterion case count before measurement"
    );
    for (pointer, expected) in [
        (
            "/criterion/registry_lookup/request",
            serde_json::json!(["first", "last", "missing"]),
        ),
        (
            "/criterion/filter_resolution/request_shape",
            serde_json::json!(["empty", "one_exact", "bounded_4096"]),
        ),
        (
            "/criterion/filter_resolution/request",
            serde_json::json!(["first", "last", "missing", "32_labels"]),
        ),
        (
            "/criterion/bfs_oat/label_count",
            serde_json::json!([254, 255, 65_534, 65_535, 65_536]),
        ),
        (
            "/criterion/bfs_oat/degree",
            serde_json::json!([1, 8, 64, 1_024]),
        ),
        ("/criterion/bfs_oat/depth", serde_json::json!([1, 4, 16])),
        (
            "/criterion/bfs_oat/csr_direction",
            serde_json::json!(["out", "in"]),
        ),
        (
            "/criterion/bfs_oat/selectivity",
            serde_json::json!([
                "none_matched",
                "one_of_32",
                "half_of_32",
                "all_matched",
                "no_filter"
            ]),
        ),
    ] {
        assert_eq!(
            case_json.pointer(pointer),
            Some(&expected),
            "P9 case matrix drifted at `{pointer}`"
        );
    }
    let ratio_limits = budget_json
        .get("criterion_ratio_limits")
        .and_then(serde_json::Value::as_array)
        .expect("P9 budgets need selector-scoped Criterion ratios");
    assert_eq!(ratio_limits.len(), 3);
    for limit in ratio_limits {
        let numerator = limit
            .get("numerator")
            .and_then(serde_json::Value::as_object)
            .expect("each Criterion ratio needs a numerator selector");
        let denominator = limit
            .get("denominator")
            .and_then(serde_json::Value::as_object)
            .expect("each Criterion ratio needs a denominator selector");
        let latency = limit
            .get("max_median_latency_ratio")
            .and_then(serde_json::Value::as_f64)
            .expect("each Criterion ratio needs a latency limit");
        let throughput = limit
            .get("min_throughput_ratio")
            .and_then(serde_json::Value::as_f64)
            .expect("each Criterion ratio needs a throughput limit");
        for selector in [numerator, denominator] {
            for key in [
                "query_surface",
                "label_count",
                "selectivity",
                "degree",
                "depth",
                "csr_direction",
            ] {
                assert!(
                    selector.contains_key(key),
                    "Criterion selector is missing `{key}`"
                );
            }
        }
        assert!(latency >= 1.0 && throughput > 0.0 && throughput <= 1.0);
    }
    let postgres_limits = budget_json
        .get("postgres_ratio_limits")
        .and_then(serde_json::Value::as_array)
        .expect("P9 budgets need selector-scoped PostgreSQL ratios");
    assert_eq!(postgres_limits.len(), 4);
    for surface in ["traverse", "shortest_path", "gql", "cypher"] {
        assert!(postgres_limits.iter().any(|limit| {
            limit
                .get("query_surface")
                .and_then(serde_json::Value::as_str)
                == Some(surface)
                && limit
                    .pointer("/numerator/label_count")
                    .and_then(serde_json::Value::as_u64)
                    == Some(65_536)
                && limit
                    .pointer("/denominator/label_count")
                    .and_then(serde_json::Value::as_u64)
                    == Some(254)
        }));
    }
    assert!(budget_json
        .get("max_relative_confidence_interval_width")
        .and_then(serde_json::Value::as_f64)
        .is_some_and(|width| width > 0.0 && width <= 1.0));
    assert_eq!(
        budget_json
            .pointer("/lineage/budget_commit_must_precede_measurement_commit")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let rss = budget_json
        .pointer("/high_cardinality_resource_limits/max_rss_bytes_per_backend")
        .and_then(serde_json::Value::as_u64)
        .expect("P9 budgets need a per-backend RSS limit");
    let pss = budget_json
        .pointer("/high_cardinality_resource_limits/max_pss_bytes_per_backend")
        .and_then(serde_json::Value::as_u64)
        .expect("P9 budgets need a per-backend PSS limit");
    let artifact = budget_json
        .pointer(
            "/high_cardinality_resource_limits/max_projection_artifact_bytes_per_directed_edge",
        )
        .and_then(serde_json::Value::as_f64)
        .expect("P9 budgets need an artifact-byte limit");
    let total_pss_ratio = budget_json
        .pointer("/high_cardinality_resource_limits/max_eight_to_one_total_query_pss_ratio")
        .and_then(serde_json::Value::as_f64)
        .expect("P9 budgets need an aggregate backend PSS scaling limit");
    let delta_pss_ratio = budget_json
        .pointer(
            "/high_cardinality_resource_limits/max_eight_to_one_query_minus_idle_total_pss_ratio",
        )
        .and_then(serde_json::Value::as_f64)
        .expect("P9 budgets need a baseline-subtracted backend PSS scaling limit");
    let label_counts = budget_json
        .pointer("/fixtures/label_counts")
        .and_then(serde_json::Value::as_array)
        .expect("P9 budgets must bind the measured label-count boundaries");
    let backend_counts = budget_json
        .pointer("/fixtures/backend_counts")
        .and_then(serde_json::Value::as_array)
        .expect("P9 budgets must bind the measured backend counts");
    assert!(
        rss > 0 && pss > 0 && artifact >= 1.0 && total_pss_ratio >= 1.0 && delta_pss_ratio >= 1.0
    );
    for boundary in [254_u64, 255, 65_534, 65_535, 65_536] {
        assert!(
            label_counts
                .iter()
                .any(|candidate| candidate.as_u64() == Some(boundary)),
            "P9 predeclared fixtures are missing label boundary `{boundary}`"
        );
    }
    assert!(
        backend_counts.len() >= 2
            && backend_counts
                .iter()
                .all(|candidate| candidate.as_u64().is_some_and(|count| count > 0)),
        "P9 resource evidence must predeclare at least two positive backend counts"
    );
}

#[test]
fn p9_open_type_measurement_tooling_is_declared_before_results() {
    let evidence = repo_path("todo/measurements/2026-08-13-p9-open-type-query");
    let protocol: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(evidence.join("measurement-protocol.json"))
            .expect("P9 needs a premeasurement protocol"),
    )
    .expect("P9 measurement protocol must be valid JSON");
    assert_eq!(
        protocol
            .get("budget_commit")
            .and_then(serde_json::Value::as_str),
        Some("dd730b8")
    );
    assert_eq!(
        protocol
            .pointer("/criterion/expected_case_count")
            .and_then(serde_json::Value::as_u64),
        Some(65)
    );
    assert_eq!(
        protocol
            .pointer("/postgres/measured_samples_per_case")
            .and_then(serde_json::Value::as_u64),
        Some(40)
    );
    assert_eq!(
        protocol
            .pointer("/postgres/memory_limit_mb")
            .and_then(serde_json::Value::as_u64),
        Some(2048)
    );
    assert_eq!(
        protocol
            .pointer("/postgres/query_memory_mb")
            .and_then(serde_json::Value::as_u64),
        Some(512)
    );
    assert_eq!(
        protocol
            .pointer("/postgres/gql_cypher_query_shape")
            .and_then(serde_json::Value::as_str),
        Some("typed_one_hop_return_vertex")
    );
    assert_eq!(
        protocol
            .pointer("/postgres/gql_cypher_oracle_digest")
            .and_then(serde_json::Value::as_str),
        Some("exact_single_target_vertex_id_2")
    );
    assert_eq!(
        protocol
            .pointer("/resources/backend_counts")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(3)
    );

    let extractor = repo_source("scripts/extract_p9_open_type_criterion.py");
    for required in [
        "criterion-estimates.csv",
        "criterion-results.csv",
        "criterion-raw-hashes.csv",
        "expected_shape",
        "estimates.json",
        "os.replace",
    ] {
        assert!(
            extractor.contains(required),
            "P9 Criterion extractor is missing `{required}`"
        );
    }

    let criterion_runner = repo_source("graph/tests/heavy/run_open_type_query_criterion.sh");
    for required in [
        "open_type_registry_lookup open_type_filter_resolution open_type_bfs",
        "cargo +1.96.0 bench",
        "criterion-run.log",
        "extract_p9_open_type_criterion.py",
    ] {
        assert!(
            criterion_runner.contains(required),
            "P9 Criterion runner is missing `{required}`"
        );
    }

    let summarizer = repo_source("scripts/summarize_p9_open_type_postgres.py");
    for required in [
        "PostgreSQL cases",
        "statistics.median",
        "set(range(1, 41))",
        "os.replace",
    ] {
        assert!(
            summarizer.contains(required),
            "P9 PostgreSQL summarizer is missing `{required}`"
        );
    }

    let latency = repo_source("graph/tests/heavy/open_type_query_latency.sh");
    for required in [
        "pgbench",
        "-M extended",
        "TRANSACTIONS=50",
        "WARMUPS=10",
        "traverse shortest_path gql cypher",
        "postgres-oracles.csv",
        "postgres-samples.csv",
        "postgres-log-hashes.csv",
        "latency-postgres-version.txt",
        "latency-settings.json",
        "current_setting('graph.memory_limit_mb')",
        "replace('MATCH (u@p9_latency_nodes {id: 1})-[@type_1]",
        "row #>> '{v,_id,id}'",
        "HAVING count(*) = 1",
        "min(row #>> '{v,_id,id}') = '2'",
        "'@', chr(58)",
    ] {
        assert!(
            latency.contains(required),
            "P9 PostgreSQL latency runner is missing `{required}`"
        );
    }
    assert_eq!(
        latency.matches("graph.gql(replace(").count(),
        2,
        "P9 pgbench and oracle GQL queries must both hide graph colons from substitution"
    );
    assert_eq!(
        latency.matches("graph.cypher(replace(").count(),
        2,
        "P9 pgbench and oracle Cypher queries must both hide graph colons from substitution"
    );
    assert!(
        !latency.contains("\\\\:p9_latency_nodes"),
        "P9 latency queries must not rely on ineffective pgbench backslash-colon escaping"
    );

    let resource_matrix = repo_source("graph/tests/heavy/run_open_type_query_resource_matrix.sh");
    for required in [
        "for backend_count in 1 4 8",
        "resource-samples.tsv",
        "resource-results.csv",
        "open_type_query_resources.sh",
    ] {
        assert!(
            resource_matrix.contains(required),
            "P9 resource matrix is missing `{required}`"
        );
    }

    let docker = repo_source("graph/tests/heavy/run_open_type_query_resources_docker.sh");
    for required in [
        "docker build",
        "docker create",
        "docker start -a",
        "docker cp",
        "docker-image-inspect.json",
        "resource-postgres-version.txt",
    ] {
        assert!(
            docker.contains(required),
            "P9 Docker evidence exporter is missing `{required}`"
        );
    }

    let checker = repo_source("todo/measurements/2026-08-13-p9-open-type-query/check_results.py");
    for required in [
        "validate_criterion",
        "validate_postgres",
        "validate_resources",
        "criterion-raw-hashes.csv",
        "postgres-log-hashes.csv",
        "merge-base",
        "--is-ancestor",
        "raw_summary_reconciled",
    ] {
        assert!(
            checker.contains(required),
            "P9 evidence checker is missing `{required}`"
        );
    }

    let metadata_writer = repo_source("scripts/write_p9_open_type_run_metadata.py");
    for required in [
        "measurement commit must equal the exact current Git HEAD",
        "source changes outside the evidence directory are not allowed",
        "dd730b8",
        "linux-uname.txt",
        "latency-postgres-version.txt",
        "latency-settings.json",
        "resource-postgres-version.txt",
        "docker-version.json",
        "run-metadata.json",
    ] {
        assert!(
            metadata_writer.contains(required),
            "P9 run-metadata writer is missing `{required}`"
        );
    }

    let self_test = repo_path("scripts/test_p9_open_type_evidence_tools.py");
    let output = Command::new("python3")
        .arg(&self_test)
        .output()
        .expect("P9 evidence-tooling self-test must execute");
    assert!(
        output.status.success(),
        "P9 evidence-tooling self-test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "P9.5d activates after the committed tooling produces retained Criterion evidence"]
fn p9_retained_criterion_results_reconcile_all_predeclared_cases() {
    let evidence = repo_path("todo/measurements/2026-08-13-p9-open-type-query");
    let estimates = retained_delimited_rows(
        &evidence.join("criterion-estimates.csv"),
        ',',
        &[
            "benchmark_id",
            "statistic",
            "confidence_level",
            "lower_bound_ns",
            "point_estimate_ns",
            "upper_bound_ns",
        ],
    );
    let results = retained_delimited_rows(
        &evidence.join("criterion-results.csv"),
        ',',
        &[
            "benchmark_id",
            "query_surface",
            "label_count",
            "request",
            "request_shape",
            "selectivity",
            "degree",
            "depth",
            "csr_direction",
            "encoding",
            "csr_type_bytes_one_direction",
            "latency_median_ns",
            "latency_ci_lower_ns",
            "latency_ci_upper_ns",
            "throughput_per_second",
        ],
    );
    assert_eq!(
        results.len(),
        65,
        "P9 needs exactly 65 normalized Criterion cases"
    );
    let result_ids = results
        .iter()
        .map(|row| row[0].as_str())
        .collect::<HashSet<_>>();
    assert_eq!(
        result_ids.len(),
        65,
        "normalized Criterion cases contain duplicates"
    );
    let estimate_keys = estimates
        .iter()
        .map(|row| (row[0].as_str(), row[1].as_str()))
        .collect::<HashSet<_>>();
    assert_eq!(
        estimate_keys.len(),
        estimates.len(),
        "raw Criterion estimates contain duplicate benchmark/statistic rows"
    );
    for row in &estimates {
        assert!(row[2]
            .parse::<f64>()
            .is_ok_and(|confidence| confidence > 0.0 && confidence <= 1.0));
        let lower = row[3].parse::<f64>().expect("raw lower bound is numeric");
        let point = row[4]
            .parse::<f64>()
            .expect("raw point estimate is numeric");
        let upper = row[5].parse::<f64>().expect("raw upper bound is numeric");
        assert!(lower > 0.0 && lower <= point && point <= upper);
    }
    let median_ids = estimates
        .iter()
        .filter(|row| row[1] == "median")
        .map(|row| row[0].as_str())
        .collect::<HashSet<_>>();
    assert_eq!(
        median_ids, result_ids,
        "raw Criterion medians and normalized cases differ"
    );
    for row in &results {
        let lower = retained_positive_f64(&row[12], "latency_ci_lower_ns");
        let median = retained_positive_f64(&row[11], "latency_median_ns");
        let upper = retained_positive_f64(&row[13], "latency_ci_upper_ns");
        assert!(lower <= median && median <= upper);
        retained_positive_f64(&row[14], "throughput_per_second");
    }
}

#[test]
#[ignore = "P9.5d activates after the committed tooling produces retained PostgreSQL evidence"]
fn p9_retained_postgres_results_cover_low_and_high_cardinality_surfaces() {
    let evidence = repo_path("todo/measurements/2026-08-13-p9-open-type-query");
    let samples = retained_delimited_rows(
        &evidence.join("postgres-samples.csv"),
        ',',
        &[
            "query_surface",
            "label_count",
            "sample_index",
            "elapsed_ns",
            "result_digest",
        ],
    );
    let rows = retained_delimited_rows(
        &evidence.join("postgres-results.csv"),
        ',',
        &[
            "query_surface",
            "label_count",
            "sample_count",
            "latency_median_ns",
            "latency_ci_lower_ns",
            "latency_ci_upper_ns",
            "throughput_per_second",
        ],
    );
    assert_eq!(
        rows.len(),
        8,
        "P9 needs four PostgreSQL surfaces at two cardinalities"
    );
    let observed = rows
        .iter()
        .map(|row| (row[0].as_str(), retained_u64(&row[1], "label_count")))
        .collect::<HashSet<_>>();
    let expected = ["traverse", "shortest_path", "gql", "cypher"]
        .into_iter()
        .flat_map(|surface| [254_u64, 65_536].map(|labels| (surface, labels)))
        .collect::<HashSet<_>>();
    assert_eq!(
        observed, expected,
        "PostgreSQL result matrix is incomplete or duplicated"
    );
    let mut raw_by_case = HashMap::<(&str, u64), Vec<f64>>::new();
    let mut raw_keys = HashSet::new();
    let mut raw_digests = HashMap::<(&str, u64), HashSet<&str>>::new();
    for sample in &samples {
        let key = (sample[0].as_str(), retained_u64(&sample[1], "label_count"));
        let sample_index = retained_u64(&sample[2], "sample_index");
        assert!(
            raw_keys.insert((key, sample_index)),
            "duplicate PostgreSQL raw sample"
        );
        raw_by_case
            .entry(key)
            .or_default()
            .push(retained_positive_f64(&sample[3], "elapsed_ns"));
        assert!(
            !sample[4].is_empty(),
            "PostgreSQL sample needs a result digest"
        );
        raw_digests.entry(key).or_default().insert(&sample[4]);
    }
    assert_eq!(
        raw_by_case.keys().copied().collect::<HashSet<_>>(),
        expected
    );
    for row in rows {
        let key = (row[0].as_str(), retained_u64(&row[1], "label_count"));
        let raw = raw_by_case
            .get(&key)
            .expect("PostgreSQL case needs raw samples");
        let sample_count = retained_u64(&row[2], "sample_count");
        assert_eq!(sample_count, raw.len() as u64);
        assert!(sample_count >= 20);
        assert_eq!(raw_digests.get(&key).map(HashSet::len), Some(1));
        let lower = retained_positive_f64(&row[4], "latency_ci_lower_ns");
        let median = retained_positive_f64(&row[3], "latency_median_ns");
        let upper = retained_positive_f64(&row[5], "latency_ci_upper_ns");
        assert!(lower <= median && median <= upper);
        let recomputed_median = retained_median(raw.clone());
        assert!((median - recomputed_median).abs() <= f64::EPSILON * recomputed_median.max(1.0));
        let throughput = retained_positive_f64(&row[6], "throughput_per_second");
        let recomputed_throughput = 1_000_000_000.0 * raw.len() as f64 / raw.iter().sum::<f64>();
        assert!((throughput - recomputed_throughput).abs() <= recomputed_throughput * 1e-6);
    }
}

#[test]
#[ignore = "P9.5d activates after the committed tooling produces retained Linux resource evidence"]
fn p9_retained_linux_resources_use_real_backend_pids_and_nonzero_pss() {
    let evidence = repo_path("todo/measurements/2026-08-13-p9-open-type-query");
    let samples = retained_delimited_rows(
        &evidence.join("resource-samples.tsv"),
        '\t',
        &[
            "run_id",
            "label_count",
            "backend_count",
            "phase",
            "sample_id",
            "epoch_ms",
            "backend",
            "pid",
            "rss_bytes",
            "pss_bytes",
        ],
    );
    let summaries = retained_delimited_rows(
        &evidence.join("resource-results.csv"),
        ',',
        &[
            "run_id",
            "label_count",
            "backend_count",
            "query_surface",
            "filter_shape",
            "degree",
            "depth",
            "projection_artifact_bytes",
            "max_per_backend_rss_bytes",
            "max_per_backend_pss_bytes",
            "max_idle_total_rss_bytes",
            "max_idle_total_pss_bytes",
            "max_loaded_total_rss_bytes",
            "max_loaded_total_pss_bytes",
            "max_query_total_rss_bytes",
            "max_query_total_pss_bytes",
            "query_minus_idle_total_rss_bytes",
            "query_minus_idle_total_pss_bytes",
        ],
    );
    assert_eq!(
        summaries.len(),
        3,
        "P9 needs one resource summary for 1, 4, and 8 backends"
    );
    let summary_counts = summaries
        .iter()
        .map(|row| retained_u64(&row[2], "backend_count"))
        .collect::<HashSet<_>>();
    assert_eq!(summary_counts, HashSet::from([1, 4, 8]));
    let mut run_backends = HashMap::<&str, HashSet<u64>>::new();
    let mut run_pids = HashMap::<&str, HashSet<u64>>::new();
    let mut run_phases = HashMap::<&str, HashSet<&str>>::new();
    for row in &samples {
        assert_eq!(retained_u64(&row[1], "label_count"), 65_536);
        let pid = retained_u64(&row[7], "pid");
        assert!(
            pid > 1,
            "resource evidence must retain a real PostgreSQL backend PID"
        );
        assert!(retained_u64(&row[8], "rss_bytes") > 0);
        assert!(
            retained_u64(&row[9], "pss_bytes") > 0,
            "Linux PSS samples must be nonzero"
        );
        run_backends
            .entry(&row[0])
            .or_default()
            .insert(retained_u64(&row[6], "backend"));
        run_pids.entry(&row[0]).or_default().insert(pid);
        run_phases.entry(&row[0]).or_default().insert(&row[3]);
    }
    for row in &summaries {
        let run_id = row[0].as_str();
        let backend_count = retained_u64(&row[2], "backend_count");
        assert_eq!(
            run_backends.get(run_id).map(HashSet::len),
            Some(usize::try_from(backend_count).expect("bounded backend count fits usize"))
        );
        assert_eq!(
            run_pids.get(run_id).map(HashSet::len),
            Some(usize::try_from(backend_count).expect("bounded backend count fits usize")),
            "resource run must retain one distinct PostgreSQL PID per backend"
        );
        assert_eq!(
            run_phases.get(run_id),
            Some(&HashSet::from(["idle", "loaded", "query"]))
        );
        for field in &row[7..16] {
            assert!(retained_u64(field, "resource summary byte field") > 0);
        }
        retained_u64(&row[16], "query_minus_idle_total_rss_bytes");
        retained_u64(&row[17], "query_minus_idle_total_pss_bytes");
    }
}

#[test]
#[ignore = "P9.5d activates after all retained evidence reconciles against predeclared budgets"]
fn p9_retained_metadata_and_checker_reconcile_budgets_and_lineage() {
    const BUDGET_COMMIT: &str = "dd730b8";
    let evidence = repo_path("todo/measurements/2026-08-13-p9-open-type-query");
    let readme = fs::read_to_string(evidence.join("README.md"))
        .expect("P9 must retain reproducible open-type measurement instructions");
    for required in [
        "Exact commit",
        "cargo bench",
        "PostgreSQL",
        "open_type_query_resources.sh",
        "Linux",
        BUDGET_COMMIT,
    ] {
        assert!(
            readme.contains(required),
            "P9 evidence README is missing `{required}`"
        );
    }
    let metadata: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(evidence.join("run-metadata.json"))
            .expect("P9 must retain machine-readable run metadata"),
    )
    .expect("P9 run metadata must be valid JSON");
    assert_eq!(
        metadata
            .get("budget_commit")
            .and_then(serde_json::Value::as_str),
        Some(BUDGET_COMMIT)
    );
    assert!(metadata
        .get("measurement_commit")
        .and_then(serde_json::Value::as_str)
        .is_some_and(
            |commit| commit.len() == 40 && commit.chars().all(|ch| ch.is_ascii_hexdigit())
        ));
    assert_eq!(
        metadata
            .get("git_status_clean")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let measurement_commit = metadata
        .get("measurement_commit")
        .and_then(serde_json::Value::as_str)
        .expect("P9 metadata needs a measurement commit");
    assert_ne!(measurement_commit, BUDGET_COMMIT);
    let ancestry = Command::new("git")
        .arg("-C")
        .arg(repo_path(""))
        .args([
            "merge-base",
            "--is-ancestor",
            BUDGET_COMMIT,
            measurement_commit,
        ])
        .status()
        .expect("git ancestry check must execute");
    assert!(
        ancestry.success(),
        "budget commit must precede measurement commit"
    );
    assert_eq!(
        metadata
            .pointer("/resources/os")
            .and_then(serde_json::Value::as_str),
        Some("Linux")
    );
    for pointer in [
        "/criterion/command",
        "/criterion/rustc",
        "/postgres/command",
        "/postgres/version",
        "/resources/command",
        "/resources/kernel",
    ] {
        assert!(metadata
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty()));
    }

    let checker = evidence.join("check_results.py");
    let checker_source =
        fs::read_to_string(&checker).expect("P9 needs an executable evidence reconciler");
    for required in [
        "criterion-estimates.csv",
        "criterion-results.csv",
        "postgres-samples.csv",
        "postgres-results.csv",
        "resource-samples.tsv",
        "resource-results.csv",
        "budgets.json",
        "cases.json",
        "merge-base",
        "--is-ancestor",
        "duplicate",
        "median",
        "throughput",
    ] {
        assert!(
            checker_source.contains(required),
            "P9 evidence checker is missing `{required}`"
        );
    }
    let output = Command::new("python3")
        .arg(&checker)
        .arg("--repo-root")
        .arg(repo_path(""))
        .arg("--evidence-dir")
        .arg(&evidence)
        .arg("--budget-commit")
        .arg(BUDGET_COMMIT)
        .output()
        .expect("P9 evidence checker must execute");
    assert!(
        output.status.success(),
        "P9 evidence checker failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("P9 evidence checker must emit one JSON report on stdout");
    for pointer in [
        "/ancestry_verified",
        "/raw_summary_reconciled",
        "/budgets_passed",
    ] {
        assert_eq!(
            report.pointer(pointer).and_then(serde_json::Value::as_bool),
            Some(true),
            "P9 checker report did not prove `{pointer}`"
        );
    }
    assert_eq!(
        report
            .pointer("/counts/criterion_cases")
            .and_then(serde_json::Value::as_u64),
        Some(65)
    );
    assert_eq!(
        report
            .pointer("/counts/postgres_cases")
            .and_then(serde_json::Value::as_u64),
        Some(8)
    );
    assert_eq!(
        report
            .pointer("/counts/resource_runs")
            .and_then(serde_json::Value::as_u64),
        Some(3)
    );
}
