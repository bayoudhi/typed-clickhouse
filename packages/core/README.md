# @typed-clickhouse/core

The TypeScript library for **typed-clickhouse**, a type-driven, ClickHouse-only
schema and query toolkit. You declare ClickHouse tables and views as
TypeScript types; a compiler plugin derives their schema from those types at
build time; the `typed-clickhouse` CLI diffs that schema against a live
database, generates a migration plan, and applies it. This package also
provides a typed SQL query layer for reading and writing that data.

It has no native (C++/Rust) dependencies, so it works in AWS Lambda, Edge
runtimes, and other environments where native addons can't be compiled or
loaded.

## Installation

```bash
npm install @typed-clickhouse/core
```

If you use `OlapTable<T>`, `MaterializedView<T>`, or other generic resources
that require compile-time schema injection, you also need the compiler
plugin's peer dependencies:

```bash
npm install -D ts-patch typia typescript
```

## Compiler Plugin Setup

The library's compiler plugin transforms generic resource declarations like
`new OlapTable<MyType>(...)` at compile time, injecting JSON schemas, column
definitions, and runtime validators. Without it, you'll get:

```
Supply the type param T so that the schema is inserted by the compiler plugin.
```

### 1. Configure `tsconfig.json`

Add the compiler plugin and typia transform to your `tsconfig.json`:

```json
{
  "compilerOptions": {
    "plugins": [
      {
        "transform": "@typed-clickhouse/core/compilerPlugin"
      },
      {
        "transform": "typia/lib/transform"
      }
    ]
  }
}
```

Both `@typed-clickhouse/core/compilerPlugin` and
`@typed-clickhouse/core/dist/compilerPlugin.js` work — use whichever you
prefer.

### 2. Install ts-patch

```bash
npx ts-patch install
```

### 3. Build with `tspc` instead of `tsc`

```bash
npx tspc
```

Or add it to your `package.json` scripts:

```json
{
  "scripts": {
    "build": "tspc"
  }
}
```

> **Note**: `tspc` is a drop-in replacement for `tsc` that loads the compiler
> plugins defined in `tsconfig.json`. Standard `tsc` ignores the `plugins`
> array.

## Usage

```typescript
import { OlapTable, sql } from "@typed-clickhouse/core";

interface Event {
  id: string;
  createdAt: Date;
  label: string;
}

// Compiler plugin injects the schema from the `Event` type param at build time.
const events = new OlapTable<Event>("events", {
  orderByFields: ["id", "createdAt"],
});

await events.insert([{ id: "1", createdAt: new Date(), label: "signup" }]);

const recent = sql`SELECT * FROM ${events} ORDER BY createdAt DESC LIMIT 10`;
```

Both CommonJS (`require`) and ES Modules (`import`) builds are published —
use whichever your runtime targets.

## ClickHouse Configuration for Serverless

In a standard project, ClickHouse connection details are read from
`tch.config.toml`. This file doesn't exist in serverless environments, so
calling `OlapTable.insert()` would throw a `ConfigError`.

Use `configureClickHouse()` to provide connection details programmatically.
Call it **once** during cold start, before any `.insert()` calls:

```typescript
import { configureClickHouse, OlapTable } from "@typed-clickhouse/core";

// Call once at module level (runs during Lambda cold start)
configureClickHouse({
  host: process.env.CLICKHOUSE_HOST!,
  port: process.env.CLICKHOUSE_PORT!,       // string, e.g. "8443"
  username: process.env.CLICKHOUSE_USER!,
  password: process.env.CLICKHOUSE_PASSWORD!,
  database: process.env.CLICKHOUSE_DATABASE, // optional — only if your OlapTable configs don't specify `database`
  useSSL: true,
});

// Define your table (compiler plugin injects schema at build time)
const myTable = new OlapTable<MyType>("my_table");

export async function handler(event: any) {
  const data = parseEvent(event);
  await myTable.insert([data]);  // Works without tch.config.toml
  return { statusCode: 200 };
}
```

### `ClickHouseConfig` fields

