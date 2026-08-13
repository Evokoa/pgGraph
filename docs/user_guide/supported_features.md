# Supported Features

This page summarizes the pgGraph 1.1 release surface and its explicit limits.

PostgreSQL source tables remain authoritative for every release. Graph writes
use PostgreSQL DML before projection synchronization, and graph reads remain
subject to the documented SQL, security, freshness, and resource contracts.

## Stable 1.1 Features

| Area | Supported behavior |
|---|---|
| PostgreSQL versions | PostgreSQL 14 through 18 |
| Registration | Manual and discovered table, relationship, filter-column, and named-graph registration |
| Search | Source-table search with `contains`, `exact`, `prefix`, and `token` modes |
| Traversal | Bounded BFS and DFS with direction, relationship type, table, tenant, indexed filters, hydration, pagination, and resource limits |
| Paths | Unweighted and weighted shortest paths across all registered relationship types, with optional relationship-type restriction through backward-compatible overloads |
| GQL and Cypher | The documented GQL-compatible read/write subset and Cypher compatibility entry point |
| Analytics | Connected components, path counting, and server-side aggregation |
| Synchronization | Manual rebuild, trigger-log synchronization, maintenance, vacuum, and transaction-local overlays |
| Persistence | Versioned, validated graph artifacts with generation manifests and lazy backend loading |
| Security | Table ACL enforcement, caller-scoped source-table RLS for topology, authorized operational telemetry, and PostgreSQL authority |
| Playground | Docker-backed Streamlit SQL playground with the Panama dataset fixture |

## Changes Shipped In 1.1

| Capability | Status | Intended contract |
|---|---|---|
| Playground portability and stability fixes | Implemented in 1.1 | Docker and Podman setup, deterministic Panama data, Python-shim handling, policy-compliant reuse of pre-provisioned virtual environments, cached initialization, scoped statement timeouts, and stable data-frame rendering |
| Cancellation-safe graph replacement | Implemented in 1.1 | Build, vacuum, foreground/background maintenance, projection compaction/repair, and durable sync ingestion publish only validated generations under one per-graph writer lock. Cancellation before publication retains the previous generation and removes only the recorded unpublished candidate; interruption after publication reconciles the backend to the new generation. Low-memory eviction is rejected unless the serving base, projection files, and relationship-identity sidecar pass recovery validation. |
| Caller-preserving query boundary | Implemented in 1.1 | Topology query entry points run as `SECURITY INVOKER`, so direct calls execute hydration and source-table SQL as the application role. A caller-authored `SECURITY DEFINER` wrapper deliberately changes PostgreSQL's effective `current_user` to its owner, as direct SQL in that wrapper does. Narrow pgGraph catalog mediators pin `search_path`, capture the outer role for graph authorization, and do not switch Rust user IDs. |
| Caller-scoped topology RLS | Implemented in 1.1 | Every topology-producing surface intersects projected nodes and relationship identities with caller-visible source rows before admission. This includes direct traversal and path APIs, workflows, components and statistics, aggregation and path estimates, GQL node/identity scans, optional and multi-pattern matches, wildcard paths, Cypher lowering, and the projected MATCH phase of mapped GQL writes. PostgreSQL DML remains the final write authority. |
| Operational telemetry authorization | Implemented in 1.1 | Selected/named-graph status requires read authorization, artifact and build-resource status requires selected-graph admin authorization, cluster/resource telemetry requires graph-schema administration, and runtime rows are filtered to caller-readable graphs. These are physical totals, not RLS-row-filtered query results. |
| Query-start catalog deduplication | Implemented in 1.1 | Each topology query initialization composes one owned state from the selected graph and one registered-catalog read. Fingerprints, schema drift, tenant scope, and applicable sync relations derive from that state; caller/graph-bound sync mediators and automatic replay reuse it without weakening freshness, ACL, or sync checks. |
| Relationship-typed shortest paths | Implemented in 1.1 | Backward-compatible shortest-path overloads accept a required `edge_types text[]`; the unweighted array is the seventh argument so legacy calls, including an untyped fifth-argument `NULL`, remain unambiguous |
| Registration troubleshooting | Implemented in 1.1 | Documentation identifies missing `::regclass` casts for table arguments |

## Current RLS Boundary

The 1.1 topology, GQL, Cypher, workflow, component, and analytics APIs
evaluate applicable node and relationship policies in the effective PostgreSQL
execution context, then exclude
hidden identities before reachability, limits, paths, costs, component unions,
statistics, or aggregate inputs are computed. Hidden seeds and targets behave
as nonexistent, a hidden intermediate blocks visible nodes behind it,
shortest-path selection chooses the best caller-visible route, and component
and aggregate counts describe only caller-visible topology.
`graph.allow_rls_tables` is now a deprecated no-op; builds accept RLS-enabled
source tables by default.

The same rule applies before GQL/Cypher rows, optional null-extension, row caps,
or write targets are selected. PostgreSQL then rechecks ACLs, RLS, constraints,
and triggers when mapped graph writes execute against their source tables.

## Not Targeted for 1.1

The following remain post-1.1 roadmap work:

- bounded set-based graph mutation batches;
- open-vocabulary relationship-type identifiers;
- PostgreSQL 19 and SQL/PGQ integration;
- a new topology trait hierarchy or cross-statement RLS cache; and
- broader GQL syntax outside the documented profile.

See the [Roadmap](/roadmap) for future direction, the
[SQL Profile](/user_guide/sql-profile) and
[GQL Profile](/user_guide/gql-profile) for exact syntax, and
[Known Issues](/known-issues) for current limitations.
