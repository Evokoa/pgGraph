# Phase 1 Design: Read-Only GQL

> Reminder: delete this tracking file before merging `feat/mutable-graph-projections` into `main`.

This is the concrete build design for Phase 1. It turns the strategy in
`architecture-plan.md` and `mutable-graph-projections-todo.md` into the actual
grammar, types, and PR slices an implementer codes from. Phase 1 runs entirely
on the existing **immutable CSR** path — no overlay, no writes, no MVCC.

Scope is the `phase_1` rows of the compatibility matrix only:
`MATCH` (single pattern), node labels → registered tables, relationship types →
registered edge labels, directed + undirected relationships, `WHERE` property
predicates, JSONB parameters, `RETURN` node/property/relationship/path,
`ORDER BY`/`SKIP`/`LIMIT`, bounded variable-length relationships.

## 1. Module layout (Phase 1 only)

```text
graph/src/gql/
  mod.rs        # entry: parse() / bind() / plan() orchestration, pgrx-free
  lexer.rs      # bytes -> Vec<Token> with spans; total
  ast.rs        # AST node types with spans
  parser.rs     # recursive descent + Pratt WHERE parser
  semantics.rs  # AST + CatalogSnapshot -> logical IR (binding)
  lower.rs      # logical IR -> physical plan
  errors.rs     # GqlError categories (pgrx-free)

graph/src/query/
  mod.rs
  catalog_snapshot.rs   # CatalogSnapshot trait + concrete impl + fake (test)
  logical_plan.rs       # logical IR types
  physical_plan.rs      # physical operator types
  execute.rs            # execution over existing engine primitives
  value.rs              # GqlValue + JSONB encoding
  explain.rs            # gql_explain() stage rendering

graph/src/sql_facade/gql.rs   # #[pg_extern] graph.gql / graph.gql_explain
```

`gql/` and the non-execute parts of `query/` are **pgrx-free** so they unit-test
without a PostgreSQL backend. Only `execute.rs`, `catalog_snapshot.rs`'s builder,
and `sql_facade/gql.rs` touch SPI/pgrx.

## 2. Grammar (Phase 1 subset, EBNF)

```ebnf
query        = match_clause , [ where_clause ] , return_clause ,
               [ order_by ] , [ skip ] , [ limit ] , EOF ;

match_clause = "MATCH" , pattern ;
pattern      = node_pat , { rel_pat , node_pat } ;     (* single linear path *)

node_pat     = "(" , [ var ] , [ ":" , label ] , [ prop_map ] , ")" ;
rel_pat      = dash , [ rel_detail ] , dash_arrow
             | arrow_dash , [ rel_detail ] , dash ;     (* see direction *)
rel_detail   = "[" , [ var ] , [ ":" , rel_type ] , [ var_len ] , [ prop_map ] , "]" ;
var_len      = "*" , [ int ] , [ ".." , int ] ;         (* bounded; max required *)

dash         = "-" ;
dash_arrow   = "->" | "-" ;                              (* "->" outbound, "-" undirected *)
arrow_dash   = "<-" ;                                    (* inbound *)

prop_map     = "{" , [ prop_entry , { "," , prop_entry } ] , "}" ;
prop_entry   = property , ":" , literal_or_param ;

where_clause = "WHERE" , expr ;
expr         = or_expr ;                                 (* Pratt-parsed *)
or_expr      = and_expr , { "OR" , and_expr } ;
and_expr     = not_expr , { "AND" , not_expr } ;
not_expr     = [ "NOT" ] , comparison ;
comparison   = operand , ( "=" | "<>" | "<" | "<=" | ">" | ">="
                         | "IN" | "IS NULL" | "IS NOT NULL" ) , [ operand ] ;
operand      = property_ref | literal | param | list ;
property_ref = var , "." , property ;

return_clause= "RETURN" , [ "DISTINCT"(*phase_3*) ] , return_item ,
               { "," , return_item } ;
return_item  = ( property_ref | var | path_var | func_call ) , [ "AS" , alias ] ;

order_by     = "ORDER" , "BY" , sort_item , { "," , sort_item } ;
sort_item    = ( property_ref | alias ) , [ "ASC" | "DESC" ] ;
skip         = "SKIP" , int ;
limit        = "LIMIT" , int ;

param        = "$" , ident ;
literal      = string | int | float | bool | "NULL" ;
list         = "[" , [ literal , { "," , literal } ] , "]" ;
```

