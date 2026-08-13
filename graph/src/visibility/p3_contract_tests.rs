//! Red architectural contracts for resumable one-hop and BFS visibility.
//!
//! These tests deliberately fail until traversal can pause at a bounded owned
//! candidate boundary, release every projection borrow, resolve node and
//! relationship visibility through PostgreSQL, and resume admission without
//! changing the established BFS result bytes.

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

fn function_body_from_any<'a>(sources: &[&'a str], signature: &str) -> &'a str {
    sources
        .iter()
        .find_map(|source| {
            source
                .contains(signature)
                .then(|| function_body(source, signature))
        })
        .unwrap_or_else(|| panic!("missing function `{signature}` in candidate modules"))
}

#[test]
fn resumable_bfs_owns_every_state_needed_across_visibility_yields() {
    let bfs = crate_source("src/bfs.rs");
    for required in [
        "struct ResumableBfsMachine",
        "enum ResumableBfsState",
        "frontier:",
        "visited:",
        "depth:",
        "parent:",
        "parent_edge_type:",
        "outputs:",
        "adjacency_cursor:",
        "fn take_candidate_batch",
        "fn apply_visibility_verdicts",
    ] {
        assert!(
            bfs.contains(required),
            "P3 resumable BFS is missing the owned state seam `{required}`"
        );
    }
    for state in [
        "NeedCandidates",
        "NeedVisibility",
        "ReadyToAdmit",
        "Complete",
    ] {
        assert!(
            bfs.contains(state),
            "P3 machine state must make `{state}` explicit instead of inferring it from partially populated collections"
        );
    }
}

#[test]
fn adjacency_materialization_is_bounded_owned_and_carries_both_policy_identities() {
    let bfs = crate_source("src/bfs.rs");
    let sql_traversal = crate_source("src/sql_traversal.rs");
    let engine = crate_source("src/engine.rs");
    let visibility = crate_source("src/visibility.rs");
    let candidate_domain = format!("{bfs}\n{sql_traversal}\n{engine}");
    for required in [
        "struct BfsAdjacencyCandidate",
        "struct BfsAdjacencyCandidateBatch",
        "struct BfsCandidateLimits",
        "sequence:",
        "parent_node:",
        "target_node:",
        "target_table_oid:",
        "target_source_key:",
        "edge_type:",
        "relationship_id:",
        "relationship_mapping_id:",
        "relationship_source_key:",
    ] {
        assert!(
            candidate_domain.contains(required),
            "P3 adjacency candidate domain is missing `{required}`"
        );
    }
    assert!(
        candidate_domain.contains(
            "adjacency_candidate_batches_preserve_neighbor_order_and_reject_count_or_byte_overflow"
        ),
        "P3 needs a pure behavioral test for sequence preservation plus candidate/key-byte bounds"
    );
    assert!(
        visibility.contains("VisibilityCandidate::Node")
            && visibility.contains("VisibilityCandidate::Relationship"),
        "P3 must resolve node and relationship source identities through the existing visibility domain"
    );
    assert!(
        candidate_domain
            .contains("adjacency_candidates_emit_node_and_relationship_visibility_in_one_sequence"),
        "P3 needs a pure test proving one topology candidate cannot be admitted until both policy verdicts are aligned"
    );
}

#[test]
fn projection_materialization_and_postgres_resolution_are_separate_phases() {
    let bfs = crate_source("src/bfs.rs");
    let engine = crate_source("src/engine.rs");
    let sql_traversal = crate_source("src/sql_traversal.rs");
    let adapter = crate_source("src/sql_visibility.rs");
    let facade = crate_source("src/sql_facade/traversal.rs");

    let materialize = function_body_from_any(
        &[&bfs, &engine, &sql_traversal],
        "fn materialize_bfs_candidate_batch",
    );
    assert!(
        !materialize.contains("Spi::")
            && !materialize.contains("pgrx::Spi")
            && !materialize.contains("resolve_lazy_visibility_batch"),
        "candidate materialization runs under projection borrows and must remain pure Rust"
    );

    let resolve = function_body(&adapter, "fn resolve_bfs_visibility_batch");
    assert!(
        !resolve.contains("ENGINE.with") && !resolve.contains("ENGINE.borrow"),
        "PostgreSQL policy probing must run only after every ENGINE borrow is released"
    );
    assert!(
        resolve.contains("resolve_lazy_visibility_batch"),
        "P3 BFS must reuse the bounded statement-local policy oracle"
    );

    let execute = function_body_from_any(
        &[&facade, &sql_traversal],
        "fn execute_lazy_traversal_candidates",
    );
    let materialize_at = execute
        .find("materialize_bfs_candidate_batch")
        .expect("lazy BFS facade must materialize a bounded batch");
    let resolve_at = execute
        .find("resolve_bfs_visibility_batch")
        .expect("lazy BFS facade must resolve that batch");
    assert_ne!(
        materialize_at, resolve_at,
        "materialization and PostgreSQL resolution must remain distinct calls"
    );
    assert!(
        bfs.contains("resumable_bfs_never_holds_projection_borrows_while_policy_probe_runs"),
        "P3 needs an instrumentation-backed regression for the borrow/SPI boundary"
    );
}

