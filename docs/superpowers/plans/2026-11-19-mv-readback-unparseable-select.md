# MV Read-back for Unparseable SELECTs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a structured materialized view whose SELECT the forked sqlparser rejects round-trip: an unchanged plan is empty, a SELECT change is a drop + create, and removal drops it.

**Architecture:** `try_migrate_sql_resource_to_mv` falls back to a text split of the (single, library-shaped) setup statement when `parse_create_materialized_view` fails. The split takes:
- the header up to the top-level `AS` after `TO <target>`;
- the refresh clause from `split_refresh_clause`;
- the target and target database from the header;
- the SELECT as the remaining text;
- the source tables from `extract_source_tables_from_query_regex`.

Comparison is unchanged: it already normalises with ClickHouse's `formatQuerySingleLine`.

**Tech Stack:** Rust CLI (`apps/cli`), ClickHouse 25.8 live tests.

**Spec:** `docs/superpowers/specs/2026-11-19-mv-readback-unparseable-select-design.md`

## Global Constraints

- **Existing behaviour:** a parseable SELECT still converts through
  `parse_create_materialized_view`, with byte-identical results, and all
  existing tests pass unchanged.
- **Guards:** the fallback keeps every existing guard of
  `try_migrate_sql_resource_to_mv`:
  - exactly 1 setup + 1 teardown;
  - teardown starts `DROP VIEW IF EXISTS`;
  - setup starts `CREATE MATERIALIZED VIEW IF NOT EXISTS`;
  - setup contains ` TO `.
- **Fallback output:** a fallback that cannot find a top-level `AS` after the
  target returns `None`, as today. It never invents a view.
- **Logging:** the fallback logs at `debug!`, not `warn!`.
- **AGENTS.md rules:**
  - `thiserror`, never `anyhow`;
  - `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`
    and `cargo fmt --check` all green;
  - the live tests are green: `TC_LIVE_CLICKHOUSE=1 cargo test -p typed-clickhouse live_ -- --test-threads=1`.
- **Public repository:** neutral names only. No names, tables, tickets or
  business context from any private consumer.

## Review Focus

1. **A view or target name that needs backticks or a database qualifier.**
   For example ``CREATE MATERIALIZED VIEW IF NOT EXISTS `db`.`mv` REFRESH … TO `db`.`t` AS …``.
   It must split correctly. The unit test pins this.
2. **Text inside the SELECT that looks like ` AS ` or ` TO `.** It must stay
   intact, because the split takes the first top-level `AS` after the target,
   and the target comes before the SELECT. The unit test pins this with
   `SELECT 'x AS y' AS a, t.2 AS b`.
3. **A code-side user `SqlResource` in library shape that sqlparser
   rejects.** It now converts too. That is intended, and the same function
   serves both sides. The unit test pins the converted fields.

---

### Task 1: Text fallback in `try_migrate_sql_resource_to_mv`

**Files:**
- Modify: `apps/cli/src/framework/core/infrastructure_map.rs`
  (`try_migrate_sql_resource_to_mv`, ~line 1945, and its tests)
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/reality_roundtrip_live.rs`
  (fixture and tests)
- Possibly modify: `apps/cli/src/framework/core/infrastructure/refresh.rs` or
  `apps/cli/src/infrastructure/olap/clickhouse/sql_parser.rs`, if the header
  split reuses their top-level tokenizer. Reuse the tokenizer; do not
  duplicate it.
- Add (commit with this task): the spec and this plan

**Interfaces:**
- Consumes:
  - `split_refresh_clause(&str) -> (String, Option<RefreshSchedule>)`
  - `extract_source_tables_from_query_regex(&str, &str)`
  - `MaterializedView` (fields include `refresh`)
- Produces: no new public API. `try_migrate_sql_resource_to_mv(sql_resource, default_database) -> Option<MaterializedView>` returns `Some` for library-shaped statements that sqlparser rejects.

- [ ] **Step 1: Write failing unit tests** in `infrastructure_map.rs` tests:
  - `migrates_mv_whose_select_sqlparser_rejects`:
    - Input setup:
      `"CREATE MATERIALIZED VIEW IF NOT EXISTS mv REFRESH EVERY 1 DAY OFFSET 10 HOUR TO t AS SELECT a.id, tuple(1, 2).2 AS second FROM src AS a LEFT JOIN other AS p FINAL ON a.id = p.id"`,
      with teardown `DROP VIEW IF EXISTS mv`.
    - Expect `Some(mv)` with:
      - name `mv` and target `t`;
      - refresh `EVERY 1 DAY OFFSET 10 HOUR`;
      - `select_sql` equal to the text after `TO t AS ` (starting `SELECT a.id`);
      - `source_tables` containing `src` and `other`.
    - Before writing the assertion, check that `parse_create_materialized_view`
      really fails on this input. If it parses, add constructs until it fails.
  - `migrates_incremental_mv_whose_select_sqlparser_rejects`: the same without
    a refresh clause, with `refresh == None`.
  - `fallback_handles_qualified_backticked_names` (Review Focus 1):
    ``CREATE MATERIALIZED VIEW IF NOT EXISTS `db`.`mv` TO `db`.`t` AS SELECT 'x AS y' AS a, t.2 AS b FROM `db`.`s` AS t FINAL``
    gives target `t`, target database `db`, and `select_sql` starting
    `SELECT 'x AS y' AS a` (Review Focus 2).
  - `fallback_without_select_returns_none`:
    `CREATE MATERIALIZED VIEW IF NOT EXISTS mv TO t` gives `None`.
- [ ] **Step 2: Run them and see them fail.**
  Run: `cargo test -p typed-clickhouse migrates_ fallback_`
  Expected: FAIL, because the function returns `None`.
- [ ] **Step 3: Implement the fallback** in `try_migrate_sql_resource_to_mv`.
  Use it only when `parse_create_materialized_view(setup_sql)` errs.
- [ ] **Step 4: Run the tests and see them pass.** Run the full Rust gate.
- [ ] **Step 5: Live test.** In `reality_roundtrip_live.rs`, add a refreshable
  view to the fixture, for example `mv_device_pairs`.
  - Its SELECT uses tuple element access (`tuple(d.id, d.label).2`) and a
    `JOIN {DB}.sensor_readings AS r FINAL`.
  - It writes into a new small target table.
  - `live_reality_roundtrip_plans_nothing` must stay empty.

  Add `live_unparseable_select_round_trips`:
  - after applying the full fixture, a SELECT change plans exactly one
    `DropMaterializedView` + one `CreateMaterializedView` for that view, and
    applying it then re-plans empty;
  - a code-side map without the view plans its `DropMaterializedView`.

  Run: `TC_LIVE_CLICKHOUSE=1 cargo test -p typed-clickhouse live_ -- --test-threads=1`.
  Expected: all PASS.

  The ClickHouse test server comes from `docker-compose.test.yml` (ports
  18123/19000, password test123). Start it with
  `docker compose -f docker-compose.test.yml up -d --wait` if it is not
  running.
- [ ] **Step 6: Commit.**
```bash
git add apps/cli/src/framework/core/infrastructure_map.rs \
  apps/cli/src/infrastructure/olap/clickhouse/reality_roundtrip_live.rs \
  <any reused-tokenizer file> \
  docs/superpowers/specs/2026-11-19-mv-readback-unparseable-select-design.md \
  docs/superpowers/plans/2026-11-19-mv-readback-unparseable-select.md
git commit -m "fix(cli): read back materialized views whose SELECT sqlparser rejects"
```
