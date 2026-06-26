<p align="center">
  <img src="assets/pggraph-banner.png" alt="pgGraph Banner" />
</p>

<h1 align="center">pgGraph    <a href="https://docs.evokoa.com/pggraph/user_guide">
    <img src="https://img.shields.io/badge/docs-pgGraph-0ea5e9?style=flat-square" alt="pgGraph documentation">
  </a></h1>

<p align="center">
  <strong>为你现有的 Postgres 数据带来图数据库级能力。</strong>
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
pgGraph 是一个 PostgreSQL 扩展，用于直接针对普通 PostgreSQL 表运行图搜索、遍历、最短路径和关系查询。

你的表仍然是事实来源。pgGraph 会构建一个派生的图索引，并让你通过 SQL 使用 `graph` schema 中的函数来查询它。

> [!IMPORTANT]
> pgGraph 处于早期 alpha 阶段。虽然内部测试表明它已相当稳定，但目前请避免在生产环境中使用；建议在 Docker 或专用开发数据库中尝试它，并分享反馈来帮助我们改进项目。

## 为什么选择 pgGraph？

PostgreSQL 很擅长关系查询，但基于图结构的查询通常需要为每个 schema 编写自定义递归 SQL：

- “查找 Alice 在 2 跳以内相关的记录。”
- “查找这个人与这家公司之间的最短路径。”
- “在已注册的表中搜索节点。”

pgGraph 在你现有的 PostgreSQL 表之上添加图查询，不需要单独的图数据库、图专用存储系统或新的查询语言。

## 快速开始

想要快速体验 pgGraph，最简单的方法是拉取预构建的 Docker 镜像——无需编译。

该镜像为多架构镜像（`linux/amd64` 和 `linux/arm64`），可在 macOS、Linux 和
Windows（通过 Docker Desktop）上运行。

```bash
docker pull ghcr.io/evokoa/pggraph:0.1.5
docker run -d --rm \
  --name pggraph \
  -e POSTGRES_PASSWORD=postgres \
  -p 5432:5432 \
  ghcr.io/evokoa/pggraph:0.1.5
```

默认数据库是 `graph`，已预配置 `pg_cron` 和维护定时任务。

验证扩展是否已加载（使用容器内的 `psql`，无需本地安装 PostgreSQL 客户端）：

```bash
docker exec pggraph psql -U postgres -d graph \
  -c "SELECT extname, extversion FROM pg_extension WHERE extname IN ('graph', 'pg_cron');"
```

如果你已在本地安装了 `psql`，也可以直接连接：

```bash
psql -h localhost -U postgres -d graph
```

如果你想从源码构建或运行完整的交互式演示，可以使用提供的快速启动脚本。
它会启动一个一次性的、基于 Docker 的 PostgreSQL 数据库，安装 pgGraph，创建两个普通 PostgreSQL 表，发现外键关系，构建图，并运行示例查询。

你需要安装并运行 Docker 或 Docker Desktop：

- macOS：安装 Docker Desktop。
- Windows：安装启用了 WSL2 的 Docker Desktop，然后从 WSL2 或 Git Bash 运行脚本。
- Linux：安装 Docker Engine 和 Docker Compose 插件。

```bash
git clone https://github.com/evokoa/pggraph.git
cd pggraph

# 运行完整的快速开始演示
scripts/quickstart.sh

# 安装到现有的 Postgres Docker 容器
scripts/quickstart.sh docker my-postgres 17 appdb postgres

# 使用 pgrx 从源码构建并安装到本地 PostgreSQL
scripts/quickstart.sh pgrx

# 使用预设数据集和模式启动 Streamlit playground（panama|ldbc，csr|mutable）
scripts/quickstart.sh playground panama csr
scripts/quickstart.sh playground panama mutable
```

支持的模式：

- `quickstart` / `demo`：构建并启动 Docker Postgres 服务，加载演示数据，
  并运行示例图查询。这是默认模式。
