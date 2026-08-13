//! Red architectural contracts for complete targeted lazy topology coverage.
//!
//! Phase 3 intentionally selects the eager oracle for mutable topology and for
//! targeted algorithms other than clean-CSR BFS. These contracts keep Phase 4
//! honest: representation-specific cursors must be owned across PostgreSQL
//! visibility probes, and each migrated algorithm must retain its established
//! ordering and admission semantics.

use std::fs;
use std::path::Path;

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

#[test]
fn bounded_classic_edge_overlays_use_owned_cursors_instead_of_eager_fallbacks() {
    let engine = crate_source("src/engine.rs");
    let neighbors = crate_source("src/projection/neighbors.rs");
    let prepare = function_body(&engine, "fn prepare_resumable_bfs");

    assert!(
        !prepare.contains("!self.edge_buffer.is_empty()"),
        "bounded committed edge overlays must not force a table-wide eager visibility scan"
    );
    assert!(
        neighbors.contains("OwnedNeighborCursor"),
        "classic overlays need an owned cursor that can cross the ENGINE/SPI boundary without retaining projection borrows"
    );
    for required in [
        "resumable_bfs_matches_eager_with_committed_overlay",
        "resumable_bfs_matches_eager_with_transaction_delta",
    ] {
        assert!(
            engine.contains(required),
            "P4 mutable-topology differential corpus is missing `{required}`"
        );
    }
}

#[test]
#[ignore = "P4 DFS/reverse checkpoint contract"]
fn dfs_and_reverse_traversal_freeze_order_and_visited_timing_before_migration() {
    let bfs = crate_source("src/bfs.rs");
    let engine = crate_source("src/engine.rs");

    for required in [
        "resumable_dfs_matches_eager_reversed_push_order_and_visited_timing",
        "resumable_reverse_traversal_matches_eager_across_overlay_durable_and_tx",
    ] {
        assert!(
            bfs.contains(required) || engine.contains(required),
            "P4 traversal differential corpus is missing `{required}`"
        );
    }
    assert!(
        bfs.contains("ResumableDfsMachine"),
        "DFS must own its stack, visited timing, reversed-neighbor cursor, parents, and outputs before policy probes can run outside ENGINE borrows"
    );
}

#[test]
#[ignore = "P4 path checkpoints contract"]
fn targeted_paths_freeze_meeting_heap_and_tie_order_before_migration() {
    let paths = crate_source("src/path_finder.rs");

    for required in [
        "resumable_bidirectional_bfs_matches_eager_meeting_node_selection",
        "resumable_dijkstra_matches_eager_heap_and_tie_order",
    ] {
        assert!(
            paths.contains(required),
            "P4 path differential corpus is missing `{required}`"
        );
    }
    assert!(
        paths.contains("ResumableBidirectionalBfs") && paths.contains("ResumableDijkstra"),
        "targeted paths need explicit resumable state; endpoint-only probing is not a substitute for filtering every intermediate during traversal"
    );
}

#[test]
#[ignore = "P4 workflow checkpoint contract"]
fn targeted_workflows_share_one_statement_local_resolver() {
    let workflow = crate_source("src/sql_facade/workflow.rs");
    let traversal = crate_source("src/sql_facade/traversal.rs");

    assert!(
        workflow.contains("targeted_workflows_share_one_lazy_resolver_and_match_eager_rows"),
        "P4 workflows need an eager/lazy differential that covers exact ordered rows and diagnostics"
    );
    assert!(
        workflow.contains("LazyVisibilityResolver")
            && (workflow.contains("execute_lazy") || traversal.contains("execute_lazy_path")),
        "one top-level workflow invocation must reuse one statement-local resolver across its targeted internal traversals"
    );
}

#[test]
fn global_topology_work_remains_on_the_eager_oracle() {
    let components = crate_source("src/sql_facade/components.rs");
    let aggregation = crate_source("src/sql_aggregation.rs");

    for (source, signature) in [
        (&components, "fn connected_components"),
        (&components, "fn component_stats"),
    ] {
        let body = function_body(source, signature);
        assert!(
            body.contains("prepare_eager_visibility") && !body.contains("execute_lazy"),
            "global surface `{signature}` must retain the eager oracle until it has a purpose-built resumable executor"
        );
    }
    assert!(
        !aggregation.contains("ResumablePathCount"),
        "path-count analytics are explicitly outside the P4 targeted-lazy slice"
    );
}
