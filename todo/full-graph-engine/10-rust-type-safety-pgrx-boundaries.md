# Rust Type Safety, Unsafe, And pgrx Boundary Plan

## Outcome

pgGraph's Rust core uses domain types for graph identity, state, values,
capacity, and units. Closed vocabularies are enums after one boundary
conversion; raw strings, integers, PostgreSQL datums, and pointers do not flow
through the engine. Safe Rust APIs cannot reach undefined behavior for any
input. PostgreSQL integration uses supported pgrx facilities first, with a
small reviewed raw-FFI allowlist only where pgrx has no suitable safe API.

This is a correctness plan, not cosmetic cleanup. Exact value semantics,
stable relation identity, safe mapped storage, transaction behavior, and
security-definer hardening are prerequisites for full GQL and PostgreSQL 19
property-graph support.

## Review Baseline

The 2026-07-09 review used the Rust reviewing checklist across production
types, GUCs, catalog rows, GQL values, persistent segments, mmap access, unsafe
blocks, PostgreSQL FFI, background workers, and SQL facades.

Inventory at review time:

- 54 explicit unsafe blocks: 31 in production paths and 23 in test fixtures;
- 7 unsafe declarations: four mapped-store constructors, two PostgreSQL
  callbacks, and one direct PostgreSQL error-reporting extern block;
- 44 security-definer SQL facades and no explicit pgrx `#[search_path]`;
- 12 `oid::integer` and 41 `graph_id::text` source occurrences;
- 178 opt-in clippy possible-truncation, sign-loss, or possible-wrap warnings;
- no repository-owned `unsafe impl`, manual `Send`/`Sync`, `transmute`, or
  `MaybeUninit`.

Current positive foundations to preserve:

- SPI parameters, `connect_mut`, PostgreSQL quoting helpers, `JsonB`, temporal
  datums, `#[pg_guard]`, and `BackgroundWorkerBuilder` already use pgrx;
- `unsafe_op_in_unsafe_fn` and undocumented-unsafe-block lints are denied;
- core control flow already has useful enums such as `SyncStatus`,
  `ReadOnlyReason`, `SchemaState`, and `MutationKind`.

The actual raw PostgreSQL call/global surface is small enough to own:
`pg_class_aclcheck`, permanent xact/subxact callback registration, `MyProcPid`,
`DataDir`, and the private `errstart`/`errcode`/`errmsg`/`errdetail`/`errhint`/
`errfinish` family. Typed `pg_sys` ABI values such as `Oid`, `Datum`, callback
events, and error-report structs are not unsafe access by themselves.

The current warnings-denied clippy gate passes. That does not prove soundness:
clippy cannot detect the safe mmap access paths below.

## Release-Blocking Findings

| ID | Finding | Evidence | Required result |
|---|---|---|---|
| RUST-00A | Safe node lookup can dereference an mmap pointer out of bounds. | `graph/src/node_store.rs:315-322` | A local bounds proof dominates every dereference; invalid indexes return a typed error/`None`. |
| RUST-00B | The mmap edge constructor does not require validated CSR value invariants, while safe accessors reuse unchecked offsets. | `graph/src/edge_store.rs:68-141`, `graph/src/edge_store.rs:522-597` | Only an unforgeable validated layout can construct mapped stores. |
| RUST-00C | Custom SQLSTATE emission calls PostgreSQL `errfinish()` below the pgrx guard, which can longjmp across live Rust frames. | `graph/src/safety.rs:238-280` | Error reporting unwinds Rust through the supported pgrx boundary before PostgreSQL ERROR. |
| RUST-00D | Durable filter deltas narrow typed values to `Option<u32>`; signed, temporal, large, and UUID updates can be changed or dropped. | `graph/src/projection/ingest.rs:32-48`, `graph/src/sql_sync.rs:1383-1391`, `graph/src/projection/segment.rs:116-126` | A versioned tagged codec preserves every registered filter value across sync, segment, and reload. |
| RUST-00E | Security-definer SQL functions do not declare a hardened search path. | `graph/src/sql_facade/` (`security_definer` entrypoints) | Every definer function has a minimal explicit pgrx `#[search_path(...)]` and a catalog/attack regression. |
| RUST-00F | Registered relation identity is stored and later resolved through names/search path. | `graph/sql/bootstrap.sql:75-91`, `graph/src/catalog/validate.rs:132-190`, `graph/src/builder.rs:341` | Catalog identity is OID/regclass based; names are display data only. |

