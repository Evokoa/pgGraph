{
  description = "pgGraph dev environment (optional, opt-in).";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        # Toolchain pinned to the rust-version in graph/Cargo.toml.
        rust = pkgs.rust-bin.stable."1.95.0".default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };

        # cargo-pgrx version is pinned in graph/Cargo.toml as `pgrx = "=0.18.0"`.
        # nixpkgs' cargo-pgrx may drift from that, so we install the exact
        # version into a project-local CARGO_INSTALL_ROOT on first shell entry.
        # This keeps the host's global ~/.cargo/bin untouched.
        pgrxVersion = "0.18.0";

        commonBuildInputs = with pkgs; [
          icu
          openssl
          zlib
          readline
          libxml2
          libxslt
          pkg-config
        ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
          libiconv
        ];

        mkShell = pg: pkgs.mkShell {
          packages = [
            rust
            pg
            pkgs.cacert
          ] ++ commonBuildInputs;

          shellHook = ''
            export PGRX_HOME="$PWD/.pgrx"
            export CARGO_INSTALL_ROOT="$PWD/.nix-cargo"
            export PATH="$CARGO_INSTALL_ROOT/bin:$PATH"
            export PG_CONFIG="${pg}/bin/pg_config"

            mkdir -p "$PGRX_HOME" "$CARGO_INSTALL_ROOT"

            # Install the pinned cargo-pgrx if not already present.
            if ! "$CARGO_INSTALL_ROOT/bin/cargo-pgrx" --version 2>/dev/null | grep -q '${pgrxVersion}'; then
              echo "Installing cargo-pgrx ${pgrxVersion} into $CARGO_INSTALL_ROOT ..."
              cargo install --locked cargo-pgrx --version ${pgrxVersion}
            fi

            # Initialize pgrx against the nix-provided Postgres for this major.
            pg_major="$("$PG_CONFIG" --version | awk '{print $2}' | cut -d. -f1)"
            if [ ! -f "$PGRX_HOME/config.toml" ] || ! grep -q "^pg$pg_major[[:space:]]*=" "$PGRX_HOME/config.toml"; then
              echo "Running cargo pgrx init --pg$pg_major=$PG_CONFIG ..."
              (cd "$PWD/graph" && cargo pgrx init --pg$pg_major="$PG_CONFIG")
            fi

            echo "pgGraph devshell: PG $pg_major @ $PG_CONFIG"
            echo "Try: (cd graph && cargo test --features pg$pg_major)"
          '';
        };
      in {
        devShells = {
          default = mkShell pkgs.postgresql_17;
          pg13 = mkShell pkgs.postgresql_13;
          pg14 = mkShell pkgs.postgresql_14;
          pg15 = mkShell pkgs.postgresql_15;
          pg16 = mkShell pkgs.postgresql_16;
          pg17 = mkShell pkgs.postgresql_17;
          pg18 = mkShell pkgs.postgresql_18;
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
