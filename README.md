<p align="center">
  <img src="assets/pggraph-banner.png" alt="pgGraph Banner" />
</p>

<h1 align="center">pgGraph    <a href="https://docs.evokoa.com/pggraph/user_guide">
    <img src="https://img.shields.io/badge/docs-pgGraph-0ea5e9?style=flat-square" alt="pgGraph documentation">
  </a></h1>

<p align="center">
  <strong>Graph database superpowers for your existing Postgres data.</strong>
</p>

<p align="center">
  <a href="https://github.com/evokoa/pggraph/stargazers">
    <img src="https://img.shields.io/github/stars/evokoa/pggraph?style=flat-square&logo=github&label=stars" alt="GitHub stars">
  </a>
  <a href="https://github.com/evokoa/pggraph/releases">
    <img src="https://img.shields.io/badge/version-0.1.5-2ea44f?style=flat-square" alt="Version 0.1.5">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" alt="License: Apache-2.0">
  </a>
  <a href="https://www.postgresql.org/">
    <img src="https://img.shields.io/badge/PostgreSQL-14--18-336791?style=flat-square&logo=postgresql&logoColor=white" alt="PostgreSQL 14-18">
  </a>
  <a href="https://ghcr.io/evokoa/pggraph">
    <img src="https://img.shields.io/badge/Docker-ghcr.io%2Fevokoa%2Fpggraph-blue?style=flat-square&logo=docker&logoColor=white" alt="Docker image">
  </a>
</p>

<p align="center">
  <a href="https://github.com/evokoa/pggraph/issues">
    <img src="https://img.shields.io/github/issues/evokoa/pggraph?style=flat-square&logo=github&label=issues" alt="GitHub issues">
  </a>
  <a href="https://github.com/evokoa/pggraph/pulls">
    <img src="https://img.shields.io/github/issues-pr/evokoa/pggraph?style=flat-square&logo=github&label=PRs" alt="GitHub pull requests">
  </a>
  <a href="https://github.com/evokoa/pggraph/commits/main">
    <img src="https://img.shields.io/github/last-commit/evokoa/pggraph?style=flat-square&logo=github&label=last%20commit" alt="Last commit">
  </a>
</p>

<p align="center">
  <a href="https://evokoa.com" target="_blank" rel="noreferrer">
  <img
    src="https://img.shields.io/badge/Built%20by-Evokoa-ff6b35?style=for-the-badge"
    alt="Built by Evokoa"
  >
  </a>
  <a href="https://x.com/evokoa_ai" target="_blank" rel="noreferrer">
    <img
      src="https://img.shields.io/badge/Follow%20on%20X-000000?style=for-the-badge&logo=x&logoColor=white"
      alt="Follow on X"
    >
  </a>
  <a href="https://discord.gg/GnHR8ezuwG" target="_blank" rel="noreferrer">
    <img
      src="https://img.shields.io/discord/1496159762704896022?style=for-the-badge&label=Join%20Discord&logo=discord&logoColor=white&color=5865F2"
      alt="Join the Evokoa Discord"
    >
  </a>
  <a href="https://www.producthunt.com/@evokoa" target="_blank" rel="noreferrer">
    <img
      src="https://img.shields.io/badge/Follow%20on%20Product%20Hunt-DA552E?style=for-the-badge&logo=product-hunt&logoColor=white"
      alt="Follow on Product Hunt"
    >
  </a>
</p>
pgGraph is a PostgreSQL extension for running graph search, traversal, shortest
path, and relationship queries directly against ordinary PostgreSQL tables.

Your tables stay the source of truth. pgGraph builds a derived graph index and
lets you query it from SQL using functions in the `graph` schema.

> [!IMPORTANT]
> pgGraph is in early alpha. Even though we have tested it to be stable,
> please avoid production use for now; try it in
> Docker or a dedicated development database and share feedback to help the
> project grow.

## Why pgGraph?

PostgreSQL is great at relational queries, but graph-style questions often
require custom recursive SQL for each schema:

- “Find records related to Alice within 2 hops.”
- “Find the shortest path between this person and this company.”
- “Search nodes across registered tables.”

pgGraph adds graph queries on top of your existing PostgreSQL tables, without
requiring a separate graph database, graph-specific storage system, or a new
query language.

## Quickstart

The fastest way to try pgGraph is to pull the pre-built Docker image — no
build step needed.

The image is multi-arch (`linux/amd64` and `linux/arm64`) and works on macOS,
Linux, and Windows via Docker Desktop.