Phase-1 grammar constraints (enforced at parse or bind time with typed errors):

- Exactly one `MATCH` with one linear pattern (no comma-separated patterns, no
  `OPTIONAL MATCH`, no `WITH` — those are phase_3).
- `func_call` in `RETURN` is limited to `count`, `nodes`, `relationships`,
  `length` (the last three only over a declared `path_var`); aggregates beyond
  `count` are phase_3.
- Variable-length (`var_len`) **must** carry an explicit upper bound; unbounded
  `*` is a syntax-level rejection (safety limit).

## 3. Token set (lexer)

```rust
pub enum TokKind {
    // punctuation
    LParen, RParen, LBracket, RBracket, LBrace, RBrace,
    Colon, Comma, Dot, Dollar, Star, DotDot,
    Dash, ArrowRight, ArrowLeft,                 // - -> <-
    Eq, Neq, Lt, Lte, Gt, Gte,
    // literals
    Ident, String, Int, Float,
    // keywords (case-insensitive match, stored canonical)
    Match, Where, Return, Distinct, OrderBy, Asc, Desc,
    Skip, Limit, And, Or, Not, In, Is, Null, True, False, As,
    Eof,
}

pub struct Token { pub kind: TokKind, pub span: Span, pub text: SmolStr }
pub struct Span { pub start: u32, pub end: u32 }   // byte offsets into query text
```

The parser boundary is PostgreSQL `text`, so the Rust frontend accepts UTF-8
query text. For any UTF-8 query text, the lexer is total: it yields either a
token stream ending in `Eof` or a `GqlError::Syntax { span, .. }`. Hard limits
(max query length, max token count) are checked here and in the parser. Byte
fuzz targets should skip invalid UTF-8 or convert it to an explicit syntax-error
case at the wrapper boundary.

## 4. AST (`gql/ast.rs`)

```rust
pub struct Query {
    pub match_: MatchClause,
    pub where_: Option<Expr>,
    pub return_: ReturnClause,
    pub order_by: Vec<SortItem>,
    pub skip: Option<u64>,
    pub limit: Option<u64>,
    pub span: Span,
}

pub struct MatchClause { pub pattern: Pattern, pub span: Span }
pub struct Pattern { pub start: NodePat, pub tail: Vec<(RelPat, NodePat)>, pub span: Span }

pub struct NodePat {
    pub var: Option<Ident>,
    pub label: Option<Ident>,
    pub props: Vec<(Ident, Operand)>,   // inline {k: v}
    pub span: Span,
}

pub struct RelPat {
    pub var: Option<Ident>,
    pub rel_type: Option<Ident>,
    pub direction: Direction,           // Out | In | Undirected
    pub var_len: Option<VarLen>,        // Some => variable-length
    pub props: Vec<(Ident, Operand)>,
    pub span: Span,
}
pub struct VarLen { pub min: u32, pub max: u32, pub span: Span }   // max always present
pub enum Direction { Out, In, Undirected }

pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Compare { lhs: Operand, op: CmpOp, rhs: Option<Operand>, span: Span },
}
pub enum CmpOp { Eq, Neq, Lt, Lte, Gt, Gte, In, IsNull, IsNotNull }

pub enum Operand {
    Property { var: Ident, property: Ident, span: Span },
    Literal(Literal),
    Param { name: Ident, span: Span },
    List(Vec<Literal>),
}
pub struct Literal { pub value: LiteralValue, pub span: Span }
pub enum LiteralValue { Str(String), Int(i64), Float(f64), Bool(bool), Null }

pub struct ReturnClause { pub distinct: bool, pub items: Vec<ReturnItem> }
pub struct ReturnItem { pub expr: ReturnExpr, pub alias: Option<Ident>, pub span: Span }
pub enum ReturnExpr {
    Var(Ident),                          // whole node or relationship
    Property { var: Ident, property: Ident },
    Func { name: Ident, args: Vec<Ident> },  // count / nodes / relationships / length
}
pub struct SortItem { pub key: SortKey, pub desc: bool }
pub enum SortKey { Property { var: Ident, property: Ident }, Alias(Ident) }

pub struct Ident { pub text: SmolStr, pub span: Span }
```

Every node carries a `Span` for diagnostics. The AST holds **no** resolved
catalog data — that is added during binding.

## 5. Catalog snapshot (`query/catalog_snapshot.rs`)

