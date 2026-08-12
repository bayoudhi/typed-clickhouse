[![MIT license](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

# typed-clickhouse

**typed-clickhouse** is a type-safe, code-first migration and query tool for ClickHouse.

Declare your ClickHouse tables and views as TypeScript types, and typed-clickhouse:

- **Diffs** your declared schema against a live database and generates a migration plan
- **Applies** that plan to bring the database in line with your code
- **Queries** ClickHouse through a typed layer generated from the same schema

There is no runtime, no streaming layer, and no workflow orchestrator here — just
schema-as-code and migrations for ClickHouse.

## How it works

1. Model your tables and views as TypeScript classes/types in your project.
2. Run the CLI to compare that model against a live ClickHouse database.
3. Review the generated migration plan.
4. Apply it, and query the result through the generated typed client.

## Releasing

`release.yaml` builds and publishes on push of a `v*.*.*` tag (or via manual
dry run). It cannot complete a real publish yet — before the first release,
this repository still needs:

- **An `NPM_TOKEN` secret.** `build-and-publish-binaries`, `publish-wrapper`,
  and `publish-library` all authenticate to the npm registry with
  `secrets.NPM_TOKEN`, which does not exist in this repository yet. It must
  be added under repo Settings → Secrets and variables → Actions before any
  tag push can publish.
- **`THIRD-PARTY-NOTICES.md` generated at publish time**, not committed
  stale. The published artifacts redistribute other people's code — the Rust
  binary statically links its full dependency tree, and tsup bundles
  `commander` into `dist/` — so the notices must be regenerated (e.g. with
  `cargo about`/`cargo-deny` for Rust and a license checker for npm) as part
  of the release, not carried over from a previous version.
- **The `@typed-clickhouse` npm scope claimed** by the publishing account.
  `publish-wrapper` and `publish-library` publish under `@typed-clickhouse/*`
  with `--access public`; that scope has not been registered on npm yet, and
  the first publish will fail until it is.

## License

typed-clickhouse is open source software, MIT licensed. See [`LICENSE`](LICENSE)
for the license text and [`NOTICE`](NOTICE) for attribution — this project is
derived from [514-labs/moosestack](https://github.com/514-labs/moosestack).
