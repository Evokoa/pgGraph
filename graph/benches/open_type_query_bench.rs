//! Production-path relationship-type query benchmark.
//!
//! Registry/filter measurements call the Engine's real O(1) lookup and
//! bounded filter-resolution paths. Traversal measurements use the real CSR
//! builder and BFS hot loop. The matrix is one-factor-at-a-time around degree
//! 8, depth 4, outbound traversal, and an all-types filter.

#![allow(missing_docs)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use graph::bench_support::{
    bfs_execute, edge_type_bytes_one_direction, reverse_edge_store, BfsConfig, EdgeStoreBuilder,
    EdgeTypeFilter, EdgeTypeId, FilterIndexBuilder, NodeStoreBuilder, OpenTypeRegistryBench,
    RawEdge,
};
use std::collections::{HashMap, HashSet};
use std::hint::black_box;
use std::time::Duration;

const LABEL_COUNTS: [u32; 5] = [254, 255, 65_534, 65_535, 65_536];
const DEGREES: [u32; 4] = [1, 8, 64, 1_024];
const DEPTHS: [i32; 3] = [1, 4, 16];
const CASES_JSON: &str =
    include_str!("../../release/evidence/engine/2026-08-13-p9-open-type-query/cases.json");

#[derive(Clone, Copy)]
enum Selectivity {
    NoneMatched,
    OneOfThirtyTwo,
    HalfOfThirtyTwo,
    AllMatched,
    NoFilter,
}

impl Selectivity {
    const fn label(self) -> &'static str {
        match self {
            Self::NoneMatched => "none_matched",
            Self::OneOfThirtyTwo => "one_of_32",
            Self::HalfOfThirtyTwo => "half_of_32",
            Self::AllMatched => "all_matched",
            Self::NoFilter => "no_filter",
        }
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Out,
    In,
}

impl Direction {
    const fn label(self) -> &'static str {
        match self {
            Self::Out => "out",
            Self::In => "in",
        }
    }
}

struct TraversalFixture {
    node_store: NodeStoreBuilder,
    outbound_store: EdgeStoreBuilder,
    inbound_store: EdgeStoreBuilder,
    filter_index: FilterIndexBuilder,
    label_count: u32,
    depth: i32,
    csr_type_bytes_one_direction: usize,
}

impl TraversalFixture {
    fn new(label_count: u32, degree: u32, depth: i32) -> Self {
        let depth_u32 = u32::try_from(depth).expect("benchmark depth is positive");
        let node_count = 1_u32
            .checked_add(degree.checked_mul(depth_u32).expect("fixture nodes fit"))
            .expect("fixture nodes fit");
        let mut node_store = NodeStoreBuilder::with_capacity(node_count as usize);
        for node in 0..node_count {
            node_store.add_node(100, format!("node_{node}"));
        }

        let mut edges = Vec::with_capacity((degree * depth_u32) as usize);
        for level in 0..depth_u32 {
            let source = if level == 0 {
                0
            } else {
                1 + (level - 1) * degree + degree - 1
            };
            let target_start = 1 + level * degree;
            for offset in 0..degree {
                let sequence = level * degree + offset;
                let logical_id = if sequence == 0 {
                    label_count
                } else {
                    1 + sequence % label_count
                };
                edges.push(RawEdge {
                    source,
                    target: target_start + offset,
                    type_id: EdgeTypeId::try_from(logical_id)
                        .expect("benchmark logical relationship type is valid"),
                    weight: None,
                    schema_reversed: false,
                });
            }
        }
        let outbound_store = EdgeStoreBuilder::try_from_edges(node_count, edges.clone(), false)
            .expect("benchmark CSR builds");
        // Construct logical edges in the opposite direction, then use the
        // production reverse-CSR path. The resulting inbound adjacency holds
        // scanned work constant while exercising the independently built
        // reverse store used by inbound traversal.
        let reversed_edges = edges
            .into_iter()
            .map(|edge| RawEdge {
                source: edge.target,
                target: edge.source,
                ..edge
            })
            .collect();
        let inbound_source = EdgeStoreBuilder::try_from_edges(node_count, reversed_edges, false)
            .expect("benchmark reverse source CSR builds");
        let inbound_store = reverse_edge_store(&inbound_source);
        let csr_type_bytes_one_direction = edge_type_bytes_one_direction(&outbound_store);
        Self {
            node_store,
            outbound_store,
            inbound_store,
            filter_index: FilterIndexBuilder::new(),
            label_count,
            depth,
            csr_type_bytes_one_direction,
        }
    }

    fn edge_store(&self, direction: Direction) -> &EdgeStoreBuilder {
        match direction {
            Direction::Out => &self.outbound_store,
            Direction::In => &self.inbound_store,
        }
    }

