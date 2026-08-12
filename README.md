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

- ~~**An `NPM_TOKEN` secret.**~~ Configured. `build-and-publish-binaries`,
  `publish-wrapper` and `publish-library` authenticate to the npm registry
  with `secrets.NPM_TOKEN`.
- ~~**`THIRD-PARTY-NOTICES.md` generated at publish time.**~~ Implemented.
  The `third-party-notices` job runs `scripts/gen-third-party-notices.sh` on
  every run (including dry runs), which uses `cargo-about` to collect
  license text for every crate statically linked into the Rust binary and
  resolves the license for `commander` (the one npm dependency tsup bundles
  into `packages/core/dist/`). The result is uploaded as a build artifact and
  downloaded by both `build-and-publish-binaries` (platform packages) and
  `publish-library` (`@typed-clickhouse/core`) before they publish — it's in
  the `files` allowlist of `packages/core/package.json` and
  `apps/cli-npm/package.json.tmpl`, so each release regenerates it fresh
  rather than carrying over a stale copy.
- **The `@typed-clickhouse` npm scope claimed** by the publishing account.
  `publish-wrapper` and `publish-library` publish under `@typed-clickhouse/*`
  with `--access public`; that scope has not been registered on npm yet, and
  the first publish will fail until it is.

npm provenance (`NPM_CONFIG_PROVENANCE`) is deliberately left unset in all
three publishing jobs while this repository is private. npm rejects
provenance attestations from a private source repository with `422
Unprocessable Entity`, and that failure previously burned a version number
on the predecessor project after some artifacts had already published. Set
`NPM_CONFIG_PROVENANCE: "true"` in `build-and-publish-binaries`,
`publish-wrapper`, and `publish-library` once this repository is made
public — provenance is genuinely worth having back at that point.

## License

typed-clickhouse is open source software, MIT licensed. See [`LICENSE`](LICENSE)
for the license text and [`NOTICE`](NOTICE) for attribution — this project is
derived from [514-labs/moosestack](https://github.com/514-labs/moosestack).