Do not expand the unsafe surface or make production-readiness claims until
RUST-00A through RUST-00F have failing regressions, fixes, and release-matrix
evidence.

## Non-Negotiable Type And Safety Invariants

1. A public or crate-visible safe function is sound for every value its type
   permits. Comments about expected callers do not repair an unsafe safe API.
2. A closed vocabulary is one canonical Rust enum. Strings exist only at SQL,
   JSON, config-file, log, or artifact compatibility boundaries.
3. An identifier or quantity with different semantics has a distinct newtype.
   Raw primitives exist only in ABI and serialized representations.
4. All narrowing, sign-changing, count, offset, and byte arithmetic is checked.
   Representation ceilings fail before allocation or persistence.
5. PostgreSQL OIDs enter through pgrx's `pg_sys::Oid`, UUIDs through pgrx
   `Uuid`, and PostgreSQL values through their native datum types. Text
   roundtrips are not an internal transport.
6. Exact GQL/PostgreSQL numeric and temporal semantics never pass through JSON
   or `f64` unless the source type is explicitly floating point.
7. Raw `pg_sys` calls are denied by default and confined to named adapter
   functions with an owner, safety contract, version matrix, and tests.
8. pgrx memory contexts are used only for PostgreSQL-owned lifetimes. They do
   not replace accounting for Rust heap, mmap, PostgreSQL executor memory, or
   spill.
9. PostgreSQL source tables remain authoritative. Stronger Rust typing must not
   bypass constraints, triggers, ACLs, RLS, snapshots, or transaction rules.

## Target Boundary

```text
SQL / GUC / catalog / background-worker entrypoints
                         |
                         v
postgres/{relation,guc,error,acl,callbacks,bgworker_tx,datum}
                         |
        one checked conversion into domain types
                         |
                         v
graph identity + GraphValue + typed IR + runtime + storage ports
                         |
                         v
persistence/{format,validate,mapped_artifact,reader,writer}
                         |
        validated serialized primitives only
```

Core graph modules do not accept `Datum`, `pg_sys::Oid`, enum-like `String`, or
unvalidated offsets. The PostgreSQL adapter may use pgrx; a future pure core
crate is considered only after this boundary is real.

## Canonical Type Model

| Concept | Canonical Rust representation | Boundary rule |
|---|---|---|
| Graph identity | `GraphId(pgrx::Uuid)` initially; distinct `BuildId`, `JobId`, `PolicyId`, `RunId` | Bind/read UUID directly; format only for text APIs and safe path components. |
| Relation identity | `TableOid`/`RelationId` constructed from pgrx `pg_sys::Oid` | Ban `oid::integer`, signed casts, and internal name re-resolution. |
| Graph indexes | `NodeIdx`, `EdgeTypeId`, `FilterColumnId`, `RelationshipId` | Production types with private fields and checked constructors. Model untyped/reserved cases as enum variants, not magic bytes. |
| Planner indexes | `NodeSlotId`, `RelationshipSlotId`, `PathSlotId`, `PatternId`, `ColumnIndex` | Typed arenas/indexed vectors prevent cross-family indexing. |
| Generations/order | `GenerationId`, `SyncId`, `SourceWatermark`, `SegmentId` | Checked ordering and no cross-domain integer comparison. |
| Counts/units | `NodeCount`, `EdgeCount`, `ByteCount`, `MemoryBudget`, `DiskBudget`, `WorkUnits` | Integer-byte arithmetic; MiB/JSON conversion is display only. |
| Edge weights | `EdgeWeight` with an explicit exact/integral domain | Represent or reject negative, fractional, zero, NULL, and overflow explicitly under one documented contract; never clamp. |
| Engine access | `EngineAccess::{Writable, ReadOnly(ReadOnlyReason)}` | Contradictory boolean/reason/status combinations cannot be represented. |
| Errors | Typed reason/resource/capacity enums inside `GraphError` | Convert to message/detail/hint and SQLSTATE only at `postgres/error`. |
| GQL values | `GraphValue` algebra | JSON is an output/input facade, not the execution value system. |

`GraphValue` must cover null/missing semantics, boolean, signed and unsigned
integers, exact numeric, explicit floating point, string, byte string, date,
time, timestamp, timestamptz, interval, UUID, list, record/map, node,
relationship, and path values. Use pgrx native datums such as `AnyNumeric`,
`Date`, timestamps, intervals, bytea, and `Uuid`, or delegate coercion and
comparison to PostgreSQL where that is the authoritative semantic boundary.

