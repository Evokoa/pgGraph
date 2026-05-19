//! Deterministic graph generator for benchmarks.
//!
//! Produces power-law degree distributions similar to real-world graphs
//! (e.g., Panama Papers: 2M nodes, 5.8M edges ≈ avg degree 2.9).
//!
//! Seed = 42 for all published benchmarks. Reproducible.

use graph::bench_support::{EdgeStoreBuilder, FilterIndexBuilder, NodeStoreBuilder};

/// Simple xorshift64 PRNG — deterministic, fast, no external dependency.
pub struct Rng(u64);

impl Rng {
    /// Create a deterministic generator from `seed`.
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 { 1 } else { seed })
    }

    /// Return the next pseudo-random `u64`.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Return the next pseudo-random `u32`.
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 16) as u32
    }

    /// Return a value in [0, n)
    #[inline]
    pub fn next_bounded(&mut self, n: u32) -> u32 {
        ((self.next_u32() as u64 * n as u64) >> 32) as u32
    }
}

/// Stores needed for BFS benchmarking.
pub struct BenchGraph {
    /// Synthetic node metadata store.
    pub node_store: NodeStoreBuilder,
    /// Synthetic CSR edge store.
    pub edge_store: EdgeStoreBuilder,
    /// Empty or synthetic filter index used by traversal benchmarks.
    pub filter_index: FilterIndexBuilder,
}

/// Generated source-table row before benchmark indexes are built.
#[derive(Clone)]
pub struct GeneratedNode {
    /// Source table OID assigned to the synthetic row.
    pub table_oid: u32,
    /// Synthetic primary-key value.
    pub pk: String,
}

/// Generated benchmark fixture before CSR and secondary indexes are built.
#[derive(Clone)]
pub struct GeneratedBenchmarkGraph {
    /// Synthetic source rows.
    pub nodes: Vec<GeneratedNode>,
    /// Synthetic raw directed edges.
    pub raw_edges: Vec<graph::bench_support::RawEdge>,
}

/// Build a benchmark graph with power-law degree distribution.
///
/// # Parameters
/// - `node_count`: Number of nodes
/// - `avg_degree`: Average outgoing edges per node (actual is power-law distributed)
/// - `seed`: PRNG seed for reproducibility
/// - `num_properties`: How many property key-value pairs per node
pub fn build_benchmark_graph(
    node_count: u32,
    avg_degree: u32,
    seed: u64,
    num_properties: u32,
) -> BenchGraph {
    build_benchmark_graph_from_fixture(build_benchmark_fixture(
        node_count,
        avg_degree,
        seed,
        num_properties,
    ))
}

/// Build a benchmark graph with one numeric filter column populated at the
/// requested percentage. Values cycle across 0..100 for predictable selectivity.
pub fn build_filtered_benchmark_graph(
    node_count: u32,
    avg_degree: u32,
    seed: u64,
    populated_percent: u32,
) -> BenchGraph {
    let mut graph = build_benchmark_graph(node_count, avg_degree, seed, 1);
    let populated_count = (node_count as usize)
        .saturating_mul(populated_percent as usize)
        .saturating_div(100)
        .max(1)
        .min(node_count as usize);
    let column_idx = graph
        .filter_index
        .register_typed_column_with_populated_count(
            100,
            "score".to_string(),
            graph::bench_support::FilterColumnType::Numeric,
            node_count as usize,
            populated_count,
        );
    for node_idx in 0..populated_count {
        graph
            .filter_index
            .set_value(column_idx, node_idx as u32, (node_idx % 100) as u32);
    }
    graph
}

/// Generate deterministic rows and raw edges without building graph indexes.
///
/// Construction benchmarks use this split to measure fixture generation
/// separately from CSR and index construction.
pub fn build_benchmark_fixture(
    node_count: u32,
    avg_degree: u32,
    seed: u64,
    _num_properties: u32,
) -> GeneratedBenchmarkGraph {
    let mut rng = Rng::new(seed);

    let mut nodes = Vec::with_capacity(node_count as usize);
    for i in 0..node_count {
        nodes.push(GeneratedNode {
            table_oid: 100,
            pk: format!("PK-{}", i),
        });
    }

    let total_edges = (node_count as u64 * avg_degree as u64) as usize;
    let mut raw_edges = Vec::with_capacity(total_edges * 2);

    let mut edge_list: Vec<u32> = (0..node_count).collect();

    for _ in 0..total_edges {
        let source = rng.next_bounded(node_count);
        let target_idx = (rng.next_u64() % edge_list.len() as u64) as usize;
        let target = edge_list[target_idx];

        if source == target {
            continue;
        }

        raw_edges.push(graph::bench_support::RawEdge {
            source,
            target,
            type_id: 1,
            weight: None,
        });
        // Bidirectional
        raw_edges.push(graph::bench_support::RawEdge {
            source: target,
            target: source,
            type_id: 1,
            weight: None,
        });

        // Grow the edge list → nodes with more edges get picked more often
        edge_list.push(source);
        edge_list.push(target);
    }

    GeneratedBenchmarkGraph { nodes, raw_edges }
}

/// Build CSR/index structures from already-generated benchmark rows and edges.
///
/// Construction benchmarks use this to exclude synthetic data generation cost.
pub fn build_benchmark_graph_from_fixture(fixture: GeneratedBenchmarkGraph) -> BenchGraph {
    let mut node_store = NodeStoreBuilder::new();

    for node in fixture.nodes {
        node_store.add_node(node.table_oid, node.pk);
    }

    let edge_store =
        EdgeStoreBuilder::try_from_edges(node_store.node_count(), fixture.raw_edges, false)
            .expect("benchmark generator emits in-range edge endpoints");

    let filter_index = FilterIndexBuilder::new();

    BenchGraph {
        node_store,
        edge_store,
        filter_index,
    }
}

/// Find a "supernode" — a node with high degree, useful for stress-testing BFS.
/// Returns the node_idx with highest degree.
pub fn find_supernode(graph: &BenchGraph) -> u32 {
    let n = graph.node_store.node_count();
    let mut best_idx = 0u32;
    let mut best_degree = 0u32;
    for i in 0..n {
        let deg = graph.edge_store.degree(i);
        if deg > best_degree {
            best_degree = deg;
            best_idx = i;
        }
    }
    best_idx
}

/// Find a "leaf" node — low degree, for testing sparse traversals.
pub fn find_leaf(graph: &BenchGraph, rng_seed: u64) -> u32 {
    let n = graph.node_store.node_count();
    let mut rng = Rng::new(rng_seed);
    // Find a node with degree 1-3
    for _ in 0..1000 {
        let idx = rng.next_bounded(n);
        let deg = graph.edge_store.degree(idx);
        if (1..=3).contains(&deg) {
            return idx;
        }
    }
    0 // Fallback to node 0
}