- `setup`：构建并启动已安装 pgGraph 的 Postgres，但不加载示例图。
- `psql`：构建并启动 Postgres，准备演示数据，然后打开 `psql`。
- `docker CONTAINER [PG_MAJOR] [DB_NAME] [DB_USER]`：通过
  `scripts/install_into_docker_postgres.sh` 将 pgGraph 安装到现有运行中的
  Postgres Docker 容器。
- `pgrx [PG_MAJOR]`：使用 `cargo pgrx install` 构建 pgGraph，并安装到本地
  PostgreSQL。
- `playground [panama|ldbc] [csr|mutable]`：使用预设数据集和投影模式启动
  Streamlit playground。
- `clean`：停止 Compose 数据库并删除其 volume。

该脚本可在 macOS 和 Linux 的普通终端中运行，也可在 Windows 上通过 WSL2 或带有 Docker Desktop 的 Git Bash 运行。它不是原生 PowerShell 或命令提示符脚本。

从 v0.1.5 开始，Docker 发布镜像覆盖 PostgreSQL 14 到 18。未带 PostgreSQL 主版本的 tag（例如 `0.1.5` 和 `latest`）使用默认 PostgreSQL 17 镜像。PostgreSQL 13 已到上游 EOL，不再是官方支持目标，但旧的 `pg13` pgrx feature 仍可按 best-effort 方式使用。扩展的 PostgreSQL 主版本必须与目标服务器匹配。

## PGXN 源码安装

pgGraph 在 PGXN 上以源码分发包的形式提供。因为 pgGraph 是一个 Rust/pgrx 扩展，
从源码构建需要 Rust 工具链。

### 前置要求

- PostgreSQL 开发头文件和 `pg_config`
- Rust 工具链（`1.96`，由 `graph/rust-toolchain.toml` 锁定）
- `cargo-pgrx` 0.19.1

### 使用 pgxn-client 安装

```bash
cargo install cargo-pgrx --version 0.19.1 --locked
# 用 pgrx 注册已安装的 PostgreSQL（自动识别主版本）：
PG_MAJOR=$(pg_config --version | sed -E 's/[^0-9]*([0-9]+).*/\1/')
cargo pgrx init --pg${PG_MAJOR}="$(which pg_config)"
pgxn install pgGraph
```

### 手动源码安装

```bash
git clone https://github.com/evokoa/pggraph.git
cd pggraph
make install # 可能需要 sudo
psql -d postgres -c "CREATE EXTENSION graph;"
```

如果你安装了多个 PostgreSQL 版本，可以将 `PG_CONFIG` 指向目标服务器的
`pg_config`，然后重新运行安装：

```bash
export PG_CONFIG=/usr/lib/postgresql/17/bin/pg_config
make install
```

如果 `make install` 需要 `sudo`，请保留 `PG_CONFIG` 环境变量：

```bash
sudo --preserve-env=PG_CONFIG make install
```

如果编译失败并出现 `fatal error: postgres.h: No such file or directory`，
请安装目标 PostgreSQL 主版本对应的服务器开发包，例如 Ubuntu 或 Debian 上的
`postgresql-server-dev-17`。

> **注意：** PGXN 分发名称是 `pgGraph`，但 PostgreSQL 扩展名是 `graph`。
> 安装后请使用 `CREATE EXTENSION graph;`。

## 文档
更多信息可在 pgGraph 文档中找到：

