# Refreshable Materialized Views Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `MaterializedView` declare a ClickHouse `REFRESH` schedule. A deploy
with no change must then plan nothing, a schedule-only change must apply with
`MODIFY REFRESH`, and a select change must recreate only the view.

**Architecture:**
- A new `refresh` module in the CLI owns the schedule types, their SQL rendering
  and a tolerant parser.
- The schedule travels on the structured `MaterializedView` through:
  - the TS JSON,
  - serde and proto,
  - DDL,
  - the read-back from `system.tables.create_table_query`,
  - the equivalence check.
- `handle_materialized_view_update` picks `ModifyMaterializedViewRefresh` over
  drop-and-create when only the schedule differs.

**Tech Stack:**
- Rust CLI (`apps/cli`): forked `sqlparser`, `rust-protobuf`, `serde`, `thiserror`.
- TypeScript library (`packages/lib`): mocha/chai tests.
- Published package `packages/core`: golden gate.
- Live tests: ClickHouse 25.8 via `docker-compose.test.yml`.

**Spec:** `docs/superpowers/specs/2026-11-18-refreshable-materialized-views-design.md`

## Global Constraints

- Views without `refresh` must not change:
  - their DDL, JSON, proto bytes and `plan.yaml` output stay byte-identical;
  - the golden gate (`packages/core/tests/golden.test.sh`) passes unchanged.
- Supported clause: `REFRESH EVERY|AFTER <n> <UNIT> [OFFSET <n> <UNIT>]
  [RANDOMIZE FOR <n> <UNIT>]`, then `[APPEND]`.
  - `OFFSET` is allowed only with `EVERY`.
  - Units: `SECOND MINUTE HOUR DAY WEEK MONTH YEAR`.
- `DEPENDS ON` and refresh `SETTINGS` are out of scope. When they are read back
  they must never compare equal to a code-side config.
- An unparseable refresh clause read from the server must produce a visible
  plan difference. It must never be treated as equal or as "no refresh".
- A materialized view in the database with no `TO` gets a `warn!` and is left
  out of reality. It must not fail the plan.
