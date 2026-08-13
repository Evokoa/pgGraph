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

fn compact(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn production_source(relative: &str) -> String {
    crate_source(relative)
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("split always yields the production prefix")
        .to_owned()
}

fn rust_sources_below(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
    {
        let path = entry.expect("source directory entry is readable").path();
        if path.is_dir() {
            rust_sources_below(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
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
fn p6_registry_is_validated_ordered_and_constant_time_by_label() {
    let registry = crate_source("src/edge_type_registry.rs");
    let engine = crate_source("src/engine.rs");
    let persistence = crate_source("src/persistence.rs");
    let persisted_scanner = crate_source("src/persisted_edge_scanner.rs");

    for required in [
        "struct EdgeTypeRegistry",
        "labels: Vec<String>",
        "ids_by_label: HashMap<String, EdgeTypeId>",
        "fn try_from_v6_labels",
        "fn register_v6",
        "fn id(&self, label: &str)",
    ] {
        assert!(
            registry.contains(required),
            "registry is missing `{required}`"
        );
    }
    assert!(
        engine.contains("edge_type_registry: EdgeTypeRegistry")
            && engine.contains(".register_v6(label)")
            && !engine.contains("edge_type_registry.iter().position"),
        "Engine must delegate registration and lookup to the O(1) registry authority"
    );
    assert!(
        persistence.contains("EdgeTypeRegistry::try_from_v6_labels"),
        "artifact load must validate and rebuild the lookup authority"
    );
    for (name, source) in [
        ("engine", engine),
        ("persisted scanner", persisted_scanner),
        ("query executor", crate_source("src/query/execute.rs")),
        ("visibility", crate_source("src/sql_visibility.rs")),
        ("aggregation", crate_source("src/sql_aggregation.rs")),
        ("GQL facade", crate_source("src/sql_facade/gql.rs")),
    ] {
        let compact = source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(
            !compact.contains("edge_type_registry.iter().position")
                && !compact.contains("registry.iter().position(|value|value==label)"),
            "{name} must not bypass the O(1) registry authority"
        );
    }
}

#[test]
fn p6_logical_edge_type_consumers_use_the_newtype() {
    struct LogicalConsumer<'a> {
        path: &'a str,
        required: &'a [&'a str],
        forbidden: &'a [&'a str],
    }

    let consumers = [
        LogicalConsumer {
            path: "src/types.rs",
            required: &["Only(std::collections::HashSet<EdgeTypeId>)"],
            forbidden: &["Only(std::collections::HashSet<u8>)"],
        },
        LogicalConsumer {
            path: "src/bfs.rs",
            required: &["edge_type:EdgeTypeId"],
            forbidden: &["edge_type:u8", "HashMap<u32,u8>"],
        },
        LogicalConsumer {
            path: "src/path_finder.rs",
            required: &["edge_type:EdgeTypeId"],
            forbidden: &["edge_type:u8"],
        },
        LogicalConsumer {
            path: "src/visibility.rs",
            required: &["allows_relationship(&self,edge_type:EdgeTypeId"],
            forbidden: &["allows_relationship(&self,edge_type:u8"],
        },
        LogicalConsumer {
            path: "src/sql_visibility.rs",
            required: &["edge_types:HashSet<EdgeTypeId>"],
            forbidden: &["edge_types:HashSet<u8>"],
        },
        LogicalConsumer {
            path: "src/sql_aggregation.rs",
            required: &["HashSet<EdgeTypeId>"],
            forbidden: &["HashSet<u8>"],
        },
        LogicalConsumer {
            path: "src/query/execute.rs",
            required: &["EdgeTypeId"],
            forbidden: &["rel_type_ids:&[u8]", "rel_type_id:u8"],
        },
        LogicalConsumer {
            path: "src/engine.rs",
            required: &[
                "type_id:EdgeTypeId",
                "edge_type_id(&self,label:&str)->Option<EdgeTypeId>",
            ],
            forbidden: &["type_id:u8", "edge_type_id(&self,label:&str)->Option<u8>"],
        },
        LogicalConsumer {
            path: "src/projection/neighbors.rs",
            required: &[
                "typeOverlayInsert=(u32,EdgeTypeId,bool,Option<RelationshipId>)",
                "typeOverlayDelete=(u32,EdgeTypeId,bool,Option<RelationshipId>)",
                "type_id:EdgeTypeId",
            ],
            forbidden: &[
                "typeOverlayInsert=(u32,u8",
                "typeOverlayDelete=(u32,u8",
                "pub(crate)type_id:u8",
            ],
        },
        LogicalConsumer {
            path: "src/projection/layered.rs",
            required: &["type_id:EdgeTypeId"],
            forbidden: &["type_id:u8", "(u32,u8,bool,RelationshipId)"],
        },
        LogicalConsumer {
            path: "src/projection/tx_delta.rs",
            required: &["type_id:EdgeTypeId"],
            forbidden: &["type_id:u8", "HashSet<(u32,u32,u8"],
        },
        LogicalConsumer {
            path: "src/projection/normalize.rs",
            required: &["type_id:EdgeTypeId"],
            forbidden: &["type_id:u8"],
        },
        LogicalConsumer {
            path: "src/projection/ingest.rs",
            required: &["type_id:EdgeTypeId"],
            forbidden: &["type_id:u8"],
        },
        LogicalConsumer {
            path: "src/projection/segment.rs",
            required: &["type_id:EdgeTypeId"],
            forbidden: &["pub(crate)type_id:u8"],
        },
    ];

    let mut failures = Vec::new();
    for consumer in consumers {
        let source = compact(&production_source(consumer.path));
        for required in consumer.required {
            if !source.contains(required) {
                failures.push(format!("{} is missing `{required}`", consumer.path));
            }
        }
        for forbidden in consumer.forbidden {
            if source.contains(forbidden) {
                failures.push(format!(
                    "{} still exposes raw logical storage `{forbidden}`",
                    consumer.path
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "P6.3 logical edge-type migration is incomplete:\n{}",
        failures.join("\n")
    );
}

#[test]
fn p6_raw_v6_edge_type_bytes_are_confined_to_named_storage_adapters() {
    let adapter_sources = [
        "src/edge_store.rs",
        "src/persisted_build.rs",
        "src/persisted_edge_scanner.rs",
        "src/persistence.rs",
        "src/projection/segment.rs",
    ];

    let mut failures = Vec::new();
    for path in adapter_sources {
        let source = crate_source(path);
        if !source.contains("EdgeTypeId") {
            failures.push(format!(
                "{path} does not name EdgeTypeId at its v6 storage boundary"
            ));
        }
        if !source.contains("from_v6_storage") && !source.contains("to_v6_storage") {
            failures.push(format!(
                "{path} does not cross v6 bytes through a named checked adapter"
            ));
        }
    }

    let raw_logical_patterns = [
        "edge_type: u8",
        "type_id: u8",
        "HashSet<u8>",
        "HashMap<u32, u8>",
        "Option<&HashSet<u8>>",
    ];
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut rust_sources = Vec::new();
    rust_sources_below(&crate_root.join("src"), &mut rust_sources);
    for source_path in rust_sources {
        let path = source_path
            .strip_prefix(crate_root)
            .expect("source belongs to graph crate")
            .to_string_lossy()
            .replace('\\', "/");
        if adapter_sources.contains(&path.as_str())
            || path == "src/edge_type_contract_tests.rs"
            || path == "src/projection/test_fixtures.rs"
            || path.starts_with("src/pg_tests/")
        {
            continue;
        }
        let source = production_source(&path);
        for pattern in raw_logical_patterns {
            if source.contains(pattern) {
                failures.push(format!("{path} contains `{pattern}`"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "raw v6 edge-type bytes escaped the explicit storage adapters:\n{}",
        failures.join("\n")
    );
    assert!(
        !production_source("src/engine.rs").contains("v6_type_ids_bytes"),
        "Engine must consume a logical EdgeStore summary rather than raw v6 type bytes"
    );
}

#[test]
fn p6_logical_migration_preserves_the_v6_byte_contract() {
    let edge_store = crate_source("src/edge_store.rs");
    let persistence = crate_source("src/persistence.rs");
    let segment = crate_source("src/projection/segment.rs");

    assert!(
        edge_store.contains("type_ids: Vec<u8>")
            && edge_store.contains("pub fn v6_type_ids_bytes(&self) -> &[u8]"),
        "P6.3 must not perform P7's physical CSR width migration"
    );
    assert!(
        persistence.contains("p6_logical_edge_type_promotion_preserves_v6_artifact_bytes")
            && persistence.contains("const VERSION: u32 = 6"),
        "the full-file v6 compatibility oracle must remain active"
    );
    assert!(
        segment.contains("const VERSION: u32 = 6") && segment.contains("from_v6_storage"),
        "segment DTOs may widen logically only through the v6 codec adapter"
    );
}

#[test]
fn p6_logical_migration_retains_behavioral_oracles() {
    let oracles = [
        (
            "src/types.rs",
            "edge_type_id_checked_v6_roundtrip_and_boundaries",
        ),
        (
            "src/projection/neighbors.rs",
            "overlay_neighbors_hide_deletes_and_append_inserts",
        ),
        (
            "src/projection/tx_delta.rs",
            "edge_overlay_cancels_local_insert_delete_pairs",
        ),
        (
            "src/path_finder.rs",
            "edge_type_filters_select_longer_unweighted_and_weighted_paths",
        ),
        (
            "src/path_finder.rs",
            "resumable_dijkstra_preserves_weighted_step_metadata_and_edge_type_filters",
        ),
        (
            "src/projection/segment.rs",
            "delta_segment_roundtrips_edge_topology_weight_and_delete_sections",
        ),
        (
            "src/persistence.rs",
            "p6_logical_edge_type_promotion_preserves_v6_artifact_bytes",
        ),
    ];

    for (path, oracle) in oracles {
        assert!(
            crate_source(path).contains(&format!("fn {oracle}")),
            "P6.3 must retain and adapt behavioral oracle `{oracle}` in {path}"
        );
    }
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
