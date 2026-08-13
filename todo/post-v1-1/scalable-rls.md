# Scalable Caller-Scoped RLS

This document owns the detailed post-1.1 RLS performance architecture. The
phase order and status live in [`README.md`](./README.md).

## Problem

pgGraph 1.1 correctly constructs the caller-visible graph before executing a
topology query. For every RLS-active mapping it eagerly hides projected
identities and scans all caller-visible source keys to reveal them. This is an
excellent correctness baseline and an efficient strategy for whole-graph work,
but a shallow targeted query can pay source work proportional to every visible
row in a large table.

The goal is to make selective queries pay for bounded candidate identities,
without evaluating PostgreSQL policies in Rust, post-filtering paths, caching
verdicts across statements, or weakening the eager implementation.

## Current ownership

| Module | Current responsibility | Post-1.1 direction |
|---|---|---|
| `visibility.rs` | Pure `VisibilityCoordinator`, immutable admission scope, execution context, and bounded ordered candidate/verdict types | Retain pure resolved verdicts and make the coordinator the only context factory |
| `sql_visibility.rs` | Sole production constructor for prepared visibility, caller RLS detection, governed source scans, and source-key resolution | Own immutable policy plans, eager resolver, bounded lazy oracle, and PostgreSQL error cleanup |
| SQL facades | Build scope, borrow engine, execute algorithms | Own the coordinator loop that alternates engine candidate production and SPI resolution |
| Core traversal/path/GQL algorithms | Synchronous per-candidate admission | Become resumable only for targeted lazy paths; consume resolved batches in stable order |
| Components/global analytics | Eager graph-wide work | Stay eager until a dedicated measured executor justifies change |

## Required boundary

```text
┌──────────────────────────────────────────────┐
│ borrow ENGINE                               │
│ enumerate bounded ordered candidate records │
└──────────────────────┬───────────────────────┘
                       │ release borrow
                       ▼
┌──────────────────────────────────────────────┐
│ resolve Unknown identities through invoker   │
│ PostgreSQL SPI; cache Visible/Hidden          │
└──────────────────────┬───────────────────────┘
                       │ no SPI state borrowed
                       ▼
┌──────────────────────────────────────────────┐
│ borrow ENGINE                               │
│ admit resolved records in original order     │
│ advance frontier/path/executor state          │
└──────────────────────────────────────────────┘
```

SPI must never be hidden inside `allows_node()` or
`allows_relationship()`. Every PostgreSQL ERROR/cancellation boundary must own
graph-sized Rust state through an explicit backend-local slot or PostgreSQL
memory/resource-owner mechanism and clear it through `PgTryBuilder::finally`.

P1 keeps eager behavior unchanged but seals the composition boundary:
`prepare_eager_visibility()` returns a `VisibilityCoordinator`, SQL facades ask
that coordinator for `QueryExecutionContext`, and raw unrestricted scope
construction is confined to the pure visibility module plus the PostgreSQL
preparation authority. Pure Rust compatibility APIs also route through the
coordinator. Candidate batches own their keys, require strictly increasing
sequence numbers, and reject row/key-byte overflow; verdict batches reject
`Unknown`, count mismatch, and sequence mismatch. No SPI runs inside an
`ENGINE.with` closure.

## Domain state

The exact names are implementation decisions, but the state model is fixed:

```text
VisibilityPlan
  Unrestricted
  Governed { node mappings, relationship mappings, identity completeness }

VisibilityStrategy
  Eager
  LazyTargeted

Verdict
  Unknown
  Visible
  Hidden

Candidate
  Node { table_oid, source_key, node_idx, sequence }
  Relationship { mapping_id, source_key, relationship_id, sequence }
```

`Unknown` is never admissible. A successful batch query transitions every
requested unknown identity to `Visible` or `Hidden`. A PostgreSQL error leaves
the query failed; it does not convert unknown identities to hidden.

The cache is owned by one top-level query invocation and charged to the query
governor. It is never stored in a backend-global cross-statement cache.

## PostgreSQL probe plans

Every plan uses catalog-resolved identifiers and bound values. Candidate count
and encoded bytes are checked before allocation and before constructing SPI
arguments.

Preferred shapes:

- scalar primary key: typed `key = ANY($1::<type>[])`;
- composite primary key: typed `VALUES` or parallel typed `unnest` columns
  joined on the real primary-key columns;
- relationship identity: the registered source-key columns with the same typed
  plan;
- encoded text-key fallback: allowed only when retained `EXPLAIN` evidence
  shows the expected source index is used or no typed adapter is available.

The lazy path must not execute a table-wide `max(octet_length(key))` preflight.
It reserves from actual bounded input bytes and a fixed return cap.

## Reentrancy

An RLS policy expression may invoke user code, including another graph query.
Lazy SPI therefore introduces reentrancy that eager precomputation largely
avoided.

- Release all engine and resolver borrows before SPI.
- Mark only active visibility resolution, not all nested graph execution.
- Reject recursive visibility resolution with one stable diagnostic rather
  than panicking, infinitely recursing, or borrowing the engine twice.
- Bind the guard to the caller/query context and clear it in a PostgreSQL
  `finally` path.
- Add cancellation and ordinary-error injection before, during, and after a
  probe; the next query must find every slot empty.

## Algorithm preservation rules

- BFS admits resolved candidates sequentially so visited order, chosen parent,
  result order, caps, and truncation match the eager oracle.
- DFS preserves reversed-neighbor push order and the current visited timing.
- Bidirectional BFS resolves bounded level chunks without changing meeting-node
  selection.
- Dijkstra initially batches one popped node's adjacency. It may not resolve
  several heap pops speculatively if the first can discover a cheaper path.
- GQL preserves source order, optional null extension, joins, wildcard path
  identity lists, row caps, and write MATCH target selection.
- Physical examined-edge work may be charged before a hidden candidate is
  rejected. Public documentation does not claim timing/resource-exhaustion
  noninterference.

## Eager/lazy strategy

Initial deterministic selection:

| Query shape | Strategy |
|---|---|
| Direct identity and endpoint probes | Lazy targeted |
| One-hop neighbors and bounded BFS/DFS | Lazy targeted |
| Shortest paths and targeted workflows | Lazy targeted |
| Eligible projection-backed GQL expansions | Lazy after differential coverage |
| Connected components and component statistics | Eager |
| Whole-table GQL node scans | Eager |
| All-possible-path and path-count enumeration | Eager |
| No applicable RLS or authorized bypass | Unrestricted |

An adaptive switch is not part of the initial implementation. It may be added
only if representative evidence shows a material win, and it must reuse known
verdicts when completing an eager scope.

## Differential and performance gates

Every migrated surface runs through eager and lazy strategies against the same
role, snapshot, graph generation, overlays, limits, and inputs. Tests compare
exact ordered rows, paths, relationship identities, counts, caps, truncation,
and documented diagnostics.

Required policy fixtures include:

- hidden seed, endpoint, intermediate, and relationship;
- RLS on one side of a two-table relationship;
- permissive plus restrictive policies;
- `current_user`, `current_setting()`, FORCE RLS, BYPASSRLS, and
  `row_security = off`;
- user-created `SECURITY DEFINER` wrapper behavior;
- partitions, scalar keys, composite keys, mutable segments, transaction-local
  additions/deletions, and savepoints;
- missing relationship identity, policy errors, recursion, cancellation, and
  resource exhaustion.

Benchmarks use 1M and 10M source rows and report:

- no-RLS, sparse-allow, sparse-deny, and broad-allow cases;
- visibility preparation/probe and graph execution time separately;
- p50/p95 total latency;
- SPI calls, requested/returned keys and bytes, and retained query plans;
- governed peak/cache/batch bytes and work units;
- shallow/deep traversal, endpoint paths, node and relationship RLS, scalar and
  composite keys; and
- eager, lazy, and PostgreSQL recursive-query comparisons where meaningful.

## Explicit non-goals

- evaluating or simplifying `pg_policy` expressions in Rust;
- transaction- or backend-scoped visibility verdict caching;
- inferring tenant equivalence from policy text fingerprints;
- a public recheck/paranoid GUC;
- one SPI call per candidate or edge;
- changing write-side locking/recheck semantics; and
- promising identical timing or work for hidden and physically absent rows.