## pgrx-First Integration Matrix

| Boundary | Required default | Raw exception policy |
|---|---|---|
| Closed GUCs | `PostgresGucEnum`, typed `GucSetting<T>`, `define_enum_guc` | None. Paths and tenant-setting names remain string GUCs. |
| SQL-facing enum types | Parse once into canonical Rust enums; evaluate `PostgresEnum` for private catalogs after upgrade testing | Retain text SQL signatures/checks temporarily where changing them would break upgrades. Unknown/NULL catalog values fail closed. |
| UUID/OID | pgrx `Uuid`, `pg_sys::Oid`, and `PgRelation` where locking/lifetime fits | Convert once into domain wrappers; never roundtrip through text/signed integer. |
| SQL execution | pgrx SPI with bound parameters and `connect_mut` for writes | Raw executor/catalog calls require a measured, documented need. |
| Security-definer path | pgrx `#[search_path(...)]` | No implicit caller path. Dynamic object names still require OID identity and safe rendering. |
| SQL arrays/results | Lazy pgrx `Array<'_, T>` and genuinely incremental `TableIterator`/batches | Collection requires a budget lease and explicit maximum. |
| Transactions | pgrx transaction helpers with explicit success/error semantics | Permanent xact/subxact callback registration may remain raw because pgrx's safe callbacks are per transaction; callbacks must be panic-free. |
| ACL checks | One adapter, preferring a supported pgrx/SPI privilege path when semantics and cost are equivalent | `pg_class_aclcheck` may remain only if the adapter tests prove no safe equivalent preserves required behavior. |
| Background workers | `BackgroundWorkerBuilder` and one transaction policy adapter | `MyProcPid` is an accepted isolated raw global required by pgrx's worker API. |
| Data directory | Supported pgrx/SPI setting access when legal in the current backend phase | Raw `DataDir` may remain in one adapter where SPI is unavailable, with null/lifetime/version tests. |
| Error reporting | pgrx `ErrorReport`/guard boundary and built-in SQLSTATEs | Never transmute a custom code into `PgSqlErrorCode`. If custom wire SQLSTATEs remain contractual, obtain/upstream a safe pgrx arbitrary-SQLSTATE API before release. |
| Memory | Rust budget governor plus bounded batches/spill; pgrx memory contexts for PostgreSQL-owned transient values | Memory contexts are not treated as a cap on Rust heap or mapped pages. |

## Work Packages

### RUST-0: Close Soundness And Security Blockers

1. Add an out-of-range mmap node regression, then make access take a validated
   `NodeIdx` or return `Option`/`GraphResult`. Make owned and mapped behavior
   identical.
2. Replace public raw mmap parts with a private parser that returns
   `ValidatedGraphLayout`. Validate section ranges, alignment, monotonic CSR
   offsets, terminal offsets, target bounds, section lengths, UTF-8, integer
   endianness, and overflow before a store exists.
3. Make `MappedGraphArtifact` own the `Mmap` plus typed validated ranges. Store
   offsets/ranges rather than free pointers; no store may outlive its owner.
4. Replace direct deep `errfinish()` use. Prefer standard SQLSTATE plus a typed
   `GraphDiagnosticCode` in detail/metadata. If preserving `PGxxx` wire codes
   is mandatory, block release until pgrx exposes a sound validated-code path.
5. Add a minimal explicit search path to every security-definer entrypoint and
   test shadow schemas/functions/operators plus `pg_proc.proconfig` drift.
6. Store relation OIDs/regclass identity at registration. Test table/schema
   rename, search-path changes, drop/recreate, concurrent DDL, and stale OIDs.
7. Replace `Option<u32>` durable filter payloads with a tagged
   `PersistedFilterValue`; migrate/rebuild segments and differential-test every
   supported type across restart.

**Exit:** KI-020 through KI-024 are closed, no safe accessor can reach
unchecked pointer arithmetic, error reporting respects the pgrx guard, and the
security/reload matrix is green on supported PostgreSQL versions.

### RUST-1: Fix Transaction And Adapter Semantics

1. Create `postgres/bgworker_tx.rs`. A Rust `GraphResult::Err` must not
   accidentally return normally from a pgrx transaction closure and commit
   partial job state. Define atomic units versus intentionally committed
   progress checkpoints.
2. Put permanent transaction callbacks in one adapter. Replace panicking
   `RefCell::borrow_mut()` paths with non-panicking state transitions; callbacks
   do not allocate, use SPI, or report PostgreSQL errors.
