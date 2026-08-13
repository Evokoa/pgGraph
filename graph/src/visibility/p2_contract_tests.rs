//! Red contracts for bounded statement-local lazy visibility resolution.
//!
//! P2 is not complete until these source-level composition contracts are
//! backed by the named behavioral tests in `visibility` and `sql_visibility`.

use super::*;
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
fn statement_cache_is_bounded_tri_state_and_query_scoped() {
    let visibility = crate_source("src/visibility.rs");
    for required in [
        "struct VisibilityStatementCache",
        "struct VisibilityCacheLimits",
        "fn node_verdict",
        "fn relationship_verdict",
        "fn try_record",
    ] {
        assert!(
            visibility.contains(required),
            "P2 statement cache is missing `{required}`"
        );
    }
    assert!(
        visibility.contains("statement_cache_records_negative_verdicts_and_charges_count_and_bytes"),
        "P2 needs a behavioral cache test covering Unknown, Visible, Hidden, negative-cache hits, and count/byte exhaustion"
    );
    assert!(
        visibility.contains("statement_cache_reuses_hidden_and_visible_verdicts_without_reprobing"),
        "P2 needs a pure behavioral test proving both cache polarities suppress duplicate probes"
    );
    assert!(
        visibility.contains("cache: VisibilityStatementCache"),
        "the query-scoped coordinator must own the cache; backend/global storage is forbidden"
    );
}

#[test]
fn lazy_probe_deduplicates_keys_without_reordering_candidate_verdicts() {
    let visibility = crate_source("src/visibility.rs");
    let adapter = crate_source("src/sql_visibility.rs");
    assert!(
        visibility.contains("struct VisibilityProbeBatch"),
        "P2 needs a distinct deduplicated probe batch rather than mutating ordered candidates"
    );
    assert!(
        visibility.contains("deduplicated_probe_keys_preserve_first_seen_order_and_fan_out_verdicts"),
        "P2 needs a duplicate-key behavioral test with stable first-seen probing and original candidate-order fan-out"
    );
    assert!(
        visibility
            .contains("deduplicated_probe_keys_keep_node_and_relationship_namespaces_distinct"),
        "P2 dedupe must not alias equal text keys from different tables or relationship mappings"
    );
    assert!(
        adapter.contains("resolve_lazy_visibility_batch"),
        "the PostgreSQL adapter lacks the bounded lazy batch resolver"
    );
    assert!(
        !function_body(&adapter, "fn resolve_lazy_visibility_batch")
            .contains("max(pg_catalog.octet_length"),
        "lazy resolution must reserve from bounded requested key bytes, not scan the table for maximum key width"
    );
}

#[test]
fn missing_relationship_identity_stays_fail_closed_before_any_probe() {
    let mut rls_edge_types = RoaringBitmap::new();
    rls_edge_types.insert(3);
    let coordinator =
        VisibilityCoordinator::from_scope_for_test(VisibilityScope::enforced_for_test(
            RoaringBitmap::new(),
            RoaringBitmap::new(),
            rls_edge_types,
        ));
    let batch = VisibilityCandidateBatch::try_new(
        vec![VisibilityCandidate::Relationship {
            sequence: 0,
            mapping_id: 9,
            source_key: "relationship-key".into(),
            relationship_id: None,
            edge_type: 3,
        }],
        VisibilityBatchLimits {
            max_candidates: 1,
            max_key_bytes: 32,
        },
    )
    .expect("bounded candidate fixture");

    assert!(matches!(
        coordinator.resolve_prepared_batch(&batch),
        Err(GraphError::RlsRelationshipIdentityMissing)
    ));
}

#[test]
fn recursive_visibility_guard_has_error_and_cancellation_cleanup_contracts() {
    let adapter = crate_source("src/sql_visibility.rs");
    for required in [
        "VISIBILITY_RESOLUTION_ACTIVE",
        "struct VisibilityResolutionGuard",
        "recursive_visibility_resolution_is_rejected",
        "visibility_resolution_guard_clears_after_error_and_cancellation",
        "_test_visibility_resolution_guard_empty",
    ] {
        assert!(
            adapter.contains(required),
            "P2 recursive-resolution cleanup contract is missing `{required}`"
        );
    }
    let resolver = function_body(&adapter, "fn resolve_lazy_visibility_batch");
    assert!(
        resolver.contains("PgTryBuilder") && resolver.contains(".finally"),
        "lazy SPI resolution must clear the reentrancy guard through PostgreSQL finally cleanup"
    );
    assert!(
        resolver.contains("VisibilityResolutionGuard"),
        "the guard must cover visibility resolution itself, not the surrounding topology query"
    );
}

#[test]
fn direct_identity_and_depth_zero_select_the_narrow_lazy_path() {
    let adapter = crate_source("src/sql_visibility.rs");
    let traversal = crate_source("src/sql_facade/traversal.rs");
    let pg_tests = crate_source("src/pg_tests/gql.rs");
    assert!(
        adapter.contains("prepare_direct_identity_visibility"),
        "P2 needs a narrowly named lazy preparation entry point for no-expansion identity probes"
    );

    let get_node = function_body(&traversal, "fn direct_get_node_rows");
    assert!(
        get_node.contains("prepare_direct_identity_visibility")
            && get_node.contains("resolve_lazy_visibility_batch"),
        "graph.get_node must resolve its one identity through the lazy coordinator"
    );
    assert!(
        !get_node.contains("source_row_visible"),
        "graph.get_node must not keep a separate post-resolution visibility oracle"
    );

    let traverse = function_body(&traversal, "fn traverse");
    let depth_zero = function_body(&traversal, "fn execute_depth_zero_lazy");
    assert!(
        traverse.contains("max_depth == 0")
            && traverse.contains("prepare_bfs_visibility")
            && traverse.contains("prepare_bfs_eager_fallback")
            && depth_zero.contains("prepare_direct_identity_visibility")
            && depth_zero.contains("resolve_lazy_visibility_batch"),
        "depth-zero traversal must retain its narrow direct-identity path while expanding traversal uses the P3 BFS resolver with eager fallback"
    );

    assert!(
        pg_tests.contains("direct_identity_lazy_matches_eager_for_visible_hidden_and_absent_rows")
            && pg_tests
                .contains("depth_zero_lazy_matches_eager_for_visible_hidden_and_absent_rows")
            && pg_tests
                .contains("hidden_and_absent_direct_identity_preserve_1_1_diagnostic_distinction"),
        "P2 needs differential result and diagnostic tests for get_node and depth-zero traversal"
    );
}

#[test]
fn positive_depth_unweighted_paths_use_the_later_resumable_oracle() {
    let traversal = crate_source("src/sql_facade/traversal.rs");
    let shortest = function_body(&traversal, "fn shortest_path_rows_governed");
    assert!(
        shortest.contains("prepare_bfs_visibility")
            && shortest.contains("execute_lazy_shortest_path_rows")
            && shortest.contains("prepare_bfs_eager_fallback")
            && !shortest.contains("prepare_direct_identity_visibility")
            && !shortest.contains("resolve_lazy_visibility_batch"),
        "P2 direct identities stay separate after P4.4 moves unweighted paths to the resumable policy oracle"
    );
}
