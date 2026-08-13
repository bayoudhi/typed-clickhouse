[![npm (core)](https://img.shields.io/npm/v/@typed-clickhouse/core.svg?label=%40typed-clickhouse%2Fcore)](https://www.npmjs.com/package/@typed-clickhouse/core)
[![npm (cli)](https://img.shields.io/npm/v/@typed-clickhouse/cli.svg?label=%40typed-clickhouse%2Fcli)](https://www.npmjs.com/package/@typed-clickhouse/cli)
[![MIT license](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

# typed-clickhouse

**typed-clickhouse** is a type-safe, code-first schema and migration tool for
ClickHouse.

You declare your tables and views as TypeScript types. A compiler plugin
derives the ClickHouse schema from those types at build time. The CLI diffs
that schema against a live database, shows you the migration plan, and applies
it.

- **Diff** your declared schema against a live database
- **Review** the generated migration plan before anything runs
- **Apply** it to bring the database in line with your code
- **Query** ClickHouse through a typed SQL layer built from the same types

There is no runtime, no streaming layer, and no workflow orchestrator here —
just schema-as-code and migrations for ClickHouse.

## Packages

| Package | What it is |
| --- | --- |
| [`@typed-clickhouse/core`](packages/core) | The TypeScript library: `OlapTable`, `MaterializedView`, the typed query layer, and the compiler plugin |
| `@typed-clickhouse/cli` | The `typed-clickhouse` CLI, a Rust binary distributed through npm |

## Installation

```bash
npm install @typed-clickhouse/core
npm install -D @typed-clickhouse/cli ts-patch typia typescript
```

The compiler plugin is what turns `new OlapTable<Event>("events")` into a real
schema, so it is not optional. Add it to your `tsconfig.json`:

```json
{
  "compilerOptions": {
    "plugins": [
      { "transform": "@typed-clickhouse/core/compilerPlugin" },
      { "transform": "typia/lib/transform" }
    ]
  }
}
```

Then activate it and build with `tspc` instead of `tsc`:

```bash
npx ts-patch install
npx tspc
```

Full plugin setup, including what the error message looks like when it isn't
wired up, is in the [`@typed-clickhouse/core` README](packages/core/README.md).

## Quick start

**1. Point the CLI at your database.** Create `tch.config.toml` in your project
root:

```toml
language = "typescript"

[clickhouse_config]
host = "localhost"
host_port = 8123
user = "default"
password = ""
db_name = "analytics"
use_ssl = false
```

**2. Declare a table.** By default the CLI reads your models from `app/`:

```typescript
// app/index.ts
import { OlapTable } from "@typed-clickhouse/core";

interface Event {
  id: string;
  createdAt: Date;
  label: string;
}

export const events = new OlapTable<Event>("events", {
  orderByFields: ["id", "createdAt"],
});
```

**3. See what would change:**

```bash
npx tspc
npx typed-clickhouse plan
```

**4. Apply it:**

```bash
npx typed-clickhouse migrate
```

`plan` and `migrate` both accept `--clickhouse-url` if you'd rather pass the
connection inline than put it in the config file — useful in CI, and the way
to keep credentials out of the repository.

## Working from an existing database

If the tables already exist and you want them described in TypeScript rather
than created from it, `db pull` writes their definitions into your project:

```bash
npx typed-clickhouse db pull --clickhouse-url clickhouse://user:pass@host:9440/db
```

Definitions land in `app/externalModels.ts` by default. Pulled tables are
marked externally managed, which means `migrate` will never create or drop
them — the database stays the owner of their existence.

## Commands

| Command | What it does |
| --- | --- |
| `plan` | Show the changes the next `migrate` would apply |
| `migrate` | Apply the migration plan to the database |
| `check` | Check the project for non-runtime errors |
| `build` | Build the project |
| `ls` | List the infrastructure the project declares |
| `peek <table>` | Show a few rows from a table |
| `query` | Run SQL against ClickHouse |
| `seed clickhouse` | Copy rows from another ClickHouse into your tables |
| `truncate` | Delete all rows, or the last N rows, from tables |
| `db pull` | Import existing table definitions into your project |
| `generate migration` | Write the migration plan to files |
| `generate hash-token` | Generate an API key hash and bearer token pair |
| `logs` | View the CLI logs |

Run `typed-clickhouse <command> --help` for the flags on any of these.

## Configuration

`tch.config.toml` in the project root configures the CLI. Every value can also
be set through the environment with the `TCH_` prefix — for example
`TCH_CLICKHOUSE_CONFIG__HOST`.

Projects carried over from the predecessor tool may still have a
`moose.config.toml`; the CLI reads that as a fallback, so renaming it is
optional.

## Documentation

- [`@typed-clickhouse/core` README](packages/core/README.md) — library API,
  compiler plugin setup, and serverless configuration
- [`MIGRATION.md`](MIGRATION.md) — moving a project over from the predecessor
  tool
- [`AGENTS.md`](AGENTS.md) — building this repository, running the tests, and
  the release process

## License

typed-clickhouse is open source software, MIT licensed. See [`LICENSE`](LICENSE)
for the license text and [`NOTICE`](NOTICE) for attribution — this project is
derived from [514-labs/moosestack](https://github.com/514-labs/moosestack).