Trait shape is in `architecture-plan.md` → "Catalog Snapshot Interface". The
concrete impl is built once per query:

S0 confirmation:

- The existing `crate::catalog::read_catalog` re-export is reachable from future
  `graph/src/query/` modules even though the underlying `catalog::read` module
  stays private.
- Table-name to OID resolution should use the existing
  `catalog::table_oid_from_name()` path, which resolves registered catalog text
  through `to_regclass($1)::oid::integer`; `sql_table_name_from_catalog()` then
  round-trips the OID back to SQL-safe `regclass` text for generated SPI SQL.
- Existing hydration already emits graph coordinates without row JSON whenever
  `hydrate=false` by skipping `hydrate_nodes()` and returning `node = NULL` with
  table OID, primary key, depth, path, and table-name fields. GQL execution can
  reuse that coordinate contract for unhydrated returns. Full row hydration
  still uses `to_jsonb(src.*)` and should remain isolated to the SQL hydration
  boundary.

```rust
pub struct CatalogSnapshotImpl {
    tables: HashMap<String, NodeLabelInfo>,   // by table_name (label)
    edges:  HashMap<String, RelTypeInfo>,     // by edge label
    fingerprint: u64,
}

impl CatalogSnapshotImpl {
    /// pgrx/SPI boundary: read once via the existing catalog read path.
    pub fn load() -> GraphResult<Self> {
        let (tables, edges, filter_columns) = crate::catalog::read_catalog()?;
        let fingerprint = crate::catalog::catalog_fingerprint(&tables, &edges, &filter_columns);
        // resolve table_name -> TableOid via existing OID lookup;
        // fold filter_columns into per-label PropertyInfo with filter_indexed = true.
        // ...
    }
}
```

`load()` is the only SPI call; everything downstream consumes the immutable
struct through the `CatalogSnapshot` trait. The binder test suite uses a
`FakeCatalog` builder (no pgrx) so binding/lowering is unit-tested fast:

```rust
let cat = FakeCatalog::new()
    .table("users", id = "id", tenant = Some("org_id"),
           props = [("name", Text), ("age", Integer)])
    .edge("follows", from = "users", to = "users", bidirectional = false);
```

`load()` resolves table names to `TableOid` using the same OID lookup the build
path already uses; the snapshot stores the OID purely so `execute.rs` can call
`acl::check_table_acl(oid)` (DR-4). Ambiguity rule: a GQL label resolves to a
registered table by name; if two registrations could match (alias collision),
`resolve_node_label` returns `AmbiguousLabel`.

## 6. Logical IR (`query/logical_plan.rs`)

Frontend-neutral (GQL today, SQL/PGQ adapter in Phase 3). Core Phase-1 subset of
the operators listed in `architecture-plan.md`:

```rust
pub enum LogicalOp {
    NodeScan { binding: VarId, label: LabelId, predicate: Option<Predicate> },
    NodeLookup { binding: VarId, label: LabelId, key: KeyExpr },
    Expand {
        src: VarId, dst: VarId, rel: RelBinding,
        rel_type: Option<RelTypeId>, direction: Direction,
        var_len: Option<VarLen>, predicate: Option<Predicate>,
    },
    Filter { input: Box<LogicalOp>, predicate: Predicate },
    Project { input: Box<LogicalOp>, items: Vec<ProjItem> },
    Sort { input: Box<LogicalOp>, keys: Vec<SortKey> },
    Skip { input: Box<LogicalOp>, n: u64 },
    Limit { input: Box<LogicalOp>, n: u64 },
}

pub struct LogicalPlan {
    pub root: LogicalOp,
    pub bindings: Vec<Binding>,        // var -> resolved label/rel-type + table OID
    pub required_acl: Vec<TableOid>,   // every touched table (DR-4)
    pub tenant_scopes: Vec<TenantScope>,
    pub is_write: bool,                // always false in Phase 1
    pub supported_modes: ProjectionModes, // csr_readonly | mutable_overlay
    pub row_bound: Option<u64>,        // from LIMIT, for the executor's budget
}
```

`Predicate` is the bound, typed form of the AST `Expr`: each `Compare` is
resolved against a `PropertyInfo` and converted to the engine's existing typed
`FilterCondition` where the column is filter-indexed, or to a hydration-time
predicate otherwise (see physical mapping). Binding produces typed errors
(`UnknownLabel`, `AmbiguousLabel`, `UnknownRelType`, `UnknownProperty`,
`UnsupportedPropertyType`, `MissingParameter`, `ParameterTypeMismatch`).