| Field | Type | Example |
| --- | --- | --- |
| `host` | `string` | `"clickhouse.example.com"` |
| `port` | `string` | `"8443"` |
| `username` | `string` | `"default"` |
| `password` | `string` | `"secret"` |
| `database` | `string?` | `"my_database"` (optional — only needed if your OlapTable configs don't specify `database`) |
| `useSSL` | `boolean` | `true` |

## What's Included

| Export | Description |
| --- | --- |
| `OlapTable` | Define ClickHouse OLAP tables |
| `View`, `MaterializedView` | Define views and materialized views |
| `SqlResource` | Define arbitrary raw-SQL-backed resources |
| `sql`, `Sql` | Tagged template literal (and its AST type) for building SQL queries |
| `select`, `where`, `join`, `groupBy`, `having`, `orderBy`, `limit`, `offset`, `paginate` | Query-builder helpers on top of the SQL layer |
| `and`, `or`, `not`, `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `like`, `ilike`, `inList`, `notIn`, `isNull`, `isNotNull`, `between` | Filter/condition builders |
| `count`, `countDistinct`, `sum`, `avg`, `min`, `max` | Aggregation helpers |
| `defineQueryModel`, `QueryClient`, `createQueryHandler`, `registerModelTools` | Typed query-model layer, including MCP tool generation |
| `SelectRowPolicy`, `buildRowPolicyOptionsFromClaims`, `MOOSE_RLS_ROLE`, `MOOSE_RLS_USER`, `MOOSE_RLS_SETTING_PREFIX` | Row-level security policies |
| `LifeCycle`, `ClickHouseEngines` | Table lifecycle and ClickHouse engine constants |
| `DataSource` | Base class for external data source connectors |
| `WebApp` | Mount a generic HTTP handler alongside your ClickHouse resources |
| `configureClickHouse` | Provide ClickHouse connection config programmatically (no `tch.config.toml`) — see [above](#clickhouse-configuration-for-serverless) |
| `getClickhouseClient` | Get the underlying ClickHouse client |
| `parseCSV`, `parseJSON`, `parseJSONWithDates` | Data parsing utilities |
| `mooseEnvSecrets`, `mooseRuntimeEnv` | Secrets and runtime environment helpers |
| Registry functions | `getTables`, `getTable`, `getViews`, `getView`, `getMaterializedViews`, `getMaterializedView`, `getSqlResources`, `getSqlResource`, `getWebApps`, `getWebApp`, `getSelectRowPolicies`, `getSelectRowPolicy` |
| Utility functions | `cliLog`, `compilerLog`, `mapTstoJs`, `getFileName`, `logError`, `quoteIdentifier` |

This is a representative subset. See
[`tests/golden/exports.json`](./tests/golden/exports.json) for the exact,
generated list of every runtime export.

## No Native Dependencies

This package — and the internal workspace library it bundles — has no native
(C++/Rust) dependencies. There is no streaming layer, no workflow
orchestrator, and no message-broker or job-scheduler client anywhere in this
project; it ships ClickHouse tooling only. That means there is nothing here
that can fail to compile or load in Lambda, Edge, or other restricted
runtimes.

## typed-clickhouse CLI Compatibility

This package ships the `tch-tspc` and `tch-runner` binaries that the
`typed-clickhouse` CLI uses to compile and run your TypeScript models.

The CLI itself lives in [`apps/cli`](https://github.com/bayoudhi/typed-clickhouse/tree/main/apps/cli)
of this same repository. Install it separately (build it from source, or use
whatever distribution channel your project relies on), then use it as normal:

### CI/CD Usage

```yaml
# GitHub Actions example
steps:
  - run: npm install
  - run: typed-clickhouse generate migration
  - run: typed-clickhouse migrate
```

The `tch-tspc` and `tch-runner` binaries are automatically available in
`node_modules/.bin/` after `npm install` — the `typed-clickhouse` CLI will
find them there.

No extra configuration is needed beyond the standard
[Compiler Plugin Setup](#compiler-plugin-setup) above.

## Origin

This package is derived from [514-labs/moosestack](https://github.com/514-labs/moosestack), MIT
licensed, Copyright (c) 2023 Tim Delisle, Nicolas Joseph. See [`NOTICE`](../../NOTICE) for the
full attribution.

- **Original project**: https://github.com/514-labs/moosestack

## License

MIT — see [LICENSE](./LICENSE) for details.
