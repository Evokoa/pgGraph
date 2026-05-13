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
    <img src="https://img.shields.io/badge/version-0.1.0-2ea44f?style=flat-square" alt="Version 0.1.0">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" alt="License: Apache-2.0">
  </a>
  <a href="https://www.postgresql.org/">
    <img src="https://img.shields.io/badge/PostgreSQL-13--18-336791?style=flat-square&logo=postgresql&logoColor=white" alt="PostgreSQL 13-18">
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

The fastest way to try pgGraph is the included quickstart script.

It starts a disposable Docker-backed PostgreSQL database, installs pgGraph,
creates two normal PostgreSQL tables, discovers the foreign key relationship,
builds the graph, and runs example queries.

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

The script works on macOS and Linux from a normal terminal, and on Windows from
WSL2 or Git Bash with Docker Desktop. It is not a native PowerShell or Command
Prompt script.

The root Docker image currently runs PostgreSQL 17. Package scripts can build
extension artifacts for PostgreSQL 13 through 18. The PostgreSQL major version
of the extension package must match the target server.

## Documentation
More information is available in the pgGraph docs:

**[Overview](https://docs.evokoa.com/pggraph/user_guide)** ·
**[Quickstart](https://docs.evokoa.com/pggraph/quickstart)** ·
**[Installation](https://docs.evokoa.com/pggraph/user_guide/installation)** ·
**[Playground](https://docs.evokoa.com/pggraph/user_guide/playground)** ·
**[Querying](https://docs.evokoa.com/pggraph/user_guide/querying)** ·
**[SQL API](https://docs.evokoa.com/pggraph/user_guide/api-reference)**

## Community

pgGraph is built by [Evokoa](https://evokoa.com). 
Follow the project through
the links at the top of this README.

## License

Apache-2.0. See [LICENSE](LICENSE).
