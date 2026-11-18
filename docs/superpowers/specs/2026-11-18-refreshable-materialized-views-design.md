# Refreshable materialized views

Date: 2026-11-18
Status: approved, not yet implemented

## Why

ClickHouse has two kinds of materialized view. The incremental kind runs its
SELECT on every insert into its source table. The refreshable kind
(`REFRESH EVERY …` / `REFRESH AFTER …`) re-runs its whole SELECT on a schedule
and atomically replaces the target's contents. typed-clickhouse only knows the
first.

The typical use is a reporting snapshot of an expensive join-heavy view,
rebuilt once a day for a BI tool. Hand-made, such a view fails in a known way.
`SELECT *` freezes its column list at CREATE time. A later deploy adds a column
to the source view, and from then on every refresh fails, while the old rows
stay readable and nothing says so. Declaring the view in code lets schema
changes reach it on deploy.

What 0.2.1 does with one today, measured against ClickHouse 25.8:

- **Refreshable view without `TO`:** the first `migrate` succeeds. Every later
  `generate` then fails with `Could not find TO target in materialized view
  definition`, because `reconstruct_sql_resource_from_mv`
  (`apps/cli/src/infrastructure/olap/clickhouse/mod.rs`) requires a `TO`
  clause on every view in the database.
- **Refreshable view with `TO`, written as a `SqlResource`:** the read-back
  rebuilds the DDL as `CREATE MATERIALIZED VIEW IF NOT EXISTS … TO … AS …`
  with no `REFRESH` clause. It never equals the code side, so every plan drops
  and fully rebuilds the view. Removing the resource from code leaves the view
  in the database.
- **The structured `MaterializedView`:** it has nowhere to put a schedule.
  Neither the proto, the Rust struct nor the TS config has a field for it.

## Goals

1. `MaterializedView` takes an optional `refresh` config. With it, the CLI
   creates a refreshable view.
2. When nothing changed, the plan for a refreshable view is empty. This is
   proven against a live server, the same way `live_reality_roundtrip_plans_nothing` proves it for everything else.
3. A schedule-only change is applied with `ALTER TABLE … MODIFY REFRESH …`,
   with no drop and no rebuild.
4. A SELECT change drops and recreates only the view. The target table and its
   rows survive, so readers never see an empty table between the drop and the
   first refresh.
5. A view without `TO` that already exists in the database no longer fails the
   plan. It is logged as a warning and left alone.

## Non-goals

- `DEPENDS ON` and refresh-level `SETTINGS` (`refresh_retries` and the like).
  Nothing needs them yet. The parser must still recognise them, so that a view
  using them hand-written is reported as a change rather than misread (see
  Constraints).
- Refreshable views without a `TO` target. The library already requires a
  target table for every materialized view, and that stays.
- Triggering or monitoring refreshes (`SYSTEM REFRESH VIEW`,
  `system.view_refreshes`). Those belong to the operator.

## Anti-goals

- A refreshable view that is rebuilt on every deploy. The empty plan is the
  feature.
- Misreading a `REFRESH` clause as "no refresh". That would plan a drop that
  turns a scheduled view into an insert-triggered one. Unknown or unparseable
  refresh clauses must come back as a difference, never as equality.
- Any change in behaviour for views without `refresh`. Their DDL, comparison
  and backfill stay byte-for-byte the same, and the golden schema baseline must
  not move.

## Constraints

- **The ClickHouse grammar**
  (`CREATE MATERIALIZED VIEW … REFRESH EVERY|AFTER <interval> [OFFSET <interval>]
  [RANDOMIZE FOR <interval>] [DEPENDS ON …] [SETTINGS …] [APPEND] TO …`).
  `OFFSET` is valid only with `EVERY`. `APPEND` comes after the refresh clause
  and before `TO`.
- **Read-back source.** The server keeps the clause in
  `system.tables.create_table_query`, for example
  `CREATE MATERIALIZED VIEW db.mv REFRESH EVERY 1 DAY OFFSET 10 HOUR TO db.t (…) AS SELECT …`.
  `as_select` does not include it. `system.view_refreshes` does not expose the
  schedule text.
