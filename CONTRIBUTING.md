# Contributing to pgGraph

pgGraph is a PostgreSQL extension written in Rust with pgrx. The authoritative
product and implementation documentation lives in `docs/user_guide` and
`docs/contributor_guide`. Implementation work should keep code, SQL behavior,
and docs aligned.

## Development Setup

```bash
git clone https://github.com/evokoa/pggraph.git
cd pggraph/graph
cargo pgrx init
cargo test --features pg17
cargo pgrx test pg17
```

`cargo pgrx init` with no arguments builds PostgreSQL from source and needs
ICU, pkg-config, bison, flex, readline, zlib, openssl, and perl on the host.
On macOS the typical prerequisites are:

```bash
brew install icu4c pkg-config bison flex readline zlib openssl@3
export PKG_CONFIG_PATH="$(brew --prefix icu4c)/lib/pkgconfig:$PKG_CONFIG_PATH"
```

To skip the source build entirely, point pgrx at an existing PostgreSQL
install:

```bash
cargo pgrx init --pg17 $(brew --prefix postgresql@17)/bin/pg_config
```

Use PostgreSQL 14 through 18 when validating compatibility-sensitive changes.
PostgreSQL 13 has reached upstream EOL and is no longer an official support
target, though the legacy `pg13` pgrx feature remains available best-effort.
The default local feature is `pg17`.

### Nix devshell (optional)

A `flake.nix` is provided for contributors who use Nix. It pins the Rust
toolchain, `cargo-pgrx`, and a Postgres major matching the pgrx feature
flag, and initializes `cargo pgrx` against the nix-provided Postgres on
first entry for the selected major — no system deps to install.

```bash
nix develop          # default shell: pg17
nix develop .#pg16   # switch majors
cd graph && cargo test --features pg17
```

With direnv installed, `direnv allow` activates the shell automatically.
The Nix path is opt-in; the brew/apt paths above remain fully supported.

## Pull Request Bar

- Keep SQL APIs aligned with `docs/user_guide/api-reference.mdx`.
- Add Rust unit tests for engine/data-structure changes.
- Add pgrx SQL tests for public SQL behavior, ACLs, SQLSTATEs, sync, and
  persistence behavior.
- Run `cargo fmt --check` before submitting.
- Include Criterion or SQL benchmark results for performance-sensitive changes.
- Update user-guide docs for SQL or operational behavior changes.
- Update contributor-guide docs for storage, loader, memory model, safety, or
  internal architecture changes.

## Scope

Accepted changes include bug fixes, crash-safety improvements, SQL API
conformance, memory/performance improvements, tests, docs, examples, and
benchmarking improvements.

Out of scope for V1 are new graph query languages, distributed consensus,
runtime dependencies outside PostgreSQL/Rust, and features that require a
separate graph service outside PostgreSQL.