3. Specify and test commit, abort, savepoint, PL exception subtransactions,
   parallel events, and `PREPARE TRANSACTION` behavior.
4. Consolidate ACL, `MyProcPid`, and early `DataDir` access in an explicit raw
   FFI allowlist. Remove replaceable raw access only after semantic tests.

**Exit:** injected worker failures cannot partially commit unintentionally or
leave jobs stuck; callback paths are panic-free and version-tested.

### RUST-2: Replace Stringly Closed State

1. Define one canonical enum each for graph kind, residency, materialization,
   projection mode, graph privilege, quota scope/dimension/enforcement, job
   status/phase, validation status, search mode, sync mode, query freshness,
   OOM action, and build scan mode.
2. Convert search, sync, freshness, OOM, scan, and projection GUCs to
   `PostgresGucEnum`. Remove the obsolete pgrx 0.18 string-GUC comment and all
   invalid-value fallback parsing.
3. Make catalog/SPI adapters use `TryFrom` once. Unknown, NULL, or corrupt
   closed state is a typed error, never `"unknown"`, `"not_found"`, or a
   default mode.
4. Keep human diagnostic text and genuinely open names/paths as strings.
5. Add one drift test comparing Rust spellings, SQL CHECK/enum vocabulary,
   configuration docs, and compatibility aliases.

**Exit:** no enum-like `CString` GUC or internal enum-like `String` field
remains, and invalid values fail at `SET` or the catalog boundary.

### RUST-3: Production Identity, Capacity, And Unit Types

1. Remove the test/development cfg from `NodeIdx` and `EdgeTypeId`; make fields
   private and add checked constructors/conversions.
2. Introduce the identity, planner-index, generation, count, budget, and work
   types in the canonical model above. Replace tuple-shaped overlay rows with
   named structs and one shared overlay contract.
3. Decide artifact capacity explicitly: either bounded `u32` local indexes or
   segmented/64-bit global IDs. Do not widen silently.
4. Replace `as` casts in counts, offsets, status, and persistence with checked
   conversions. Return SQL `bigint` for counts that can exceed `i32`.
5. Use pgrx `pg_sys::Oid` at every SPI/catalog boundary and ban
   `oid::integer`.
6. Enable sign-loss, possible-wrap, and possible-truncation clippy lints as
   deny-by-default after the inventory is burned down.

**Exit:** compile-fail tests prevent cross-domain ID/slot use; boundary tests
at `i32::MAX`/`u32::MAX` do not wrap; raw IDs remain only in ABI/format code.

### RUST-4: Exact GQL And Property Value Algebra

1. Replace GQL decimal `f64` and internal `serde_json::Value` transport with
   `GraphValue` and typed binding rows.
2. Preserve exact numeric scale/precision, temporal types, UUID, bytea,
   null/missing, and overflow behavior. No failed `from_f64` conversion may
   silently produce JSON null.
3. Convert JSONB only at explicitly JSON-facing SQL functions. Prefer typed
   SQL rows for future standard GQL/SQL-PGQ surfaces.
4. Replace `GraphId(String)` and internal `graph_id::text` queries with native
   UUID binding. Add distinct ID types for jobs, builds, policies, and runs.
5. Define `EdgeWeight`; reject unsupported values during registration/build/
   sync instead of casting to bigint and clamping.

**Exit:** differential tests match PostgreSQL for large integers, high-scale
numeric, SUM/AVG, mixed numeric comparisons, date/time/timestamps/intervals,
UUID, bytea, null/unknown, overflow, and non-finite cases.

### RUST-5: Typed Persistent Formats

1. Make format structs use checked serialized primitives and convert to domain
   types only after validation.
2. Version the filter-value codec and any index-width change. Document old
   reader, rebuild, rollback, and mixed-generation behavior.
3. Fuzz every section offset/count, malformed CSR, PK UTF-8, filter tag/value,
   segment, manifest, and compatibility branch.
4. Define the endian policy: decoded fallback or explicit unsupported-target
   rejection. Native typed loads must not reinterpret little-endian bytes on a
   big-endian host.

**Exit:** corrupt artifacts cannot construct mapped stores; old/new artifacts
have deterministic accept/rebuild/reject behavior with no partial publish.

### RUST-6: Unsafe Isolation And Proof

1. Set `unsafe_code = "deny"` at crate policy and permit it only in named mmap
   and PostgreSQL adapter modules. CI rejects a new unsafe file/function.