- **Normalisation.** The server may rewrite the interval spelling (case, plural
  units). Compare parsed values, never strings. Whether the server rewrites
  `24 HOUR` as `1 DAY` must be checked against the live server; if it does not,
  they compare unequal, and that is correct.
- **AGENTS.md rules.** `thiserror` errors, no `anyhow`, `cargo clippy -D
  warnings`, inline tests, and the golden gate (`packages/core/tests/golden.test.sh`).

## Approaches

- **A. A `refresh` option on `MaterializedView`.** Chosen. ClickHouse
  treats a refreshable view as the same statement with one more clause, so the
  stored state, the proto and the diff stay one type.
- **B. A separate `RefreshableMaterializedView` class.** Its name would say
  which behaviour you get, and it could never pick up the dev-mode backfill.
  The cost is a second type through the proto, the diff and the ordering.
  Rejected for that cost.
- **C. Leave it to `SqlResource`.** No library change, but it is the
  rebuild-every-deploy behaviour above. Rejected.

## The design

### TypeScript (`packages/lib/src/dmv2/sdk/materializedView.ts`)

```ts
type IntervalUnit = 'SECOND' | 'MINUTE' | 'HOUR' | 'DAY' | 'WEEK' | 'MONTH' | 'YEAR';
type RefreshInterval = `${number} ${IntervalUnit}`;

type RefreshConfig =
  | { every: RefreshInterval; offset?: RefreshInterval; randomizeFor?: RefreshInterval; append?: boolean }
  | { after: RefreshInterval; randomizeFor?: RefreshInterval; append?: boolean };

interface MaterializedViewConfig<T> {
  // …existing fields…
  refresh?: RefreshConfig;
}
```

- The union puts `offset` on the `every` branch only, so the compiler rejects
  `OFFSET` with `AFTER`.
- The constructor validates the interval strings at runtime too (a positive
  integer, then a unit), because the template-literal type accepts `1.5 DAY`
  and `-1 DAY`. Units are upper-cased before validation.
- `MaterializedView` exposes `refresh: RefreshConfig | undefined`.
  `MaterializedViewJson` in `internal.ts` gains `refresh`, and it is serialised
  only when it is set, so the JSON for views without `refresh` is unchanged.
- `selectTables` keeps its meaning (dependency ordering). For a refreshable
  view it no longer means "the table whose inserts trigger it", and the JSDoc
  says so.

### Stored state (proto and Rust)

`packages/protobuf/infrastructure_map.proto`:

```proto
message RefreshInterval { uint64 value = 1; string unit = 2; }
message RefreshSchedule {
  oneof kind { RefreshInterval every = 1; RefreshInterval after = 2; }
  optional RefreshInterval offset = 3;
  optional RefreshInterval randomize_for = 4;
  bool append = 5;
}
message MaterializedView { … optional RefreshSchedule refresh = 7; }
```

The Rust `MaterializedView`
(`apps/cli/src/framework/core/infrastructure/materialized_view.rs`) gains
`refresh: Option<RefreshSchedule>`, with `#[serde(default,
skip_serializing_if = "Option::is_none")]`, so existing stored state and
existing JSON deserialise unchanged. `RefreshSchedule` and `RefreshInterval`
are structured types with `Display` (renders the SQL clause), `FromStr`/parse,
and `to_proto`/`from_proto`. A unit is a closed enum, and `Display` writes
the canonical upper-case singular form.

### DDL

- `MaterializedView::to_create_sql` and `execute_create_materialized_view`
  (`clickhouse/mod.rs`) insert `REFRESH <schedule> [APPEND]` between the name
  and `TO` when `refresh` is set. The output for views without `refresh` is
  unchanged.
- New atomic operation `ModifyMaterializedViewRefresh { mv, dependency_info }`
  in `ddl_ordering.rs`, with its `SerializableOlapOperation` counterpart and
  its entry in `utilities/migration_plan_schema.json`. It executes
  `ALTER TABLE <mv> MODIFY REFRESH <schedule>`.
- A change to `append` always takes the drop-and-create path. `MODIFY REFRESH`
  is used for the schedule only, which avoids depending on how a given server
  version treats `APPEND` there.

