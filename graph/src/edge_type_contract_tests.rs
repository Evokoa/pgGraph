//! P6 release contracts for the logical relationship-type identity domain.
//!
//! P6 changes Rust's logical type authority without changing the v6 artifact
//! bytes. These tests deliberately keep the type/conversion work separate from
//! the later P7 storage migration and require retained width evidence before
//! P6 can be closed.

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
fn p6_edge_type_id_is_the_unconditional_logical_u32_authority() {
    let types = crate_source("src/types.rs");
    let declaration = types
        .find("pub struct EdgeTypeId")
        .expect("types.rs must define EdgeTypeId");
    let declaration_prefix = &types[declaration.saturating_sub(240)..declaration];

    assert!(
        types.contains("pub struct EdgeTypeId(u32)"),
        "P6 must widen the one logical EdgeTypeId authority to u32"
    );
    assert!(
        !declaration_prefix.contains("cfg(any(test, feature = \"development\"))"),
        "EdgeTypeId must be a production type rather than a test/development wrapper"
    );
    assert_eq!(
        types.matches("struct EdgeTypeId").count(),
        1,
        "the logical edge-type domain must not be shadowed by a second newtype"
    );
    for required in [
        "pub const UNTYPED: Self = Self(0)",
        "pub const SENTINEL: Self = Self(u32::MAX)",
    ] {
        assert!(
            types.contains(required),
            "missing logical invariant `{required}`"
        );
    }

    assert!(
        types.contains("#[repr(transparent)]") && types.contains("pub const fn get(self) -> u32"),
        "the logical authority must expose a stable layout and read-only accessor"
    );
}

#[test]
fn p6_reserved_values_cross_v6_only_through_checked_conversions() {
    let types = crate_source("src/types.rs");
    for required in [
        "impl TryFrom<u32> for EdgeTypeId",
        "pub fn from_v6_storage(value: u8)",
        "pub fn to_v6_storage(self)",
    ] {
        assert!(
            types.contains(required),
            "P6 must expose the checked conversion boundary `{required}`"
        );
    }

    let logical = types
        .split("impl TryFrom<u32> for EdgeTypeId")
        .nth(1)
        .expect("logical conversion exists");
    assert!(
        logical.contains("SENTINEL") && logical.contains("Err("),
        "logical conversion must reject the reserved u32 sentinel"
    );
    let from_v6 = types
        .split("pub fn from_v6_storage(value: u8)")
        .nth(1)
        .expect("v6 widening conversion exists");
    assert!(
        from_v6.contains("u8::MAX") && from_v6.contains("Err("),
        "v6 decoding must reject the all-ones physical sentinel"
    );
    let to_v6 = types
        .split("pub fn to_v6_storage(self)")
        .nth(1)
        .expect("v6 narrowing conversion exists");
    assert!(
        to_v6.contains("u8::try_from") && to_v6.contains("Err("),
        "v6 encoding must be checked and must not truncate logical IDs"
    );
}

#[test]
fn p6_width_benchmark_keeps_reserved_boundaries_and_low_cardinality_control() {
    let cargo = crate_source("Cargo.toml");
    let benchmark = crate_source("benches/edge_type_width_bench.rs");
    let guide = repo_source("docs/contributor_guide/benchmarking.mdx");

    assert!(
        cargo.contains("name = \"edge_type_width_bench\"")
            && cargo.contains("required-features = [\"benchmarks\"]"),
        "the width benchmark must remain an explicit benchmark-only target"
    );
    for boundary in ["254", "255", "65_534", "65_535"] {
        assert!(
            benchmark.contains(boundary),
            "width benchmark is missing reserved boundary `{boundary}`"
        );
    }
    assert!(
        benchmark.contains("EdgeTypeArtifactFixture::new(EDGE_COUNT, 254, CandidateWidth::Four)"),
        "fixed-u32 low-cardinality control must remain comparable with adaptive u8"
    );
    assert!(
        guide.contains(
            "cargo bench --features \"pg17 benchmarks\" --bench edge_type_width_bench --no-run"
        ),
        "the benchmark compile gate must remain reproducible"
    );
}

#[test]
#[ignore = "P6.4 retained multidimensional width evidence checkpoint"]
fn p6_retains_width_measurement_evidence_before_closure() {
    let evidence = repo_path("todo/measurements/2026-08-13-p6-edge-type-width");
    let readme = fs::read_to_string(evidence.join("README.md"))
        .expect("P6 must retain a reproducible edge-type-width measurement README");
    let summary = fs::read_to_string(evidence.join("summary.csv"))
        .expect("P6 must retain machine-readable edge-type-width results");

    for required in [
        "exact commit",
        "cargo bench",
        "u8",
        "u16",
        "u32",
        "artifact bytes",
        "decode",
        "copy",
    ] {
        assert!(
            readme.to_ascii_lowercase().contains(required),
            "P6 evidence README is missing `{required}`"
        );
    }
    let header = summary
        .lines()
        .next()
        .expect("P6 summary must have a header");
    for column in [
        "candidate_width",
        "label_count",
        "edge_count",
        "artifact_bytes",
        "decode_median_ns",
        "copy_median_ns",
    ] {
        assert!(
            header.split(',').any(|candidate| candidate == column),
            "P6 summary is missing `{column}`"
        );
    }
    for case in ["u8,254", "u16,255", "u16,65534", "u32,65535", "u32,254"] {
        assert!(
            summary.lines().skip(1).any(|row| row.starts_with(case)),
            "P6 summary is missing width-boundary case `{case}`"
        );
    }
}
