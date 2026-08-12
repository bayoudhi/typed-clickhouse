# AGENTS.md

Multi-language monorepo (Rust CLI + TypeScript library) using PNPM workspaces, Turbo Repo, and Cargo workspace.

**CRITICAL**: Run `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `pnpm --filter @typed-clickhouse/lib test` before committing. When changing the serverless library, verify the golden schema baseline still reproduces (`cd packages/core && pnpm build && pnpm test && ./tests/golden.test.sh`). Logs: `~/.tch/*-cli.log`. Always format the code.

## Build & Development Commands

### All Languages
- **Build all**: `pnpm build` (Turbo orchestrates builds)
- **Lint all**: `pnpm lint`
- **Format**: `pnpm format` (Prettier for TS/JS)

### Rust
- **Build**: `cargo build`
- **Test**: `cargo test`
- **Lint**: `cargo clippy --all-targets -- -D warnings` (REQUIRED pre-commit, no warnings allowed)
- **Format**: `cargo fmt`
- Toolchain is pinned by `rust-toolchain.toml`. Never pass an explicit
  `toolchain:` to `actions-rust-lang/setup-rust-toolchain` — it shadows the pin
  and lints on a different compiler than the one that builds releases.

### TypeScript
- **Test lib**: `cd packages/lib && pnpm test` (189 tests, mocha)
- **Typecheck**: `cd packages/lib && pnpm typecheck`
- **Build serverless library**: `pnpm --filter @typed-clickhouse/core build`

## Code Style Guidelines

### TypeScript/JavaScript
- **Imports**: Group by external deps, internal modules, types; use named exports from barrel files (`index.ts`)
- **Naming**: camelCase for vars/functions, PascalCase for types/classes/components, UPPER_SNAKE_CASE for constants
- **Types**: Prefer interfaces for objects, types for unions/intersections; explicit return types on public APIs
- **Unused vars**: Prefix with `_` (e.g., `_unusedParam`) to bypass linting errors
- **Formatting**: Prettier with `experimentalTernaries: true`; auto-formats on commit (Husky + lint-staged)
- **ESLint**: flat config at `eslint.config.js`; `@typescript-eslint/no-explicit-any` disabled

### Rust
- **Error handling**: Use `thiserror` with `#[derive(thiserror::Error)]`; define errors near fallibility unit (NO global `Error` type); NEVER use `anyhow::Result`
- **Naming**: snake_case for functions/vars, PascalCase for types/traits, SCREAMING_SNAKE_CASE for constants
- **Constants**: Place in `constants.rs` at appropriate module level
- **Newtypes**: Use tuple structs with validation constructors (e.g., `struct UserId(String)`)
- **Tests**: Inline with `#[cfg(test)]` modules
- **Documentation**: Required for all public APIs

## Repository Structure

- **`apps/cli/`**: the Rust CLI
- **`apps/cli-npm/`**: npm wrapper that ships the CLI binaries
- **`packages/lib/`**: TypeScript library source
- **`packages/core/`**: the published serverless library
- **`packages/ts-config/`**: shared TypeScript config
- **`packages/protobuf/`**: `.proto` definitions; `apps/cli/build.rs` generates Rust from these

## Testing Philosophy

- **Library tests** (`packages/lib/tests/`): unit tests colocated with the library
- **Golden gate** (`packages/core/tests/`): asserts the compiler
  plugin's emitted schema does not drift from the captured baseline
- There are no E2E tests or templates in this repository. The downstream
  consumer pins published versions.

## Key Technologies

Rust (CLI), TypeScript (lib), ClickHouse (OLAP)

## Identifiers deliberately left alone

These still read `moose` and must stay that way. `MIGRATION.md` explains each to
users; the short version for contributors:

- `_MOOSE_STATE` — the ClickHouse state table. Renaming orphans stored state.
- `[MOOSE_METADATA:DO_NOT_MODIFY]` — the column-comment prefix written into live
  ClickHouse tables and parsed back out.
- `moose_rls_role` / `moose_rls_user` / `SQL_moose_rls_*` — ClickHouse role, user
  and setting names baked into row policies in live databases.
- `MOOSE_RLS_ROLE`, `MOOSE_RLS_USER`, `MOOSE_RLS_SETTING_PREFIX`,
  `MOOSE_RUNTIME_ENV_PREFIX`, `mooseRuntimeEnv`, `mooseEnvSecrets` — the six
  `@typed-clickhouse/core` exports that still carry the old name. They are the
  published surface, pinned by `packages/core/tests/golden/exports.json`.
- `moose_version` — proto field 19, kept as provenance beside
  `data_model_version`.
- `MOOSE_CLI_VERSION` — the build-time version contract between `build.rs` and
  the release workflow.
- `OLD_PROJECT_CONFIG_FILE = "moose.config.toml"` — the fallback that keeps
  existing projects loading.
- `@514labs/moose-lib` in tests and the compiler plugin — a real, third-party
  published package this one is interoperability-compatible with.

## Release

- **`THIRD-PARTY-NOTICES.md` must be generated at publish time**, not committed
  stale. The published artifacts redistribute other people's code: the Rust
  binary statically links its full dependency tree, and tsup bundles
  `commander` into `dist/`. Those licences — predominantly MIT and Apache-2.0 —
  require their notices to travel with the binary, and Apache-2.0 propagates any
  upstream `NOTICE`. Generate with `cargo about` or `cargo-deny` for Rust and a
  licence checker for npm, and list the file in each package's `files`.
- **The CLI and the library version in lockstep.** `bin.rs` compares the CLI's
  version to `tch-runner print-version` with strict equality, so both packages
  must publish from the same tag. `release.yaml`'s `version` job computes it
  once and every publishing job consumes that output.