    fn config(&self, selectivity: Selectivity, degree: u32) -> BfsConfig {
        let encountered = degree
            .checked_mul(u32::try_from(self.depth).expect("fixture depth fits u32"))
            .expect("fixture edge count fits u32");
        let edge_type_filter = match selectivity {
            Selectivity::NoneMatched => EdgeTypeFilter::NoneMatched,
            Selectivity::OneOfThirtyTwo => EdgeTypeFilter::Only(HashSet::from([
                EdgeTypeId::try_from(self.label_count).expect("fixture type is valid"),
            ])),
            Selectivity::HalfOfThirtyTwo => EdgeTypeFilter::Only(
                (0..u32::try_from(self.depth).expect("fixture depth fits u32"))
                    .flat_map(|level| {
                        (degree / 2..degree).map(move |offset| level * degree + offset)
                    })
                    .map(|sequence| {
                        let type_id = if sequence == 0 {
                            self.label_count
                        } else {
                            1 + sequence % self.label_count
                        };
                        EdgeTypeId::try_from(type_id).expect("fixture type is valid")
                    })
                    .collect(),
            ),
            Selectivity::AllMatched => EdgeTypeFilter::Only(
                std::iter::once(self.label_count)
                    .chain(2..=encountered)
                    .map(|type_id| EdgeTypeId::try_from(type_id).expect("fixture type is valid"))
                    .collect(),
            ),
            Selectivity::NoFilter => EdgeTypeFilter::All,
        };
        BfsConfig {
            seed_node: 0,
            max_depth: self.depth,
            max_nodes: 2_000_000,
            max_frontier: 2_000_000,
            edge_type_filter,
            filter_ops: Vec::new(),
            tenant: None,
            tenanted_table_oids: HashSet::new(),
            tenant_membership: HashMap::new(),
            tenant_membership_removals: HashMap::new(),
            overlay_insert_edges: HashMap::new(),
            overlay_deleted_edges: HashMap::new(),
            any_direction_overlays: None,
        }
    }
}

const fn adaptive_encoding(label_count: u32) -> &'static str {
    match label_count {
        0..=254 => "adaptive_u8",
        255..=65_534 => "adaptive_u16",
        _ => "adaptive_u32",
    }
}

fn validate_case_manifest() {
    let cases: serde_json::Value =
        serde_json::from_str(CASES_JSON).expect("predeclared benchmark cases are valid JSON");
    assert_eq!(
        cases.pointer("/criterion/registry_lookup/label_count"),
        Some(&serde_json::json!(LABEL_COUNTS))
    );
    assert_eq!(
        cases.pointer("/criterion/bfs_oat/degree"),
        Some(&serde_json::json!(DEGREES))
    );
    assert_eq!(
        cases.pointer("/criterion/bfs_oat/depth"),
        Some(&serde_json::json!(DEPTHS))
    );
    assert_eq!(
        cases.pointer("/criterion/bfs_oat/selectivity"),
        Some(&serde_json::json!([
            "none_matched",
            "one_of_32",
            "half_of_32",
            "all_matched",
            "no_filter"
        ]))
    );
    assert_eq!(
        cases.pointer("/criterion/bfs_oat/csr_direction"),
        Some(&serde_json::json!(["out", "in"]))
    );
    assert_eq!(
        cases.pointer("/criterion/filter_resolution/request_shape"),
        Some(&serde_json::json!(["empty", "one_exact", "bounded_4096"]))
    );
    assert_eq!(
        cases.pointer("/criterion/filter_resolution/request"),
        Some(&serde_json::json!([
            "first",
            "last",
            "missing",
            "32_labels"
        ]))
    );
    let expected = cases
        .get("criterion_expected_case_count")
        .and_then(serde_json::Value::as_u64)
        .expect("predeclared case count is an integer");
    let registry_and_filter_cases = LABEL_COUNTS.len() * (3 + 3 + 4);
    assert_eq!(
        expected as usize,
        registry_and_filter_cases + traversal_cases().len()
    );
}

