//! BFS Benchmark Suite
//!
//! Measures raw traversal performance of the BFS engine at multiple scales,
//! depths, and filter configurations. No Postgres, no SPI — just the engine.
//!
//! Separates:
//! - **Graph construction** time (not measured in traversal benchmarks)
//! - **BFS execution** time, including per-query visited/depth/parent state
//! - **SQL result materialization**, which is not measured here
//!
//! All graphs use seed=42 for reproducibility.

// Criterion's `criterion_group!` macro generates public harness glue that is
// not useful as API documentation for this benchmark binary.
#![allow(missing_docs)]

mod graph_gen;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use graph::bench_support::*;
use std::collections::{HashMap, HashSet};
use std::hint::black_box;
use std::time::Duration;

// ─── Scale parameters approximating Panama Papers size ───
// Panama: 2,016,523 nodes / 5,792,334 edges ≈ avg degree 2.87
const SEED: u64 = 42;

// Scale tiers for benchmarking
const TINY: u32 = 10_000;
const SMALL: u32 = 100_000;
const MEDIUM: u32 = 500_000;
const PANAMA: u32 = 2_000_000;
const AVG_DEGREE: u32 = 3;

fn traversal_config(seed_node: u32, max_depth: i32) -> BfsConfig {
    BfsConfig {
        seed_node,
        max_depth,
        max_nodes: 1_000_000,
        max_frontier: 1_000_000,
        edge_type_filter: EdgeTypeFilter::All,
        filter_ops: Vec::new(),
        tenant: None,
        tenanted_table_oids: HashSet::new(),
        tenant_membership: HashMap::new(),
        overlay_insert_edges: HashMap::new(),
        overlay_deleted_edges: HashMap::new(),
    }
}

fn add_overlay_edges(config: &mut BfsConfig, graph: &graph_gen::BenchGraph, stride: usize) {
    let node_count = graph.node_store.node_count();
    for source in (0..node_count).step_by(stride) {
        let inserted_target = source.wrapping_add(17) % node_count;
        config
            .overlay_insert_edges
            .insert(source, vec![(inserted_target, 1)]);

        let (targets, type_ids) = graph.edge_store.neighbors(source);
        if let Some((&target, &type_id)) = targets.first().zip(type_ids.first()) {
            config
                .overlay_deleted_edges
                .entry(source)
                .or_default()
                .insert((target, type_id));
        }
    }
}

/// BFS execution over synthetic CSR stores.
///
/// This excludes SQL row materialization, but includes the traversal state
/// allocations performed by `bfs_execute` on each query.
fn bench_bfs_traverse(c: &mut Criterion) {
    let mut group = c.benchmark_group("bfs_traverse");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(50);

    for &(label, node_count) in &[
        ("10k", TINY),
        ("100k", SMALL),
        ("500k", MEDIUM),
        ("2M_panama", PANAMA),
    ] {
        // Build graph ONCE outside the benchmark loop
        let graph = graph_gen::build_benchmark_graph(node_count, AVG_DEGREE, SEED, 2);
        let supernode = graph_gen::find_supernode(&graph);
        let leaf = graph_gen::find_leaf(&graph, SEED + 1);

        group.throughput(Throughput::Elements(node_count as u64));

        // ── Depth 1 from supernode (many neighbors) ──
        group.bench_with_input(
            BenchmarkId::new("d1_supernode", label),
            &(&graph, supernode),
            |b, (g, seed)| {
                b.iter(|| {
                    let config = traversal_config(black_box(*seed), 1);
                    let config = black_box(config);
                    black_box(bfs_execute(
                        black_box(&g.node_store),
                        black_box(&g.edge_store),
                        black_box(&g.filter_index),
                        &config,
                    ))
                })
            },
        );

        // ── Depth 3 from supernode ──
        group.bench_with_input(
            BenchmarkId::new("d3_supernode", label),
            &(&graph, supernode),
            |b, (g, seed)| {
                b.iter(|| {
                    let config = traversal_config(black_box(*seed), 3);
                    let config = black_box(config);
                    black_box(bfs_execute(
                        black_box(&g.node_store),
                        black_box(&g.edge_store),
                        black_box(&g.filter_index),
                        &config,
                    ))
                })
            },
        );

        // ── Depth 5 from supernode ──
        group.bench_with_input(
            BenchmarkId::new("d5_supernode", label),
            &(&graph, supernode),
            |b, (g, seed)| {
                b.iter(|| {
                    let config = traversal_config(black_box(*seed), 5);
                    let config = black_box(config);
                    black_box(bfs_execute(
                        black_box(&g.node_store),
                        black_box(&g.edge_store),
                        black_box(&g.filter_index),
                        &config,
                    ))
                })
            },
        );

        // ── Depth 3 from leaf (sparse start) ──
        group.bench_with_input(
            BenchmarkId::new("d3_leaf", label),
            &(&graph, leaf),
            |b, (g, seed)| {
                b.iter(|| {
                    let config = traversal_config(black_box(*seed), 3);
                    let config = black_box(config);
                    black_box(bfs_execute(
                        black_box(&g.node_store),
                        black_box(&g.edge_store),
                        black_box(&g.filter_index),
                        &config,
                    ))
                })
            },
        );
    }

    group.finish();
}