#[test]
fn lazy_one_hop_and_bfs_have_byte_exact_eager_oracles() {
    let bfs = crate_source("src/bfs.rs");
    let heavy = crate_source("tests/heavy/run_sqlstate_acl_boundary.sh");
    for required in [
        "resumable_bfs_matches_eager_result_bytes",
        "resumable_bfs_preserves_duplicate_parent_selection",
        "resumable_bfs_preserves_max_nodes_and_frontier_truncation",
    ] {
        assert!(
            bfs.contains(required),
            "P3 BFS differential corpus is missing `{required}`"
        );
    }
    for required in [
        "one_hop_lazy_matches_eager_for_node_and_relationship_rls",
        "bounded_bfs_lazy_matches_eager_for_order_parents_caps_and_truncation",
        "multi_seed_lazy_matches_eager_for_order_and_rows",
    ] {
        assert!(
            heavy.contains(required),
            "P3 PostgreSQL differential corpus is missing `{required}`"
        );
    }
}

#[test]
fn frontier_visibility_is_set_based_indexable_and_cancellation_safe() {
    let adapter = crate_source("src/sql_visibility.rs");
    let adapter_tests = crate_source("src/sql_visibility.rs");
    let baseline = crate_source("tests/heavy/rls_large_table_baseline.sh");

    for required in [
        "fn build_bfs_node_probe_query",
        "fn build_bfs_relationship_probe_query",
        "fn resolve_bfs_visibility_batch",
    ] {
        assert!(
            adapter.contains(required),
            "P3 set-based PostgreSQL adapter is missing `{required}`"
        );
    }
    let resolve = function_body(&adapter, "fn resolve_bfs_visibility_batch");
    assert!(
        resolve.contains("VisibilityProbeBatch")
            && resolve.contains("build_bfs_node_probe_query")
            && resolve.contains("build_bfs_relationship_probe_query"),
        "P3 must probe deduplicated frontier sets through the set-based node and relationship builders"
    );
    for required in [
        "lazy_bfs_batches_frontier_per_table_and_relationship_mapping",
        "lazy_bfs_frontier_probe_uses_source_primary_key_indexes",
        "lazy_bfs_policy_cancellation_cleans_machine_and_retries",
    ] {
        assert!(
            adapter_tests.contains(required),
            "P3 set-based/cancellation corpus is missing `{required}`"
        );
    }
    assert!(
        baseline.contains("p3_node_bfs_lazy") && baseline.contains("requested_keys"),
        "the retained large-table runner must measure bounded lazy BFS frontier work"
    );
}

#[test]
fn transaction_node_or_filter_topology_remains_eager_until_its_owned_cursor_exists() {
    let engine = crate_source("src/engine.rs");
    let prepare = function_body(&engine, "fn prepare_resumable_traversal");
    assert!(
        prepare.contains("tx_stats.added_nodes")
            && prepare.contains("tx_stats.filter_updates")
            && prepare.contains("return Ok(None)"),
        "transaction-local node/filter changes must retain eager visibility until P4 owns their resumable cursors"
    );
}

#[test]
fn targeted_surfaces_share_one_resolver_while_non_bfs_algorithms_stay_eager() {
    let facade = crate_source("src/sql_facade/traversal.rs");

    for signature in [
        "fn direct_get_neighbors_rows",
        "fn traverse",
        "fn traverse_many",
    ] {
        let body = function_body(&facade, signature);
        assert!(
            body.contains("execute_lazy_bfs_rows")
                || body.contains("execute_lazy_bfs_candidates")
                || (body.contains("direct_get_neighbors_rows")
                    && function_body(&facade, "fn direct_get_neighbors_rows")
                        .contains("execute_lazy_bfs_rows")),
            "P3 targeted surface `{signature}` must route through resumable lazy BFS"
        );
    }
    let many = function_body(&facade, "fn traverse_many");
    assert!(
        many.contains("execute_lazy_bfs_candidates") && many.contains("&mut lazy"),
        "multi-seed traversal must share one statement-local resolver across roots"
    );
    for signature in [
        "fn shortest_path_rows_governed",
        "fn weighted_shortest_path",
    ] {
        let body = function_body(&facade, signature);
        assert!(
            body.contains("prepare_eager_visibility") && !body.contains("execute_lazy_bfs_rows"),
            "P4-owned surface `{signature}` must remain eager during P3"
        );
    }
}

#[test]
fn batching_and_admission_charge_their_existing_resource_phases() {
    let bfs = crate_source("src/bfs.rs");
    let engine = crate_source("src/engine.rs");
    let sql_traversal = crate_source("src/sql_traversal.rs");
    let adapter = crate_source("src/sql_visibility.rs");
    let materialize = function_body_from_any(
        &[&bfs, &engine, &sql_traversal],
        "fn materialize_bfs_candidate_batch",
    );
    let admit = function_body(&bfs, "fn apply_visibility_verdicts");
    let resolve = function_body(&adapter, "fn resolve_bfs_visibility_batch");

    assert!(
        materialize.contains("consume_expansion"),
        "candidate production must preserve the existing one-work-unit-per-edge expansion charge"
    );
    assert!(
        resolve.contains("ResourcePhase::QueryVisibility")
            && resolve.contains("check_postgres_interrupts"),
        "policy batching must remain governed and cancellation-responsive"
    );
    assert!(
        admit.contains("sequence"),
        "resolved verdicts must be admitted strictly in materialized neighbor order"
    );
    assert!(
        bfs.contains("max_nodes")
            && bfs.contains("max_frontier")
            && bfs.contains("truncated")
            && bfs.contains("resumable_bfs_charges_each_materialized_edge_once_across_yields"),
        "P3 must retain the existing cap/truncation state and prove yielding does not double-charge expansion work"
    );
}