fn bench_registry_and_filters(c: &mut Criterion) {
    validate_case_manifest();
    let mut lookup_group = c.benchmark_group("open_type_registry_lookup");
    lookup_group.sample_size(30);
    lookup_group.measurement_time(Duration::from_secs(5));
    for label_count in LABEL_COUNTS {
        let fixture = OpenTypeRegistryBench::new(label_count);
        for (request, label, expected) in [
            ("first", "type_1".to_string(), Some(1)),
            ("last", format!("type_{label_count}"), Some(label_count)),
            ("missing", "type_missing".to_string(), None),
        ] {
            assert_eq!(fixture.lookup(&label), expected);
            lookup_group.bench_with_input(
                BenchmarkId::new(
                    "query_surface=registry_lookup",
                    format!(
                        "label_count={label_count}/request={request}/encoding={}",
                        adaptive_encoding(label_count)
                    ),
                ),
                &label,
                |b, label| b.iter(|| black_box(fixture.lookup(black_box(label)))),
            );
        }
    }
    lookup_group.finish();

    let mut filter_group = c.benchmark_group("open_type_filter_resolution");
    filter_group.sample_size(20);
    filter_group.measurement_time(Duration::from_secs(5));
    for label_count in LABEL_COUNTS {
        let fixture = OpenTypeRegistryBench::new(label_count);
        let one = vec![format!("type_{label_count}")];
        let first = vec!["type_1".to_string()];
        let missing = vec!["type_missing".to_string()];
        let thirty_two = (1..=32).map(|id| format!("type_{id}")).collect::<Vec<_>>();
        let all = (1..=label_count.min(4_096))
            .map(|id| format!("type_{id}"))
            .collect::<Vec<_>>();
        for (request_shape, labels, expected) in [
            ("empty", Some(Vec::new()), 0),
            ("one_exact", Some(one.clone()), 1),
            ("bounded_4096", Some(all.clone()), all.len()),
        ] {
            assert_eq!(fixture.resolve_filter_len(labels.as_deref()), expected);
            filter_group.throughput(Throughput::Elements(expected.max(1) as u64));
            filter_group.bench_with_input(
                BenchmarkId::new(
                    "query_surface=filter_resolution",
                    format!(
                        "label_count={label_count}/request_shape={request_shape}/encoding={}",
                        adaptive_encoding(label_count)
                    ),
                ),
                &labels,
                |b, labels| {
                    b.iter(|| black_box(fixture.resolve_filter_len(black_box(labels.as_deref()))))
                },
            );
        }
        for (request, labels, expected) in [
            ("first", first, Some(1)),
            ("last", one, Some(1)),
            ("missing", missing, None),
            ("32_labels", thirty_two, Some(32)),
        ] {
            assert_eq!(fixture.try_resolve_filter_len(&labels), expected);
            filter_group.bench_with_input(
                BenchmarkId::new(
                    "query_surface=filter_request",
                    format!(
                        "label_count={label_count}/request={request}/encoding={}",
                        adaptive_encoding(label_count)
                    ),
                ),
                &labels,
                |b, labels| b.iter(|| black_box(fixture.try_resolve_filter_len(black_box(labels)))),
            );
        }
    }
    filter_group.finish();
}

fn traversal_cases() -> Vec<(u32, u32, i32, Direction, Selectivity)> {
    let mut cases = LABEL_COUNTS
        .into_iter()
        .map(|label_count| (label_count, 8, 4, Direction::Out, Selectivity::NoFilter))
        .collect::<Vec<_>>();
    cases.extend(
        DEGREES
            .into_iter()
            .map(|degree| (254, degree, 4, Direction::Out, Selectivity::NoFilter)),
    );
    cases.extend(
        DEPTHS
            .into_iter()
            .map(|depth| (254, 8, depth, Direction::Out, Selectivity::NoFilter)),
    );
    cases.push((254, 8, 4, Direction::In, Selectivity::NoFilter));
    cases.extend(
        [
            Selectivity::NoneMatched,
            Selectivity::OneOfThirtyTwo,
            Selectivity::HalfOfThirtyTwo,
            Selectivity::AllMatched,
            Selectivity::NoFilter,
        ]
        .into_iter()
        .map(|selectivity| (254, 8, 4, Direction::Out, selectivity)),
    );
    cases.sort_by_key(|(labels, degree, depth, direction, selectivity)| {
        (
            *labels,
            *degree,
            *depth,
            direction.label(),
            selectivity.label(),
        )
    });
    cases.dedup_by_key(|(labels, degree, depth, direction, selectivity)| {
        (
            *labels,
            *degree,
            *depth,
            direction.label(),
            selectivity.label(),
        )
    });
    cases
}

fn bench_bfs_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("open_type_bfs");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(5));
    for (label_count, degree, depth, direction, selectivity) in traversal_cases() {
        let fixture = TraversalFixture::new(label_count, degree, depth);
        let config = fixture.config(selectivity, degree);
        let expected_minimum = match selectivity {
            Selectivity::NoneMatched => 1,
            Selectivity::OneOfThirtyTwo => 2,
            Selectivity::HalfOfThirtyTwo => 1 + u64::from(degree / 2) * depth as u64,
            Selectivity::AllMatched | Selectivity::NoFilter => 1 + degree as u64 * depth as u64,
        };
        let control = bfs_execute(
            &fixture.node_store,
            fixture.edge_store(direction),
            &fixture.filter_index,
            &config,
        );
        assert_eq!(control.visited.len(), expected_minimum);
        group.throughput(Throughput::Elements(u64::from(degree) * depth as u64));
        group.bench_function(
            BenchmarkId::new(
                "query_surface=bfs",
                format!(
                    "label_count={label_count}/selectivity={}/degree={degree}/depth={depth}/csr_direction={}/csr_type_bytes_one_direction={}/encoding={}",
                    selectivity.label(),
                    direction.label(),
                    fixture.csr_type_bytes_one_direction,
                    adaptive_encoding(label_count)
                ),
            ),
            |b| {
                b.iter(|| {
                    black_box(bfs_execute(
                        black_box(&fixture.node_store),
                        black_box(fixture.edge_store(direction)),
                        black_box(&fixture.filter_index),
                        black_box(&config),
                    ))
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_registry_and_filters, bench_bfs_queries);
criterion_main!(benches);