### Read-back (`clickhouse/mod.rs`, `infra_reality_checker.rs`)

- `reconstruct_sql_resource_from_mv`: when the view has no `TO`, return "skip
  this object" instead of an error. Log `warn!` with its name, and leave it
  out of the reality map, as an external object. This fixes Goal 5 for every
  user, not just refreshable views.
- Parse the refresh clause out of `create_table_query`, between the view name
  and `TO`. Keep it in the reconstructed setup text
  (`CREATE MATERIALIZED VIEW IF NOT EXISTS <name> REFRESH … [APPEND] TO …
  AS …`).
- `parse_create_materialized_view` (`sql_parser.rs`) learns the refresh
  clause and returns it. `try_migrate_sql_resource_to_mv` puts it on the
  structured view, so the reality `MaterializedView` has `refresh` set.
  - Consequence, intended: a single-statement `SqlResource` in the library
    pattern that contains `REFRESH` now converts to a structured refreshable
    view, where it used to be rebuilt on every plan.
  - Multi-statement resources, such as a `DROP` + `CREATE` pair, still do not
    convert. That is unchanged.
- A clause using `DEPENDS ON` or `SETTINGS`, or one that fails to parse,
  yields a schedule that never compares equal to any code-side config. The
  plan then shows a drop and create, which is visible and harmless, and never
  a silent match (see Anti-goals).

### Comparison and diff (`infra_reality_checker.rs`, `infrastructure_map.rs`)

- `materialized_views_are_equivalent`:
  - Compares `refresh` by value.
  - When **either** side has `refresh`, skips `source_tables`, as
    `views_are_equivalent` already does. The reason: for a refreshable view
    the source list is only a dependency hint, and the read-back parser
    recovers a different set from joins and subqueries.
  - Views without `refresh` are compared exactly as today.
- `diff_materialized_views`: when two views differ **only** in the schedule
  (`every`/`after` value, `offset`, `randomize_for`), emit
  `ModifyMaterializedViewRefresh`. Any other difference, including adding or
  removing `refresh` and changing the select, target or `append`, stays on the existing drop-and-create path.
- `check_materialized_view_population` (`diff_strategy.rs`) and its MV caller
  in `diff_materialized_views`: skip population when `refresh` is set.
  ClickHouse runs the first refresh at CREATE.
- Ordering: dropping and recreating the view still happens around the target
  table's column ALTERs, through the existing ordering. Nothing new.

### Tests

- **Rust unit:**
  - Parse/`Display` round-trip for every clause combination.
  - Parsing server spellings (lower case, plural units).
  - `OFFSET` with `AFTER` is rejected.
  - Equivalence: a refresh difference counts; `source_tables` are ignored only
    when `refresh` is set.
  - A diff that differs only in schedule yields a single
    `ModifyMaterializedViewRefresh`.
  - A `DEPENDS ON` clause read back never equals a code-side config.
  - A view without `TO` is skipped with a warning.
- **TS:**
  - `refresh` serialises into the infra map JSON.
  - JSON without `refresh` is byte-identical to today's.
  - Runtime interval validation.
  - The golden gate passes unchanged.
- **Live (`reality_roundtrip_live.rs`, `TC_LIVE_CLICKHOUSE=1`):**
  - Extend the fixture with a refreshable view (`EVERY 1 DAY OFFSET 10 HOUR`,
    with a `TO` target and a join in its SELECT). The existing
    `live_reality_roundtrip_plans_nothing` must still plan nothing.
  - Schedule change: exactly one `ModifyMaterializedViewRefresh`, applied
    successfully. Then plan nothing.
  - Select change: a drop and create of the view, with target rows present
    afterwards.
  - A view without `TO` created by hand in the test database: the plan
    succeeds.
  - Record whether the server rewrites `24 HOUR` as `1 DAY`.

### Release

The next release after 0.2.1, through the repo's existing release workflow.
Document `refresh` in `packages/core/README.md` with one example.

## Left to the builder

- Module and file placement for the refresh types. Parser technique: regex or
  `sqlparser`, whichever handles the server's output.
- Plan-output wording for the new operation.