2. Keep `unsafe_op_in_unsafe_fn = "deny"` and
   `clippy::undocumented_unsafe_blocks = "deny"`; require `# Safety` docs on
   every unsafe function and a local invariant comment on every block.
3. Maintain an inventory recording owner, caller proof, lifetime/provenance,
   version support, and tests for each unsafe site.
4. Replace test fixtures that pretend unrelated stack allocations are one
   mapped region with one aligned owned backing allocation.
5. Remove ABI-incompatible fuzz stubs by extracting pure validation code from
   pgrx linkage; typed generated stubs are interim only.

**Exit:** the allowlist is minimal and stable, every unsafe precondition is
encoded or checked at the closest boundary, and no raw pointer escapes it.

### RUST-7: Streaming pgrx SQL Boundaries

1. Accept lazy `Array<'_, T>` for large SQL arrays, validate NULL/length, and
   collect only under an explicit budget.
2. Replace `TableIterator` wrappers over complete `Vec` results with resumable
   or bounded-batch execution. PostgreSQL `LIMIT`, iterator drop,
   `statement_timeout`, and cancellation stop graph work and release leases.
3. Keep SPI values and SQL rows typed until the outer facade. Delay JSON and
   display-string conversion.
4. Do not move ordinary graph allocations into PostgreSQL memory contexts;
   integrate pgrx contexts only where PostgreSQL must own a datum lifetime.

**Exit:** large-array and `LIMIT 1` tests stay within peak-RSS/work gates and
cancellation does not leave frontier/result memory live.

### RUST-8: Verification And Continuous Enforcement

Required gates:

- unit/property tests for all enum parsers, IDs, checked arithmetic, slot
  preservation, state machines, and `GraphValue` coercions;
- compile-fail tests for mixed identity/slot types;
- pgrx tests for enum GUCs, native UUID/OID roundtrips, search path, relation
  rename/drop/recreate, worker transactions, SQLSTATEs, and callbacks;
- an in-memory aligned mapped backing for Miri, because OS file mappings are
  not Miri-supported;
- ASan/LSan or Valgrind against a PostgreSQL process loading the extension,
  covering load/traversal/reload/replacement/corruption/callback/worker paths;
- scheduled/release fuzzing for validated layouts and typed decoders without
  ABI-incompatible PostgreSQL stubs;
- PG14 through PG19 matrix coverage where the locked pgrx version exposes the
  feature;
- CI source guards for unsafe allowlist drift, `oid::integer`, internal
  `graph_id::text`, enum-like `CString` GUCs, unchecked narrowing casts, and
  `Result<_, String>` in production modules.

Every fix starts with a failing regression and closes only with its exact
command, PostgreSQL versions, compatibility impact, and artifact/SQL migration
evidence recorded in `progress.md`.

## Build-Order Integration

1. Run RUST-0 in checkpoint 0 alongside the existing correctness/security
   reproductions. These are blockers, not checkpoint-4 cleanup.
2. Run RUST-1, budget/unit types from RUST-3, and persistent types from RUST-5
   while implementing checkpoints 1A through 3. Do not create an untyped new
   artifact and plan to repair it later.
3. Complete RUST-2, the remaining RUST-3 work, and RUST-6 during checkpoint 4
   before canonical IR is declared stable.
4. Implement RUST-4 as the first foundation of checkpoint 6 full-GQL slices.
5. Apply RUST-7 vertically as each streaming operator/facade migrates.
6. PostgreSQL 19 adapters reuse the same types and pgrx boundary; they do not
   introduce a parallel stringly catalog model.
7. RUST-8 is continuous and becomes release evidence, not a final cleanup
   phase.

## Overall Definition Of Done

- All release-blocking findings are closed with supported-version regressions.
- Every closed state in core/runtime/storage is a canonical enum.
- Every graph identity, planner slot, count, offset, byte quantity, generation,
  and watermark uses the correct domain type.
- No safe API can trigger UB for a value its signature accepts.
- Unsafe code exists only in the reviewed allowlist and passes its Miri,
  sanitizer, fuzz, and PostgreSQL matrix gates.
- pgrx-native UUID, OID, enum GUC, search-path, SPI, datum, guard, and worker
  features are used where they fit; raw PostgreSQL access has explicit proof
  that no suitable pgrx API exists.
- Full GQL execution uses exact typed values rather than JSON/`f64` as its
  semantic core.
- Public SQL and artifact changes have upgrade, rebuild, rollback, and release
  documentation; PostgreSQL source-of-truth behavior is unchanged.
