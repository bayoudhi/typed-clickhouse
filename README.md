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

## License

typed-clickhouse is open source software, MIT licensed. See [`LICENSE`](LICENSE)
for the license text and [`NOTICE`](NOTICE) for attribution — this project is
derived from [514-labs/moosestack](https://github.com/514-labs/moosestack).
