//! Release-gate contracts for scalable caller-scoped RLS strategy and evidence.
//!
//! These tests deliberately separate the already-supported P4 execution
//! semantics from the P5 release claims. Targeted work must select bounded
//! policy probes, global work must retain the eager oracle, relationship
//! identity completeness must be summarized outside query startup, and scale
//! claims require retained reproducible evidence rather than local anecdotes.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("graph crate must live under the repository root")
        .join(relative)
}

fn repo_source(relative: &str) -> String {
    fs::read_to_string(repo_path(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

fn crate_source(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let remaining = &source[start..];
    let open = remaining.find('{').expect("function must have a body");
    let mut depth = 0usize;
    for (offset, byte) in remaining[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &remaining[..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("function `{signature}` has unbalanced braces")
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_offset = source
        .find(start)
        .unwrap_or_else(|| panic!("missing section start `{start}`"));
    let remaining = &source[start_offset..];
    let end_offset = remaining
        .find(end)
        .unwrap_or_else(|| panic!("missing section end `{end}` after `{start}`"));
    &remaining[..end_offset + end.len()]
}

fn csv_column<'a>(header: &[&str], row: &'a [&str], name: &str) -> &'a str {
    let index = header
        .iter()
        .position(|column| *column == name)
        .unwrap_or_else(|| panic!("missing CSV column `{name}`"));
    row.get(index)
        .copied()
        .unwrap_or_else(|| panic!("CSV row has no value for `{name}`"))
}

#[test]
fn deterministic_selector_inventory_keeps_targeted_work_lazy_and_global_work_eager() {
    let inventory: serde_json::Value = serde_json::from_str(&repo_source(
        "todo/post-v1-1/topology-security-inventory.json",
    ))
    .expect("topology security inventory must be valid JSON");
    let entries = inventory["entrypoints"]
        .as_array()
        .expect("topology inventory entrypoints must be an array");

    for sql_name in ["connected_components", "component_stats"] {
        let matching = entries
            .iter()
            .filter(|entry| entry["sql_name"].as_str() == Some(sql_name))
            .collect::<Vec<_>>();
        assert!(
            !matching.is_empty(),
            "missing global inventory entry `{sql_name}`"
        );
        assert!(
            matching
                .iter()
                .all(|entry| entry["strategy"].as_str() == Some("whole_graph_eager")),
            "global topology entry `{sql_name}` must deterministically retain the eager oracle"
        );
    }

    for sql_name in [
        "get_node",
        "get_neighbors",
        "traverse",
        "shortest_path",
        "weighted_shortest_path",
        "expand",
        "find_related",
        "neighborhood",
    ] {
        let matching = entries
            .iter()
            .filter(|entry| entry["sql_name"].as_str() == Some(sql_name))
            .collect::<Vec<_>>();
        assert!(
            !matching.is_empty(),
            "missing targeted inventory entry `{sql_name}`"
        );
        assert!(
            matching.iter().all(|entry| matches!(
                entry["strategy"].as_str(),
                Some("targeted_scope" | "targeted_source_probe")
            )),
            "targeted topology entry `{sql_name}` must select bounded policy work"
        );
    }

    for sql_name in ["gql", "cypher"] {
        assert!(
            entries.iter().any(|entry| {
                entry["sql_name"].as_str() == Some(sql_name)
                    && entry["strategy"].as_str() == Some("conditional_statement")
            }),
            "{sql_name} must retain its targeted-vs-global physical selector"
        );
    }
}

#[test]
fn relationship_identity_completeness_is_summarized_across_base_durable_and_tx_state() {
    let visibility = crate_source("src/sql_visibility.rs");
    let engine = crate_source("src/engine.rs");
    let tx_delta = crate_source("src/projection/tx_delta.rs");
    let preparation = function_body(&visibility, "fn prepare_bfs_visibility");
    let direct = function_body(&visibility, "fn prepare_direct_identity_visibility");
    let summary = function_body(
        &engine,
        "pub(crate) fn has_missing_relationship_identity_for_types",
    );

    for required in [
        "refresh_relationship_identity_completeness_summary",
        "has_missing_relationship_identity_for_types",
    ] {
        assert!(
            engine.contains(required),
            "loaded projection completeness must provide `{required}`"
        );
    }
    assert!(
        tx_delta.contains("has_missing_relationship_identity_for_types"),
        "transaction deltas must expose monotonic relationship-identity completeness"
    );
    for required in [
        "relationship_identity_missing_edge_types",
        "projection_snapshot",
        "edge_buffer_missing_relationship_identity_edge_types",
        "tx_delta::has_missing_relationship_identity_for_types",
    ] {
        assert!(
            summary.contains(required),
            "the O(1) completeness decision must include `{required}`"
        );
    }
    for (name, body) in [
        ("prepare_bfs_visibility", preparation),
        ("prepare_direct_identity_visibility", direct),
    ] {
        assert!(
            body.contains("has_missing_relationship_identity_for_types"),
            "{name} must consult the O(1) base/durable/transaction completeness summary"
        );
        assert!(
            !body.contains("type_ids_slice")
                && !body.contains("relationship_ids_slice")
                && !body.contains("validate_relationship_identity_completeness")
                && !body.contains(".edge_store"),
            "{name} must not scan projected relationship arrays at query start"
        );
    }
    assert!(
        visibility.contains("RlsRelationshipIdentityMissing"),
        "summary-based validation must retain fail-closed PG023 behavior"
    );
}

#[test]
fn any_future_adaptive_selector_reuses_known_verdicts_instead_of_restarting_policy_work() {
    let visibility = crate_source("src/sql_visibility.rs");
    let design = repo_source("todo/post-v1-1/scalable-rls.md");
    let normalized_design = design.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized_design.contains("must reuse known verdicts when completing an eager scope")
            && normalized_design.contains("may never restart policy work from zero"),
        "the no-restart adaptive policy invariant must remain explicit"
    );

    if visibility.contains("AdaptiveVisibility") {
        assert!(
            visibility.contains("complete_eager_visibility_reusing_verdicts"),
            "adaptive fallback must carry the statement cache into eager completion"
        );
        let fallback = function_body(&visibility, "fn complete_eager_visibility_reusing_verdicts");
        assert!(
            !fallback.contains("prepare_eager_visibility("),
            "adaptive completion must not restart PostgreSQL policy work from zero"
        );
    }
}

#[test]
fn p5_runner_records_gql_selector_resource_and_relationship_summary_metrics() {
    let runner = repo_source("graph/tests/heavy/rls_large_table_baseline.sh");
    for required in [
        "p5_release",
        "p5_gql_identity_one_hop_auto",
        "p5_gql_whole_source_auto",
        "selected_strategy",
        "memory_peak_bytes",
        "work_units",
        "relationship_completeness_checks",
        "selector_class",
    ] {
        assert!(
            runner.contains(required),
            "P5 retained runner is missing `{required}`"
        );
    }
    assert!(
        runner.contains("NODE_COUNT:-1000000") && runner.contains("10000000"),
        "the retained runner must own explicit 1M and 10M release profiles"
    );
    assert!(
        runner.contains("full|compact|p3_selective|p5_release")
            && runner.contains("RUN_PROFILE must be full, compact, p3_selective, or p5_release"),
        "p5_release must be an explicit validated runner profile"
    );

    let sample_schema = section(
        &runner,
        "CREATE TABLE public.rls_bench_samples",
        "GRANT INSERT ON public.rls_bench_samples TO :role_name;",
    );
    for column in [
        "selected_strategy text NOT NULL",
        "selector_class text NOT NULL",
        "memory_peak_bytes bigint NOT NULL",
        "work_units bigint NOT NULL",
        "relationship_completeness_checks bigint NOT NULL",
        "gql_read_recheck_calls bigint NOT NULL",
        "gql_read_recheck_rows bigint NOT NULL",
        "gql_read_recheck_elapsed_micros bigint NOT NULL",
    ] {
        assert!(
            sample_schema.contains(column),
            "P5 sample schema must retain `{column}`"
        );
    }

    let measure = section(
        &runner,
        "CREATE FUNCTION public.rls_bench_measure",
        "GRANT EXECUTE ON FUNCTION public.rls_bench_measure(text,text,text,text,text,text,text,text,bigint,integer) TO :role_name;",
    );
    for metric in [
        "metrics->>'selected_strategy'",
        "metrics->>'memory_peak_bytes'",
        "metrics->>'work_units'",
        "metrics->>'relationship_completeness_checks'",
        "metrics->>'gql_read_recheck_calls'",
        "metrics->>'gql_read_recheck_rows'",
        "metrics->>'gql_read_recheck_elapsed_micros'",
    ] {
        assert!(
            measure.contains(metric),
            "P5 measurement must persist the runtime metric `{metric}`"
        );
    }
    assert!(
        measure.contains("selector_class text")
            && measure.contains("selector_class, key_shape")
            && !measure.contains("WHEN 'lazy' THEN 'targeted'"),
        "selector_class must be supplied by the workload shape, independently of its oracle strategy"
    );
    assert!(
        measure.contains("PERFORM graph._test_set_visibility_strategy(visibility_strategy)")
            && measure.find("EXECUTE query_sql INTO observed")
                < measure.find("metrics := graph._test_visibility_metrics()"),
        "each sample must reset metrics before execution and read them immediately afterward"
    );

    for required_case in [
        "p5_gql_identity_one_hop_auto",
        "p5_gql_whole_source_auto",
        "p5_no_rls_auto",
    ] {
        assert!(
            runner.contains(required_case),
            "P5 runner must execute `{required_case}`"
        );
    }
    assert!(
        runner.contains("selected_strategy <> 'lazy'")
            && runner.contains("selected_strategy <> 'eager'")
            && runner.contains("spi_calls <> 0")
            && runner.contains("source_rows <> 0")
            && runner.contains("relationship_completeness_checks"),
        "P5 runner must fail when selector, no-RLS, or completeness telemetry is semantically wrong"
    );
}

#[test]
fn p5_metrics_surface_resets_statement_counters_and_reports_resource_snapshot() {
    let visibility = crate_source("src/sql_visibility.rs");
    let setter = function_body(&visibility, "fn test_set_visibility_strategy");
    let metrics = function_body(&visibility, "fn test_visibility_metrics");

    for reset in [
        "VISIBILITY_SELECTED_STRATEGY",
        "VISIBILITY_LAST_METRICS",
        "BFS_VISIBILITY_LAST_METRICS",
        "GQL_READ_RECHECK_METRICS",
    ] {
        assert!(
            setter.contains(reset),
            "strategy reset must clear `{reset}` before each retained sample"
        );
    }
    for field in [
        "selected_strategy",
        "relationship_completeness_checks",
        "memory_peak_bytes",
        "work_units",
        "gql_read_recheck_calls",
        "gql_read_recheck_rows",
        "gql_read_recheck_elapsed_micros",
    ] {
        assert!(
            metrics.contains(field),
            "development metrics JSON must expose `{field}`"
        );
    }
    assert!(
        metrics.contains("last_operation_snapshot"),
        "resource telemetry must come from the completed operation snapshot"
    );
}

#[test]
#[ignore = "P5.3 retained 1M/10M evidence checkpoint"]
fn p5_retained_1m_and_10m_evidence_is_complete_and_budgeted() {
    let measurements = repo_path("todo/measurements");
    for scale in ["1m", "10m"] {
        let expected_nodes = match scale {
            "1m" => "node_count=1000000",
            "10m" => "node_count=10000000",
            _ => unreachable!(),
        };
        let suffix = format!("-p5-selective-rls-{scale}");
        let matching = fs::read_dir(&measurements)
            .expect("measurement inventory must be readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(&suffix))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "P5 requires exactly one retained {scale} release evidence directory"
        );
        let directory = &matching[0];
        assert!(
            directory.is_dir(),
            "P5 requires retained {scale} evidence at {}",
            directory.display()
        );
        for required in [
            "README.md",
            "commit.txt",
            "git-status.txt",
            "run-metadata.txt",
            "attempts.csv",
            "samples.csv",
            "summary.csv",
            "database-metadata.csv",
            "budgets.json",
        ] {
            assert!(
                directory.join(required).is_file(),
                "P5 {scale} evidence is missing `{required}`"
            );
        }
        let metadata = fs::read_to_string(directory.join("run-metadata.txt"))
            .expect("P5 run metadata must be readable");
        assert!(
            metadata.lines().any(|line| line == "status=complete")
                && metadata.lines().any(|line| line == "dirty=false")
                && metadata.lines().any(|line| line == expected_nodes),
            "P5 {scale} evidence must be a complete run from a clean commit"
        );
        let attempts = fs::read_to_string(directory.join("attempts.csv"))
            .expect("P5 attempts must be readable");
        assert!(
            attempts.lines().nth(1).is_some()
                && attempts
                    .lines()
                    .skip(1)
                    .all(|line| { line.split(',').nth(1) == Some("complete") }),
            "P5 {scale} evidence may not close with censored or failed cases"
        );
        let budgets: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(directory.join("budgets.json"))
                .expect("P5 budget manifest must be readable"),
        )
        .expect("P5 budget manifest must be valid JSON");
        for field in [
            "required_samples",
            "max_p95_total_ms",
            "max_p95_visibility_ms",
            "max_memory_peak_bytes",
            "max_source_rows",
            "max_work_units",
        ] {
            assert!(
                budgets
                    .get(field)
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|value| value > 0),
                "P5 {scale} budget manifest needs positive integer `{field}`"
            );
        }
        let summary =
            fs::read_to_string(directory.join("summary.csv")).expect("P5 summary must be readable");
        for column in [
            "samples",
            "p50_ms",
            "p95_ms",
            "p50_visibility_ms",
            "p95_visibility_ms",
            "max_spi_calls",
            "max_requested_keys",
            "max_source_rows",
            "max_memory_peak_bytes",
            "max_work_units",
            "selected_strategy",
            "selector_class",
        ] {
            assert!(
                summary
                    .lines()
                    .next()
                    .is_some_and(|header| header.split(',').any(|value| value == column)),
                "P5 {scale} summary is missing `{column}`"
            );
        }
        let rows = summary.lines().skip(1).collect::<Vec<_>>();
        assert!(
            rows.iter()
                .any(|row| row.split(',').any(|value| value == "targeted"))
                && rows
                    .iter()
                    .any(|row| row.split(',').any(|value| value == "global")),
            "P5 {scale} summary must retain both targeted and global selector classes"
        );
        assert!(
            rows.iter()
                .any(|row| row.split(',').any(|value| value == "lazy"))
                && rows
                    .iter()
                    .any(|row| row.split(',').any(|value| value == "eager")),
            "P5 {scale} summary must prove the selected lazy/eager strategies"
        );

        let header = summary
            .lines()
            .next()
            .expect("P5 summary must have a header")
            .split(',')
            .collect::<Vec<_>>();
        let required_samples = budgets["required_samples"]
            .as_u64()
            .expect("required_samples was checked above");
        let max_p95_total_ms = budgets["max_p95_total_ms"]
            .as_u64()
            .expect("max_p95_total_ms was checked above") as f64;
        let max_p95_visibility_ms = budgets["max_p95_visibility_ms"]
            .as_u64()
            .expect("max_p95_visibility_ms was checked above")
            as f64;
        let max_memory_peak_bytes = budgets["max_memory_peak_bytes"]
            .as_u64()
            .expect("max_memory_peak_bytes was checked above");
        let max_source_rows = budgets["max_source_rows"]
            .as_u64()
            .expect("max_source_rows was checked above");
        let max_work_units = budgets["max_work_units"]
            .as_u64()
            .expect("max_work_units was checked above");
        for line in rows {
            let row = line.split(',').collect::<Vec<_>>();
            let samples = csv_column(&header, &row, "samples")
                .parse::<u64>()
                .expect("P5 samples must be an integer");
            let p95_total = csv_column(&header, &row, "p95_ms")
                .parse::<f64>()
                .expect("P5 p95_ms must be numeric");
            let p95_visibility = csv_column(&header, &row, "p95_visibility_ms")
                .parse::<f64>()
                .expect("P5 p95_visibility_ms must be numeric");
            let peak_memory = csv_column(&header, &row, "max_memory_peak_bytes")
                .parse::<u64>()
                .expect("P5 max_memory_peak_bytes must be an integer");
            let work_units = csv_column(&header, &row, "max_work_units")
                .parse::<u64>()
                .expect("P5 max_work_units must be an integer");
            assert!(
                samples >= required_samples,
                "P5 {scale} row is undersampled"
            );
            assert!(
                p95_total <= max_p95_total_ms
                    && p95_visibility <= max_p95_visibility_ms
                    && peak_memory <= max_memory_peak_bytes
                    && work_units <= max_work_units,
                "P5 {scale} latency, memory, or work budget was exceeded"
            );
            if csv_column(&header, &row, "selector_class") == "targeted" {
                let source_rows = csv_column(&header, &row, "max_source_rows")
                    .parse::<u64>()
                    .expect("P5 max_source_rows must be an integer");
                assert!(
                    source_rows <= max_source_rows,
                    "P5 {scale} targeted source-work budget was exceeded"
                );
            }
        }
        let plans = directory.join("plans");
        assert!(
            plans.is_dir()
                && fs::read_dir(&plans)
                    .expect("P5 plan inventory must be readable")
                    .filter_map(Result::ok)
                    .any(|entry| entry.path().is_file()),
            "P5 {scale} evidence must retain at least one query plan"
        );
    }
}

#[test]
#[ignore = "P4.7/P5.3 documentation and evidence closure checkpoint"]
fn p4_and_p5_close_only_with_public_docs_and_retained_evidence_links() {
    let ledger = repo_source("todo/post-v1-1/README.md");
    let supported = repo_source("docs/user_guide/supported_features.md");
    assert!(
        ledger.contains("| P4 | Complete |") && ledger.contains("| P5 | Complete |"),
        "P4/P5 may close only after retained release evidence passes"
    );
    for required in [
        "p5-selective-rls-1m",
        "p5-selective-rls-10m",
        "relationship-identity completeness summary",
        "deterministic targeted-lazy/global-eager",
    ] {
        assert!(
            ledger.contains(required) || supported.contains(required),
            "P4/P5 closure documentation is missing `{required}`"
        );
    }
}