- Errors use `thiserror` near the failing unit, never `anyhow`.
- Every task ends green on all of:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --check`
- Tasks touching TS also pass:
  - `pnpm --filter @typed-clickhouse/lib test`
  - `pnpm --filter @typed-clickhouse/lib typecheck`
- This is a public repo. No names, tables, tickets or business context from any
  private consumer go in code, tests, fixtures, docs or commit messages. Use
  neutral fixtures (`events`, `devices`, `sensor_readings`, `mv_*`).

## Review Focus

1. **Spelling the server writes back.** It may come back lower-case, with plural
   units (`10 HOURS`) or with extra spaces. These must parse to the same schedule
   as the code side. Task 1 pins this.
2. **`REFRESH` or `TO` inside the SELECT.** A column named `refresh` or a string
   literal `' TO '` inside the SELECT must not be taken for the clause. The split
   only scans between the view name and the top-level `TO`. Task 1 pins this.
3. **Stored state from 0.2.1.** A proto `MaterializedView` without field 7, and
   JSON without `refresh`, must load with `refresh: None`. Task 2 pins this.
4. **Adding `refresh` to an existing incremental view, or removing it.** That is
   a drop and create, never `MODIFY REFRESH`, because ClickHouse cannot alter a
   view between kinds. Task 4 pins this.
5. **Quoted and qualified names in `create_table_query`.** For example
   `` CREATE MATERIALIZED VIEW `db`.`mv` REFRESH … TO `db`.`t` (…) AS … ``.
   These must split and parse. Tasks 1 and 3 pin this.

---

### Task 1: Refresh schedule types, rendering and parser

**Files:**
- Create: `apps/cli/src/framework/core/infrastructure/refresh.rs`
- Modify: `apps/cli/src/framework/core/infrastructure/mod.rs` (add `pub mod refresh;`)
- Add (commit with this task): the spec, and this plan.

**Interfaces:**
- Produces:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum IntervalUnit { Second, Minute, Hour, Day, Week, Month, Year }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RefreshInterval { pub value: u64, pub unit: IntervalUnit }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefreshKind { Every, After }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshSchedule {
    pub kind: RefreshKind,
    pub interval: RefreshInterval,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<RefreshInterval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub randomize_for: Option<RefreshInterval>,
    #[serde(default)]
    pub append: bool,
    /// Reality-only: clause text this module does not model (DEPENDS ON,
    /// SETTINGS, or a clause that failed to parse). Code never sets it, so a
    /// schedule carrying it never equals a code-side one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmodelled: Option<String>,
}

impl RefreshSchedule {
    /// The clause without APPEND: `REFRESH EVERY 1 DAY OFFSET 10 HOUR`.
    pub fn schedule_sql(&self) -> String;
    /// `schedule_sql()` plus ` APPEND` when set: what goes between name and TO.
    pub fn to_create_clause(&self) -> String;
    /// Placeholder for a clause that did not parse; `unmodelled = Some(raw)`.
    pub fn unparsed(raw: &str) -> Self;
    /// True when the two differ only in kind/interval/offset/randomize_for.
    pub fn differs_only_in_schedule(&self, other: &Self) -> bool;
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RefreshParseError { /* variants for: missing EVERY/AFTER, bad interval,
                                 unknown unit, OFFSET with AFTER */ }

/// Parses `REFRESH …` (with or without trailing APPEND), case-insensitive,
/// singular or plural units.
pub fn parse_refresh_clause(clause: &str) -> Result<RefreshSchedule, RefreshParseError>;

/// Splits a CREATE MATERIALIZED VIEW statement into the statement with the
/// refresh clause (and APPEND) removed, and the parsed schedule. Unparseable
/// clause -> `Some(RefreshSchedule::unparsed(raw))`. No REFRESH -> `(sql, None)`.
/// Scans only between the view name and the first top-level ` TO `.
pub fn split_refresh_clause(create_sql: &str) -> (String, Option<RefreshSchedule>);
```
`differs_only_in_schedule` is false when either side has `unmodelled` set, or
when `append` differs.

- [ ] **Step 1: Write failing tests** in `refresh.rs` `#[cfg(test)] mod tests`:
  - `renders_every_with_offset`:
    `schedule_sql()` == `"REFRESH EVERY 1 DAY OFFSET 10 HOUR"`.
  - `renders_after_randomize_append`:
    `to_create_clause()` == `"REFRESH AFTER 30 MINUTE RANDOMIZE FOR 5 MINUTE APPEND"`.
  - `parse_round_trips_every_combination`: for each combination of kind,
    offset, randomize_for and append,
    `parse_refresh_clause(&s.to_create_clause()) == Ok(s)`.
  - `parses_server_spelling`: `"refresh every 1 day offset 10 hours"` parses
    equal to `EVERY 1 DAY OFFSET 10 HOUR`.
  - `rejects_offset_with_after`: `"REFRESH AFTER 1 HOUR OFFSET 5 MINUTE"` is an
    `Err`.
  - `depends_on_is_unmodelled`: splitting
    `"CREATE MATERIALIZED VIEW mv REFRESH EVERY 1 HOUR DEPENDS ON other TO t AS SELECT 1"`
    yields a schedule with `unmodelled.is_some()`.
  - `split_qualified_quoted`: splitting
    `` "CREATE MATERIALIZED VIEW `db`.`mv` REFRESH EVERY 1 DAY OFFSET 10 HOUR TO `db`.`t` (`id` String) AS SELECT id FROM src" ``
    yields
    `` "CREATE MATERIALIZED VIEW `db`.`mv` TO `db`.`t` (`id` String) AS SELECT id FROM src" ``
    and the `EVERY 1 DAY OFFSET 10 HOUR` schedule.
  - `split_ignores_select_body`: splitting
    `"CREATE MATERIALIZED VIEW mv TO t AS SELECT refresh, ' TO ' AS x FROM s"`
    yields `(same sql, None)`.
  - `split_unparseable_is_visible`: `"… REFRESH SOMETIMES TO t AS …"` yields
    `Some(s)` with `s.unmodelled.is_some()`.
  - `differs_only_in_schedule_cases`:
    - true for `1 DAY` vs `2 DAY`;
    - false when `append` differs;
    - false when either side has `unmodelled`.