```bash
docker pull ghcr.io/evokoa/pggraph:0.1.5
docker run -d --rm \
  --name pggraph \
  -e POSTGRES_PASSWORD=postgres \
  -p 5432:5432 \
  ghcr.io/evokoa/pggraph:0.1.5
```

The default database is `graph` with `pg_cron` and a maintenance job
pre-configured.

Verify the extensions are loaded (uses `psql` inside the container, so you
don't need a local PostgreSQL client):

```bash
docker exec pggraph psql -U postgres -d graph \
  -c "SELECT extname, extversion FROM pg_extension WHERE extname IN ('graph', 'pg_cron');"
```

If you have `psql` installed locally you can also connect directly:

```bash
psql -h localhost -U postgres -d graph
```

To build from source or run the full interactive demo instead, use the included
quickstart script. It starts a disposable Docker-backed PostgreSQL database,
installs pgGraph, creates two normal PostgreSQL tables, discovers the foreign
key relationship, builds the graph, and runs example queries.

You need Docker or Docker Desktop installed and running:

- macOS: install Docker Desktop.
- Windows: install Docker Desktop with WSL2 enabled, then run the script from
  WSL2 or Git Bash.
- Linux: install Docker Engine and the Docker Compose plugin.

```bash
git clone https://github.com/evokoa/pggraph.git
cd pggraph

# run the full quickstart demo
scripts/quickstart.sh

# install into existing Postgres Docker container
scripts/quickstart.sh docker my-postgres 17 appdb postgres

# source build/install with pgrx into local PostgreSQL
scripts/quickstart.sh pgrx

# start Streamlit playground with a preset dataset (panama|ldbc)
scripts/quickstart.sh playground panama
```

Supported modes:

- `quickstart` / `demo`: build and start the Docker Postgres service, load demo
  data, and run example graph queries. This is the default mode.
- `setup`: build and start Postgres with pgGraph installed, but do not load the
  sample graph.
- `psql`: build and start Postgres, prepare demo data, then open `psql`.
- `docker CONTAINER [PG_MAJOR] [DB_NAME] [DB_USER]`: install pgGraph into an
  existing running Postgres Docker container via
  `scripts/install_into_docker_postgres.sh`.
- `pgrx [PG_MAJOR]`: build and install pgGraph into a local PostgreSQL using
  `cargo pgrx install`.
- `playground [panama|ldbc]`: start the Streamlit playground using a preset
  dataset.
- `clean`: stop the Compose database and remove its volume.

The script works on macOS and Linux from a normal terminal, and on Windows from
WSL2 or Git Bash with Docker Desktop. It is not a native PowerShell or Command
Prompt script.

The root Docker image currently runs PostgreSQL 17. Package scripts can build
extension artifacts for officially supported PostgreSQL 14 through 18 targets.
PostgreSQL 13 is no longer an official support target after upstream EOL, though
the legacy `pg13` pgrx feature remains available on a best-effort basis. The
PostgreSQL major version of the extension package must match the target server.

## PGXN Source Installation

pgGraph is available on PGXN as a source distribution. Because pgGraph is a
Rust/pgrx extension, building from source requires the Rust toolchain.

### Prerequisites

- PostgreSQL development headers and `pg_config`
- Rust toolchain (`1.95`, pinned by `graph/rust-toolchain.toml`)
- `cargo-pgrx` 0.18.0

### Install with pgxn-client

```bash
cargo install cargo-pgrx --version 0.18.0 --locked
# Register the installed PostgreSQL with pgrx (auto-detects the major):
PG_MAJOR=$(pg_config --version | sed -E 's/[^0-9]*([0-9]+).*/\1/')
cargo pgrx init --pg${PG_MAJOR}="$(which pg_config)"
pgxn install pgGraph
```

### Manual source install

```bash
git clone https://github.com/evokoa/pggraph.git
cd pggraph
make install # may need sudo
psql -d postgres -c "CREATE EXTENSION graph;"
```

If you have multiple PostgreSQL installations, set `PG_CONFIG` to the target
server's `pg_config`, then re-run the installation:

```bash
export PG_CONFIG=/usr/lib/postgresql/17/bin/pg_config
make install
```

If `sudo` is needed for `make install`, preserve `PG_CONFIG`:

```bash
sudo --preserve-env=PG_CONFIG make install
```

If compilation fails with `fatal error: postgres.h: No such file or directory`,
install the PostgreSQL server development package for the target PostgreSQL
major, such as `postgresql-server-dev-17` on Ubuntu or Debian.

> **Note:** The PGXN distribution name is `pgGraph` but the PostgreSQL extension
> name is `graph`. Use `CREATE EXTENSION graph;` after installation.

## Documentation
More information is available in the pgGraph docs:

**[Overview](https://docs.evokoa.com/pggraph/user_guide)** ·
**[Quickstart](https://docs.evokoa.com/pggraph/quickstart)** ·
**[Installation](https://docs.evokoa.com/pggraph/user_guide/installation)** ·
**[Playground](https://docs.evokoa.com/pggraph/user_guide/playground)** ·
**[Querying](https://docs.evokoa.com/pggraph/user_guide/querying)** ·
**[SQL API](https://docs.evokoa.com/pggraph/user_guide/api-reference)**

## pgGraph: High-Speed Graph Execution Inside PostgreSQL

pgGraph is not "Postgres plus graph syntax." It is a cache-friendly graph
execution layer for data that already lives in your ordinary relational tables.

The core idea is simple but powerful: keep PostgreSQL as your system of record,
but build a highly optimized, read-heavy graph runtime from that relational
metadata. The result is closer to a rebuildable graph index than a graph
database: it is built from Postgres tables, operated with Postgres controls,
and optimized for repeated bounded traversal over known topology.

### The Tech: Why It's So Fast

Graph traversals usually die on recursive SQL queries or endless joins. pgGraph
bypasses this by compiling your relational data into a specialized memory
structure.

- **O(1) adjacency via CSR.** `graph.build()` compiles your relationships into
  forward and reverse compressed sparse row (CSR) edge stores. A node's
  neighbors are stored as a contiguous array slice. Instead of rediscovering
  relationships via SQL, traversals are executed as raw, graph-native memory
  scans.
- **A tight traversal loop.** SQL-facing calls resolve coordinates, labels,
  filters, and tenant scopes before entering the traversal loop. Once inside,
  the engine streams CSR neighbors, checking compact `u8` edge-label IDs,
  typed `FilterIndex` values, tenant bitmaps, active bits, and sync overlays.
- **Read-only artifact mapping.** Persisted `.pggraph` artifacts are written
  atomically. When a new Postgres backend spins up, it validates the artifact
  and maps immutable forward graph arrays and the resolution index read-only.
  The operating system page cache can then share those physical pages across
  isolated PostgreSQL backends without copying the base graph into each
  backend's Rust heap. This is not a replacement for PostgreSQL's buffer pool:
  PostgreSQL remains responsible for table storage, WAL, MVCC, durability, and
  crash recovery, while pgGraph's artifact is derived state that can be rebuilt
  from source tables.
- **Predictable and safe.** Unbounded graph expansion can crash a database.
  pgGraph includes explicit circuit breakers: depth limits, visited-node
  tracking, frontier limits, pagination, and strict OOM/memory safeguards.

### PostgreSQL Remains Authoritative

Your application data does not move. Source tables, constraints, indexes, ACLs,
RLS, backups, and app writes remain 100% standard PostgreSQL concerns.

pgGraph is strictly derived state. You run the algorithms over internal node
indexes, and the engine returns source table coordinates or hydrates the raw
PostgreSQL rows on the fly. Build, sync, vacuum, and maintenance operations are
fully visible and SQL-callable.

### How pgGraph Compares

#### vs. Apache AGE: Execution Layer vs. Storage Layer

Apache AGE is a property graph database inside Postgres. It uses graph
namespaces, vertex and edge tables, `agtype`, and openCypher.

pgGraph does not ask you to move your data or learn Cypher. You keep your
existing schema and accelerate it with SQL functions like `graph.search()` and
`graph.shortest_path()`. Use AGE for a dedicated property graph model; use
pgGraph to add bounded, high-speed graph traversal to an existing relational
schema.

#### vs. PostgreSQL 19 SQL/PGQ

SQL:2023 and PostgreSQL 19 introduce `CREATE PROPERTY GRAPH`, `GRAPH_TABLE`,
and standard graph pattern matching backed by PostgreSQL's planner and
optimizer — the same engine that makes PostgreSQL's relational queries strong.

pgGraph operates at a different layer. SQL/PGQ expresses graph patterns and lets
the optimizer choose how to execute them. pgGraph precomputes CSR adjacency
stores and rebuildable artifacts for workloads that repeatedly traverse the same
topology with bounded depth, path limits, filters, tenants, and application
pagination. The two can be complementary: future adapters could map eligible
SQL/PGQ patterns onto pgGraph's precomputed runtime, while general graph queries
continue to use PostgreSQL's relational execution path.

## Community

pgGraph is built by [Evokoa](https://evokoa.com). 
Follow the project through
the links at the top of this README.

## License

Apache-2.0. See [LICENSE](LICENSE).
