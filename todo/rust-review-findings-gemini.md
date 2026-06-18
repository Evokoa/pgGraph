# Rust Review Findings — Gemini Work (Uncommitted Changes)

Date: 2026-06-18

## Scope
- Reviewed changed files from unstaged/uncommitted Gemini changes in:
  - `docs/...`
  - `graph/src/...`
  - `graph/tests/heavy/...`
  - `graph/Cargo.toml`, `graph/Cargo.lock`
  - `scripts/inspect_pggraph_artifact.py`

## Severity summary
- Blocking: 3
- Request-change: 5
- Suggest: 1

## Blocking findings

1. **[block] Compile failure in `graph/src/catalog/graphs.rs:588`**
   - Code: `rows.first().map(quota_from_row).transpose()?`
   - Problem: `Map<SpiTupleTable, _>` does not expose `.transpose()`.
   - Impact: `cargo check` fails (`E0599`), preventing all subsequent validation.
   - Fix approach: parse first row manually:
     - `if let Some(row) = rows.first() { ... }` then `quota_from_row(row)?` wrapped to `GraphResult`.

2. **[block] Heavy gate script uses invalid SQL API in `graph/tests/heavy/named_graphs_heavy_gate.sh`**
   - Invalid calls:
     - `graph.enable_sync('heavy_named', namespace := 'app')` (`enable_sync` is no-arg)
     - `graph.run_job(NULL)` (`run_job(job_id text)` requires text)
     - `graph.graph_storage_usage()` (function does not exist)
   - Impact: gate cannot execute as written.
   - Fix approach: rewrite to supported API calls or remove invalid assertions.

3. **[block] Release gate now enables the broken named-graphs gate by default**
   - `graph/tests/heavy/run_release_gate.sh` sets `RUN_NAMED_GRAPHS_HEAVY=1` by default and then runs `named_graphs_heavy_gate.sh`.
   - Because the named-graphs gate currently calls invalid SQL APIs, the default release gate is broken by the new in-progress script.
   - Fix approach: fix the gate before enabling it by default, or default it off until it is valid and verified.

## Request-change findings

4. **[request-change] Public docs examples wrong in `docs/user_guide/administration-and-security.mdx:214-277`**
   - `add_table/add_edge` examples use non-existent `graph_name` arg.
   - `graph.build('customer_360', concurrently := true)` is not a valid overload; named-graph build is `graph.build_graph(...)`, while session-selected build is `graph.build(concurrently := ...)`.
   - `add_sync_policy(... mode := 'delta')` uses unsupported arg.
   - `set_graph_quota('owner', 'max_named_graphs', 25, ...)` missing required `scope_key` for owner.
   - `memory_profile` and `search` examples use unsupported signatures.
   - `graph_storage_usage` example references non-existent function.
   - Impact: end-user copy-paste breakage in public docs.

5. **[request-change] Architecture/upgrade docs diverge from implementation**
   - `docs/contributor_guide/architecture.mdx:144,148` claim artifact path with namespace/name path and `graph.reset('graph_name')`.
   - `docs/user_guide/installation.mdx:391-408` repeats unsupported assumptions:
     - migration path mismatch (`graph` vs `pggraph`, namespace/name layout),
     - `graph.reset('default')`.
   - Impact: operators may follow incorrect runbooks.

6. **[request-change] Script path resolution is not validated in `scripts/inspect_pggraph_artifact.py:90-104`**
   - `--graph-id` path is built directly under `PGDATA/graph/<id>/main.pggraph`, but runtime graph artifacts are rooted under the configurable `graph.data_dir`, not always `graph`.
   - `id` is not validated as graph UUID-like; this can produce opaque/fragile behavior with bad IDs or traversal-like inputs.
   - Fix approach: validate graph id format (expected UUID) before path join.

7. **[request-change] Unexplained lockfile churn in `graph/Cargo.lock`**
   - `postgres-protocol`, `postgres-types`, and `tokio-postgres` were bumped without a matching `Cargo.toml` dependency change or package-management intent.
   - This looks like incidental resolver churn from a local command, not a deliberate dependency update.
   - Fix approach: revert the lockfile churn unless there is a documented dependency-update reason and verification.

8. **[request-change] Public operational failure matrix references unstable/nonexistent status shapes**
   - `docs/user_guide/troubleshooting.mdx` adds rows such as `graph.status().sync_lag_bytes`, `quota_exceeded`, and `disk_full` that do not match the current status API/status vocabulary.
   - Impact: public troubleshooting docs describe signals operators cannot query as written.
   - Fix approach: rewrite the matrix against actual `graph.status()`, `graph.sync_health()`, `graph.projection_status()`, `graph.jobs()`, and `graph.graph_quota_usage()` columns.

## Suggest

9. **[suggest] Reduce lint allow scope**
   - `graph/src/lib.rs:15` and `graph/src/sql_facade/runtime.rs:1` add global `#![allow(clippy::type_complexity)]`.
   - Prefer local per-function allow where unavoidable.

## Additional context
- Current compile check status during review:
  - `cargo check --features "pg17 development"` in `graph/` fails at item 1 above.
- Earlier review note about `runtime.rs` `mark_loaded_graph(graph)` being a compile blocker was removed after re-checking the signature and scope; `graph` is already a `&GraphMetadata` there.
- `Cargo.lock` also changed due dependency-resolution updates.
- Heavy gate file is currently present as untracked:
  - `graph/tests/heavy/named_graphs_heavy_gate.sh`
