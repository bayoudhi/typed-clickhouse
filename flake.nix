{
  description = "typed-clickhouse - development environment for the Rust CLI and TypeScript library";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    safe-chain-nix = {
      url = "github:LucioFranco/safe-chain-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      perSystem =
        {
          config,
          self',
          inputs',
          system,
          lib,
          ...
        }:
        let
          # Apply rust overlay
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ (import inputs.rust-overlay) ];
          };

          # Safe-chain wrapper for malware protection
          safeChain = inputs.safe-chain-nix.lib.${system}.safeChain;

          # Rust toolchain
          rustToolchain = pkgs.rust-bin.stable.latest.default.override {
            extensions = [
              "rust-src"
              "clippy"
              "rustfmt"
            ];
          };

          # Node.js with PNPM (wrapped with safe-chain for malware protection)
          nodejs = safeChain.wrapNode pkgs.nodejs_20;
          pnpm = pkgs.pnpm;

          # Python with required packages (wrapped with safe-chain for malware protection)
          python = pkgs.python313;
          pythonEnv = (
            python.withPackages (
              ps: with ps; [
                pip
                setuptools
                wheel
              ]
            )
          );
          wrappedPython = safeChain.wrapPython pythonEnv;

          # Common build inputs
          commonBuildInputs =
            with pkgs;
            [
              pkg-config
              openssl
              protobuf
              # For rdkafka
              rdkafka
              cyrus_sasl
              zlib
              zstd
              lz4
            ]
            ++ lib.optionals pkgs.stdenv.isDarwin [
              pkgs.apple-sdk
              pkgs.libiconv
            ];

          # Helper to convert aliases to scripts
          aliasToScript =
            alias:
            let
              pwd = if alias ? pwd then "$WORKSPACE_ROOT/${alias.pwd}" else "$WORKSPACE_ROOT";
            in
            ''
              set -e
              cd "${pwd}"
              ${alias.cmd}
            '';

          # Define test command aliases
          testAliases = {
            cargo-test = {
              cmd = "cargo test";
            };
            ts-test = {
              pwd = "packages/lib";
              cmd = "pnpm test";
            };
            test-all = {
              cmd = ''
                cargo test && \
                (cd packages/lib && pnpm test)
              '';
            };
          };

          # Generate scripts for all aliases
          testScripts = pkgs.runCommand "test-scripts" { } ''
            mkdir -p $out/bin
            ${lib.concatStringsSep "\n" (
              lib.mapAttrsToList (name: alias: ''
                cat > $out/bin/${name} << 'EOF'
                #!/usr/bin/env bash
                export WORKSPACE_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
                ${aliasToScript alias}
                EOF
                chmod +x $out/bin/${name}
              '') testAliases
            )}
          '';
        in
        {
          # Development Shell
          devShells.default = pkgs.mkShell {
            name = "typed-clickhouse";

            buildInputs = [
              # Languages (with safe-chain malware protection)
              rustToolchain
              nodejs
              pnpm
              wrappedPython
              pkgs.black

              # Development tools
              pkgs.git
              pkgs.turbo
              pkgs.protobuf
              pkgs.maturin
              pkgs.husky

              # Test scripts
              testScripts

              # Build dependencies
            ]
            ++ commonBuildInputs;

            shellHook = ''
              # Set up PNPM
              export PNPM_HOME="$HOME/.local/share/pnpm"
              export PATH="$PNPM_HOME:$PATH"

              # Initialize husky git hooks if not already set up
              if [ -d ".git" ] && [ ! -d ".husky/_" ]; then
                echo "Setting up husky git hooks..."
                husky install
              fi
            '';
          };

          # The `packages` and `apps` outputs were removed in Phase B. Every
          # derivation in them referenced a tree that no longer exists: the
          # `template-packages` derivation took `src = ./templates` (the
          # templates directory was deleted when project scaffolding was
          # dropped), the CLI derivation built `-p moose-cli` (the crate is now
          # `typed-clickhouse`), pinned `outputHashes` for the Temporal crates
          # (no longer dependencies), and copied templates into its output, and
          # the library derivation ran `pnpm build --filter=@514labs/moose-lib`
          # (not a package in this workspace). They could not evaluate, let
          # alone build. The devShell above is the part that still works and is
          # the only thing this flake is used for.
        };
    };
}