- [ ] **Step 2: Run and see them fail.**
  Run: `cargo test -p typed-clickhouse refresh::`
  .
  Expected: compile errors / FAIL.

- [ ] **Step 3: Implement the types and functions above.**
  - A hand-written tokenizer over the clause is enough. The forked `sqlparser`
    has no ClickHouse `REFRESH` grammar, so it cannot be used here.
  - `split_refresh_clause` finds the first case-insensitive `REFRESH` token and
    the first top-level `TO` token after it, both before the first top-level
    ` AS `. Backtick and quote state must be honoured.

- [ ] **Step 4: Run the tests and see them pass,** plus clippy and fmt.
  Run: `cargo test -p typed-clickhouse refresh:: && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
  Expected: PASS, no warnings.

- [ ] **Step 5: Commit**
```bash
git add apps/cli/src/framework/core/infrastructure/refresh.rs apps/cli/src/framework/core/infrastructure/mod.rs \
  docs/superpowers/specs/2026-11-18-refreshable-materialized-views-design.md \
  docs/superpowers/plans/2026-11-18-refreshable-materialized-views.md
git commit -m "feat(cli): add refresh schedule types and parser for refreshable MVs"
```

---

### Task 2: Carry `refresh` on `MaterializedView`: serde, proto, DDL

**Files:**
- Modify: `packages/protobuf/infrastructure_map.proto` (messages per spec, and
  `optional RefreshSchedule refresh = 7;` on `MaterializedView`; add
  `optional string unmodelled = 6;` to `RefreshSchedule`)
- Modify: `apps/cli/src/framework/core/infrastructure/materialized_view.rs`
- Modify: every struct literal of `MaterializedView { … }` that the compiler
  flags (add `refresh: None`)

**Interfaces:**
- Consumes: Task 1 `RefreshSchedule`, `to_create_clause()`.
- Produces:
  - `MaterializedView.refresh: Option<RefreshSchedule>`, with
    `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `RefreshSchedule::to_proto(&self) -> ProtoRefreshSchedule`.
  - `RefreshSchedule::from_proto(ProtoRefreshSchedule) -> RefreshSchedule`.
  - `to_create_sql()` emits
    `CREATE MATERIALIZED VIEW IF NOT EXISTS <name> <clause> TO <target> AS <select>`
    when `refresh` is set, and today's exact string otherwise.

- [ ] **Step 1: Write failing tests** in `materialized_view.rs` tests:
  - `create_sql_with_refresh`:
    `` "CREATE MATERIALIZED VIEW IF NOT EXISTS `mv` REFRESH EVERY 1 DAY OFFSET 10 HOUR TO `t` AS SELECT 1" ``.
  - `create_sql_without_refresh_unchanged`: equals today's literal
    `` "CREATE MATERIALIZED VIEW IF NOT EXISTS `mv` TO `t` AS SELECT 1" ``.
  - `proto_round_trip_with_refresh`:
    `from_proto(mv.to_proto()) == mv`, for a schedule with every field set.
  - `proto_without_field_7_loads_none` (Review Focus 3): a
    `ProtoMaterializedView` built without `refresh` gives
    `from_proto(..).refresh == None`.
  - `deserializes_ts_json`:
    `serde_json::from_str::<MaterializedView>` of
    `{"name":"mv","selectSql":"SELECT 1","sourceTables":[],"targetTable":"t","refresh":{"kind":"every","interval":{"value":1,"unit":"DAY"},"offset":{"value":10,"unit":"HOUR"},"append":false}}`
    has the expected schedule.
  - `json_without_refresh_omits_key`: serialising an MV with `refresh: None`
    has no `"refresh"` key.