/// Synthetic graph construction benchmark — how long to build CSR/indexes from
/// generated raw edges. This is not end-to-end SQL `graph.build()` latency.
fn bench_graph_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_construction");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10); // Construction is expensive

    for &(label, node_count) in &[("10k", TINY), ("100k", SMALL), ("500k", MEDIUM)] {
        let fixture = graph_gen::build_benchmark_fixture(node_count, AVG_DEGREE, SEED, 2);
        group.bench_function(BenchmarkId::new("build", label), |b| {
            b.iter_batched(
                || fixture.clone(),
                |fixture| black_box(graph_gen::build_benchmark_graph_from_fixture(fixture)),
                BatchSize::LargeInput,
            )
        });
    }

    group.finish();
}

/// Overlay-aware traversal benchmark.
///
/// The no-overlay case protects the common hot path where `NeighborIter` streams
/// base CSR neighbors without materializing a per-node vector. Sparse and dense
/// cases exercise pending sync insert/delete overlays before vacuum.
fn bench_bfs_overlay_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("bfs_overlay_paths");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(40);

    let graph = graph_gen::build_benchmark_graph(SMALL, AVG_DEGREE, SEED, 2);
    let seed = graph_gen::find_supernode(&graph);
    group.throughput(Throughput::Elements(SMALL as u64));
    let no_overlay_config = traversal_config(seed, 3);
    let mut sparse_config = traversal_config(seed, 3);
    add_overlay_edges(&mut sparse_config, &graph, 257);
    let mut dense_config = traversal_config(seed, 3);
    add_overlay_edges(&mut dense_config, &graph, 1);

    group.bench_function("no_overlay_d3", |b| {
        b.iter(|| {
            let config = black_box(&no_overlay_config);
            black_box(bfs_execute(
                black_box(&graph.node_store),
                black_box(&graph.edge_store),
                black_box(&graph.filter_index),
                config,
            ))
        })
    });

    group.bench_function("sparse_overlay_d3", |b| {
        b.iter(|| {
            let config = black_box(&sparse_config);
            black_box(bfs_execute(
                black_box(&graph.node_store),
                black_box(&graph.edge_store),
                black_box(&graph.filter_index),
                config,
            ))
        })
    });

    group.bench_function("dense_overlay_d3", |b| {
        b.iter(|| {
            let config = black_box(&dense_config);
            black_box(bfs_execute(
                black_box(&graph.node_store),
                black_box(&graph.edge_store),
                black_box(&graph.filter_index),
                config,
            ))
        })
    });

    group.finish();
}

/// Traversal benchmark with registered numeric filter columns.
///
/// Sparse and dense cases exercise the storage-mode split used by the launch
/// `FilterIndex` rather than a separate search index.
fn bench_bfs_filter_index_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("bfs_filter_index_paths");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(40);

    for &(label, populated_percent) in &[("sparse_10pct", 10), ("dense_100pct", 100)] {
        let graph =
            graph_gen::build_filtered_benchmark_graph(SMALL, AVG_DEGREE, SEED, populated_percent);
        let seed = graph_gen::find_supernode(&graph);
        let mut config = traversal_config(seed, 3);
        config.filter_ops = vec![FilterOp::new(0, FilterCondition::Gte(50))];
        group.throughput(Throughput::Elements(SMALL as u64));
        group.bench_with_input(
            BenchmarkId::new("score_gte_50_d3", label),
            &config,
            |b, config| {
                b.iter(|| {
                    black_box(bfs_execute(
                        black_box(&graph.node_store),
                        black_box(&graph.edge_store),
                        black_box(&graph.filter_index),
                        black_box(config),
                    ))
                })
            },
        );
    }

    group.finish();
}

/// Release-readiness benchmark for durable layered projection traversal.
///
/// These scenarios cover the Microphase 16 surfaces that can regress once
/// committed overlay reads are routed through durable projection segments.
fn bench_layered_projection_release_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("layered_projection_release_paths");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);

    let graph = graph_gen::build_benchmark_graph(SMALL, AVG_DEGREE, SEED, 2);
    let seed = graph_gen::find_supernode(&graph);
    let config = traversal_config(seed, 3);
    group.throughput(Throughput::Elements(SMALL as u64));

    for &(label, scenario) in &[
        ("base_only_d3", LayeredProjectionBenchScenario::BaseOnly),
        ("small_l0_d3", LayeredProjectionBenchScenario::SmallL0),
        ("many_l0_d3", LayeredProjectionBenchScenario::ManyL0),
        (
            "compacted_l1_d3",
            LayeredProjectionBenchScenario::CompactedL1,
        ),
        (
            "compacted_l2_d3",
            LayeredProjectionBenchScenario::CompactedL2,
        ),
        (
            "dirty_chunk_rewrite_d3",
            LayeredProjectionBenchScenario::DirtyChunkRewrite,
        ),
        (
            "tx_delta_overlay_d3",
            LayeredProjectionBenchScenario::TxDeltaOverlay,
        ),
        (
            "gql_relationship_expansion_d3",
            LayeredProjectionBenchScenario::GqlRelationshipExpansion,
        ),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                black_box(bfs_layered_projection_execute(
                    black_box(&graph.node_store),
                    black_box(&graph.edge_store),
                    black_box(&graph.filter_index),
                    black_box(&config),
                    black_box(scenario),
                ))
            })
        });
    }

    group.bench_function("weighted_path_dijkstra", |b| {
        b.iter(|| {
            black_box(weighted_layered_projection_path(
                black_box(&graph.node_store),
                black_box(&graph.edge_store),
                black_box(0),
                black_box(127),
            ))
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_bfs_traverse,
    bench_graph_construction,
    bench_bfs_overlay_paths,
    bench_bfs_filter_index_paths,
    bench_layered_projection_release_paths,
);
criterion_main!(benches);
