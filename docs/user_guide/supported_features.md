# Supported Features

This page distinguishes behavior available in pgGraph 1.0 from work targeted
for pgGraph 1.1. A feature marked **Planned for 1.1** is not supported by the
latest stable package until the 1.1 release notes mark it as shipped.

PostgreSQL source tables remain authoritative for every release. Graph writes
use PostgreSQL DML before projection synchronization, and graph reads remain
subject to the documented SQL, security, freshness, and resource contracts.

## Stable 1.0 Features

| Area | Supported behavior |
|---|---|
| PostgreSQL versions | PostgreSQL 14 through 18 |
| Registration | Manual and discovered table, relationship, filter-column, and named-graph registration |
| Search | Source-table search with `contains`, `exact`, `prefix`, and `token` modes |
| Traversal | Bounded BFS and DFS with direction, relationship type, table, tenant, indexed filters, hydration, pagination, and resource limits |
| Paths | Unweighted and weighted shortest paths across all registered relationship types |
| GQL and Cypher | The documented GQL-compatible read/write subset and Cypher compatibility entry point |
| Analytics | Connected components, path counting, and server-side aggregation |
| Synchronization | Manual rebuild, trigger-log synchronization, maintenance, vacuum, and transaction-local overlays |
| Persistence | Versioned, validated graph artifacts with generation manifests and lazy backend loading |
| Security | Table ACL enforcement and source-table PostgreSQL authority; see the current RLS limitation below |
| Playground | Docker-backed Streamlit SQL playground with the Panama dataset fixture |

## Targeted 1.1 Changes

| Capability | Status | Intended contract |
|---|---|---|
| Playground portability and stability fixes | Implemented for 1.1; unreleased | Docker and Podman setup, deterministic Panama data, Python-shim handling, policy-compliant reuse of pre-provisioned virtual environments, cached initialization, scoped statement timeouts, and stable data-frame rendering |
| Cancellation-safe graph replacement | Implemented for 1.1; unreleased | Build, vacuum, foreground/background maintenance, projection compaction/repair, and durable sync ingestion publish only validated generations under one per-graph writer lock. Cancellation before publication retains the previous generation and removes only the recorded unpublished candidate; interruption after publication reconciles the backend to the new generation. Low-memory eviction is rejected unless the serving base, projection files, and relationship-identity sidecar pass recovery validation. |
| Caller-preserving query boundary | Implemented for 1.1; unreleased | Topology query entry points run as `SECURITY INVOKER`, so hydration and other source-table SQL observe the outer application role. Narrow `SECURITY DEFINER` catalog mediators pin `search_path`, capture the outer role, and expose only authorized graph metadata; they do not switch Rust user IDs. This establishes caller identity but does not yet provide the planned topology/RLS intersection. |
| Caller-scoped topology RLS | Planned for 1.1 | Traversal, paths, components, aggregation, GQL, Cypher, and workflows use the projection intersected with caller-visible node and relationship rows |
| Operational telemetry authorization | Implemented for 1.1; unreleased | Selected/named-graph status requires read authorization, artifact and build-resource status requires selected-graph admin authorization, cluster/resource telemetry requires graph-schema administration, and runtime rows are filtered to caller-readable graphs. These are physical totals, not RLS-row-filtered query results. |
| Query-start catalog deduplication | Planned for 1.1 | Resolve the selected graph and registered catalog once per graph query without weakening freshness, drift, ACL, or sync checks |
| Relationship-typed shortest paths | Planned for 1.1 | Backward-compatible shortest-path overloads accept `edge_types text[]`; legacy calls continue to use every registered type |
| Registration troubleshooting | Implemented for 1.1; unreleased | Documentation identifies missing `::regclass` casts for table arguments |

## Current RLS Boundary

pgGraph 1.0 enforces table-level privileges, but topology stored in the graph
projection is not filtered by the querying role's source-table row policies.
Hydration and source-table SQL may apply RLS after topology has already
influenced reachability. `graph.allow_rls_tables` acknowledges this boundary;
it does not provide query-time topology filtering.

Until pgGraph 1.1 ships caller-scoped topology RLS, do not use a shared graph
projection to isolate tenants or roles whose source-row visibility differs.
Use separate graphs or PostgreSQL-native recursive SQL where row policies must
control every traversal hop.

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