- [ ] **Step 2: Run, and see them fail.** Run: `cargo test -p typed-clickhouse materialized_view::`

- [ ] **Step 3: Implement.**
  - Add the proto messages.
  - Map them in `to_proto`/`from_proto`, with the proto `oneof kind` mapped to
    `RefreshKind`. A unit string is upper-case singular, and an unknown unit in
    stored state maps to `RefreshSchedule::unparsed`.
  - Add the field and branch `to_create_sql`.
  - Fix the struct literals the compiler flags.

- [ ] **Step 4: Run** `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
  Expected: PASS.

- [ ] **Step 5: Commit**
  Message: `feat(cli): store and render the refresh schedule on MaterializedView`

---

### Task 3: Read back the schedule, and skip views without `TO`

**Files:**
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/sql_parser.rs`
  (`MaterializedViewStatement`, `parse_create_materialized_view`)
- Modify: `apps/cli/src/framework/core/infrastructure_map.rs`
  (`try_migrate_sql_resource_to_mv`)
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/mod.rs`
  (`reconstruct_sql_resource_from_mv`, `list_sql_resources`)

**Interfaces:**
- Consumes: Task 1 `split_refresh_clause`. Task 2 `MaterializedView.refresh`.
- Produces:
  - `MaterializedViewStatement.refresh: Option<RefreshSchedule>`.
  - `try_migrate_sql_resource_to_mv` sets `refresh` from it.
  - `reconstruct_sql_resource_from_mv(...) -> Result<Option<SqlResource>, OlapChangesError>`:
    it returns `Ok(None)` when there is no `TO`, and `list_sql_resources`
    `warn!`s with the view name and skips it.

- [ ] **Step 1: Write failing tests:**
  - In `sql_parser.rs`:
    - `parses_refreshable_mv`: parsing
      `"CREATE MATERIALIZED VIEW IF NOT EXISTS mv REFRESH EVERY 1 DAY OFFSET 10 HOUR TO t AS SELECT a FROM s JOIN u ON s.id = u.id"`
      gives target `t`, the schedule, and a `select_statement` beginning
      `SELECT a FROM s`.
    - `parse_without_refresh_unchanged`: `refresh == None`, all other fields
      as today.
  - In `infrastructure_map.rs`:
    - `migrates_refreshable_sql_resource`: a one-statement resource with the
      `REFRESH` setup above converts to an MV with `refresh.is_some()`.
    - `multi_statement_refresh_resource_not_migrated`: a `DROP` + `CREATE`
      pair returns `None`.
  - In `clickhouse/mod.rs`:
    - `reconstruct_keeps_refresh`: for create_query
      `` "CREATE MATERIALIZED VIEW db.mv REFRESH EVERY 1 DAY OFFSET 10 HOUR TO db.t (`id` String) AS SELECT id FROM db.s" ``,
      the setup contains `REFRESH EVERY 1 DAY OFFSET 10 HOUR TO`.
    - `reconstruct_without_to_is_skipped`: for
      `"CREATE MATERIALIZED VIEW db.mv REFRESH EVERY 1 HOUR ENGINE = MergeTree ORDER BY id AS SELECT 1"`
      it returns `Ok(None)`.

- [ ] **Step 2: Run, and see them fail.**

- [ ] **Step 3: Implement.**
  - `parse_create_materialized_view` calls `split_refresh_clause` first, and
    hands the stripped SQL to `sqlparser`.
  - `reconstruct_sql_resource_from_mv` calls `split_refresh_clause` on
    `create_query`. It returns `None` when `MATERIALIZED_VIEW_TO_PATTERN`
    finds no `TO`, and otherwise builds setup as
    `CREATE MATERIALIZED VIEW IF NOT EXISTS {name} {clause} TO {target} AS {as_select}`,
    leaving out `{clause} ` when there is none.

- [ ] **Step 4: Run** the full Rust gate (`build`, `test`, `clippy`, `fmt`).

- [ ] **Step 5: Commit**
  Message: `fix(cli): read refresh schedules back and skip MVs without a TO target`

---

### Task 4: Compare, diff and execute: equivalence, `ModifyMaterializedViewRefresh`, no backfill

**Files:**
- Modify: `apps/cli/src/framework/core/infra_reality_checker.rs`
  (`materialized_views_are_equivalent`)
- Modify: `apps/cli/src/infrastructure/olap/ddl_ordering.rs`
  (`AtomicOlapOperation`, `handle_materialized_view_update`, the serialisation
  match around line 428 and the dependency match around line 542)
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/mod.rs`:
  - add the `SerializableOlapOperation::ModifyMaterializedViewRefresh` variant;
  - add `refresh` on `CreateMaterializedView`;
  - add the description text and the executor arm;
  - `execute_create_materialized_view` takes `refresh: Option<&RefreshSchedule>`.