## 7. Physical plan & mapping to existing primitives (`physical_plan.rs`, `lower.rs`, `execute.rs`)

```rust
pub enum PhysicalOp {
    IndexNodeLookup { ... },        // -> resolution_index lookup
    SourceTableSearch { ... },      // -> sql_search.rs / filter_index seed scan
    ExpandOutCsr { ... },           // -> edge_store.neighbors() forward
    ExpandInCsr { ... },            // -> reverse_edge_store neighbors
    FilterIndexPredicate { ... },   // -> filter_index typed FilterCondition
    HydrationPredicate { ... },     // -> predicate evaluated against hydrated jsonb
    ProjectionJson { ... },         // -> value.rs JSONB encoding
    AggregateRows { ... },          // -> count only in Phase 1
    SortRows / LimitRows / SkipRows,
}
```

Mapping rules:

- A directed `Expand` (`Out`/`In`) lowers to `ExpandOutCsr`/`ExpandInCsr` over
  the existing forward/reverse CSR. Undirected lowers to a union of both with
  de-duplication on the destination coordinate (1D).
- Bounded variable-length lowers to a bounded BFS frontier reusing `bfs.rs`
  iteration with the explicit max depth as the budget.
- A `WHERE`/inline predicate on a filter-indexed column lowers to
  `FilterIndexPredicate` (fast path, existing typed filters). A predicate on a
  modeled-but-not-indexed column lowers to `HydrationPredicate` evaluated after
  `to_jsonb(src.*)` hydration. A predicate on an unmodeled type was already
  rejected at bind time (Q2).
- Tenant scope reuses the existing tenant bitmap path; ACL is enforced by
  `acl::check_table_acl` over `required_acl` before execution begins (DR-4).

`gql_explain()` renders, per stage: parse summary, binding summary, logical
plan, physical plan, chosen runtime (`csr_readonly`), and any rejection reason.

## 8. JSONB result shape

Canonical schema and the null-vs-missing / hydrate rules live in
`architecture-plan.md` → "Canonical JSONB Result Shape". Worked examples for the
snapshot-test corpus:

```jsonc
// MATCH (u:users {id:$id})-[:follows]->(v:users) RETURN v.id, v.name  (hydrate=true)
{"v.id": "u2", "v.name": "Ada"}

// RETURN v                                                            (hydrate=true)
{"v": {"_id": {"table":"users","id":"u2"}, "_labels":["users"], "id":"u2", "name":"Ada", "age":31}}

// RETURN v                                                            (hydrate=false)
{"v": {"_id": {"table":"users","id":"u2"}, "_labels":["users"]}}

// MATCH p = (a:users)-[:follows*1..3]->(b:users) RETURN p             (hydrate=false)
{"p": {"_path": {
   "nodes": [{"_id":{"table":"users","id":"u1"},"_labels":["users"]}, ...],
   "relationships": [{"_type":"follows","_start":{...},"_end":{...}}, ...]
}}}

// RETURN count(v)
{"count(v)": 12}
```

Reserved keys (`_id`,`_labels`,`_type`,`_start`,`_end`,`_path`) are stable
contract. A source column beginning with `_` is a bind-time rejection.

## 9. Error categories (`gql/errors.rs`, pgrx-free)

```rust
pub enum GqlError {
    Syntax { span: Span, msg: String, hint: Option<String> },
    UnsupportedFeature { span: Option<Span>, feature: String },
    Bind(BindError),            // UnknownLabel, AmbiguousLabel, UnknownRelType,
                                // UnknownProperty, UnsupportedPropertyType, ReservedKey
    Parameter { name: String, kind: ParamErrKind },  // Missing | TypeMismatch
    Memory { detail: String },  // Phase 1: only row/hydration caps
    Execution { detail: String },
    Internal(String),
}
```

These map to `GraphError`/SQLSTATE at the `sql_facade/gql.rs` boundary. The
SQLSTATE assignment is the **pre-facade gate task (DR-5)** — do not expose
`graph.gql()` publicly until it is done. `Result<_, String>` is banned in these
APIs (rust-planning rule 20).

## 10. PR slices (TDD order)

Each slice is Red→Green→Refactor with the tests written first.

