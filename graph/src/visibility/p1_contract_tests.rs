//! Red architectural contracts for the visibility coordinator boundary.
//!
//! These tests intentionally inspect the crate's composition sites. The P1
//! change is architectural: the relevant failure is a topology path bypassing
//! the coordinator, even when its current output happens to be correct.

use std::fs;
use std::path::{Path, PathBuf};

fn crate_source(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

fn rust_files(directory: &str) -> Vec<PathBuf> {
    fn collect(path: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(path).expect("source directory must be readable") {
            let path = entry.expect("source entry must be readable").path();
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    collect(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(directory),
        &mut files,
    );
    files.sort();
    files
}

fn production_source(source: &str) -> String {
    source.to_string()
}

fn enclosing_function_is_nonproduction(lines: &[&str], line_index: usize) -> bool {
    let tests_module = (0..=line_index)
        .rev()
        .find(|index| lines[*index].contains("mod tests"));
    if tests_module.is_some_and(|module| {
        let start = module.saturating_sub(3);
        lines[start..module]
            .iter()
            .any(|line| line.trim() == "#[cfg(test)]")
    }) {
        return true;
    }
    let function_line = (0..=line_index)
        .rev()
        .find(|index| lines[*index].contains("fn "));
    let Some(function_line) = function_line else {
        return false;
    };
    let attribute_start = function_line.saturating_sub(12);
    lines[attribute_start..function_line].iter().any(|line| {
        matches!(
            line.trim(),
            "#[cfg(test)]"
                | "#[cfg(feature = \"benchmarks\")]"
                | "#[cfg(any(test, feature = \"benchmarks\"))]"
        )
    })
}

#[test]
fn production_unrestricted_execution_requires_a_sealed_preparation_proof() {
    let mut bypasses = Vec::new();
    for path in rust_files("src") {
        if path.ends_with("visibility.rs")
            || path.ends_with("sql_visibility.rs")
            || path.ends_with("p1_contract_tests.rs")
        {
            continue;
        }
        let source = fs::read_to_string(&path).expect("Rust source must be readable");
        let lines = source.lines().collect::<Vec<_>>();
        for (line_index, line) in lines.iter().enumerate() {
            if line.contains("VisibilityScope::Unrestricted")
                || line.contains("unrestricted_for_test_or_benchmark")
                || line.contains("unrestricted_for_benchmark")
            {
                if enclosing_function_is_nonproduction(&lines, line_index) {
                    continue;
                }
                bypasses.push(format!(
                    "{}:{}",
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .expect("source is under manifest directory")
                        .display(),
                    line_index + 1
                ));
            }
        }
    }

    assert!(
        bypasses.is_empty(),
        "production code constructs unrestricted visibility without the sealed policy-preparation proof: {}",
        bypasses.join(", ")
    );
    let crate_root = crate_source("src/lib.rs");
    assert!(
        crate_root.contains("#[cfg(any(test, feature = \"benchmarks\"))]\npub mod bench_support"),
        "benchmark-only unrestricted execution must not compile in normal production builds"
    );
    let cargo = crate_source("Cargo.toml");
    assert!(cargo.contains("benchmarks = []"));
    let benchmark_targets = cargo.split("[[bench]]").skip(1).collect::<Vec<_>>();
    assert!(
        !benchmark_targets.is_empty()
            && benchmark_targets
                .iter()
                .all(|target| target.contains("required-features = [\"benchmarks\"]")),
        "every benchmark target must require the benchmark-only feature"
    );
}

#[test]
fn coordinator_owns_context_construction_and_retains_the_eager_parity_oracle() {
    let visibility = crate_source("src/visibility.rs");
    assert!(
        visibility.contains("struct VisibilityCoordinator"),
        "P1 requires one coordinator type to own prepared visibility and execution contexts"
    );
    assert!(
        visibility.contains("_proof: crate::sql_visibility::PreparedVisibilityProof"),
        "coordinator construction must require the unforgeable SQL policy-preparation proof"
    );
    assert!(
        visibility.contains("pub(crate) struct VisibilityScope(VisibilityState)"),
        "the unrestricted/enforced representation must remain opaque outside visibility.rs"
    );

    let mut direct_context_sites = Vec::new();
    for path in rust_files("src/sql_facade") {
        let source = fs::read_to_string(&path).expect("facade source must be readable");
        for (line_index, line) in production_source(&source).lines().enumerate() {
            if line.contains("QueryExecutionContext::new")
                || line.contains("QueryExecutionContext::with_edge_type_filter")
            {
                direct_context_sites.push(format!(
                    "{}:{}",
                    path.file_name()
                        .expect("facade has a file name")
                        .to_string_lossy(),
                    line_index + 1
                ));
            }
        }
    }
    assert!(
        direct_context_sites.is_empty(),
        "SQL facades still assemble execution contexts outside the coordinator: {}",
        direct_context_sites.join(", ")
    );

    let bfs = crate_source("src/bfs.rs");
    assert!(
        bfs.contains("eager_coordinator_matches_direct_scope_byte_for_byte"),
        "P1 must retain a byte-for-byte BFS output oracle comparing the coordinator with the direct eager scope"
    );
}

#[test]
fn bounded_ordered_candidate_and_verdict_domain_types_are_present() {
    let visibility = crate_source("src/visibility.rs");
    for required in [
        "enum VisibilityCandidate",
        "struct VisibilityCandidateBatch",
        "enum VisibilityVerdict",
        "struct VisibilityVerdictBatch",
        "struct VisibilityBatchLimits",
    ] {
        assert!(
            visibility.contains(required),
            "P1 visibility domain is missing `{required}`"
        );
    }

    assert!(
        visibility
            .contains("candidate_batches_preserve_sequence_and_reject_count_or_byte_overflow"),
        "P1 batch types need a behavioral test for stable sequence plus count/byte bounds"
    );
    assert!(
        visibility.contains("verdict_batches_reject_unknown_or_misaligned_results"),
        "P1 verdict batches must fail closed on Unknown and reject positional misalignment"
    );
}

#[test]
fn sql_visibility_has_no_direct_postgres_adapter_call_inside_engine_closures() {
    let source = crate_source("src/sql_visibility.rs");
    let mut remaining = source.as_str();
    let mut violations = Vec::new();

    while let Some(start) = remaining.find("ENGINE.with") {
        remaining = &remaining[start..];
        let Some(open) = remaining.find('{') else {
            break;
        };
        let mut depth = 0usize;
        let mut close = None;
        for (offset, byte) in remaining[open..].bytes().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + offset + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.expect("ENGINE.with closure must have balanced braces");
        let closure = &remaining[..close];
        if closure.contains("Spi::") || closure.contains("pgrx::Spi::") {
            violations.push(closure.lines().next().unwrap_or("ENGINE.with"));
        }
        remaining = &remaining[close..];
    }

    assert!(
        violations.is_empty(),
        "PostgreSQL adapter calls must occur only after ENGINE borrows are released"
    );
}