- Modify: `apps/cli/src/cli/routines/migrate.rs` (the match at line 357)
- Modify: `apps/cli/utilities/migration_plan_schema.json` (the new op, and
  optional `refresh` on `CreateMaterializedView`)
- Modify: `apps/cli/src/framework/core/infrastructure_map.rs`
  (`check_materialized_view_population` caller, around line 901: skip when
  `refresh.is_some()`)

**Interfaces:**
- Consumes: Task 1 `differs_only_in_schedule`, `schedule_sql()`. Task 2 field.
- Produces:
  - `AtomicOlapOperation::ModifyMaterializedViewRefresh { mv: MaterializedView, dependency_info: DependencyInfo }`.
  - `SerializableOlapOperation::ModifyMaterializedViewRefresh { name: String, database: Option<String>, refresh: RefreshSchedule }`.
    It executes `` ALTER TABLE `<db>`.`<name>` MODIFY <schedule_sql()> ``.
  - `SerializableOlapOperation::CreateMaterializedView` gains
    `#[serde(default, skip_serializing_if = "Option::is_none")] refresh: Option<RefreshSchedule>`.

`materialized_views_are_equivalent` rules:
- Compare `refresh` by value.
- Skip the `source_tables` comparison when either side has `refresh`.
- Change nothing else.

`handle_materialized_view_update` emits a single
`ModifyMaterializedViewRefresh` (setup only) when all three hold:
- both sides have `refresh`;
- `before.refresh.differs_only_in_schedule(after.refresh)` is true;
- the two views are equivalent once `before.refresh` is replaced by
  `after.refresh`.

Every other case takes today's drop and create.

- [ ] **Step 1: Write failing tests:**
  - In `infra_reality_checker.rs`:
    - `refresh_difference_is_not_equivalent`.
    - `refreshable_ignores_source_tables`: same view, `source_tables` `[a]`
      vs `[a, b]`, both with `refresh` → equivalent.
    - `incremental_still_compares_source_tables`: the same pair without
      `refresh` → not equivalent.
  - In `ddl_ordering.rs`:
    - `schedule_only_change_is_modify`: `1 DAY` → `2 DAY` gives exactly one
      setup op `ModifyMaterializedViewRefresh` and no teardown.
    - `select_change_on_refreshable_is_drop_create`.
    - `append_change_is_drop_create`.
    - `adding_refresh_is_drop_create` (Review Focus 4): `None` → `Some`.
    - `removing_refresh_is_drop_create`: `Some` → `None`.
  - In `infrastructure_map.rs`:
    - `refreshable_mv_is_not_populated`: a new refreshable MV in a
      non-production diff adds no population op. A new incremental MV still
      does, as today.
  - In `clickhouse/mod.rs`:
    - `serializable_create_without_refresh_unchanged`: the serde JSON of
      `CreateMaterializedView` with `refresh: None` has no `refresh` key.

- [ ] **Step 2: Run, and see them fail.**