### 1A — Frontend foundation
- Docs/contract reconciliation: `graph/src/lib.rs:5`, `docs/user_guide/index.mdx`,
  `docs/contributor_guide/architecture.mdx`.
- `lexer.rs`, `ast.rs`, `parser.rs`, `errors.rs`.
- Tests: lexer unit, parser unit, AST span assertions, **fuzz target**
  (`cargo-fuzz`) proving totality (random input → typed error, never panic),
  `insta` snapshots for diagnostics.
- No binding, no execution, no SQL function.

### 1B — First vertical slice (single directed MATCH)
- `catalog_snapshot.rs` (trait + impl + `FakeCatalog`), `semantics.rs` (binder),
  `logical_plan.rs`, `lower.rs` (Expand only), `execute.rs`, `explain.rs`,
  `sql_facade/gql.rs` behind `#[cfg(feature = "development")]`.
- Reads: one directed `MATCH (a:L)-[:T]->(b:L) RETURN a, b`, coordinate-only.
- Tests: binder unit (against fake), logical/physical lowering, `gql_explain`
  snapshot, pgrx SQL test comparing results to an equivalent `graph.traverse()`,
  negative tests (unknown label/type), ACL denial.

Implementation checkpoint:
- Added `graph/src/query/{catalog_snapshot,semantics,logical_plan,physical_plan,lower,execute,explain}.rs`
  with pgrx-free binding, logical/physical planning, coordinate-only CSR
  execution, and a stable one-hop explain string.
- Added `graph/src/sql_facade/gql.rs` behind the `development` feature. The
  facade performs enable/freshness checks, requires ACL on both bound tables,
  maps frontend errors through the existing `GraphError` boundary, and returns
  coordinate rows as JSONB arrays.
- Catalog labels are derived from simple unquoted regclass relation names only;
  duplicate derived labels are rejected as ambiguous until an explicit label
  mapping exists. Bidirectional registered edges bind both physical directions.
- Unit coverage now drives binder negatives, lowering, execution filtering, and
  explain output. A development-feature pgrx test file covers the live
  `graph.traverse()` comparison and ACL-denial gate; local execution still needs
  a working `sfw cargo pgrx test --features "pg17 development" gql` run because
  the wrapper currently returns only its firewall banner.

### 1C — Predicates, RETURN shapes, hydration
- `WHERE` (eq/neq/range/null/membership), inline prop maps, `RETURN`
  node/relationship/property, `value.rs` JSONB encoding + `hydrate` flag.
- DR-4 enforcement (ACL/RLS/tenant) wired in execution.
- Tests: generated matrix over predicate × return-shape × hydration × tenant;
  null-vs-missing; reserved-key rejection; parameter missing/type-mismatch.

Implementation checkpoint:
- Added `query/value.rs` for pgrx-free hydrated predicate evaluation and JSONB
  row projection. SQL hydration remains isolated in `sql_facade/gql.rs`.
- Bound `WHERE`, inline node property maps, whole-node returns, property
  returns, literal lists, and JSONB parameters. Relationship property maps,
  functions, ordering, limits, and variable-length remain later-slice
  rejections.
- `graph.gql(query, params := ..., hydrate := ...)` now returns canonical JSON
  objects rather than the Phase 1B coordinate array. Node returns always include
  `_id` and `_labels`; `hydrate=false` suppresses source-row fields but still
  hydrates internally when predicates or property returns require it.
- Current coverage includes binder property negatives, predicate filtering,
  hydrated node projection, missing-parameter behavior through the value layer,
  and the development pgrx ACL/traverse-parity test path. The broader generated
  matrix and tenant-specific rows remain before the public exposure gate.

### 1D — Ordering, limits, variable-length, undirected
- `ORDER BY`/`SKIP`/`LIMIT` with hard row caps; undirected union+dedup; bounded
  var-length BFS.
- Tests: ordering stability, limit/zero/over-limit, undirected identity rules,
  var-length min/max bounds, unbounded-`*` rejection.
- **Public exposure gate:** flip `graph.gql()` out of `development` only when
  1A–1D + DR-5 SQLSTATE + docs positioning + compatibility-matrix rows are all
  green.

## 11. Benchmark gate

Re-run `bfs_bench` against the `pre_gql_mutable_overlay` baseline after 1B and
1D. The shared planner/IR must add **zero** measurable regression to existing
CSR traversal (GQL is additive; existing SQL paths must not route through new
code). Record the comparison in the baseline file before marking Phase 1 done.