**[概览](https://docs.evokoa.com/pggraph/user_guide)** ·
**[快速开始](https://docs.evokoa.com/pggraph/quickstart)** ·
**[安装](https://docs.evokoa.com/pggraph/user_guide/installation)** ·
**[Playground](https://docs.evokoa.com/pggraph/user_guide/playground)** ·
**[查询](https://docs.evokoa.com/pggraph/user_guide/querying)** ·
**[SQL API](https://docs.evokoa.com/pggraph/user_guide/api-reference)**

## pgGraph：PostgreSQL 内部的高速图执行

pgGraph 不是“Postgres 加上图语法”。它是一个缓存友好的图执行层，用于已经存在于普通关系表中的数据。

核心思想简单但强大：保留 PostgreSQL 作为你的核心记录系统，但从这些关系元数据构建一个针对读取优化的图运行时。结果更接近可重建的图索引而不是图数据库：它从 Postgres 表构建，受 Postgres 权限系统控制，并针对已知拓扑上的重复、有边界遍历进行优化。

### 技术：为什么它这么快

图遍历通常会卡在递归 SQL 查询或无尽的 join 上。pgGraph 通过把你的关系数据编译成专门的内存结构来绕过这一点。

- **通过 CSR 实现 O(1) 邻接访问。** `graph.build()` 会把你的关系编译成正向和反向的压缩稀疏行（CSR）边存储。一个节点的邻居被存储为连续的数组切片。遍历不是通过 SQL 重新发现关系，而是作为底层的、图原生的内存扫描来执行。
- **紧凑的遍历热循环。** 面向 SQL 的调用会在进入遍历循环之前解析坐标、标签、过滤器和租户范围。一旦进入内部，引擎会流式读取 CSR 邻居，检查紧凑的 `u8` 边标签 ID、有类型的 `FilterIndex` 值、租户位图、活跃位和同步覆盖层。
- **只读构件映射。** 持久化的 `.pggraph` 文件会被原子写入。当一个新的 Postgres 后端进程启动时，它会验证该构件，并以只读方式映射不可变的正向图数组和解析索引。操作系统页缓存可以在相互隔离的 PostgreSQL 后端进程之间共享这些物理页，而无需把基础图复制到每个后端的 Rust 堆中。这不是 PostgreSQL buffer pool 的替代品：PostgreSQL 仍然负责表存储、WAL、MVCC、持久性和崩溃恢复；pgGraph 的构件是可从源表重建的派生状态。
- **可预测且安全。** 无限制的图遍历可能让数据库崩溃。pgGraph 包含显式断路器：深度限制、已访问节点跟踪、frontier 限制、分页，以及严格的 OOM/内存保护。

### PostgreSQL 仍然权威

你的应用数据不会移动。源表、约束、索引、ACL、RLS、备份和应用写入仍然 100% 是标准 PostgreSQL 的原生特性。

pgGraph 的状态是严格派生自原始数据的。你在内部节点索引上运行算法，引擎返回源表坐标，或即时补全原始 PostgreSQL 行。构建、同步、vacuum 和维护操作都是完全可见且可通过 SQL 调用的。

### pgGraph 如何比较

#### 对比 Apache AGE：执行层 vs. 存储层

Apache AGE 是 Postgres 内部的属性图数据库。它使用图命名空间、顶点和边表、`agtype` 以及 openCypher。

pgGraph 不要求你移动数据或学习 Cypher。你保留现有 schema，并用 `graph.search()` 和 `graph.shortest_path()` 这样的 SQL 函数加速它。对于专用的属性图模型，请使用 AGE；对于给现有关系 schema 添加有边界的高速图遍历，请使用 pgGraph。

#### 对比 PostgreSQL 19 SQL/PGQ

SQL:2023 和 PostgreSQL 19 引入了 `CREATE PROPERTY GRAPH`、`GRAPH_TABLE` 和标准图模式匹配，并由 PostgreSQL 的 planner 和 optimizer 支撑——与驱动 PostgreSQL 强大关系查询的引擎相同。

pgGraph 在不同的层级上工作。SQL/PGQ 表达图模式并让 optimizer 选择执行方式。pgGraph 为反复遍历同一拓扑的工作负载预计算 CSR 邻接存储和可重建构件，适合有深度上限、路径限制、过滤器、租户范围和应用分页的遍历。两者可以互补：未来的适配器可以把符合条件的 SQL/PGQ 模式映射到 pgGraph 的预计算运行时，而通用图查询继续使用 PostgreSQL 的关系执行路径。

## 社区

pgGraph 由 [Evokoa](https://evokoa.com) 构建。
通过本 README 顶部的链接关注该项目。

## 许可证

Apache-2.0。参见 [LICENSE](LICENSE)。