- [ ] **Step 3: Implement** the variants, the executor arm (through the
  existing `run_query`), the description string
  (`"Modifying refresh schedule of materialized view '<name>'"`), the schema
  JSON, the rules above, and the population skip.

- [ ] **Step 4: Run** the full Rust gate.

- [ ] **Step 5: Commit**
  Message: `feat(cli): diff refreshable MVs and apply schedule changes with MODIFY REFRESH`

---

### Task 5: Live round-trip tests against ClickHouse

**Files:**
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/reality_roundtrip_live.rs`

**Interfaces:**
- Consumes: everything from Tasks 2–4. Fixture helpers `fixture(Stage)`,
  `apply`, `plan_against_reality`, `describe`, `reset_database`.

Fixture addition, at `Stage::Views`:
- A target table `device_snapshot` (`id String`, `readings UInt64`,
  `ReplacingMergeTree` `ORDER BY id`).
- A `MaterializedView` `mv_device_snapshot`:
  - `refresh: EVERY 1 DAY OFFSET 10 HOUR`;
  - a select that joins `devices` and `sensor_readings`, grouped by `d.id`;
  - `source_tables` `devices` and `sensor_readings`.

- [ ] **Step 1: Write the tests** (each starts with the `enabled()` guard):
  - `live_reality_roundtrip_plans_nothing`: unchanged test, now with the
    refreshable view in the fixture. It must still assert zero changes.
  - `live_refresh_schedule_change_is_modify_only`:
    1. Apply `full`.
    2. Plan against a copy with `interval = 2 DAY`.
    3. Assert the plan is exactly one `ModifyMaterializedViewRefresh`.
    4. Apply it.
    5. Plan the copy again and assert empty.
    6. Assert `system.tables.create_table_query` contains `EVERY 2 DAY`.
  - `live_refresh_select_change_keeps_target_rows`:
    1. Insert two `devices` rows.
    2. `SYSTEM REFRESH VIEW mv_device_snapshot`, then
       `SYSTEM WAIT VIEW mv_device_snapshot`.
    3. Change the select (add `HAVING readings >= 0`).
    4. Assert the plan is a drop and create of the view only, with no table op.
    5. Apply it.
    6. Assert `SELECT count() FROM device_snapshot` > 0 straight after the
       apply.
  - `live_mv_without_to_does_not_fail_plan`:
    1. Create
       `CREATE MATERIALIZED VIEW stray REFRESH EVERY 1 HOUR ENGINE = MergeTree ORDER BY tuple() AS SELECT 1 AS x`
       by hand.
    2. `plan_against_reality(full)` succeeds and reports no change for
       `stray`.
  - `live_records_server_spelling`:
    1. Create a view with `REFRESH EVERY 24 HOUR`, then read
       `create_table_query`.
    2. Assert it round-trips through `split_refresh_clause` to the same
       schedule.
    3. Record in an assertion message whether the server rewrote `24 HOUR`.

- [ ] **Step 2: Start ClickHouse and run them.**
  ```bash
  docker compose -f docker-compose.test.yml up -d --wait
  TC_LIVE_CLICKHOUSE=1 cargo test live_ -- --test-threads=1
  ```
  Expected: all `live_` tests PASS. If a server spelling breaks the parser,
  fix it in Task 1's parser, and add that spelling to `parses_server_spelling`.

- [ ] **Step 3: Run** the full Rust gate.

- [ ] **Step 4: Commit**
  Message: `test(cli): prove refreshable MVs round-trip against a live server`

---

### Task 6: TypeScript `refresh` option, serialisation and docs

**Files:**
- Modify: `packages/lib/src/dmv2/sdk/materializedView.ts`
- Modify: `packages/lib/src/dmv2/internal.ts` (`MaterializedViewJson`, and the
  MV loop near line 1140)
- Modify: `packages/lib/src/dmv2/index.ts`, plus `browserCompatible.ts` /
  `serverless.ts` if they re-export `MaterializedViewConfig` (export
  `RefreshConfig`, `RefreshInterval`)
- Create: `packages/lib/tests/materialized-view-refresh.test.ts`
- Modify: `packages/core/README.md` (one example)

**Interfaces:**
- Consumes: the Rust JSON shape from Task 2:
  `{ kind: 'every'|'after', interval: { value, unit }, offset?, randomizeFor?, append }`.
- Produces:
```ts
export type IntervalUnit = 'SECOND' | 'MINUTE' | 'HOUR' | 'DAY' | 'WEEK' | 'MONTH' | 'YEAR';
export type RefreshInterval = `${number} ${IntervalUnit}`;
export type RefreshConfig =
  | { every: RefreshInterval; offset?: RefreshInterval; randomizeFor?: RefreshInterval; append?: boolean }
  | { after: RefreshInterval; randomizeFor?: RefreshInterval; append?: boolean };
// MaterializedViewConfig<T>.refresh?: RefreshConfig
// MaterializedView.refresh: RefreshConfig | undefined
```
- The constructor validates each interval with `/^\s*(\d+)\s+([A-Za-z]+?)S?\s*$/i`.
  The value must be a positive integer, and the unit is upper-cased into
  `IntervalUnit`. Anything else throws
  `Invalid refresh interval '<input>' on materialized view '<name>': expected '<positive integer> <SECOND|MINUTE|HOUR|DAY|WEEK|MONTH|YEAR>'`.
- `toInfraMap` adds `refresh` to the MV JSON only when it is set.

- [ ] **Step 1: Write failing tests** in `materialized-view-refresh.test.ts`,
  following `materialized-view-lifecycle.test.ts`'s mock schema/columns
  setup:
  - `serializes every+offset`: for
    `refresh: { every: '1 DAY', offset: '10 HOUR' }`, the infra map MV equals
    `{ kind: 'every', interval: { value: 1, unit: 'DAY' }, offset: { value: 10, unit: 'HOUR' }, append: false }`.
  - `serializes after+randomizeFor+append`.
  - `omits refresh when not set`: `'refresh' in json` is false.
  - `accepts plural and lowercase`: `'10 hours'` → `{ value: 10, unit: 'HOUR' }`.
  - `rejects bad intervals`: each of `'1.5 DAY'`, `'-1 DAY'`, `'0 DAY'` and
    `'1 FORTNIGHT'` throws the message above.
  - `offset with after does not typecheck`:
    ```ts
    // @ts-expect-error OFFSET is only valid with EVERY
    const bad: RefreshConfig = { after: '1 HOUR', offset: '5 MINUTE' };
    ```

- [ ] **Step 2: Run, and see them fail.**
  Run: `pnpm --filter @typed-clickhouse/lib test`
  Expected: the new tests FAIL.

- [ ] **Step 3: Implement** the types, the validation, the field and the
  serialisation. Update the class JSDoc: with `refresh`, the view re-runs on a
  schedule rather than on insert, and `selectTables` is only used for
  dependency ordering. Add the README example.

- [ ] **Step 4: Run everything.**
  ```bash
  pnpm --filter @typed-clickhouse/lib test && pnpm --filter @typed-clickhouse/lib typecheck
  cd packages/core && pnpm build && pnpm test && ./tests/golden.test.sh
  ```
  Expected: all PASS, and the golden baseline is unchanged. Do not
  re-capture it.

- [ ] **Step 5: Commit**
  Message: `feat(lib): add refresh option to MaterializedView`

---

### Task 7: Whole-branch verification

- [ ] **Step 1: Check for private references.** Review `git diff main` for
  any names, tables or business context from private consumers, using a
  pattern list kept outside this repository. Expected: none.
- [ ] **Step 2: Run the full gate once more.**
  - Rust gate.
  - `live_` tests.
  - lib tests and typecheck.
  - core build, test and golden.
  Expected: all green.
- [ ] **Step 3: Leave the release to the owner.** Use the repo's release
  workflow, with no version bump in this branch unless the owner asks.
